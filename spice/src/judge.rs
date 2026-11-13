//! One bounded external YES/NO adjudication process shared by every question.

use std::env;
use std::time::Duration;

use spice::ambient::Ambient;

use sill::{
    Argv, Cancellation, CancellationToken, Completion, Deadlines, Environment, Output,
    ProcessGroup, Redaction, RestartDisposition, Spec, StdinPolicy, StreamPolicy, WaitPolicy,
};

const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_REPLY_BYTES: usize = 4 * 1024;
const OUTPUT_DRAIN: Duration = Duration::from_millis(250);
const REAP_DEADLINE: Duration = Duration::from_millis(500);
const TERMINATE_GRACE: Duration = Duration::from_millis(100);

/// One semantically complete framing, identified independently of its order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Prompt {
    framing: u8,
    text: String,
}

impl Prompt {
    pub(crate) fn new(framing: u8, text: String) -> Self {
        Self { framing, text }
    }

    pub(crate) const fn framing(&self) -> u8 {
        self.framing
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }
}

/// One bounded invocation retained beside a decisive answer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Attempt {
    framing: u8,
    reply: String,
    answer: Option<bool>,
}

impl Attempt {
    pub(crate) const fn framing(&self) -> u8 {
        self.framing
    }

    pub(crate) fn reply(&self) -> &str {
        &self.reply
    }

    pub(crate) const fn answer(&self) -> Option<bool> {
        self.answer
    }
}

/// The first unambiguous answer and every attempt that led to it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Decision {
    confirmed: bool,
    attempts: Vec<Attempt>,
}

impl Decision {
    pub(crate) const fn confirmed(&self) -> bool {
        self.confirmed
    }

    pub(crate) fn attempts(&self) -> &[Attempt] {
        &self.attempts
    }
}

/// Failure to obtain one decisive answer inside the declared boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Error {
    reason: &'static str,
    detail: String,
}

impl Error {
    #[cfg(test)]
    pub(crate) const fn reason(&self) -> &'static str {
        self.reason
    }
}

/// The sole executable/model boundary used by adjudication consumers.
#[derive(Debug)]
pub(crate) struct Process {
    binary: String,
    timeout: Duration,
    cancellation: CancellationToken,
    ambient: Ambient,
}

impl Process {
    /// Ask ordered equivalent framings until exactly one normalized answer lands.
    pub(crate) fn adjudicate(&mut self, prompts: &[Prompt]) -> Result<Decision, Error> {
        let binary = self.binary.clone();
        let timeout = self.timeout;
        let cancellation = self.cancellation.clone();
        let environment = self.ambient.environment().map_err(spec_error)?;
        adjudicate_with(prompts, |prompt| {
            invoke_with(
                &binary,
                prompt,
                timeout,
                cancellation.clone(),
                environment.clone(),
            )
        })
    }

    /// A judge whose attempts end with `cancellation` and inherit `ambient`.
    pub(crate) fn new(cancellation: CancellationToken, ambient: Ambient) -> Self {
        Self {
            binary: crate::cli::JUDGE_EXECUTABLE.to_owned(),
            timeout: ATTEMPT_TIMEOUT,
            cancellation,
            ambient,
        }
    }
}

pub(crate) fn adjudicate_with(
    prompts: &[Prompt],
    mut invoke_prompt: impl FnMut(&str) -> Result<String, Error>,
) -> Result<Decision, Error> {
    if prompts.is_empty() {
        return Err(failure_error(
            "judge_error",
            "an adjudication question declared no framing",
        ));
    }
    let mut attempts = Vec::with_capacity(prompts.len());
    for prompt in prompts {
        match invoke_prompt(prompt.text()) {
            Ok(raw) => {
                let reply = compact(&raw);
                let answer = parse_yes_no(&raw);
                attempts.push(Attempt {
                    framing: prompt.framing(),
                    reply,
                    answer,
                });
                if let Some(confirmed) = answer {
                    return Ok(Decision {
                        confirmed,
                        attempts,
                    });
                }
            }
            Err(error) if error.reason == "ambiguous" => attempts.push(Attempt {
                framing: prompt.framing(),
                reply: compact(&error.detail),
                answer: None,
            }),
            Err(error) => return Err(error),
        }
    }
    Err(Error {
        reason: "ambiguous",
        detail: format!(
            "judge did not return one YES/NO in {} attempts: {}",
            prompts.len(),
            attempts
                .iter()
                .map(|attempt| format!("{}:{:?}", attempt.framing, attempt.reply))
                .collect::<Vec<_>>()
                .join(" | ")
        ),
    })
}

