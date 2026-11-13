use std::error::Error;
use std::fmt;

use super::declaration::metric_parser_declaration;
use super::{
    EvidenceError, EvidenceState, LatchBinding, LatchLimits, LatchOutcome, MetricEvidence,
    MetricId, MetricKey, MetricMeasurement, ParserEvidence, ResolvedMetricRule,
};
use crate::state::{
    ActorInputs, BoundDecision, BoundError, BoundRule, KeyError, LatchEntry, LatchKind,
    LatchObservation, StateDecision, StateKey,
};

/// One bounded structural measurement compiled for current state reconciliation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetricStateBinding {
    metric: MetricId,
    key: MetricKey,
    value: u64,
    parser: ParserEvidence,
    rule: ResolvedMetricRule,
    entry: LatchEntry,
    bound: BoundRule,
}

impl MetricStateBinding {
    /// Bind one parsed value and resolved policy rule to the sole current
    /// durable-state key and bound vocabulary.
    pub fn new(
        metric: MetricId,
        key: MetricKey,
        value: u64,
        parser: ParserEvidence,
        rule: ResolvedMetricRule,
    ) -> Result<Self, StateBindingError> {
        if rule.metric() != metric {
            return Err(StateBindingError::RuleMetric {
                expected: metric,
                observed: rule.metric(),
            });
        }
        let expected_parser = metric_parser_declaration(metric);
        if !parser.matches_declaration(expected_parser) {
            return Err(StateBindingError::Parser(format!(
                "{} parser evidence does not match {}",
                parser.parser().as_str(),
                expected_parser.id().as_str()
            )));
        }
        let kind = latch_kind(metric).ok_or(StateBindingError::UnboundedMetric(metric))?;
        let state_key = state_key(&key)?;
        let entry = LatchEntry::new(kind, state_key).map_err(StateBindingError::Key)?;
        let bound = BoundRule::new(
            rule_identity(&rule),
            metric.as_str(),
            rule.base_limit(),
            rule.ratchet_limit(),
            rule.unlimited(),
        )
        .map_err(StateBindingError::Bound)?;
        Ok(Self {
            metric,
            key,
            value,
            parser,
            rule,
            entry,
            bound,
        })
    }

    #[must_use]
    pub const fn metric(&self) -> MetricId {
        self.metric
    }

    #[must_use]
    pub const fn key(&self) -> &MetricKey {
        &self.key
    }

    #[must_use]
    pub const fn entry(&self) -> &LatchEntry {
        &self.entry
    }

    /// Produce the exact observation consumed by [`crate::state::CurrentState`].
    pub fn observation(
        &self,
        actor_inputs: ActorInputs,
    ) -> Result<LatchObservation, StateBindingError> {
        LatchObservation::new(
            self.entry.clone(),
            self.bound.clone(),
            self.value,
            actor_inputs,
        )
        .map_err(StateBindingError::Bound)
    }

    /// Decide this bound directly against one known latch state.
    ///
    /// [`Self::observation`] hands the same rule to
    /// [`crate::state::CurrentState`], which reads `latched` from the ledger it
    /// is about to rewrite and returns the decision inside a transaction. An
    /// acquisition that only measures needs the decision without the rewrite,
    /// so it reads the latch itself and decides here. Both routes call the one
    /// [`BoundRule`] compiled above, so neither can compute a limit the other
    /// would not.
    ///
    #[must_use]
    pub fn decide(&self, latched: bool) -> BoundDecision {
        self.bound.decide(self.value, latched)
    }

