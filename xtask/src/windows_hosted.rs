//! GitHub-hosted disposable native release execution with durable resume.

use std::error::Error;
use std::fmt;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::hosted_release_session::{
    NativeTarget, ReleaseSession, SessionError, TargetWorkflow, schema_provider_failure,
    terminal_provider_failure,
};
pub use windows_execution::hosted::client::{
    Attach, HostedExecution, ReleaseInputs, WindowsHostedError, landed_commit, windows_hosted,
};
use windows_execution::hosted::client::{
    HostedRunControl, HostedRunObserver, HostedWorkflow, Preflight, PreparedBundleTransport,
};

mod cross_bundle;
mod package_read;

pub use cross_bundle::CrossHost;
use package_read::require_prepared_read;

struct PreparedBundles;

static PREPARED_BUNDLES: PreparedBundles = PreparedBundles;

impl PreparedBundleTransport for PreparedBundles {
    fn fetch(
        &self,
        repository: &str,
        tag: &str,
        build_key: &str,
        destination: &Path,
    ) -> Result<(), String> {
        let repository = prepared_repository(repository);
        with_ghcr_login(|| {
            crate::prepared_artifact::fetch_bundle(&repository, tag, build_key, destination)
                .map_err(|error| error.to_string())
        })
    }
}

/// The GHCR package holding `repository`'s prepared artifacts.
fn prepared_repository(repository: &str) -> String {
    format!("ghcr.io/{}/prepared", repository.to_ascii_lowercase())
}

/// Run `operation` while ORAS holds this machine's `gh` credential for GHCR,
/// then log out again.
fn with_ghcr_login<T>(operation: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let user = command_text("gh", &["api", "user", "--jq", ".login"])?;
    let token = command_text("gh", &["auth", "token"])?;
    let mut login = crate::prepared_artifact::exact_oras_command()
        .map_err(|error| error.to_string())?
        .args([
            "login",
            "ghcr.io",
            "--username",
            user.trim(),
            "--password-stdin",
        ])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not launch oras login: {error}"))?;
    login
        .stdin
        .take()
        .expect("piped login input")
        .write_all(token.trim().as_bytes())
        .map_err(|error| format!("could not supply GHCR credential: {error}"))?;
    let status = login
        .wait()
        .map_err(|error| format!("could not wait for oras login: {error}"))?;
    if !status.success() {
        return Err(format!("oras login refused with {status}"));
    }
    let result = operation();
    let logout = crate::prepared_artifact::exact_oras_command().and_then(|mut command| {
        command
            .args(["logout", "ghcr.io"])
            .status()
            .map_err(prepared_artifact::PreparedArtifactError::Launch)
    });
    match (result, logout) {
        (Ok(value), Ok(status)) if status.success() => Ok(value),
        (Ok(_), Ok(status)) => Err(format!("oras logout refused with {status}")),
        (Ok(_), Err(error)) => Err(error.to_string()),
        (Err(error), _) => Err(error),
    }
}

fn command_text(program: &str, arguments: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| format!("could not launch {program}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} {} refused with {}: {}",
            arguments.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| format!("{program} returned non-UTF-8 output: {error}"))
}

/// Re-enter one provider preflight; its signed result names the source.
pub fn hosted_release_inputs(attach: Attach) -> Result<ReleaseInputs, WindowsHostedError> {
    windows_execution::hosted::client::release_inputs(Preflight::Attach(attach))
}

/// Dispatch one fresh provider preflight of `source_commit`, once this
/// coordinator has proven it can read the prepared bundles the release will
/// fetch.
pub fn hosted_release_inputs_dispatch(
    source_commit: String,
) -> Result<ReleaseInputs, WindowsHostedError> {
    require_prepared_read()?;
    windows_execution::hosted::client::release_inputs(Preflight::Dispatch(source_commit))
}

