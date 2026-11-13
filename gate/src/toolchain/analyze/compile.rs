//! The one bounded compiler process shared by native and app analysis.

use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::measure::spawn;
use sill::{
    Argv, Completion, Environment, Output, StdinPolicy, StreamPolicy, WaitOutcome, WaitPolicy,
};

use super::{Analysis, Captured, compile_findings, complete_compiler_output, plural};
use crate::GateId;
use crate::measure::Bounds;
use crate::toolchain::{ArmOutcome, FindingDetail};

#[derive(Clone, Copy)]
pub(in crate::toolchain) struct CompileInvocation<'a> {
    pub(super) name: &'static str,
    pub(super) program: &'a str,
    pub(super) arguments: &'a [&'a str],
    pub(super) cwd: &'a Path,
    pub(super) capture: CompileCapture,
}

#[derive(Clone, Copy)]
pub(in crate::toolchain) enum CompileCapture {
    Prefix,
    CargoJsonTail,
}

pub(in crate::toolchain) struct CompileFailure {
    pub(super) analysis: Box<Analysis>,
    elapsed: Option<Duration>,
}

pub(in crate::toolchain) struct CompileSuccess {
    elapsed: Duration,
    output: String,
}

impl CompileSuccess {
    pub(super) fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// The complete printed output of the passing compiler.
    pub(super) fn output(&self) -> &str {
        &self.output
    }
}

impl CompileFailure {
    fn before_start(analysis: Analysis) -> Self {
        Self {
            analysis: Box::new(analysis),
            elapsed: None,
        }
    }

    fn after_wait(analysis: Analysis, elapsed: Duration) -> Self {
        Self {
            analysis: Box::new(analysis),
            elapsed: Some(elapsed),
        }
    }

    pub(super) fn into_parts(self) -> (Analysis, Option<Duration>) {
        (*self.analysis, self.elapsed)
    }
}

pub(in crate::toolchain) fn run_compile_analysis(
    id: GateId,
    root: &Path,
    invocation: CompileInvocation<'_>,
    bounds: &Bounds,
    environment: &Environment,
    detail: fn(String, String) -> FindingDetail,
) -> Result<CompileSuccess, CompileFailure> {
    run_compile_analysis_with_wait(id, root, invocation, bounds, environment, detail, None)
}

pub(in crate::toolchain) fn run_compile_analysis_with_wait(
    id: GateId,
    root: &Path,
    invocation: CompileInvocation<'_>,
    bounds: &Bounds,
    environment: &Environment,
    detail: fn(String, String) -> FindingDetail,
    wait: Option<WaitPolicy>,
) -> Result<CompileSuccess, CompileFailure> {
    let CompileInvocation {
        name,
        program,
        arguments,
        cwd,
        capture,
    } = invocation;
    let argv = Argv::new(program, arguments.iter().map(OsStr::new)).map_err(|error| {
        CompileFailure::before_start(Analysis::terminal(
            ArmOutcome::ParseFailure,
            format!("{name} argument vector was refused: {error}"),
        ))
    })?;
    let mut spec = bounds.spec(argv, cwd);
    spec.environment = environment.clone();
    configure_output(&mut spec, capture, bounds);
    if let Some(wait) = wait {
        spec.wait = wait;
    }
    let (outcome, elapsed) = wait_for_compile(name, spec)?;
    compile_reply(
        CompileReply {
            id,
            root,
            cwd,
            name,
            capture,
            detail,
        },
        &Captured::of(&outcome),
        elapsed,
    )
}

/// Everything one compile reply is judged against besides its captured result.
#[derive(Clone, Copy)]
pub(in crate::toolchain) struct CompileReply<'a> {
    pub(super) id: GateId,
    pub(super) root: &'a Path,
    pub(super) cwd: &'a Path,
    pub(super) name: &'static str,
    pub(super) capture: CompileCapture,
    pub(super) detail: fn(String, String) -> FindingDetail,
}

