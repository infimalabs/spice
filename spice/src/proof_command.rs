//! The binary rim for the two daemonless proof leaves.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use clap::ArgMatches;

use proof::{
    CheckStanding, ContainerError, EnvironmentError, EvidenceError, EvidencePublishError,
    InputError, NativeError, PayloadError, ProofError, RederiveError, ReplayError, ReplayFailure,
    ReplayRefusal, SealError,
};
use sill::Deadlines;
use wire::{Fault, FaultClass, Outcome, ProofEvidence, Refusal, Reply, ReplyPosition};

use spice::config::{LoadError, Resolved};
use spice::repository::Repository;

const MEMBER_RUNTIME: Duration = Duration::from_secs(60 * 60);
const TERMINATE_HORIZON: Duration = Duration::from_secs(10);
const KILL_HORIZON: Duration = Duration::from_secs(10);
const TERMINATION_GRACE: Duration = Duration::from_secs(5);
const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

/// Seal and atomically publish the detached proof for the selected target.
pub(crate) fn seal(
    matches: &ArgMatches,
    configuration: Option<&spice::config::RepositoryCapture>,
) -> Outcome<ProofEvidence> {
    let invocation = match Invocation::acquire(matches, configuration) {
        Ok(invocation) => invocation,
        Err(failure) => return failure.outcome(),
    };
    match proof::seal(
        &invocation.layout,
        invocation.config.root().proof(),
        &invocation.workspace,
        limits(),
    ) {
        Ok(evidence) => direct(evidence),
        Err(error) => Failure::seal(error).outcome(),
    }
}

/// Re-execute and verify the existing detached proof for the selected target.
pub(crate) fn replay(
    matches: &ArgMatches,
    configuration: Option<&spice::config::RepositoryCapture>,
) -> Outcome<ProofEvidence> {
    let invocation = match Invocation::acquire(matches, configuration) {
        Ok(invocation) => invocation,
        Err(failure) => return failure.outcome(),
    };
    match proof::rederive(
        &invocation.layout,
        invocation.config.root().proof(),
        &invocation.workspace,
        limits(),
    ) {
        Ok(evidence) => direct(evidence),
        Err(error) => Failure::rederive(error).outcome(),
    }
}

fn direct(body: ProofEvidence) -> Outcome<ProofEvidence> {
    Outcome::Reply(Reply {
        position: ReplyPosition::Direct {},
        body,
    })
}

struct Invocation {
    layout: proof::Layout,
    workspace: PathBuf,
    config: Resolved,
}

impl Invocation {
    fn acquire(
        matches: &ArgMatches,
        configuration: Option<&spice::config::RepositoryCapture>,
    ) -> Result<Self, Failure> {
        let current = std::env::current_dir().map_err(|error| {
            Failure::fault(
                FaultClass::Environment,
                "proof::working-directory-unavailable",
                error.to_string(),
            )
        })?;
        let repository = Repository::discover(&current).map_err(|error| {
            Failure::fault(
                FaultClass::Environment,
                spice::repository::ResolveError::REASON,
                error.to_string(),
            )
        })?;
        let loaded = spice::config::capture_repository_scoped(&repository, &[], configuration)
            .map_err(Failure::configuration)?;
        let config = loaded.resolved().clone();
        let state =
            repository
                .admit_state()
                .map_err(|error| match crate::state_failure(&error) {
                    crate::StateFailure::Refusal(refusal) => Failure::Refusal(refusal),
                    crate::StateFailure::Fault(fault) => Failure::Fault(fault),
                })?;
        let target = selected_target(matches)?;
        let layout = proof::Layout::new(repository.worktree_root(), state.proof(), target);
        Ok(Self {
            layout,
            workspace: repository.worktree_root().to_path_buf(),
            config,
        })
    }
}

fn selected_target(matches: &ArgMatches) -> Result<wire::manifest::ReleaseTarget, Failure> {
    match matches.get_one::<String>("target").map(String::as_str) {
        Some(target) => target_from_spelling(target).ok_or_else(|| {
            Failure::refusal(
                "proof::target-refused",
                "one of the three release targets",
                target,
                "pass a canonical release target triple",
            )
        }),
        None => proof::compiled_target().map_err(|error| {
            Failure::refusal(
                "proof::target-refused",
                "one of the two compiled proof release targets",
                error.to_string(),
                "pass --target x86_64-pc-windows-msvc for the hosted Windows proof",
            )
        }),
    }
}

