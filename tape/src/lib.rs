//! Verbatim vendor capture.
//!
//! Tape commits bytes before anything interprets them. An append is durable
//! before the caller is allowed to decode it, so a decoder that panics, a
//! dialect that cannot classify, and a resident that dies all leave the
//! original bytes intact and re-readable.
//!
//! Tape is not authority. It never decides what bytes mean, never merges two
//! captures, and never rewrites a member it already sealed. The one promise it
//! makes is positional: a [`Locus`] handed back by [`Tape::tee`] resolves to
//! the same bytes forever, whether the member holding it is still plaintext or
//! has since been rotated into an immutable archive.
//!
//! A tape directory holds plaintext for the member currently accepting
//! appends, one seekable Zstandard archive (`NNNNNNNNNNNN.zst`) per member
//! [`Tape::seal`] rotated, and one index naming every sealed member and its
//! plaintext length. Member-numbered plaintext makes recovery unambiguous:
//! a plaintext file whose member the index already lists is the residue of a
//! seal that published and then stopped, and finishing that seal is a
//! verified deletion rather than a guess.
//!
//! Storage is chosen by which file is present, never recorded in the index:
//! readers resolve a member's plaintext first, then its archive, and treat
//! only a missing file as absent. Sealing is a mechanism without policy.
//! Nothing in this crate decides when a member is large or old enough to
//! seal, and nothing converts existing members on its own.

#![forbid(unsafe_code)]

mod archive;
mod file;
mod read;
mod seal;
mod tee;

pub use file::{ArchivedFile, ArchivedReader, archive_file};
pub use read::{Locus, Window};
pub use seal::{BoundedExtents, Extent, Index, Member, Seal};

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sill::{Acquisition, Availability, LockFile, LockName, Possession};

/// Name of the index within a tape directory.
const INDEX: &str = "index";

/// Name of the durable source-generation identity.
const SOURCE: &str = "source";

/// Header of the one source identity form tape writes.
const SOURCE_FORMAT: &str = "tape-source 1";
/// Maximum bytes admitted while reading the fixed source identity document.
const SOURCE_READ_LIMIT: u64 = 64;

/// Name of the writer lock within a tape directory.
const LOCK: &str = "tape.lock";

/// Mode for every file tape publishes.
#[cfg(unix)]
const PRIVATE: u32 = 0o600;

/// Mode for the tape directory itself.
#[cfg(unix)]
const PRIVATE_DIRECTORY: u32 = 0o700;

/// Distinguishes concurrent stagers within one process.
static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

/// Leading text of every staging name.
const STAGED_PREFIX: &str = ".tape-staged-";

/// Trailing text of every staging name.
const STAGED_SUFFIX: &str = ".tmp";

/// One capture directory.
///
/// A tape is a directory rather than a file because sealing publishes whole
/// members by rename, and rename is only atomic within one directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tape {
    root: PathBuf,
}

/// Opaque identity of one durable tape generation.
///
/// The value changes when a tape directory is replaced, but not when members
/// within one tape are sealed. Its spelling is deliberately opaque to callers;
/// equality and serialization are the complete public contract.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Serialize)]
#[serde(transparent)]
pub struct SourceId(String);

impl SourceId {
    fn fresh() -> Self {
        Self(uuid::Uuid::new_v4().hyphenated().to_string())
    }

    /// Canonical opaque spelling used in durable checkpoints.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::str::FromStr for SourceId {
    type Err = SourceIdError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let parsed = uuid::Uuid::parse_str(text).map_err(|_| SourceIdError)?;
        let canonical = parsed.hyphenated().to_string();
        if canonical != text {
            return Err(SourceIdError);
        }
        Ok(Self(canonical))
    }
}

impl<'de> serde::Deserialize<'de> for SourceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let text = <String as serde::Deserialize>::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// A source identity was not in tape's one current spelling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceIdError;

impl fmt::Display for SourceIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("source identity is not a canonical UUID")
    }
}

impl std::error::Error for SourceIdError {}

