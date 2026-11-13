//! Windows name operations beneath held directory paths.
//!
//! Each held directory denies delete sharing, so while an admission holds its
//! chain no held directory can be renamed or removed and every joined path
//! keeps naming the held directory. Name operations therefore go through those
//! paths.
//!
//! Windows refuses to unlink the last name of an image a process is running,
//! or a file another process opened without delete sharing, yet lets either
//! take another name. Replacement therefore links the prior object under a
//! second name before atomically replacing the destination, and a removal that
//! meets an object in use leaves it under a canonical temporary name, which
//! install maintenance reclaims once it is free.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;

use sill::authority::{Directory, Entry};
use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION};

use super::{
    AdmittedPath, FileImage, Generation, InstallError, ObservedFile, changed_on_name_race,
    temporary_name,
};

/// One held directory: a hold without delete sharing on its path.
pub(super) type Held = Directory;

pub(super) fn open_root(path: &Path) -> io::Result<Held> {
    Directory::open(path)
}

pub(super) fn open_directory(parent: &Held, name: &OsString) -> io::Result<Held> {
    parent.open_directory(name)
}

pub(super) fn held_generation(directory: &Held) -> io::Result<Generation> {
    Generation::of_object(directory.identity()?.object())
}

pub(super) fn create_directory(parent: &Held, name: &OsString) -> io::Result<()> {
    sill::authority::create_private_directory(&parent.path().join(name))
}

