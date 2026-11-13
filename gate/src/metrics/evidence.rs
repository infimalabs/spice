use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::declaration::{
    KeyShape, MetricDeclaration, MetricId, MetricUnit, ParserDeclaration, ParserId,
    STRUCTURAL_ARMS, STRUCTURAL_METRICS, metric_parser_declaration,
};
use crate::manifest::GateId;
use crate::state::LatchInheritance;
use wire::TaskHandle;

/// The sole report schema for compiled structural metrics.
pub const STRUCTURAL_METRIC_REPORT_VERSION: u64 = 1;

/// Canonical path/function key.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FunctionKey {
    path: String,
    function: String,
}

impl FunctionKey {
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub fn function(&self) -> &str {
        &self.function
    }
}

/// Complete metric key vocabulary.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case", tag = "shape", content = "key")]
pub enum MetricKey {
    /// Whole-arm evidence used only when no concrete key can be measured.
    Arm,
    /// One canonical repository-relative path.
    Path(String),
    /// One canonical path/function pair.
    Function(FunctionKey),
}

impl MetricKey {
    /// Construct one canonical path key.
    pub fn path(path: impl Into<String>) -> Result<Self, EvidenceError> {
        Ok(Self::Path(canonical_path(path.into())?))
    }

    /// Construct one canonical path/function key.
    pub fn function(
        path: impl Into<String>,
        function: impl Into<String>,
    ) -> Result<Self, EvidenceError> {
        let path = canonical_path(path.into())?;
        let function = function.into();
        if function.trim().is_empty() || function.contains(['\r', '\n']) {
            return Err(EvidenceError::InvalidKey(
                "function must be non-empty single-line text".to_owned(),
            ));
        }
        Ok(Self::Function(FunctionKey { path, function }))
    }
}

/// Exact parser identity retained with a successful measurement.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "mode")]
pub enum ParserEvidence {
    Native { parser: ParserId },
}

impl ParserEvidence {
    /// Compiled parser that interpreted the input.
    #[must_use]
    pub const fn parser(&self) -> ParserId {
        match self {
            Self::Native { parser } => *parser,
        }
    }

    pub(crate) fn matches_declaration(&self, declaration: ParserDeclaration) -> bool {
        self.parser() == declaration.id()
    }
}

/// Typed policy-selection evidence consumed by one measurement.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuleBinding {
    field: String,
    rule_id: Option<String>,
    source: String,
    match_basis: String,
}

impl RuleBinding {
    /// Bind one resolved global or named rule.
    pub fn new(
        field: impl Into<String>,
        rule_id: Option<impl Into<String>>,
        source: impl Into<String>,
        match_basis: impl Into<String>,
    ) -> Result<Self, EvidenceError> {
        let binding = Self {
            field: field.into(),
            rule_id: rule_id.map(Into::into),
            source: source.into(),
            match_basis: match_basis.into(),
        };
        if !valid_text(&binding.field)
            || !valid_text(&binding.source)
            || !valid_text(&binding.match_basis)
            || binding.rule_id.as_ref().is_some_and(|id| !valid_text(id))
        {
            return Err(EvidenceError::InvalidBinding(
                "rule binding fields must be non-empty single-line text".to_owned(),
            ));
        }
        Ok(binding)
    }

    #[must_use]
    pub fn field(&self) -> &str {
        &self.field
    }

    #[must_use]
    pub fn rule_id(&self) -> Option<&str> {
        self.rule_id.as_deref()
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub fn match_basis(&self) -> &str {
        &self.match_basis
    }
}

/// Exact durable-bound and latch evidence consumed by one measurement.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LatchBinding {
    kind: String,
    base_limit: u64,
    ratchet_limit: u64,
    active_limit: u64,
    unlimited: bool,
    over_limit: bool,
    over_base: bool,
    ratchet_breach: bool,
    transition: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inherited: Option<LatchInheritance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    covered: Option<TaskHandle>,
}

/// The limits one latch decision measured against.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LatchLimits {
    pub base: u64,
    pub ratchet: u64,
    pub active: u64,
    pub unlimited: bool,
}

/// How one measurement fell against its latch limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LatchOutcome {
    pub over_limit: bool,
    pub over_base: bool,
    pub ratchet_breach: bool,
}

