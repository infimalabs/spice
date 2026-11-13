use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use wire::{Digest, Domain};

use super::{BatteryError, valid_text};
use crate::RulePolicy;
use crate::manifest::OriginLayer;
use crate::state::{
    ActorResolution, ActorSource, BoundDecision, LatchEntry, LatchKind, StateKey, StateLedger,
};

/// Explicit selected-tree scope supplied to every arm.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BatteryScope {
    name: String,
    provenance: String,
}

impl BatteryScope {
    /// Bind a named scope to the source that selected it.
    pub fn new(
        name: impl Into<String>,
        provenance: impl Into<String>,
    ) -> Result<Self, BatteryError> {
        let scope = Self {
            name: name.into(),
            provenance: provenance.into(),
        };
        if !valid_text(&scope.name) || !valid_text(&scope.provenance) {
            return Err(BatteryError::new(
                "battery scope name and provenance must be non-empty single-line text",
            ));
        }
        Ok(scope)
    }

    /// Stable scope name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Exact source that selected this scope.
    #[must_use]
    pub fn provenance(&self) -> &str {
        &self.provenance
    }
}

/// One resolved configuration value and its winning source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationEvidence {
    key: String,
    value_digest: Digest,
    release: String,
    layer: OriginLayer,
    document: String,
    field: Vec<String>,
}

impl ConfigurationEvidence {
    /// Bind a resolved value identity to its exact layered provenance.
    pub fn new(
        key: impl Into<String>,
        value_digest: Digest,
        release: impl Into<String>,
        layer: OriginLayer,
        document: impl Into<String>,
        field: Vec<String>,
    ) -> Result<Self, BatteryError> {
        let evidence = Self {
            key: key.into(),
            value_digest,
            release: release.into(),
            layer,
            document: document.into(),
            field,
        };
        evidence.validate()?;
        Ok(evidence)
    }

    pub(super) fn validate(&self) -> Result<(), BatteryError> {
        if !valid_text(&self.key)
            || !valid_text(&self.release)
            || !valid_text(&self.document)
            || self.field.is_empty()
            || self.field.iter().any(|part| !valid_text(part))
        {
            return Err(BatteryError::new(
                "configuration evidence requires a key, release, document, and exact field path",
            ));
        }
        Ok(())
    }

    /// Resolved configuration key.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Identity of the resolved value.
    #[must_use]
    pub const fn value_digest(&self) -> &Digest {
        &self.value_digest
    }

    /// Release that understood the winning document grammar.
    #[must_use]
    pub fn release(&self) -> &str {
        &self.release
    }

    /// Winning source layer.
    #[must_use]
    pub const fn layer(&self) -> OriginLayer {
        self.layer
    }

    /// Exact source document.
    #[must_use]
    pub fn document(&self) -> &str {
        &self.document
    }

    /// Exact source field path.
    #[must_use]
    pub fn field(&self) -> &[String] {
        &self.field
    }
}

/// Complete discovery evidence for one declared executable.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "state")]
pub enum BatteryToolEvidence {
    Identified {
        name: String,
        program: String,
        version: String,
        executable_digest: Digest,
    },
    Unavailable {
        name: String,
        program: String,
        reason: String,
    },
}

impl BatteryToolEvidence {
    /// Retain one admitted executable identity.
    pub fn identified(
        name: impl Into<String>,
        program: impl Into<String>,
        version: impl Into<String>,
        executable_digest: Digest,
    ) -> Result<Self, BatteryError> {
        let evidence = Self::Identified {
            name: name.into(),
            program: program.into(),
            version: version.into(),
            executable_digest,
        };
        evidence.validate()?;
        Ok(evidence)
    }