/// Dispatch or resume all native release stages concurrently from one admitted
/// provider preflight. Every run binding lands in the Git-common session before
/// its watcher starts.
pub fn hosted_release_stage(
    inputs_run: Attach,
    host: &CrossHost,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), HostedReleaseError> {
    let Attach::Run(inputs_run) = inputs_run else {
        return Err(HostedReleaseError::Refused(
            "aggregate hosted release requires one exact preflight run".to_owned(),
        ));
    };
    let inputs = hosted_release_inputs(Attach::Run(inputs_run))?;
    hosted_release_stage_with_inputs(inputs_run, &inputs, host, output, error_output)
}

/// Create or resume all native stages from already admitted release inputs.
pub fn hosted_release_stage_with_inputs(
    inputs_run: u64,
    inputs: &ReleaseInputs,
    host: &CrossHost,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), HostedReleaseError> {
    hosted_release_stage_with_inputs_into(
        inputs_run,
        inputs,
        host,
        Path::new("dist/stage"),
        output,
        error_output,
    )
}

/// Create or resume all native stages beneath a caller-owned stage root.
pub(crate) fn hosted_release_stage_with_inputs_into(
    inputs_run: u64,
    inputs: &ReleaseInputs,
    host: &CrossHost,
    stage_root: &Path,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), HostedReleaseError> {
    require_prepared_read()?;
    let session = ReleaseSession::open(inputs_run, inputs.workflows().clone(), inputs)?;
    write_session_path(output, &session)?;

    let linux_attach = session.attachment(NativeTarget::Linux)?;
    let macos_attach = session.attachment(NativeTarget::Macos)?;
    let windows_attach = session.attachment(NativeTarget::Windows)?;
    let linux_workflow = session.workflow(NativeTarget::Linux)?;
    let macos_workflow = session.workflow(NativeTarget::Macos)?;
    let windows_workflow = session.workflow(NativeTarget::Windows)?;
    let linux_recorder = session.recorder(NativeTarget::Linux);
    let macos_recorder = session.recorder(NativeTarget::Macos);
    let windows_recorder = session.recorder(NativeTarget::Windows);

    let outcomes = std::thread::scope(|scope| {
        let linux = scope.spawn(|| {
            let mut output = Vec::new();
            let control = resumable_control(&linux_recorder, &linux_workflow);
            let result = linux_hosted_stage_controlled(
                inputs,
                linux_attach,
                stage_root,
                control,
                &mut output,
            );
            PlatformOutcome::new(NativeTarget::Linux, output, Vec::new(), result)
        });
        let macos = scope.spawn(|| {
            let mut output = Vec::new();
            let control = resumable_control(&macos_recorder, &macos_workflow);
            let result = macos_hosted_stage_controlled(
                inputs,
                macos_attach,
                stage_root,
                control,
                &mut output,
            );
            PlatformOutcome::new(NativeTarget::Macos, output, Vec::new(), result)
        });
        let windows = scope.spawn(|| {
            let mut output = Vec::new();
            let mut error = Vec::new();
            let control = resumable_control(&windows_recorder, &windows_workflow);
            let result = windows_hosted_stage_controlled(
                inputs,
                host,
                windows_attach,
                stage_root,
                control,
                &mut output,
                &mut error,
            )
            .and_then(|execution| {
                (execution.exit_code() == 0).then_some(()).ok_or_else(|| {
                    WindowsHostedError::ProviderResponse(format!(
                        "hosted Windows staging exited {}",
                        execution.exit_code()
                    ))
                })
            });
            PlatformOutcome::new(NativeTarget::Windows, output, error, result)
        });
        [
            linux.join().expect("Linux hosted stage panicked"),
            macos.join().expect("macOS hosted stage panicked"),
            windows.join().expect("Windows hosted stage panicked"),
        ]
    });

    let mut failures = Vec::new();
    for outcome in outcomes {
        output
            .write_all(&outcome.output)
            .map_err(WindowsHostedError::Output)?;
        error_output
            .write_all(&outcome.error_output)
            .map_err(WindowsHostedError::Output)?;
        match outcome.result {
            Ok(()) => {
                if let Err(error) = session.succeeded(outcome.target) {
                    failures.push(format!("{} session record: {error}", outcome.target.name()));
                }
                writeln!(output, "hosted {} stage succeeded", outcome.target.name())
                    .map_err(WindowsHostedError::Output)?;
            }
            Err(error) => {
                let detail = error.to_string();
                if let Err(error) = session.failed(
                    outcome.target,
                    terminal_provider_failure(&error) || detail.contains(" staging exited "),
                    schema_provider_failure(&error),
                    detail.clone(),
                ) {
                    failures.push(format!("{} session record: {error}", outcome.target.name()));
                }
                writeln!(
                    error_output,
                    "hosted {} stage failed: {detail}",
                    outcome.target.name()
                )
                .map_err(WindowsHostedError::Output)?;
                failures.push(format!("{}: {detail}", outcome.target.name()));
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(HostedReleaseError::Platform(failures))
    }
}

fn write_session_path(
    output: &mut impl Write,
    session: &ReleaseSession,
) -> Result<(), WindowsHostedError> {
    writeln!(
        output,
        "hosted release session {}",
        session.path().display()
    )
    .map_err(WindowsHostedError::Output)
}

fn resumable_control<'a>(
    observer: &'a dyn HostedRunObserver,
    workflow: &'a TargetWorkflow,
) -> HostedRunControl<'a> {
    let workflow = match workflow {
        TargetWorkflow::Source => HostedWorkflow::Source,
        TargetWorkflow::Current => HostedWorkflow::Current,
        TargetWorkflow::Exact(blob) => HostedWorkflow::Exact(blob),
    };
    HostedRunControl::resumable(observer, workflow).with_prepared_bundles(&PREPARED_BUNDLES)
}

