//! Optional RTK command rewriting with an unconditional native fallback.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::time::{Duration, Instant};

use sill::{
    Argv, Cancellation, Completion, Deadlines, Environment, Output, ProcessGroup, Redaction,
    RestartDisposition, Spec, StdinPolicy, StreamPolicy, WaitPolicy,
};

pub use wire::body::{RtkHealth as Health, RtkHealthState as HealthState};

use crate::ambient::Ambient;

/// Exact configured RTK identity carried into supervised shell commands.
pub const EXECUTABLE_VARIABLE: &str = wire::SpiceVariable::RtkExecutable.as_str();
/// RTK's upstream history database override.
pub const HISTORY_VARIABLE: &str = "RTK_DB_PATH"; // env-policy: allow

const SELECTOR_TIMEOUT: Duration = Duration::from_secs(5);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(10);
const OUTPUT_DRAIN: Duration = Duration::from_millis(250);
const REAP_DEADLINE: Duration = Duration::from_millis(500);
const TERMINATE_GRACE: Duration = Duration::from_millis(100);
const MAX_SELECTOR_OUTPUT: usize = 64 * 1024;
const MAX_DIAGNOSTIC_EXECUTABLE_CHARS: usize = 160;
const MAX_HEALTH_DETAIL_CHARS: usize = 1024;
const EXTENDED_REGEX_OPERATORS: &str = "|+?(){}";

/// Command selected for execution and an optional non-blocking degradation notice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Decision {
    command: Vec<OsString>,
    diagnostic: Option<String>,
}

impl Decision {
    /// Final command argv, rewritten only after a valid RTK answer.
    #[must_use]
    pub fn command(&self) -> &[OsString] {
        &self.command
    }