impl LatchBinding {
    /// Bind a completed bounded-state decision.
    pub fn new(
        kind: impl Into<String>,
        limits: LatchLimits,
        outcome: LatchOutcome,
        transition: impl Into<String>,
    ) -> Result<Self, EvidenceError> {
        let binding = Self {
            kind: kind.into(),
            base_limit: limits.base,
            ratchet_limit: limits.ratchet,
            active_limit: limits.active,
            unlimited: limits.unlimited,
            over_limit: outcome.over_limit,
            over_base: outcome.over_base,
            ratchet_breach: outcome.ratchet_breach,
            transition: transition.into(),
            inherited: None,
            covered: None,
        };
        binding.validate()?;
        Ok(binding)
    }

    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
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
    pub const fn active_limit(&self) -> u64 {
        self.active_limit
    }

    #[must_use]
    pub const fn unlimited(&self) -> bool {
        self.unlimited
    }

    #[must_use]
    pub const fn over_limit(&self) -> bool {
        self.over_limit
    }

    #[must_use]
    pub const fn over_base(&self) -> bool {
        self.over_base
    }

    #[must_use]
    pub const fn ratchet_breach(&self) -> bool {
        self.ratchet_breach
    }

    #[must_use]
    pub fn transition(&self) -> &str {
        &self.transition
    }

    /// Origin of a held latch the current change did not create.
    #[must_use]
    pub const fn inherited(&self) -> Option<&LatchInheritance> {
        self.inherited.as_ref()
    }

    /// The pending sweep task that holds this breach, as a peer holds a flex
    /// claim.
    #[must_use]
    pub const fn covered(&self) -> Option<&TaskHandle> {
        self.covered.as_ref()
    }

    /// Hold this breach for the pending sweep task that covers it.
    ///
    /// # Errors
    ///
    /// Refuses a cover on a decision inside its limit: only a breach is held.
    pub fn with_cover(mut self, covered: Option<TaskHandle>) -> Result<Self, EvidenceError> {
        self.covered = covered;
        self.validate()?;
        Ok(self)
    }

    /// Record what the ledger knows of a held latch's origin.
    ///
    /// # Errors
    ///
    /// Refuses inheritance on any transition but `held`: only a latch that
    /// predates the current change can be inherited debt.
    pub fn with_inheritance(
        mut self,
        inherited: Option<LatchInheritance>,
    ) -> Result<Self, EvidenceError> {
        self.inherited = inherited;
        self.validate()?;
        Ok(self)
    }
    fn validate(&self) -> Result<(), EvidenceError> {
        if !valid_text(&self.kind) || !valid_text(&self.transition) {
            return Err(EvidenceError::InvalidBinding(
                "latch kind and transition must be non-empty single-line text".to_owned(),
            ));
        }
        if self.ratchet_limit < self.base_limit || self.active_limit < self.base_limit {
            return Err(EvidenceError::InvalidBinding(
                "latch limits must preserve base <= ratchet and active".to_owned(),
            ));
        }
        self.validate_transition()?;
        if self.covered.is_some() && !self.over_limit {
            return Err(EvidenceError::InvalidBinding(
                "only a latch over its limit can be held by a sweep task".to_owned(),
            ));
        }
        if self.unlimited && (self.over_limit || self.over_base || self.ratchet_breach) {
            return Err(EvidenceError::InvalidBinding(
                "an unlimited latch decision cannot report a bound breach".to_owned(),
            ));
        }
        Ok(())
    }

    /// The recorded transition agrees with the active limit, the outcome
    /// flags, and whether the latch may carry inherited origin evidence.
    fn validate_transition(&self) -> Result<(), EvidenceError> {
        if !matches!(
            self.transition.as_str(),
            "clear" | "latched" | "held" | "healed"
        ) {
            return Err(EvidenceError::InvalidBinding(
                "latch transition must be clear, latched, held, or healed".to_owned(),
            ));
        }
        let prior_latched = matches!(self.transition.as_str(), "held" | "healed");
        let expected_active = if prior_latched {
            self.base_limit
        } else {
            self.ratchet_limit
        };
        if self.active_limit != expected_active {
            return Err(EvidenceError::InvalidBinding(
                "active latch limit disagrees with the recorded transition".to_owned(),
            ));
        }
        let flags_match = match self.transition.as_str() {
            "clear" => !self.over_limit && !self.ratchet_breach,
            "latched" => self.over_limit && self.over_base && self.ratchet_breach,
            "held" => self.over_limit && self.over_base,
            "healed" => !self.over_limit && !self.over_base && !self.ratchet_breach,
            _ => false,
        };
        if !flags_match || (self.over_limit && !self.over_base) {
            return Err(EvidenceError::InvalidBinding(
                "latch outcome flags disagree with the recorded transition".to_owned(),
            ));
        }
        if self.inherited.is_some() && self.transition != "held" {
            return Err(EvidenceError::InvalidBinding(
                "only a held latch carries inherited origin evidence".to_owned(),
            ));
        }
        Ok(())
    }
}