    /// Decide this bound against the run's judged latches and retain the exact
    /// transaction observation beside the decision used by acquisition
    /// evidence.
    ///
    /// `committed_over` holds the subject to its base limit when the committed
    /// value already broke its bound, so the debt reads as inherited even
    /// though the ledger never latched it.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn observe_decision(
        &self,
        latches: &crate::state::JudgedLatches,
        committed_over: bool,
        actor_inputs: &ActorInputs,
    ) -> Result<(BoundDecision, LatchObservation), StateBindingError> {
        let latched = latches.contains(&self.entry) || committed_over;
        let observation = self
            .observation(actor_inputs.clone())?
            .with_prior_latched(latched);
        Ok((
            latches.attribute(&self.entry, self.decide(latched)),
            observation,
        ))
    }

    /// Convert a reconciled current-state decision into complete metric evidence.
    pub fn evidence(&self, decision: &StateDecision) -> Result<MetricEvidence, StateBindingError> {
        if decision.entry() != &self.entry {
            return Err(StateBindingError::Decision(
                "state decision entry does not match the compiled metric key".to_owned(),
            ));
        }
        self.measured(decision.bound())
    }

    /// Convert a bound decided by [`Self::decide`] into complete metric evidence.
    ///
    /// [`Self::evidence`] is this same conversion for the reconciling route,
    /// which owns a [`StateDecision`] because it rewrote the ledger. An arm that
    /// only measures holds the [`BoundDecision`] [`Self::decide`] handed back
    /// and has no state decision to present, so it converts from that instead.
    /// Both arrive at the one private `measurement` step and attach the same
    /// rule and latch bindings to the same measured value, so the row an arm
    /// reports and the row the state transaction would report cannot disagree
    /// about the limit that value was judged against.
    ///
    /// # Errors
    ///
    /// Refuses a decision that disagrees with this binding's metric, value, or
    /// resolved rule, and refuses evidence its own declaration rejects.
    pub fn measured(&self, decision: &BoundDecision) -> Result<MetricEvidence, StateBindingError> {
        self.evidence_with_cover(decision, None)
    }

    /// Convert a decision acquired against the run's judged latches, held by
    /// the pending sweep task that covers its breach.
    ///
    /// # Errors
    ///
    /// Refuses what [`Self::measured`] refuses.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn judged(
        &self,
        latches: &crate::state::JudgedLatches,
        decision: &BoundDecision,
    ) -> Result<MetricEvidence, StateBindingError> {
        self.evidence_with_cover(decision, latches.cover(&self.entry, decision))
    }

    fn evidence_with_cover(
        &self,
        decision: &BoundDecision,
        cover: Option<&wire::TaskHandle>,
    ) -> Result<MetricEvidence, StateBindingError> {
        let measurement = self.measurement(decision, cover)?;
        MetricEvidence::new(
            self.metric,
            self.key.clone(),
            EvidenceState::measured(measurement),
        )
        .map_err(StateBindingError::Evidence)
    }

    fn measurement(
        &self,
        decision: &BoundDecision,
        cover: Option<&wire::TaskHandle>,
    ) -> Result<MetricMeasurement, StateBindingError> {
        self.validate_decision(decision)?;
        let latch = LatchBinding::new(
            self.entry.kind().as_str(),
            LatchLimits {
                base: decision.base_limit(),
                ratchet: decision.elastic_limit(),
                active: decision.active_limit(),
                unlimited: decision.unlimited(),
            },
            LatchOutcome {
                over_limit: decision.over_limit(),
                over_base: decision.over_base(),
                ratchet_breach: decision.elastic_breach(),
            },
            decision.latch_transition().as_str(),
        )
        .and_then(|latch| latch.with_inheritance(decision.inherited().cloned()))
        .and_then(|latch| latch.with_cover(cover.cloned()))
        .map_err(StateBindingError::Evidence)?;
        Ok(MetricMeasurement::new(self.value, self.parser.clone())
            .with_rule(self.rule.binding().clone())
            .with_latch(latch))
    }

    fn validate_decision(&self, decision: &BoundDecision) -> Result<(), StateBindingError> {
        let matches = decision.rule() == rule_identity(&self.rule)
            && decision.metric() == self.metric.as_str()
            && decision.measured() == self.value
            && decision.base_limit() == self.rule.base_limit()
            && decision.elastic_limit() == self.rule.ratchet_limit()
            && decision.unlimited() == self.rule.unlimited();
        if matches {
            Ok(())
        } else {
            Err(StateBindingError::Decision(
                "bound decision disagrees with its metric, value, or resolved rule".to_owned(),
            ))
        }
    }
}

fn state_key(key: &MetricKey) -> Result<StateKey, StateBindingError> {
    match key {
        MetricKey::Path(path) => StateKey::path(path).map_err(StateBindingError::Key),
        MetricKey::Function(function) => {
            StateKey::function(function.path(), function.function()).map_err(StateBindingError::Key)
        }
        MetricKey::Arm => Err(StateBindingError::KeyShape(
            "a bounded measurement requires a concrete path or function key".to_owned(),
        )),
    }
}

fn rule_identity(rule: &ResolvedMetricRule) -> String {
    rule.binding().rule_id().map_or_else(
        || format!("global:{}", rule.binding().field()),
        str::to_owned,
    )
}

const fn latch_kind(metric: MetricId) -> Option<LatchKind> {
    match metric {
        MetricId::FileLines => Some(LatchKind::FileLoc),
        MetricId::FileBytes => Some(LatchKind::FileBytes),
        MetricId::RoutineCcn => Some(LatchKind::ComplexityCcn),
        MetricId::RoutineLength => Some(LatchKind::ComplexityLength),
        MetricId::RepositoryDocumentCharacters => Some(LatchKind::RepoDocChars),
        MetricId::MagicNumberRegressions
        | MetricId::MarkdownLinkInvalidTargets
        | MetricId::RustDocMarkdownTargetAbsences => None,
    }
}

/// Why a parsed structural value could not cross current bounded state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StateBindingError {
    UnboundedMetric(MetricId),
    RuleMetric {
        expected: MetricId,
        observed: MetricId,
    },
    KeyShape(String),
    Parser(String),
    Key(KeyError),
    Bound(BoundError),
    Decision(String),
    Evidence(EvidenceError),
}

impl fmt::Display for StateBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnboundedMetric(metric) => {
                write!(formatter, "{} has no durable bound", metric.as_str())
            }
            Self::RuleMetric { expected, observed } => write!(
                formatter,
                "{} cannot consume the resolved {} rule",
                expected.as_str(),
                observed.as_str()
            ),
            Self::KeyShape(reason) | Self::Parser(reason) | Self::Decision(reason) => {
                formatter.write_str(reason)
            }
            Self::Key(error) => write!(formatter, "state key: {error}"),
            Self::Bound(error) => write!(formatter, "bounded state: {error}"),
            Self::Evidence(error) => write!(formatter, "metric evidence: {error}"),
        }
    }
}

impl Error for StateBindingError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Key(error) => Some(error),
            Self::Bound(error) => Some(error),
            Self::Evidence(error) => Some(error),
            _ => None,
        }
    }
}
