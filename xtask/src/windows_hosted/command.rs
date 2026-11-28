//! Bounded external commands used by hosted release preparation.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use sill::{
    Argv, Cancellation, Completion, Deadlines, Environment, Output, ProcessGroup, Redaction,
    RestartDisposition, Spec, StdinPolicy, StreamPolicy, WaitPolicy,
};

const OUTPUT_DRAIN: Duration = Duration::from_secs(10);
const REAP: Duration = Duration::from_secs(10);
const GRACE: Duration = Duration::from_secs(5);
const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

/// Complete output from one supervised command.
pub(super) struct CommandOutput {
    stdout: Vec<u8>,
}

impl CommandOutput {
    pub(super) fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    pub(super) fn stdout_text(self, operation: &str) -> Result<String, String> {
        String::from_utf8(self.stdout)
            .map_err(|error| format!("{operation} returned non-UTF-8 output: {error}"))
    }
}

/// Run one external operation under an exact process-group deadline.
pub(super) fn run<I, S>(
    operation: &str,
    runtime: Duration,
    program: impl Into<OsString>,
    arguments: I,
    environment: &Environment,
    cwd: &Path,
    stdin: Option<&[u8]>,
) -> Result<CommandOutput, String>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let spec = Spec {
        argv: Argv::new(program, arguments).map_err(|error| error.to_string())?,
        cwd: cwd.to_path_buf(),
        environment: environment.clone(),
        group: ProcessGroup::New,
        deadlines: Deadlines::new(runtime, OUTPUT_DRAIN, REAP)
            .map_err(|error| error.to_string())?,
        wait: WaitPolicy::new(operation, Cancellation::Never, RestartDisposition::Never)
            .map_err(|error| error.to_string())?,
        output: Output::new(
            if stdin.is_some() {
                StdinPolicy::Pipe
            } else {
                StdinPolicy::Null
            },
            StreamPolicy::CaptureBounded {
                limit: OUTPUT_LIMIT,
            },
            StreamPolicy::CaptureTail {
                limit: OUTPUT_LIMIT,
            },
        ),
        redaction: Redaction::None,
        grace: GRACE,
    };
    let mut running =
        sill::spawn(spec).map_err(|error| format!("could not launch {operation}: {error}"))?;
    if let Some(stdin) = stdin {
        running
            .write_stdin(stdin)
            .map_err(|error| format!("could not supply {operation} input: {error}"))?;
    }
    let outcome = running
        .wait()
        .map_err(|error| format!("could not wait for {operation}: {error}"))?;
    let stdout = outcome
        .stdout()
        .map(|output| output.bytes())
        .unwrap_or_default();
    let stderr = outcome
        .stderr()
        .map(|output| output.bytes())
        .unwrap_or_default();
    match outcome.completion() {
        Completion::Exited(status) if status.success() => {
            if outcome
                .stdout()
                .is_some_and(sill::CapturedOutput::truncated)
            {
                Err(format!(
                    "{operation} output exceeded its {OUTPUT_LIMIT} byte bound"
                ))
            } else {
                Ok(CommandOutput {
                    stdout: stdout.to_vec(),
                })
            }
        }
        Completion::Exited(status) => Err(format!(
            "{operation} refused with {status}: {}",
            String::from_utf8_lossy(stderr).trim()
        )),
        Completion::TimedOut(_) => Err(format!(
            "{operation} exceeded its {} second deadline",
            runtime.as_secs()
        )),
        Completion::Cancelled(_) => Err(format!("{operation} was cancelled")),
    }
}