    /// Retain explicit tool discovery failure.
    pub fn unavailable(
        name: impl Into<String>,
        program: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<Self, BatteryError> {
        let evidence = Self::Unavailable {
            name: name.into(),
            program: program.into(),
            reason: reason.into(),
        };
        evidence.validate()?;
        Ok(evidence)
    }

    /// Stable declaration name.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Identified { name, .. } | Self::Unavailable { name, .. } => name,
        }
    }

    /// Sole program name requested by the declaration.
    #[must_use]
    pub fn program(&self) -> &str {
        match self {
            Self::Identified { program, .. } | Self::Unavailable { program, .. } => program,
        }
    }

    /// Identified version text, absent when discovery failed.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        match self {
            Self::Identified { version, .. } => Some(version),
            Self::Unavailable { .. } => None,
        }
    }

    /// Identified executable identity, absent when discovery failed.
    #[must_use]
    pub const fn executable_digest(&self) -> Option<&Digest> {
        match self {
            Self::Identified {
                executable_digest, ..
            } => Some(executable_digest),
            Self::Unavailable { .. } => None,
        }
    }

    pub(super) fn validate(&self) -> Result<(), BatteryError> {
        let (name, program, detail) = match self {
            Self::Identified {
                name,
                program,
                version,
                ..
            } => (name, program, version),
            Self::Unavailable {
                name,
                program,
                reason,
            } => (name, program, reason),
        };
        if !valid_text(name) || !valid_text(program) || !valid_text(detail) {
            return Err(BatteryError::new(
                "tool name, program, and version/reason must be non-empty single-line text",
            ));
        }
        Ok(())
    }
}

/// Selected actor retained without hiding its resolution source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActorEvidence {
    source: String,
    value: String,
}

impl From<&ActorResolution> for ActorEvidence {
    fn from(actor: &ActorResolution) -> Self {
        Self {
            source: actor_source(actor.source()).to_owned(),
            value: actor.value().to_owned(),
        }
    }
}

impl ActorEvidence {
    /// Input class that supplied the actor.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Exact resolved actor.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    fn validate_actor(&self) -> Result<(), BatteryError> {
        if self.source != "active-actor" || !valid_text(&self.value) {
            return Err(BatteryError::new(
                "battery actor must be an explicit active-actor identity",
            ));
        }
        Ok(())
    }
}

/// Complete retained calculation behind one elastic decision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BoundEvidence {
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
    latch_transition: String,
}

impl From<&BoundDecision> for BoundEvidence {
    fn from(bound: &BoundDecision) -> Self {
        Self {
            rule: bound.rule().to_owned(),
            metric: bound.metric().to_owned(),
            measured: bound.measured(),
            base_limit: bound.base_limit(),
            elastic_limit: bound.elastic_limit(),
            active_limit: bound.active_limit(),
            unlimited: bound.unlimited(),
            over_limit: bound.over_limit(),
            over_base: bound.over_base(),
            elastic_breach: bound.elastic_breach(),
            latch_transition: bound.latch_transition().as_str().to_owned(),
        }
    }
}

impl BoundEvidence {
    /// Stable rule name.
    #[must_use]
    pub fn rule(&self) -> &str {
        &self.rule
    }

    /// Stable metric name the rule bounded.
    #[must_use]
    pub fn metric(&self) -> &str {
        &self.metric
    }

    /// Exact measured value.
    #[must_use]
    pub const fn measured(&self) -> u64 {
        self.measured
    }

    /// Strict base limit.
    #[must_use]
    pub const fn base_limit(&self) -> u64 {
        self.base_limit
    }

    /// Policy-compiled elastic limit.
    #[must_use]
    pub const fn elastic_limit(&self) -> u64 {
        self.elastic_limit
    }

    /// Active limit selected after latch state.
    #[must_use]
    pub const fn active_limit(&self) -> u64 {
        self.active_limit
    }

    /// Whether the measured value consumed peer-claim authority.
    #[must_use]
    pub const fn over_base(&self) -> bool {
        self.over_base
    }

    /// Resulting sticky-state transition.
    #[must_use]
    pub fn latch_transition(&self) -> &str {
        &self.latch_transition
    }

    pub(super) fn validate(&self) -> Result<(), BatteryError> {
        if !self.valid_shape() {
            return Err(BatteryError::new(
                "bound evidence has invalid identity, limits, or transition",
            ));
        }
        let prior_latched = matches!(self.latch_transition.as_str(), "held" | "healed");
        let expected_active = if prior_latched {
            self.base_limit
        } else {
            self.elastic_limit
        };
        if self.active_limit != expected_active
            || !self.transition_flags_match()
            || !self.measured_flags_match()
            || (self.over_limit && !self.over_base)
            || (self.unlimited && (self.over_limit || self.over_base || self.elastic_breach))
        {
            return Err(BatteryError::new(
                "bound evidence flags disagree with its limits or latch transition",
            ));
        }
        Ok(())
    }