/// Stage the signed macOS release target on one fresh provider-hosted machine,
/// or re-enter the provider run `attach` names, and publish its
/// provider-attested files into `dist/stage`.
pub fn macos_hosted_stage(
    inputs: &ReleaseInputs,
    attach: Option<Attach>,
    output: &mut impl Write,
) -> Result<(), WindowsHostedError> {
    require_prepared_read()?;
    macos_hosted_stage_controlled(
        inputs,
        attach,
        Path::new("dist/stage"),
        HostedRunControl::default().with_prepared_bundles(&PREPARED_BUNDLES),
        output,
    )
}

fn macos_hosted_stage_controlled(
    inputs: &ReleaseInputs,
    attach: Option<Attach>,
    stage_root: &Path,
    control: HostedRunControl<'_>,
    output: &mut impl Write,
) -> Result<(), WindowsHostedError> {
    let stage = windows_execution::hosted::client::macos_hosted_stage_controlled(
        inputs, attach, stage_root, control,
    )?;
    writeln!(
        output,
        "hosted macOS run {} on {} staged aarch64-apple-darwin",
        stage.run_id, stage.runner_image
    )
    .map_err(WindowsHostedError::Output)
}

/// Stage the Linux release target on one fresh provider-hosted machine from
/// the release source archive at `archive`, or re-enter the provider run
/// `attach` names, writing the retained stage log to `output` and publishing
/// the staged files into `dist/stage` when it exits zero.
pub fn linux_hosted_stage(
    inputs: &ReleaseInputs,
    attach: Option<Attach>,
    output: &mut impl Write,
) -> Result<(), WindowsHostedError> {
    linux_hosted_stage_controlled(
        inputs,
        attach,
        Path::new("dist/stage"),
        HostedRunControl::default(),
        output,
    )
}