fn invoke_with(
    binary: &str,
    prompt: &str,
    timeout: Duration,
    cancellation: CancellationToken,
    environment: Environment,
) -> Result<String, Error> {
    let spec = process_spec(binary, timeout, cancellation, environment)?;
    let mut child = sill::spawn(spec).map_err(|error| Error {
        reason: "unavailable",
        detail: format!("could not launch {binary}: {error}"),
    })?;
    child
        .write_stdin(prompt.as_bytes())
        .map_err(|error| Error {
            reason: "judge_error",
            detail: format!("could not write judge prompt: {error}"),
        })?;
    let outcome = child.wait().map_err(|error| Error {
        reason: "judge_error",
        detail: format!("judge process boundary failed: {error}"),
    })?;
    reply(&outcome, binary, timeout)
}

/// Classify one reaped judge process: only a clean, drained, successful exit
/// carries a reply.
fn reply(outcome: &sill::WaitOutcome, binary: &str, timeout: Duration) -> Result<String, Error> {
    classify_reply(
        ReplyEvidence {
            completion: outcome.completion(),
            stdout: outcome.stdout().map(ReplyCapture::from),
            stderr: outcome.stderr().map(ReplyCapture::from),
            outputs_drained: outcome.outputs_drained(),
        },
        binary,
        timeout,
    )
}

/// The bounded process evidence the judge classifier consumes.
///
/// Keeping the classifier on borrowed bytes avoids copying capture buffers and
/// lets its observable boundaries run in process without manufacturing a child.
struct ReplyEvidence<'a> {
    completion: &'a Completion,
    stdout: Option<ReplyCapture<'a>>,
    stderr: Option<ReplyCapture<'a>>,
    outputs_drained: bool,
}

#[derive(Clone, Copy)]
struct ReplyCapture<'a> {
    bytes: &'a [u8],
    truncated: bool,
    complete: bool,
}

impl<'a> From<&'a sill::CapturedOutput> for ReplyCapture<'a> {
    fn from(output: &'a sill::CapturedOutput) -> Self {
        Self {
            bytes: output.bytes(),
            truncated: output.truncated(),
            complete: output.complete(),
        }
    }
}

fn classify_reply(
    evidence: ReplyEvidence<'_>,
    binary: &str,
    timeout: Duration,
) -> Result<String, Error> {
    if !evidence.outputs_drained {
        return Err(failure_error(
            "judge_error",
            "judge descendants retained an output pipe past the drain deadline",
        ));
    }
    match evidence.completion {
        Completion::TimedOut(_) => {
            return Err(Error {
                reason: "timeout",
                detail: format!("judge exceeded {}ms", timeout.as_millis()),
            });
        }
        Completion::Cancelled(_) => {
            return Err(failure_error("cancelled", "judge request was cancelled"));
        }
        Completion::Exited(_) => {}
    }
    let stdout = captured_output(evidence.stdout, "stdout")?;
    let stderr = captured_output(evidence.stderr, "stderr")?;
    let status = evidence.completion.status();
    if !status.success() {
        return Err(Error {
            reason: "unavailable",
            detail: format!(
                "{binary} exited with {}{}",
                status
                    .code()
                    .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {}", compact(stderr))
                },
            ),
        });
    }
    Ok(stdout.to_owned())
}

