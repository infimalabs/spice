//! GitHub-hosted disposable native release execution with durable resume.

use std::error::Error;
use std::fmt;
use std::io::Write;
use std::path::Path;

use crate::hosted_release_session::{
    NativeTarget, ReleaseSession, SessionError, terminal_provider_failure,
};
pub use windows_execution::hosted::client::{
    Attach, HostedExecution, ReleaseInputs, WindowsHostedError, windows_hosted,
};
use windows_execution::hosted::client::{HostedRunControl, release_workflow_blobs};
pub fn hosted_release_inputs(attach: Attach) -> Result<ReleaseInputs, WindowsHostedError> {
    windows_execution::hosted::client::release_inputs(Some(attach))
}

/// Dispatch one fresh provider preflight for the current landed source.
pub fn hosted_release_inputs_fresh() -> Result<ReleaseInputs, WindowsHostedError> {
    windows_execution::hosted::client::release_inputs(None)
}
/// Dispatch or resume all native release stages concurrently from one admitted
/// provider preflight. Every run binding lands in the Git-common session before
/// its watcher starts.
pub fn hosted_release_stage(
    inputs_run: Attach,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), HostedReleaseError> {
    let Attach::Run(inputs_run) = inputs_run else {
        return Err(HostedReleaseError::Refused(
            "aggregate hosted release requires one exact preflight run".to_owned(),
        ));
    };
    let inputs = hosted_release_inputs(Attach::Run(inputs_run))?;
    hosted_release_stage_with_inputs(inputs_run, &inputs, output, error_output)
}

/// Create or resume all native stages from already admitted release inputs.
pub fn hosted_release_stage_with_inputs(
    inputs_run: u64,
    inputs: &ReleaseInputs,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), HostedReleaseError> {
    let workflows = release_workflow_blobs()?;
    let session = ReleaseSession::open(inputs_run, workflows, inputs)?;
    writeln!(
        output,
        "hosted release session {}",
        session.path().display()
    )
    .map_err(WindowsHostedError::Output)?;

    let linux_attach = session.attachment(NativeTarget::Linux)?;
    let macos_attach = session.attachment(NativeTarget::Macos)?;
    let windows_attach = session.attachment(NativeTarget::Windows)?;
    let linux_recorder = session.recorder(NativeTarget::Linux);
    let macos_recorder = session.recorder(NativeTarget::Macos);
    let windows_recorder = session.recorder(NativeTarget::Windows);

    let outcomes = std::thread::scope(|scope| {
        let linux = scope.spawn(|| {
            let mut output = Vec::new();
            let control = HostedRunControl::resumable(&linux_recorder);
            let result = linux_hosted_stage_controlled(inputs, linux_attach, control, &mut output);
            PlatformOutcome::new(NativeTarget::Linux, output, Vec::new(), result)
        });
        let macos = scope.spawn(|| {
            let mut output = Vec::new();
            let control = HostedRunControl::resumable(&macos_recorder);
            let result = macos_hosted_stage_controlled(inputs, macos_attach, control, &mut output);
            PlatformOutcome::new(NativeTarget::Macos, output, Vec::new(), result)
        });
        let windows = scope.spawn(|| {
            let mut output = Vec::new();
            let mut error = Vec::new();
            let control = HostedRunControl::resumable(&windows_recorder);
            let result = windows_hosted_stage_controlled(
                inputs,
                windows_attach,
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

/// Stage the signed macOS release target on one fresh provider-hosted machine,
/// or re-enter the provider run `attach` names, and publish its
/// provider-attested files into `dist/stage`.
pub fn macos_hosted_stage(
    inputs: &ReleaseInputs,
    attach: Option<Attach>,
    output: &mut impl Write,
) -> Result<(), WindowsHostedError> {
    macos_hosted_stage_controlled(inputs, attach, HostedRunControl::default(), output)
}

fn macos_hosted_stage_controlled(
    inputs: &ReleaseInputs,
    attach: Option<Attach>,
    control: HostedRunControl<'_>,
    output: &mut impl Write,
) -> Result<(), WindowsHostedError> {
    let stage = windows_execution::hosted::client::macos_hosted_stage_controlled(
        inputs,
        attach,
        Path::new("dist/stage"),
        control,
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
    linux_hosted_stage_controlled(inputs, attach, HostedRunControl::default(), output)
}

fn linux_hosted_stage_controlled(
    inputs: &ReleaseInputs,
    attach: Option<Attach>,
    control: HostedRunControl<'_>,
    output: &mut impl Write,
) -> Result<(), WindowsHostedError> {
    let stage = windows_execution::hosted::client::linux_hosted_stage_controlled(
        inputs,
        attach,
        Path::new("dist/stage"),
        control,
    )?;
    output
        .write_all(&stage.log)
        .map_err(WindowsHostedError::Output)?;
    output
        .write_all(&stage.proof_log)
        .map_err(WindowsHostedError::Output)?;
    writeln!(
        output,
        "hosted Linux run {} on {} with proof image {} staged {} and proved {}",
        stage.run_id, stage.runner_image, stage.proof_image, stage.exit_code, stage.proof_exit_code
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
/// the provider inputs, or re-enter the provider run `attach` names.
pub fn windows_hosted_stage(
    inputs: &ReleaseInputs,
    attach: Option<Attach>,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<HostedExecution, WindowsHostedError> {
    windows_hosted_stage_controlled(
        inputs,
        attach,
        HostedRunControl::default(),
        output,
        error_output,
    )
}

fn windows_hosted_stage_controlled(
    inputs: &ReleaseInputs,
    attach: Option<Attach>,
    control: HostedRunControl<'_>,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<HostedExecution, WindowsHostedError> {
    windows_execution::hosted::client::require_public_attestation_repository()?;
    let arguments = vec![
        "run".to_owned(),
        "--locked".to_owned(),
        "-p".to_owned(),
        "xtask".to_owned(),
        "--".to_owned(),
        "stage-target".to_owned(),
        "x86_64-pc-windows-msvc".to_owned(),
        "--unsigned".to_owned(),
        "--proof-container".to_owned(),
        inputs.manifest().proof_identity.to_owned(),
    ];
    let command = windows_execution::protocol::Command {
        program: "@cargo".to_owned(),
        arguments,
    };
    let execution = windows_execution::hosted::client::windows_release_commands_into_controlled(
        vec![command],
        Path::new("dist/stage"),
        inputs,
        attach,
        control,
    )?;
    let first = &execution.outputs[0];
    output
        .write_all(&first.stdout)
        .map_err(WindowsHostedError::Output)?;
    error_output
        .write_all(&first.stderr)
        .map_err(WindowsHostedError::Output)?;
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