/// The most findings one measurement carries into the report.
///
/// A count alone tells a reader which file to search; a finding tells them
/// where to look inside it. Carrying every one would let a single pathological
/// source set the size of the whole report, so a measurement carries a bounded
/// sample and [`MetricMeasurement::value`] keeps the true total. The two are
/// therefore not interchangeable, and a reader comparing them can tell that a
/// listing was shortened without being told separately.
pub const MEASUREMENT_FINDING_LIMIT: usize = 16;

/// One source-located reason a measurement reads the way it does.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct MetricFinding {
    /// Selected-tree-relative path of the source that carries the finding.
    pub path: String,
    /// One-based line within that source.
    pub line: u64,
    /// The target as written, which is the string a reader must go find.
    pub target: String,
    /// The spelling the target should have used, where one is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
}

/// One trusted number with every declaration/state input that produced it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetricMeasurement {
    value: u64,
    parser: ParserEvidence,
    rule: Option<RuleBinding>,
    latch: Option<LatchBinding>,
    threshold: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    findings: Vec<MetricFinding>,
}

impl MetricMeasurement {
    #[must_use]
    pub fn new(value: u64, parser: ParserEvidence) -> Self {
        Self {
            value,
            parser,
            rule: None,
            latch: None,
            threshold: None,
            findings: Vec::new(),
        }
    }

    /// Attach the located findings behind this value, bounded as they are taken.
    ///
    /// The bound is [`MEASUREMENT_FINDING_LIMIT`] and it is applied here, at the
    /// only door into the field, so no caller can widen it by constructing the
    /// vector differently. Truncation keeps the leading entries, which are the
    /// scanners' canonical order, so a shortened listing still begins where a
    /// reader would begin reading the file.
    #[must_use]
    pub fn with_findings(mut self, mut findings: Vec<MetricFinding>) -> Self {
        findings.truncate(MEASUREMENT_FINDING_LIMIT);
        self.findings = findings;
        self
    }

    #[must_use]
    pub fn with_rule(mut self, rule: RuleBinding) -> Self {
        self.rule = Some(rule);
        self
    }

    #[must_use]
    pub fn with_latch(mut self, latch: LatchBinding) -> Self {
        self.latch = Some(latch);
        self
    }

    #[must_use]
    pub const fn with_threshold(mut self, threshold: u64) -> Self {
        self.threshold = Some(threshold);
        self
    }

    #[must_use]
    pub const fn value(&self) -> u64 {
        self.value
    }

    #[must_use]
    pub const fn parser(&self) -> &ParserEvidence {
        &self.parser
    }

    #[must_use]
    pub const fn rule(&self) -> Option<&RuleBinding> {
        self.rule.as_ref()
    }

    #[must_use]
    pub const fn latch(&self) -> Option<&LatchBinding> {
        self.latch.as_ref()
    }

    #[must_use]
    pub const fn threshold(&self) -> Option<u64> {
        self.threshold
    }

    #[must_use]
    pub fn findings(&self) -> &[MetricFinding] {
        &self.findings
    }
}

/// Complete acquisition state; absence is not a member of this vocabulary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "state")]
pub enum EvidenceState {
    Measured {
        measurement: Box<MetricMeasurement>,
    },
    Unavailable {
        reason: String,
    },
    Unsupported {
        source_field: String,
        reason: String,
        recovery: String,
    },
    Unmeasured {
        reason: String,
    },
    Timeout {
        milliseconds: u64,
        reason: String,
    },
    Failed {
        reason: String,
        exit_code: Option<i32>,
    },
    NotApplicable {
        reason: String,
    },
}

impl EvidenceState {
    /// Bind one trusted measurement without exposing representation sizing.
    #[must_use]
    pub fn measured(measurement: MetricMeasurement) -> Self {
        Self::Measured {
            measurement: Box::new(measurement),
        }
    }