fn process_spec(
    binary: &str,
    timeout: Duration,
    cancellation: CancellationToken,
    environment: Environment,
) -> Result<Spec, Error> {
    let capture = StreamPolicy::Capture {
        limit: MAX_REPLY_BYTES,
    };
    Ok(Spec {
        argv: Argv::new(binary, std::iter::empty::<String>()).map_err(spec_error)?,
        cwd: env::current_dir().map_err(|error| Error {
            reason: "judge_error",
            detail: format!("could not resolve judge working directory: {error}"),
        })?,
        environment,
        group: ProcessGroup::New,
        deadlines: Deadlines::new(timeout, OUTPUT_DRAIN, REAP_DEADLINE).map_err(spec_error)?,
        wait: WaitPolicy::new(
            "shared optional judge",
            Cancellation::Token(cancellation),
            RestartDisposition::Never,
        )
        .map_err(spec_error)?,
        output: Output::new(StdinPolicy::Pipe, capture, capture),
        redaction: Redaction::All,
        grace: TERMINATE_GRACE,
    })
}

fn spec_error(error: sill::SpecError) -> Error {
    Error {
        reason: "judge_error",
        detail: format!("judge process contract is invalid: {error}"),
    }
}

fn captured_output<'a>(output: Option<ReplyCapture<'a>>, stream: &str) -> Result<&'a str, Error> {
    let output = output.ok_or_else(|| Error {
        reason: "judge_error",
        detail: format!("judge {stream} capture was unavailable"),
    })?;
    if output.truncated {
        return Err(Error {
            reason: "ambiguous",
            detail: format!("judge {stream} exceeded {MAX_REPLY_BYTES} bytes"),
        });
    }
    if !output.complete {
        return Err(Error {
            reason: "judge_error",
            detail: format!("judge {stream} did not close within the output deadline"),
        });
    }
    std::str::from_utf8(output.bytes).map_err(|error| Error {
        reason: "ambiguous",
        detail: format!("judge {stream} was not UTF-8: {error}"),
    })
}

pub(crate) fn parse_yes_no(raw: &str) -> Option<bool> {
    let compact = compact(raw);
    let normalized = compact
        .trim_end_matches(['.', '!'])
        .trim()
        .to_ascii_uppercase();
    match normalized.as_str() {
        "YES" => Some(true),
        "NO" => Some(false),
        _ => None,
    }
}

