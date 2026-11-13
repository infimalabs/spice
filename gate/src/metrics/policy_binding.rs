use std::error::Error;
use std::fmt;

use super::declaration::{MetricId, STRUCTURAL_METRICS};
use super::evidence::{MetricMeasurement, ParserEvidence, RuleBinding};
use crate::policy::{
    BoundName, Policy, PolicyContext, ResolvedBound, ResolvedMagic, SelectionProvenance,
};

/// Resolved named/global rule consumed by one bounded structural metric.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedMetricRule {
    metric: MetricId,
    base_limit: u64,
    ratchet_limit: u64,
    unlimited: bool,
    binding: RuleBinding,
}

impl ResolvedMetricRule {
    #[must_use]
    pub const fn metric(&self) -> MetricId {
        self.metric
    }

    #[must_use]
    pub const fn base_limit(&self) -> u64 {
        self.base_limit
    }

    #[must_use]
    pub const fn ratchet_limit(&self) -> u64 {
        self.ratchet_limit
    }

    #[must_use]
    pub const fn unlimited(&self) -> bool {
        self.unlimited
    }

    #[must_use]
    pub const fn binding(&self) -> &RuleBinding {
        &self.binding
    }

    /// Whether an edited value inherited debt beyond the configured flex ceiling.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub(crate) fn changed_committed_ratchet_breach(
        &self,
        committed: Option<u64>,
        value: u64,
    ) -> bool {
        committed.is_some_and(|committed| {
            !self.unlimited && committed > self.ratchet_limit && committed != value
        })
    }

    /// Whether byte-distinct authored content inherited debt above its strict base limit.
    ///
    /// `Some` is already the caller's proof that current bytes differ from the
    /// committed document. Character counts may remain equal across that edit.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub(crate) fn changed_committed_base_breach(&self, committed: Option<u64>) -> bool {
        committed.is_some_and(|committed| !self.unlimited && committed > self.base_limit)
    }
}

/// Resolved threshold and selection provenance for magic-number scanning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedMagicMetric {
    threshold: u64,
    binding: RuleBinding,
}

impl ResolvedMagicMetric {
    #[must_use]
    pub const fn threshold(&self) -> u64 {
        self.threshold
    }

    #[must_use]
    pub const fn binding(&self) -> &RuleBinding {
        &self.binding
    }

    /// Bind one parsed regression count to the exact rule that selected its
    /// threshold.
    #[must_use]
    pub fn measurement(&self, value: u64, parser: ParserEvidence) -> MetricMeasurement {
        MetricMeasurement::new(value, parser)
            .with_rule(self.binding.clone())
            .with_threshold(self.threshold)
    }
}

/// Resolve one of the five bounded metrics through the sole current policy.
///
/// # Errors
///
/// Missing rule dimensions and arithmetic refusals remain typed errors. A
/// caller cannot obtain global limits by omitting context required by a named
/// rule.
pub fn resolve_metric_rule(
    policy: &Policy,
    metric: MetricId,
    context: &PolicyContext,
) -> Result<ResolvedMetricRule, PolicyBindingError> {
    let bound_name = bound_name(metric).ok_or(PolicyBindingError::UnboundedMetric(metric))?;
    let resolved = policy.resolve_bound(bound_name, context).map_err(|error| {
        PolicyBindingError::Resolution {
            metric,
            reason: error.to_string(),
        }
    })?;
    build_bound(metric, resolved)
}

/// Resolve the per-path magic threshold through the sole current policy.
pub fn resolve_magic_metric(
    policy: &Policy,
    context: &PolicyContext,
) -> Result<ResolvedMagicMetric, PolicyBindingError> {
    let resolved =
        policy
            .resolve_magic(context)
            .map_err(|error| PolicyBindingError::Resolution {
                metric: MetricId::MagicNumberRegressions,
                reason: error.to_string(),
            })?;
    build_magic(resolved)
}

fn build_bound(
    metric: MetricId,
    resolved: ResolvedBound,
) -> Result<ResolvedMetricRule, PolicyBindingError> {
    Ok(ResolvedMetricRule {
        metric,
        base_limit: resolved.limit(),
        ratchet_limit: resolved.ratchet_limit(),
        unlimited: resolved.unlimited(),
        binding: binding(metric, resolved.selection())?,
    })
}

fn build_magic(resolved: ResolvedMagic) -> Result<ResolvedMagicMetric, PolicyBindingError> {
    Ok(ResolvedMagicMetric {
        threshold: resolved.examine_threshold(),
        binding: binding(MetricId::MagicNumberRegressions, resolved.selection())?,
    })
}

fn binding(
    metric: MetricId,
    selection: Option<&SelectionProvenance>,
) -> Result<RuleBinding, PolicyBindingError> {
    let field = declaration(metric).policy_field();
    let binding = match selection {
        Some(selection) => RuleBinding::new(
            field,
            Some(selection.rule_id().to_string()),
            format!(
                "{}#{}",
                selection.source().location(),
                selection.source().field()
            ),
            selection.match_basis(),
        ),
        None => RuleBinding::new(
            field,
            None::<String>,
            "compiled policy globals",
            "no named rule selected",
        ),
    };
    binding.map_err(|error| PolicyBindingError::Binding {
        metric,
        reason: error.to_string(),
    })
}

fn declaration(metric: MetricId) -> super::MetricDeclaration {
    STRUCTURAL_METRICS
        .into_iter()
        .find(|declaration| declaration.id() == metric)
        .expect("every structural metric has one declaration")
}

const fn bound_name(metric: MetricId) -> Option<BoundName> {
    match metric {
        MetricId::FileLines => Some(BoundName::FileLoc),
        MetricId::FileBytes => Some(BoundName::FileBytes),
        MetricId::RoutineCcn => Some(BoundName::RoutineCcn),
        MetricId::RoutineLength => Some(BoundName::RoutineLength),
        MetricId::RepositoryDocumentCharacters => Some(BoundName::RepoTruthDocChars),
        MetricId::MagicNumberRegressions
        | MetricId::MarkdownLinkInvalidTargets
        | MetricId::RustDocMarkdownTargetAbsences => None,
    }
}

/// Why current rule resolution could not bind one structural measurement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PolicyBindingError {
    UnboundedMetric(MetricId),
    Resolution { metric: MetricId, reason: String },
    Binding { metric: MetricId, reason: String },
}

impl fmt::Display for PolicyBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnboundedMetric(metric) => {
                write!(
                    formatter,
                    "{} is not a bounded policy metric",
                    metric.as_str()
                )
            }
            Self::Resolution { metric, reason } => {
                write!(formatter, "{} policy resolution: {reason}", metric.as_str())
            }
            Self::Binding { metric, reason } => {
                write!(formatter, "{} policy binding: {reason}", metric.as_str())
            }
        }
    }
}

impl Error for PolicyBindingError {}