    /// Exact refusal for a selected input this release cannot represent.
    pub fn unsupported(
        source_field: impl Into<String>,
        reason: impl Into<String>,
        recovery: impl Into<String>,
    ) -> Self {
        Self::Unsupported {
            source_field: source_field.into(),
            reason: reason.into(),
            recovery: recovery.into(),
        }
    }

    /// Only a declared non-applicability state skips execution.
    #[must_use]
    pub const fn is_skip(&self) -> bool {
        matches!(self, Self::NotApplicable { .. })
    }
}

/// One explicit state for one metric key.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetricEvidence {
    metric: MetricId,
    unit: MetricUnit,
    key: MetricKey,
    state: EvidenceState,
}

impl MetricEvidence {
    /// Validate and bind evidence to its compiled declaration.
    pub fn new(
        metric: MetricId,
        key: MetricKey,
        state: EvidenceState,
    ) -> Result<Self, EvidenceError> {
        let declaration = metric_declaration(metric);
        validate_key(declaration, &key, &state)?;
        validate_state(declaration, &state)?;
        Ok(Self {
            metric,
            unit: declaration.unit(),
            key,
            state,
        })
    }

    #[must_use]
    pub const fn metric(&self) -> MetricId {
        self.metric
    }

    #[must_use]
    pub const fn unit(&self) -> MetricUnit {
        self.unit
    }

    #[must_use]
    pub const fn key(&self) -> &MetricKey {
        &self.key
    }

    #[must_use]
    pub const fn state(&self) -> &EvidenceState {
        &self.state
    }
}

/// Canonical evidence for one of the four named arms.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StructuralArmEvidence {
    gate_id: GateId,
    metrics: Vec<MetricEvidence>,
}

impl StructuralArmEvidence {
    /// Canonicalize one complete arm measurement set.
    pub fn new(gate_id: GateId, mut metrics: Vec<MetricEvidence>) -> Result<Self, EvidenceError> {
        let declaration = STRUCTURAL_ARMS
            .into_iter()
            .find(|arm| arm.id() == gate_id)
            .ok_or(EvidenceError::ForeignGate(gate_id))?;
        metrics.sort_by(|left, right| {
            left.metric
                .cmp(&right.metric)
                .then_with(|| left.key.cmp(&right.key))
        });
        reject_duplicate_evidence(&metrics)?;
        require_arm_metrics(declaration.metrics(), &metrics)?;
        require_paired_keys(gate_id, &metrics)?;
        Ok(Self { gate_id, metrics })
    }

    #[must_use]
    pub const fn gate_id(&self) -> GateId {
        self.gate_id
    }

    #[must_use]
    pub fn metrics(&self) -> &[MetricEvidence] {
        &self.metrics
    }

