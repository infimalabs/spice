use super::*;

use std::ffi::OsString;
use std::process::ExitStatus;

pub(super) fn required_read(
    root: Worktree<'_>,
    args: &[&str],
    action: &str,
) -> Result<String, Error> {
    let output = run(root, args)?;
    if output.success() {
        Ok(output.stdout.trim().to_owned())
    } else {
        Err(Error::fault("git::command-failed", output.detail(action)))
    }
}

pub(super) fn try_read(root: Worktree<'_>, args: &[&str]) -> Result<Option<String>, Error> {
    let output = run(root, args)?;
    Ok(output.success().then(|| output.stdout.trim().to_owned()))
}

pub(super) fn run(root: Worktree<'_>, args: &[&str]) -> Result<GitOutput, Error> {
    run_bounded(root, args, None, &[])
}

pub(super) fn run_with_input(
    root: Worktree<'_>,
    args: &[&str],
    input: &[u8],
) -> Result<GitOutput, Error> {
    run_bounded(root, args, Some(input), &[])
}

pub(super) fn run_with_environment(
    root: Worktree<'_>,
    args: &[&str],
    environment: &[(&str, &str)],
) -> Result<GitOutput, Error> {
    run_bounded(root, args, None, environment)
}

fn run_bounded(
    root: Worktree<'_>,
    args: &[&str],
    input: Option<&[u8]>,
    environment: &[(&str, &str)],
) -> Result<GitOutput, Error> {
    let spec = git_spec(root, args, input.is_some(), environment)?;
    let mut running =
        sill::spawn(spec).map_err(|error| Error::fault("git::launch-failed", error.to_string()))?;
    if let Some(input) = input {
        running
            .write_stdin(input)
            .map_err(|error| Error::fault("git::stdin-failed", error.to_string()))?;
    }
    let outcome = running
        .wait()
        .map_err(|error| Error::fault("git::wait-failed", error.to_string()))?;
    git_output(args, outcome)
}

fn git_spec(
    root: Worktree<'_>,
    args: &[&str],
    piped_input: bool,
    environment: &[(&str, &str)],
) -> Result<sill::Spec, Error> {
    let mut arguments = Vec::with_capacity(args.len() + 2);
    arguments.push(OsString::from("-C"));
    arguments.push(root.as_os_str().to_owned());
    arguments.extend(args.iter().map(OsString::from));

    let mut inherited = root
        .ambient
        .variables()
        .iter()
        .cloned()
        .collect::<BTreeMap<_, _>>();
    inherited.insert(OsString::from("GIT_TERMINAL_PROMPT"), OsString::from("0"));
    inherited.insert(
        OsString::from("GIT_SSH_COMMAND"),
        OsString::from("ssh -o BatchMode=yes -o ConnectTimeout=5"),
    );
    for (key, value) in environment {
        inherited.insert(OsString::from(key), OsString::from(value));
    }

    let capture = sill::StreamPolicy::CaptureBounded {
        limit: OUTPUT_LIMIT,
    };
    Ok(sill::Spec {
        argv: sill::Argv::new("git", arguments).map_err(invalid_spec)?,
        cwd: root.to_path_buf(),
        environment: sill::Environment::new(inherited).map_err(invalid_spec)?,
        group: sill::ProcessGroup::New,
        deadlines: sill::Deadlines::new(
            command_limit(args),
            OUTPUT_DRAIN_LIMIT,
            PROCESS_GROUP_REAP_LIMIT,
        )
        .map_err(invalid_spec)?,
        wait: sill::WaitPolicy::new(
            format!("git {}", args.join(" ")),
            sill::Cancellation::Never,
            sill::RestartDisposition::Never,
        )
        .map_err(invalid_spec)?,
        output: sill::Output::new(
            if piped_input {
                sill::StdinPolicy::Pipe
            } else {
                sill::StdinPolicy::Null
            },
            capture,
            capture,
        ),
        redaction: sill::Redaction::All,
        grace: PROCESS_GROUP_GRACE,
    })
}

fn invalid_spec(error: sill::SpecError) -> Error {
    Error::fault("git::command-invalid", error.to_string())
}

fn command_limit(args: &[&str]) -> Duration {
    if args
        .first()
        .is_some_and(|name| matches!(*name, "fetch" | "push"))
    {
        NETWORK_COMMAND_LIMIT
    } else {
        LOCAL_COMMAND_LIMIT
    }
}

fn git_output(args: &[&str], outcome: sill::WaitOutcome) -> Result<GitOutput, Error> {
    let stdout = captured(outcome.stdout(), "stdout")?;
    let stderr = captured(outcome.stderr(), "stderr")?;
    let total = stdout.len().saturating_add(stderr.len());
    if outcome
        .stdout()
        .is_some_and(sill::CapturedOutput::truncated)
        || outcome
            .stderr()
            .is_some_and(sill::CapturedOutput::truncated)
        || total > OUTPUT_LIMIT
    {
        return Err(Error::fault(
            "git::output-limit",
            format!("git {} exceeded {} bytes", args.join(" "), OUTPUT_LIMIT),
        ));
    }
    if !outcome.outputs_drained() {
        return Err(Error::fault(
            "git::output-drain-timeout",
            format!(
                "git {} output did not close within {} milliseconds",
                args.join(" "),
                OUTPUT_DRAIN_LIMIT.as_millis(),
            ),
        ));
    }
    let status = match outcome.completion() {
        sill::Completion::Exited(status) => *status,
        sill::Completion::TimedOut(_) => {
            return Err(Error::fault(
                "git::command-timeout",
                format!(
                    "git {} exceeded {} seconds",
                    args.join(" "),
                    command_limit(args).as_secs(),
                ),
            ));
        }
        sill::Completion::Cancelled(_) => {
            return Err(Error::fault(
                "git::command-cancelled",
                format!("git {} was cancelled", args.join(" ")),
            ));
        }
    };
    Ok(GitOutput {
        status: exit_code(status),
        stdout,
        stderr,
    })
}

fn captured(output: Option<&sill::CapturedOutput>, name: &str) -> Result<String, Error> {
    let bytes = output.map_or(&[][..], sill::CapturedOutput::bytes);
    String::from_utf8(bytes.to_vec())
        .map_err(|error| Error::fault("git::output-not-utf8", format!("Git {name}: {error}")))
}

fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}
