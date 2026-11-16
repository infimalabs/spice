//! Closed selected-target routing for the three sealed proof environments.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::path::Path;

use windows_execution::protocol::{Command as HostedCommand, Completion as HostedCompletion};
use wire::manifest::{ProofEnvironment, ReleaseTarget};

use crate::config::{Config, ExecutablePath};
use crate::container::{
    Container, ContainerBattery, ContainerError, Execution, ExecutionCompletion, Limits, OciRuntime,
};
use crate::native::{NativeError, NativeRunner};
use crate::payload::AdmittedPayload;

/// The sole admitted sealed environment for the selected release target.
#[derive(Clone, Debug)]
pub struct SealedEnvironment {
    target: ReleaseTarget,
    kind: EnvironmentKind,
}

#[derive(Clone, Debug)]
enum EnvironmentKind {
    Oci(Container),
    Native(NativeRunner),
    HostedWindows,
}

impl SealedEnvironment {
    /// Select and admit the environment fixed by `target`.
    ///
    /// Target selection happens before any configured path is inspected. The
    /// selected branch reads only its corresponding optional local path.
    ///
    /// # Errors
    ///
    /// Refuses a payload for another target, a missing selected path, or failed
    /// runtime admission.
    pub fn selected(
        target: ReleaseTarget,
        config: &Config,
        payload: &AdmittedPayload,
        workspace: impl AsRef<Path>,
        limits: Limits,
    ) -> Result<Self, EnvironmentError> {
        validate_payload_target(target, payload.value().target)?;
        let selected = Self::for_target(target, config, workspace.as_ref(), limits)?;
        if let EnvironmentKind::Oci(container) = &selected.kind {
            validate_payload_image(&payload.value().pins.proof_container, container.image())?;
        }
        Ok(selected)
    }

    fn for_target(
        target: ReleaseTarget,
        config: &Config,
        workspace: &Path,
        limits: Limits,
    ) -> Result<Self, EnvironmentError> {
        let kind = match target {
            ReleaseTarget::X86_64UnknownLinuxMusl => {
                let binary = configured_binary(target, config)?;
                let runtime = OciRuntime::attest(binary, limits).map_err(EnvironmentError::Oci)?;
                let container =
                    Container::new(runtime, workspace, limits).map_err(EnvironmentError::Oci)?;
                EnvironmentKind::Oci(container)
            }
            ReleaseTarget::Aarch64AppleDarwin => {
                let binary = configured_binary(target, config)?;
                let runner = NativeRunner::attest(binary, workspace, limits)
                    .map_err(EnvironmentError::Native)?;
                EnvironmentKind::Native(runner)
            }
            ReleaseTarget::X86_64PcWindowsMsvc => EnvironmentKind::HostedWindows,
        };
        Ok(Self { target, kind })
    }

    /// Release target selected by the proof command.
    #[must_use]
    pub const fn target(&self) -> ReleaseTarget {
        self.target
    }

