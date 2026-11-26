//! Disposable browser checks against this worktree's current app sources.

mod scripted_omp;

use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;

#[cfg(unix)]
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
#[cfg(unix)]
use signal_hook::iterator::Signals;
use wire::{DemoAnswer, Digest, Outcome};

/// Optional native OMP proof driven beside the disposable app.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppDemoMode {
    /// Preserve the ordinary browser demo.
    Browser,
    /// Seat one real OMP lane driven by the built-in scripted sequence.
    ScriptedOmp,
}

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

/// Stage the current worktree's app sources, build its native Spice, and run
/// an isolated demo serving exactly the generation those sources produced.
///
/// # Errors
///
/// Refuses an app build, source, or stage failure, a staged generation that
/// moved while seeding, a native build or demo seed failure, or a resident
/// that cannot launch or exit successfully.
pub fn app_demo(
    environment: &crate::ReleaseEnvironment,
    open: bool,
    mode: AppDemoMode,
    output: &mut impl Write,
) -> Result<(), AppDemoError> {
    let start =
        std::env::current_dir().map_err(|error| fail(format!("current directory: {error}")))?;
    let repository = spice::repository::Repository::discover(&start)
        .map_err(|error| fail(format!("active worktree unavailable: {error}")))?;
    app_demo_at(repository.worktree_root(), environment, open, mode, output)
}

fn app_demo_at(
    root: &Path,
    environment: &crate::ReleaseEnvironment,
    open: bool,
    mode: AppDemoMode,
    output: &mut impl Write,
) -> Result<(), AppDemoError> {
    let staged = crate::app::app_from(root, environment, output)
        .map_err(|error| fail(format!("current app sources did not stage: {error}")))?;
    let executable = build_spice(root, output)?;
    let demo = DemoRoot::create()?;

    let result = (|| {
        seed_demo(&executable, root, demo.path())?;
        let mut harness = match &mode {
            AppDemoMode::Browser => None,
            AppDemoMode::ScriptedOmp => Some(scripted_omp::Harness::prepare(demo.path())?),
        };
        let served = matching_generation(demo.path(), &staged.generation)?;
        writeln!(
            output,
            "[app-demo] worktree={} sources={} staged_generation={}",
            root.display(),
            staged.sources,
            staged.generation,
        )
        .map_err(|error| fail(format!("app-demo output failed: {error}")))?;
        writeln!(
            output,
            "[app-demo] root={} served_generation={served}",
            demo.path().display()
        )
        .map_err(|error| fail(format!("app-demo output failed: {error}")))?;
        output
            .flush()
            .map_err(|error| fail(format!("app-demo output failed: {error}")))?;

        run_demo(&executable, demo.path(), open, harness.as_mut(), output)
    })();
    finish_demo(
        demo,
        result,
        matches!(mode, AppDemoMode::ScriptedOmp),
        output,
    )
}

fn staged_generation(root: &Path) -> Result<Digest, AppDemoError> {
    let source = spice::serve::AppSource::resolve(root)
        .map_err(|error| fail(format!("worktree staged app refused: {error}")))?;
    source
        .current_generation()
        .map(|generation| generation.identity().clone())
        .map_err(|error| fail(format!("worktree staged app refused: {error}")))
}

fn matching_generation(root: &Path, expected: &Digest) -> Result<Digest, AppDemoError> {
    let served = staged_generation(root)?;
    if &served == expected {
        Ok(served)
    } else {
        Err(fail(format!(
            "worktree staged generation moved while seeding: expected {expected}, served {served}"
        )))
    }
}

