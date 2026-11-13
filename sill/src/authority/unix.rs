use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, DirBuilder, File, Metadata, Permissions};
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustix::fs::{AtFlags, Dir, FileType};

use super::{Entry, EntryKind, FileIdentity, FileObject, Occupancy};

/// The owner-only mode of a created private file.
const PRIVATE_FILE: libc::c_uint = 0o600;

const BLOCK_BYTES: u64 = 512;
const NANOSECONDS: i128 = 1_000_000_000;

pub(super) fn owner_mode(path: &Path) -> io::Result<u32> {
    Ok(fs::symlink_metadata(path)?.mode() & 0o777)
}

pub(super) fn file_mode(file: &File) -> io::Result<u32> {
    Ok(file.metadata()?.mode() & 0o7777)
}

pub(super) fn set_owner_mode(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, Permissions::from_mode(mode))
}

pub(super) fn set_file_mode(file: &File, mode: u32) -> io::Result<()> {
    file.set_permissions(Permissions::from_mode(mode))
}

pub(super) fn create_private_directory(path: &Path) -> io::Result<()> {
    DirBuilder::new().mode(0o700).create(path)
}

pub(super) fn create_private_file(path: &Path) -> io::Result<File> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE)
        .custom_flags(libc::O_CLOEXEC)
        .open(path)
}

#[cfg(target_os = "macos")]
pub(super) fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    let source = c_path(source)?;
    let destination = c_path(destination)?;
    // SAFETY: both arguments are NUL-terminated paths that outlive the call.
    let result =
        unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Issues the `renameat2` system call directly: the musl libc bundled for
/// static Linux builds exports no `renameat2` symbol to link against.
#[cfg(target_os = "linux")]
pub(super) fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    renameat_with(CWD, source, CWD, destination, RenameFlags::NOREPLACE).map_err(io::Error::from)
}

pub(super) fn directory_link_form(target: &Path) -> io::Result<PathBuf> {
    Ok(target.to_path_buf())
}

pub(super) fn link_directory(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

pub(super) fn read_directory_link(link: &Path) -> io::Result<PathBuf> {
    fs::read_link(link)
}

pub(super) fn allocated_bytes(_path: &Path, metadata: &Metadata) -> io::Result<u64> {
    Ok(metadata.blocks().saturating_mul(BLOCK_BYTES))
}

pub(super) fn occupancy(_path: &Path, metadata: &Metadata) -> io::Result<Occupancy> {
    Ok(Occupancy {
        allocated: metadata.blocks().saturating_mul(BLOCK_BYTES),
        linked: (metadata.nlink() > 1).then(|| object(metadata)),
        changed: instant(metadata.mtime(), metadata.mtime_nsec())
            .max(instant(metadata.ctime(), metadata.ctime_nsec())),
    })
}

/// A file time as an instant; one before the epoch reads as the epoch.
fn instant(seconds: i64, nanoseconds: i64) -> SystemTime {
    u64::try_from(seconds)
        .ok()
        .and_then(|seconds| {
            let nanoseconds = u32::try_from(nanoseconds).unwrap_or(0);
            UNIX_EPOCH.checked_add(Duration::new(seconds, nanoseconds))
        })
        .unwrap_or(UNIX_EPOCH)
}

pub(super) fn is_link_like(metadata: &Metadata) -> bool {
    metadata.file_type().is_symlink()
}

pub(super) fn file_identity(file: &File) -> io::Result<FileIdentity> {
    Ok(identity(&file.metadata()?))
}

pub(super) fn path_identity(path: &Path) -> io::Result<FileIdentity> {
    Ok(identity(&fs::symlink_metadata(path)?))
}

pub(super) fn owned_by_current_user(path: &Path) -> io::Result<bool> {
    // SAFETY: `geteuid` reads this process's credentials and cannot fail.
    let user = unsafe { libc::geteuid() };
    Ok(fs::symlink_metadata(path)?.uid() == user)
}

pub(super) fn owner_identity(path: &Path) -> io::Result<String> {
    Ok(format!("uid:{}", fs::symlink_metadata(path)?.uid()))
}

pub(super) fn process_owners() -> io::Result<super::ProcessOwners> {
    // SAFETY: `geteuid` reads this process's credentials and cannot fail.
    let identity = format!("uid:{}", unsafe { libc::geteuid() });
    Ok(super::ProcessOwners {
        user: identity.clone(),
        default_owner: identity,
    })
}

fn identity(metadata: &Metadata) -> FileIdentity {
    FileIdentity {
        object: object(metadata),
        bytes: metadata.size(),
        modified: i128::from(metadata.mtime()) * NANOSECONDS + i128::from(metadata.mtime_nsec()),
        changed: i128::from(metadata.ctime()) * NANOSECONDS + i128::from(metadata.ctime_nsec()),
    }
}

fn object(metadata: &Metadata) -> FileObject {
    FileObject {
        volume: metadata.dev(),
        index: u128::from(metadata.ino()).to_le_bytes(),
    }
}

pub(super) fn open_unfollowed(path: &Path) -> io::Result<File> {
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

pub(super) fn hold_directory(path: &Path) -> io::Result<File> {
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
        .map_err(directory_refusal)
}

pub(super) fn open_directory_entry(
    directory: &File,
    _path: &Path,
    name: &OsStr,
) -> io::Result<File> {
    open_at(
        directory,
        name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        0,
    )
    .map_err(directory_refusal)
}

/// A final link refuses a directory open as `ELOOP`; report it as the
/// non-directory it is.
fn directory_refusal(error: io::Error) -> io::Error {
    if error.raw_os_error() == Some(libc::ELOOP) {
        io::Error::new(io::ErrorKind::NotADirectory, error)
    } else {
        error
    }
}

pub(super) struct Entries {
    directory: File,
    listing: Dir,
}

pub(super) fn entries(directory: &File, _path: &Path) -> io::Result<Entries> {
    Ok(Entries {
        listing: Dir::read_from(directory)?,
        directory: directory.try_clone()?,
    })
}

impl Iterator for Entries {
    type Item = io::Result<(OsString, EntryKind)>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let entry = match self.listing.next()? {
                Ok(entry) => entry,
                Err(error) => return Some(Err(error.into())),
            };
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            let name = OsStr::from_bytes(name).to_os_string();
            let kind =
                rustix::fs::statat(&self.directory, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW)
                    .map(|stat| match FileType::from_raw_mode(stat.st_mode) {
                        FileType::RegularFile => EntryKind::File,
                        FileType::Directory => EntryKind::Directory,
                        _ => EntryKind::Other,
                    });
            return Some(kind.map(|kind| (name, kind)).map_err(Into::into));
        }
    }
}