    pub(crate) fn validate(&self) -> Result<(), EvidenceError> {
        let mut metrics = Vec::with_capacity(self.metrics.len());
        for metric in &self.metrics {
            metrics.push(MetricEvidence::new(
                metric.metric,
                metric.key.clone(),
                metric.state.clone(),
            )?);
        }
        let expected = Self::new(self.gate_id, metrics)?;
        if expected != *self {
            return Err(EvidenceError::InvalidBinding(
                "retained structural arm disagrees with its compiled declaration".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Complete report: four ordered arms plus the shared repo-doc metric.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetricReport {
    version: u64,
    arms: Vec<StructuralArmEvidence>,
    repository_documents: Vec<MetricEvidence>,
}

impl MetricReport {
    /// Admit a total report without accepting partial arm or document evidence.
    pub fn new(
        mut arms: Vec<StructuralArmEvidence>,
        mut repository_documents: Vec<MetricEvidence>,
    ) -> Result<Self, EvidenceError> {
        arms.sort_by_key(|arm| arm_position(arm.gate_id));
        require_all_arms(&arms)?;
        canonicalize_repository_documents(&mut repository_documents)?;
        Ok(Self {
            version: STRUCTURAL_METRIC_REPORT_VERSION,
            arms,
            repository_documents,
        })
    }

    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    #[must_use]
    pub fn arms(&self) -> &[StructuralArmEvidence] {
        &self.arms
    }

    #[must_use]
    pub fn repository_documents(&self) -> &[MetricEvidence] {
        &self.repository_documents
    }

    /// Deterministic JSON bytes with one trailing newline.
    #[must_use]
    pub fn canonical_json(&self) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(self).expect("metric reports contain only JSON values");
        bytes.push(b'\n');
        bytes
    }
}

/// Why evidence could not be admitted as one total structural report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EvidenceError {
    ForeignGate(GateId),
    DuplicateGate(GateId),
    MissingGate(GateId),
    MissingMetric(MetricId),
    ForeignMetric { gate: GateId, metric: MetricId },
    DuplicateEvidence { metric: MetricId, key: MetricKey },
    InvalidKey(String),
    InvalidState { metric: MetricId, reason: String },
    InvalidBinding(String),
    UnpairedKeys { gate: GateId, reason: String },
}

impl fmt::Display for EvidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ForeignGate(gate) => write!(formatter, "{} is not a structural arm", gate),
            Self::DuplicateGate(gate) => write!(formatter, "{} appears twice", gate),
            Self::MissingGate(gate) => write!(formatter, "{} has no evidence", gate),
            Self::MissingMetric(metric) => write!(formatter, "{} has no evidence", metric.as_str()),
            Self::ForeignMetric { gate, metric } => {
                write!(formatter, "{} does not declare {}", gate, metric.as_str())
            }
            Self::DuplicateEvidence { metric, key } => {
                write!(formatter, "{} repeats key {key:?}", metric.as_str())
            }
            Self::InvalidKey(reason) | Self::InvalidBinding(reason) => formatter.write_str(reason),
            Self::InvalidState { metric, reason } => {
                write!(
                    formatter,
                    "{} evidence is invalid: {reason}",
                    metric.as_str()
                )
            }
            Self::UnpairedKeys { gate, reason } => {
                write!(formatter, "{gate} metric keys differ: {reason}")
            }
        }
    }
}

impl Error for EvidenceError {}

fn metric_declaration(metric: MetricId) -> MetricDeclaration {
    STRUCTURAL_METRICS
        .into_iter()
        .find(|declaration| declaration.id() == metric)
        .expect("every public metric identity has one declaration")
}

fn validate_key(
    declaration: MetricDeclaration,
    key: &MetricKey,
    state: &EvidenceState,
) -> Result<(), EvidenceError> {
    if matches!(key, MetricKey::Arm) {
        return if matches!(state, EvidenceState::Measured { .. }) {
            Err(EvidenceError::InvalidKey(
                "measured evidence requires a concrete path or function key".to_owned(),
            ))
        } else {
            Ok(())
        };
    }
    let valid = matches!(
        (declaration.key(), key),
        (KeyShape::Path, MetricKey::Path(_)) | (KeyShape::PathFunction, MetricKey::Function(_))
    );
    if valid {
        Ok(())
    } else {
        Err(EvidenceError::InvalidKey(format!(
            "{} requires a {:?} key",
            declaration.id().as_str(),
            declaration.key()
        )))
    }
}

fn validate_state(
    declaration: MetricDeclaration,
    state: &EvidenceState,
) -> Result<(), EvidenceError> {
    match state {
        EvidenceState::Measured { measurement } => validate_measurement(declaration, measurement),
        EvidenceState::Unavailable { reason }
        | EvidenceState::Unmeasured { reason }
        | EvidenceState::NotApplicable { reason }
        | EvidenceState::Failed { reason, .. }
        | EvidenceState::Timeout { reason, .. } => require_reason(declaration.id(), reason),
        EvidenceState::Unsupported {
            source_field,
            reason,
            recovery,
        } => {
            for value in [source_field, reason, recovery] {
                require_reason(declaration.id(), value)?;
            }
            Ok(())
        }
    }
}

fn validate_measurement(
    declaration: MetricDeclaration,
    measurement: &MetricMeasurement,
) -> Result<(), EvidenceError> {
    if !measurement
        .parser
        .matches_declaration(metric_parser_declaration(declaration.id()))
    {
        return invalid_state(
            declaration.id(),
            "parser evidence does not match declaration",
        );
    }
    validate_findings(declaration, measurement)?;
    match declaration.id() {
        MetricId::FileLines
        | MetricId::FileBytes
        | MetricId::RoutineCcn
        | MetricId::RoutineLength
        | MetricId::RepositoryDocumentCharacters => validate_bounded(declaration, measurement),
        MetricId::MagicNumberRegressions => validate_magic(declaration, measurement),
        MetricId::MarkdownLinkInvalidTargets | MetricId::RustDocMarkdownTargetAbsences => {
            validate_zero_tolerance(declaration, measurement)
        }
    }
}