fn build_spice(root: &Path, output: &mut impl Write) -> Result<PathBuf, AppDemoError> {
    writeln!(output, "[app-demo] building current worktree spice")
        .map_err(|error| fail(format!("app-demo output failed: {error}")))?;
    output
        .flush()
        .map_err(|error| fail(format!("app-demo output failed: {error}")))?;
    let built = Command::new("cargo")
        .current_dir(root)
        .args([
            "build",
            "--locked",
            "--package",
            "spice",
            "--bin",
            "spice",
            "--message-format",
            "json-render-diagnostics",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| fail(format!("current spice build did not start: {error}")))?;
    if !built.status.success() {
        return Err(fail(format!(
            "current spice build exited with {}",
            exit_description(built.status.code())
        )));
    }
    built_executable(&String::from_utf8_lossy(&built.stdout))
        .ok_or_else(|| fail("current spice build reported no executable"))
}

fn built_executable(events: &str) -> Option<PathBuf> {
    events
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| {
            event.get("reason").and_then(serde_json::Value::as_str) == Some("compiler-artifact")
                && event
                    .pointer("/target/name")
                    .and_then(serde_json::Value::as_str)
                    == Some("spice")
        })
        .filter_map(|event| event.get("executable")?.as_str().map(PathBuf::from))
        .next_back()
}

fn seed_demo(executable: &Path, source: &Path, root: &Path) -> Result<(), AppDemoError> {
    let seeded = Command::new(executable)
        .current_dir(source)
        .arg("demo")
        .arg("--root")
        .arg(root)
        .args(["--seed-only", "--json"])
        .output()
        .map_err(|error| fail(format!("app-demo seed did not start: {error}")))?;
    child_answer("seed", seeded).map(|_| ())
}

fn run_demo(
    executable: &Path,
    root: &Path,
    open: bool,
    harness: Option<&mut scripted_omp::Harness>,
    output: &mut impl Write,
) -> Result<(), AppDemoError> {
    // The child owns graceful shutdown. Catch the same terminal signals in this
    // waiting parent so its disposable root is removed after the child exits.
    #[cfg(unix)]
    let _signals = Signals::new([SIGHUP, SIGINT, SIGTERM])
        .map_err(|error| fail(format!("app-demo signal handling unavailable: {error}")))?;
    let discovery = demo_discovery(root)?;
    let mut command = Command::new(executable);
    command
        .current_dir(root)
        .arg("demo")
        .arg("--reuse")
        .arg(root)
        .arg("--json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The demo resident and every supervisor it launches must not inherit the
    // launching seat: a lane identity or Git-common wrapper from another
    // repository routes demo lanes' commands to that repository's resident.
    for variable in [
        wire::SpiceVariable::Lane,
        wire::SpiceVariable::Session,
        wire::SpiceVariable::WrapperSpice,
    ] {
        command.env_remove(variable.as_str());
    }
    if let Some(harness) = harness.as_deref() {
        harness.configure(&mut command);
    }
    if open {
        command.arg("--open");
    }
    let mut child = DemoChild::spawn(&mut command)?;
    let announced = match wait_for_endpoint(&mut child, &discovery) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            child.stop();
            return Err(error);
        }
    };
    if let Some(endpoint) = announced.as_deref()
        && let Err(error) = announce_endpoint(endpoint, output)
    {
        child.stop();
        return Err(error);
    }
    if let Some(harness) = harness
        && let Err(error) = harness
            .seat(executable, root)
            .and_then(|()| harness.drive(executable, root, &mut child))
    {
        child.stop();
        return Err(error);
    }
    let completed = child.finish()?;
    report_demo_answer(
        child_answer("resident", completed)?,
        announced.as_deref(),
        output,
    )
}

struct DemoChild {
    child: Child,
    stdout: JoinHandle<io::Result<Vec<u8>>>,
    stderr: JoinHandle<io::Result<Vec<u8>>>,
}

impl DemoChild {
    fn spawn(command: &mut Command) -> Result<Self, AppDemoError> {
        let mut child = command
            .spawn()
            .map_err(|error| fail(format!("app-demo resident did not start: {error}")))?;
        let stdout = child
            .stdout
            .take()
            .expect("the app-demo resident's stdout is piped");
        let stderr = child
            .stderr
            .take()
            .expect("the app-demo resident's stderr is piped");
        Ok(Self {
            child,
            stdout: drain(stdout),
            stderr: drain(stderr),
        })
    }

    fn id(&self) -> u32 {
        self.child.id()
    }

    fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    fn interrupt(&mut self) -> Result<(), AppDemoError> {
        #[cfg(unix)]
        {
            // SAFETY: libc::kill does not dereference memory; the child PID is
            // owned by this handle and SIGINT asks its demo shutdown path to
            // publish the final receipt.
            let result = unsafe { libc::kill(self.child.id().cast_signed(), SIGINT) };
            if result == 0 {
                Ok(())
            } else {
                Err(fail(format!(
                    "app-demo resident interrupt failed: {}",
                    io::Error::last_os_error()
                )))
            }
        }
        #[cfg(not(unix))]
        {
            self.child
                .kill()
                .map_err(|error| fail(format!("app-demo resident stop failed: {error}")))
        }
    }

    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.finish();
    }

    fn finish(mut self) -> Result<Output, AppDemoError> {
        let status = match self.child.wait() {
            Ok(status) => status,
            Err(error) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
                return Err(fail(format!("app-demo resident wait failed: {error}")));
            }
        };
        Ok(Output {
            status,
            stdout: drained("standard output", self.stdout)?,
            stderr: drained("standard error", self.stderr)?,
        })
    }
}

fn drain(mut source: impl Read + Send + 'static) -> JoinHandle<io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        source.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

fn drained(stream: &str, worker: JoinHandle<io::Result<Vec<u8>>>) -> Result<Vec<u8>, AppDemoError> {
    worker
        .join()
        .map_err(|_| fail(format!("app-demo resident {stream} reader panicked")))?
        .map_err(|error| fail(format!("app-demo resident {stream} read failed: {error}")))
}

fn demo_discovery(root: &Path) -> Result<PathBuf, AppDemoError> {
    let repository = spice::repository::Repository::discover(root)
        .map_err(|error| fail(format!("app-demo root unavailable: {error}")))?;
    let state = repository
        .admit_state()
        .map_err(|error| fail(format!("app-demo state unavailable: {error}")))?;
    let discovery = state.serve().to_path_buf();
    let runtime = discovery
        .parent()
        .ok_or_else(|| fail("app-demo discovery has no runtime directory"))?;
    fs::create_dir_all(runtime)
        .map_err(|error| fail(format!("app-demo discovery runtime unavailable: {error}")))?;
    Ok(discovery)
}

/// What wakes the wait for the resident's endpoint.
enum Readiness {
    /// The runtime directory changed: discovery may have been published.
    Changed,
    /// The resident ended.
    Ended,
    Failed(String),
}

/// The endpoint once discovery is published, or `None` once the resident
/// ends without publishing it. Discovery is published from a bound listener,
/// so its publication is the endpoint's readiness.
fn wait_for_endpoint(
    child: &mut DemoChild,
    discovery: &Path,
) -> Result<Option<String>, AppDemoError> {
    let runtime = discovery
        .parent()
        .ok_or_else(|| fail("app-demo discovery has no runtime directory"))?;
    let (wake, wakes) = std::sync::mpsc::channel();
    let exit = sill::ProcessExitObserver::or_ended(child.id()).map_err(|error| {
        fail(format!(
            "app-demo resident exit observer unavailable: {error}"
        ))
    })?;
    let ended = wake.clone();
    std::thread::spawn(move || {
        let _ = ended.send(match exit.wait() {
            Ok(()) => Readiness::Ended,
            Err(error) => Readiness::Failed(format!("resident exit observer: {error}")),
        });
    });
    loop {
        // Armed before the look, so a publication between the look and the
        // wait still wakes it.
        let observer = sill::DirectObserver::arm(runtime)
            .map_err(|error| fail(format!("app-demo discovery observer unavailable: {error}")))?;
        let changed = wake.clone();
        std::thread::spawn(move || {
            let _ = changed.send(match observer.wait() {
                Ok(()) => Readiness::Changed,
                Err(error) => Readiness::Failed(format!("discovery observer: {error}")),
            });
        });
        if let Some(endpoint) = published_endpoint(discovery)? {
            return Ok(Some(endpoint));
        }
        if child
            .try_wait()
            .map_err(|error| fail(format!("app-demo resident status unavailable: {error}")))?
            .is_some()
        {
            return Ok(None);
        }
        match wakes.recv() {
            Ok(Readiness::Changed | Readiness::Ended) => {}
            Ok(Readiness::Failed(detail)) => {
                return Err(fail(format!("app-demo readiness wait failed: {detail}")));
            }
            Err(std::sync::mpsc::RecvError) => {
                return Err(fail("app-demo readiness observers ended without a wake"));
            }
        }
    }
}

fn published_endpoint(discovery: &Path) -> Result<Option<String>, AppDemoError> {
    let bytes = match fs::read(discovery) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(fail(format!(
                "app-demo discovery unavailable at {}: {error}",
                discovery.display()
            )));
        }
    };
    let discovery: spice::serve::Discovery = wire::decode(&bytes)
        .map_err(|error| fail(format!("app-demo discovery invalid: {error}")))?;
    Ok(Some(discovery.endpoint))
}