/// What one reaped compiler says: typed diagnostics outrank its exit status.
pub(in crate::toolchain) fn compile_reply(
    reply: CompileReply<'_>,
    captured: &Captured<'_>,
    elapsed: Duration,
) -> Result<CompileSuccess, CompileFailure> {
    let CompileReply {
        id,
        root,
        cwd,
        name,
        capture,
        detail,
    } = reply;
    let status = match captured.completion {
        Completion::Exited(status) => *status,
        Completion::TimedOut(_) => {
            let analysis = Analysis::terminal(
                ArmOutcome::Timeout,
                format!("{name} outlived the bounds it was given"),
            );
            return Err(CompileFailure::after_wait(analysis, elapsed));
        }
        Completion::Cancelled(_) => {
            let analysis = Analysis::terminal(
                ArmOutcome::Cancelled,
                format!("{name} was cancelled before it reported"),
            );
            return Err(CompileFailure::after_wait(analysis, elapsed));
        }
    };
    let printed = complete_compiler_output(
        name,
        captured,
        matches!(capture, CompileCapture::CargoJsonTail),
    )
    .map_err(|reason| {
        CompileFailure::after_wait(
            Analysis::terminal(ArmOutcome::ParseFailure, reason),
            elapsed,
        )
    })?;
    let findings = compile_findings(root, cwd, &printed, detail);
    if !findings.is_empty() {
        return Err(CompileFailure::after_wait(
            Analysis {
                outcome: ArmOutcome::Findings,
                reason: format!(
                    "{} parsed {} from {name}",
                    id.as_str(),
                    plural(findings.len(), "diagnostic")
                ),
                findings,
                exit_code: None,
                timing: None,
            },
            elapsed,
        ));
    }
    if status.success() {
        return Ok(CompileSuccess {
            elapsed,
            output: printed,
        });
    }
    let Some(code) = status.code() else {
        return Err(CompileFailure::after_wait(
            Analysis::terminal(
                ArmOutcome::ParseFailure,
                with_compiler_output(
                    format!("{name} was signalled without a typed diagnostic"),
                    &printed,
                ),
            ),
            elapsed,
        ));
    };
    Err(CompileFailure::after_wait(
        Analysis::refused(
            with_compiler_output(
                format!("{name} refused with {code} and emitted no typed diagnostic"),
                &printed,
            ),
            code,
        ),
        elapsed,
    ))
}

fn with_compiler_output(mut reason: String, printed: &str) -> String {
    let printed = printed.trim();
    if !printed.is_empty() {
        reason.push_str("; compiler output: ");
        // Captures are already bounded; report reasons must remain single-line.
        reason.extend(printed.chars().map(|character| match character {
            '\n' | '\r' => ' ',
            other => other,
        }));
    }
    reason
}

fn wait_for_compile(
    name: &str,
    spec: sill::Spec,
) -> Result<(WaitOutcome, Duration), CompileFailure> {
    let running = spawn(spec).map_err(|error| {
        CompileFailure::before_start(Analysis::terminal(
            ArmOutcome::ParseFailure,
            format!("{name} did not start: {error}"),
        ))
    })?;
    let started = Instant::now();
    let outcome = running.wait().map_err(|error| {
        CompileFailure::after_wait(
            Analysis::terminal(
                ArmOutcome::ParseFailure,
                format!("{name} could not be waited on: {error}"),
            ),
            started.elapsed(),
        )
    })?;
    Ok((outcome, started.elapsed()))
}