    fn valid_shape(&self) -> bool {
        valid_text(&self.rule)
            && valid_text(&self.metric)
            && matches!(
                self.latch_transition.as_str(),
                "clear" | "latched" | "held" | "healed"
            )
            && self.elastic_limit >= self.base_limit
            && self.active_limit >= self.base_limit
    }

    fn transition_flags_match(&self) -> bool {
        match self.latch_transition.as_str() {
            "clear" => !self.over_limit && !self.elastic_breach,
            "latched" => self.over_limit && self.over_base && self.elastic_breach,
            "held" => self.over_limit && self.over_base,
            "healed" => !self.over_limit && !self.over_base && !self.elastic_breach,
            _ => false,
        }
    }

    fn measured_flags_match(&self) -> bool {
        let enabled = !self.unlimited;
        self.over_limit == (enabled && self.measured > self.active_limit)
            && self.over_base == (enabled && self.measured > self.base_limit)
            && self.elastic_breach == (enabled && self.measured > self.elastic_limit)
    }
}

/// One exact sticky-state key visible to the run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LatchEvidence {
    kind: String,
    path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    function: Option<String>,
}

impl From<&LatchEntry> for LatchEvidence {
    fn from(entry: &LatchEntry) -> Self {
        Self {
            kind: latch_kind(entry.kind()).to_owned(),
            path: entry.key().repo_path().to_owned(),
            function: entry.key().function_name().map(str::to_owned),
        }
    }
}

impl LatchEvidence {
    /// Owning sticky-state class.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Canonical repository path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Function component for path/function keys.
    #[must_use]
    pub fn function(&self) -> Option<&str> {
        self.function.as_deref()
    }

    pub(super) fn validate(&self) -> Result<(), BatteryError> {
        if !matches!(
            self.kind.as_str(),
            "file-loc" | "file-bytes" | "complexity-ccn" | "complexity-length" | "repo-doc-chars"
        ) || !valid_text(&self.path)
            || self
                .function
                .as_deref()
                .is_some_and(|value| !valid_text(value))
        {
            return Err(BatteryError::new(
                "latch evidence has an invalid kind, path, or function",
            ));
        }
        let kind = match self.kind.as_str() {
            "file-loc" => LatchKind::FileLoc,
            "file-bytes" => LatchKind::FileBytes,
            "complexity-ccn" => LatchKind::ComplexityCcn,
            "complexity-length" => LatchKind::ComplexityLength,
            "repo-doc-chars" => LatchKind::RepoDocChars,
            _ => unreachable!("kind spelling was checked above"),
        };
        let key = match &self.function {
            Some(function) => StateKey::function(&self.path, function.clone()),
            None => StateKey::path(&self.path),
        }
        .map_err(|error| BatteryError::new(format!("invalid latch key: {error}")))?;
        let entry = LatchEntry::new(kind, key)
            .map_err(|error| BatteryError::new(format!("invalid latch key shape: {error}")))?;
        if entry.key().repo_path() != self.path {
            return Err(BatteryError::new("latch path must already be canonical"));
        }
        Ok(())
    }
}

/// Immutable, serializable evidence shared by all 25 result positions.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BatteryContext {
    source: Digest,
    actor: ActorEvidence,
    scope: BatteryScope,
    configuration: Vec<ConfigurationEvidence>,
    policy: Digest,
    state: Digest,
    bounds: Vec<BoundEvidence>,
    latches: Vec<LatchEvidence>,
    tools: Vec<BatteryToolEvidence>,
}

impl BatteryContext {
    /// Immutable source tree identity.
    #[must_use]
    pub const fn source(&self) -> &Digest {
        &self.source
    }

    /// Explicit resolved actor.
    #[must_use]
    pub const fn actor(&self) -> &ActorEvidence {
        &self.actor
    }

    /// Explicit selected scope.
    #[must_use]
    pub const fn scope(&self) -> &BatteryScope {
        &self.scope
    }

    /// Resolved configuration values and winning provenance.
    #[must_use]
    pub fn configuration(&self) -> &[ConfigurationEvidence] {
        &self.configuration
    }

