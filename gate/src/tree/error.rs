use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// What prevented a Git source from becoming one immutable gate tree.
#[derive(Debug)]
pub enum TreeError {
    /// The requested path could not be opened as a repository.
    Repository {
        /// Requested repository path.
        path: PathBuf,
        /// What the Git implementation reported.
        evidence: String,
    },
    /// The repository stores references in a format the gate cannot read.
    ReferenceStorage {
        /// Requested repository path.
        path: PathBuf,
        /// The `extensions.refStorage` value Git configured.
        format: String,
    },
    /// The repository has no index to capture.
    IndexAbsent {
        /// Expected index path.
        path: PathBuf,
    },
    /// The captured index could not be decoded.
    IndexUnreadable {
        /// Index path.
        path: PathBuf,
        /// What the Git implementation reported.
        evidence: String,
    },
    /// The index was replaced or changed during selection.
    IndexChanged {
        /// Index path.
        path: PathBuf,
    },
    /// The index carries unresolved merge stages.
    UnmergedIndex {
        /// Conflicted path.
        path: String,
        /// Nonzero index stage.
        stage: u32,
    },
    /// The named revision did not resolve uniquely to one object.
    RevisionUnresolved {
        /// Exact requested revision.
        revision: String,
        /// What the Git implementation reported.
        evidence: String,
    },
    /// The named revision resolved to an object other than a commit.
    RevisionNotCommit {
        /// Exact requested revision.
        revision: String,
        /// Actual object kind.
        actual: String,
    },
    /// A reference changed while its selected commit was materialized.
    RevisionChanged {
        /// Exact requested revision.
        revision: String,
    },
    /// A selected path is not a stable normalized UTF-8 relative path.
    UnnameablePath {
        /// Lossy rendering of the refused bytes.
        path: String,
    },
    /// Two selected paths collide in one filesystem tree.
    PathCollision {
        /// First colliding path.
        first: String,
        /// Second colliding path.
        second: String,
    },
    /// A selected path is a submodule commit rather than local bytes.
    Submodule {
        /// Refused path.
        path: String,
    },
    /// A selected path carries a mode the gate cannot faithfully reproduce.
    UnsupportedMode {
        /// Refused path.
        path: String,
        /// Raw Git mode.
        mode: u32,
    },
    /// A selected Git object was absent or unreadable.
    MissingObject {
        /// Path or revision that required the object.
        path: String,
        /// Requested object identity.
        object: String,
        /// What the object database reported.
        evidence: String,
    },
    /// A selected object's bytes did not reproduce its identity.
    ObjectChanged {
        /// Path or revision that required the object.
        path: String,
        /// Requested object identity.
        expected: String,
        /// Identity of the bytes actually read.
        actual: String,
    },
    /// A selected object had the wrong Git kind for its mode or parent.
    UnexpectedObject {
        /// Path that required the object.
        path: String,
        /// Requested object identity.
        object: String,
        /// Required Git object kind.
        expected: &'static str,
        /// Actual Git object kind.
        actual: String,
    },
    /// A selected commit or tree object was malformed.
    MalformedObject {
        /// Object identity.
        object: String,
        /// What decoding or hashing reported.
        evidence: String,
    },
    /// The private output changed before it could be returned.
    MaterializationChanged {
        /// First affected relative path.
        path: String,
    },
    /// The filesystem refused an operation needed by selection.
    Io {
        /// Operation being attempted.
        operation: &'static str,
        /// Path at which it failed.
        path: PathBuf,
        /// What the filesystem reported.
        source: io::Error,
    },
}

impl TreeError {
    pub(super) fn io(operation: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}

impl fmt::Display for TreeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Repository { path, evidence } => write!(
                formatter,
                "{} is not a readable Git repository: {evidence}",
                path.display()
            ),
            Self::ReferenceStorage { path, format } => write!(
                formatter,
                "{} stores Git references as {format}, which the gate reads only as files; \
                 migrate the repository with `git refs migrate --ref-format=files`, or create \
                 it with `git init --ref-format=files`",
                path.display()
            ),
            Self::IndexAbsent { path } => {
                write!(formatter, "Git index {} is absent", path.display())
            }
            Self::IndexUnreadable { path, evidence } => write!(
                formatter,
                "Git index {} could not be captured: {evidence}",
                path.display()
            ),
            Self::IndexChanged { path } => write!(
                formatter,
                "Git index {} changed while its staged tree was selected",
                path.display()
            ),
            Self::UnmergedIndex { path, stage } => {
                write!(
                    formatter,
                    "staged path {path:?} remains at merge stage {stage}"
                )
            }
            Self::RevisionUnresolved { revision, evidence } => write!(
                formatter,
                "revision {revision:?} did not name exactly one object: {evidence}"
            ),
            Self::RevisionNotCommit { revision, actual } => write!(
                formatter,
                "revision {revision:?} names a {actual}, not a commit"
            ),
            Self::RevisionChanged { revision } => write!(
                formatter,
                "revision source for {revision:?} moved while its tree was selected"
            ),
            Self::UnnameablePath { path } => write!(
                formatter,
                "Git path {path:?} is not a normalized UTF-8 relative path"
            ),
            Self::PathCollision { first, second } => write!(
                formatter,
                "Git paths {first:?} and {second:?} collide in one tree"
            ),
            Self::Submodule { path } => write!(
                formatter,
                "Git path {path:?} is a submodule, which has no local blob to measure"
            ),
            Self::UnsupportedMode { path, mode } => write!(
                formatter,
                "Git path {path:?} carries unsupported mode {mode:o}"
            ),
            Self::MissingObject {
                path,
                object,
                evidence,
            } => write!(
                formatter,
                "Git object {object} for {path:?} is unavailable: {evidence}"
            ),
            Self::ObjectChanged {
                path,
                expected,
                actual,
            } => write!(
                formatter,
                "Git object {expected} for {path:?} read back as {actual}"
            ),
            Self::UnexpectedObject {
                path,
                object,
                expected,
                actual,
            } => write!(
                formatter,
                "Git object {object} for {path:?} is {actual}, not {expected}"
            ),
            Self::MalformedObject { object, evidence } => {
                write!(formatter, "Git object {object} is malformed: {evidence}")
            }
            Self::MaterializationChanged { path } => write!(
                formatter,
                "selected tree path {path:?} changed before publication"
            ),
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "{operation} at {} failed: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for TreeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
