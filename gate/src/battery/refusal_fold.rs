//! Bounded, non-authoritative diagnostics derived from a canonical battery.
//!
//! The canonical [`BatteryReport`] remains the complete machine authority. This
//! module projects only refusal facts for a human at a terminal: every refusing
//! arm, the exact selected-arm command that recovers its complete evidence, and
//! only those structural rows that contributed to refusal.

use std::fmt;

use super::family_evidence::{metric_key, metric_terminal, quoted};
use super::{ArmEvidence, BatteryArmResult, BatteryReport, BatteryState, FocusedArmReport};
use crate::metrics::{EvidenceState as MetricState, MetricEvidence, MetricMeasurement};

/// Hard byte limit for one human-facing refusal fold.
pub const BATTERY_REFUSAL_FOLD_MAX_BYTES: usize = 65_536;

/// Space retained for the terminal score and exact omission counts.
const SUMMARY_RESERVE: usize = 256;

/// Sites one refusing measurement names inside the file it already named.
///
/// Deliberately far below the report's own listing bound: the canonical report
/// carries the listing a tool reads, this carries the handful a human needs to
/// know where to start looking. Rendering every carried site would let one
/// pathological file spend the whole fold budget and push other refusing arms
/// out of the text entirely, which trades a known blind spot for a worse one.
pub(super) const REFUSAL_FOLD_SITE_LIMIT: usize = 3;

/// One bounded, non-authoritative rendering of a canonical report refusal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatteryRefusalFold {
    text: String,
    omitted_arms: usize,
    omitted_findings: usize,
}

impl BatteryRefusalFold {
    /// UTF-8 fold text, including its trailing newline.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Fold bytes suitable for a diagnostic stream.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.text.as_bytes()
    }

    /// Number of arm reasons replaced by bounded fallback summaries.
    #[must_use]
    pub const fn omitted_arms(&self) -> usize {
        self.omitted_arms
    }

    /// Number of concrete metric findings omitted by the byte limit.
    #[must_use]
    pub const fn omitted_findings(&self) -> usize {
        self.omitted_findings
    }

    /// Render several focused canonical rows under one shared byte ceiling.
    #[must_use]
    pub fn from_focused_reports(reports: &[FocusedArmReport]) -> Self {
        render(reports.iter().map(FocusedArmReport::result))
    }

    /// Consume the fold as diagnostic bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.text.into_bytes()
    }
}

impl fmt::Display for BatteryRefusalFold {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

impl BatteryReport {
    /// Derive a bounded human diagnostic without changing canonical report bytes.
    #[must_use]
    pub fn refusal_fold(&self) -> BatteryRefusalFold {
        render(self.results())
    }
}

impl FocusedArmReport {
    /// Derive the same bounded human diagnostic for this canonical focused arm.
    #[must_use]
    pub fn refusal_fold(&self) -> BatteryRefusalFold {
        render([self.result()])
    }
}

struct ArmFold {
    full: String,
    fallback: String,
}

struct FoldInventory {
    total_arms: usize,
    admitted_arms: usize,
    finding_arms: usize,
    cannot_run_arms: usize,
    metric_finding_count: usize,
    arms: Vec<ArmFold>,
    findings: Vec<String>,
}

fn render<'result>(
    results: impl IntoIterator<Item = &'result BatteryArmResult>,
) -> BatteryRefusalFold {
    let results = results.into_iter().collect::<Vec<_>>();
    let refused: Vec<_> = results
        .iter()
        .copied()
        .filter(|result| !result.state().admits())
        .collect();
    let arms: Vec<_> = refused.iter().map(|result| arm_fold(result)).collect();
    let findings: Vec<_> = refused
        .iter()
        .flat_map(|result| metric_findings(result))
        .collect();
    let metric_finding_count = findings.len();
    let finding_arms = refused
        .iter()
        .filter(|result| result.state() == BatteryState::Failed)
        .count();
    let cannot_run_arms = refused.len().saturating_sub(finding_arms);
    let admitted_arms = results.len().saturating_sub(refused.len());
    render_inventory(FoldInventory {
        total_arms: results.len(),
        admitted_arms,
        finding_arms,
        cannot_run_arms,
        metric_finding_count,
        arms,
        findings,
    })
}