    /// Why RTK degraded to native execution.
    #[must_use]
    pub fn diagnostic(&self) -> Option<&str> {
        self.diagnostic.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SelectorResult {
    code: i32,
    stdout: String,
    stderr: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum InvocationFailure {
    Launch(String),
    Observation(String),
}

impl InvocationFailure {
    fn detail(self) -> String {
        match self {
            Self::Launch(detail) | Self::Observation(detail) => detail,
        }
    }
}

/// Observe whether the exact configured executable can join supervised commands.
#[must_use]
pub fn probe_health(executable: &OsStr, working: &Path, ambient: &Ambient) -> Health {
    let environment = match ambient.environment() {
        Ok(environment) => environment,
        Err(error) => {
            return health(
                HealthState::ProtocolInvalid,
                None,
                format!("invalid health-probe environment: {error}"),
            );
        }
    };
    probe_health_in_environment(executable, working, &environment)
}

/// Observe RTK launchability under the exact environment a supervised lane will use.
#[must_use]
pub fn probe_health_in_environment(
    executable: &OsStr,
    working: &Path,
    environment: &Environment,
) -> Health {
    let started = Instant::now();
    probe_health_with(executable, |program, arguments, input| {
        let remaining = HEALTH_TIMEOUT.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(InvocationFailure::Observation(
                "RTK health exceeded 10s deadline".to_owned(),
            ));
        }
        run_bounded(
            program,
            arguments,
            working,
            environment.clone(),
            input,
            remaining,
            "RTK health probe",
        )
    })
}

fn probe_health_with<F>(executable: &OsStr, mut invoke: F) -> Health
where
    F: FnMut(&OsStr, &[OsString], Option<&[u8]>) -> Result<SelectorResult, InvocationFailure>,
{
    // Hook enablement is structural, so only launchability is a lane-wide
    // fact; status, output shape, and fidelity are validated for each command.
    let version_result = match invoke(executable, &["--version".into()], None) {
        Ok(result) => result,
        Err(InvocationFailure::Launch(reason)) => {
            return health(HealthState::Missing, None, reason);
        }
        Err(InvocationFailure::Observation(reason)) => {
            return health(
                HealthState::Active,
                None,
                format!(
                    "executable launched; version observation unavailable: {reason}; rewrites are validated per command"
                ),
            );
        }
    };
    let version_output = if version_result.stdout.trim().is_empty() {
        version_result.stderr.trim()
    } else {
        version_result.stdout.trim()
    };
    let version = parse_version(version_output)
        .map(|parts| format!("{}.{}.{}", parts[0], parts[1], parts[2]));
    let detail = version.as_ref().map_or_else(
        || {
            format!(
                "executable launched with exit {}; version unrecognized from {version_output:?}; rewrites are validated per command",
                version_result.code
            )
        },
        |version| {
            format!(
                "executable launched as RTK {version} with exit {}; rewrites are validated per command",
                version_result.code
            )
        },
    );
    health(HealthState::Active, version, detail)
}

fn health(state: HealthState, version: Option<String>, detail: impl Into<String>) -> Health {
    let mut detail = detail.into();
    if detail.chars().count() > MAX_HEALTH_DETAIL_CHARS {
        detail = detail
            .chars()
            .take(MAX_HEALTH_DETAIL_CHARS - 3)
            .collect::<String>();
        detail.push_str("...");
    }
    Health::new(state, version, detail)
}

fn parse_version(output: &str) -> Option<[u32; 3]> {
    let mut words = output.split_whitespace();
    while let Some(word) = words.next() {
        if !word.eq_ignore_ascii_case("rtk") {
            continue;
        }
        let mut parts = words.next()?.split('.');
        let version = [
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
        ];
        return parts.next().is_none().then_some(version);
    }
    None
}

/// Ask the configured RTK executable to rewrite one command under the exact
/// working directory and environment the command will run in.
#[must_use]
pub fn rewrite_in(
    command: &[OsString],
    executable: &OsStr,
    working: &Path,
    environment: &Environment,
) -> Decision {
    rewrite_with(command, executable, |arguments| {
        run_selector_in(executable, arguments, working, environment.clone())
    })
}

fn rewrite_with<F>(command: &[OsString], executable: &OsStr, invoke: F) -> Decision
where
    F: FnOnce(&[String]) -> Result<SelectorResult, String>,
{
    let native = || Decision {
        command: command.to_vec(),
        diagnostic: None,
    };
    let Some(arguments) = selector_arguments(command) else {
        return degraded(command, executable, "command contains non-UTF-8 arguments");
    };
    let result = match invoke(&arguments) {
        Ok(result) => result,
        Err(reason) => return degraded(command, executable, &reason),
    };
    let rewritten = result.stdout.trim();
    if result.code == 1 && rewritten.is_empty() {
        return native();
    }
    if rewritten.is_empty() {
        return degraded(
            command,
            executable,
            &format!("rewrite returned exit {} with empty stdout", result.code),
        );
    }
    if let Err(reason) = rewrite_fidelity(command, rewritten) {
        return degraded(command, executable, &reason);
    }

    let Some(shell_index) = shell_command_index(command) else {
        let mut parsed = match shell_words::split(rewritten) {
            Ok(parsed) if !parsed.is_empty() => parsed,
            Ok(_) => return degraded(command, executable, "rewrite returned empty argv"),
            Err(error) => {
                return degraded(
                    command,
                    executable,
                    &format!("rewrite returned malformed argv: {error}"),
                );
            }
        };
        remap_direct_frontend(&mut parsed, executable);
        return Decision {
            command: parsed.into_iter().map(OsString::from).collect(),
            diagnostic: None,
        };
    };

    let mut selected = command.to_vec();
    selected[shell_index] = remap_shell_frontend(rewritten, executable).into();
    Decision {
        command: selected,
        diagnostic: None,
    }
}

fn rewrite_fidelity(command: &[OsString], rewritten: &str) -> Result<(), String> {
    let arguments = selector_arguments(command)
        .ok_or_else(|| "command contains non-UTF-8 arguments".to_owned())?;
    let written = if arguments.len() == 1 {
        shell_words::split(&arguments[0])
            .map_err(|error| format!("written shell command could not be parsed: {error}"))?
    } else {
        arguments.clone()
    };
    let substituted = shell_words::split(rewritten)
        .map_err(|error| format!("rewrite returned malformed argv: {error}"))?;
    if substituted.is_empty() {
        return Err("rewrite returned empty argv".to_owned());
    }
    let operators = narrowed_regex_operators(&written, &substituted);
    if !operators.is_empty() {
        return Err(format!(
            "rewrite narrowed the written regex dialect for operators {operators:?}"
        ));
    }
    if arguments.len() > 1
        && let Some(index) = split_argument(&arguments, &substituted)
    {
        return Err(format!("rewrite split or unquoted argument {index}"));
    }
    Ok(())
}

fn narrowed_regex_operators(written: &[String], substituted: &[String]) -> String {
    let written_extended = written
        .iter()
        .any(|word| matches!(word.as_str(), "rg" | "egrep") || requests_extended_regex(word));
    let substituted_basic = substituted
        .iter()
        .any(|word| matches!(word.as_str(), "grep" | "fgrep"));
    if !written_extended
        || !substituted_basic
        || substituted.iter().any(|word| requests_extended_regex(word))
    {
        return String::new();
    }
    EXTENDED_REGEX_OPERATORS
        .chars()
        .filter(|operator| {
            written
                .iter()
                .any(|word| !word.starts_with('-') && word.contains(*operator))
        })
        .collect()
}

fn requests_extended_regex(word: &str) -> bool {
    matches!(word, "-E" | "--extended-regexp")
        || (word.starts_with('-') && !word.starts_with("--") && word.contains('E'))
}

fn split_argument(arguments: &[String], substituted: &[String]) -> Option<usize> {
    arguments.iter().enumerate().find_map(|(index, argument)| {
        if substituted.iter().any(|word| word == argument) {
            return None;
        }
        let split_on_whitespace = argument.split_whitespace().count() > 1;
        (split_on_whitespace || split_across_words(argument, substituted)).then_some(index)
    })
}

fn split_across_words(argument: &str, substituted: &[String]) -> bool {
    let target = argument.split_whitespace().collect::<String>();
    for start in 0..substituted.len().saturating_sub(1) {
        let mut joined = substituted[start].clone();
        for word in &substituted[start + 1..] {
            joined.push_str(word);
            if joined.len() > target.len() {
                break;
            }
            if joined == target {
                return true;
            }
        }
    }
    false
}

fn selector_arguments(command: &[OsString]) -> Option<Vec<String>> {
    if let Some(index) = shell_command_index(command) {
        return command[index].to_str().map(|text| vec![text.to_owned()]);
    }
    command
        .iter()
        .map(|argument| argument.to_str().map(str::to_owned))
        .collect()
}

fn shell_command_index(command: &[OsString]) -> Option<usize> {
    if command.len() < 3 || !matches!(command[1].to_str(), Some("-c" | "-lc")) {
        return None;
    }
    let program = Path::new(&command[0]).file_name()?.to_str()?;
    matches!(program, "bash" | "dash" | "sh" | "zsh").then_some(2)
}

fn remap_direct_frontend(command: &mut [String], executable: &OsStr) {
    if command.first().is_some_and(|word| word == "rtk")
        && let Some(executable) = executable.to_str()
    {
        command[0] = executable.to_owned();
    }
}

fn remap_shell_frontend(command: &str, executable: &OsStr) -> String {
    let Some(executable) = executable.to_str() else {
        return command.to_owned();
    };
    if executable == "rtk" {
        return command.to_owned();
    }
    let quoted = shell_quote(executable);
    if command == "rtk" {
        return quoted;
    }
    command
        .strip_prefix("rtk ")
        .map_or_else(|| command.to_owned(), |tail| format!("{quoted} {tail}"))
}

fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

fn degraded(command: &[OsString], executable: &OsStr, reason: &str) -> Decision {
    let mut display = executable.to_string_lossy().into_owned();
    if display.chars().count() > MAX_DIAGNOSTIC_EXECUTABLE_CHARS {
        display = display
            .chars()
            .take(MAX_DIAGNOSTIC_EXECUTABLE_CHARS - 3)
            .collect::<String>();
        display.push_str("...");
    }
    Decision {
        command: command.to_vec(),
        diagnostic: Some(format!(
            "RTK rewrite degraded to native executable={display:?}: {reason}"
        )),
    }
}

fn run_selector_in(
    executable: &OsStr,
    arguments: &[String],
    working: &Path,
    environment: Environment,
) -> Result<SelectorResult, String> {
    let selector_arguments = ["rewrite".to_owned(), "--".to_owned()]
        .into_iter()
        .chain(arguments.iter().cloned())
        .map(OsString::from)
        .collect::<Vec<_>>();
    run_bounded(
        executable,
        &selector_arguments,
        working,
        environment,
        None,
        SELECTOR_TIMEOUT,
        "optional RTK rewrite selector",
    )
    .map_err(InvocationFailure::detail)
}

fn run_bounded(
    executable: &OsStr,
    arguments: &[OsString],
    working: &Path,
    environment: Environment,
    input: Option<&[u8]>,
    runtime: Duration,
    label: &str,
) -> Result<SelectorResult, InvocationFailure> {
    let capture = StreamPolicy::Capture {
        limit: MAX_SELECTOR_OUTPUT,
    };
    let spec = Spec {
        argv: Argv::new(executable.to_owned(), arguments.iter().cloned()).map_err(|error| {
            InvocationFailure::Launch(format!("invalid selector argv: {error}"))
        })?,
        cwd: working.to_path_buf(),
        environment,
        group: ProcessGroup::New,
        deadlines: Deadlines::new(runtime, OUTPUT_DRAIN, REAP_DEADLINE).map_err(|error| {
            InvocationFailure::Launch(format!("invalid selector deadlines: {error}"))
        })?,
        wait: WaitPolicy::new(label, Cancellation::Never, RestartDisposition::Never).map_err(
            |error| InvocationFailure::Launch(format!("invalid selector wait policy: {error}")),
        )?,
        output: Output::new(
            if input.is_some() {
                StdinPolicy::Pipe
            } else {
                StdinPolicy::Null
            },
            capture,
            capture,
        ),
        redaction: Redaction::None,
        grace: TERMINATE_GRACE,
    };
    let mut child = sill::spawn(spec)
        .map_err(|error| InvocationFailure::Launch(format!("selector launch failed: {error}")))?;
    if let Some(input) = input {
        child.write_stdin(input).map_err(|error| {
            InvocationFailure::Observation(format!("selector stdin failed: {error}"))
        })?;
    }
    let outcome = child.wait().map_err(|error| {
        InvocationFailure::Observation(format!("selector process boundary failed: {error}"))
    })?;
    if !outcome.outputs_drained() {
        return Err(InvocationFailure::Observation(
            "selector descendants retained an output pipe".to_owned(),
        ));
    }
    match outcome.completion() {
        Completion::TimedOut(_) => {
            return Err(InvocationFailure::Observation(format!(
                "{label} exceeded {}ms deadline",
                runtime.as_millis()
            )));
        }
        Completion::Cancelled(_) => {
            return Err(InvocationFailure::Observation(
                "selector was cancelled".to_owned(),
            ));
        }
        Completion::Exited(_) => {}
    }
    let stdout =
        captured_text(outcome.stdout(), "stdout").map_err(InvocationFailure::Observation)?;
    let stderr =
        captured_text(outcome.stderr(), "stderr").map_err(InvocationFailure::Observation)?;
    Ok(SelectorResult {
        code: outcome.completion().status().code().unwrap_or(-1),
        stdout,
        stderr,
    })
}

fn captured_text(output: Option<&sill::CapturedOutput>, stream: &str) -> Result<String, String> {
    let output = output.ok_or_else(|| format!("selector {stream} was unavailable"))?;
    if output.truncated() {
        return Err(format!(
            "selector {stream} exceeded {MAX_SELECTOR_OUTPUT} bytes"
        ));
    }
    if !output.complete() {
        return Err(format!("selector {stream} did not close"));
    }
    String::from_utf8(output.bytes().to_vec())
        .map_err(|_| format!("selector {stream} was not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn answer(code: i32, stdout: &str) -> Result<SelectorResult, String> {
        Ok(result(code, stdout))
    }

    fn result(code: i32, stdout: &str) -> SelectorResult {
        SelectorResult {
            code,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        }
    }

    #[test]
    fn health_enables_the_exact_executable_after_one_launch_regardless_of_status_or_version() {
        let executable = OsStr::new("/opt/RTK Tools/rtk");
        let mut stage = 0;
        let observed = probe_health_with(executable, |program, arguments, input| {
            let arguments = arguments
                .iter()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            assert_eq!(
                stage, 0,
                "health must not globally probe individual rewrites"
            );
            assert_eq!(program, executable);
            assert_eq!(arguments, ["--version"]);
            assert_eq!(input, None);
            stage += 1;
            Ok(result(73, "rtk 0.41.0\n"))
        });

        assert_eq!(stage, 1);
        assert_eq!(observed.state(), HealthState::Active);
        assert_eq!(observed.version(), Some("0.41.0"));
    }

    #[test]
    fn health_disables_only_launch_failure_and_keeps_observation_advisory() {
        let missing = probe_health_with(OsStr::new("missing-rtk"), |_, _, _| {
            Err(InvocationFailure::Launch(
                "selector launch failed: not found".to_owned(),
            ))
        });
        assert_eq!(missing.state(), HealthState::Missing);

        let observed = probe_health_with(OsStr::new("rtk"), |_, arguments, _| {
            assert_eq!(arguments, [OsString::from("--version")]);
            Err(InvocationFailure::Observation(
                "selector stdout was not UTF-8".to_owned(),
            ))
        });
        assert_eq!(observed.state(), HealthState::Active);
        assert_eq!(observed.version(), None);
    }

    #[test]
    fn health_keeps_unrecognized_and_silent_version_answers_active() {
        for answer in [result(19, "unknown version\n"), result(29, "")] {
            let observed = probe_health_with(OsStr::new("rtk"), |_, arguments, _| {
                assert_eq!(arguments, [OsString::from("--version")]);
                Ok(answer.clone())
            });
            assert_eq!(observed.state(), HealthState::Active);
            assert_eq!(observed.version(), None);
        }
    }

    #[test]
    fn health_publishes_one_bounded_actionable_detail() {
        let observed = probe_health_with(OsStr::new("missing-rtk"), |_, _, _| {
            Err(InvocationFailure::Launch(
                "x".repeat(MAX_HEALTH_DETAIL_CHARS * 2),
            ))
        });

        assert_eq!(observed.state(), HealthState::Missing);
        assert_eq!(observed.detail().chars().count(), MAX_HEALTH_DETAIL_CHARS);
        assert!(observed.detail().ends_with("..."));
    }

    #[test]
    fn direct_and_shell_rewrites_remap_the_canonical_frontend() {
        let direct = words(&["git", "status"]);
        let selected = rewrite_with(&direct, OsStr::new("alternate-rtk"), |arguments| {
            assert_eq!(arguments, ["git", "status"]);
            answer(73, "rtk git status\n")
        });
        assert_eq!(
            selected.command(),
            words(&["alternate-rtk", "git", "status"])
        );
        assert_eq!(selected.diagnostic(), None);

        let shell = words(&["/bin/sh", "-c", "git status"]);
        let selected = rewrite_with(&shell, OsStr::new("/opt/RTK Tools/rtk"), |arguments| {
            assert_eq!(arguments, ["git status"]);
            answer(29, "rtk git status\n")
        });
        assert_eq!(
            selected.command()[2],
            OsString::from("'/opt/RTK Tools/rtk' git status")
        );
    }

    #[test]
    fn nonempty_rewrites_ignore_status_while_empty_and_unavailable_answers_stay_native() {
        let native = words(&["git", "status"]);
        let no_match = rewrite_with(&native, OsStr::new("rtk"), |_| answer(1, ""));
        assert_eq!(no_match.command(), native);
        assert_eq!(no_match.diagnostic(), None);

        for code in [0, 1, 3, 9, 73] {
            let decision = rewrite_with(&native, OsStr::new("rtk"), |_| {
                answer(code, "rtk git status")
            });
            assert_eq!(decision.command(), words(&["rtk", "git", "status"]));
            assert_eq!(decision.diagnostic(), None);
        }

        for response in [answer(0, ""), answer(9, "")] {
            let decision = rewrite_with(&native, OsStr::new("rtk"), |_| response);
            assert_eq!(decision.command(), native);
            assert!(
                decision
                    .diagnostic()
                    .is_some_and(|line| { line.contains("RTK rewrite degraded to native") })
            );
        }
        let unavailable = rewrite_with(&native, OsStr::new("missing-rtk"), |_| {
            Err("selector launch failed: not found".to_owned())
        });
        assert_eq!(unavailable.command(), native);
        assert!(
            unavailable
                .diagnostic()
                .is_some_and(|line| line.contains("missing-rtk"))
        );

        let long_executable = "x".repeat(4 * MAX_DIAGNOSTIC_EXECUTABLE_CHARS);
        let bounded = rewrite_with(&native, OsStr::new(&long_executable), |_| {
            Err("selector launch failed: not found".to_owned())
        });
        let diagnostic = bounded.diagnostic().expect("degradation is diagnosed");
        assert!(
            diagnostic.len() < MAX_DIAGNOSTIC_EXECUTABLE_CHARS * 2,
            "diagnostic was not bounded: {diagnostic}"
        );
        assert!(!diagnostic.contains(&long_executable));
    }

    #[test]
    fn malformed_direct_output_and_non_utf8_input_degrade_without_mutation() {
        let native = words(&["git", "status"]);
        let malformed = rewrite_with(&native, OsStr::new("rtk"), |_| {
            answer(3, "rtk 'unterminated")
        });
        assert_eq!(malformed.command(), native);
        assert!(malformed.diagnostic().is_some());

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;

            let native = vec![OsString::from_vec(vec![0xff])];
            let decision = rewrite_with(&native, OsStr::new("rtk"), |_| {
                panic!("non-UTF-8 command must not start RTK")
            });
            assert_eq!(decision.command(), native);
            assert!(decision.diagnostic().is_some());
        }
    }

    #[test]
    fn regex_narrowing_and_split_arguments_degrade_to_the_exact_native_argv() {
        for (native, rewritten, reason) in [
            (
                words(&["rg", "--count", "alpha|beta", "subject"]),
                "rtk grep --count alpha|beta subject",
                "regex dialect",
            ),
            (
                words(&["rg", "--count", "alpha|beta", "subject"]),
                "rtk rg --count alpha | beta subject",
                "split or unquoted",
            ),
            (
                words(&["rg", "needle", "path with spaces"]),
                "rtk rg needle path with spaces",
                "split or unquoted",
            ),
        ] {
            let decision = rewrite_with(&native, OsStr::new("rtk"), |_| answer(3, rewritten));
            assert_eq!(decision.command(), native);
            assert!(
                decision
                    .diagnostic()
                    .is_some_and(|diagnostic| diagnostic.contains(reason)),
                "unexpected diagnostic: {:?}",
                decision.diagnostic()
            );
        }
    }

    #[test]
    fn fidelity_accepts_extended_regex_and_quoted_argument_boundaries() {
        let native = words(&["rg", "--count", "alpha|beta", "path with spaces"]);
        let decision = rewrite_with(&native, OsStr::new("alternate-rtk"), |_| {
            answer(3, "rtk grep --count -E 'alpha|beta' 'path with spaces'")
        });

        assert_eq!(
            decision.command(),
            words(&[
                "alternate-rtk",
                "grep",
                "--count",
                "-E",
                "alpha|beta",
                "path with spaces"
            ])
        );
        assert_eq!(decision.diagnostic(), None);
    }
}
