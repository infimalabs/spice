//! Durable, same-directory file publication.
//!
//! A publication writes a new file beside its destination, synchronizes the
//! complete bytes, atomically replaces the destination, and then synchronizes
//! the parent directory. Callers can either use [`Destination::publish`] or
//! stage bytes explicitly with [`Destination::stage`].

#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);
const CREATE_ATTEMPTS: usize = 128;
const TEMPORARY_PREFIX: &str = ".spice-atomic-";
const TEMPORARY_SUFFIX: &str = ".tmp";

mod rooted;
pub use rooted::{RootedFiles, RootedPublish, is_rooted_path};

/// Whether a path carries the exact temporary filename emitted by this crate.
///
/// This recognizes the name without reading or resolving the path, so a
/// filesystem watcher can classify a publication file that disappeared before
/// metadata inspection.
#[must_use]
pub fn is_temporary_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(body) = name
        .strip_prefix(TEMPORARY_PREFIX)
        .and_then(|body| body.strip_suffix(TEMPORARY_SUFFIX))
    else {
        return false;
    };
    let Some((process, sequence)) = body.split_once('-') else {
        return false;
    };
    let (Ok(process), Ok(sequence)) = (process.parse::<u32>(), sequence.parse::<u64>()) else {
        return false;
    };
    name == format!("{TEMPORARY_PREFIX}{process}-{sequence}{TEMPORARY_SUFFIX}")
}

/// A destination whose parent directory has been resolved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Destination {
    path: PathBuf,
    parent: PathBuf,
}

impl Destination {
    /// Resolves a destination in an existing parent directory.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, Error> {
        let requested = path.as_ref();
        let file_name = requested.file_name().ok_or_else(|| {
            Error::Refused(Refusal::DestinationHasNoFileName {
                destination: requested.to_path_buf(),
            })
        })?;
        let parent = canonical_parent(requested, Operation::ResolveDestination)?;

        Ok(Self {
            path: parent.join(file_name),
            parent,
        })
    }

    /// Returns the resolved destination path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Creates an empty same-directory temporary file.
    pub fn stage(&self) -> Result<StagedWrite, Error> {
        for _ in 0..CREATE_ATTEMPTS {
            let temporary = self.parent.join(temporary_name());
            match create_stage(&temporary) {
                Ok(file) => {
                    return Ok(StagedWrite {
                        file: Some(file),
                        temporary: Some(temporary),
                        destination: self.clone(),
                    });
                }
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
                Err(source) => {
                    return Err(Error::io(Operation::CreateTemporary, temporary, source));
                }
            }
        }

        Err(Error::io(
            Operation::CreateTemporary,
            self.parent.clone(),
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "temporary filename attempts exhausted",
            ),
        ))
    }

    /// Writes and publishes complete replacement bytes.
    ///
    /// An error from [`Operation::SyncDirectory`] occurs after replacement:
    /// the new bytes are visible, but their survival across a crash is not
    /// proved. All earlier errors leave the destination unchanged.
    pub fn publish<F>(&self, write: F) -> Result<(), Error>
    where
        F: FnOnce(&mut StagedWrite) -> io::Result<()>,
    {
        let mut staged = self.stage()?;
        if let Err(source) = write(&mut staged) {
            return Err(staged.fail(Operation::WriteTemporary, source));
        }
        staged.commit()
    }

    /// Publishes an already-written regular file from the same directory.
    ///
    /// Different parent directories are refused even when they happen to be
    /// mounted on the same filesystem. Keeping the source beside the
    /// destination makes the atomicity contract explicit and checkable before
    /// any mutation occurs. An error from [`Operation::SyncDirectory`] occurs
    /// after the source has replaced the destination.
    pub fn replace_from(&self, source: impl AsRef<Path>) -> Result<(), Error> {
        let requested = source.as_ref();
        let file_name = requested.file_name().ok_or_else(|| {
            Error::Refused(Refusal::SourceHasNoFileName {
                source: requested.to_path_buf(),
            })
        })?;
        let parent = canonical_parent(requested, Operation::ResolveSource)?;
        let source = parent.join(file_name);

        if parent != self.parent {
            return Err(Error::Refused(Refusal::DifferentDirectories {
                source,
                destination: self.path.clone(),
            }));
        }
        if source == self.path {
            return Err(Error::Refused(Refusal::SourceIsDestination {
                path: source,
            }));
        }

        let metadata = fs::symlink_metadata(&source)
            .map_err(|error| Error::io(Operation::InspectSource, &source, error))?;
        if !metadata.file_type().is_file() {
            return Err(Error::Refused(Refusal::SourceIsNotRegularFile { source }));
        }

        // The handle closes where this block ends, which is before the rename
        // on every path out. A scope says that once; a `drop` call has to be
        // repeated on each exit, and reads as a no-op on targets whose `File`
        // carries no `Drop` at all.
        {
            let file = OpenOptions::new()
                .write(true)
                .open(&source)
                .map_err(|error| Error::io(Operation::OpenSource, &source, error))?;
            file.sync_all()
                .map_err(|error| Error::io(Operation::SyncFile, &source, error))?;
        }

        fs::rename(&source, &self.path)
            .map_err(|error| Error::io(Operation::Replace, &source, error))?;
        sync_directory(&self.parent)
    }
}