    pub(crate) fn battery(
        &self,
        commands: &[HostedCommand],
    ) -> Result<BatteryEnvironment<'_>, BatteryStartError> {
        let (identity, kind) = match &self.kind {
            EnvironmentKind::Oci(container) => (
                ProofEnvironment::Oci {
                    image: container.proof_image().clone(),
                },
                container
                    .battery()
                    .map(BatteryEnvironmentKind::Oci)
                    .map_err(|source| BatteryStartError::Execution(source.to_string()))?,
            ),
            EnvironmentKind::Native(runner) => (
                ProofEnvironment::native(runner.identity().clone()),
                BatteryEnvironmentKind::Native(runner),
            ),
            EnvironmentKind::HostedWindows => {
                windows_execution::hosted::client::require_public_attestation_repository()
                    .map_err(BatteryStartError::from_hosted)?;
                let execution =
                    windows_execution::hosted::client::windows_hosted_commands(commands.to_vec())
                        .map_err(BatteryStartError::from_hosted)?;
                let identity = ProofEnvironment::hosted_windows(execution.result.identity.clone());
                let protocol_outputs = &execution.result.outputs;
                let outputs = execution
                    .outputs
                    .into_iter()
                    .zip(protocol_outputs)
                    .map(|(output, protocol)| Execution {
                        completion: match output.completion {
                            HostedCompletion::Exited { code } => ExecutionCompletion::Exited {
                                success: code == 0,
                                code: Some(code),
                            },
                            HostedCompletion::TimedOut => {
                                ExecutionCompletion::TimedOut { code: Some(124) }
                            }
                        },
                        stdout: crate::container::Captured {
                            bytes: output.stdout,
                            truncated: protocol.stdout.truncated,
                            complete: true,
                        },
                        stderr: crate::container::Captured {
                            bytes: output.stderr,
                            truncated: protocol.stderr.truncated,
                            complete: true,
                        },
                    })
                    .collect();
                (
                    identity,
                    BatteryEnvironmentKind::HostedWindows(RefCell::new(outputs)),
                )
            }
        };
        Ok(BatteryEnvironment {
            target: self.target,
            identity,
            kind,
        })
    }

    #[cfg(test)]
    pub(crate) fn fixture_oci(container: Container) -> Self {
        Self {
            target: ReleaseTarget::X86_64UnknownLinuxMusl,
            kind: EnvironmentKind::Oci(container),
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture_native(runner: NativeRunner) -> Self {
        Self {
            target: ReleaseTarget::Aarch64AppleDarwin,
            kind: EnvironmentKind::Native(runner),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BatteryStartError {
    HostedAttestationPrerequisite {
        repository: String,
        visibility: String,
    },
    Execution(String),
}

impl BatteryStartError {
    fn from_hosted(error: windows_execution::hosted::client::WindowsHostedError) -> Self {
        match error {
            windows_execution::hosted::client::WindowsHostedError::PublicAttestationRequired {
                repository,
                visibility,
            } => Self::HostedAttestationPrerequisite {
                repository,
                visibility,
            },
            error => Self::Execution(error.to_string()),
        }
    }
}

fn configured_binary(
    target: ReleaseTarget,
    config: &Config,
) -> Result<&ExecutablePath, EnvironmentError> {
    match target {
        ReleaseTarget::X86_64UnknownLinuxMusl => config
            .container()
            .ok_or(EnvironmentError::MissingContainerPath),
        ReleaseTarget::Aarch64AppleDarwin => {
            config.runner().ok_or(EnvironmentError::MissingRunnerPath)
        }
        ReleaseTarget::X86_64PcWindowsMsvc => {
            unreachable!("hosted Windows does not use a machine-local executable path")
        }
    }
}

/// The exact execution scope shared by one complete proof battery.
pub(crate) struct BatteryEnvironment<'a> {
    target: ReleaseTarget,
    identity: ProofEnvironment,
    kind: BatteryEnvironmentKind<'a>,
}

enum BatteryEnvironmentKind<'a> {
    Oci(ContainerBattery<'a>),
    Native(&'a NativeRunner),
    HostedWindows(RefCell<VecDeque<Execution>>),
}

impl BatteryEnvironment<'_> {
    pub(crate) const fn target(&self) -> ReleaseTarget {
        self.target
    }

    pub(crate) fn identity(&self) -> ProofEnvironment {
        self.identity.clone()
    }

    pub(crate) fn execute(&self, program: &str, arguments: &[String]) -> Result<Execution, String> {
        match &self.kind {
            BatteryEnvironmentKind::Oci(battery) => battery
                .execute(program, arguments)
                .map_err(|source| source.to_string()),
            BatteryEnvironmentKind::Native(runner) => runner
                .execute(program, arguments)
                .map_err(|source| source.to_string()),
            BatteryEnvironmentKind::HostedWindows(outputs) => outputs
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| "hosted result has no output for this proof member".to_owned()),
        }
    }

    pub(crate) fn close(self) -> Result<(), String> {
        match self.kind {
            BatteryEnvironmentKind::Oci(battery) => {
                battery.close().map_err(|source| source.to_string())
            }
            BatteryEnvironmentKind::Native(_) => Ok(()),
            BatteryEnvironmentKind::HostedWindows(outputs) => {
                if outputs.into_inner().is_empty() {
                    Ok(())
                } else {
                    Err("hosted result carries unconsumed proof members".to_owned())
                }
            }
        }
    }
}

/// Select the sole release target admitted for the compiled host tuple.
///
/// # Errors
///
/// Refuses every host except exact `aarch64-apple-darwin` and
/// `x86_64-unknown-linux-musl`.
pub fn compiled_target() -> Result<ReleaseTarget, UnsupportedHost> {
    select_host(Host::compiled())
}

fn validate_payload_target(
    selected: ReleaseTarget,
    payload: ReleaseTarget,
) -> Result<(), EnvironmentError> {
    if payload == selected {
        Ok(())
    } else {
        Err(EnvironmentError::PayloadTarget { selected, payload })
    }
}

fn validate_payload_image(payload: &str, resolved: &str) -> Result<(), EnvironmentError> {
    if payload == resolved {
        Ok(())
    } else {
        Err(EnvironmentError::ImageMismatch {
            payload: payload.to_owned(),
            resolved: resolved.to_owned(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Host {
    arch: &'static str,
    vendor: &'static str,
    os: &'static str,
    environment: &'static str,
    abi: &'static str,
}

impl Host {
    const fn compiled() -> Self {
        Self {
            arch: compiled_arch(),
            vendor: compiled_vendor(),
            os: compiled_os(),
            environment: compiled_environment(),
            abi: compiled_abi(),
        }
    }
}

fn select_host(host: Host) -> Result<ReleaseTarget, UnsupportedHost> {
    match host {
        Host {
            arch: "aarch64",
            vendor: "apple",
            os: "macos",
            environment: "",
            abi: "",
        } => Ok(ReleaseTarget::Aarch64AppleDarwin),
        Host {
            arch: "x86_64",
            vendor: "unknown",
            os: "linux",
            environment: "musl",
            abi: "",
        } => Ok(ReleaseTarget::X86_64UnknownLinuxMusl),
        _ => Err(UnsupportedHost {
            arch: host.arch,
            vendor: host.vendor,
            os: host.os,
            environment: host.environment,
            abi: host.abi,
        }),
    }
}

const fn compiled_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        "other"
    }
}

const fn compiled_vendor() -> &'static str {
    if cfg!(target_vendor = "apple") {
        "apple"
    } else if cfg!(target_vendor = "unknown") {
        "unknown"
    } else {
        "other"
    }
}

const fn compiled_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "other"
    }
}

const fn compiled_environment() -> &'static str {
    if cfg!(target_env = "musl") {
        "musl"
    } else if cfg!(target_env = "") {
        ""
    } else {
        "other"
    }
}

const fn compiled_abi() -> &'static str {
    if cfg!(target_abi = "") { "" } else { "other" }
}