fn render_inventory(inventory: FoldInventory) -> BatteryRefusalFold {
    let complete_summary = summary(
        inventory.total_arms,
        inventory.admitted_arms,
        inventory.finding_arms,
        inventory.cannot_run_arms,
        inventory.metric_finding_count,
        0,
        0,
    );
    let arm_size = inventory
        .arms
        .iter()
        .fold(0usize, |size, arm| size.saturating_add(arm.full.len()));
    let finding_size = inventory
        .findings
        .iter()
        .fold(0usize, |size, finding| size.saturating_add(finding.len()));
    let complete_size = complete_summary
        .len()
        .saturating_add(arm_size)
        .saturating_add(finding_size);
    if complete_size <= BATTERY_REFUSAL_FOLD_MAX_BYTES {
        let mut text = String::with_capacity(complete_size);
        for arm in inventory.arms {
            text.push_str(&arm.full);
        }
        for finding in inventory.findings {
            text.push_str(&finding);
        }
        text.push_str(&complete_summary);
        return BatteryRefusalFold {
            text,
            omitted_arms: 0,
            omitted_findings: 0,
        };
    }

    render_bounded(inventory)
}

fn render_bounded(inventory: FoldInventory) -> BatteryRefusalFold {
    let fallback_size = inventory
        .arms
        .iter()
        .fold(0usize, |size, arm| size.saturating_add(arm.fallback.len()));
    let fixed_size = fallback_size.saturating_add(SUMMARY_RESERVE);
    assert!(
        fixed_size <= BATTERY_REFUSAL_FOLD_MAX_BYTES,
        "compiled battery arm summaries exceed the refusal fold limit"
    );
    let mut budget = BATTERY_REFUSAL_FOLD_MAX_BYTES - fixed_size;
    let mut chosen_arms = Vec::with_capacity(inventory.arms.len());
    let mut omitted_arms = 0;
    for arm in inventory.arms {
        let extra = arm.full.len().saturating_sub(arm.fallback.len());
        if arm.full.len() <= arm.fallback.len() || extra <= budget {
            budget = budget.saturating_sub(extra);
            chosen_arms.push(arm.full);
        } else {
            omitted_arms += 1;
            chosen_arms.push(arm.fallback);
        }
    }

    let mut chosen_findings = Vec::new();
    let mut omitted_findings = 0;
    for finding in inventory.findings {
        if finding.len() <= budget {
            budget -= finding.len();
            chosen_findings.push(finding);
        } else {
            omitted_findings += 1;
        }
    }
    let summary = summary(
        inventory.total_arms,
        inventory.admitted_arms,
        inventory.finding_arms,
        inventory.cannot_run_arms,
        inventory.metric_finding_count,
        omitted_arms,
        omitted_findings,
    );
    assert!(
        summary.len() <= SUMMARY_RESERVE,
        "refusal fold summary exceeds its fixed reserve"
    );

    let mut text = String::with_capacity(BATTERY_REFUSAL_FOLD_MAX_BYTES);
    for arm in chosen_arms {
        text.push_str(&arm);
    }
    for finding in chosen_findings {
        text.push_str(&finding);
    }
    text.push_str(&summary);
    assert!(
        text.len() <= BATTERY_REFUSAL_FOLD_MAX_BYTES,
        "refusal fold bypassed its byte limit"
    );
    BatteryRefusalFold {
        text,
        omitted_arms,
        omitted_findings,
    }
}

fn summary(
    total_arms: usize,
    admitted_arms: usize,
    finding_arms: usize,
    cannot_run_arms: usize,
    metric_findings: usize,
    omitted_arms: usize,
    omitted_findings: usize,
) -> String {
    format!(
        concat!(
            "battery refusal summary: total_arms={} ",
            "admitted_arms={} finding_arms={} ",
            "cannot_run_arms={} metric_findings={} ",
            "omitted_arms={} omitted_findings={}\n"
        ),
        total_arms,
        admitted_arms,
        finding_arms,
        cannot_run_arms,
        metric_findings,
        omitted_arms,
        omitted_findings
    )
}