impl Tape {
    /// Open a tape, creating the directory when it does not yet exist.
    ///
    /// # Errors
    ///
    /// Returns an I/O failure when the directory cannot be created or
    /// resolved.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, Error> {
        let requested = root.as_ref();
        create_directory(requested)?;
        let root = fs::canonicalize(requested)
            .map_err(|error| Error::io(Operation::OpenTape, requested, error))?;

        let tape = Self { root };
        tape.prepare_source()?;
        Ok(tape)
    }

    /// Open an existing tape without creating or recovering anything.
    ///
    /// This is the observer door. A missing directory is an error rather than
    /// an invitation to create one, so a read-only surface cannot leave an
    /// empty tape behind merely by looking in the wrong place.
    ///
    /// # Errors
    ///
    /// Returns an I/O failure when the path is absent, is not a directory, or
    /// cannot be resolved.
    pub fn open_existing(root: impl AsRef<Path>) -> Result<Self, Error> {
        let requested = root.as_ref();
        let metadata = fs::symlink_metadata(requested)
            .map_err(|error| Error::io(Operation::OpenTape, requested, error))?;
        if !metadata.is_dir() {
            return Err(Error::io(
                Operation::OpenTape,
                requested,
                io::Error::new(io::ErrorKind::InvalidInput, "tape root is not a directory"),
            ));
        }
        let root = fs::canonicalize(requested)
            .map_err(|error| Error::io(Operation::OpenTape, requested, error))?;

        Ok(Self { root })
    }

    /// The directory holding this tape.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Durable identity of this exact tape generation.
    ///
    /// This read never creates or repairs an identity. A missing, unreadable,
    /// or malformed identity is a failure instead of a guess from paths or
    /// metadata.
    ///
    /// # Errors
    ///
    /// Returns an I/O failure when the identity cannot be read, or a refusal
    /// when its bytes are not the one form tape publishes.
    pub fn source_id(&self) -> Result<SourceId, Error> {
        let path = self.source_path();
        let file =
            File::open(&path).map_err(|error| Error::io(Operation::ReadSource, &path, error))?;
        let metadata = file
            .metadata()
            .map_err(|error| Error::io(Operation::ReadSource, &path, error))?;
        if metadata.len() > SOURCE_READ_LIMIT {
            return Err(Error::Refused(Refusal::SourceReadLimit {
                limit: SOURCE_READ_LIMIT,
                observed: metadata.len(),
            }));
        }
        let mut bytes = Vec::with_capacity(SOURCE_READ_LIMIT as usize + 1);
        file.take(SOURCE_READ_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| Error::io(Operation::ReadSource, &path, error))?;
        if bytes.len() as u64 > SOURCE_READ_LIMIT {
            return Err(Error::Refused(Refusal::SourceReadLimit {
                limit: SOURCE_READ_LIMIT,
                observed: bytes.len() as u64,
            }));
        }
        let text = String::from_utf8(bytes).map_err(|error| {
            Error::io(
                Operation::ReadSource,
                &path,
                io::Error::new(io::ErrorKind::InvalidData, error),
            )
        })?;
        parse_source(&text)
    }

    /// Plaintext path for one member.
    fn plain_path(&self, member: u64) -> PathBuf {
        self.root
            .join(format!("{member:0width$}.plain", width = seal::DIGITS))
    }

    /// Seekable archive path for one member.
    fn archive_path(&self, member: u64) -> PathBuf {
        self.root
            .join(format!("{member:0width$}.zst", width = seal::DIGITS))
    }

    /// Path of the index.
    fn index_path(&self) -> PathBuf {
        self.root.join(INDEX)
    }

    /// Path of the durable source identity.
    fn source_path(&self) -> PathBuf {
        self.root.join(SOURCE)
    }

    /// Open the current source identity, or initialize an empty directory as a
    /// new tape.
    ///
    /// Existing generation state without a source refuses. Source creation is
    /// initialization, never recovery of an earlier tape.
    fn prepare_source(&self) -> Result<(), Error> {
        match self.source_id() {
            Ok(_) => return self.sync_directory(),
            Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        let _possession = self.possess()?;
        let missing = match self.source_id() {
            Ok(_) => return self.sync_directory(),
            Err(error) => {
                if matches!(
                    &error,
                    Error::Io { source, .. } if source.kind() == io::ErrorKind::NotFound
                ) {
                    error
                } else {
                    return Err(error);
                }
            }
        };
        if self.has_generation_state()? {
            return Err(missing);
        }

        let source = SourceId::fresh();
        let bytes = format!("{SOURCE_FORMAT}\n{source}\n");
        self.publish(&self.source_path(), bytes.as_bytes())?;
        self.sync_directory()
    }

    /// Whether this directory already contains state belonging to a tape.
    fn has_generation_state(&self) -> Result<bool, Error> {
        let entries = fs::read_dir(&self.root)
            .map_err(|error| Error::io(Operation::OpenTape, &self.root, error))?;
        for entry in entries {
            let entry = entry.map_err(|error| Error::io(Operation::OpenTape, &self.root, error))?;
            let name = entry.file_name();
            let transient = name.to_str().is_some_and(|name| {
                name == LOCK || (name.starts_with(STAGED_PREFIX) && name.ends_with(STAGED_SUFFIX))
            });
            if !transient {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Take the writer lock, refusing rather than blocking when it is held.
    ///
    /// Appending and sealing both mutate the same member, so they serialize
    /// against each other. Refusing a contended lock keeps a stalled peer from
    /// turning into an unbounded wait inside a capture path.
    fn possess(&self) -> Result<Possession, Error> {
        let name = LockName::new("tape").map_err(|_| Error::Refused(Refusal::LockNameRejected))?;
        let path = self.root.join(LOCK);
        let lock = LockFile::new(name, &path);

        match lock
            .try_acquire()
            .map_err(|error| Error::io(Operation::Possess, path, error))?
        {
            Acquisition::Acquired(possession) => Ok(possession),
            Acquisition::Contended => Err(Error::Refused(Refusal::Contended)),
        }
    }

    /// Observe whether another process currently holds this tape's writer lock.
    ///
    /// Missing lock files are available. This observer never creates or writes
    /// lock state, so inventorying an inactive tape cannot change it.
    ///
    /// # Errors
    ///
    /// Returns an I/O failure when an existing lock file cannot be inspected.
    pub fn writer_contended(&self) -> Result<bool, Error> {
        let name = LockName::new("tape").map_err(|_| Error::Refused(Refusal::LockNameRejected))?;
        let path = self.root.join(LOCK);
        let lock = LockFile::new(name, &path);
        lock.availability()
            .map(|availability| availability == Availability::Held)
            .map_err(|error| Error::io(Operation::Possess, path, error))
    }

    /// Synchronize the tape directory itself.
    ///
    /// Synchronizing a file commits its bytes; it does not commit the name the
    /// bytes answer to. A member created and written but whose directory entry
    /// is still only in the page cache is a member that a power loss can leave
    /// absent while every byte of it survives, which would strand a locus that
    /// tape had already promised.
    fn sync_directory(&self) -> Result<(), Error> {
        sill::authority::Directory::open(&self.root)
            .and_then(|directory| directory.sync())
            .map_err(|error| Error::io(Operation::SyncDirectory, &self.root, error))
    }

    /// Publish bytes into this tape's directory at owner-only mode.
    ///
    /// The bytes are staged here and then handed to [`atomic`] for
    /// replacement, rather than written through `Destination::publish`,
    /// because staging here is what lets the mode be set before any byte is
    /// visible under the final name.
    fn publish(&self, destination: &Path, bytes: &[u8]) -> Result<(), Error> {
        let staged = self.root.join(temporary_name());
        // The handle closes where this block ends, which is ahead of both the
        // unlink below and the rename [`atomic`] performs: neither may run
        // while this process still holds the staged bytes open.
        let written = {
            let mut file = create_private(&staged)
                .map_err(|error| Error::io(Operation::StagePublication, &staged, error))?;
            file.write_all(bytes).and_then(|()| file.sync_all())
        };
        if let Err(error) = written {
            let _ = fs::remove_file(&staged);
            return Err(Error::io(Operation::StagePublication, staged, error));
        }
        self.install(&staged, destination)
    }

    /// Atomically replace `destination` with a closed, synchronized staged
    /// file, removing the staged file when the replacement fails.
    fn install(&self, staged: &Path, destination: &Path) -> Result<(), Error> {
        let replaced =
            atomic::Destination::new(destination).and_then(|target| target.replace_from(staged));
        if let Err(error) = replaced {
            let _ = fs::remove_file(staged);
            return Err(Error::Publish(Box::new(error)));
        }
        Ok(())
    }
}

/// Create a file readable and writable only by its owner.
#[cfg(unix)]
fn create_private(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE)
        .open(path)
}

/// Create a file, on a target without Unix permission bits.
#[cfg(not(unix))]
fn create_private(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Open a member's plaintext for appending, creating it privately if absent.
fn open_member(path: &Path) -> io::Result<File> {
    match create_private_append(path) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => open_private_append(path),
        Err(error) => Err(error),
    }
}

/// Create a member's plaintext for appending, failing if it exists.
#[cfg(unix)]
fn create_private_append(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .append(true)
        .create_new(true)
        .mode(PRIVATE)
        .open(path)
}

/// Create a member's plaintext for appending, without Unix permission bits.
#[cfg(not(unix))]
fn create_private_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().append(true).create_new(true).open(path)
}

/// Open a member's plaintext for appending, creating it privately.
#[cfg(unix)]
fn open_private_append(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(PRIVATE)
        .open(path)
}

/// Open a member's plaintext for appending, without Unix permission bits.
#[cfg(not(unix))]
fn open_private_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().append(true).create(true).open(path)
}