pub(super) fn generation_at(
    parent: &Held,
    name: &OsString,
    display: &Path,
) -> Result<Option<Generation>, InstallError> {
    match sill::authority::path_identity(&parent.path().join(name)) {
        Ok(identity) => Generation::of_object(identity.object())
            .map(Some)
            .map_err(|source| InstallError::io("inspect admitted install name", display, source)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(InstallError::io(
            "inspect admitted install name",
            display,
            source,
        )),
    }
}

pub(super) fn observe_named(
    parent: &Held,
    name: &OsString,
    display: &Path,
) -> Result<Option<ObservedFile>, InstallError> {
    let mut file = match parent
        .open_entry(name, false)
        .map_err(|source| InstallError::io("open admitted install file", display, source))?
    {
        Entry::Missing => return Ok(None),
        Entry::Link => return Err(InstallError::PathShape(display.to_path_buf())),
        Entry::Opened(file) => file,
    };
    let metadata = file
        .metadata()
        .map_err(|source| InstallError::io("inspect admitted install file", display, source))?;
    if !metadata.is_file() {
        return Err(InstallError::PathShape(display.to_path_buf()));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| InstallError::io("read admitted install file", display, source))?;
    let mode = sill::authority::file_mode(&file)
        .map_err(|source| InstallError::io("inspect admitted install file", display, source))?;
    Ok(Some(ObservedFile {
        image: FileImage::new(mode & 0o777, bytes),
        generation: Generation::of_file(&file)
            .map_err(|source| InstallError::io("inspect admitted install file", display, source))?,
    }))
}

/// Exclusively create the owner-only staged name a publication fills.
pub(super) fn create_staged(parent: &Held, name: &OsString) -> io::Result<File> {
    parent.create_private(name)
}

/// Write, protect, and flush the staged image, answering its generation.
///
/// The handle closes before admission so that only the staged name, never an
/// open handle of this process, stands between the image and its rename.
pub(super) fn fill_staged(
    mut staged: File,
    image: &FileImage,
    display: &Path,
) -> Result<Generation, InstallError> {
    staged
        .write_all(&image.bytes)
        .map_err(|source| InstallError::io("write admitted staged file", display, source))?;
    sill::authority::set_file_mode(&staged, image.mode)
        .map_err(|source| InstallError::io("set admitted staged mode", display, source))?;
    staged
        .sync_all()
        .map_err(|source| InstallError::io("synchronize admitted staged file", display, source))?;
    Generation::of_file(&staged)
        .map_err(|source| InstallError::io("inspect admitted staged file", display, source))
}

pub(super) fn discard_name(parent: &Held, name: &OsString) {
    let _ = parent.remove_file(name);
}

/// Unlink `name`, or, while its object is in use, leave it under a fresh
/// canonical temporary name for install maintenance to reclaim.
pub(super) fn remove_name(
    parent: &Held,
    name: &OsString,
    display: &Path,
) -> Result<(), InstallError> {
    match parent.remove_file(name) {
        Ok(()) => Ok(()),
        Err(source) if in_use(&source) => {
            rename_noreplace(parent, name, &temporary_name(), display)
        }
        Err(source) => Err(InstallError::io(
            "remove admitted install name",
            display,
            source,
        )),
    }
}

pub(super) fn remove_directory(parent: &Held, name: &OsString) -> io::Result<()> {
    fs::remove_dir(parent.path().join(name))
}

pub(super) fn sync(directory: &Held) -> io::Result<()> {
    directory.sync()
}

pub(super) fn rename_noreplace(
    parent: &Held,
    old_name: &OsString,
    new_name: &OsString,
    display: &Path,
) -> Result<(), InstallError> {
    sill::authority::rename_no_replace(&parent.path().join(old_name), &parent.path().join(new_name))
        .map_err(|source| {
            InstallError::io("atomically move admitted install name", display, source)
        })
}

impl AdmittedPath {
    /// Replace the expected destination with the staged name, or leave the
    /// expected image in place.
    ///
    /// A second name links the destination's object first and is proved to
    /// be the expected image; the atomic replacement then never leaves the
    /// destination absent, and the prior object survives under the second
    /// name even when it is a running image. After it, the parent and both
    /// names are verified; a refusal moves the prior object back. The prior
    /// image then leaves through [`remove_name`].
    pub(super) fn admit_replacement(
        &self,
        temporary: &OsString,
        staged_generation: Generation,
        require_displaced: impl FnOnce(&OsString) -> Result<(), InstallError>,
    ) -> Result<(), InstallError> {
        let parent = self.parent();
        let displaced = temporary_name();
        changed_on_name_race(
            hard_link(parent, &self.file_name, &displaced, &self.display),
            &self.display,
        )?;
        let linked = self
            .verify_named_parent()
            .and_then(|()| require_displaced(&displaced));
        if let Err(error) = linked {
            discard_name(parent, &displaced);
            return Err(error);
        }
        if let Err(error) = changed_on_name_race(
            replace(parent, temporary, &self.file_name, &self.display),
            &self.display,
        ) {
            discard_name(parent, &displaced);
            return Err(error);
        }
        let admitted = self
            .verify_named_parent()
            .and_then(|()| self.require_named_generation(&self.file_name, staged_generation));
        if let Err(error) = admitted {
            replace(parent, &displaced, &self.file_name, &self.display)?;
            return Err(error);
        }
        remove_name(parent, &displaced, &self.display)
    }
}

/// Whether a refused unlink met an object in use rather than a denial of the
/// name itself.
pub(in super::super) fn in_use(error: &io::Error) -> bool {
    error.raw_os_error().is_some_and(|code| {
        code == ERROR_ACCESS_DENIED as i32 || code == ERROR_SHARING_VIOLATION as i32
    })
}

fn replace(
    parent: &Held,
    source: &OsString,
    destination: &OsString,
    display: &Path,
) -> Result<(), InstallError> {
    parent.replace(source, destination).map_err(|source| {
        InstallError::io("atomically replace admitted install name", display, source)
    })
}

/// Give the object `existing` names the additional name `new`.
fn hard_link(
    parent: &Held,
    existing: &OsString,
    new: &OsString,
    display: &Path,
) -> Result<(), InstallError> {
    parent
        .link(existing, new)
        .map_err(|source| InstallError::io("link admitted install name", display, source))
}