fn target_from_spelling(spelling: &str) -> Option<wire::manifest::ReleaseTarget> {
    use wire::manifest::ReleaseTarget;

    match spelling {
        "aarch64-apple-darwin" => Some(ReleaseTarget::Aarch64AppleDarwin),
        "x86_64-unknown-linux-musl" => Some(ReleaseTarget::X86_64UnknownLinuxMusl),
        "x86_64-pc-windows-msvc" => Some(ReleaseTarget::X86_64PcWindowsMsvc),
        _ => None,
    }
}

fn limits() -> proof::Limits {
    let deadlines = Deadlines::new(MEMBER_RUNTIME, TERMINATE_HORIZON, KILL_HORIZON)
        .expect("compiled proof deadlines are positive and ordered");
    proof::Limits::new(deadlines, TERMINATION_GRACE, OUTPUT_LIMIT)
        .expect("compiled proof output bound is nonzero")
}

enum Failure {
    Refusal(Refusal),
    Fault(Fault),
}

impl Failure {
    fn refusal(
        reason: &'static str,
        expected: impl Into<String>,
        observed: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                reason.to_owned(),
                None,
                Some(expected.into()),
                Some(observed.into()),
            )
            .with_detail(detail),
        ))
    }

    fn fault(class: FaultClass, reason: &'static str, detail: impl Into<String>) -> Self {
        Self::Fault(Fault {
            class,
            reason: reason.to_owned(),
            detail: detail.into(),
        })
    }

    fn outcome<B>(self) -> Outcome<B> {
        match self {
            Self::Refusal(refusal) => Outcome::Refusal(refusal),
            Self::Fault(fault) => Outcome::Fault(fault),
        }
    }

    fn configuration(error: LoadError) -> Self {
        if let Some(repository) = error.repository() {
            return Self::fault(
                FaultClass::Environment,
                spice::repository::ResolveError::REASON,
                repository.to_string(),
            );
        }
        let diagnostics = error.diagnostic();
        if error.unreadable() {
            return Self::fault(
                FaultClass::Environment,
                "proof::configuration-unreadable",
                diagnostics,
            );
        }
        Self::refusal(
            "proof::configuration-refused",
            "valid tracked proof policy and machine-local runtime paths",
            diagnostics,
            "repair the named configuration values before running proof again",
        )
    }

    /// Map the shared replay core's refusal; `layout_remedy` names how the
    /// caller's own operation restores its layout.
    fn proof(error: ProofError, layout_remedy: &'static str) -> Self {
        match error {
            ProofError::ReadPayload(error) => Self::input(error, InputKind::Payload),
            ProofError::AdmitPayload(error) => Self::payload(error),
            error @ (ProofError::Layout(_) | ProofError::EnvironmentTarget { .. }) => {
                Self::refusal(
                    "proof::layout-refused",
                    "one payload, layout, and compiled release target",
                    error.to_string(),
                    layout_remedy,
                )
            }
            ProofError::Environment(error) => Self::environment(error),
            ProofError::Replay(error) => Self::replay(error),
            ProofError::Refused(refusal) => Self::battery(refusal),
            ProofError::Evidence(error) => Self::evidence(error),
        }
    }

    fn seal(error: SealError) -> Self {
        match error {
            SealError::Proof(error) => Self::proof(
                error,
                "stage the payload under its exact compiled-target layout and retry",
            ),
            SealError::Receipt(error) => Self::refusal(
                "proof::verification-refused",
                "one replay and payload for the same release target",
                error.to_string(),
                "restore the target-matched payload and rerun the complete battery",
            ),
            SealError::EncodeReceipt(error) => {
                Self::fault(FaultClass::Protocol, "proof::encoding-failed", error)
            }
            SealError::PrepareDestination { path, source } => Self::fault(
                FaultClass::Store,
                "proof::publication-failed",
                format!("cannot prepare {}: {source}", path.display()),
            ),
            SealError::Publish(error) => Self::publication(error),
            error @ SealError::PublishedReceipt { .. } => Self::fault(
                FaultClass::Protocol,
                "proof::publication-invalid",
                error.to_string(),
            ),
        }
    }

    fn rederive(error: RederiveError) -> Self {
        match error {
            RederiveError::Proof(error) => Self::proof(
                error,
                "restore the target-matched staged payload and detached receipt",
            ),
            RederiveError::ReadReceipt(error) => Self::input(error, InputKind::Receipt),
            RederiveError::AdmitReceipt(error) => Self::receipt(error),
        }
    }

    fn replay(error: ReplayError) -> Self {
        match error.failure() {
            ReplayFailure::HostedAttestationPrerequisite => Self::refusal(
                "proof::hosted-attestation-prerequisite",
                "a public infimalabs/spice repository eligible for public Sigstore attestation",
                error.to_string(),
                "publish the repository before executing the Windows release proof; private hosted runs, cross-builds, Wine, and local emulation are not release proof",
            ),
            ReplayFailure::Execution => Self::fault(
                FaultClass::Environment,
                "proof::process-failed",
                error.to_string(),
            ),
        }
    }

    fn input(error: InputError, kind: InputKind) -> Self {
        if matches!(&error, InputError::Unreadable { .. }) {
            return Self::fault(
                FaultClass::Environment,
                kind.unreadable_reason(),
                error.to_string(),
            );
        }
        Self::refusal(
            kind.refused_reason(),
            kind.expected(),
            error.to_string(),
            kind.remedy(),
        )
    }

    fn payload(error: PayloadError) -> Self {
        if matches!(&error, PayloadError::Encode(_)) {
            return Self::fault(
                FaultClass::Protocol,
                "proof::encoding-failed",
                error.to_string(),
            );
        }
        Self::refusal(
            "proof::payload-refused",
            "one canonical current target payload with complete bound identities",
            error.to_string(),
            "regenerate the exact target payload from the current distribution pipeline",
        )
    }

    fn receipt(error: EvidenceError) -> Self {
        if matches!(&error, EvidenceError::Encode(_)) {
            return Self::fault(
                FaultClass::Protocol,
                "proof::encoding-failed",
                error.to_string(),
            );
        }
        Self::refusal(
            "proof::receipt-refused",
            "one exact canonical current detached proof receipt",
            error.to_string(),
            "restore or reseal the receipt selected by the admitted payload",
        )
    }

    fn evidence(error: EvidenceError) -> Self {
        if matches!(&error, EvidenceError::Encode(_)) {
            return Self::fault(
                FaultClass::Protocol,
                "proof::encoding-failed",
                error.to_string(),
            );
        }
        Self::refusal(
            "proof::verification-refused",
            "one payload, environment, battery, replay, and receipt identity chain",
            error.to_string(),
            "restore matching proof inputs and rerun the current battery",
        )
    }

    fn environment(error: EnvironmentError) -> Self {
        match &error {
            EnvironmentError::UnsupportedHost(_) | EnvironmentError::PayloadTarget { .. } => {
                return Self::refusal(
                    "proof::target-refused",
                    "the payload target selected by this compiled host",
                    error.to_string(),
                    "stage the payload built for this executable's exact release target",
                );
            }
            EnvironmentError::ImageMismatch { .. } => {
                return Self::refusal(
                    "proof::runner-refused",
                    "the exact proof image bound by the payload",
                    error.to_string(),
                    "restore the proof image archive used to stage this payload",
                );
            }
            EnvironmentError::MissingContainerPath | EnvironmentError::MissingRunnerPath => {}
            EnvironmentError::Oci(error) => {
                return Self::runtime(error.to_string(), container_disposition(error));
            }
            EnvironmentError::Native(error) => {
                return Self::runtime(error.to_string(), native_disposition(error));
            }
        }
        let root = wire::STATE_DIRECTORY;
        let remediation = if matches!(error, EnvironmentError::MissingRunnerPath) {
            format!(
                "bind proof.runner to the runner this worktree sealed: spice config proof.runner PATH plans a write to this worktree's Git-directory {root}/worktree.toml, and rerunning it with the plan's --apply=DIGEST applies it"
            )
        } else {
            format!("set proof.container in Git-common {root}/config.toml")
        };
        Self::refusal(
            "proof::runner-refused",
            "one configured executable for the compiled proof target",
            error.to_string(),
            remediation,
        )
    }

    fn runtime(observed: String, disposition: RuntimeDisposition) -> Self {
        match disposition {
            RuntimeDisposition::Refusal => Self::refusal(
                "proof::runner-refused",
                "one exact executable satisfying the sealed proof runtime contract",
                observed,
                "repair the configured runtime path or attestation contract and retry",
            ),
            RuntimeDisposition::Io => Self::fault(
                FaultClass::Environment,
                "proof::runner-unavailable",
                observed,
            ),
            RuntimeDisposition::Process => {
                Self::fault(FaultClass::Environment, "proof::process-failed", observed)
            }
            RuntimeDisposition::Protocol => {
                Self::fault(FaultClass::Protocol, "proof::execution-invalid", observed)
            }
        }
    }

    fn battery(refusal: ReplayRefusal) -> Self {
        let replay = refusal.replay();
        let failing = replay
            .checks()
            .iter()
            .filter(|check| !check.standing().passes())
            .collect::<Vec<_>>();
        assert!(
            !failing.is_empty(),
            "a refused battery is refused by a member, so at least one must have failed",
        );
        let mut detail = format!(
            "repair {} and rerun the proof leaf",
            failing
                .iter()
                .map(|check| check.kind().as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        for check in &failing {
            if let Some(diagnosis) = check.diagnosis() {
                detail.push_str(&format!(
                    "\n\n{} {}:\n{}",
                    check.kind().as_str(),
                    standing(check.standing()),
                    diagnosis.trim_end()
                ));
            }
        }
        Self::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "proof::battery-refused",
                Some(replay.replay_digest().to_string()),
                Some("every generated proof battery member to complete cleanly".to_owned()),
                Some(
                    failing
                        .iter()
                        .map(|check| {
                            format!("{}={}", check.kind().as_str(), standing(check.standing()))
                        })
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
            )
            .with_detail(detail),
        ))
    }

    fn publication(error: EvidencePublishError) -> Self {
        match error {
            EvidencePublishError::Encode { source } => {
                Self::fault(FaultClass::Protocol, "proof::encoding-failed", source)
            }
            EvidencePublishError::Publish(atomic::Error::Refused(refusal)) => Self::refusal(
                "proof::publication-refused",
                "one regular detached receipt destination in its resolved parent",
                refusal.to_string(),
                "repair the proof destination shape and rerun the seal operation",
            ),
            EvidencePublishError::Publish(error @ atomic::Error::Io { .. }) => Self::fault(
                FaultClass::Store,
                "proof::publication-failed",
                error.to_string(),
            ),
        }
    }
}

