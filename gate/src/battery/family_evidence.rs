use serde::{Deserialize, Serialize};
use wire::Digest;

use super::{ArmFamily, BatteryError, valid_text};
use crate::core::{CoreArmReport, CoreStanding};
use crate::hygiene::{AnalysisEvidence, HygieneMeasurement, HygieneOutcome, HygieneProvenance};
use crate::manifest::GateId;
use crate::metrics::{
    EvidenceState as MetricState, MetricEvidence, MetricKey, MetricMeasurement, MetricUnit,
    StructuralArmEvidence,
};
use crate::toolchain::{ArmMeasurement, ArmOutcome};

/// Exact common battery result vocabulary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BatteryState {
    Passed,
    Failed,
    Unavailable,
    Unsupported,
    Unmeasured,
    TimedOut,
    ExplicitSkip,
    NotApplicable,
    Excluded,
}

impl BatteryState {
    /// Only pass and contract-owned absence/exclusion admit publication.
    #[must_use]
    pub const fn admits(self) -> bool {
        matches!(self, Self::Passed | Self::NotApplicable | Self::Excluded)
    }

    /// Stable terminal-state spelling used by bounded human diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Unavailable => "unavailable",
            Self::Unsupported => "unsupported",
            Self::Unmeasured => "unmeasured",
            Self::TimedOut => "timed-out",
            Self::ExplicitSkip => "explicit-skip",
            Self::NotApplicable => "not-applicable",
            Self::Excluded => "excluded",
        }
    }
}

/// Exact provenance required for an admitted non-execution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultProvenance {
    release: String,
    source: String,
    field: Vec<String>,
}

impl ResultProvenance {
    /// Bind an applicability/exclusion decision to its exact source field.
    pub fn new(
        release: impl Into<String>,
        source: impl Into<String>,
        field: Vec<String>,
    ) -> Result<Self, BatteryError> {
        let provenance = Self {
            release: release.into(),
            source: source.into(),
            field,
        };
        provenance.validate()?;
        Ok(provenance)
    }

    fn validate(&self) -> Result<(), BatteryError> {
        if !valid_text(&self.release)
            || !valid_text(&self.source)
            || self.field.is_empty()
            || self.field.iter().any(|part| !valid_text(part))
        {
            return Err(BatteryError::new(
                "result provenance requires release, source, and exact field path",
            ));
        }
        Ok(())
    }

    /// Release that owns the decision grammar.
    #[must_use]
    pub fn release(&self) -> &str {
        &self.release
    }

    /// Exact policy or declaration source.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Exact structurally segmented field.
    #[must_use]
    pub fn field(&self) -> &[String] {
        &self.field
    }
}

/// Existing family-owned typed evidence retained by one battery position.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "family")]
pub enum ArmEvidence {
    Core {
        report: CoreArmReport,
    },
    Toolchain {
        report: ArmMeasurement,
    },
    Metrics {
        report: StructuralArmEvidence,
    },
    Hygiene {
        report: HygieneMeasurement,
    },
    ExecutorFailure {
        gate_id: GateId,
        arm_family: ArmFamily,
        declaration: String,
        reason: String,
    },
}

impl ArmEvidence {
    /// Family represented by compiled evidence.
    #[must_use]
    pub const fn family(&self) -> ArmFamily {
        match self {
            Self::Core { .. } => ArmFamily::Core,
            Self::Toolchain { .. } => ArmFamily::Toolchain,
            Self::Metrics { .. } => ArmFamily::Metrics,
            Self::Hygiene { .. } => ArmFamily::Hygiene,
            Self::ExecutorFailure { arm_family, .. } => *arm_family,
        }
    }

    /// Gate identity carried by the family report.
    #[must_use]
    pub const fn gate_id(&self) -> GateId {
        match self {
            Self::Core { report } => report.id(),
            Self::Toolchain { report } => report.gate_id,
            Self::Metrics { report } => report.gate_id(),
            Self::Hygiene { report } => report.gate_id,
            Self::ExecutorFailure { gate_id, .. } => *gate_id,
        }
    }

    /// Typed terminal state derived from the retained family authority.
    pub fn state(&self) -> Result<BatteryState, BatteryError> {
        match self {
            Self::Core { report } => Ok(core_state(report.id(), report.standing())),
            Self::Toolchain { report } => Ok(toolchain_state(report.outcome)),
            Self::Metrics { report } => metric_state(report),
            Self::Hygiene { report } => Ok(hygiene_state(report.outcome)),
            Self::ExecutorFailure { .. } => Ok(BatteryState::Failed),
        }
    }

