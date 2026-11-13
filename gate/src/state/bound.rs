use super::*;

/// Candidate provenance and actor identity available to state reconciliation.
///
/// Operations requiring a real actor admit only the active actor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ActorInputs {
    #[serde(skip_serializing_if = "Option::is_none")]
    active_actor: Option<String>,
    candidate_path: String,
}

impl ActorInputs {
    /// Start provenance for one canonical repository-relative path.
    pub fn new(candidate_path: impl AsRef<str>) -> Result<Self, BoundError> {
        Ok(Self {
            active_actor: None,
            candidate_path: normalize_repo_path(candidate_path.as_ref()).map_err(|reason| {
                BoundError::InvalidPath {
                    path: candidate_path.as_ref().to_owned(),
                    reason,
                }
            })?,
        })
    }

    /// Record the active worktree actor when present.
    #[must_use]
    pub fn with_active_actor(mut self, value: impl Into<String>) -> Self {
        self.active_actor = non_empty(value);
        self
    }

    /// Canonical candidate path.
    #[must_use]
    pub fn candidate_path(&self) -> &str {
        &self.candidate_path
    }

    /// Active worktree actor candidate.
    #[must_use]
    pub fn active_actor(&self) -> Option<&str> {
        self.active_actor.as_deref()
    }

    /// Resolve a human/session actor, refusing provenance-only fallbacks.
    pub fn require_actor(&self) -> Result<ActorResolution, BoundError> {
        if let Some(value) = &self.active_actor {
            return Ok(ActorResolution::new(ActorSource::ActiveActor, value));
        }
        Err(BoundError::MissingActor)
    }
}

fn non_empty(value: impl Into<String>) -> Option<String> {
    let value = value.into();
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Which provenance input supplied a selected actor.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActorSource {
    ActiveActor,
}

/// One selected provenance value and its source.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ActorResolution {
    source: ActorSource,
    value: String,
}

impl ActorResolution {
    fn new(source: ActorSource, value: &str) -> Self {
        Self {
            source,
            value: value.to_owned(),
        }
    }

    /// Input class that supplied this value.
    #[must_use]
    pub const fn source(&self) -> ActorSource {
        self.source
    }

    /// Selected actor identity.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// One typed elastic bound before a measured value is considered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundRule {
    rule: String,
    metric: String,
    base_limit: u64,
    elastic_limit: u64,
    unlimited: bool,
}

impl BoundRule {
    /// Validate a complete rule and its configured limits.
    pub fn new(
        rule: impl Into<String>,
        metric: impl Into<String>,
        base_limit: u64,
        elastic_limit: u64,
        unlimited: bool,
    ) -> Result<Self, BoundError> {
        let rule = rule.into();
        let metric = metric.into();
        if rule.trim().is_empty() {
            return Err(BoundError::EmptyRule);
        }
        if metric.trim().is_empty() {
            return Err(BoundError::EmptyMetric);
        }
        if base_limit == 0 {
            return Err(BoundError::ZeroBaseLimit);
        }
        if elastic_limit < base_limit {
            return Err(BoundError::ElasticBelowBase {
                base_limit,
                elastic_limit,
            });
        }
        Ok(Self {
            rule: rule.trim().to_owned(),
            metric: metric.trim().to_owned(),
            base_limit,
            elastic_limit,
            unlimited,
        })
    }

    /// Evaluate one value in fixed calculation order.
    ///
    /// The policy-compiled elastic limit is selected first, then the active
    /// limit, enabled/over-limit, over-base, flex-breach, and finally latch
    /// transition.
    #[must_use]
    pub fn decide(&self, measured: u64, latched: bool) -> BoundDecision {
        let active_limit = if latched {
            self.base_limit
        } else {
            self.elastic_limit
        };
        let enabled = !self.unlimited;
        let over_limit = enabled && measured > active_limit;
        let over_base = enabled && measured > self.base_limit;
        let elastic_breach = enabled && measured > self.elastic_limit;
        let latch_transition = if latched && !over_base {
            LatchTransition::Healed
        } else if latched {
            LatchTransition::Held
        } else if elastic_breach {
            LatchTransition::Latched
        } else {
            LatchTransition::Clear
        };
        BoundDecision {
            rule: self.rule.clone(),
            metric: self.metric.clone(),
            measured,
            base_limit: self.base_limit,
            elastic_limit: self.elastic_limit,
            active_limit,
            unlimited: self.unlimited,
            over_limit,
            over_base,
            elastic_breach,
            latch_transition,
            inherited: None,
        }
    }
}