/// A same-directory temporary file that has not yet been published.
#[derive(Debug)]
pub struct StagedWrite {
    file: Option<File>,
    temporary: Option<PathBuf>,
    destination: Destination,
}

impl StagedWrite {
    /// Returns the private temporary path.
    #[must_use]
    pub fn temporary_path(&self) -> &Path {
        self.temporary
            .as_deref()
            .expect("a staged write always has a temporary path")
    }

    /// Synchronizes and atomically publishes the staged bytes.
    ///
    /// An error from [`Operation::SyncDirectory`] occurs after replacement:
    /// the destination contains the staged bytes, but directory durability is
    /// unproved.
    pub fn commit(mut self) -> Result<(), Error> {
        let temporary = self
            .temporary
            .as_ref()
            .expect("a staged write always has a temporary path")
            .clone();
        // The handle closes where this block ends, which is ahead of both exits
        // below: the failure path hands `self` to `fail`, and the success path
        // renames over the destination. Neither may run while the process still
        // holds the staged bytes open.
        let synced = {
            let file = self
                .file
                .take()
                .expect("a staged write always has an open file");
            file.sync_all()
        };
        if let Err(source) = synced {
            return Err(self.fail(Operation::SyncFile, source));
        }

        let replaced = fs::rename(&temporary, &self.destination.path);
        if let Err(source) = replaced {
            return Err(self.fail(Operation::Replace, source));
        }
        self.temporary.take();

        sync_directory(&self.destination.parent)
    }

    fn fail(mut self, operation: Operation, source: io::Error) -> Error {
        self.file.take();
        let path = self
            .temporary
            .take()
            .unwrap_or_else(|| self.destination.path.clone());
        let cleanup = match fs::remove_file(&path) {
            Ok(()) => None,
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => Some(error),
        };
        Error::Io {
            operation,
            path,
            source,
            cleanup,
        }
    }
}

impl Write for StagedWrite {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.file
            .as_mut()
            .expect("a staged write always has an open file")
            .write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file
            .as_mut()
            .expect("a staged write always has an open file")
            .flush()
    }
}

impl Seek for StagedWrite {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.file
            .as_mut()
            .expect("a staged write always has an open file")
            .seek(position)
    }
}

impl Drop for StagedWrite {
    fn drop(&mut self) {
        self.file.take();
        if let Some(temporary) = self.temporary.take() {
            let _ = fs::remove_file(temporary);
        }
    }
}

/// A publication refusal detected before mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// The destination does not identify a file.
    DestinationHasNoFileName { destination: PathBuf },
    /// The prepared source does not identify a file.
    SourceHasNoFileName { source: PathBuf },
    /// Prepared source and destination are not in one resolved directory.
    DifferentDirectories {
        source: PathBuf,
        destination: PathBuf,
    },
    /// Replacing a path with itself is not a publication.
    SourceIsDestination { path: PathBuf },
    /// Prepared publication accepts regular files only.
    SourceIsNotRegularFile { source: PathBuf },
    /// A rooted path is empty, absolute, or climbs out of its root.
    PathNotRooted { path: PathBuf },
    /// A directory beneath the root is a symlink or not a directory.
    AncestorNotDirectory { path: PathBuf },
    /// The destination exists as something other than a regular file.
    DestinationNotRegularFile { destination: PathBuf },
    /// A publisher layered over this seam declined before any mutation.
    Withheld { path: PathBuf, reason: String },
}

