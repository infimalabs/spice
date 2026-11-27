//! The sealed OCI process contract used by proof replay.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sill::{
    Argv, Cancellation, Deadlines, Environment, Output, ProcessGroup, Redaction,
    RestartDisposition, Spec, SpecError, StdinPolicy, StreamPolicy, WaitError, WaitPolicy,
};
use wire::manifest::ProofImage;

use crate::VendorSet;
use crate::challenge::Challenge;
use crate::launch::{Launch, LaunchError, System};

mod error;
mod stage;

pub use error::ContainerError;
pub(crate) use error::{is_executable, validate_command};
pub use stage::{ProviderCanvas, ReleaseStage, WindowsCross};

const WORKSPACE_MOUNT: &str = "/workspace";
const CARGO_HOME: &str = "/cargo-home";
const CARGO_TARGET: &str = "/cargo-target";
/// Where Rosetta writes a thread-context file it cannot keep in memory.
const TEMPORARY: &str = "/tmp";
const RUST_TEST_THREADS: &str = "1";
const ATTESTATION_PREFIX: &str = "spice-proof-attestation-v1:";
const BATTERY_KEEPALIVE_SCRIPT: &str = "while :; do sleep 3600; done";

/// The one attestation exchange used by boundary preflight and every member.
macro_rules! attest_and_exec {
    () => {
        r#"
dialect=$1
challenge=$2
shift 2
printf 'spice-proof-attestation-v1:%s:%s\n' "$dialect" "$challenge" >&2
exec "$@"
"#
    };
}