/// Every input and output behind one bound decision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BoundDecision {
    rule: String,
    metric: String,
    measured: u64,
    base_limit: u64,
    elastic_limit: u64,
    active_limit: u64,
    unlimited: bool,
    over_limit: bool,
    over_base: bool,
    elastic_breach: bool,
    pub(super) latch_transition: LatchTransition,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) inherited: Option<LatchInheritance>,
}

impl BoundDecision {
    /// Stable rule name.
    #[must_use]
    pub fn rule(&self) -> &str {
        &self.rule
    }

    /// Stable metric name.
    #[must_use]
    pub fn metric(&self) -> &str {
        &self.metric
    }

    /// Measured value.
    #[must_use]
    pub const fn measured(&self) -> u64 {
        self.measured
    }

    /// Base limit.
    #[must_use]
    pub const fn base_limit(&self) -> u64 {
        self.base_limit
    }

    /// Policy-compiled elastic limit.
    #[must_use]
    pub const fn elastic_limit(&self) -> u64 {
        self.elastic_limit
    }

    /// Active limit, base when latched and flex otherwise.
    #[must_use]
    pub const fn active_limit(&self) -> u64 {
        self.active_limit
    }

    /// Whether this rule disables all over-limit outcomes.
    #[must_use]
    pub const fn unlimited(&self) -> bool {
        self.unlimited
    }

    /// Whether the measured value exceeds the active limit.
    #[must_use]
    pub const fn over_limit(&self) -> bool {
        self.over_limit
    }

    /// Whether the measured value exceeds base while enabled.
    #[must_use]
    pub const fn over_base(&self) -> bool {
        self.over_base
    }

    /// Whether the measured value exceeds flex while enabled.
    #[must_use]
    pub const fn elastic_breach(&self) -> bool {
        self.elastic_breach
    }

    /// Resulting latch movement.
    #[must_use]
    pub const fn latch_transition(&self) -> LatchTransition {
        self.latch_transition
    }

    /// Origin of a held latch the current change did not create.
    #[must_use]
    pub const fn inherited(&self) -> Option<&LatchInheritance> {
        self.inherited.as_ref()
    }
}

/// How one observation changes sticky state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LatchTransition {
    Clear,
    Latched,
    Held,
    Healed,
}

impl LatchTransition {
    /// Stable report spelling of this transition.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::Latched => "latched",
            Self::Held => "held",
            Self::Healed => "healed",
        }
    }
}

/// A malformed bound or unresolved actor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BoundError {
    EmptyRule,
    EmptyMetric,
    ZeroBaseLimit,
    ElasticBelowBase { base_limit: u64, elastic_limit: u64 },
    MissingActor,
    InvalidPath { path: String, reason: String },
}

impl fmt::Display for BoundError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRule => formatter.write_str("bound rule must be non-empty"),
            Self::EmptyMetric => formatter.write_str("bound metric must be non-empty"),
            Self::ZeroBaseLimit => formatter.write_str("bound base limit must be positive"),
            Self::ElasticBelowBase {
                base_limit,
                elastic_limit,
            } => write!(
                formatter,
                "bound elastic limit {elastic_limit} must be at least base limit {base_limit}"
            ),
            Self::MissingActor => {
                formatter.write_str("rule requires actor identity; provide an active actor")
            }
            Self::InvalidPath { path, reason } => {
                write!(formatter, "invalid repository path {path:?}: {reason}")
            }
        }
    }
}

impl Error for BoundError {}