fn configure_output(spec: &mut sill::Spec, capture: CompileCapture, bounds: &Bounds) {
    if matches!(capture, CompileCapture::CargoJsonTail) {
        spec.output = Output::new(
            StdinPolicy::Null,
            StreamPolicy::CaptureTail {
                limit: bounds.capture(),
            },
            StreamPolicy::Capture {
                limit: bounds.capture(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::ExitStatus;

    use super::*;
    use crate::toolchain::ArmFinding;
    use crate::toolchain::captured::Stream;

    const ROOT: &str = "/fixture/tree";

    fn stream(bytes: &[u8], truncated: bool) -> Option<Stream<'_>> {
        Some(Stream {
            bytes,
            truncated,
            complete: true,
        })
    }

    fn reply(
        capture: CompileCapture,
        completion: &Completion,
        stdout: Option<Stream<'_>>,
        stderr: Option<Stream<'_>>,
    ) -> Result<CompileSuccess, Analysis> {
        compile_reply(
            CompileReply {
                id: GateId::AppWebTypecheck,
                root: Path::new(ROOT),
                cwd: Path::new(ROOT),
                name: "fixture compiler",
                capture,
                detail: |code, message| FindingDetail::AppWebOutput { code, message },
            },
            &Captured {
                completion,
                observation_failure: None,
                stdout,
                stderr,
            },
            Duration::ZERO,
        )
        .map_err(|failure| failure.into_parts().0)
    }

    fn exited(code: i32) -> Completion {
        Completion::Exited(ExitStatus::from_raw(code << 8))
    }

    /// A refusal the compiler did not type keeps its exit code and what it
    /// printed, flattened onto the one line a report reason may carry.
    #[test]
    fn an_untyped_refusal_retains_its_code_and_printed_output() {
        let Err(analysis) = reply(
            CompileCapture::Prefix,
            &exited(17),
            stream(b"compiling fixture\r\n", false),
            stream(
                b"error: lint configuration rejected\n  invalid lint level\n",
                false,
            ),
        ) else {
            panic!("a refusing compiler was admitted");
        };

        assert_eq!(analysis.outcome, ArmOutcome::NonzeroAnalysis);
        assert_eq!(analysis.exit_code, Some(17));
        assert!(analysis.findings.is_empty());
        for printed in [
            "compiling fixture",
            "error: lint configuration rejected",
            "invalid lint level",
        ] {
            assert!(analysis.reason.contains(printed), "{}", analysis.reason);
        }
        assert!(!analysis.reason.contains(['\n', '\r']));
    }

    /// A typed diagnostic is the finding even when the compiler then exits
    /// nonzero, and is located relative to the selected tree.
    #[test]
    fn typed_diagnostics_outrank_the_exit_status() {
        let Err(analysis) = reply(
            CompileCapture::Prefix,
            &exited(101),
            stream(b"", false),
            stream(
                b"error[E0308]: mismatched types\n  --> /fixture/tree/app/src/lib.rs:3:9\n",
                false,
            ),
        ) else {
            panic!("a diagnosed compiler was admitted");
        };

        assert_eq!(analysis.outcome, ArmOutcome::Findings);
        assert_eq!(analysis.exit_code, None);
        assert_eq!(
            analysis.findings,
            [ArmFinding {
                path: Some("app/src/lib.rs".to_owned()),
                line: Some(3),
                column: Some(9),
                detail: FindingDetail::AppWebOutput {
                    code: "E0308".to_owned(),
                    message: "mismatched types".to_owned(),
                },
            }]
        );
    }

    /// Cargo JSON keeps only whole records past the truncation point; every
    /// other capture must be whole before its silence can mean success.
    #[test]
    fn only_cargo_json_may_be_read_from_a_truncated_tail() {
        assert!(
            reply(
                CompileCapture::CargoJsonTail,
                &exited(0),
                stream(b"ial record\n{\"reason\":\"build-finished\"}\n", true),
                stream(b"", false),
            )
            .is_ok(),
            "a clean tail was refused"
        );
        let Err(truncated) = reply(
            CompileCapture::Prefix,
            &exited(0),
            stream(b"partial", true),
            stream(b"", false),
        ) else {
            panic!("a truncated prefix capture was admitted");
        };
        assert_eq!(truncated.outcome, ArmOutcome::ParseFailure);

        let Err(unread) = reply(
            CompileCapture::CargoJsonTail,
            &exited(0),
            stream(b"", false),
            stream(b"warning", true),
        ) else {
            panic!("a truncated stderr was admitted");
        };
        assert_eq!(unread.outcome, ArmOutcome::ParseFailure);
    }

    #[test]
    fn bounds_and_cancellation_end_without_reading_output() {
        let killed = ExitStatus::from_raw(9);
        for (completion, outcome) in [
            (Completion::TimedOut(killed), ArmOutcome::Timeout),
            (Completion::Cancelled(killed), ArmOutcome::Cancelled),
            (Completion::Exited(killed), ArmOutcome::ParseFailure),
        ] {
            let Err(analysis) = reply(
                CompileCapture::Prefix,
                &completion,
                stream(b"", false),
                stream(b"", false),
            ) else {
                panic!("{completion:?} was admitted");
            };
            assert_eq!(analysis.outcome, outcome);
        }
    }
}