    /// Exact result reason derived from the retained family authority.
    pub fn reason(&self) -> Result<String, BatteryError> {
        match self {
            Self::Core { report } => Ok(report.diagnostic().to_owned()),
            Self::Toolchain { report } => Ok(report.reason.clone()),
            Self::Metrics { report } => metric_reason(report),
            Self::Hygiene { report } => hygiene_reason(report),
            Self::ExecutorFailure { reason, .. } => Ok(reason.clone()),
        }
    }

    /// Declaration-owned provenance for applicability or exclusion.
    pub fn provenance(&self) -> Result<Option<ResultProvenance>, BatteryError> {
        match self {
            Self::Core { report }
                if report.id() == GateId::RepoShape
                    && report.standing() == CoreStanding::Skipped =>
            {
                Ok(Some(ResultProvenance::new(
                    "current",
                    "gate::core::repo-shape",
                    vec!["compiled-applicability".to_owned()],
                )?))
            }
            Self::Toolchain { report } if report.outcome.is_skip() => {
                Ok(Some(ResultProvenance::new(
                    "current",
                    report.configuration_source.clone(),
                    vec![report.configuration_field.clone()],
                )?))
            }
            Self::Metrics { report } if metric_state(report)? == BatteryState::NotApplicable => {
                Ok(Some(ResultProvenance::new(
                    "current",
                    format!("gate::metrics::{}", report.gate_id()),
                    vec!["compiled-applicability".to_owned()],
                )?))
            }
            Self::Hygiene { report } if report.outcome.is_skip() => {
                Ok(Some(hygiene_provenance(report)?))
            }
            _ => Ok(None),
        }
    }

    pub(super) fn validate_execution(
        &self,
        state: BatteryState,
        reason: &str,
        provenance: Option<&ResultProvenance>,
    ) -> Result<(), BatteryError> {
        self.validate()?;
        if self.state()? != state || self.reason()? != reason {
            return Err(BatteryError::new(
                "arm state or reason disagrees with its retained family evidence",
            ));
        }
        let expected = self.provenance()?;
        if provenance != expected.as_ref() {
            return Err(BatteryError::new(
                "arm provenance disagrees with its declaration-owned skip evidence",
            ));
        }
        Ok(())
    }

    pub(super) fn validate_expected(
        &self,
        family: ArmFamily,
        gate_id: GateId,
        declaration: &str,
        source: &Digest,
    ) -> Result<(), BatteryError> {
        if self.family() != family || self.gate_id() != gate_id {
            return Err(BatteryError::new(
                "arm evidence belongs to a different compiled gate or family",
            ));
        }
        match self {
            Self::Core { report } if report.tree() != source => Err(BatteryError::new(
                "core arm evidence belongs to a different immutable tree",
            )),
            Self::ExecutorFailure {
                declaration: observed,
                ..
            } if observed != declaration => Err(BatteryError::new(
                "executor failure names a different compiled declaration",
            )),
            _ => Ok(()),
        }
    }

    fn validate(&self) -> Result<(), BatteryError> {
        match self {
            Self::Core { report } => report
                .validate()
                .map_err(|error| BatteryError::new(format!("invalid core evidence: {error}"))),
            Self::Toolchain { report } => report
                .validate()
                .map_err(|error| BatteryError::new(format!("invalid toolchain evidence: {error}"))),
            Self::Metrics { report } => report
                .validate()
                .map_err(|error| BatteryError::new(format!("invalid metric evidence: {error}"))),
            Self::Hygiene { report } => report
                .validate()
                .map_err(|error| BatteryError::new(format!("invalid hygiene evidence: {error}"))),
            Self::ExecutorFailure {
                declaration,
                reason,
                ..
            } if valid_text(declaration) && valid_text(reason) => Ok(()),
            Self::ExecutorFailure { .. } => Err(BatteryError::new(
                "executor failure requires declaration and one-line reason",
            )),
        }
    }
}

const fn core_state(gate_id: GateId, standing: CoreStanding) -> BatteryState {
    match standing {
        CoreStanding::Passed => BatteryState::Passed,
        CoreStanding::Failed => BatteryState::Failed,
        CoreStanding::Unavailable => BatteryState::Unavailable,
        CoreStanding::Unsupported => BatteryState::Unsupported,
        CoreStanding::Unmeasured => BatteryState::Unmeasured,
        CoreStanding::TimedOut => BatteryState::TimedOut,
        CoreStanding::Skipped if matches!(gate_id, GateId::RepoShape) => {
            BatteryState::NotApplicable
        }
        CoreStanding::Skipped => BatteryState::ExplicitSkip,
    }
}