/// Create the tape directory, private to its owner, if it is absent.
fn create_directory(path: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    let result = {
        use std::os::unix::fs::DirBuilderExt;

        fs::DirBuilder::new()
            .recursive(true)
            .mode(PRIVATE_DIRECTORY)
            .create(path)
    };
    #[cfg(not(unix))]
    let result = fs::DirBuilder::new().recursive(true).create(path);

    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(Error::io(Operation::OpenTape, path, error)),
    }
}

/// A staging name that cannot collide with a member, an index, or a lock.
fn temporary_name() -> String {
    let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
    format!(
        "{STAGED_PREFIX}{}-{sequence}{STAGED_SUFFIX}",
        std::process::id()
    )
}

/// Parse the one framed source identity spelling tape publishes.
fn parse_source(text: &str) -> Result<SourceId, Error> {
    let Some(body) = text.strip_suffix('\n') else {
        return Err(Error::Refused(Refusal::MalformedSource {
            line: text.lines().next_back().unwrap_or("").to_owned(),
        }));
    };
    let mut lines = body.split('\n');
    if lines.next() != Some(SOURCE_FORMAT) {
        return Err(Error::Refused(Refusal::MalformedSource {
            line: body.lines().next().unwrap_or("").to_owned(),
        }));
    }
    let identity = lines.next().ok_or_else(|| {
        Error::Refused(Refusal::MalformedSource {
            line: String::new(),
        })
    })?;
    if let Some(extra) = lines.next() {
        return Err(Error::Refused(Refusal::MalformedSource {
            line: extra.to_owned(),
        }));
    }
    identity.parse().map_err(|_| {
        Error::Refused(Refusal::MalformedSource {
            line: identity.to_owned(),
        })
    })
}