fn announce_endpoint(endpoint: &str, output: &mut impl Write) -> Result<(), AppDemoError> {
    write_endpoint(endpoint, output)
        .and_then(|()| output.flush())
        .map_err(|error| fail(format!("app-demo output failed: {error}")))
}

/// The right-to-left layout surface, then the endpoint whose line marks
/// readiness, so a reader waiting on readiness already holds both.
fn write_endpoint(endpoint: &str, output: &mut impl Write) -> io::Result<()> {
    writeln!(
        output,
        "[app-demo] rtl={endpoint}{}",
        spice::serve::canvas::RTL_SHELL_ROUTE
    )?;
    writeln!(output, "[app-demo] endpoint={endpoint}")
}

fn report_demo_answer(
    answer: DemoAnswer,
    announced: Option<&str>,
    output: &mut impl Write,
) -> Result<(), AppDemoError> {
    let pending = match (announced, answer.endpoint.as_deref()) {
        (Some(announced), Some(completed)) if announced == completed => None,
        (Some(announced), Some(completed)) => {
            return Err(fail(format!(
                "app-demo endpoint moved after readiness: announced {announced}, completed {completed}"
            )));
        }
        (Some(announced), None) => {
            return Err(fail(format!(
                "app-demo endpoint disappeared after readiness: {announced}"
            )));
        }
        (None, endpoint) => endpoint,
    };
    writeln!(
        output,
        "[app-demo] transcript={} mode={}",
        answer.transcript, answer.mode
    )
    .map_err(|error| fail(format!("app-demo output failed: {error}")))?;
    if let Some(endpoint) = pending {
        write_endpoint(endpoint, output)
            .map_err(|error| fail(format!("app-demo output failed: {error}")))?;
    }
    output
        .flush()
        .map_err(|error| fail(format!("app-demo output failed: {error}")))
}

fn child_answer(stage: &str, completed: Output) -> Result<DemoAnswer, AppDemoError> {
    let primary = if completed.status.success() {
        &completed.stdout
    } else {
        &completed.stderr
    };
    let fallback = if completed.status.success() {
        &completed.stderr
    } else {
        &completed.stdout
    };
    let document = if primary.iter().all(u8::is_ascii_whitespace) {
        fallback
    } else {
        primary
    };
    let outcome = wire::decode(document)
        .map_err(|error| fail(format!("app-demo {stage} returned invalid JSON: {error}")))?;
    let answer = outcome_answer(stage, outcome)?;
    if completed.status.success() {
        Ok(answer)
    } else {
        Err(fail(format!(
            "app-demo {stage} exited with {}",
            exit_description(completed.status.code())
        )))
    }
}

fn outcome_answer(stage: &str, outcome: Outcome<DemoAnswer>) -> Result<DemoAnswer, AppDemoError> {
    match outcome {
        Outcome::Reply(reply) => Ok(reply.body),
        Outcome::Refusal(refusal) => Err(fail(format!(
            "app-demo {stage} refused: {}",
            refusal.reason()
        ))),
        Outcome::Fault(fault) => Err(fail(format!("app-demo {stage} failed: {}", fault.reason))),
    }
}