const fn toolchain_state(outcome: ArmOutcome) -> BatteryState {
    match outcome {
        ArmOutcome::Passed => BatteryState::Passed,
        ArmOutcome::ToolUnavailable => BatteryState::Unavailable,
        ArmOutcome::UnsupportedVersion | ArmOutcome::Unsupported => BatteryState::Unsupported,
        ArmOutcome::NotApplicable => BatteryState::NotApplicable,
        ArmOutcome::Excluded => BatteryState::Excluded,
        ArmOutcome::Timeout => BatteryState::TimedOut,
        ArmOutcome::Findings
        | ArmOutcome::Cancelled
        | ArmOutcome::ParseFailure
        | ArmOutcome::NonzeroAnalysis => BatteryState::Failed,
    }
}

const fn hygiene_state(outcome: HygieneOutcome) -> BatteryState {
    match outcome {
        HygieneOutcome::Passed => BatteryState::Passed,
        HygieneOutcome::NotApplicable => BatteryState::NotApplicable,
        HygieneOutcome::Excluded => BatteryState::Excluded,
        HygieneOutcome::Unavailable => BatteryState::Unavailable,
        HygieneOutcome::Unsupported => BatteryState::Unsupported,
        HygieneOutcome::Unmeasured => BatteryState::Unmeasured,
        HygieneOutcome::Timeout => BatteryState::TimedOut,
        HygieneOutcome::Findings | HygieneOutcome::ParseFailure | HygieneOutcome::Failed => {
            BatteryState::Failed
        }
    }
}

/// Fold one arm's per-metric states into the single state the battery reports.
///
/// An arm carries one row per measured path, so one file's own state sits
/// beside its neighbours' measurements: a Markdown source that cannot be read
/// is that file's evidence and not the whole arm's. The fold therefore settles
/// by precedence rather than by agreement.
///
/// A proven violation outranks an unread file, because the violation is proven
/// whatever else went unread. An unread file in turn outranks a clean
/// measurement, because part of the arm was never established and `Passed`
/// would claim that it was — the same reason acquisition refuses to score an
/// unreadable source as zero.
///
/// A clean measurement in turn outranks `NotApplicable`, which is not a gap at
/// all: it says there was nothing here to measure, so it may only speak for the
/// arm when it speaks for every row. Letting it outrank a measurement would let
/// one inapplicable path report the arm as skipped, and mint skip provenance
/// over neighbours that really were measured.
///
/// Two *different* unestablished states are still refused. They name different
/// missing things, and one arm reports one reason, so there is no honest way to
/// carry both. Every row is inspected before the state is chosen, so that
/// refusal does not depend on the order the rows happen to arrive in.
fn metric_state(report: &StructuralArmEvidence) -> Result<BatteryState, BatteryError> {
    let mut measured = false;
    let mut failed = false;
    let mut inapplicable = false;
    let mut unestablished: Option<BatteryState> = None;
    for metric in report.metrics() {
        match metric_terminal(metric) {
            BatteryState::Failed => failed = true,
            BatteryState::Passed => measured = true,
            BatteryState::NotApplicable => inapplicable = true,
            state => match unestablished {
                Some(previous) if previous != state => {
                    return Err(BatteryError::new(
                        "metric arm mixes incompatible terminal evidence states",
                    ));
                }
                _ => unestablished = Some(state),
            },
        }
    }
    match (failed, unestablished, measured, inapplicable) {
        (true, ..) => Ok(BatteryState::Failed),
        (false, Some(state), ..) => Ok(state),
        (false, None, true, _) => Ok(BatteryState::Passed),
        (false, None, false, true) => Ok(BatteryState::NotApplicable),
        (false, None, false, false) => {
            Err(BatteryError::new("metric arm evidence must not be empty"))
        }
    }
}

pub(super) fn metric_terminal(evidence: &MetricEvidence) -> BatteryState {
    match evidence.state() {
        MetricState::Measured { measurement } if over_bound(evidence.unit(), measurement) => {
            BatteryState::Failed
        }
        MetricState::Measured { .. } => BatteryState::Passed,
        MetricState::Unavailable { .. } => BatteryState::Unavailable,
        MetricState::Unsupported { .. } => BatteryState::Unsupported,
        MetricState::Unmeasured { .. } => BatteryState::Unmeasured,
        MetricState::Timeout { .. } => BatteryState::TimedOut,
        MetricState::Failed { .. } => BatteryState::Failed,
        MetricState::NotApplicable { .. } => BatteryState::NotApplicable,
    }
}

