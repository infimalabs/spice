//! Provider client for disposable hosted Windows execution.

use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
#[cfg(unix)]
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{
    AdmissionError, GITHUB_PROVIDER, ImagePin, Request, ResultDocument, ToolchainPins, WORKFLOW,
    admit_result,
};
use crate::protocol::{
    Command as HostedCommand, Completion, Limits, MAX_OUTPUT_BYTES, MAX_RUNTIME_SECONDS,
};
use serde::Deserialize;
use wire::{Digest, Domain};

mod linux;
mod source;
pub use linux::{LinuxHostedStage, linux_hosted_stage};
use source::{delete_source, push_source};

const POLICY: &str = ::std::include_str!("../../../dist/windows/hosted.toml");
const REMOTE: &str = "origin";
const WORKFLOW_PATH: &str = ".github/workflows/windows.yml";

/// Complete observed outcome of one provider-authenticated command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostedOutput {
    pub completion: Completion,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Complete observed outcome of one provider-authenticated batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostedExecution {
    pub result: ResultDocument,
    pub outputs: Vec<HostedOutput>,
}

impl HostedExecution {
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        self.outputs[0].completion.exit_code()
    }
}

/// Dispatch one argv with the exact selected worktree to a fresh hosted machine.
pub fn windows_hosted(
    arguments: Vec<OsString>,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<HostedExecution, WindowsHostedError> {
    let (program, command_arguments) = arguments
        .split_first()
        .ok_or(WindowsHostedError::MissingProgram)?;
    let command = HostedCommand {
        program: unicode("program", program)?,
        arguments: command_arguments
            .iter()
            .map(|argument| unicode("argument", argument))
            .collect::<Result<Vec<_>, _>>()?,
    };
    let execution = windows_hosted_commands(vec![command])?;
    let first = &execution.outputs[0];
    output
        .write_all(&first.stdout)
        .map_err(WindowsHostedError::Output)?;
    error_output
        .write_all(&first.stderr)
        .map_err(WindowsHostedError::Output)?;
    Ok(execution)
}

/// Dispatch a bounded argv batch on one fresh hosted machine and signed run.
pub fn windows_hosted_commands(
    commands: Vec<HostedCommand>,
) -> Result<HostedExecution, WindowsHostedError> {
    windows_hosted_commands_into(commands, None, HostedSource::Worktree)
}

/// Which source tree one hosted run builds.
#[derive(Clone, Copy, Debug)]
pub enum HostedSource<'a> {
    /// The exact worktree as it stands, uncommitted changes included.
    Worktree,
    /// The release source archive at this path: the one-use commit's tree is
    /// the archive's tree, and the signed request binds the archive's identity.
    Archive(&'a Path),
}

/// Require the configured repository to be public before a release proof can
/// dispatch work whose authority is a public GitHub Sigstore attestation.
///
/// Ordinary private hosted development remains available through
/// [`windows_hosted_commands`]. Release proof calls this boundary first so a
/// private repository cannot be mistaken for an attestation-capable one.
pub fn require_public_attestation_repository() -> Result<(), WindowsHostedError> {
    let policy: Policy = toml::from_str(POLICY).map_err(WindowsHostedError::Policy)?;
    policy.admit()?;
    let root = workspace_root()?;
    let visibility = successful_text(
        "repository visibility",
        command(
            &root,
            "gh",
            [
                OsString::from("api"),
                OsString::from(format!("repos/{}", policy.repository)),
                OsString::from("--jq"),
                OsString::from(".visibility"),
            ],
        )?,
    )?;
    admit_public_attestation_visibility(&policy.repository, visibility.trim())
}

fn admit_public_attestation_visibility(
    repository: &str,
    visibility: &str,
) -> Result<(), WindowsHostedError> {
    if visibility == "public" {
        Ok(())
    } else {
        Err(WindowsHostedError::PublicAttestationRequired {
            repository: repository.to_owned(),
            visibility: visibility.to_owned(),
        })
    }
}

/// Dispatch a hosted batch and publish its signed artifact files when requested.
pub fn windows_hosted_commands_into(
    commands: Vec<HostedCommand>,
    destination: Option<&Path>,
    source: HostedSource<'_>,
) -> Result<HostedExecution, WindowsHostedError> {
    let policy: Policy = toml::from_str(POLICY).map_err(WindowsHostedError::Policy)?;
    policy.admit()?;
    let root = workspace_root()?;
    let destination = destination.map(|path| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        }
    });
    let scratch = Scratch::new()?;
    let challenge = challenge()?;
    let source_ref = format!("spice-hosted-{}", &challenge[..24]);
    let source = source::prepare(&root, scratch.path(), &challenge, source)?;
    let request = Request {
        version: super::PROTOCOL_VERSION,
        challenge,
        provider: policy.provider,
        repository: policy.repository.clone(),
        source_ref: source_ref.clone(),
        source: source.archive.to_string(),
        tree: source.tree,
        image: policy.image,
        toolchain: policy.toolchain,
        limits: Limits {
            runtime_seconds: MAX_RUNTIME_SECONDS,
            output_bytes: MAX_OUTPUT_BYTES,
        },
        commands,
    };
    request.admit().map_err(WindowsHostedError::Admission)?;

    let cancellation = Cancellation::install()?;
    let run = HostedRun {
        root: &root,
        scratch: scratch.path(),
        repository: &policy.repository,
        source_ref: &source_ref,
        source_commit: &source.commit,
    };
    let request_json = serde_json::to_string(&request).map_err(WindowsHostedError::Encode)?;
    with_pushed_source(&run, &cancellation, || {
        dispatch(
            &run,
            WORKFLOW_PATH,
            &serde_json::json!({ "request": request_json }),
            |run_id| {
                complete_dispatched_run(
                    &run,
                    &request,
                    run_id,
                    destination.as_deref(),
                    &cancellation,
                )
            },
        )
    })
}