/// The tape operation an I/O failure was attempting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    /// Opening or creating the tape directory.
    OpenTape,
    /// Taking the writer lock.
    Possess,
    /// Appending verbatim bytes.
    Append,
    /// Reading plaintext for a locus or a seal.
    ReadPlaintext,
    /// Removing plaintext whose member is sealed.
    RemovePlaintext,
    /// Reading a seekable member archive.
    ReadArchive,
    /// Compressing a member's plaintext into a staged archive.
    PrepareArchive,
    /// Comparing a staged archive against its plaintext.
    VerifyArchive,
    /// Removing staged files an interrupted holder left behind.
    RemoveStaged,
    /// Staging bytes for publication.
    StagePublication,
    /// Reading the index.
    ReadIndex,
    /// Inspecting the current member extents.
    InspectExtents,
    /// Reading the durable source-generation identity.
    ReadSource,
    /// Synchronizing the tape directory.
    SyncDirectory,
}

impl fmt::Display for Operation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::OpenTape => "open tape directory",
            Self::Possess => "take the tape writer lock",
            Self::Append => "append verbatim bytes",
            Self::ReadPlaintext => "read plaintext",
            Self::RemovePlaintext => "remove sealed plaintext",
            Self::ReadArchive => "read member archive",
            Self::PrepareArchive => "prepare member archive",
            Self::VerifyArchive => "verify member archive",
            Self::RemoveStaged => "remove staged residue",
            Self::StagePublication => "stage publication",
            Self::ReadIndex => "read index",
            Self::InspectExtents => "inspect tape member extents",
            Self::ReadSource => "read tape source identity",
            Self::SyncDirectory => "synchronize the tape directory",
        })
    }
}

