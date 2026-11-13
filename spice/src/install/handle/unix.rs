//! Unix name operations relative to held directory descriptors.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
use rustix::fs::RenameFlags;
use rustix::fs::{self as unix_fs, AtFlags, Mode, OFlags};

use super::super::DIRECTORY_MODE;
use super::changed_on_name_race;
use super::{AdmittedPath, FileImage, Generation, InstallError, ObservedFile};

/// One held directory: a descriptor every name operation is relative to.
pub(super) type Held = File;

pub(super) fn open_root(path: &Path) -> io::Result<Held> {
    unix_fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(Into::into)
}

pub(super) fn open_directory(parent: &Held, name: &OsString) -> io::Result<Held> {
    unix_fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(Into::into)
}

pub(super) fn held_generation(directory: &Held) -> io::Result<Generation> {
    Ok(Generation::observe(&directory.metadata()?))
}

pub(super) fn create_directory(parent: &Held, name: &OsString) -> io::Result<()> {
    unix_fs::mkdirat(parent, name, Mode::from_raw_mode(DIRECTORY_MODE as _)).map_err(Into::into)
}

pub(super) fn generation_at(
    parent: &Held,
    name: &OsString,
    display: &Path,
) -> Result<Option<Generation>, InstallError> {
    match unix_fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => {
            #[cfg(target_vendor = "apple")]
            let device = stat.st_dev as u64;
            #[cfg(not(target_vendor = "apple"))]
            let device = stat.st_dev;
            Ok(Some(Generation {
                device,
                inode: stat.st_ino,
            }))
        }
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(source) => Err(InstallError::io(
            "inspect admitted install name",
            display,
            source.into(),
        )),
    }
}

pub(super) fn observe_named(
    parent: &Held,
    name: &OsString,
    display: &Path,
) -> Result<Option<ObservedFile>, InstallError> {
    let descriptor = match unix_fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    ) {
        Ok(descriptor) => descriptor,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(source) => {
            return Err(InstallError::io(
                "open admitted install file",
                display,
                source.into(),
            ));
        }
    };
    let mut file = File::from(descriptor);
    let metadata = file
        .metadata()
        .map_err(|source| InstallError::io("inspect admitted install file", display, source))?;
    if !metadata.is_file() {
        return Err(InstallError::PathShape(display.to_path_buf()));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| InstallError::io("read admitted install file", display, source))?;
    Ok(Some(ObservedFile {
        image: FileImage::new(metadata.permissions().mode() & 0o777, bytes),
        generation: Generation::observe(&metadata),
    }))
}

/// Exclusively create the private staged name a publication fills.
pub(super) fn create_staged(parent: &Held, name: &OsString) -> io::Result<File> {
    unix_fs::openat(
        parent,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::from_raw_mode(0o600),
    )
    .map(File::from)
    .map_err(Into::into)
}

/// Write, protect, and flush the staged image, answering its generation.
pub(super) fn fill_staged(
    mut staged: File,
    image: &FileImage,
    display: &Path,
) -> Result<Generation, InstallError> {
    staged
        .write_all(&image.bytes)
        .map_err(|source| InstallError::io("write admitted staged file", display, source))?;
    staged
        .set_permissions(fs::Permissions::from_mode(image.mode))
        .map_err(|source| InstallError::io("set admitted staged mode", display, source))?;
    staged
        .sync_all()
        .map_err(|source| InstallError::io("synchronize admitted staged file", display, source))?;
    Ok(Generation::observe(&staged.metadata().map_err(
        |source| InstallError::io("inspect admitted staged file", display, source),
    )?))
}

pub(super) fn discard_name(parent: &Held, name: &OsString) {
    let _ = unix_fs::unlinkat(parent, name, AtFlags::empty());
}

pub(super) fn remove_name(
    parent: &Held,
    name: &OsString,
    display: &Path,
) -> Result<(), InstallError> {
    unix_fs::unlinkat(parent, name, AtFlags::empty())
        .map_err(|source| InstallError::io("remove admitted install name", display, source.into()))
}

pub(super) fn remove_directory(parent: &Held, name: &OsString) -> io::Result<()> {
    unix_fs::unlinkat(parent, name, AtFlags::REMOVEDIR).map_err(Into::into)
}

pub(super) fn sync(directory: &Held) -> io::Result<()> {
    directory.sync_all()
}

impl AdmittedPath {
    /// Exchange the staged name with the expected destination, or leave
    /// neither moved.
    ///
    /// The exchange is atomic, so the destination is never absent. After it,
    /// the parent and both names are verified; a refusal exchanges back. The
    /// displaced prior image then leaves under the staged name.
    pub(super) fn admit_replacement(
        &self,
        temporary: &OsString,
        staged_generation: Generation,
        require_displaced: impl FnOnce(&OsString) -> Result<(), InstallError>,
    ) -> Result<(), InstallError> {
        changed_on_name_race(
            exchange(self.parent(), temporary, &self.file_name, &self.display),
            &self.display,
        )?;
        let admitted = self.verify_named_parent().and_then(|()| {
            self.require_named_generation(&self.file_name, staged_generation)?;
            require_displaced(temporary)
        });
        if let Err(error) = admitted {
            exchange(self.parent(), temporary, &self.file_name, &self.display)?;
            return Err(error);
        }
        remove_name(self.parent(), temporary, &self.display)
    }
}

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
pub(super) fn rename_noreplace(
    parent: &Held,
    old_name: &OsString,
    new_name: &OsString,
    display: &Path,
) -> Result<(), InstallError> {
    unix_fs::renameat_with(parent, old_name, parent, new_name, RenameFlags::NOREPLACE).map_err(
        |source| {
            InstallError::io(
                "atomically move admitted install name",
                display,
                source.into(),
            )
        },
    )
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
pub(super) fn rename_noreplace(
    _parent: &Held,
    _old_name: &OsString,
    _new_name: &OsString,
    display: &Path,
) -> Result<(), InstallError> {
    Err(InstallError::InvalidRecord(format!(
        "atomic install rename is unavailable for {}",
        display.display()
    )))
}

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn exchange(
    parent: &Held,
    old_name: &OsString,
    new_name: &OsString,
    display: &Path,
) -> Result<(), InstallError> {
    unix_fs::renameat_with(parent, old_name, parent, new_name, RenameFlags::EXCHANGE).map_err(
        |source| {
            InstallError::io(
                "atomically exchange admitted install names",
                display,
                source.into(),
            )
        },
    )
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
fn exchange(
    _parent: &Held,
    _old_name: &OsString,
    _new_name: &OsString,
    display: &Path,
) -> Result<(), InstallError> {
    Err(InstallError::InvalidRecord(format!(
        "atomic install exchange is unavailable for {}",
        display.display()
    )))
}