/// The exact compiled host tuple that has no release proof environment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnsupportedHost {
    arch: &'static str,
    vendor: &'static str,
    os: &'static str,
    environment: &'static str,
    abi: &'static str,
}

impl fmt::Display for UnsupportedHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            concat!(
                "compiled host has no sealed proof environment: ",
                "arch={:?}, vendor={:?}, os={:?}, env={:?}, abi={:?}"
            ),
            self.arch, self.vendor, self.os, self.environment, self.abi
        )
    }
}

impl Error for UnsupportedHost {}

/// Why the selected proof environment could not be established.
#[derive(Debug)]
pub enum EnvironmentError {
    PayloadTarget {
        selected: ReleaseTarget,
        payload: ReleaseTarget,
    },
    ImageMismatch {
        payload: String,
        resolved: String,
    },
    MissingContainerPath,
    MissingRunnerPath,
    Oci(ContainerError),
    Native(NativeError),
}

impl fmt::Display for EnvironmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PayloadTarget { selected, payload } => write!(
                formatter,
                "proof command selects {selected}, but admitted payload targets {payload}"
            ),
            Self::ImageMismatch { payload, resolved } => write!(
                formatter,
                concat!(
                    "resolved proof image does not match admitted musl payload: ",
                    "payload={:?}, resolved={:?}"
                ),
                payload, resolved
            ),
            Self::MissingContainerPath => formatter
                .write_str("proof.container is required for x86_64-unknown-linux-musl proof"),
            Self::MissingRunnerPath => {
                formatter.write_str("proof.runner is required for aarch64-apple-darwin proof")
            }
            Self::Oci(source) => source.fmt(formatter),
            Self::Native(source) => source.fmt(formatter),
        }
    }
}

