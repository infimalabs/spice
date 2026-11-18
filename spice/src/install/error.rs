use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

use wire::Digest;

use crate::repository::{ResolveError, StateError};

/// Planning, admission, publication, or reversal failure.
#[derive(Debug)]
pub enum InstallError {
    Repository(ResolveError),
    State(StateError),
    Encode(serde_json::Error),
    Decode(serde_json::Error),
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    PathShape(PathBuf),
    PathOutsideRoot(PathBuf),
    ParentChanged(PathBuf),
    GenerationChanged(PathBuf),
    BinaryNotRegular(PathBuf),
    BinaryEmpty(PathBuf),
    /// An existing target whose mode this platform cannot restore on reversal.
    OccupantUnrestorable {
        path: PathBuf,
        mode: u32,
    },
    HookPathUnrepresentable {
        path: PathBuf,
        reason: &'static str,
    },
    /// A macOS symbol input failed to establish the executable it belongs to.
    SymbolsRefused {
        path: PathBuf,
        reason: &'static str,
    },
    /// The repository keeps references in a backend the gate cannot read.
    ReferenceStorage {
        common: PathBuf,
        format: String,
    },
    GitNonUtf8(&'static str),
    BinaryContextChanged,
    DigestMismatch {
        expected: Digest,
        observed: Digest,
    },
    InvalidRecord(String),
    /// A receipt whose install schema has no registered forward crossing and
    /// can only be uninstalled by an executable that supports that schema.
    ForeignRecord {
        path: PathBuf,
        version: u16,
    },
    /// The selected executable carries no admissible linked canvas archive.
    CanvasRefused {
        path: PathBuf,
        reason: String,
    },
    RecordMissing,
    ApplyInProgress,
    UnapplyInProgress,
    Drift {
        path: String,
        observed: Option<Digest>,
    },
}

impl InstallError {
    pub(crate) fn io(operation: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }
}

impl fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Repository(_)
            | Self::State(_)
            | Self::Encode(_)
            | Self::Decode(_)
            | Self::Io { .. }
            | Self::PathShape(_)
            | Self::PathOutsideRoot(_)
            | Self::ParentChanged(_)
            | Self::GenerationChanged(_)
            | Self::BinaryNotRegular(_)
            | Self::BinaryEmpty(_)
            | Self::OccupantUnrestorable { .. }
            | Self::HookPathUnrepresentable { .. }
            | Self::SymbolsRefused { .. } => fmt_input_error(self, formatter),
            Self::ReferenceStorage { .. } | Self::GitNonUtf8(_) => fmt_git_error(self, formatter),
            Self::BinaryContextChanged
            | Self::DigestMismatch { .. }
            | Self::InvalidRecord(_)
            | Self::ForeignRecord { .. }
            | Self::CanvasRefused { .. }
            | Self::RecordMissing
            | Self::ApplyInProgress
            | Self::UnapplyInProgress
            | Self::Drift { .. } => fmt_progress_error(self, formatter),
        }
    }
}

fn fmt_input_error(error: &InstallError, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match error {
        InstallError::Repository(error) => fmt::Display::fmt(error, formatter),
        InstallError::State(error) => fmt::Display::fmt(error, formatter),
        InstallError::Encode(error) => write!(formatter, "install plan did not encode: {error}"),
        InstallError::Decode(error) => write!(formatter, "install record did not decode: {error}"),
        InstallError::Io {
            operation,
            path,
            source,
        } => write!(
            formatter,
            "failed to {operation} at {}: {source}",
            path.display()
        ),
        InstallError::PathShape(path) => write!(
            formatter,
            "install path is not a real regular file or directory: {}",
            path.display()
        ),
        InstallError::PathOutsideRoot(path) => write!(
            formatter,
            "install path resolves outside its canonical permitted root: {}",
            path.display()
        ),
        InstallError::ParentChanged(path) => write!(
            formatter,
            "install destination parent generation changed after planning: {}",
            path.display()
        ),
        InstallError::GenerationChanged(path) => write!(
            formatter,
            "install receipt generation changed before publication: {}",
            path.display()
        ),
        InstallError::BinaryNotRegular(path) => {
            write!(
                formatter,
                "Spice executable is not a regular file: {}",
                path.display()
            )
        }
        InstallError::BinaryEmpty(path) => {
            write!(formatter, "Spice executable is empty: {}", path.display())
        }
        InstallError::OccupantUnrestorable { path, mode } => write!(
            formatter,
            "existing install target {} has mode {mode:#05o}, which this platform cannot restore on reversal",
            path.display()
        ),
        InstallError::HookPathUnrepresentable { path, reason } => write!(
            formatter,
            "installed Spice hook path {path:?} {reason} and cannot be rendered on one shell line",
        ),
        InstallError::SymbolsRefused { path, reason } => {
            write!(formatter, "macOS symbol input {} {reason}", path.display())
        }
        _ => unreachable!("input error formatter received another install error"),
    }
}

fn fmt_git_error(error: &InstallError, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match error {
        InstallError::ReferenceStorage { common, format } => write!(
            formatter,
            "{} stores Git references as {format}, which Spice reads only as files",
            common.display(),
        ),
        InstallError::GitNonUtf8(operation) => {
            write!(
                formatter,
                "Git emitted non-UTF-8 while trying to {operation}"
            )
        }
        _ => unreachable!("Git error formatter received another install error"),
    }
}

fn fmt_progress_error(error: &InstallError, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match error {
        InstallError::BinaryContextChanged => {
            formatter.write_str("Spice executable selection differs from the recorded plan")
        }
        InstallError::DigestMismatch { expected, observed } => write!(
            formatter,
            "install plan digest mismatch: expected {expected}, observed {observed}"
        ),
        InstallError::InvalidRecord(reason) => {
            write!(formatter, "invalid install record: {reason}")
        }
        InstallError::ForeignRecord { path, version } => write!(
            formatter,
            "install record at {} is schema {version}, not this executable's schema {}",
            path.display(),
            super::RECORD_VERSION,
        ),
        InstallError::CanvasRefused { path, reason } => write!(
            formatter,
            "Spice executable {} carries no admissible linked canvas archive: {reason}",
            path.display(),
        ),
        InstallError::RecordMissing => formatter.write_str("no Spice install record exists"),
        InstallError::ApplyInProgress => formatter.write_str("Spice installation is incomplete"),
        InstallError::UnapplyInProgress => formatter.write_str("Spice unapply is incomplete"),
        InstallError::Drift { path, observed } => write!(
            formatter,
            "installed path drifted at {path}; observed {}",
            observed
                .as_ref()
                .map_or_else(|| "absence".to_owned(), ToString::to_string)
        ),
        _ => unreachable!("progress error formatter received another install error"),
    }
}

impl Error for InstallError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Repository(error) => Some(error),
            Self::State(error) => Some(error),
            Self::Encode(error) | Self::Decode(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<ResolveError> for InstallError {
    fn from(error: ResolveError) -> Self {
        Self::Repository(error)
    }
}

impl From<StateError> for InstallError {
    fn from(error: StateError) -> Self {
        Self::State(error)
    }
}