fn linux_hosted_stage_controlled(
    inputs: &ReleaseInputs,
    attach: Option<Attach>,
    stage_root: &Path,
    control: HostedRunControl<'_>,
    output: &mut impl Write,
) -> Result<(), WindowsHostedError> {
    let stage = windows_execution::hosted::client::linux_hosted_stage_controlled(
        inputs, attach, stage_root, control,
    )?;
    output
        .write_all(&stage.log)
        .map_err(WindowsHostedError::Output)?;
    output
        .write_all(&stage.smoke_log)
        .map_err(WindowsHostedError::Output)?;
    writeln!(
        output,
        "hosted Linux run {} on {} with proof image {} stage exit {} and native smoke exit {}",
        stage.run_id, stage.runner_image, stage.proof_image, stage.exit_code, stage.smoke_exit_code
    )
    .map_err(WindowsHostedError::Output)?;
    if stage.exit_code == 0 {
        Ok(())
    } else {
        Err(WindowsHostedError::ProviderResponse(format!(
            "hosted Linux staging exited {}",
            stage.exit_code
        )))
    }
}

/// Stage the Windows release target on one fresh provider-hosted machine from
/// the provider inputs and this host's cross-built bundle, or re-enter the
/// provider run `attach` names.
pub fn windows_hosted_stage(
    inputs: &ReleaseInputs,
    host: &CrossHost,
    attach: Option<Attach>,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<HostedExecution, WindowsHostedError> {
    require_prepared_read()?;
    windows_hosted_stage_controlled(
        inputs,
        host,
        attach,
        Path::new("dist/stage"),
        HostedRunControl::default().with_prepared_bundles(&PREPARED_BUNDLES),
        output,
        error_output,
    )
}

fn windows_hosted_stage_controlled(
    inputs: &ReleaseInputs,
    host: &CrossHost,
    attach: Option<Attach>,
    stage_root: &Path,
    control: HostedRunControl<'_>,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<HostedExecution, WindowsHostedError> {
    let repository = windows_execution::hosted::client::require_public_attestation_repository()?;
    let bundle = cross_bundle::release_bundle(host, inputs, &repository, output, error_output)?;
    let mut commands = vec![bundle.stage];
    commands.extend_from_slice(bundle.battery.commands());
    let execution = windows_execution::hosted::client::windows_release_commands_into_controlled(
        commands, bundle.pin, stage_root, inputs, attach, control,
    )?;
    let first = &execution.outputs[0];
    output
        .write_all(&first.stdout)
        .map_err(WindowsHostedError::Output)?;
    error_output
        .write_all(&first.stderr)
        .map_err(WindowsHostedError::Output)?;
    if execution.exit_code() == 0 {
        let layout = proof::Layout::at_stage(
            stage_root,
            stage_root.join("proofs"),
            wire::manifest::ReleaseTarget::X86_64PcWindowsMsvc,
        );
        proof::seal_hosted_windows(&layout, &execution, 1, &bundle.battery).map_err(|error| {
            WindowsHostedError::ProviderResponse(format!(
                "hosted Windows proof receipt refused: {error}"
            ))
        })?;
    }
    Ok(execution)
}

struct PlatformOutcome {
    target: NativeTarget,
    output: Vec<u8>,
    error_output: Vec<u8>,
    result: Result<(), WindowsHostedError>,
}

impl PlatformOutcome {
    fn new(
        target: NativeTarget,
        output: Vec<u8>,
        error_output: Vec<u8>,
        result: Result<(), WindowsHostedError>,
    ) -> Self {
        Self {
            target,
            output,
            error_output,
            result,
        }
    }
}

/// Aggregate hosted release session failure.
#[derive(Debug)]
pub enum HostedReleaseError {
    Hosted(WindowsHostedError),
    Session(String),
    Refused(String),
    Platform(Vec<String>),
}

impl fmt::Display for HostedReleaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hosted(source) => source.fmt(formatter),
            Self::Session(detail) => formatter.write_str(detail),
            Self::Refused(detail) => formatter.write_str(detail),
            Self::Platform(failures) => write!(
                formatter,
                "hosted release stages failed: {}",
                failures.join("; ")
            ),
        }
    }
}

impl Error for HostedReleaseError {}

impl From<WindowsHostedError> for HostedReleaseError {
    fn from(source: WindowsHostedError) -> Self {
        Self::Hosted(source)
    }
}

impl From<SessionError> for HostedReleaseError {
    fn from(source: SessionError) -> Self {
        Self::Session(source.to_string())
    }
}
