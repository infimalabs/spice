//! The current source repository's strict change selection, decision first.
//!
//! A direct invocation speaks to a person: bounded phase progress on stderr,
//! then a summary reply. Under the pre-commit worker the same run forwards
//! only the protocol lines its supervisor parses. Either way subprocess output
//! is retained whole by digest and a failure shows only its bounded tail.

use std::io;
use std::time::Duration;

use spice::ambient::Ambient;
use wire::output::Export;
use wire::{Digest, Outcome, Refusal, Reply};

use spice::change_gate::ChangeGateReport;

use super::direct;
use super::output::{self, Audience, Elapsed, Phase, PhaseLog, Progress, Summary};

/// Run the current source repository's strict ordinary change selection.
///
/// This leaf is the policy-free destination for source-development delegates:
/// Cargo may build this product command, but no xtask command or hook owns a
/// second selection implementation.
#[derive(serde::Serialize)]
#[serde(rename_all = "kebab-case", tag = "evidence")]
pub(crate) enum CurrentSourceSelectionReport {
    /// Full validated evidence for a direct operator invocation.
    Focused {
        report: Box<ChangeGateReport>,
        /// The decision-first terminal projection; `--json` carries `report`.
        #[serde(skip)]
        text: String,
    },
    /// Compact success identity for the bounded configured-command supervisor.
    ///
    /// The source gate has already constructed and validated the full report;
    /// its supervisor consumes only the process disposition and exact source.
    Supervised { source: Digest },
}

impl CurrentSourceSelectionReport {
    /// The direct invocation's summary; a supervised admission stays silent.
    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Self::Focused { text, .. } => Some(text),
            Self::Supervised { .. } => None,
        }
    }

    /// Whether this outcome replies to a person rather than a supervisor.
    pub(crate) const fn speaks(outcome: &Outcome<Self>) -> bool {
        matches!(
            outcome,
            Outcome::Reply(Reply {
                body: Self::Focused { .. },
                ..
            })
        )
    }
}

pub(crate) fn selection(live: bool, ambient: &Ambient) -> Outcome<CurrentSourceSelectionReport> {
    // The supervising pre-commit worker injects this authority into its leaf.
    let supervised = ambient
        .get(wire::SpiceVariable::PreCommitSource.as_str())
        .is_some();
    let _containment = if supervised {
        match gate::measure::inherit_process_group() {
            Ok(containment) => Some(containment),
            Err(detail) => {
                return crate::environment_fault("gate::selection-containment-unavailable", detail);
            }
        }
    } else {
        None
    };
    let captures = output::current_captures();
    let progress = Progress::stderr("gate selection", live);
    let phase = Phase::new("capture");
    let audience = if supervised {
        Audience::Supervisor(Box::new(io::stderr()))
    } else {
        Audience::Person(&progress)
    };
    let mut log = PhaseLog::new(audience, phase.clone(), output::change_gate_event);
    // A supervised run is watched by its pre-commit parent, which beats.
    let (result, elapsed) = if supervised {
        (
            spice::change_gate::run_current_source(&mut log, ambient),
            Duration::ZERO,
        )
    } else {
        progress.watching(
            || phase.get(),
            || spice::change_gate::run_current_source(&mut log, ambient),
        )
    };
    let capture = log.into_capture();
    let rerun = if supervised {
        "`git commit`"
    } else {
        "`spice gate selection`"
    };
    let outcome = match result {
        Ok(report) if report.admits() && supervised => {
            direct(CurrentSourceSelectionReport::Supervised {
                source: report.selection().source().clone(),
            })
        }
        Ok(report) if report.admits() => {
            let retained = output::retain(&captures, &capture);
            let text = summary(&report, elapsed, retained.as_ref());
            direct(CurrentSourceSelectionReport::Focused {
                report: Box::new(report),
                text,
            })
        }
        Ok(report) => Outcome::Refusal(Refusal::evidence(
            "gate::selection-regressed",
            format!(
                "{}selected gate regressed\n{}",
                report.refusal_fold(),
                output::failure_evidence(&captures, &capture),
            ),
            format!("repair the selected change, then rerun {rerun}"),
        )),
        Err(error) if error.is_fault() => crate::environment_fault(
            error.reason(),
            failure_detail(&captures, error.detail(), &capture),
        ),
        Err(error) => Outcome::Refusal(Refusal::evidence(
            error.reason(),
            failure_detail(&captures, error.detail(), &capture),
            format!("repair the named selection input, then rerun {rerun}"),
        )),
    };
    if !supervised {
        progress.line(format_args!(
            "{} after {}",
            output::verdict(&outcome),
            Elapsed(elapsed)
        ));
    }
    outcome
}

/// The error's own detail, bounded, then the run's bounded tail.
fn failure_detail(captures: &output::Captures, detail: &str, capture: &[u8]) -> String {
    format!(
        "{}\n{}",
        output::bounded(captures, detail),
        output::failure_evidence(captures, capture)
    )
}

/// Counts, one row per selected policy arm, the retained output, then the
/// next command.
fn summary(
    report: &ChangeGateReport,
    elapsed: Duration,
    retained: Result<&Export, &String>,
) -> String {
    let source = report.selection().source().to_string();
    let mut summary = Summary::new(format_args!(
        "gate selection: admitted source {} in {} ({} changes, {} policy arms, {} test targets)",
        source.get(..12).unwrap_or(&source),
        Elapsed(elapsed),
        report.selection().changed_sources().len(),
        report.policy().len(),
        report.tests().tests().len(),
    ));
    for arm in report.policy().iter() {
        summary.row(arm.state().as_str(), arm.gate_id().as_str(), None);
    }
    match retained {
        Ok(export) => summary.closing(
            "complete output",
            &format!("{} ({} bytes)", export.path, export.bytes),
        ),
        Err(error) => summary.closing("complete output was not retained", error),
    }
    summary.closing("next", "git commit");
    summary.finish()
}
