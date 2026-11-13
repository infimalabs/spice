use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;

use sill::{SpecError, WaitError};

/// Why a sealed environment was not established or executed.
#[derive(Debug)]
pub enum ContainerError {
    RuntimeNotAbsolute(PathBuf),
    InspectRuntime {
        path: PathBuf,
        source: io::Error,
    },
    RuntimeNotRegular(PathBuf),
    RuntimeNotExecutable(PathBuf),
    RuntimeProbeSpec(SpecError),
    SpawnRuntimeProbe(io::Error),
    WaitRuntimeProbe(WaitError),
    RuntimeDialectNotAttested {
        observed: String,
    },
    ImageNotResolved {
        image: String,
        observed: String,
    },
    ImageIdentityDrift {
        expected: String,
        observed: String,
    },
    VendorSet(Box<crate::VendorSetError>),
    ImageVendorSetDrift {
        image: String,
        expected: String,
        observed: String,
    },
    InspectWorkspace {
        path: PathBuf,
        source: io::Error,
    },
    WorkspaceNotDirectory(PathBuf),
    UnrepresentableWorkspace(PathBuf),
    ZeroOutputLimit,
    EmptyProgram,
    NulInCommand,
    CreateAttestationChallenge(io::Error),
    Spec(SpecError),
    SpawnRuntime(io::Error),
    Wait(WaitError),
    MissingCapture(&'static str),
    RuntimeControlRefused {
        operation: &'static str,
        observed: String,
    },
    RuntimeNotAttested {
        expected: String,
        observed: String,
    },
}

impl fmt::Display for ContainerError {
    /// Two partitions: admitting the sealed appliance, then running inside it.
    ///
    /// The split exists only so neither half outgrows a reader. This match
    /// carries no catch-all, so a new variant is a compile error here rather
    /// than a closed partition it silently falls out of.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RuntimeNotAbsolute(..)
            | Self::InspectRuntime { .. }
            | Self::RuntimeNotRegular(..)
            | Self::RuntimeNotExecutable(..)
            | Self::RuntimeProbeSpec(..)
            | Self::SpawnRuntimeProbe(..)
            | Self::WaitRuntimeProbe(..)
            | Self::RuntimeDialectNotAttested { .. }
            | Self::ImageNotResolved { .. }
            | Self::ImageIdentityDrift { .. }
            | Self::ImageVendorSetDrift { .. } => self.fmt_appliance(formatter),
            Self::VendorSet(..)
            | Self::InspectWorkspace { .. }
            | Self::WorkspaceNotDirectory(..)
            | Self::UnrepresentableWorkspace(..)
            | Self::ZeroOutputLimit
            | Self::EmptyProgram
            | Self::NulInCommand
            | Self::CreateAttestationChallenge(..)
            | Self::Spec(..)
            | Self::SpawnRuntime(..)
            | Self::Wait(..)
            | Self::MissingCapture(..)
            | Self::RuntimeControlRefused { .. }
            | Self::RuntimeNotAttested { .. } => self.fmt_execution(formatter),
        }
    }
}