    /// Identity of the complete compiled policy document.
    #[must_use]
    pub const fn policy(&self) -> &Digest {
        &self.policy
    }

    /// Identity of the complete state ledger.
    #[must_use]
    pub const fn state(&self) -> &Digest {
        &self.state
    }

    /// Complete bound decisions supplied to this run.
    #[must_use]
    pub fn bounds(&self) -> &[BoundEvidence] {
        &self.bounds
    }

    /// Complete sticky-state inventory supplied to this run.
    #[must_use]
    pub fn latches(&self) -> &[LatchEvidence] {
        &self.latches
    }

    /// Complete discovered tool identity evidence.
    #[must_use]
    pub fn tools(&self) -> &[BatteryToolEvidence] {
        &self.tools
    }

    pub(super) fn validate(&self) -> Result<(), BatteryError> {
        self.actor.validate_actor()?;
        if !valid_text(&self.scope.name) || !valid_text(&self.scope.provenance) {
            return Err(BatteryError::new(
                "battery context has invalid actor or scope",
            ));
        }
        let mut configuration_keys = BTreeSet::new();
        for configuration in &self.configuration {
            configuration.validate()?;
            if !configuration_keys.insert(&configuration.key) {
                return Err(BatteryError::new(format!(
                    "battery context repeats configuration key {:?}",
                    configuration.key
                )));
            }
        }
        for bound in &self.bounds {
            bound.validate()?;
        }
        let mut latch_keys = BTreeSet::new();
        for latch in &self.latches {
            latch.validate()?;
            if !latch_keys.insert((&latch.kind, &latch.path, &latch.function)) {
                return Err(BatteryError::new("battery context repeats a latch key"));
            }
        }
        let mut tool_names = BTreeSet::new();
        for tool in &self.tools {
            tool.validate()?;
            if !tool_names.insert(tool.name()) {
                return Err(BatteryError::new(format!(
                    "battery context repeats tool evidence {:?}",
                    tool.name()
                )));
            }
        }
        Ok(())
    }
}

/// Borrowed authorities supplied unchanged to every arm execution.
///
/// The record is built where the authorities live; [`Self::snapshot`]
/// validates the complete set before any run, context, or executor binding
/// can read it.
pub struct BatteryInputs<'a> {
    pub source: &'a Digest,
    pub actor: &'a ActorResolution,
    pub scope: &'a BatteryScope,
    pub configuration: &'a [ConfigurationEvidence],
    pub policy: &'a RulePolicy,
    pub state: &'a StateLedger,
    pub bounds: &'a [BoundDecision],
    pub tools: &'a [BatteryToolEvidence],
}

impl BatteryInputs<'_> {
    /// The validated context these inputs describe.
    ///
    /// # Errors
    ///
    /// Refuses inputs whose policy cannot be encoded or whose evidence breaks
    /// a [`BatteryContext`] invariant.
    pub(super) fn snapshot(&self) -> Result<BatteryContext, BatteryError> {
        let policy = wire::encode(self.policy.document()).map_err(|error| {
            BatteryError::new(format!("cannot encode compiled policy: {error}"))
        })?;
        let context = BatteryContext {
            source: self.source.clone(),
            actor: ActorEvidence::from(self.actor),
            scope: self.scope.clone(),
            configuration: self.configuration.to_vec(),
            policy: Digest::compute(Domain::GatePolicy, &policy),
            state: self.state.digest(),
            bounds: self.bounds.iter().map(BoundEvidence::from).collect(),
            latches: self
                .state
                .latches()
                .iter()
                .map(LatchEvidence::from)
                .collect(),
            tools: self.tools.to_vec(),
        };
        context.validate()?;
        Ok(context)
    }
}

fn actor_source(source: ActorSource) -> &'static str {
    match source {
        ActorSource::ActiveActor => "active-actor",
    }
}

fn latch_kind(kind: LatchKind) -> &'static str {
    match kind {
        LatchKind::FileLoc => "file-loc",
        LatchKind::FileBytes => "file-bytes",
        LatchKind::ComplexityCcn => "complexity-ccn",
        LatchKind::ComplexityLength => "complexity-length",
        LatchKind::RepoDocChars => "repo-doc-chars",
    }
}