/// The filesystem operation associated with an I/O failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    ResolveDestination,
    PrepareDirectory,
    ResolveSource,
    CreateTemporary,
    WriteTemporary,
    InspectSource,
    OpenSource,
    SyncFile,
    SetMode,
    Replace,
    SyncDirectory,
}

impl fmt::Display for Operation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ResolveDestination => "resolve destination directory",
            Self::PrepareDirectory => "prepare destination directory",
            Self::ResolveSource => "resolve source directory",
            Self::CreateTemporary => "create temporary file",
            Self::WriteTemporary => "write temporary file",
            Self::InspectSource => "inspect source file",
            Self::OpenSource => "open source file",
            Self::SyncFile => "sync file",
            Self::SetMode => "set file mode",
            Self::Replace => "replace destination",
            Self::SyncDirectory => "sync parent directory",
        })
    }
}

/// A typed publication failure.
#[derive(Debug)]
pub enum Error {
    Refused(Refusal),
    Io {
        operation: Operation,
        path: PathBuf,
        source: io::Error,
        cleanup: Option<io::Error>,
    },
}

impl Error {
    fn io(operation: Operation, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
            cleanup: None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(refusal) => write!(formatter, "publication refused: {refusal}"),
            Self::Io {
                operation,
                path,
                source,
                cleanup,
            } => {
                write!(
                    formatter,
                    "failed to {operation} at {}: {source}",
                    path.display()
                )?;
                if let Some(cleanup) = cleanup {
                    write!(formatter, "; temporary cleanup also failed: {cleanup}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Refused(_) => None,
            Self::Io { source, .. } => Some(source),
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DestinationHasNoFileName { destination } => {
                write!(
                    formatter,
                    "destination {} has no file name",
                    destination.display()
                )
            }
            Self::SourceHasNoFileName { source } => {
                write!(formatter, "source {} has no file name", source.display())
            }
            Self::DifferentDirectories {
                source,
                destination,
            } => write!(
                formatter,
                "source {} and destination {} are not in the same directory",
                source.display(),
                destination.display()
            ),
            Self::SourceIsDestination { path } => {
                write!(
                    formatter,
                    "source and destination are both {}",
                    path.display()
                )
            }
            Self::SourceIsNotRegularFile { source } => {
                write!(
                    formatter,
                    "source {} is not a regular file",
                    source.display()
                )
            }
            Self::PathNotRooted { path } => write!(
                formatter,
                "path {} is not a nonempty relative path beneath its root",
                path.display()
            ),
            Self::AncestorNotDirectory { path } => write!(
                formatter,
                "{} is a symlink or not a directory",
                path.display()
            ),
            Self::DestinationNotRegularFile { destination } => write!(
                formatter,
                "destination {} exists and is not a regular file",
                destination.display()
            ),
            Self::Withheld { path, reason } => {
                write!(formatter, "{} was withheld: {reason}", path.display())
            }
        }
    }
}

fn canonical_parent(path: &Path, operation: Operation) -> Result<PathBuf, Error> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    fs::canonicalize(parent).map_err(|error| Error::io(operation, parent, error))
}

fn temporary_name() -> OsString {
    let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
    OsString::from(format!(
        "{TEMPORARY_PREFIX}{}-{sequence}{TEMPORARY_SUFFIX}",
        std::process::id()
    ))
}

/// Create one stage that is owner-only from creation, so no published byte is
/// ever reachable by another principal before its owner chooses a mode.
#[cfg(any(unix, windows))]
fn create_stage(temporary: &Path) -> io::Result<File> {
    sill::authority::create_private_file(temporary)
}

/// Wasm contract checks have no filesystem principals to exclude.
#[cfg(not(any(unix, windows)))]
fn create_stage(temporary: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary)
}

fn sync_directory(parent: &Path) -> Result<(), Error> {
    sync_directory_handle(parent)
        .map_err(|error| Error::io(Operation::SyncDirectory, parent, error))
}

#[cfg(any(unix, windows))]
fn sync_directory_handle(parent: &Path) -> io::Result<()> {
    sill::authority::Directory::open(parent)?.sync()
}