/// Findings are a bounded sample of the value, never a second count of it.
///
/// The bound is re-checked here rather than trusted from
/// [`MetricMeasurement::with_findings`], because a decoded report reaches this
/// field through `Deserialize` and never passes that builder at all.
fn validate_findings(
    declaration: MetricDeclaration,
    measurement: &MetricMeasurement,
) -> Result<(), EvidenceError> {
    let findings = &measurement.findings;
    if findings.len() > MEASUREMENT_FINDING_LIMIT {
        return invalid_state(
            declaration.id(),
            "measurement carries more findings than the admitted bound",
        );
    }
    if u64::try_from(findings.len()).unwrap_or(u64::MAX) > measurement.value {
        return invalid_state(
            declaration.id(),
            "measurement carries more findings than the value they explain",
        );
    }
    if findings.iter().any(|finding| {
        finding.line == 0
            || !valid_text(&finding.path)
            || !valid_text(&finding.target)
            || finding
                .expected
                .as_deref()
                .is_some_and(|value| !valid_text(value))
    }) {
        return invalid_state(
            declaration.id(),
            "a finding must name a one-based line with single-line path and target text",
        );
    }
    Ok(())
}

fn validate_bounded(
    declaration: MetricDeclaration,
    measurement: &MetricMeasurement,
) -> Result<(), EvidenceError> {
    let rule = measurement
        .rule
        .as_ref()
        .ok_or_else(|| state_error(declaration.id(), "missing resolved rule evidence"))?;
    if rule.field != declaration.policy_field() {
        return invalid_state(
            declaration.id(),
            "resolved rule field does not match declaration",
        );
    }
    let latch = measurement
        .latch
        .as_ref()
        .ok_or_else(|| state_error(declaration.id(), "missing latch decision evidence"))?;
    if Some(latch.kind()) != declaration.latch_kind() {
        return invalid_state(declaration.id(), "latch kind does not match declaration");
    }
    latch.validate()?;
    let enabled = !latch.unlimited();
    if latch.over_limit() != (enabled && measurement.value > latch.active_limit())
        || latch.over_base() != (enabled && measurement.value > latch.base_limit())
        || latch.ratchet_breach() != (enabled && measurement.value > latch.ratchet_limit())
    {
        return invalid_state(
            declaration.id(),
            "latch flags disagree with the retained measurement value",
        );
    }
    if measurement.threshold.is_some() {
        return invalid_state(declaration.id(), "bounded metric carries a magic threshold");
    }
    Ok(())
}

fn validate_magic(
    declaration: MetricDeclaration,
    measurement: &MetricMeasurement,
) -> Result<(), EvidenceError> {
    let rule = measurement
        .rule
        .as_ref()
        .ok_or_else(|| state_error(declaration.id(), "missing magic rule evidence"))?;
    if rule.field != declaration.policy_field() {
        return invalid_state(
            declaration.id(),
            "magic rule field does not match declaration",
        );
    }
    if measurement.threshold.is_none() || measurement.latch.is_some() {
        return invalid_state(
            declaration.id(),
            "magic measurement requires threshold and forbids latch evidence",
        );
    }
    Ok(())
}

fn validate_zero_tolerance(
    declaration: MetricDeclaration,
    measurement: &MetricMeasurement,
) -> Result<(), EvidenceError> {
    if measurement.rule.is_some() || measurement.latch.is_some() || measurement.threshold.is_some()
    {
        return invalid_state(
            declaration.id(),
            "zero-tolerance measurement carries unrelated rule, latch, or threshold evidence",
        );
    }
    // A zero-tolerance value is a count of breaches and nothing else, so any
    // nonzero value is a refusal. Requiring a finding here is what makes the
    // report's naming structural: a breach that named nothing cannot be encoded
    // in the first place, rather than being encoded and merely unhelpful.
    if measurement.value > 0 && measurement.findings.is_empty() {
        return invalid_state(
            declaration.id(),
            "a zero-tolerance breach must name at least one finding",
        );
    }
    Ok(())
}

fn require_reason(metric: MetricId, reason: &str) -> Result<(), EvidenceError> {
    if valid_text(reason) {
        Ok(())
    } else {
        invalid_state(metric, "reason must be non-empty single-line text")
    }
}