/// Whether one completed measurement broke the bound its unit or rule sets.
///
/// The unit is consulted first, and decides alone for a unit whose count is
/// itself the finding. Reaching the threshold for those would report every case
/// mismatch, absent doc target, and magic-number regression in the tree as a
/// pass, which is the one result the steps they answer for never give. The two
/// Markdown-arm measurements are forbidden a threshold outright; a magic
/// measurement is required to carry one, because there the threshold records
/// which literals were examined rather than how many regressions are allowed.
///
/// A latch breach a pending sweep task holds is that task's to repair, so it
/// informs instead of refusing, as a peer-held bound does.
fn over_bound(unit: MetricUnit, measurement: &MetricMeasurement) -> bool {
    if unit.admits_only_zero() {
        return measurement.value() > 0;
    }
    measurement
        .latch()
        .is_some_and(|latch| latch.over_limit() && latch.covered().is_none())
        || measurement
            .threshold()
            .is_some_and(|threshold| measurement.value() > threshold)
}

/// Whether one completed measurement broke its latch limit while a pending
/// sweep task holds it.
fn held_breach(measurement: &MetricMeasurement) -> bool {
    measurement
        .latch()
        .is_some_and(|latch| latch.over_limit() && latch.covered().is_some())
}

/// Breaching rows one failed arm reason names before it counts the rest.
///
/// A reason is one line a person reads at a terminal after `reason=`, and a
/// file-shape or complexity arm usually breaks on a handful of rows, so a
/// handful is named in full. Past that the line stops being a reason and
/// becomes a listing, which the canonical report already carries row by row;
/// the count of what was left off is stated rather than implied so a reader
/// cannot mistake the named rows for the whole breach.
const METRIC_REASON_CLAUSE_LIMIT: usize = 6;

/// The reason for whichever state the fold above settled on.
///
/// The row that carries that state is the one asked for the words, rather than
/// the arm's first row. Since an arm may hold a measurement beside a file that
/// was never read, the first row is often a measurement, which knows nothing
/// about why the arm did not complete.
///
/// A failed arm has no single carrier: every row that broke its bound is a
/// reason in its own right, so the arm names each one rather than reporting
/// that some unnamed measurement somewhere exceeded some unnamed bound.
fn metric_reason(report: &StructuralArmEvidence) -> Result<String, BatteryError> {
    let state = metric_state(report)?;
    if state == BatteryState::Passed {
        return Ok(held_reason(report).unwrap_or_else(|| {
            "all declared structural measurements completed within threshold".to_owned()
        }));
    }
    if state == BatteryState::Failed {
        return Ok(breach_reason(report));
    }
    let carrier = report
        .metrics()
        .iter()
        .find(|metric| metric_terminal(metric) == state)
        .ok_or_else(|| BatteryError::new("metric arm evidence must not be empty"))?;
    Ok(match carrier.state() {
        MetricState::Unavailable { reason }
        | MetricState::Unmeasured { reason }
        | MetricState::Timeout { reason, .. }
        | MetricState::Failed { reason, .. }
        | MetricState::NotApplicable { reason }
        | MetricState::Unsupported { reason, .. } => reason.clone(),
        MetricState::Measured { .. } => {
            unreachable!("measured rows settle on passed or failed, both handled above")
        }
    })
}