impl Error for EnvironmentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Oci(source) => Some(source),
            Self::Native(source) => Some(source),
            Self::PayloadTarget { .. }
            | Self::ImageMismatch { .. }
            | Self::MissingContainerPath
            | Self::MissingRunnerPath => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use wire::manifest::ReleaseTarget;

    use super::{
        EnvironmentError, Host, UnsupportedHost, configured_binary, select_host,
        validate_payload_image, validate_payload_target,
    };
    use crate::config::Config;

    const DARWIN: Host = Host {
        arch: "aarch64",
        vendor: "apple",
        os: "macos",
        environment: "",
        abi: "",
    };
    const MUSL: Host = Host {
        arch: "x86_64",
        vendor: "unknown",
        os: "linux",
        environment: "musl",
        abi: "",
    };

    #[test]
    fn only_the_two_exact_compiled_host_tuples_select_targets() {
        assert_eq!(select_host(DARWIN), Ok(ReleaseTarget::Aarch64AppleDarwin));
        assert_eq!(select_host(MUSL), Ok(ReleaseTarget::X86_64UnknownLinuxMusl));
    }

    #[test]
    fn every_host_component_is_a_closed_selection_boundary() {
        for unsupported in [
            Host {
                arch: "x86_64",
                ..DARWIN
            },
            Host {
                vendor: "unknown",
                ..DARWIN
            },
            Host {
                os: "linux",
                ..DARWIN
            },
            Host {
                environment: "gnu",
                ..DARWIN
            },
            Host {
                abi: "sim",
                ..DARWIN
            },
            Host {
                arch: "aarch64",
                ..MUSL
            },
            Host {
                vendor: "pc",
                ..MUSL
            },
            Host {
                os: "macos",
                ..MUSL
            },
            Host {
                environment: "gnu",
                ..MUSL
            },
            Host {
                abi: "eabi",
                ..MUSL
            },
        ] {
            assert_eq!(
                select_host(unsupported),
                Err(UnsupportedHost {
                    arch: unsupported.arch,
                    vendor: unsupported.vendor,
                    os: unsupported.os,
                    environment: unsupported.environment,
                    abi: unsupported.abi,
                })
            );
        }
    }

    #[test]
    fn each_target_requires_only_its_selected_configured_path() {
        let runner_only = Config::fixture(None, Some("unselected-runner".into()));
        assert!(matches!(
            configured_binary(ReleaseTarget::X86_64UnknownLinuxMusl, &runner_only),
            Err(EnvironmentError::MissingContainerPath)
        ));

        let container_only = Config::fixture(Some("unselected-container".into()), None);
        assert!(matches!(
            configured_binary(ReleaseTarget::Aarch64AppleDarwin, &container_only),
            Err(EnvironmentError::MissingRunnerPath)
        ));
    }

    #[test]
    fn payload_target_and_resolved_musl_image_are_exact() {
        assert!(matches!(
            validate_payload_target(
                ReleaseTarget::Aarch64AppleDarwin,
                ReleaseTarget::X86_64UnknownLinuxMusl,
            ),
            Err(EnvironmentError::PayloadTarget { .. })
        ));
        assert!(matches!(
            validate_payload_image(
                "sha256:4444444444444444444444444444444444444444444444444444444444444444",
                "sha256:5555555555555555555555555555555555555555555555555555555555555555",
            ),
            Err(EnvironmentError::ImageMismatch { .. })
        ));
        assert!(
            validate_payload_image(
                "sha256:4444444444444444444444444444444444444444444444444444444444444444",
                "sha256:4444444444444444444444444444444444444444444444444444444444444444",
            )
            .is_ok()
        );
    }
}
