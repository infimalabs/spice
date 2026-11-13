use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use sill::{
    Argv, Cancellation, Completion, Deadlines, Environment, Output, ProcessGroup, Redaction,
    RestartDisposition, Spec, StdinPolicy, StreamPolicy, WaitPolicy,
};

use super::super::{CARGO_HOME_VARIABLE, Host, IsolationError};

const RUNTIME: Duration = Duration::from_secs(20 * 60);
const DRAIN: Duration = Duration::from_secs(10);
const REAP: Duration = Duration::from_secs(10);
const GRACE: Duration = Duration::from_secs(2);
const OUTPUT: usize = 16 * 1024 * 1024;

pub(super) fn run_vendor(
    worktree: &Path,
    vendor: &Path,
    host: &Host,
) -> Result<toml::Value, IsolationError> {
    let mut entries = host
        .seed_environment()
        .entries()
        .iter()
        .cloned()
        .collect::<BTreeMap<_, _>>();
    entries.insert(
        OsString::from(CARGO_HOME_VARIABLE),
        host.bootstrap_home().as_os_str().to_owned(),
    );
    let environment = Environment::new(entries).map_err(|error| {
        IsolationError::new(
            "cargo-isolation::seed-environment-invalid",
            error.to_string(),
        )
    })?;
    let arguments = [
        OsString::from("vendor"),
        OsString::from("--locked"),
        OsString::from("--versioned-dirs"),
        OsString::from("--manifest-path"),
        worktree.join("Cargo.toml").into_os_string(),
        vendor.as_os_str().to_owned(),
    ];
    let spec = spec(host.cargo(), arguments, worktree, environment)?;
    let running = sill::spawn(spec)
        .map_err(|error| IsolationError::new("cargo-isolation::seed-failed", error.to_string()))?;
    let outcome = running
        .wait()
        .map_err(|error| IsolationError::new("cargo-isolation::seed-failed", error.to_string()))?;
    classify(&outcome)
}

fn spec(
    cargo: &Path,
    arguments: impl IntoIterator<Item = OsString>,
    worktree: &Path,
    environment: Environment,
) -> Result<Spec, IsolationError> {
    let deadlines = Deadlines::new(RUNTIME, DRAIN, REAP)
        .map_err(|error| IsolationError::new("cargo-isolation::seed-invalid", error.to_string()))?;
    let wait = WaitPolicy::new(
        "immutable Cargo directory-source seed",
        Cancellation::Never,
        RestartDisposition::Never,
    )
    .map_err(|error| IsolationError::new("cargo-isolation::seed-invalid", error.to_string()))?;
    let capture = StreamPolicy::Capture { limit: OUTPUT };
    Ok(Spec {
        argv: Argv::new(cargo, arguments).map_err(|error| {
            IsolationError::new("cargo-isolation::seed-invalid", error.to_string())
        })?,
        cwd: worktree.to_path_buf(),
        environment,
        group: ProcessGroup::New,
        deadlines,
        wait,
        output: Output::new(StdinPolicy::Null, capture, capture),
        redaction: Redaction::All,
        grace: GRACE,
    })
}

fn classify(outcome: &sill::WaitOutcome) -> Result<toml::Value, IsolationError> {
    if !outcome.outputs_drained() {
        return Err(IsolationError::new(
            "cargo-isolation::seed-cleanup-failed",
            "cargo vendor output did not drain before its deadline",
        ));
    }
    let stdout = complete_output("stdout", outcome.stdout())?;
    let stderr = complete_output("stderr", outcome.stderr())?;
    match outcome.completion() {
        Completion::Exited(status) if status.success() => {
            toml::from_str(std::str::from_utf8(stdout).map_err(|error| {
                IsolationError::new("cargo-isolation::seed-output-invalid", error.to_string())
            })?)
            .map_err(|error| {
                IsolationError::new("cargo-isolation::seed-output-invalid", error.to_string())
            })
        }
        completion => Err(IsolationError::new(
            "cargo-isolation::seed-failed",
            format!(
                "completion={completion:?}; stdout={}; stderr={}",
                String::from_utf8_lossy(stdout),
                String::from_utf8_lossy(stderr),
            ),
        )),
    }
}

fn complete_output<'a>(
    stream: &str,
    output: Option<&'a sill::CapturedOutput>,
) -> Result<&'a [u8], IsolationError> {
    let output = output.ok_or_else(|| {
        IsolationError::new(
            "cargo-isolation::seed-output-invalid",
            format!("cargo vendor {stream} was not captured"),
        )
    })?;
    if output.truncated() || !output.complete() {
        return Err(IsolationError::new(
            "cargo-isolation::seed-output-invalid",
            format!("cargo vendor {stream} exceeded its complete capture bound"),
        ));
    }
    Ok(output.bytes())
}