fn exit_description(code: Option<i32>) -> String {
    code.map_or_else(|| "a signal".to_owned(), |code| code.to_string())
}

fn finish_demo(
    mut demo: DemoRoot,
    result: Result<(), AppDemoError>,
    preserve_failure: bool,
    output: &mut impl Write,
) -> Result<(), AppDemoError> {
    if preserve_failure && result.is_err() {
        let path = demo.preserve();
        let reported = writeln!(
            output,
            "[app-demo] scripted OMP recovery root retained: {}",
            path.display()
        )
        .map_err(|error| fail(format!("app-demo final report failed: {error}")));
        return combine_results(result, reported);
    }
    let path = demo.path().to_path_buf();
    match demo.remove() {
        Ok(()) => {
            let reported = writeln!(
                output,
                "[app-demo] disposable root removed: {}",
                path.display()
            )
            .map_err(|error| fail(format!("app-demo final report failed: {error}")));
            combine_results(result, reported)
        }
        Err(cleanup) => {
            let reported = writeln!(
                output,
                "[app-demo] disposable root left at {}: {cleanup}",
                path.display()
            )
            .map_err(|error| fail(format!("app-demo final report failed: {error}")));
            let failure = match result {
                Ok(()) => cleanup,
                Err(child) => fail(format!("{cleanup}; child result: {child}")),
            };
            match reported {
                Ok(()) => Err(failure),
                Err(report) => Err(fail(format!("{failure}; {report}"))),
            }
        }
    }
}

fn combine_results(
    primary: Result<(), AppDemoError>,
    report: Result<(), AppDemoError>,
) -> Result<(), AppDemoError> {
    match (primary, report) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(report)) => Err(fail(format!("{primary}; {report}"))),
    }
}

struct DemoRoot(PathBuf);