/// Wasm contract checks have no filesystem principals to exclude.
#[cfg(not(any(unix, windows)))]
fn sync_directory_handle(parent: &Path) -> io::Result<()> {
    OpenOptions::new().read(true).open(parent)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn staged_bytes_are_hidden_until_complete_replacement() {
        let directory = TestDirectory::new();
        let path = directory.path().join("state");
        fs::write(&path, b"old complete bytes").unwrap();
        let destination = Destination::new(&path).unwrap();
        let mut staged = destination.stage().unwrap();

        staged.write_all(b"new ").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"old complete bytes");
        staged.write_all(b"complete bytes").unwrap();
        staged.commit().unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new complete bytes");
        assert_eq!(directory.entries(), vec![OsString::from("state")]);
    }

    #[test]
    fn failed_writer_preserves_destination_and_cleans_temporary() {
        let directory = TestDirectory::new();
        let path = directory.path().join("state");
        fs::write(&path, b"old complete bytes").unwrap();
        let destination = Destination::new(&path).unwrap();

        let error = destination
            .publish(|staged| {
                staged.write_all(b"partial")?;
                Err(io::Error::other("injected writer failure"))
            })
            .unwrap_err();

        assert!(matches!(
            error,
            Error::Io {
                operation: Operation::WriteTemporary,
                cleanup: None,
                ..
            }
        ));
        assert_eq!(fs::read(&path).unwrap(), b"old complete bytes");
        assert_eq!(directory.entries(), vec![OsString::from("state")]);
    }

    #[test]
    fn prepared_file_publishes_new_parent_entry() {
        let directory = TestDirectory::new();
        let source = directory.path().join("prepared");
        let destination_path = directory.path().join("published");
        fs::write(&source, b"prepared bytes").unwrap();
        let destination = Destination::new(&destination_path).unwrap();

        destination.replace_from(&source).unwrap();

        assert!(!source.exists());
        assert_eq!(fs::read(&destination_path).unwrap(), b"prepared bytes");
        assert_eq!(directory.entries(), vec![OsString::from("published")]);
    }

    #[test]
    fn prepared_file_in_another_directory_is_refused_without_mutation() {
        let source_directory = TestDirectory::new();
        let destination_directory = TestDirectory::new();
        let source = source_directory.path().join("prepared");
        let destination_path = destination_directory.path().join("published");
        fs::write(&source, b"prepared bytes").unwrap();
        fs::write(&destination_path, b"old bytes").unwrap();
        let destination = Destination::new(&destination_path).unwrap();
        let source_entries = source_directory.entries();
        let destination_entries = destination_directory.entries();

        let error = destination.replace_from(&source).unwrap_err();

        assert!(matches!(
            error,
            Error::Refused(Refusal::DifferentDirectories { .. })
        ));
        assert_eq!(fs::read(&source).unwrap(), b"prepared bytes");
        assert_eq!(fs::read(&destination_path).unwrap(), b"old bytes");
        assert_eq!(source_directory.entries(), source_entries);
        assert_eq!(destination_directory.entries(), destination_entries);
    }

    #[test]
    fn destination_without_a_file_name_is_refused_without_mutation() {
        let directory = TestDirectory::new();
        let sentinel = directory.path().join("sentinel");
        fs::write(&sentinel, b"untouched").unwrap();
        let requested = directory.path().join("..");
        let entries = directory.entries();

        let error = Destination::new(&requested).unwrap_err();

        assert!(matches!(
            error,
            Error::Refused(Refusal::DestinationHasNoFileName { destination })
                if destination == requested
        ));
        assert_eq!(fs::read(&sentinel).unwrap(), b"untouched");
        assert_eq!(directory.entries(), entries);
    }

    #[test]
    fn source_without_a_file_name_is_refused_without_mutation() {
        let directory = TestDirectory::new();
        let destination_path = directory.path().join("published");
        fs::write(&destination_path, b"old bytes").unwrap();
        let destination = Destination::new(&destination_path).unwrap();
        let requested = directory.path().join("..");
        let entries = directory.entries();

        let error = destination.replace_from(&requested).unwrap_err();

        assert!(matches!(
            error,
            Error::Refused(Refusal::SourceHasNoFileName { source }) if source == requested
        ));
        assert_eq!(fs::read(&destination_path).unwrap(), b"old bytes");
        assert_eq!(directory.entries(), entries);
    }

    #[test]
    fn source_equal_to_destination_is_refused_without_mutation() {
        let directory = TestDirectory::new();
        let destination_path = directory.path().join("published");
        fs::write(&destination_path, b"old bytes").unwrap();
        let destination = Destination::new(&destination_path).unwrap();
        let resolved_destination = destination.path().to_path_buf();
        let entries = directory.entries();

        let error = destination.replace_from(&destination_path).unwrap_err();

        assert!(matches!(
            error,
            Error::Refused(Refusal::SourceIsDestination { path }) if path == resolved_destination
        ));
        assert_eq!(fs::read(&destination_path).unwrap(), b"old bytes");
        assert_eq!(directory.entries(), entries);
    }

    #[test]
    fn non_regular_source_is_refused_without_mutation() {
        let directory = TestDirectory::new();
        let source = directory.path().join("prepared-directory");
        let destination_path = directory.path().join("published");
        fs::create_dir(&source).unwrap();
        fs::write(&destination_path, b"old bytes").unwrap();
        let destination = Destination::new(&destination_path).unwrap();
        let resolved_source = fs::canonicalize(directory.path())
            .unwrap()
            .join("prepared-directory");
        let entries = directory.entries();

        let error = destination.replace_from(&source).unwrap_err();

        assert!(matches!(
            error,
            Error::Refused(Refusal::SourceIsNotRegularFile { source: refused })
                if refused == resolved_source
        ));
        assert_eq!(fs::read(&destination_path).unwrap(), b"old bytes");
        assert!(source.is_dir());
        assert_eq!(directory.entries(), entries);
    }

    #[test]
    fn dropping_an_abandoned_stage_removes_its_temporary() {
        let directory = TestDirectory::new();
        let destination_path = directory.path().join("published");
        fs::write(&destination_path, b"old bytes").unwrap();
        let destination = Destination::new(&destination_path).unwrap();
        let entries = directory.entries();
        let mut staged = destination.stage().unwrap();
        staged.write_all(b"abandoned bytes").unwrap();
        let temporary = staged.temporary_path().to_path_buf();
        assert!(temporary.is_file());
        assert_eq!(sill::authority::owner_mode(&temporary).unwrap(), 0o600);

        drop(staged);

        assert!(!temporary.exists());
        assert_eq!(fs::read(&destination_path).unwrap(), b"old bytes");
        assert_eq!(directory.entries(), entries);
    }

    #[test]
    fn simultaneous_stages_use_distinct_live_temporaries() {
        let directory = TestDirectory::new();
        let destination_path = directory.path().join("published");
        fs::write(&destination_path, b"old bytes").unwrap();
        let destination = Destination::new(&destination_path).unwrap();
        let entries = directory.entries();

        let first = destination.stage().unwrap();
        let second = destination.stage().unwrap();
        let first_path = first.temporary_path().to_path_buf();
        let second_path = second.temporary_path().to_path_buf();

        assert_ne!(first_path, second_path);
        assert!(first_path.is_file());
        assert!(second_path.is_file());

        drop(first);
        drop(second);
        assert_eq!(fs::read(&destination_path).unwrap(), b"old bytes");
        assert_eq!(directory.entries(), entries);
    }

    #[test]
    fn temporary_path_recognition_accepts_only_the_generated_decimal_shape() {
        let directory = TestDirectory::new();
        let destination = Destination::new(directory.path().join("published")).unwrap();
        let staged = destination.stage().unwrap();
        assert!(is_temporary_path(staged.temporary_path()));

        for name in [
            ".spice-atomic-01-2.tmp",
            ".spice-atomic-1-02.tmp",
            ".spice-atomic-1-.tmp",
            ".spice-atomic--1.tmp",
            ".spice-atomic-1-2.tmp.extra",
            "spice-atomic-1-2.tmp",
        ] {
            assert!(!is_temporary_path(Path::new(name)), "admitted {name:?}");
        }
    }

    // Kept local because these are atomic unit tests. Sharing gate's integration-test
    // helper needs a workspace member and an atomic dev-dependency. The gate lattice
    // test filters Cargo.lock to workspace members, so that edge would change its
    // deliberate `("atomic", &[])` row.
    pub(crate) struct TestDirectory(PathBuf);

    impl TestDirectory {
        pub(crate) fn new() -> Self {
            for _ in 0..CREATE_ATTEMPTS {
                let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "spice-atomic-test-{}-{sequence}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("failed to create test directory: {error}"),
                }
            }
            panic!("test directory attempts exhausted");
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }

        pub(crate) fn entries(&self) -> Vec<OsString> {
            let mut entries = fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            entries.sort();
            entries
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
}
