use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct LockName(String);

impl LockName {
    pub fn new(name: impl Into<String>) -> Result<Self, LockNameError> {
        let name = name.into();
        if name.is_empty()
            || matches!(name.as_str(), "." | "..")
            || name
                .chars()
                .any(|character| character == '/' || character == '\\' || character == '\0')
        {
            return Err(LockNameError);
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LockNameError;

impl fmt::Display for LockNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("lock name must be one normal path component")
    }
}

impl Error for LockNameError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockFile {
    name: LockName,
    path: PathBuf,
}

impl LockFile {
    pub fn new(name: LockName, path: impl Into<PathBuf>) -> Self {
        Self {
            name,
            path: path.into(),
        }
    }

    pub fn name(&self) -> &LockName {
        &self.name
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn try_acquire(&self) -> io::Result<Acquisition> {
        try_acquire(self)
    }

    /// Try to possess the lock, exposing each provisional contention.
    ///
    /// The callback runs after an exclusive attempt was denied but before the
    /// contention is classified as a real exclusive holder or a concurrent
    /// availability probe. It exists so callers can prove that a starting
    /// owner survives that exact overlap without relying on scheduler timing.
    #[doc(hidden)]
    pub fn try_acquire_with(&self, after_contention: impl FnMut()) -> io::Result<Acquisition> {
        try_acquire_with(self, after_contention)
    }

    /// Try to possess a caller-opened lock file without reopening its pathname.
    ///
    /// The caller owns how the descriptor is resolved. The descriptor must be
    /// a close-on-exec regular file opened for reading and writing.
    pub fn try_acquire_opened(&self, file: File) -> io::Result<Acquisition> {
        try_acquire_opened(self, file)
    }

    /// Block on a caller-opened descriptor until the kernel grants possession.
    #[cfg(unix)]
    pub(crate) fn acquire_opened(&self, file: File) -> io::Result<Possession> {
        acquire_opened(self, file, Operation::Exclusive)
    }

    /// Observe whether the kernel would grant this existing lock now.
    ///
    /// Missing lock files are available. This operation never creates a file;
    /// an existing file is opened without following symlinks and shared only
    /// for the duration of the nonblocking kernel probe.
    ///
    /// # Errors
    ///
    /// Returns an operating-system refusal opening or probing an existing
    /// target.
    pub fn availability(&self) -> io::Result<Availability> {
        availability(self)
    }

    /// Observe availability while exposing the instant the probe is held.
    ///
    /// The callback runs only when the target is available, after the shared
    /// probe lands and before it is released. It exists to make overlap with
    /// an exclusive acquisition deterministic in tests.
    #[doc(hidden)]
    pub fn availability_with(&self, while_held: impl FnOnce()) -> io::Result<Availability> {
        availability_with(self, while_held)
    }

    pub(crate) fn availability_opened(&self, file: File) -> io::Result<Availability> {
        availability_opened(file)
    }

    /// Block until the kernel grants exclusive possession.
    ///
    /// # Errors
    ///
    /// Returns an operating-system refusal opening or possessing the target.
    pub fn acquire(&self) -> io::Result<Possession> {
        acquire(self)
    }
}

#[derive(Debug)]
pub enum Acquisition {
    Acquired(Possession),
    Contended,
}

/// One point-in-time observation of a configured lock file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Availability {
    Available,
    Held,
}

/// Exclusive kernel possession of one lock file.
#[derive(Debug)]
pub struct Possession {
    lock: LockFile,
    file: File,
    #[cfg(unix)]
    unlock_on_drop: bool,
}

impl Possession {
    pub fn lock(&self) -> &LockFile {
        &self.lock
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    /// Replace the holder record without first publishing an empty lock file.
    ///
    /// # Errors
    ///
    /// Returns an operating-system refusal measuring, writing, flushing, or
    /// synchronizing the lock file.
    pub fn rewrite_record(&self, record: &[u8]) -> io::Result<()> {
        self.rewrite_record_with(record, || Ok(()))
    }

    /// Replace the holder record, exposing the instant before it lands.
    ///
    /// The callback runs after every preparatory step a separate reader could
    /// observe and immediately before the one write that lands the replacement.
    /// It exists so callers can prove what a contended reader finds mid-rewrite
    /// without relying on a scheduler to reproduce that instant.
    #[doc(hidden)]
    pub fn rewrite_record_with(
        &self,
        record: &[u8],
        before_landing: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        let spanned = self.file.metadata()?.len();
        let bytes = replacement_record(record.to_vec(), spanned);
        let mut writer = &self.file;
        writer.seek(SeekFrom::Start(0))?;
        before_landing()?;
        writer.write_all(&bytes)?;
        writer.flush()?;
        self.file.sync_data()
    }

    #[cfg(unix)]
    pub(crate) fn retain_until_last_close(&mut self) {
        self.unlock_on_drop = false;
    }

    /// Arrange for one command and its descendants to retain this possession
    /// if the parent drops or exits first.
    ///
    /// The child inherits only this already validated lock descriptor. Normal
    /// close-on-exec behavior remains in force for every other descriptor.
    #[cfg(unix)]
    pub fn retain_through_child(&mut self, command: &mut std::process::Command) {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::process::CommandExt as _;

        let descriptor = self.file.as_raw_fd();
        self.retain_until_last_close();
        // SAFETY: `fcntl` is async-signal-safe, and the closure touches only a
        // copied descriptor integer between fork and exec. Clearing CLOEXEC in
        // the child leaves the parent's descriptor flags unchanged.
        unsafe {
            command.pre_exec(move || {
                let flags = platform::fcntl(descriptor, platform::F_GETFD);
                if flags == -1
                    || platform::fcntl(descriptor, platform::F_SETFD, flags & !platform::FD_CLOEXEC)
                        == -1
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    /// Keep the parent-held Windows file lock while the command runs.
    ///
    /// Windows handles are non-inheritable here; the waiting parent remains
    /// the lock owner for the complete child lifetime.
    #[cfg(windows)]
    pub fn retain_through_child(&mut self, _command: &mut std::process::Command) {}
}

/// One record covering every byte the file already spans, ready to be written whole.
///
/// An empty file is how release is spelled, so emptying the file before writing
/// leaves a window in which the lock claims no holder while a process still has
/// kernel possession. Padding the record out to the span it replaces lets one
/// write supersede the whole record, so a contended reader never lands in an
/// artificial empty window.
pub(crate) fn replacement_record(mut record: Vec<u8>, spanned: u64) -> Vec<u8> {
    record.push(b'\n');
    let spanned = usize::try_from(spanned).unwrap_or(usize::MAX);
    record.resize(record.len().max(spanned), b' ');
    record
}

/// One kernel lock request.
#[derive(Clone, Copy)]
enum Operation {
    Exclusive,
    TryExclusive,
    TryShared,
}

fn try_acquire(lock: &LockFile) -> io::Result<Acquisition> {
    try_acquire_with(lock, || {})
}

fn try_acquire_with(
    lock: &LockFile,
    mut after_contention: impl FnMut(),
) -> io::Result<Acquisition> {
    loop {
        match acquire_with(lock, Operation::TryExclusive) {
            Ok(possession) => return Ok(Acquisition::Acquired(possession)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => after_contention(),
            Err(error) => return Err(error),
        }
        match availability(lock)? {
            Availability::Available => {}
            Availability::Held => return Ok(Acquisition::Contended),
        }
    }
}

fn try_acquire_opened(lock: &LockFile, file: File) -> io::Result<Acquisition> {
    loop {
        match acquire_opened(lock, file.try_clone()?, Operation::TryExclusive) {
            Ok(possession) => return Ok(Acquisition::Acquired(possession)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        match availability_opened(file.try_clone()?)? {
            Availability::Available => {}
            Availability::Held => return Ok(Acquisition::Contended),
        }
    }
}

fn acquire(lock: &LockFile) -> io::Result<Possession> {
    acquire_with(lock, Operation::Exclusive)
}

fn acquire_with(lock: &LockFile, operation: Operation) -> io::Result<Possession> {
    let file = platform::open(&lock.path, true)?;
    acquire_opened(lock, file, operation)
}

fn acquire_opened(lock: &LockFile, file: File, operation: Operation) -> io::Result<Possession> {
    require_regular(&file)?;
    platform::admit(&file)?;
    platform::lock(&file, operation)?;
    Ok(Possession {
        lock: lock.clone(),
        file,
        #[cfg(unix)]
        unlock_on_drop: true,
    })
}

fn availability(lock: &LockFile) -> io::Result<Availability> {
    availability_with(lock, || {})
}

fn availability_with(lock: &LockFile, while_held: impl FnOnce()) -> io::Result<Availability> {
    let file = match platform::open(&lock.path, false) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Availability::Available);
        }
        Err(error) => return Err(error),
    };
    availability_opened_with(file, while_held)
}

fn availability_opened(file: File) -> io::Result<Availability> {
    availability_opened_with(file, || {})
}

/// Probe with a shared request, then give back what the probe took.
///
/// Dropping `file` is not enough. Callers probe with a `try_clone` while the
/// original descriptor stays open, and a lock lives on the open file
/// description (Windows: file object) both share, so closing one copy
/// releases nothing. An unlock on any copy releases it for all of them, which
/// also closes the window against a child that already inherited the
/// description across a `fork` with no `exec`.
fn availability_opened_with(file: File, while_held: impl FnOnce()) -> io::Result<Availability> {
    require_regular(&file)?;
    platform::admit(&file)?;
    match platform::lock(&file, Operation::TryShared) {
        Ok(()) => {
            while_held();
            platform::unlock(&file)?;
            Ok(Availability::Available)
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(Availability::Held),
        Err(error) => Err(error),
    }
}

fn require_regular(file: &File) -> io::Result<()> {
    if file.metadata()?.is_file() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "lock target is not a regular file",
        ))
    }
}

impl Drop for Possession {
    fn drop(&mut self) {
        #[cfg(unix)]
        if !self.unlock_on_drop {
            return;
        }
        let _result = platform::unlock(&self.file);
    }
}

#[cfg(unix)]
mod platform {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::fd::AsRawFd as _;
    use std::os::raw::c_int;
    use std::path::Path;

    use super::Operation;

    pub const F_GETFD: c_int = 1;
    pub const F_SETFD: c_int = 2;
    pub const FD_CLOEXEC: c_int = 1;
    const LOCK_SH: c_int = 1;
    const LOCK_EX: c_int = 2;
    const LOCK_NB: c_int = 4;
    const LOCK_UN: c_int = 8;

    #[cfg(target_os = "macos")]
    const O_CLOEXEC: c_int = 0x01_00_00_00;
    #[cfg(target_os = "linux")]
    const O_CLOEXEC: c_int = 0x8_00_00;
    #[cfg(target_os = "macos")]
    const O_NOFOLLOW: c_int = 0x100;
    #[cfg(target_os = "linux")]
    const O_NOFOLLOW: c_int = 0x2_00_00;

    unsafe extern "C" {
        pub fn fcntl(descriptor: c_int, command: c_int, ...) -> c_int;
        fn flock(descriptor: c_int, operation: c_int) -> c_int;
    }

    /// Open the lock file without following a final symlink.
    pub(super) fn open(path: &Path, create: bool) -> io::Result<File> {
        use std::os::unix::fs::OpenOptionsExt as _;

        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .custom_flags(O_CLOEXEC | O_NOFOLLOW);
        if create {
            options.create(true).mode(0o600);
        }
        options.open(path)
    }

    /// A lock descriptor never leaks into an executed child by default.
    pub(super) fn admit(file: &File) -> io::Result<()> {
        // SAFETY: `fcntl(F_GETFD)` only reads flags of a descriptor `file` owns.
        let flags = unsafe { fcntl(file.as_raw_fd(), F_GETFD) };
        if flags == -1 {
            return Err(io::Error::last_os_error());
        }
        if flags & FD_CLOEXEC == 0 {
            return Err(io::Error::other(
                "lock descriptor was opened without CLOEXEC",
            ));
        }
        Ok(())
    }

    pub(super) fn lock(file: &File, operation: Operation) -> io::Result<()> {
        let operation = match operation {
            Operation::Exclusive => LOCK_EX,
            Operation::TryExclusive => LOCK_EX | LOCK_NB,
            Operation::TryShared => LOCK_SH | LOCK_NB,
        };
        flock_result(file, operation)
    }

    pub(super) fn unlock(file: &File) -> io::Result<()> {
        flock_result(file, LOCK_UN)
    }

    fn flock_result(file: &File, operation: c_int) -> io::Result<()> {
        // SAFETY: `flock` acts only on a descriptor `file` owns.
        if unsafe { flock(file.as_raw_fd(), operation) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle as _;
    use std::path::Path;

    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
        LockFileEx, UnlockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    use super::Operation;

    /// Windows byte-range locks are mandatory: a locked byte refuses reads and
    /// writes through every other handle. Possession therefore locks one byte
    /// far beyond any holder record, so readers keep reading the record.
    const LOCK_OFFSET: u64 = u64::MAX - 1;

    /// Open the lock file itself, never the target of a final reparse point.
    ///
    /// The standard library opens Windows handles non-inheritable.
    pub(super) fn open(path: &Path, create: bool) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        if create {
            options.create(true);
        }
        options.open(path)
    }

    pub(super) fn admit(_file: &File) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn lock(file: &File, operation: Operation) -> io::Result<()> {
        let flags = match operation {
            Operation::Exclusive => LOCKFILE_EXCLUSIVE_LOCK,
            Operation::TryExclusive => LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            Operation::TryShared => LOCKFILE_FAIL_IMMEDIATELY,
        };
        let mut overlapped = lock_range();
        // SAFETY: `file` owns the synchronous handle for the call, and the
        // range descriptor outlives the call, which completes before return.
        if unsafe { LockFileEx(file.as_raw_handle(), flags, 0, 1, 0, &raw mut overlapped) } != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == i32::try_from(ERROR_LOCK_VIOLATION).ok() {
            Err(io::Error::new(io::ErrorKind::WouldBlock, error))
        } else {
            Err(error)
        }
    }

    pub(super) fn unlock(file: &File) -> io::Result<()> {
        let mut overlapped = lock_range();
        // SAFETY: as for `LockFileEx`, over the same one-byte range.
        if unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &raw mut overlapped) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn lock_range() -> OVERLAPPED {
        let mut overlapped = OVERLAPPED::default();
        overlapped.Anonymous.Anonymous.Offset = LOCK_OFFSET as u32;
        overlapped.Anonymous.Anonymous.OffsetHigh = (LOCK_OFFSET >> 32) as u32;
        overlapped
    }
}

/// Wasm builds check contracts only; they own no kernel file locks.
#[cfg(not(any(unix, windows)))]
mod platform {
    use std::fs::File;
    use std::io;
    use std::path::Path;

    use super::Operation;

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "file locks require a Unix or Windows target",
        )
    }

    pub(super) fn open(_path: &Path, _create: bool) -> io::Result<File> {
        Err(unsupported())
    }

    pub(super) fn admit(_file: &File) -> io::Result<()> {
        Err(unsupported())
    }

    pub(super) fn lock(_file: &File, _operation: Operation) -> io::Result<()> {
        Err(unsupported())
    }

    pub(super) fn unlock(_file: &File) -> io::Result<()> {
        Err(unsupported())
    }
}

#[cfg(test)]
pub(crate) mod tests;