/// One hosted run: the workspace it is driven from and the pushed source
/// revision the provider builds.
#[derive(Clone, Copy)]
struct HostedRun<'a> {
    root: &'a Path,
    scratch: &'a Path,
    repository: &'a str,
    source_ref: &'a str,
    source_commit: &'a str,
}

/// Push the run's one-use source ref, run `operation` unless cancellation
/// already arrived, and delete the ref whatever happened.
fn with_pushed_source<T>(
    run: &HostedRun<'_>,
    cancellation: &Cancellation,
    operation: impl FnOnce() -> Result<T, WindowsHostedError>,
) -> Result<T, WindowsHostedError> {
    let operation = push_source(run.root, run.source_commit, run.source_ref).and_then(|()| {
        if cancellation.requested() {
            Err(WindowsHostedError::Cancelled { run_id: None })
        } else {
            operation()
        }
    });
    let cleanup = delete_source(run.root, run.source_ref);
    match (operation, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(error), Err(cleanup)) => Err(WindowsHostedError::OperationAndCleanup {
            operation: Box::new(error),
            cleanup: Box::new(cleanup),
        }),
    }
}

/// Dispatch `workflow` at the run's source ref with `inputs`, complete the
/// provider run through `complete`, cancel it on any other failure, and
/// delete its artifact.
fn dispatch<T>(
    run: &HostedRun<'_>,
    workflow: &str,
    inputs: &serde_json::Value,
    complete: impl FnOnce(u64) -> Result<T, WindowsHostedError>,
) -> Result<T, WindowsHostedError> {
    let HostedRun {
        root,
        repository,
        source_ref,
        ..
    } = *run;
    let dispatched = command_with_input(
        root,
        "gh",
        [
            OsString::from("workflow"),
            OsString::from("run"),
            OsString::from(workflow),
            OsString::from("--repo"),
            OsString::from(repository),
            OsString::from("--ref"),
            OsString::from(source_ref),
            OsString::from("--json"),
        ],
        serde_json::to_vec(inputs).map_err(WindowsHostedError::Encode)?,
    )?;
    let url = successful_text("workflow dispatch", dispatched)?;
    let run_id = url
        .trim()
        .rsplit('/')
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| {
            WindowsHostedError::ProviderResponse(format!("dispatch returned no run URL: {url:?}"))
        })?;

    let operation = complete(run_id);
    let operation = match operation {
        Err(error @ WindowsHostedError::Cancelled { .. }) => Err(error),
        Err(error) => finish_run_cleanup(Err(error), cancel_run(root, repository, run_id)),
        success => success,
    };
    let cleanup = delete_artifact(root, repository, run_id);
    finish_artifact_cleanup(operation, cleanup)
}

