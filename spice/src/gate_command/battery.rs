//! The ordinary battery over the staged index or one revision, decision first.
//!
//! Progress names each family as it starts and beats with the arm under
//! analysis. The reply leads with counts, lists the arms that refused (every
//! arm under `--verbose`), and names the retained complete report.

use std::time::Duration;

use gate::{BatteryReport, RepositoryExecutionEvent, Selector};
use serde::Serialize;
use spice::ambient::Ambient;

use super::output::{self, Elapsed, Phase, Progress, Summary, family_name};

/// Characters of an arm's reason one summary row shows.
const REASON_CHARS: usize = 160;

/// One battery report and its decision-first terminal projection.
///
/// `--json` carries the report exactly as before; the text never enters it.
#[derive(Serialize)]
#[serde(transparent)]
pub(crate) struct BatteryRun {
    report: BatteryReport,
    #[serde(skip)]
    text: String,
}

impl BatteryRun {
    pub(crate) fn text(&self) -> Option<&str> {
        Some(&self.text)
    }
}

/// How the person asked for this run, so its next command repeats it.
pub(super) struct Invocation<'a> {
    pub(super) at: Option<&'a str>,
    pub(super) verbose: bool,
    /// Whether progress reaches standard error (false under `--json`).
    pub(super) live: bool,
    pub(super) ambient: &'a Ambient,
}

impl Invocation<'_> {
    fn command(&self, verbose: bool) -> String {
        let mut command = "spice gate".to_owned();
        if let Some(at) = self.at {
            command.push_str(" --at ");
            command.push_str(at);
        }
        if verbose {
            command.push_str(" --verbose");
        }
        command
    }
}

/// Run the battery with live progress and summarize its decision.
pub(super) fn run(
    selector: Selector,
    invocation: &Invocation<'_>,
    captures: &output::Captures,
) -> Result<BatteryRun, spice::gate::RunError> {
    let progress = Progress::stderr("gate", invocation.live);
    let phase = Phase::new("capture");
    progress.line(format_args!("capture started"));
    let mut trace = |event: RepositoryExecutionEvent| match event {
        RepositoryExecutionEvent::FamilyInvoked { family } => {
            progress.line(format_args!("family {} started", family_name(family)));
            phase.set(format!("family {}", family_name(family)));
        }
        RepositoryExecutionEvent::AnalysisInvoked { gate_id, .. }
        | RepositoryExecutionEvent::ArmInvoked { gate_id, .. } => {
            phase.set(format!("arm {}", gate_id.as_str()));
        }
        RepositoryExecutionEvent::ToolchainAnalysisCompleted { .. } => {}
    };
    let (result, elapsed) = progress.watching(
        || phase.get(),
        || {
            let prepared = spice::gate::prepare_traced(invocation.ambient, selector, &mut trace)?;
            let snapshot = prepared.worktree_divergence()?;
            spice::gate::run_prepared(prepared).map(|report| (report, snapshot))
        },
    );
    let verdict = match &result {
        Ok((report, _)) if report.admits() => "admitted",
        Ok(_) => "refused",
        Err(_) => "failed",
    };
    progress.line(format_args!("{verdict} after {}", Elapsed(elapsed)));
    let (report, snapshot) = result?;
    let retained = wire::encode(&report)
        .map_err(|error| error.to_string())
        .and_then(|bytes| output::retain(captures, &bytes));
    let text = summary(&report, elapsed, invocation, retained.as_ref(), &snapshot);
    Ok(BatteryRun { report, text })
}

fn summary(
    report: &BatteryReport,
    elapsed: Duration,
    invocation: &Invocation<'_>,
    retained: Result<&wire::output::Export, &String>,
    snapshot: &gate::tree::WorktreeDivergence,
) -> String {
    let results = report.results();
    let refused = results
        .iter()
        .filter(|result| !result.state().admits())
        .count();
    let source = report.context().source().to_string();
    let source = source.get(..12).unwrap_or(&source);
    let mut summary = if refused == 0 {
        Summary::new(format_args!(
            "gate: admitted all {} arms for source {source} in {}",
            results.len(),
            Elapsed(elapsed),
        ))
    } else {
        Summary::new(format_args!(
            "gate: refused {refused} of {} arms for source {source} in {}",
            results.len(),
            Elapsed(elapsed),
        ))
    };
    for result in results
        .iter()
        .filter(|result| invocation.verbose || !result.state().admits())
    {
        summary.row(result.state().as_str(), result.gate_id().as_str(), None);
        let reason = result.reason().lines().next().unwrap_or_default();
        summary.detail(format_args!("{}", output::clip(reason, REASON_CHARS)));
    }
    match retained {
        Ok(export) => summary.closing(
            "complete report",
            &format!("{} ({} bytes)", export.path, export.bytes),
        ),
        Err(error) => summary.closing("complete report was not retained", error),
    }
    summary.snapshot(snapshot);
    if !invocation.verbose {
        summary.closing("detail", &invocation.command(true));
    }
    if refused > 0 {
        summary.closing("after repair", &invocation.command(false));
    } else if invocation.at.is_none() {
        summary.closing("next", "git commit");
    }
    summary.finish()
}