/// A tape refusal: an answer about something tape declined to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// Another writer holds the tape.
    Contended,
    /// The writer lock name was rejected by the lock floor.
    LockNameRejected,
    /// The locus names a member this tape has never issued.
    UnknownMember {
        /// Member the locus named.
        member: u64,
    },
    /// The locus reaches past the end of its member.
    OutOfRange {
        /// Locus as presented.
        locus: Locus,
        /// Bytes the member actually holds.
        length: u64,
    },
    /// The index does not parse as the current format.
    MalformedIndex {
        /// The line that did not parse.
        line: String,
    },
    /// A bounded reader refused to load a larger index generation.
    IndexReadLimit {
        /// Maximum index bytes admitted by the caller.
        limit: u64,
        /// Bytes observed before reading the index.
        observed: u64,
    },
    /// A bounded reader refused to materialize another extent.
    ExtentReadLimit {
        /// Maximum extents admitted by the caller.
        limit: usize,
        /// First extent count outside that maximum.
        observed: usize,
        /// Exact index bytes read before the count was known.
        index_bytes: u64,
    },
    /// The source-generation identity is not the one form tape writes.
    MalformedSource {
        /// First non-current line, or the final line when framing is truncated.
        line: String,
    },
    /// A source identity exceeds the fixed document read bound.
    SourceReadLimit {
        /// Maximum source document bytes admitted by tape.
        limit: u64,
        /// Bytes observed before the read was refused.
        observed: u64,
    },
    /// No plaintext or archive holds a member the read expected, including
    /// after a full retry against concurrent seals.
    MemberVanished {
        /// Member that could not be resolved.
        member: u64,
    },
    /// Plaintext beside an index entry holds a different length than the
    /// index names, so it is neither the member's residue nor its source.
    ResidueMismatch {
        /// Member the index names.
        member: u64,
        /// Plaintext length the index names.
        indexed: u64,
        /// Plaintext length on disk.
        observed: u64,
    },
    /// A prepared archive did not reproduce its member's plaintext exactly,
    /// so it was not published.
    ArchiveMismatch {
        /// Member whose archive was refused.
        member: u64,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Contended => formatter.write_str("another writer holds this tape"),
            Self::LockNameRejected => {
                formatter.write_str("the lock floor rejected the tape lock name")
            }
            Self::UnknownMember { member } => {
                write!(formatter, "this tape has not issued member {member}")
            }
            Self::OutOfRange { locus, length } => write!(
                formatter,
                "{locus} reaches past the {length} byte(s) member {} holds",
                locus.member
            ),
            Self::MalformedIndex { line } => {
                write!(
                    formatter,
                    "the index line {line:?} is not the current format"
                )
            }
            Self::IndexReadLimit { limit, observed } => write!(
                formatter,
                "the {observed}-byte tape index exceeds the {limit}-byte read limit"
            ),
            Self::ExtentReadLimit {
                limit,
                observed,
                index_bytes,
            } => write!(
                formatter,
                "tape extent {observed} exceeds the {limit}-extent read limit after {index_bytes} index bytes"
            ),
            Self::MalformedSource { line } => write!(
                formatter,
                "the source identity line {line:?} is not the current format"
            ),
            Self::SourceReadLimit { limit, observed } => write!(
                formatter,
                "the {observed}-byte tape source identity exceeds the {limit}-byte read limit"
            ),
            Self::MemberVanished { member } => write!(
                formatter,
                "no plaintext or archive storage holds member {member}"
            ),
            Self::ResidueMismatch {
                member,
                indexed,
                observed,
            } => write!(
                formatter,
                "plaintext for sealed member {member} holds {observed} byte(s) where the index names {indexed}"
            ),
            Self::ArchiveMismatch { member } => write!(
                formatter,
                "the prepared archive for member {member} does not reproduce its plaintext"
            ),
        }
    }
}