fn complete_dispatched_run(
    run: &HostedRun<'_>,
    request: &Request,
    run_id: u64,
    destination: Option<&Path>,
    cancellation: &Cancellation,
) -> Result<HostedExecution, WindowsHostedError> {
    watch_run(run.root, run.repository, run_id, cancellation)?;
    let (artifact_dir, runner_path, result_path) =
        download_attested_artifact(run, &request.challenge, run_id)?;
    let runner = digest_file(Domain::ProofNativeRunner, &runner_path)?;
    let result_bytes = fs::read(&result_path).map_err(|source| WindowsHostedError::Io {
        path: result_path.clone(),
        source,
    })?;
    let result: ResultDocument =
        serde_json::from_slice(&result_bytes).map_err(WindowsHostedError::Decode)?;
    let identity =
        admit_result(request, &result, &runner).map_err(WindowsHostedError::Admission)?;
    if identity.run_id != run_id {
        return Err(WindowsHostedError::RunIdentity {
            dispatched: run_id,
            signed: identity.run_id,
        });
    }
    let outputs = result
        .outputs
        .iter()
        .map(|output| {
            Ok(HostedOutput {
                completion: output.completion.clone(),
                stdout: output
                    .stdout
                    .bytes()
                    .map_err(|_| WindowsHostedError::Admission(AdmissionError::Capture))?,
                stderr: output
                    .stderr
                    .bytes()
                    .map_err(|_| WindowsHostedError::Admission(AdmissionError::Capture))?,
            })
        })
        .collect::<Result<Vec<_>, WindowsHostedError>>()?;
    if let Some(destination) = destination {
        if outputs
            .iter()
            .all(|output| matches!(output.completion, Completion::Exited { code: 0 }))
        {
            publish_artifacts(
                &artifact_dir.join(format!("spice-hosted-stage-{}", request.challenge)),
                &result.artifacts,
                destination,
                &request.source,
            )?;
        }
    } else if !result.artifacts.is_empty() {
        return Err(WindowsHostedError::ProviderResponse(
            "hosted execution returned unrequested artifacts".to_owned(),
        ));
    }
    Ok(HostedExecution { result, outputs })
}

fn finish_artifact_cleanup<T>(
    operation: Result<T, WindowsHostedError>,
    cleanup: Result<(), WindowsHostedError>,
) -> Result<T, WindowsHostedError> {
    match (operation, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(operation), Err(cleanup)) => Err(WindowsHostedError::ArtifactOperationAndCleanup {
            operation: Box::new(operation),
            cleanup: Box::new(cleanup),
        }),
    }
}

fn finish_run_cleanup<T>(
    operation: Result<T, WindowsHostedError>,
    cleanup: Result<(), WindowsHostedError>,
) -> Result<T, WindowsHostedError> {
    match (operation, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(operation), Err(cleanup)) => Err(WindowsHostedError::RunOperationAndCleanup {
            operation: Box::new(operation),
            cleanup: Box::new(cleanup),
        }),
    }
}

fn download_attested_artifact(
    run: &HostedRun<'_>,
    challenge: &str,
    run_id: u64,
) -> Result<(PathBuf, PathBuf, PathBuf), WindowsHostedError> {
    let artifact_dir = download_artifact(run, run_id, &format!("spice-hosted-{challenge}"))?;
    let result_path = artifact_dir.join("result.json");
    let runner_path = artifact_dir.join("bootstrap-target/release/windows-hosted.exe");
    for path in [&runner_path, &result_path] {
        verify_attestation(run, WORKFLOW, path)?;
    }
    Ok((artifact_dir, runner_path, result_path))
}

/// Download the run's one artifact `name` into a fresh scratch directory.
fn download_artifact(
    run: &HostedRun<'_>,
    run_id: u64,
    name: &str,
) -> Result<PathBuf, WindowsHostedError> {
    let artifact_dir = run.scratch.join("artifact");
    fs::create_dir(&artifact_dir).map_err(|source| WindowsHostedError::Io {
        path: artifact_dir.clone(),
        source,
    })?;
    successful(
        "artifact download",
        command(
            run.root,
            "gh",
            [
                OsString::from("run"),
                OsString::from("download"),
                OsString::from(run_id.to_string()),
                OsString::from("--repo"),
                OsString::from(run.repository),
                OsString::from("--name"),
                OsString::from(name),
                OsString::from("--dir"),
                artifact_dir.as_os_str().to_owned(),
            ],
        )?,
    )?;
    Ok(artifact_dir)
}

/// Require a GitHub Sigstore attestation over the file at `path` signed by
/// `workflow` running at the run's one-use source commit and tag on a
/// provider-hosted runner.
fn verify_attestation(
    run: &HostedRun<'_>,
    workflow: &str,
    path: &Path,
) -> Result<(), WindowsHostedError> {
    let HostedRun {
        root,
        repository,
        source_ref,
        source_commit,
        ..
    } = *run;
    let verification = command(
        root,
        "gh",
        [
            OsString::from("attestation"),
            OsString::from("verify"),
            path.as_os_str().to_owned(),
            OsString::from("--repo"),
            OsString::from(repository),
            OsString::from("--signer-workflow"),
            OsString::from(format!("{repository}/{workflow}")),
            OsString::from("--signer-digest"),
            OsString::from(source_commit),
            OsString::from("--source-ref"),
            OsString::from(format!("refs/tags/{source_ref}")),
            OsString::from("--source-digest"),
            OsString::from(source_commit),
            OsString::from("--deny-self-hosted-runners"),
            OsString::from("--format=json"),
        ],
    )?;
    successful("artifact attestation", verification).map(|_| ())
}