impl ContainerError {
    /// Why the sealed appliance was never admitted: the runtime binary, the
    /// interface it attests, and the vendor-set image it must already hold.
    fn fmt_appliance(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RuntimeNotAbsolute(path) => {
                write!(
                    formatter,
                    "proof runtime path must be absolute: {}",
                    path.display()
                )
            }
            Self::InspectRuntime { path, source } => {
                write!(
                    formatter,
                    "cannot inspect proof runtime {}: {source}",
                    path.display()
                )
            }
            Self::RuntimeNotRegular(path) => {
                write!(
                    formatter,
                    "proof runtime is not a regular file: {}",
                    path.display()
                )
            }
            Self::RuntimeNotExecutable(path) => {
                write!(
                    formatter,
                    "proof runtime is not executable: {}",
                    path.display()
                )
            }
            Self::RuntimeProbeSpec(source) => {
                write!(
                    formatter,
                    "invalid sealed OCI runtime identity probe: {source}"
                )
            }
            Self::SpawnRuntimeProbe(source) => {
                write!(
                    formatter,
                    "cannot start sealed OCI runtime identity probe: {source}"
                )
            }
            Self::WaitRuntimeProbe(source) => {
                write!(
                    formatter,
                    "sealed OCI runtime identity probe failed: {source}"
                )
            }
            Self::RuntimeDialectNotAttested { observed } => write!(
                formatter,
                "proof runtime did not attest a Docker or Podman interface: {observed}"
            ),
            Self::ImageNotResolved { image, observed } => write!(
                formatter,
                "proof runtime does not hold proof image {image}. Restore this lane with \
                 `spice dev xtask -- proof-image load \
                 target/proof-image/spice-proof-image.tar`, or rebuild that archive with \
                 `spice dev xtask -- proof-image build` when it is absent \
                 too. A global docker prune takes this image from every lane at once, so \
                 an image that was present a moment ago usually means a prune, not a \
                 missing setup step: {observed}"
            ),
            Self::ImageIdentityDrift { expected, observed } => write!(
                formatter,
                "proof runtime resolved a different appliance: expected {expected:?}, \
                 observed {observed:?}"
            ),
            Self::ImageVendorSetDrift {
                image,
                expected,
                observed,
            } => write!(
                formatter,
                "sealed Linux image {image} has a stale vendored package set: expected \
                 {expected}, observed {observed}. Reseal it with \
                 `spice dev xtask -- proof-image build`"
            ),
            _ => unreachable!("appliance admission partition is closed"),
        }
    }

    /// Why an admitted appliance did not run: the workspace it mounts, the
    /// process contract it is given, and the isolation it must prove.
    fn fmt_execution(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VendorSet(source) => {
                write!(formatter, "cannot derive the proof vendor set: {source}")
            }
            Self::InspectWorkspace { path, source } => write!(
                formatter,
                "cannot inspect proof workspace {}: {source}",
                path.display()
            ),
            Self::WorkspaceNotDirectory(path) => {
                write!(
                    formatter,
                    "proof workspace is not a directory: {}",
                    path.display()
                )
            }
            Self::UnrepresentableWorkspace(path) => write!(
                formatter,
                "proof workspace cannot be represented in an OCI mount: {}",
                path.display()
            ),
            Self::ZeroOutputLimit => {
                formatter.write_str("proof output capture limit must be nonzero")
            }
            Self::EmptyProgram => formatter.write_str("proof battery program must not be empty"),
            Self::NulInCommand => formatter.write_str("proof battery command must not contain NUL"),
            Self::CreateAttestationChallenge(source) => {
                write!(
                    formatter,
                    "cannot create sealed runtime challenge: {source}"
                )
            }
            Self::Spec(source) => write!(formatter, "invalid sealed process contract: {source}"),
            Self::SpawnRuntime(source) => {
                write!(formatter, "cannot start sealed runtime: {source}")
            }
            Self::Wait(source) => write!(formatter, "sealed runtime wait failed: {source}"),
            Self::MissingCapture(stream) => {
                write!(formatter, "sealed runtime returned no {stream} capture")
            }
            Self::RuntimeControlRefused {
                operation,
                observed,
            } => write!(
                formatter,
                "sealed runtime could not {operation}: {observed}"
            ),
            Self::RuntimeNotAttested { expected, observed } => write!(
                formatter,
                "sealed runtime did not prove its OCI isolation contract: expected {expected:?}, \
                 observed {observed:?}"
            ),
            _ => unreachable!("sealed execution partition is closed"),
        }
    }
}

impl Error for ContainerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InspectRuntime { source, .. }
            | Self::InspectWorkspace { source, .. }
            | Self::CreateAttestationChallenge(source)
            | Self::SpawnRuntimeProbe(source)
            | Self::SpawnRuntime(source) => Some(source),
            Self::VendorSet(source) => Some(source.as_ref()),
            Self::RuntimeProbeSpec(source) | Self::Spec(source) => Some(source),
            Self::WaitRuntimeProbe(source) | Self::Wait(source) => Some(source),
            Self::RuntimeNotAbsolute(_)
            | Self::RuntimeNotRegular(_)
            | Self::RuntimeNotExecutable(_)
            | Self::RuntimeDialectNotAttested { .. }
            | Self::ImageNotResolved { .. }
            | Self::ImageIdentityDrift { .. }
            | Self::ImageVendorSetDrift { .. }
            | Self::WorkspaceNotDirectory(_)
            | Self::UnrepresentableWorkspace(_)
            | Self::ZeroOutputLimit
            | Self::EmptyProgram
            | Self::NulInCommand
            | Self::MissingCapture(_)
            | Self::RuntimeControlRefused { .. }
            | Self::RuntimeNotAttested { .. } => None,
        }
    }
}

pub(crate) fn validate_command(program: &str, arguments: &[String]) -> Result<(), ContainerError> {
    if program.is_empty() {
        return Err(ContainerError::EmptyProgram);
    }
    if program.contains('\0') || arguments.iter().any(|argument| argument.contains('\0')) {
        return Err(ContainerError::NulInCommand);
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
pub(crate) fn is_executable(_metadata: &fs::Metadata) -> bool {
    false
}