#[derive(Clone, Copy)]
enum InputKind {
    Payload,
    Receipt,
}

impl InputKind {
    const fn refused_reason(self) -> &'static str {
        match self {
            Self::Payload => "proof::payload-refused",
            Self::Receipt => "proof::receipt-refused",
        }
    }

    const fn unreadable_reason(self) -> &'static str {
        match self {
            Self::Payload => "proof::payload-unreadable",
            Self::Receipt => "proof::receipt-unreadable",
        }
    }

    const fn expected(self) -> &'static str {
        match self {
            Self::Payload => "one regular target payload at the exact staged path",
            Self::Receipt => "one regular detached receipt selected by the admitted payload",
        }
    }

    const fn remedy(self) -> &'static str {
        match self {
            Self::Payload => "stage the exact target payload as a regular file and retry",
            Self::Receipt => "seal or restore the exact detached receipt as a regular file",
        }
    }
}

#[derive(Clone, Copy)]
enum RuntimeDisposition {
    Refusal,
    Io,
    Process,
    Protocol,
}

fn container_disposition(error: &ContainerError) -> RuntimeDisposition {
    match error {
        ContainerError::InspectRuntime { source, .. }
            if source.kind() != io::ErrorKind::NotFound =>
        {
            RuntimeDisposition::Io
        }
        ContainerError::InspectWorkspace { .. } => RuntimeDisposition::Io,
        ContainerError::SpawnRuntimeProbe(_)
        | ContainerError::WaitRuntimeProbe(_)
        | ContainerError::CreateAttestationChallenge(_)
        | ContainerError::SpawnRuntime(_)
        | ContainerError::Wait(_)
        | ContainerError::MissingCapture(_) => RuntimeDisposition::Process,
        ContainerError::RuntimeProbeSpec(_)
        | ContainerError::ZeroOutputLimit
        | ContainerError::EmptyProgram
        | ContainerError::NulInCommand
        | ContainerError::Spec(_) => RuntimeDisposition::Protocol,
        ContainerError::RuntimeNotAbsolute(_)
        | ContainerError::InspectRuntime { .. }
        | ContainerError::RuntimeNotRegular(_)
        | ContainerError::RuntimeNotExecutable(_)
        | ContainerError::RuntimeDialectNotAttested { .. }
        | ContainerError::ImageNotResolved { .. }
        | ContainerError::ImageIdentityDrift { .. }
        | ContainerError::VendorSet(_)
        | ContainerError::ImageVendorSetDrift { .. }
        | ContainerError::WorkspaceNotDirectory(_)
        | ContainerError::UnrepresentableWorkspace(_)
        | ContainerError::RuntimeControlRefused { .. }
        | ContainerError::RuntimeNotAttested { .. } => RuntimeDisposition::Refusal,
    }
}