fn publish_artifacts(
    source: &Path,
    artifacts: &[super::Artifact],
    destination: &Path,
    source_archive: &str,
) -> Result<(), WindowsHostedError> {
    for artifact in artifacts {
        let path = source.join(&artifact.path);
        let metadata = fs::metadata(&path).map_err(|source| WindowsHostedError::Io {
            path: path.clone(),
            source,
        })?;
        if !metadata.is_file()
            || metadata.len() != artifact.bytes
            || digest_file(Domain::DistArtifact, &path)? != artifact.digest
        {
            return Err(WindowsHostedError::ProviderResponse(format!(
                "hosted artifact {} differs from its signed inventory",
                artifact.path
            )));
        }
        if artifact.path.ends_with("/payload.json") {
            admit_payload_source(&path, source_archive)?;
        }
    }
    let mut published = artifacts.iter().collect::<Vec<_>>();
    published.sort_by_key(|artifact| artifact.path.ends_with("/payload.json"));
    for artifact in published {
        let source = source.join(&artifact.path);
        let path = destination.join(&artifact.path);
        let parent = path.parent().expect("admitted artifact path has a parent");
        fs::create_dir_all(parent).map_err(|source| WindowsHostedError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
        let staged = path.with_extension(format!("hosted-{}", std::process::id()));
        fs::copy(&source, &staged).map_err(|source| WindowsHostedError::Io {
            path: staged.clone(),
            source,
        })?;
        fs::rename(&staged, &path).map_err(|source| WindowsHostedError::Io {
            path: path.clone(),
            source,
        })?;
    }
    Ok(())
}

/// A staged target payload records the source archive this run was bound to.
fn admit_payload_source(path: &Path, source: &str) -> Result<(), WindowsHostedError> {
    let bytes = fs::read(path).map_err(|source| WindowsHostedError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let payload: wire::manifest::TargetPayload =
        wire::decode(&bytes).map_err(WindowsHostedError::Decode)?;
    if payload.archive.as_str() == source {
        Ok(())
    } else {
        Err(WindowsHostedError::ProviderResponse(format!(
            "hosted target payload records source archive {}, not the requested {source}",
            payload.archive
        )))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArtifactCleanupAction {
    Complete,
    Delete(u64),
    Refuse(usize),
}

fn artifact_cleanup_action(inventory: &ArtifactInventory) -> ArtifactCleanupAction {
    match inventory.artifacts.as_slice() {
        [] => ArtifactCleanupAction::Complete,
        [artifact] => ArtifactCleanupAction::Delete(artifact.id),
        artifacts => ArtifactCleanupAction::Refuse(artifacts.len()),
    }
}

fn delete_artifact(root: &Path, repository: &str, run_id: u64) -> Result<(), WindowsHostedError> {
    let listed = command(
        root,
        "gh",
        [
            OsString::from("api"),
            OsString::from(format!(
                "repos/{repository}/actions/runs/{run_id}/artifacts"
            )),
        ],
    )?;
    let listed = successful_text("artifact inventory", listed)?;
    let inventory: ArtifactInventory =
        serde_json::from_str(&listed).map_err(WindowsHostedError::Decode)?;
    let artifact_id = match artifact_cleanup_action(&inventory) {
        ArtifactCleanupAction::Complete => return Ok(()),
        ArtifactCleanupAction::Delete(artifact_id) => artifact_id,
        ArtifactCleanupAction::Refuse(count) => {
            return Err(WindowsHostedError::ProviderResponse(format!(
                "run {run_id} published {count} artifacts instead of one"
            )));
        }
    };
    successful(
        "artifact deletion",
        command(
            root,
            "gh",
            [
                OsString::from("api"),
                OsString::from("--method"),
                OsString::from("DELETE"),
                OsString::from(format!(
                    "repos/{repository}/actions/artifacts/{artifact_id}"
                )),
            ],
        )?,
    )?;
    Ok(())
}

fn digest_file(domain: Domain, path: &Path) -> Result<Digest, WindowsHostedError> {
    let file = fs::File::open(path).map_err(|source| WindowsHostedError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Digest::compute_reader(domain, file, 512 * 1024 * 1024)
        .map(|(_, digest)| digest)
        .map_err(|source| WindowsHostedError::Io {
            path: path.to_path_buf(),
            source,
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WatchObservation {
    Running,
    Exited { success: bool, code: Option<i32> },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WatchAction {
    Wait,
    Complete,
    Cancel,
    Failed(Option<i32>),
}

fn watch_action(cancel_requested: bool, observation: WatchObservation) -> WatchAction {
    if cancel_requested {
        return WatchAction::Cancel;
    }
    match observation {
        WatchObservation::Running => WatchAction::Wait,
        WatchObservation::Exited { success: true, .. } => WatchAction::Complete,
        WatchObservation::Exited {
            success: false,
            code,
        } => WatchAction::Failed(code),
    }
}

fn watch_run(
    root: &Path,
    repository: &str,
    run_id: u64,
    cancellation: &Cancellation,
) -> Result<(), WindowsHostedError> {
    let launch = |source| WindowsHostedError::Launch {
        program: "gh".to_owned(),
        source,
    };
    let mut child = provider_command(root, "gh")
        .args([
            OsString::from("run"),
            OsString::from("watch"),
            OsString::from(run_id.to_string()),
            OsString::from("--repo"),
            OsString::from(repository),
            OsString::from("--compact"),
            OsString::from("--exit-status"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(launch)?;
    let (events, receiver) = mpsc::channel();
    cancellation.wake(events.clone());
    let exit = sill::ProcessExitObserver::or_ended(child.id()).map_err(launch)?;
    thread::spawn(move || {
        let _ = events.send(match exit.wait() {
            Ok(()) => WatchEvent::Exited,
            Err(error) => WatchEvent::ObserverFailed(error),
        });
    });
    loop {
        let observation = match child.try_wait().map_err(launch)? {
            None => WatchObservation::Running,
            Some(status) => WatchObservation::Exited {
                success: status.success(),
                code: status.code(),
            },
        };
        // Read after observing the watcher, so a signal racing its exit still
        // cancels the run.
        match watch_action(cancellation.requested(), observation) {
            WatchAction::Wait => match receiver.recv() {
                Ok(WatchEvent::Exited) => {}
                #[cfg(unix)]
                Ok(WatchEvent::Cancel) => {}
                Ok(WatchEvent::ObserverFailed(source)) => return Err(launch(source)),
                Err(mpsc::RecvError) => {
                    return Err(launch(io::Error::other(
                        "the watcher's exit observer ended without a wake",
                    )));
                }
            },
            WatchAction::Complete => return Ok(()),
            WatchAction::Cancel => {
                stop_watch(&mut child)?;
                cancel_run(root, repository, run_id)?;
                return Err(WindowsHostedError::Cancelled {
                    run_id: Some(run_id),
                });
            }
            WatchAction::Failed(code) => {
                return Err(WindowsHostedError::Command {
                    operation: "workflow execution",
                    code,
                    stdout: String::new(),
                    stderr: String::new(),
                });
            }
        }
    }
}

fn stop_watch(child: &mut Child) -> Result<(), WindowsHostedError> {
    match child.kill() {
        Ok(()) => {}
        Err(source) if source.kind() == io::ErrorKind::InvalidInput => {}
        Err(source) => {
            return Err(WindowsHostedError::Launch {
                program: "gh".to_owned(),
                source,
            });
        }
    }
    child
        .wait()
        .map(|_| ())
        .map_err(|source| WindowsHostedError::Launch {
            program: "gh".to_owned(),
            source,
        })
}

fn cancel_run(root: &Path, repository: &str, run_id: u64) -> Result<(), WindowsHostedError> {
    let cancelled = command(
        root,
        "gh",
        [
            OsString::from("run"),
            OsString::from("cancel"),
            OsString::from(run_id.to_string()),
            OsString::from("--repo"),
            OsString::from(repository),
        ],
    )?;
    if cancelled.status.success() {
        // The cancellation request is asynchronous. Wait until the provider
        // reaches a terminal state before inventorying artifacts, otherwise a
        // late upload can race a currently empty inventory.
        let _observed = command(
            root,
            "gh",
            [
                OsString::from("run"),
                OsString::from("watch"),
                OsString::from(run_id.to_string()),
                OsString::from("--repo"),
                OsString::from(repository),
                OsString::from("--compact"),
                OsString::from("--exit-status"),
            ],
        )?;
    }

    let status = successful_text(
        "workflow state after cancellation",
        command(
            root,
            "gh",
            [
                OsString::from("run"),
                OsString::from("view"),
                OsString::from(run_id.to_string()),
                OsString::from("--repo"),
                OsString::from(repository),
                OsString::from("--json"),
                OsString::from("status"),
                OsString::from("--jq"),
                OsString::from(".status"),
            ],
        )?,
    )?;
    if status.trim() == "completed" {
        Ok(())
    } else if cancelled.status.success() {
        Err(WindowsHostedError::ProviderResponse(format!(
            "cancelled provider run {run_id} did not reach a terminal state"
        )))
    } else {
        successful("workflow cancellation", cancelled).map(|_| ())
    }
}

/// What ends one wait inside a hosted run.
enum WatchEvent {
    Exited,
    #[cfg(unix)]
    Cancel,
    ObserverFailed(io::Error),
}

struct Cancellation {
    #[cfg(unix)]
    requested: Arc<AtomicBool>,
    #[cfg(unix)]
    wakers: Arc<Mutex<Vec<mpsc::Sender<WatchEvent>>>>,
    #[cfg(unix)]
    signals: signal_hook::iterator::Handle,
}

impl Cancellation {
    fn install() -> Result<Self, WindowsHostedError> {
        #[cfg(unix)]
        {
            use signal_hook::consts::signal::{SIGINT, SIGTERM};

            let requested = Arc::new(AtomicBool::new(false));
            let wakers = Arc::new(Mutex::new(Vec::<mpsc::Sender<WatchEvent>>::new()));
            let mut signals = signal_hook::iterator::Signals::new([SIGINT, SIGTERM])
                .map_err(WindowsHostedError::Signal)?;
            let handle = signals.handle();
            let flag = Arc::clone(&requested);
            let waking = Arc::clone(&wakers);
            thread::Builder::new()
                .name("hosted-cancellation".to_owned())
                .spawn(move || {
                    for _signal in signals.forever() {
                        flag.store(true, Ordering::Release);
                        if let Ok(wakers) = waking.lock() {
                            for waker in wakers.iter() {
                                let _ = waker.send(WatchEvent::Cancel);
                            }
                        }
                    }
                })
                .map_err(WindowsHostedError::Signal)?;
            Ok(Self {
                requested,
                wakers,
                signals: handle,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {})
        }
    }

    fn requested(&self) -> bool {
        #[cfg(unix)]
        {
            self.requested.load(Ordering::Acquire)
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    /// Send `WatchEvent::Cancel` to `events` on every request from now on,
    /// at once when one was already made.
    fn wake(&self, events: mpsc::Sender<WatchEvent>) {
        #[cfg(unix)]
        {
            if let Ok(mut wakers) = self.wakers.lock() {
                wakers.push(events.clone());
            }
            if self.requested() {
                let _ = events.send(WatchEvent::Cancel);
            }
        }
        #[cfg(not(unix))]
        {
            drop(events);
        }
    }
}

#[cfg(unix)]
impl Drop for Cancellation {
    fn drop(&mut self) {
        self.signals.close();
    }
}

fn workspace_root() -> Result<PathBuf, WindowsHostedError> {
    let current = std::env::current_dir().map_err(|source| WindowsHostedError::Io {
        path: PathBuf::from("."),
        source,
    })?;
    let root = successful_text(
        "workspace discovery",
        command(
            &current,
            "git",
            ["rev-parse", "--show-toplevel"].map(OsString::from),
        )?,
    )?;
    Ok(PathBuf::from(root.trim()))
}

fn challenge() -> Result<String, WindowsHostedError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| WindowsHostedError::Random(error.to_string()))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn unicode(kind: &'static str, value: &OsStr) -> Result<String, WindowsHostedError> {
    value
        .to_str()
        .map(str::to_owned)
        .ok_or(WindowsHostedError::NonUnicode(kind))
}

fn command<I>(root: &Path, program: &str, arguments: I) -> Result<Output, WindowsHostedError>
where
    I: IntoIterator<Item = OsString>,
{
    provider_command(root, program)
        .args(arguments)
        .output()
        .map_err(|source| WindowsHostedError::Launch {
            program: program.to_owned(),
            source,
        })
}

/// Starts a provider subprocess in its own process group, so a terminal
/// interrupt reaches only this client, which cancels at its typed
/// checkpoints. Prompts are disabled because a background group cannot read
/// the terminal.
fn provider_command(root: &Path, program: &str) -> Command {
    let mut command = Command::new(program);
    command
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=5",
        )
        .env("GH_PROMPT_DISABLED", "1");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    command
}

fn command_with_input<I>(
    root: &Path,
    program: &str,
    arguments: I,
    input: Vec<u8>,
) -> Result<Output, WindowsHostedError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut child = provider_command(root, program)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| WindowsHostedError::Launch {
            program: program.to_owned(),
            source,
        })?;
    child
        .stdin
        .take()
        .ok_or_else(|| {
            WindowsHostedError::ProviderResponse(format!("{program} stdin is unavailable"))
        })?
        .write_all(&input)
        .map_err(WindowsHostedError::Output)?;
    child
        .wait_with_output()
        .map_err(|source| WindowsHostedError::Launch {
            program: program.to_owned(),
            source,
        })
}

fn successful(operation: &'static str, output: Output) -> Result<Output, WindowsHostedError> {
    if output.status.success() {
        Ok(output)
    } else {
        Err(WindowsHostedError::Command {
            operation,
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn successful_text(operation: &'static str, output: Output) -> Result<String, WindowsHostedError> {
    let output = successful(operation, output)?;
    String::from_utf8(output.stdout).map_err(|_| {
        WindowsHostedError::ProviderResponse(format!("{operation} returned non-UTF-8 output"))
    })
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self, WindowsHostedError> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| WindowsHostedError::Clock(error.to_string()))?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "spice-windows-hosted-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).map_err(|source| WindowsHostedError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.0);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    schema: u16,
    provider: String,
    repository: String,
    workflow: String,
    image: ImagePin,
    toolchain: ToolchainPins,
}

impl Policy {
    fn admit(&self) -> Result<(), WindowsHostedError> {
        if self.schema != 1
            || self.provider != GITHUB_PROVIDER
            || self.workflow != WORKFLOW
            || self.workflow != WORKFLOW_PATH
        {
            return Err(WindowsHostedError::PolicyIdentity);
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct ArtifactInventory {
    artifacts: Vec<Artifact>,
}

#[derive(Deserialize)]
struct Artifact {
    id: u64,
}

/// Why no provider-authenticated execution result could be returned.
#[derive(Debug)]
pub enum WindowsHostedError {
    MissingProgram,
    NonUnicode(&'static str),
    Signal(io::Error),
    Policy(toml::de::Error),
    PolicyIdentity,
    Admission(super::AdmissionError),
    Encode(serde_json::Error),
    Decode(serde_json::Error),
    Random(String),
    Clock(String),
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Launch {
        program: String,
        source: io::Error,
    },
    Command {
        operation: &'static str,
        code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    ProviderResponse(String),
    PublicAttestationRequired {
        repository: String,
        visibility: String,
    },
    RunIdentity {
        dispatched: u64,
        signed: u64,
    },
    Cancelled {
        run_id: Option<u64>,
    },
    Output(io::Error),
    RunOperationAndCleanup {
        operation: Box<WindowsHostedError>,
        cleanup: Box<WindowsHostedError>,
    },
    ArtifactOperationAndCleanup {
        operation: Box<WindowsHostedError>,
        cleanup: Box<WindowsHostedError>,
    },
    OperationAndCleanup {
        operation: Box<WindowsHostedError>,
        cleanup: Box<WindowsHostedError>,
    },
}

impl fmt::Display for WindowsHostedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingProgram => {
                formatter.write_str("windows-hosted requires PROGRAM [ARG...] after --")
            }
            Self::NonUnicode(kind) => write!(formatter, "hosted {kind} is not Unicode"),
            Self::Signal(source) => write!(
                formatter,
                "cannot install hosted cancellation handler: {source}"
            ),
            Self::Policy(source) => {
                write!(formatter, "hosted Windows policy does not decode: {source}")
            }
            Self::PolicyIdentity => formatter
                .write_str("hosted Windows policy names an unsupported provider or workflow"),
            Self::Admission(source) => source.fmt(formatter),
            Self::Encode(source) => write!(formatter, "hosted request does not encode: {source}"),
            Self::Decode(source) => write!(formatter, "hosted result does not decode: {source}"),
            Self::Random(detail) => {
                write!(formatter, "fresh hosted challenge is unavailable: {detail}")
            }
            Self::Clock(detail) => write!(
                formatter,
                "hosted scratch identity is unavailable: {detail}"
            ),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Launch { program, source } => {
                write!(formatter, "cannot launch {program}: {source}")
            }
            Self::Command {
                operation,
                code,
                stdout,
                stderr,
            } => write!(
                formatter,
                "{operation} failed with exit {code:?}: stdout={stdout:?} stderr={stderr:?}"
            ),
            Self::ProviderResponse(detail) => formatter.write_str(detail),
            Self::PublicAttestationRequired {
                repository,
                visibility,
            } => write!(
                formatter,
                "public hosted-attestation prerequisite is unsatisfied: repository {repository} has visibility {visibility:?}"
            ),
            Self::RunIdentity { dispatched, signed } => write!(
                formatter,
                "signed provider run {signed} does not match dispatched run {dispatched}"
            ),
            Self::Cancelled {
                run_id: Some(run_id),
            } => {
                write!(formatter, "hosted provider run {run_id} was cancelled")
            }
            Self::Cancelled { run_id: None } => {
                formatter.write_str("hosted execution was cancelled before provider dispatch")
            }
            Self::Output(source) => write!(formatter, "hosted output failed: {source}"),
            Self::RunOperationAndCleanup { operation, cleanup } => write!(
                formatter,
                "hosted operation failed: {operation}; provider run cleanup also failed: {cleanup}"
            ),
            Self::ArtifactOperationAndCleanup { operation, cleanup } => write!(
                formatter,
                "hosted operation failed: {operation}; result artifact retirement also failed: {cleanup}"
            ),
            Self::OperationAndCleanup { operation, cleanup } => write!(
                formatter,
                "hosted operation failed: {operation}; source retirement also failed: {cleanup}"
            ),
        }
    }
}

impl Error for WindowsHostedError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Policy(source) => Some(source),
            Self::Admission(source) => Some(source),
            Self::Encode(source) | Self::Decode(source) => Some(source),
            Self::Io { source, .. }
            | Self::Launch { source, .. }
            | Self::Output(source)
            | Self::Signal(source) => Some(source),
            Self::OperationAndCleanup { operation, .. }
            | Self::RunOperationAndCleanup { operation, .. }
            | Self::ArtifactOperationAndCleanup { operation, .. } => Some(operation),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Artifact, ArtifactCleanupAction, ArtifactInventory, WatchAction, WatchObservation,
        WindowsHostedError, admit_public_attestation_visibility, artifact_cleanup_action,
        finish_artifact_cleanup, finish_run_cleanup, watch_action,
    };

    #[test]
    fn release_attestation_requires_public_repository_visibility() {
        assert!(admit_public_attestation_visibility("owner/repo", "public").is_ok());
        assert!(matches!(
            admit_public_attestation_visibility("owner/repo", "private"),
            Err(WindowsHostedError::PublicAttestationRequired {
                repository,
                visibility,
            }) if repository == "owner/repo" && visibility == "private"
        ));
    }

    #[test]
    fn cancellation_preempts_every_provider_watch_observation() {
        for observation in [
            WatchObservation::Running,
            WatchObservation::Exited {
                success: true,
                code: Some(0),
            },
            WatchObservation::Exited {
                success: false,
                code: Some(1),
            },
        ] {
            assert_eq!(watch_action(true, observation), WatchAction::Cancel);
        }
    }

    #[test]
    fn provider_watch_outcomes_remain_typed_without_cancellation() {
        assert_eq!(
            watch_action(false, WatchObservation::Running),
            WatchAction::Wait
        );
        assert_eq!(
            watch_action(
                false,
                WatchObservation::Exited {
                    success: true,
                    code: Some(0),
                }
            ),
            WatchAction::Complete
        );
        assert_eq!(
            watch_action(
                false,
                WatchObservation::Exited {
                    success: false,
                    code: Some(23),
                }
            ),
            WatchAction::Failed(Some(23))
        );
    }

    #[test]
    fn artifact_inventory_selects_only_one_exact_artifact() {
        let inventory = |ids: &[u64]| ArtifactInventory {
            artifacts: ids.iter().map(|id| Artifact { id: *id }).collect(),
        };

        assert_eq!(
            artifact_cleanup_action(&inventory(&[])),
            ArtifactCleanupAction::Complete
        );
        assert_eq!(
            artifact_cleanup_action(&inventory(&[42])),
            ArtifactCleanupAction::Delete(42)
        );
        assert_eq!(
            artifact_cleanup_action(&inventory(&[42, 43])),
            ArtifactCleanupAction::Refuse(2)
        );
    }

    #[test]
    fn artifact_cleanup_preserves_operation_and_cleanup_failures() {
        let completed = finish_artifact_cleanup(Ok(7_u8), Ok(()));
        assert_eq!(completed.expect("successful cleanup retains result"), 7);

        let operation_failed = finish_artifact_cleanup::<()>(
            Err(WindowsHostedError::Cancelled { run_id: Some(42) }),
            Ok(()),
        );
        assert!(matches!(
            operation_failed,
            Err(WindowsHostedError::Cancelled { run_id: Some(42) })
        ));

        let cleanup_failed =
            finish_artifact_cleanup(Ok(()), Err(WindowsHostedError::PolicyIdentity));
        assert!(matches!(
            cleanup_failed,
            Err(WindowsHostedError::PolicyIdentity)
        ));

        let both_failed = finish_artifact_cleanup::<()>(
            Err(WindowsHostedError::Cancelled { run_id: Some(42) }),
            Err(WindowsHostedError::PolicyIdentity),
        );
        assert!(matches!(
            both_failed,
            Err(WindowsHostedError::ArtifactOperationAndCleanup {
                operation,
                cleanup,
            }) if matches!(*operation, WindowsHostedError::Cancelled { run_id: Some(42) })
                && matches!(*cleanup, WindowsHostedError::PolicyIdentity)
        ));

        let run_and_cleanup_failed = finish_run_cleanup::<()>(
            Err(WindowsHostedError::Cancelled { run_id: Some(42) }),
            Err(WindowsHostedError::PolicyIdentity),
        );
        assert!(matches!(
            run_and_cleanup_failed,
            Err(WindowsHostedError::RunOperationAndCleanup {
                operation,
                cleanup,
            }) if matches!(*operation, WindowsHostedError::Cancelled { run_id: Some(42) })
                && matches!(*cleanup, WindowsHostedError::PolicyIdentity)
        ));
    }
}