pub(super) fn create_private_entry(
    directory: &File,
    _path: &Path,
    name: &OsStr,
) -> io::Result<File> {
    open_at(
        directory,
        name,
        libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        PRIVATE_FILE,
    )
}

pub(super) fn open_entry(
    directory: &File,
    _path: &Path,
    name: &OsStr,
    write: bool,
) -> io::Result<Entry> {
    let access = if write { libc::O_RDWR } else { libc::O_RDONLY };
    let flags = access | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
    match open_at(directory, name, flags, 0) {
        Ok(file) => Ok(Entry::Opened(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Entry::Missing),
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => Ok(Entry::Link),
        Err(error) => Err(error),
    }
}

pub(super) fn replace_entry(
    directory: &File,
    _path: &Path,
    source: &OsStr,
    destination: &OsStr,
) -> io::Result<()> {
    let source = c_name(source)?;
    let destination = c_name(destination)?;
    let descriptor = directory.as_raw_fd();
    // SAFETY: the descriptor is open for the call, and both names are
    // NUL-terminated single components that outlive it.
    let result = unsafe {
        libc::renameat(
            descriptor,
            source.as_ptr(),
            descriptor,
            destination.as_ptr(),
        )
    };
    status(result)
}

pub(super) fn link_entry(
    directory: &File,
    _path: &Path,
    existing: &OsStr,
    new: &OsStr,
) -> io::Result<()> {
    let existing = c_name(existing)?;
    let new = c_name(new)?;
    let descriptor = directory.as_raw_fd();
    // SAFETY: the descriptor is open for the call, and both names are
    // NUL-terminated single components that outlive it. Without
    // `AT_SYMLINK_FOLLOW`, a final link is itself linked, never followed.
    let result =
        unsafe { libc::linkat(descriptor, existing.as_ptr(), descriptor, new.as_ptr(), 0) };
    status(result)
}

pub(super) fn remove_entry(directory: &File, _path: &Path, name: &OsStr) -> io::Result<()> {
    let name = c_name(name)?;
    // SAFETY: the descriptor is open for the call, and `name` is a
    // NUL-terminated single component that outlives it.
    status(unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) })
}

pub(super) fn sync_directory(directory: &File, _path: &Path) -> io::Result<()> {
    directory.sync_all()
}

fn open_at(
    directory: &File,
    name: &OsStr,
    flags: libc::c_int,
    mode: libc::c_uint,
) -> io::Result<File> {
    let name = c_name(name)?;
    // SAFETY: the descriptor is open for the call, `name` is NUL-terminated,
    // and `mode` is read only when `flags` creates the entry.
    let descriptor = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags, mode) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a new descriptor that nothing else owns.
    Ok(File::from(unsafe { OwnedFd::from_raw_fd(descriptor) }))
}

fn status(result: libc::c_int) -> io::Result<()> {
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

#[cfg(target_os = "macos")]
fn c_path(path: &Path) -> io::Result<CString> {
    c_name(path.as_os_str())
}