fn native_disposition(error: &NativeError) -> RuntimeDisposition {
    match error {
        NativeError::InspectRunner { source, .. } if source.kind() != io::ErrorKind::NotFound => {
            RuntimeDisposition::Io
        }
        NativeError::ReadRunner { .. } | NativeError::InspectWorkspace { .. } => {
            RuntimeDisposition::Io
        }
        NativeError::CreateChallenge(_)
        | NativeError::Spawn(_)
        | NativeError::Wait(_)
        | NativeError::MissingCapture(_)
        | NativeError::PreflightOutputIncomplete
        | NativeError::PreflightFailed { .. }
        | NativeError::PreflightTimedOut { .. }
        | NativeError::PreflightCancelled { .. } => RuntimeDisposition::Process,
        NativeError::Command(error) => container_disposition(error),
        NativeError::Spec(_) => RuntimeDisposition::Protocol,
        NativeError::RunnerNotAbsolute(_)
        | NativeError::InspectRunner { .. }
        | NativeError::RunnerNotRegular(_)
        | NativeError::RunnerNotExecutable(_)
        | NativeError::RunnerChanged { .. }
        | NativeError::WorkspaceNotDirectory(_)
        | NativeError::IncompleteAttestation { .. }
        | NativeError::RunnerNotAttested { .. } => RuntimeDisposition::Refusal,
    }
}

