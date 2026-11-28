//! GitHub-hosted disposable native release execution with durable resume.

use std::error::Error;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::hosted_release_session::{
    NativeTarget, ReleaseSession, SessionError, TargetRecorder, TargetWorkflow,
    schema_provider_failure, terminal_provider_failure,
};
pub use windows_execution::hosted::client::{
    Attach, HostedExecution, ReleaseInputs, WindowsHostedError, landed_commit, windows_hosted,
};
use windows_execution::hosted::client::{
    HostedRunControl, HostedRunObserver, HostedWorkflow, Preflight, PreparedBundleTransport,
};

mod command;
mod cross_bundle;
mod package_read;

pub use cross_bundle::CrossHost;
use package_read::require_prepared_read;

struct PreparedBundles {
    environment: sill::Environment,
    cwd: PathBuf,
}

impl PreparedBundles {
    fn new(environment: sill::Environment, cwd: PathBuf) -> Self {
        Self { environment, cwd }
    }
}

impl PreparedBundleTransport for PreparedBundles {
    fn fetch(
        &self,
        repository: &str,
        tag: &str,
        build_key: &str,
        destination: &Path,
    ) -> Result<(), String> {
        let repository = prepared_repository(repository);
        with_ghcr_login(&self.environment, &self.cwd, || {
            crate::prepared_artifact::fetch_bundle(&repository, tag, build_key, destination)
                .map_err(|error| error.to_string())
        })
    }
}

/// The GHCR package holding `repository`'s prepared artifacts.
fn prepared_repository(repository: &str) -> String {
    format!("ghcr.io/{}/prepared", repository.to_ascii_lowercase())
}

/// GHCR authenticates a token alone, so one fixed username serves a release
/// host's personal token and a workflow's installation token alike; the
/// latter cannot read its own GitHub user.
const GHCR_USERNAME: &str = "spice-release";
const REGISTRY_AUTH_RUNTIME: Duration = Duration::from_secs(2 * 60);

/// Run `operation` while Docker and ORAS can read this machine's `gh`
/// credential for GHCR, then remove that credential again.
fn with_ghcr_login<T>(
    environment: &sill::Environment,
    cwd: &Path,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let token = command_text(
        "GitHub registry token lookup",
        REGISTRY_AUTH_RUNTIME,
        environment,
        cwd,
        "gh",
        &["auth", "token"],
    )?;
    command::run(
        "GHCR login",
        REGISTRY_AUTH_RUNTIME,
        "docker",
        [
            "login",
            "ghcr.io",
            "--username",
            GHCR_USERNAME,
            "--password-stdin",
        ],
        environment,
        cwd,
        Some(token.trim().as_bytes()),
    )?;
    let result = operation();
    let logout = command::run(
        "GHCR logout",
        REGISTRY_AUTH_RUNTIME,
        "docker",
        ["logout", "ghcr.io"],
        environment,
        cwd,
        None,
    );
    match (result, logout) {
        (Ok(value), Ok(_)) => Ok(value),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), _) => Err(error),
    }
}

fn command_text(
    operation: &str,
    runtime: Duration,
    environment: &sill::Environment,
    cwd: &Path,
    program: &str,
    arguments: &[&str],
) -> Result<String, String> {
    command::run(
        operation,
        runtime,
        program,
        arguments,
        environment,
        cwd,
        None,
    )?
    .stdout_text(operation)
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
    host: &CrossHost,
) -> Result<ReleaseInputs, WindowsHostedError> {
    let cwd = std::env::current_dir().map_err(|source| WindowsHostedError::Io {
        path: PathBuf::from("."),
        source,
    })?;
    require_prepared_read(&host.ambient, &cwd)?;
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
    let inputs = Arc::new(hosted_release_inputs(Attach::Run(inputs_run))?);
    hosted_release_stage_with_inputs_into(
        inputs_run,
        &inputs,
        host,
        Path::new("dist/stage"),
        output,
        error_output,
    )
    .map(drop)
}

/// When one native target's stage ran and which provider run it admitted.
pub(crate) struct TargetSpan {
    pub(crate) target: NativeTarget,
    pub(crate) started: SystemTime,
    pub(crate) finished: SystemTime,
    pub(crate) run: Option<u64>,
}

/// The native targets every aggregate hosted release stages.
const NATIVE_TARGETS: [NativeTarget; 3] = [
    NativeTarget::Linux,
    NativeTarget::Macos,
    NativeTarget::Windows,
];

/// Longest the coordinator log stays silent while a native stage runs.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(4 * 60);