/// The rule deciding whether a network namespace can carry traffic off
/// loopback, as one shell function over a `/sys/class/net` directory.
///
/// A fresh namespace is not an empty one. The kernel materializes DOWN tunnel
/// stubs — `gre0`, `sit0`, `tunl0`, `ip6tnl0` and their relatives — as soon as
/// those modules are loaded anywhere on the host, and `/sys/class/net` also
/// holds `bonding_masters`, which is a control file and not an interface at
/// all, so deciding on the name `lo` would turn away exactly the isolated
/// `--network=none` container this boundary exists to admit.
///
/// So the rule asks what an interface can do rather than what it is called.
/// Only an administratively up interface can carry a packet, which admits
/// every stub while it stays down, and `IFF_LOOPBACK` is set by the kernel for
/// the loopback device alone, so a real adapter renamed `lo` is refused.
macro_rules! network_rule {
    () => {
        r#"
network_isolated() {
    for interface in "$1"/*; do
        [ -d "$interface" ] || continue
        read -r flags < "$interface/flags" || return 1
        [ $(( flags & 0x1 )) -ne 0 ] || continue
        [ $(( flags & 0x8 )) -ne 0 ] || return 1
    done
    return 0
}
"#
    };
}

/// The rule deciding whether the caller's mounts are the ones this boundary
/// grants, as one shell function over a `mountinfo` file.
///
/// It names the first bound not met rather than returning a bare failure, so
/// the refusal a member sees still says which mount was wrong.
macro_rules! mount_rule {
    () => {
        r#"
mount_violation() {
    workspace_read_only=false
    cargo_home_tmpfs=false
    cargo_target_tmpfs=false
    while IFS= read -r mount; do
        case "$mount" in
            *" /workspace ro,"*|*" /workspace ro "*) workspace_read_only=true ;;
            *" /cargo-home "*" - tmpfs "*) cargo_home_tmpfs=true ;;
            *" /cargo-target "*" - tmpfs "*) cargo_target_tmpfs=true ;;
        esac
    done < "$1"
    [ "$workspace_read_only" = true ] || { echo workspace-not-read-only; return; }
    [ "$cargo_home_tmpfs" = true ] || { echo cargo-home-not-tmpfs; return; }
    [ "$cargo_target_tmpfs" = true ] || { echo cargo-target-not-tmpfs; return; }
}
"#
    };
}

/// The rule proving the runtime-created compilation scratch starts empty.
macro_rules! cache_rule {
    () => {
        r#"
cache_violation() {
    for cache in "$@"; do
        for entry in "$cache"/* "$cache"/.[!.]* "$cache"/..?*; do
            [ ! -e "$entry" ] && [ ! -L "$entry" ] \
                || { echo cache-not-empty; return; }
        done
    done
}
"#
    };
}

const PREFLIGHT_SCRIPT: &str = concat!(
    network_rule!(),
    mount_rule!(),
    cache_rule!(),
    r#"
refuse() {
    printf 'spice-proof-preflight-refused:%s\n' "$1" >&2
    exit 70
}

[ "${CARGO_HOME:-}" = /cargo-home ] || refuse cargo-home-environment
[ "${CARGO_TARGET_DIR:-}" = /cargo-target ] || refuse cargo-target-environment
[ "${RUST_TEST_THREADS:-}" = 1 ] || refuse rust-test-threads-environment
[ -d /cargo-home ] || refuse cargo-home-missing
[ -d /cargo-target ] || refuse cargo-target-missing
[ -d /sys/class/net ] || refuse network-state-unavailable

violation=$(mount_violation /proc/self/mountinfo)
[ -z "$violation" ] || refuse "$violation"

violation=$(cache_violation /cargo-home /cargo-target)
[ -z "$violation" ] || refuse "$violation"
network_isolated /sys/class/net || refuse network-reaches-past-loopback
"#,
    attest_and_exec!()
);

const MEMBER_SCRIPT: &str = attest_and_exec!();

/// One observed OCI runtime interface.
#[derive(Clone, Debug)]
pub struct OciRuntime {
    kind: RuntimeKind,
    binary: PathBuf,
    launcher: Arc<dyn Launch>,
}
// The launcher is an execution seam, not part of the admitted runtime identity.
impl PartialEq for OciRuntime {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.binary == other.binary
    }
}

impl Eq for OciRuntime {}

impl OciRuntime {
    /// Admit one executable whose bounded identity response names Docker or Podman.
    ///
    /// # Errors
    ///
    /// Refuses non-absolute, missing, non-regular, or non-executable paths and
    /// any failed, incomplete, ambiguous, or unrecognized identity exchange.
    pub fn attest(binary: impl AsRef<Path>, limits: Limits) -> Result<Self, ContainerError> {
        Self::attest_with(Arc::new(System), binary.as_ref(), limits)
    }

    pub(crate) fn attest_with(
        launcher: Arc<dyn Launch>,
        binary: &Path,
        limits: Limits,
    ) -> Result<Self, ContainerError> {
        if !binary.is_absolute() {
            return Err(ContainerError::RuntimeNotAbsolute(binary.to_path_buf()));
        }
        let binary = fs::canonicalize(binary).map_err(|source| ContainerError::InspectRuntime {
            path: binary.to_path_buf(),
            source,
        })?;
        let metadata = fs::metadata(&binary).map_err(|source| ContainerError::InspectRuntime {
            path: binary.clone(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(ContainerError::RuntimeNotRegular(binary));
        }
        if !is_executable(&metadata) {
            return Err(ContainerError::RuntimeNotExecutable(binary));
        }
        let kind = observe_runtime_kind(launcher.as_ref(), &binary, limits)?;
        Ok(Self {
            kind,
            binary,
            launcher,
        })
    }

    /// Exact runtime executable admitted above.
    #[must_use]
    pub fn binary(&self) -> &Path {
        &self.binary
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeKind {
    Docker,
    Podman,
}

impl RuntimeKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Podman => "podman",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Attestation {
    challenge: Challenge,
}

impl Attestation {
    fn fresh() -> Result<Self, ContainerError> {
        Ok(Self {
            challenge: Challenge::fresh().map_err(ContainerError::CreateAttestationChallenge)?,
        })
    }

    fn marker(&self, kind: RuntimeKind) -> String {
        format!(
            "{ATTESTATION_PREFIX}{}:{}\n",
            kind.as_str(),
            self.challenge.as_str()
        )
    }

    #[cfg(test)]
    fn fixture(challenge: &str) -> Self {
        Self {
            challenge: Challenge::fixture(challenge),
        }
    }
}

/// Explicit process bounds for every battery member.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    deadlines: Deadlines,
    grace: Duration,
    output_limit: usize,
}

impl Limits {
    /// State all execution and evidence bounds.
    ///
    /// # Errors
    ///
    /// Refuses a zero output limit; deadline admission is owned by [`Deadlines`].
    pub fn new(
        deadlines: Deadlines,
        grace: Duration,
        output_limit: usize,
    ) -> Result<Self, ContainerError> {
        if output_limit == 0 {
            return Err(ContainerError::ZeroOutputLimit);
        }
        Ok(Self {
            deadlines,
            grace,
            output_limit,
        })
    }

    pub(crate) fn process_spec(
        self,
        argv: Argv,
        cwd: PathBuf,
        label: impl Into<String>,
    ) -> Result<Spec, SpecError> {
        Ok(Spec {
            argv,
            cwd,
            environment: Environment::empty(),
            group: ProcessGroup::New,
            deadlines: self.deadlines,
            wait: WaitPolicy::new(label, Cancellation::Never, RestartDisposition::Never)?,
            output: Output::new(
                StdinPolicy::Null,
                StreamPolicy::Capture {
                    limit: self.output_limit,
                },
                StreamPolicy::Capture {
                    limit: self.output_limit,
                },
            ),
            redaction: Redaction::None,
            grace: self.grace,
        })
    }
}

/// Why one bounded runtime probe produced no evidence.
///
/// The three process failures stay typed because they say something about this
/// host rather than about the runtime's answer; everything else collapses into
/// the single observation string its caller reports.
enum ProbeRefusal {
    Spec(SpecError),
    Spawn(std::io::Error),
    Wait(WaitError),
    Observed(String),
}

impl ProbeRefusal {
    fn dialect(self) -> ContainerError {
        match self {
            Self::Spec(source) => ContainerError::RuntimeProbeSpec(source),
            Self::Spawn(source) => ContainerError::SpawnRuntimeProbe(source),
            Self::Wait(source) => ContainerError::WaitRuntimeProbe(source),
            Self::Observed(observed) => ContainerError::RuntimeDialectNotAttested { observed },
        }
    }

    fn image(self, image: &str) -> ContainerError {
        match self {
            Self::Spec(source) => ContainerError::RuntimeProbeSpec(source),
            Self::Spawn(source) => ContainerError::SpawnRuntimeProbe(source),
            Self::Wait(source) => ContainerError::WaitRuntimeProbe(source),
            Self::Observed(observed) => ContainerError::ImageNotResolved {
                image: image.to_owned(),
                observed,
            },
        }
    }
}

/// Run one bounded runtime question and return the exact bytes it answered.
///
/// An answer counts only when the process exited successfully, both streams
/// were captured whole, and stderr stayed silent. Anything else is the
/// observation, and the caller decides which refusal it names.
fn probe(
    launcher: &dyn Launch,
    binary: &Path,
    arguments: &[&str],
    label: &'static str,
    limits: Limits,
) -> Result<Vec<u8>, ProbeRefusal> {
    let spec = Spec {
        argv: Argv::new(
            binary.as_os_str().to_owned(),
            arguments.iter().map(OsString::from),
        )
        .map_err(ProbeRefusal::Spec)?,
        cwd: binary
            .parent()
            .expect("a canonical absolute executable has a parent")
            .to_owned(),
        environment: Environment::empty(),
        group: ProcessGroup::New,
        deadlines: limits.deadlines,
        wait: WaitPolicy::new(label, Cancellation::Never, RestartDisposition::Never)
            .map_err(ProbeRefusal::Spec)?,
        output: Output::new(
            StdinPolicy::Null,
            StreamPolicy::Capture {
                limit: limits.output_limit,
            },
            StreamPolicy::Capture {
                limit: limits.output_limit,
            },
        ),
        redaction: Redaction::None,
        grace: limits.grace,
    };
    let execution = launcher.run(spec).map_err(|error| match error {
        LaunchError::Spawn(source) => ProbeRefusal::Spawn(source),
        LaunchError::Wait(source) => ProbeRefusal::Wait(source),
        LaunchError::MissingCapture(stream) => {
            ProbeRefusal::Observed(format!("{stream} capture missing"))
        }
    })?;
    let Execution {
        completion,
        stdout,
        stderr,
    } = execution;
    if !matches!(
        completion,
        ExecutionCompletion::Exited { success: true, .. }
    ) || stdout.truncated
        || stderr.truncated
        || !stdout.complete
        || !stderr.complete
        || !stderr.bytes.is_empty()
    {
        return Err(ProbeRefusal::Observed(format!(
            "completion={completion:?}, stdout={:?}, stderr={:?}",
            String::from_utf8_lossy(&stdout.bytes),
            String::from_utf8_lossy(&stderr.bytes)
        )));
    }
    Ok(stdout.bytes)
}

fn observe_runtime_kind(
    launcher: &dyn Launch,
    binary: &Path,
    limits: Limits,
) -> Result<RuntimeKind, ContainerError> {
    let stdout = probe(
        launcher,
        binary,
        &["--version"],
        "sealed OCI runtime identity probe",
        limits,
    )
    .map_err(ProbeRefusal::dialect)?;
    parse_runtime_kind(&stdout).ok_or_else(|| ContainerError::RuntimeDialectNotAttested {
        observed: format!("stdout={:?}", String::from_utf8_lossy(&stdout)),
    })
}

/// The one platform the sealed Linux appliance is assembled for.
const IMAGE_PLATFORM: &str = "amd64 linux";

/// The same platform in the spelling `run` takes, rather than the one
/// `image inspect --format` reports.
///
/// Stated rather than left to the runtime's default, because a runtime that
/// infers it from the host writes a mismatch warning onto the stream the
/// attestation is read from, and every non-amd64 host would then fail admission
/// with output it never produced. Nothing outside can repair that: the runtime
/// is spawned with an empty environment, so `DOCKER_DEFAULT_PLATFORM` cannot
/// reach it.
const RUN_PLATFORM: &str = "linux/amd64";

/// Resolve the current vendor set's deterministic local tag to the exact
/// identity, platform, and vendored source set this host actually holds.
///
/// A tag is only a lookup key. The exact image ID read from the same probe is
/// retained for every execution and payload pin. Platform and vendor-set label
/// are admitted before any container starts.
fn resolve_image(
    runtime: &OciRuntime,
    vendor_set: &VendorSet,
    limits: Limits,
) -> Result<ProofImage, ContainerError> {
    let tag = vendor_set.image_tag();
    let inspect_format = format!(
        "{{{{.Id}}}} {{{{.Architecture}}}} {{{{.Os}}}} {{{{index .Config.Labels \"{}\"}}}}",
        crate::VENDOR_SET_LABEL
    );
    let stdout = probe(
        runtime.launcher.as_ref(),
        &runtime.binary,
        &["image", "inspect", "--format", &inspect_format, &tag],
        "sealed proof image resolution",
        limits,
    )
    .map_err(|refusal| refusal.image(&tag))?;
    let observed = String::from_utf8_lossy(&stdout).trim().to_owned();
    let Some((observed_image, observed_vendor_set)) = observed.rsplit_once(' ') else {
        return Err(ContainerError::ImageIdentityDrift {
            expected: format!("one exact image ID, {IMAGE_PLATFORM}, and vendor-set label"),
            observed,
        });
    };
    let Some((identity, observed_platform)) = observed_image.split_once(' ') else {
        return Err(ContainerError::ImageIdentityDrift {
            expected: format!("one exact image ID and {IMAGE_PLATFORM}"),
            observed: observed_image.to_owned(),
        });
    };
    let image = identity
        .parse::<ProofImage>()
        .map_err(|_| ContainerError::ImageIdentityDrift {
            expected: "one exact SHA-256 image ID".to_owned(),
            observed: identity.to_owned(),
        })?;
    if observed_platform != IMAGE_PLATFORM {
        return Err(ContainerError::ImageIdentityDrift {
            expected: format!("{} {IMAGE_PLATFORM}", image.as_str()),
            observed: observed_image.to_owned(),
        });
    }
    if observed_vendor_set != vendor_set.as_str() {
        return Err(ContainerError::ImageVendorSetDrift {
            image: tag,
            expected: vendor_set.to_string(),
            observed: observed_vendor_set.to_owned(),
        });
    }
    Ok(image)
}

fn parse_runtime_kind(bytes: &[u8]) -> Option<RuntimeKind> {
    let version = std::str::from_utf8(bytes).ok()?;
    let version = version
        .strip_suffix("\r\n")
        .or_else(|| version.strip_suffix('\n'))
        .unwrap_or(version);
    if version.contains(['\r', '\n']) {
        return None;
    }
    if version
        .strip_prefix("Docker version ")
        .is_some_and(|remainder| !remainder.is_empty())
    {
        Some(RuntimeKind::Docker)
    } else if version
        .strip_prefix("podman version ")
        .is_some_and(|remainder| !remainder.is_empty())
    {
        Some(RuntimeKind::Podman)
    } else {
        None
    }
}

/// A network-disabled build environment with fresh empty cache mounts.
#[derive(Clone, Debug)]
pub struct Container {
    runtime: OciRuntime,
    image: ProofImage,
    workspace: PathBuf,
    limits: Limits,
}

impl Container {
    /// Admit one sealed OCI environment selected by the workspace vendor set.
    ///
    /// Every battery gets new tmpfs at Cargo's home and target paths, disables
    /// networking, mounts the source read-only, and clears the host environment
    /// before the runtime starts. All members enter that one battery scope.
    ///
    /// # Errors
    ///
    /// Refuses an absent or stale vendor-set image, an unreadable workspace or
    /// lockfile, or a mount path the OCI argument grammar cannot represent
    /// exactly.
    pub fn new(
        runtime: OciRuntime,
        workspace: impl AsRef<Path>,
        limits: Limits,
    ) -> Result<Self, ContainerError> {
        let requested = workspace.as_ref();
        let workspace =
            fs::canonicalize(requested).map_err(|source| ContainerError::InspectWorkspace {
                path: requested.to_path_buf(),
                source,
            })?;
        if !workspace.is_dir() {
            return Err(ContainerError::WorkspaceNotDirectory(workspace));
        }
        let Some(spelling) = workspace.to_str() else {
            return Err(ContainerError::UnrepresentableWorkspace(workspace));
        };
        if spelling.contains([',', '\n', '\r']) {
            return Err(ContainerError::UnrepresentableWorkspace(workspace));
        }
        let vendor_set = VendorSet::read(workspace.join("Cargo.lock"))
            .map_err(|source| ContainerError::VendorSet(Box::new(source)))?;
        let image = resolve_image(&runtime, &vendor_set, limits)?;

        Ok(Self {
            runtime,
            image,
            workspace,
            limits,
        })
    }

    /// Digest-pinned environment identity executed by every battery member.
    #[must_use]
    pub fn image(&self) -> &str {
        self.image.as_str()
    }

    pub(crate) const fn proof_image(&self) -> &ProofImage {
        &self.image
    }

    fn runtime_spec(
        &self,
        runtime_arguments: Vec<String>,
        label: impl Into<String>,
    ) -> Result<Spec, ContainerError> {
        self.limits
            .process_spec(
                Argv::new(
                    self.runtime.binary.as_os_str().to_owned(),
                    runtime_arguments.into_iter().map(OsString::from),
                )
                .map_err(ContainerError::Spec)?,
                self.workspace.clone(),
                label,
            )
            .map_err(ContainerError::Spec)
    }

    /// Start the sole owner of the battery's two anonymous scratch mounts.
    ///
    /// Members enter this container with `exec` below. Consequently the
    /// runtime, rather than a host path or persistent volume, creates the
    /// scratch once and destroys it with this exact container.
    fn battery_spec(&self, name: &str) -> Result<Spec, ContainerError> {
        let mount = format!(
            "type=bind,source={},destination={WORKSPACE_MOUNT},readonly",
            self.workspace.display()
        );
        let runtime_arguments = vec![
            "run".to_owned(),
            "--detach".to_owned(),
            "--name".to_owned(),
            name.to_owned(),
            "--platform".to_owned(),
            RUN_PLATFORM.to_owned(),
            "--network=none".to_owned(),
            "--read-only".to_owned(),
            "--cap-drop=ALL".to_owned(),
            "--security-opt=no-new-privileges".to_owned(),
            "--mount".to_owned(),
            mount,
            // Spelled with `--tmpfs` rather than `--mount type=tmpfs` because
            // only this flag takes the execute permission below: both admitted
            // runtimes accept arbitrary mount options here, while `--mount`
            // accepts only a size and a mode and rejects anything else. Both
            // mounts stay `nosuid,nodev` and stay tmpfs, which is what the
            // preflight reads them for.
            "--tmpfs".to_owned(),
            format!("{CARGO_HOME}:rw,noexec,mode=0700"),
            // The one mount that may execute, because it is the one members
            // write programs to: every member compiles into `CARGO_TARGET_DIR`
            // and then runs what it compiled, build scripts first and test
            // binaries after. Granted here alone rather than to both, so the
            // Cargo home stays a directory nothing can be run out of.
            // Four GiB is explicit rather than the runtime's host-dependent
            // half-memory default: the complete debug graph reaches beyond a
            // two-GiB Docker VM default, while the measured 3.9-GiB boundary
            // completes the whole mutation member.
            "--tmpfs".to_owned(),
            format!("{CARGO_TARGET}:rw,exec,mode=0700,size=4g"),
            // Rosetta, which translates this amd64 image on an arm64 Docker
            // host, backs a thread context with a temporary file under `/tmp`
            // when it cannot keep it in memory, and aborts the thread when
            // that file cannot be created. Under a read-only root that abort
            // took every new thread and process in a sealed stage and left
            // its `xtask` waiting on a mutex until killed. Empty, private to
            // this container, and the one scratch nothing is pointed at:
            // `TMPDIR` stays under the target directory the image fixes.
            "--tmpfs".to_owned(),
            format!("{TEMPORARY}:rw,nosuid,nodev,noexec,mode=1777"),
            "--workdir".to_owned(),
            WORKSPACE_MOUNT.to_owned(),
            "--env".to_owned(),
            format!("CARGO_HOME={CARGO_HOME}"),
            "--env".to_owned(),
            format!("CARGO_TARGET_DIR={CARGO_TARGET}"),
            "--env".to_owned(),
            "CARGO_NET_OFFLINE=true".to_owned(),
            // Debug symbols and incremental object graphs do not change test
            // semantics or evidence, but together exceed the bounded tmpfs
            // before Gate links. Keep the shared compilation scratch finite
            // and deterministic across developer VM memory settings.
            "--env".to_owned(),
            "CARGO_PROFILE_TEST_DEBUG=0".to_owned(),
            "--env".to_owned(),
            "CARGO_INCREMENTAL=0".to_owned(),
            // Docker Desktop translates this amd64 appliance when its host is
            // arm64. Concurrent libtest children can then outlive a completed
            // process wait and strand the harness. One explicit harness thread
            // keeps every member deterministic without relaxing any battery
            // assertion or changing which tests run.
            "--env".to_owned(),
            format!("RUST_TEST_THREADS={RUST_TEST_THREADS}"),
            self.image.to_string(),
            "/bin/sh".to_owned(),
            "-ceu".to_owned(),
            BATTERY_KEEPALIVE_SCRIPT.to_owned(),
        ];
        self.runtime_spec(runtime_arguments, "sealed proof battery scratch owner")
    }

    fn exec_spec(
        &self,
        name: &str,
        script: &str,
        program: &str,
        arguments: &[String],
        attestation: &Attestation,
        label: impl Into<String>,
    ) -> Result<Spec, ContainerError> {
        validate_command(program, arguments)?;
        let mut runtime_arguments = vec![
            "exec".to_owned(),
            name.to_owned(),
            "/bin/sh".to_owned(),
            "-ceu".to_owned(),
            script.to_owned(),
            "spice-proof-attestation".to_owned(),
            self.runtime.kind.as_str().to_owned(),
            attestation.challenge.as_str().to_owned(),
            program.to_owned(),
        ];
        runtime_arguments.extend(arguments.iter().cloned());
        self.runtime_spec(runtime_arguments, label)
    }

    fn removal_spec(&self, name: &str) -> Result<Spec, ContainerError> {
        self.runtime_spec(
            vec!["rm".to_owned(), "--force".to_owned(), name.to_owned()],
            "sealed proof battery scratch cleanup",
        )
    }

    fn run(&self, spec: Spec) -> Result<Execution, ContainerError> {
        self.runtime
            .launcher
            .run(spec)
            .map_err(|error| match error {
                LaunchError::Spawn(source) => ContainerError::SpawnRuntime(source),
                LaunchError::Wait(source) => ContainerError::Wait(source),
                LaunchError::MissingCapture(stream) => ContainerError::MissingCapture(stream),
            })
    }

    fn run_attested(
        &self,
        spec: Spec,
        attestation: &Attestation,
    ) -> Result<Execution, ContainerError> {
        let mut execution = self.run(spec)?;
        self.verify_attestation(&mut execution.stderr, attestation)?;
        Ok(execution)
    }

    fn require_control(
        operation: &'static str,
        execution: &Execution,
    ) -> Result<(), ContainerError> {
        if matches!(
            execution.completion,
            ExecutionCompletion::Exited { success: true, .. }
        ) && !execution.stdout.truncated
            && !execution.stderr.truncated
            && execution.stdout.complete
            && execution.stderr.complete
            && execution.stderr.bytes.is_empty()
        {
            return Ok(());
        }
        Err(Self::control_refusal(operation, execution))
    }

    fn control_refusal(operation: &'static str, execution: &Execution) -> ContainerError {
        ContainerError::RuntimeControlRefused {
            operation,
            observed: format!(
                "completion={:?}, stdout={:?}, stderr={:?}",
                execution.completion,
                String::from_utf8_lossy(&execution.stdout.bytes),
                String::from_utf8_lossy(&execution.stderr.bytes)
            ),
        }
    }

    pub(crate) fn battery(&self) -> Result<ContainerBattery<'_>, ContainerError> {
        let scratch = Challenge::fresh().map_err(ContainerError::CreateAttestationChallenge)?;
        let name = format!("spice-proof-{}", scratch.as_str());
        let started = self.run(self.battery_spec(&name)?)?;
        if !matches!(
            started.completion,
            ExecutionCompletion::Exited { success: true, .. }
        ) {
            return Err(Self::control_refusal("start battery scratch", &started));
        }
        let mut battery = ContainerBattery {
            container: self,
            identifier: Some(name),
        };
        Self::require_control("start battery scratch", &started)?;
        let observed = std::str::from_utf8(&started.stdout.bytes)
            .ok()
            .and_then(|stdout| {
                stdout
                    .strip_suffix("\r\n")
                    .or_else(|| stdout.strip_suffix('\n'))
            })
            .and_then(|identifier| identifier.parse::<Challenge>().ok());
        let Some(identifier) = observed else {
            return Err(ContainerError::RuntimeControlRefused {
                operation: "identify battery scratch",
                observed: format!(
                    "stdout={:?}",
                    String::from_utf8_lossy(&started.stdout.bytes)
                ),
            });
        };
        battery.identifier = Some(identifier.to_string());

        let attestation = Attestation::fresh()?;
        let preflight = battery.container.run_attested(
            battery.container.exec_spec(
                battery.identifier(),
                PREFLIGHT_SCRIPT,
                "/bin/true",
                &[],
                &attestation,
                "sealed proof battery boundary preflight",
            )?,
            &attestation,
        )?;
        Self::require_control("preflight battery scratch", &preflight)?;
        Ok(battery)
    }

    fn verify_attestation(
        &self,
        stderr: &mut Captured,
        attestation: &Attestation,
    ) -> Result<(), ContainerError> {
        let expected = attestation.marker(self.runtime.kind);
        if !stderr.bytes.starts_with(expected.as_bytes()) {
            return Err(ContainerError::RuntimeNotAttested {
                expected,
                observed: String::from_utf8_lossy(&stderr.bytes).into_owned(),
            });
        }
        stderr.bytes.drain(..expected.len());
        Ok(())
    }
}

/// The one battery-scoped owner of both anonymous compilation scratch mounts.
#[derive(Debug)]
pub(crate) struct ContainerBattery<'a> {
    container: &'a Container,
    identifier: Option<String>,
}

impl ContainerBattery<'_> {
    fn identifier(&self) -> &str {
        self.identifier
            .as_deref()
            .expect("an open battery always identifies its scratch owner")
    }

    pub(crate) fn execute(
        &self,
        program: &str,
        arguments: &[String],
    ) -> Result<Execution, ContainerError> {
        let attestation = Attestation::fresh()?;
        self.container.run_attested(
            self.container.exec_spec(
                self.identifier(),
                MEMBER_SCRIPT,
                program,
                arguments,
                &attestation,
                format!("sealed proof battery member {program:?}"),
            )?,
            &attestation,
        )
    }

    fn remove(&mut self) -> Result<(), ContainerError> {
        let Some(identifier) = self.identifier.take() else {
            return Ok(());
        };
        let removal = self
            .container
            .removal_spec(&identifier)
            .and_then(|spec| self.container.run(spec))
            .and_then(|removed| Container::require_control("remove battery scratch", &removed));
        if let Err(error) = removal {
            self.identifier = Some(identifier);
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn close(mut self) -> Result<(), ContainerError> {
        self.remove()
    }
}

impl Drop for ContainerBattery<'_> {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Execution {
    pub(crate) completion: ExecutionCompletion,
    pub(crate) stdout: Captured,
    pub(crate) stderr: Captured,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExecutionCompletion {
    Exited { success: bool, code: Option<i32> },
    TimedOut { code: Option<i32> },
    Cancelled { code: Option<i32> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Captured {
    pub(crate) bytes: Vec<u8>,
    pub(crate) truncated: bool,
    pub(crate) complete: bool,
}

impl From<&sill::CapturedOutput> for Captured {
    fn from(output: &sill::CapturedOutput) -> Self {
        Self {
            bytes: output.bytes().to_vec(),
            truncated: output.truncated(),
            complete: output.complete(),
        }
    }
}

#[cfg(test)]
mod tests;