/// Render one member standing in the spelling its own identity already uses.
///
/// Spelling each standing again here let the two drift apart, and a standing
/// added to the battery could reach an operator under a name the receipt never
/// carried. The standing renders itself, so there is one spelling to change.
fn standing(standing: CheckStanding) -> String {
    format!("{}{}", standing.as_str(), code_suffix(standing.code()))
}

fn code_suffix(code: Option<i32>) -> String {
    code.map_or_else(String::new, |code| format!("(code={code})"))
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;

    use proof::{EvidencePublishError, Input, InputError, PayloadError, ProofError, SealError};
    use wire::manifest::ReleaseTarget;
    use wire::{FaultClass, Outcome};

    use super::{Failure, target_from_spelling};

    #[test]
    fn explicit_target_selects_the_hosted_windows_proof() {
        assert_eq!(
            target_from_spelling("x86_64-pc-windows-msvc"),
            Some(ReleaseTarget::X86_64PcWindowsMsvc)
        );
        assert_eq!(target_from_spelling("x86_64-pc-windows"), None);
    }

    #[test]
    fn input_shape_refuses_while_io_and_encoding_defects_fault() {
        let missing = Failure::proof(
            ProofError::ReadPayload(InputError::Missing {
                input: Input::Payload,
                path: PathBuf::from("payload.json"),
            }),
            "restage",
        )
        .outcome::<()>();
        let Outcome::Refusal(missing) = missing else {
            panic!("missing payload must refuse")
        };
        assert_eq!(missing.reason(), "proof::payload-refused");

        let unreadable = Failure::proof(
            ProofError::ReadPayload(InputError::Unreadable {
                input: Input::Payload,
                path: PathBuf::from("payload.json"),
                source: io::Error::new(io::ErrorKind::PermissionDenied, "fixture"),
            }),
            "restage",
        )
        .outcome::<()>();
        let Outcome::Fault(unreadable) = unreadable else {
            panic!("payload I/O must fault")
        };
        assert_eq!(unreadable.reason, "proof::payload-unreadable");
        assert_eq!(unreadable.class, FaultClass::Environment);

        let encoding = Failure::proof(
            ProofError::AdmitPayload(PayloadError::Encode("fixture encoder".to_owned())),
            "restage",
        )
        .outcome::<()>();
        let Outcome::Fault(encoding) = encoding else {
            panic!("encoding defect must fault")
        };
        assert_eq!(encoding.reason, "proof::encoding-failed");
        assert_eq!(encoding.class, FaultClass::Protocol);
    }

    #[test]
    fn publication_refusal_and_io_failure_remain_distinct() {
        let refused = Failure::seal(SealError::Publish(EvidencePublishError::Publish(
            atomic::Error::Refused(atomic::Refusal::DestinationHasNoFileName {
                destination: PathBuf::from("/"),
            }),
        )))
        .outcome::<()>();
        let Outcome::Refusal(refused) = refused else {
            panic!("publication shape must refuse")
        };
        assert_eq!(refused.reason(), "proof::publication-refused");

        let failed = Failure::seal(SealError::Publish(EvidencePublishError::Publish(
            atomic::Error::Io {
                operation: atomic::Operation::Replace,
                path: PathBuf::from("receipt.json"),
                source: io::Error::other("fixture"),
                cleanup: None,
            },
        )))
        .outcome::<()>();
        let Outcome::Fault(failed) = failed else {
            panic!("publication I/O must fault")
        };
        assert_eq!(failed.reason, "proof::publication-failed");
        assert_eq!(failed.class, FaultClass::Store);
    }
}