pub(crate) fn compact(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn failure_error(reason: &'static str, detail: &str) -> Error {
    Error {
        reason,
        detail: detail.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::ExitStatus;

    fn prompts() -> Vec<Prompt> {
        vec![
            Prompt::new(2, "third semantic framing".to_owned()),
            Prompt::new(0, "first semantic framing".to_owned()),
            Prompt::new(1, "second semantic framing".to_owned()),
        ]
    }

    const TEST_TIMEOUT: Duration = Duration::from_millis(40);

    fn capture(bytes: &[u8]) -> ReplyCapture<'_> {
        ReplyCapture {
            bytes,
            truncated: false,
            complete: true,
        }
    }
    fn exit_status(code: u32) -> ExitStatus {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt as _;
            ExitStatus::from_raw((code as i32) << 8)
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt as _;
            ExitStatus::from_raw(code)
        }
    }

    fn evidence<'a>(
        completion: &'a Completion,
        stdout: Option<ReplyCapture<'a>>,
        stderr: Option<ReplyCapture<'a>>,
        outputs_drained: bool,
    ) -> ReplyEvidence<'a> {
        ReplyEvidence {
            completion,
            stdout,
            stderr,
            outputs_drained,
        }
    }

    #[test]
    fn malformed_replies_retry_in_declared_order_and_stop_on_one_answer() {
        let mut invoked = Vec::new();
        let mut replies = ["Answer: maybe", "YES"].into_iter();

        let decision = adjudicate_with(&prompts(), |prompt| {
            invoked.push(prompt.to_owned());
            Ok(replies.next().expect("bounded fixture reply").to_owned())
        })
        .expect("second framing is decisive");

        assert!(decision.confirmed());
        assert_eq!(
            invoked,
            ["third semantic framing", "first semantic framing"]
        );
        assert_eq!(
            decision
                .attempts()
                .iter()
                .map(Attempt::framing)
                .collect::<Vec<_>>(),
            [2, 0]
        );
        assert_eq!(decision.attempts()[0].answer(), None);
        assert_eq!(decision.attempts()[1].answer(), Some(true));
    }

    #[test]
    fn ambiguity_exhausts_the_exact_declared_bound_without_interpretation() {
        let calls = std::cell::Cell::new(0);
        let error = adjudicate_with(&prompts(), |_| {
            calls.set(calls.get() + 1);
            Ok("YES and NO".to_owned())
        })
        .expect_err("mixed replies never become a verdict");

        assert_eq!(calls.get(), 3);
        assert_eq!(error.reason(), "ambiguous");
    }

    #[test]
    fn a_clean_success_returns_the_complete_captured_reply() {
        let completion = Completion::Exited(exit_status(0));
        let reply = classify_reply(
            evidence(
                &completion,
                Some(capture(b"YES\n")),
                Some(capture(b"")),
                true,
            ),
            "judge",
            TEST_TIMEOUT,
        )
        .expect("a clean judge reply is available");

        assert_eq!(reply, "YES\n");
    }

    #[test]
    fn a_nonzero_exit_is_unavailable() {
        let completion = Completion::Exited(exit_status(7));
        let error = classify_reply(
            evidence(
                &completion,
                Some(capture(b"")),
                Some(capture(b"not ready")),
                true,
            ),
            "judge",
            TEST_TIMEOUT,
        )
        .expect_err("an unsuccessful judge has no reply");

        assert_eq!(error.reason(), "unavailable");
    }

    #[test]
    fn undrained_output_is_a_boundary_failure() {
        let completion = Completion::Exited(exit_status(0));
        let error = classify_reply(
            evidence(
                &completion,
                Some(capture(b"YES\n")),
                Some(capture(b"")),
                false,
            ),
            "judge",
            TEST_TIMEOUT,
        )
        .expect_err("a retained output pipe invalidates the reply");

        assert_eq!(error.reason(), "judge_error");
    }

    #[test]
    fn timeout_and_cancellation_remain_distinct() {
        let success = exit_status(0);
        let timed_out = Completion::TimedOut(success);
        let cancelled = Completion::Cancelled(success);

        let timeout = classify_reply(
            evidence(&timed_out, None, None, true),
            "judge",
            TEST_TIMEOUT,
        )
        .expect_err("a timed-out judge has no reply");
        let cancellation = classify_reply(
            evidence(&cancelled, None, None, true),
            "judge",
            TEST_TIMEOUT,
        )
        .expect_err("a cancelled judge has no reply");

        assert_eq!(timeout.reason(), "timeout");
        assert_eq!(cancellation.reason(), "cancelled");
    }

    #[test]
    fn invalid_captures_keep_their_failure_class() {
        let completion = Completion::Exited(exit_status(0));
        let stderr = Some(capture(b""));

        let truncated = classify_reply(
            evidence(
                &completion,
                Some(ReplyCapture {
                    truncated: true,
                    ..capture(b"partial")
                }),
                stderr,
                true,
            ),
            "judge",
            TEST_TIMEOUT,
        )
        .expect_err("truncated output is ambiguous");
        let incomplete = classify_reply(
            evidence(
                &completion,
                Some(ReplyCapture {
                    complete: false,
                    ..capture(b"YES\n")
                }),
                stderr,
                true,
            ),
            "judge",
            TEST_TIMEOUT,
        )
        .expect_err("an open capture is a boundary failure");
        let missing = classify_reply(
            evidence(&completion, None, stderr, true),
            "judge",
            TEST_TIMEOUT,
        )
        .expect_err("a missing capture is a boundary failure");
        let invalid_utf8 = classify_reply(
            evidence(&completion, Some(capture(b"\xff")), stderr, true),
            "judge",
            TEST_TIMEOUT,
        )
        .expect_err("non-UTF-8 output is ambiguous");

        assert_eq!(truncated.reason(), "ambiguous");
        assert_eq!(incomplete.reason(), "judge_error");
        assert_eq!(missing.reason(), "judge_error");
        assert_eq!(invalid_utf8.reason(), "ambiguous");
    }

    #[test]
    fn parser_accepts_only_one_normalized_yes_or_no() {
        assert_eq!(parse_yes_no(" YES\n"), Some(true));
        assert_eq!(parse_yes_no("no."), Some(false));
        assert_eq!(parse_yes_no("Answer: NO"), None);
        assert_eq!(parse_yes_no("YES but NO"), None);
        assert_eq!(parse_yes_no("perhaps"), None);
    }
}