/// A typed tape failure.
#[derive(Debug)]
pub enum Error {
    /// Tape declined; nothing was captured, sealed, or removed.
    Refused(Refusal),
    /// Publication through the filesystem primitive failed.
    Publish(Box<atomic::Error>),
    /// An I/O failure, named by the operation attempting it.
    Io {
        /// What tape was doing.
        operation: Operation,
        /// Path involved.
        path: PathBuf,
        /// Underlying failure.
        source: io::Error,
    },
}

impl Error {
    fn io(operation: Operation, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }

    /// The refusal this error carries, when it refused.
    #[must_use]
    pub fn refusal(&self) -> Option<&Refusal> {
        match self {
            Self::Refused(refusal) => Some(refusal),
            Self::Publish(_) | Self::Io { .. } => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(refusal) => write!(formatter, "tape refused: {refusal}"),
            Self::Publish(error) => write!(formatter, "tape publication failed: {error}"),
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "failed to {operation} at {}: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Refused(_) => None,
            Self::Publish(error) => Some(error),
            Self::Io { source, .. } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CREATE_ATTEMPTS: usize = 128;

    fn create_scratch_root(candidates: impl Iterator<Item = PathBuf>) -> PathBuf {
        for root in candidates.take(CREATE_ATTEMPTS) {
            match fs::create_dir(&root) {
                Ok(()) => return root,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("create a scratch root at {}: {error}", root.display()),
            }
        }
        panic!("no scratch root was free within {CREATE_ATTEMPTS} attempts");
    }

    pub(crate) struct Scratch {
        root: PathBuf,
        removed: bool,
    }

    impl Scratch {
        pub(crate) fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let directory = std::env::temp_dir();
            let process = std::process::id();
            let root = create_scratch_root(std::iter::repeat_with(|| {
                let unique = NEXT.fetch_add(1, Ordering::Relaxed);
                directory.join(format!("spice-tape-{label}-{process}-{unique}"))
            }));
            Self {
                root,
                removed: false,
            }
        }

        pub(crate) fn path(&self) -> &Path {
            &self.root
        }

        pub(crate) fn remove(mut self) {
            fs::remove_dir_all(&self.root).unwrap_or_else(|error| {
                panic!(
                    "remove the scratch root at {}: {error}",
                    self.root.display()
                )
            });
            self.assert_gone();
            self.removed = true;
        }

        fn assert_removed(mut self) {
            self.assert_gone();
            self.removed = true;
        }

        fn assert_gone(&self) {
            assert!(
                !self.root.exists(),
                "the scratch root outlived its removal at {}",
                self.root.display()
            );
        }
    }

    impl Drop for Scratch {
        /// Attempt cleanup during unwinding without replacing the first panic.
        fn drop(&mut self) {
            if !self.removed {
                let _ = fs::remove_dir_all(&self.root);
            }
        }
    }

    /// Contention is tested here rather than beside the other fixtures because
    /// holding the writer lock without appending requires [`Tape::possess`],
    /// which is deliberately not part of the public surface.
    #[test]
    fn a_second_writer_is_refused_rather_than_left_to_wait() {
        let scratch = Scratch::new("contended");
        let tape = Tape::open(scratch.path()).expect("tape opens");

        let held = tape.possess().expect("the first writer takes the lock");

        assert_eq!(
            Tape::open(tape.root()).expect("a current tape reopens while its writer is active"),
            tape,
            "reopening a current generation does not compete with capture"
        );
        assert!(
            tape.writer_contended()
                .expect("the held writer lock is observable")
        );

        for refused in [
            tape.tee(b"blocked"),
            tape.seal().map(|_| Locus::new(0, 0, 0)),
        ] {
            let error = refused.expect_err("a second writer is refused");
            assert!(
                matches!(error.refusal(), Some(Refusal::Contended)),
                "got {error}"
            );
        }

        drop(held);
        tape.tee(b"admitted")
            .expect("the lock is released with its possession");
        assert!(
            !tape
                .writer_contended()
                .expect("the released writer lock is observable")
        );

        scratch.remove();
    }

    #[test]
    fn source_identity_reads_are_bounded() {
        let scratch = Scratch::new("bounded-source");
        let tape = Tape::open(scratch.path()).expect("tape opens");
        fs::write(
            tape.source_path(),
            vec![b'x'; SOURCE_READ_LIMIT as usize + 1],
        )
        .expect("oversized source plants");

        let error = tape
            .source_id()
            .expect_err("oversized source identity refuses");
        assert!(matches!(
            error.refusal(),
            Some(Refusal::SourceReadLimit {
                limit: SOURCE_READ_LIMIT,
                observed
            }) if *observed == SOURCE_READ_LIMIT + 1
        ));

        scratch.remove();
    }

    #[test]
    fn generation_state_without_a_source_is_not_initialized_as_a_new_tape() {
        let scratch = Scratch::new("missing-source");
        let tape = Tape::open(scratch.path()).expect("tape opens");
        tape.tee(b"captured").expect("generation state is durable");
        fs::remove_file(tape.source_path()).expect("source is removed");

        let error = Tape::open(scratch.path()).expect_err("missing source refuses reopen");
        assert!(matches!(
            error,
            Error::Io {
                operation: Operation::ReadSource,
                source,
                ..
            } if source.kind() == io::ErrorKind::NotFound
        ));
        assert!(
            !tape.source_path().exists(),
            "reopening state without a source synthesized an identity"
        );
        scratch.remove();
    }

    /// A tee that cannot commit the member's name has not captured anything a
    /// locus could promise, so the failure is returned rather than assumed.
    #[test]
    fn a_directory_synchronization_failure_is_surfaced() {
        let scratch = Scratch::new("unsyncable");
        let tape = Tape::open(scratch.path()).expect("tape opens");
        tape.tee(b"captured").expect("the first append is captured");
        tape.sync_directory()
            .expect("an intact directory synchronizes");

        fs::remove_dir_all(scratch.path()).expect("the directory is removed underneath the tape");

        let error = tape
            .sync_directory()
            .expect_err("a directory that cannot be opened is not a success");
        assert!(
            matches!(
                error,
                Error::Io {
                    operation: Operation::SyncDirectory,
                    ..
                }
            ),
            "got {error}",
        );
        scratch.assert_removed();
    }

    #[test]
    fn every_staging_name_is_distinct_and_cannot_be_mistaken_for_a_member() {
        let first = temporary_name();
        let second = temporary_name();

        assert_ne!(first, second, "concurrent stagers must not collide");
        for name in [first, second] {
            assert!(name.starts_with('.'), "{name} is not hidden");
            assert!(name.ends_with(".tmp"), "{name} is not marked temporary");
            assert!(
                !name.ends_with(".zst") && !name.ends_with(".plain") && name != INDEX,
                "{name} could be mistaken for tape's own content"
            );
        }
    }
}