/// Create or resume all native stages beneath a caller-owned stage root.
///
/// Each target starts as soon as the admitted inputs exist and never waits on a
/// sibling; its lines reach `output` and `error_output` as it writes them. The
/// first failed target ends the wait; the answer names when each finished
/// target ran and its admitted provider run.
pub(crate) fn hosted_release_stage_with_inputs_into(
    inputs_run: u64,
    inputs: &Arc<ReleaseInputs>,
    host: &CrossHost,
    stage_root: &Path,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<Vec<TargetSpan>, HostedReleaseError> {
    let session = ReleaseSession::open(inputs_run, inputs.workflows().clone(), inputs)?;
    write_session_path(output, &session)?;
    require_prepared_read(&host.ambient, inputs.tree())?;

    let mut stages = Vec::with_capacity(NATIVE_TARGETS.len());
    for target in NATIVE_TARGETS {
        stages.push(NativeStage {
            target,
            attach: session.attachment(target)?,
            workflow: session.workflow(target)?,
            recorder: session.recorder(target),
        });
    }
    let host = Arc::new(host.clone());
    let (events, received) = mpsc::channel();
    for stage in stages {
        stage.spawn(
            Arc::clone(inputs),
            Arc::clone(&host),
            stage_root.to_path_buf(),
            events.clone(),
        );
    }
    drop(events);
    await_stages(&received, &session, output, error_output)
}

/// One native target's admitted resume state, moved onto the thread that
/// stages it.
struct NativeStage {
    target: NativeTarget,
    attach: Option<Attach>,
    workflow: TargetWorkflow,
    recorder: TargetRecorder,
}

impl NativeStage {
    /// Stage this target on its own detached thread, sending its lines and then
    /// its outcome to `events`. When the coordinator stops waiting after a
    /// sibling fails, the thread ends with the process; the session already
    /// binds its provider run for resume.
    fn spawn(
        self,
        inputs: Arc<ReleaseInputs>,
        host: Arc<CrossHost>,
        stage_root: PathBuf,
        events: mpsc::Sender<StageEvent>,
    ) {
        thread::spawn(move || {
            let started = SystemTime::now();
            let mut output = StageLines::new(self.target, Stream::Output, events.clone());
            let mut error_output = StageLines::new(self.target, Stream::Error, events.clone());
            let prepared = PreparedBundles::new(host.ambient.clone(), inputs.tree().to_path_buf());
            let control = resumable_control(&self.recorder, &self.workflow, &prepared);
            let result = match self.target {
                NativeTarget::Linux => linux_hosted_stage_controlled(
                    &inputs,
                    self.attach,
                    &stage_root,
                    control,
                    &mut output,
                ),
                NativeTarget::Macos => macos_hosted_stage_controlled(
                    &inputs,
                    self.attach,
                    &stage_root,
                    control,
                    &mut output,
                ),
                NativeTarget::Windows => windows_hosted_stage_controlled(
                    &inputs,
                    &host,
                    self.attach,
                    &stage_root,
                    control,
                    &mut output,
                    &mut error_output,
                )
                .and_then(|execution| {
                    (execution.exit_code() == 0).then_some(()).ok_or_else(|| {
                        WindowsHostedError::ProviderResponse(format!(
                            "hosted Windows staging exited {}",
                            execution.exit_code()
                        ))
                    })
                }),
            };
            drop((output, error_output));
            // A coordinator that stopped waiting no longer receives.
            let _ = events.send(StageEvent::Finished(PlatformOutcome {
                target: self.target,
                started,
                finished: SystemTime::now(),
                result,
            }));
        });
    }
}

/// Settle each native stage as it finishes and write its lines as they arrive.
/// Whenever no line arrives for [`PROGRESS_INTERVAL`], name the stages still
/// running. Stop waiting once any stage fails.
fn await_stages(
    received: &mpsc::Receiver<StageEvent>,
    session: &ReleaseSession,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<Vec<TargetSpan>, HostedReleaseError> {
    let started = Instant::now();
    let mut running = NATIVE_TARGETS.to_vec();
    let mut failures = Vec::new();
    let mut spans = Vec::with_capacity(NATIVE_TARGETS.len());
    while !running.is_empty() && failures.is_empty() {
        match received.recv_timeout(PROGRESS_INTERVAL) {
            Ok(StageEvent::Line {
                target,
                stream,
                line,
            }) => {
                let sink: &mut dyn Write = match stream {
                    Stream::Output => &mut *output,
                    Stream::Error => &mut *error_output,
                };
                write!(sink, "[{}] ", target.name())
                    .and_then(|()| sink.write_all(&line))
                    .map_err(WindowsHostedError::Output)?;
            }
            Ok(StageEvent::Finished(outcome)) => {
                running.retain(|target| *target != outcome.target);
                spans.extend(settle(
                    outcome,
                    session,
                    &mut failures,
                    output,
                    error_output,
                )?);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => writeln!(
                output,
                "hosted release still staging {} after {}s",
                target_names(&running),
                started.elapsed().as_secs()
            )
            .map_err(WindowsHostedError::Output)?,
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!(
                "hosted {} stage ended without an outcome",
                target_names(&running)
            ),
        }
    }
    if failures.is_empty() {
        return Ok(spans);
    }
    if !running.is_empty() {
        writeln!(
            error_output,
            "hosted release stops waiting on {} after a failed stage; the session keeps each bound provider run for resume",
            target_names(&running)
        )
        .map_err(WindowsHostedError::Output)?;
    }
    Err(HostedReleaseError::Platform(failures))
}

fn target_names(targets: &[NativeTarget]) -> String {
    targets
        .iter()
        .map(|target| target.name())
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Clone, Copy)]
enum Stream {
    Output,
    Error,
}

enum StageEvent {
    Line {
        target: NativeTarget,
        stream: Stream,
        line: Vec<u8>,
    },
    Finished(PlatformOutcome),
}

/// Forwards each complete line one native stage writes to the coordinator as
/// soon as it is written.
struct StageLines {
    target: NativeTarget,
    stream: Stream,
    pending: Vec<u8>,
    events: mpsc::Sender<StageEvent>,
}

impl StageLines {
    fn new(target: NativeTarget, stream: Stream, events: mpsc::Sender<StageEvent>) -> Self {
        Self {
            target,
            stream,
            pending: Vec::new(),
            events,
        }
    }

    fn send(&self, line: Vec<u8>) {
        // A coordinator that stopped waiting no longer receives.
        let _ = self.events.send(StageEvent::Line {
            target: self.target,
            stream: self.stream,
            line,
        });
    }

    /// Send an unterminated trailing line, terminated.
    fn finish_line(&mut self) {
        if !self.pending.is_empty() {
            let mut line = std::mem::take(&mut self.pending);
            line.push(b'\n');
            self.send(line);
        }
    }
}

impl Write for StageLines {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line = self.pending.drain(..=end).collect();
            self.send(line);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.finish_line();
        Ok(())
    }
}

impl Drop for StageLines {
    fn drop(&mut self) {
        self.finish_line();
    }
}

/// Record one finished target in the session, returning its span when it
/// succeeded and appending each failure to `failures`.
fn settle(
    outcome: PlatformOutcome,
    session: &ReleaseSession,
    failures: &mut Vec<String>,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<Option<TargetSpan>, WindowsHostedError> {
    match outcome.result {
        Ok(()) => {
            if let Err(error) = session.succeeded(outcome.target) {
                failures.push(format!("{} session record: {error}", outcome.target.name()));
            }
            let run = match session.attachment(outcome.target) {
                Ok(Some(Attach::Run(run))) => Some(run),
                Ok(Some(Attach::Sole) | None) | Err(_) => None,
            };
            writeln!(output, "hosted {} stage succeeded", outcome.target.name())
                .map_err(WindowsHostedError::Output)?;
            Ok(Some(TargetSpan {
                target: outcome.target,
                started: outcome.started,
                finished: outcome.finished,
                run,
            }))
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
            Ok(None)
        }
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
    prepared: &'a PreparedBundles,
) -> HostedRunControl<'a> {
    let workflow = match workflow {
        TargetWorkflow::Source => HostedWorkflow::Source,
        TargetWorkflow::Current => HostedWorkflow::Current,
        TargetWorkflow::Exact(blob) => HostedWorkflow::Exact(blob),
    };
    HostedRunControl::resumable(observer, workflow).with_prepared_bundles(prepared)
}

/// Stage the signed macOS release target on one fresh provider-hosted machine,
/// or re-enter the provider run `attach` names, and publish its
/// provider-attested files into `dist/stage`.
pub fn macos_hosted_stage(
    inputs: &ReleaseInputs,
    host: &CrossHost,
    attach: Option<Attach>,
    output: &mut impl Write,
) -> Result<(), WindowsHostedError> {
    require_prepared_read(&host.ambient, inputs.tree())?;
    let prepared = PreparedBundles::new(host.ambient.clone(), inputs.tree().to_path_buf());
    macos_hosted_stage_controlled(
        inputs,
        attach,
        Path::new("dist/stage"),
        HostedRunControl::default().with_prepared_bundles(&prepared),
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
    require_prepared_read(&host.ambient, inputs.tree())?;
    let prepared = PreparedBundles::new(host.ambient.clone(), inputs.tree().to_path_buf());
    windows_hosted_stage_controlled(
        inputs,
        host,
        attach,
        Path::new("dist/stage"),
        HostedRunControl::default().with_prepared_bundles(&prepared),
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
    started: SystemTime,
    finished: SystemTime,
    result: Result<(), WindowsHostedError>,
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