/// Name every measurement that broke its bound: which row, what it measured,
/// and the limit it was held to.
///
/// A row that failed in its own right says why, and says it first; the
/// breaching measurements follow in the arm's canonical row order so the same
/// evidence always reads the same way. The fold above settles on `Failed` only
/// when at least one row is failed or over bound, so the clause list is never
/// empty.
fn breach_reason(report: &StructuralArmEvidence) -> String {
    let mut clauses: Vec<String> = report
        .metrics()
        .iter()
        .find_map(|metric| match metric.state() {
            MetricState::Failed { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .into_iter()
        .collect();
    let breaches: Vec<(&MetricEvidence, &MetricMeasurement)> = report
        .metrics()
        .iter()
        .filter_map(|metric| match metric.state() {
            MetricState::Measured { measurement } if over_bound(metric.unit(), measurement) => {
                Some((metric, &**measurement))
            }
            _ => None,
        })
        .collect();
    clauses.extend(
        breaches
            .iter()
            .take(METRIC_REASON_CLAUSE_LIMIT)
            .map(|(metric, measurement)| breach_clause(metric, measurement)),
    );
    let undisplayed = breaches.len().saturating_sub(METRIC_REASON_CLAUSE_LIMIT);
    if undisplayed > 0 {
        clauses.push(format!("and {undisplayed} more over bound"));
    }
    clauses.join("; ")
}

/// Name every breach a pending sweep task holds, so a passing arm still says
/// which debt it passed over and who owns its repair.
fn held_reason(report: &StructuralArmEvidence) -> Option<String> {
    let held: Vec<String> = report
        .metrics()
        .iter()
        .filter_map(|metric| match metric.state() {
            MetricState::Measured { measurement } if held_breach(measurement) => {
                Some(breach_clause(metric, measurement))
            }
            _ => None,
        })
        .collect();
    (!held.is_empty()).then(|| {
        format!(
            "every breach is held by a pending sweep task: {}",
            held.join("; ")
        )
    })
}

/// One breaching row as `METRIC KEY measured VALUE UNIT over LIMIT`, followed
/// by the latch's origin when the current change did not create it.
fn breach_clause(metric: &MetricEvidence, measurement: &MetricMeasurement) -> String {
    let mut clause = format!(
        "{} {} measured {} {} over {}",
        metric.metric().as_str(),
        metric_key(metric.key()),
        measurement.value(),
        metric.unit().as_str(),
        breach_limit(metric.unit(), measurement)
    );
    if let Some(inherited) = measurement.latch().and_then(|latch| latch.inherited()) {
        clause.push_str(&format!(" ({inherited})"));
    }
    if let Some(task) = measurement.latch().and_then(|latch| latch.covered()) {
        clause.push_str(&format!(" held by sweep task {task}"));
    }
    clause
}

/// The bound a breaching measurement crossed, consulted in the order
/// [`over_bound`] consults it.
///
/// A unit that admits only zero is bounded at zero whatever else the row
/// carries: a magic measurement's threshold is the examine bar, not an
/// allowance, and reading it here as the limit would print a bound the row was
/// never held to. Otherwise the latch that tripped names its active limit, and
/// failing that the threshold the value exceeded. Every row this is asked about
/// broke one of those, so the closing fallback is never reached.
fn breach_limit(unit: MetricUnit, measurement: &MetricMeasurement) -> u64 {
    if unit.admits_only_zero() {
        return 0;
    }
    if let Some(latch) = measurement.latch().filter(|latch| latch.over_limit()) {
        return latch.active_limit();
    }
    measurement
        .threshold()
        .filter(|threshold| measurement.value() > *threshold)
        .unwrap_or(0)
}

/// Spell one metric key the way every battery rendering spells it.
pub(super) fn metric_key(key: &MetricKey) -> String {
    match key {
        MetricKey::Arm => "arm".to_owned(),
        MetricKey::Path(path) => format!("path={}", quoted(path)),
        MetricKey::Function(function) => format!(
            "path={} function={}",
            quoted(function.path()),
            quoted(function.function())
        ),
    }
}

pub(super) fn quoted(value: &str) -> String {
    serde_json::to_string(value).expect("validated battery text always serializes as JSON text")
}

fn hygiene_reason(report: &HygieneMeasurement) -> Result<String, BatteryError> {
    if report.outcome == HygieneOutcome::Excluded {
        let exclusion = report.evidence.iter().find_map(|evidence| match evidence {
            AnalysisEvidence::Exclusion(exclusion) => Some(exclusion),
            _ => None,
        });
        return exclusion
            .map(|evidence| evidence.reason.clone())
            .ok_or_else(|| BatteryError::new("excluded hygiene result lacks exclusion evidence"));
    }
    Ok(report.reason.clone())
}

fn hygiene_provenance(report: &HygieneMeasurement) -> Result<ResultProvenance, BatteryError> {
    let provenance = report.evidence.iter().find_map(|evidence| match evidence {
        AnalysisEvidence::LanguageApplicability {
            applicable: false,
            provenance,
            ..
        } => Some(provenance),
        AnalysisEvidence::Exclusion(exclusion) => Some(&exclusion.provenance),
        _ => None,
    });
    let provenance = provenance
        .ok_or_else(|| BatteryError::new("hygiene skip lacks declaration-owned provenance"))?;
    provenance_from_hygiene(provenance)
}

fn provenance_from_hygiene(
    provenance: &HygieneProvenance,
) -> Result<ResultProvenance, BatteryError> {
    let evidence = ResultProvenance {
        release: provenance.release.clone(),
        source: provenance.document.clone(),
        field: vec![provenance.layer.clone(), provenance.field.clone()],
    };
    evidence.validate()?;
    Ok(evidence)
}