fn invalid_state<T>(metric: MetricId, reason: &str) -> Result<T, EvidenceError> {
    Err(state_error(metric, reason))
}

fn state_error(metric: MetricId, reason: &str) -> EvidenceError {
    EvidenceError::InvalidState {
        metric,
        reason: reason.to_owned(),
    }
}

fn require_arm_metrics(
    expected: &[MetricId],
    evidence: &[MetricEvidence],
) -> Result<(), EvidenceError> {
    let gate = metric_declaration(expected[0]).owner();
    for row in evidence {
        if !expected.contains(&row.metric) {
            return Err(EvidenceError::ForeignMetric {
                gate,
                metric: row.metric,
            });
        }
    }
    for metric in expected {
        if !evidence.iter().any(|row| row.metric == *metric) {
            return Err(EvidenceError::MissingMetric(*metric));
        }
    }
    Ok(())
}

fn reject_duplicate_evidence(evidence: &[MetricEvidence]) -> Result<(), EvidenceError> {
    for pair in evidence.windows(2) {
        if pair[0].metric == pair[1].metric && pair[0].key == pair[1].key {
            return Err(EvidenceError::DuplicateEvidence {
                metric: pair[0].metric,
                key: pair[0].key.clone(),
            });
        }
    }
    Ok(())
}

fn require_paired_keys(gate: GateId, evidence: &[MetricEvidence]) -> Result<(), EvidenceError> {
    let paired = match gate {
        GateId::FileShape => Some((MetricId::FileLines, MetricId::FileBytes)),
        GateId::Complexity => Some((MetricId::RoutineCcn, MetricId::RoutineLength)),
        _ => None,
    };
    let Some((left, right)) = paired else {
        return Ok(());
    };
    let left = keys_for(left, evidence);
    let right = keys_for(right, evidence);
    if left == right {
        Ok(())
    } else {
        Err(EvidenceError::UnpairedKeys {
            gate,
            reason: format!("left={left:?} right={right:?}"),
        })
    }
}

fn keys_for(metric: MetricId, evidence: &[MetricEvidence]) -> BTreeSet<MetricKey> {
    evidence
        .iter()
        .filter(|row| row.metric == metric)
        .map(|row| row.key.clone())
        .collect()
}

fn require_all_arms(arms: &[StructuralArmEvidence]) -> Result<(), EvidenceError> {
    let mut seen = BTreeSet::new();
    for arm in arms {
        if !seen.insert(arm.gate_id) {
            return Err(EvidenceError::DuplicateGate(arm.gate_id));
        }
    }
    for declaration in STRUCTURAL_ARMS {
        if !seen.contains(&declaration.id()) {
            return Err(EvidenceError::MissingGate(declaration.id()));
        }
    }
    Ok(())
}

fn canonicalize_repository_documents(
    evidence: &mut Vec<MetricEvidence>,
) -> Result<(), EvidenceError> {
    evidence.sort_by(|left, right| left.key.cmp(&right.key));
    reject_duplicate_evidence(evidence)?;
    if evidence.is_empty() {
        return Err(EvidenceError::MissingMetric(
            MetricId::RepositoryDocumentCharacters,
        ));
    }
    for row in evidence {
        if row.metric != MetricId::RepositoryDocumentCharacters {
            return Err(EvidenceError::ForeignMetric {
                gate: GateId::RepoDocs,
                metric: row.metric,
            });
        }
    }
    Ok(())
}

fn arm_position(gate: GateId) -> usize {
    STRUCTURAL_ARMS
        .into_iter()
        .position(|arm| arm.id() == gate)
        .unwrap_or(usize::MAX)
}

fn canonical_path(path: String) -> Result<String, EvidenceError> {
    let invalid = path.is_empty()
        || path.starts_with('/')
        || path.ends_with('/')
        || path.contains('\\')
        || path.contains(['\r', '\n'])
        || path.split('/').any(|part| matches!(part, "" | "." | ".."));
    if invalid {
        Err(EvidenceError::InvalidKey(format!(
            "metric path {path:?} is not canonical repository-relative text"
        )))
    } else {
        Ok(path)
    }
}

fn valid_text(value: &str) -> bool {
    !value.trim().is_empty() && !value.contains(['\r', '\n'])
}