impl DemoRoot {
    fn create() -> Result<Self, AppDemoError> {
        loop {
            let path = std::env::temp_dir().join(format!(
                "spice-app-demo-{}-{}",
                std::process::id(),
                NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(fail(format!("{}: {error}", path.display()))),
            }
        }
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn preserve(&mut self) -> PathBuf {
        std::mem::take(&mut self.0)
    }
    fn remove(mut self) -> Result<(), AppDemoError> {
        let path = std::mem::take(&mut self.0);
        match fs::remove_dir_all(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(fail(format!(
                "app-demo cleanup {}: {error}",
                path.display()
            ))),
        }
    }
}

impl Drop for DemoRoot {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

#[derive(Debug)]
pub struct AppDemoError(String);

fn fail(message: impl Into<String>) -> AppDemoError {
    AppDemoError(message.into())
}

impl fmt::Display for AppDemoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for AppDemoError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(root: &Path, identity: &str, omitted: Option<&str>) {
        let generation = root
            .join(spice::serve::APP_STAGE)
            .join(spice::serve::APP_STAGE_GENERATIONS)
            .join(identity);
        fs::create_dir_all(&generation).unwrap();
        for asset in spice::serve::APP.assets() {
            if omitted != Some(asset.file()) {
                fs::write(generation.join(asset.file()), asset.file()).unwrap();
            }
        }
        fs::write(
            root.join(spice::serve::APP_STAGE)
                .join(spice::serve::APP_STAGE_CURRENT),
            format!("{identity}\n"),
        )
        .unwrap();
    }

    #[test]
    fn prepares_runtime_before_observing_discovery() {
        let root = DemoRoot::create().unwrap();
        fs::create_dir(root.path().join(".git")).unwrap();

        let discovery = demo_discovery(root.path()).unwrap();

        assert!(discovery.parent().unwrap().is_dir());
    }

    #[test]
    fn selects_only_a_complete_worktree_stage() {
        let source = DemoRoot::create().unwrap();
        let identity = "5555555555555555555555555555555555555555555555555555555555555555";
        stage(source.path(), identity, None);
        assert_eq!(staged_generation(source.path()).unwrap().as_str(), identity);

        let incomplete = DemoRoot::create().unwrap();
        stage(
            incomplete.path(),
            identity,
            Some(wire::manifest::APP_MODULE),
        );
        assert!(staged_generation(incomplete.path()).is_err());
        let missing = DemoRoot::create().unwrap();
        assert!(staged_generation(missing.path()).is_err());
    }

    #[test]
    fn refuses_a_seeded_generation_that_does_not_match_the_worktree() {
        let seeded = DemoRoot::create().unwrap();
        let expected = "7777777777777777777777777777777777777777777777777777777777777777"
            .parse()
            .unwrap();
        let served = "8888888888888888888888888888888888888888888888888888888888888888";
        stage(seeded.path(), served, None);

        let error = matching_generation(seeded.path(), &expected).unwrap_err();
        assert!(error.to_string().contains(served));
    }

    #[test]
    fn disposable_receipt_omits_commands_for_the_root_it_removes() {
        let answer = DemoAnswer {
            root: "/tmp/disposable-demo".to_owned(),
            transcript: "/tmp/disposable-demo/transcript.jsonl".to_owned(),
            mode: "app".to_owned(),
            lifecycle: "interrupted-preserved".to_owned(),
            endpoint: Some("http://127.0.0.1:43123".to_owned()),
            launch_command: "spice demo --reuse /tmp/disposable-demo".to_owned(),
            remove_command: "rm -rf -- /tmp/disposable-demo".to_owned(),
        };
        let mut output = Vec::new();

        report_demo_answer(answer, None, &mut output).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("http://127.0.0.1:43123"));
        assert!(!output.contains("preserved"));
        for command_fragment in ["launch_command", "remove_command", "--reuse", "rm -rf"] {
            assert!(!output.contains(command_fragment), "{output}");
        }
    }

    #[derive(Default)]
    struct FlushCapture {
        bytes: Vec<u8>,
        visible: usize,
        flushes: usize,
    }

    impl Write for FlushCapture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.visible = self.bytes.len();
            self.flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn live_endpoint_is_flushed_once_before_the_final_receipt() {
        let endpoint = "http://127.0.0.1:43123";
        let answer = DemoAnswer {
            root: "/tmp/disposable-demo".to_owned(),
            transcript: "/tmp/disposable-demo/transcript.jsonl".to_owned(),
            mode: "app".to_owned(),
            lifecycle: "interrupted-preserved".to_owned(),
            endpoint: Some(endpoint.to_owned()),
            launch_command: "unused".to_owned(),
            remove_command: "unused".to_owned(),
        };
        let mut output = FlushCapture::default();

        announce_endpoint(endpoint, &mut output).unwrap();
        assert_eq!(
            std::str::from_utf8(&output.bytes[..output.visible]).unwrap(),
            "[app-demo] rtl=http://127.0.0.1:43123/rtl/\n[app-demo] endpoint=http://127.0.0.1:43123\n"
        );
        report_demo_answer(answer, Some(endpoint), &mut output).unwrap();

        let output_text = String::from_utf8(output.bytes).unwrap();
        assert_eq!(output_text.matches("[app-demo] endpoint=").count(), 1);
        assert!(
            output_text.find("[app-demo] endpoint=").unwrap()
                < output_text.find("[app-demo] transcript=").unwrap()
        );
        assert_eq!(output.flushes, 2);
    }

    #[test]
    fn disposable_child_failure_reports_reason_without_preservation_recovery() {
        let error = outcome_answer(
            "resident",
            Outcome::Fault(wire::Fault {
                class: wire::FaultClass::Environment,
                reason: "demo::forced-child-failure".to_owned(),
                detail: concat!(
                    "demo root preserved at /tmp/disposable-demo; ",
                    "retry with spice demo --reuse /tmp/disposable-demo; ",
                    "remove with rm -rf -- /tmp/disposable-demo"
                )
                .to_owned(),
            }),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("demo::forced-child-failure"));
        assert!(!error.contains("--reuse"));
        assert!(!error.contains("rm -rf"));
    }
}