fn arm_fold(result: &BatteryArmResult) -> ArmFold {
    let identity = format!(
        "arm position={} gate={} state={}\n",
        result.position(),
        result.gate_id(),
        result.state().as_str()
    );
    let command = format!(
        "  full-evidence: spice dev xtask -- gate --report {}\n",
        result.gate_id()
    );
    ArmFold {
        full: format!("{identity}  reason={}\n{command}", quoted(result.reason())),
        fallback: format!("{identity}  reason=<omitted from bounded fold>\n{command}"),
    }
}

fn metric_findings(result: &BatteryArmResult) -> Vec<String> {
    let ArmEvidence::Metrics { report } = result.evidence() else {
        return Vec::new();
    };
    report
        .metrics()
        .iter()
        .filter_map(|metric| metric_finding(result, metric))
        .collect()
}

fn metric_finding(result: &BatteryArmResult, metric: &MetricEvidence) -> Option<String> {
    let terminal = metric_terminal(metric);
    if terminal.admits() {
        return None;
    }
    let mut finding = format!(
        "finding gate={} metric={} key={} state={} unit={}",
        result.gate_id(),
        metric.metric().as_str(),
        metric_key(metric.key()),
        terminal.as_str(),
        metric.unit().as_str()
    );
    match metric.state() {
        MetricState::Measured { measurement } => {
            finding.push_str(&format!(" value={}", measurement.value()));
            if let Some(threshold) = measurement.threshold() {
                finding.push_str(&format!(" threshold={threshold}"));
            }
            if let Some(latch) = measurement.latch() {
                finding.push_str(&format!(
                    " base_limit={} ratchet_limit={} active_limit={} latch_transition={}",
                    latch.base_limit(),
                    latch.ratchet_limit(),
                    latch.active_limit(),
                    latch.transition()
                ));
                if let Some(inherited) = latch.inherited() {
                    finding.push_str(&format!(" inherited={}", quoted(&inherited.to_string())));
                }
            }
        }
        MetricState::Unavailable { reason }
        | MetricState::Unmeasured { reason }
        | MetricState::NotApplicable { reason } => {
            finding.push_str(&format!(" reason={}", quoted(reason)));
        }
        MetricState::Unsupported {
            source_field,
            reason,
            recovery,
        } => {
            finding.push_str(&format!(
                " source_field={} reason={} recovery={}",
                quoted(source_field),
                quoted(reason),
                quoted(recovery)
            ));
        }
        MetricState::Timeout {
            milliseconds,
            reason,
        } => {
            finding.push_str(&format!(
                " milliseconds={milliseconds} reason={}",
                quoted(reason)
            ));
        }
        MetricState::Failed { reason, exit_code } => {
            finding.push_str(&format!(
                " exit_code={} reason={}",
                exit_code.map_or_else(|| "none".to_owned(), |code| code.to_string()),
                quoted(reason)
            ));
        }
    }
    finding.push('\n');
    if let MetricState::Measured { measurement } = metric.state() {
        finding.push_str(&finding_sites(measurement));
    }
    Some(finding)
}

/// Name where inside the file the breach sits, one bounded line per site.
///
/// A metric key names a file; before this the fold stopped there and left the
/// reader to search it. The undisplayed count is stated rather than implied,
/// because a reader who cannot tell a complete listing from a shortened one
/// will read three sites as the whole breach and stop early.
fn finding_sites(measurement: &MetricMeasurement) -> String {
    let findings = measurement.findings();
    let mut sites = String::new();
    for finding in findings.iter().take(REFUSAL_FOLD_SITE_LIMIT) {
        sites.push_str(&format!(
            "  site path={} line={} target={}",
            quoted(&finding.path),
            finding.line,
            quoted(&finding.target)
        ));
        if let Some(expected) = finding.expected.as_deref() {
            sites.push_str(&format!(" expected={}", quoted(expected)));
        }
        sites.push('\n');
    }
    let undisplayed = findings.len().saturating_sub(REFUSAL_FOLD_SITE_LIMIT);
    if undisplayed > 0 {
        sites.push_str(&format!("  sites-undisplayed={undisplayed}\n"));
    }
    sites
}
