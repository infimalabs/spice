//! Compiled declarations and deterministic evaluation for the core gate family.
//!
//! Acquisition is deliberately outside this module. Each arm receives a
//! complete observation bound to one immutable tree identity, so process loss,
//! timeout, and applicability decisions cross as explicit evidence rather than
//! being mistaken for a successful measurement.

#[cfg(not(target_arch = "wasm32"))]
mod acquire;
mod admission;
#[cfg(not(target_arch = "wasm32"))]
mod config_keys;
#[cfg(not(target_arch = "wasm32"))]
mod configuration_inventory;
mod declaration;
mod evaluate;
#[cfg(not(target_arch = "wasm32"))]
mod shape;

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};
use wire::{Digest, Phase, TaskHandle};

#[cfg(not(target_arch = "wasm32"))]
pub use acquire::{ActorCapture, ClaimCapture, CoreSource, acquire_core, git_subprocess_count};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use acquire::{acquire_core_arm, acquire_core_with_observations};
#[cfg(not(target_arch = "wasm32"))]
pub use config_keys::{CONFIG_KEY_DECLARATIONS, ConfigKeyDeclaration};
pub use declaration::{
    CoreArmDeclaration, CoreTimeoutContract, CoreToolContract, GitCommandContract,
    core_declarations,
};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use evaluate::evaluate_core_arm;
pub use evaluate::{evaluate_core, lacks_commit_authority};

use crate::GateId;
use crate::state::{BoundDecision, LatchInheritance};
use admission::{validate_observation, validate_repo_path};

/// Current core-family report schema.
pub const CORE_REPORT_VERSION: u64 = 1;

/// The core family in its fixed battery order.
pub const CORE_GATE_IDS: [GateId; 8] = [
    GateId::MergeIntegrity,
    GateId::PlanningPhase,
    GateId::RepoShape,
    GateId::Staging,
    GateId::RepoDocs,
    GateId::ConfigKeyValidity,
    GateId::ConfigFalseDisable,
    GateId::ConfigTrackedTrust,
];

/// A successfully acquired merge-integrity observation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", tag = "state")]
pub enum MergeIntegrityEvidence {
    /// No `MERGE_HEAD` exists.
    NoMerge,
    /// The index already differs from `HEAD`, so it cannot be an empty merge.
    StagedContent {
        head_tree: String,
        staged_tree: String,
    },
    /// The index equals `HEAD` and was compared with `git merge-tree`.
    Compared {
        head_tree: String,
        expected_tree: String,
        completion: MergeTreeCompletion,
        conflicted_paths: Vec<String>,
    },
}

/// Accepted completion class of `git merge-tree --write-tree`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeTreeCompletion {
    /// Exit zero with a clean computed tree.
    Clean,
    /// Exit one with a computed top tree and conflict report.
    Conflicted,
}

/// A successfully acquired active-claim observation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", tag = "state")]
pub enum PlanningPhaseEvidence {
    /// No active claim exists in this worktree.
    NoActiveClaim,
    /// The active task and its exact current workflow phase.
    Active { task: TaskHandle, phase: Phase },
}

/// One deterministic gate finding.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CoreFinding {
    code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    detail: String,
}

impl CoreFinding {
    /// Build one named finding, optionally attached to a canonical repo path.
    ///
    /// # Errors
    ///
    /// Refuses empty codes and details or a non-canonical path.
    pub fn new(
        code: impl Into<String>,
        path: Option<impl Into<String>>,
        detail: impl Into<String>,
    ) -> Result<Self, CoreInputError> {
        let finding = Self {
            code: code.into(),
            path: path.map(Into::into),
            detail: detail.into(),
        };
        if finding.code.trim().is_empty() || finding.detail.trim().is_empty() {
            return Err(CoreInputError::new(
                "core finding code and detail must be non-empty",
            ));
        }
        if let Some(path) = &finding.path {
            validate_repo_path(path)?;
        }
        Ok(finding)
    }

    /// Stable finding class.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Optional repository-relative path.
    #[must_use]
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// Deterministic diagnostic detail.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

/// Completed repository-shape findings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RepoShapeEvidence {
    findings: Vec<CoreFinding>,
}

impl RepoShapeEvidence {
    /// Canonicalize completed namespace, path-shape, and name-cluster findings.
    #[must_use]
    pub fn new(mut findings: Vec<CoreFinding>) -> Self {
        findings.sort();
        findings.dedup();
        Self { findings }
    }

    /// Findings in canonical order.
    #[must_use]
    pub fn findings(&self) -> &[CoreFinding] {
        &self.findings
    }
}

/// Completed partial-staging observation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StagingEvidence {
    paths: Vec<String>,
}

impl StagingEvidence {
    /// Build the sorted set of files present in both staged and unstaged diffs.
    ///
    /// # Errors
    ///
    /// Refuses a non-canonical repository path.
    pub fn new(paths: impl IntoIterator<Item = impl Into<String>>) -> Result<Self, CoreInputError> {
        let mut paths: Vec<String> = paths.into_iter().map(Into::into).collect();
        for path in &paths {
            validate_repo_path(path)?;
        }
        paths.sort();
        paths.dedup();
        Ok(Self { paths })
    }

    /// Partially staged paths in canonical order.
    #[must_use]
    pub fn paths(&self) -> &[String] {
        &self.paths
    }
}

/// One selected doctrine document and its active bound.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DocMeasurement {
    path: String,
    characters: u64,
    limit: RepoDocLimit,
    peer_claim: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inherited: Option<LatchInheritance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    covered: Option<wire::TaskHandle>,
}

/// One resolved active repository-document bound.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", tag = "kind")]
pub enum RepoDocLimit {
    AtMost { characters: u64 },
    Unlimited,
}

impl DocMeasurement {
    /// Build one resolved document measurement.
    ///
    /// # Errors
    ///
    /// Refuses a non-canonical repository path.
    pub fn new(
        path: impl Into<String>,
        characters: u64,
        limit: u64,
        peer_claim: bool,
    ) -> Result<Self, CoreInputError> {
        let path = path.into();
        validate_repo_path(&path)?;
        Ok(Self {
            path,
            characters,
            limit: RepoDocLimit::AtMost { characters: limit },
            peer_claim,
            inherited: None,
            covered: None,
        })
    }

    /// Build one selected document under an explicitly unlimited policy.
    ///
    /// # Errors
    ///
    /// Refuses a non-canonical repository path.
    pub fn unlimited(path: impl Into<String>, characters: u64) -> Result<Self, CoreInputError> {
        let path = path.into();
        validate_repo_path(&path)?;
        Ok(Self {
            path,
            characters,
            limit: RepoDocLimit::Unlimited,
            peer_claim: false,
            inherited: None,
            covered: None,
        })
    }

    /// Repository-relative document path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Character count in decoded document text.
    #[must_use]
    pub const fn characters(&self) -> u64 {
        self.characters
    }

    /// Resolved active cap after bound and latch policy, if finite.
    #[must_use]
    pub const fn limit(&self) -> Option<u64> {
        match self.limit {
            RepoDocLimit::AtMost { characters } => Some(characters),
            RepoDocLimit::Unlimited => None,
        }
    }

    /// Complete resolved active bound.
    #[must_use]
    pub const fn bound(&self) -> RepoDocLimit {
        self.limit
    }

    /// Whether a peer currently holds the applicable bound claim.
    #[must_use]
    pub const fn peer_claim(&self) -> bool {
        self.peer_claim
    }

    /// Record what the ledger knows of a held latch's origin.
    #[must_use]
    pub fn with_inheritance(mut self, inherited: Option<LatchInheritance>) -> Self {
        self.inherited = inherited;
        self
    }

    /// Origin of the held latch this document's cap came from, when the
    /// current change did not create it.
    #[must_use]
    pub const fn inherited(&self) -> Option<&LatchInheritance> {
        self.inherited.as_ref()
    }

    /// Hold this breach for the pending sweep task that covers it.
    #[must_use]
    pub fn with_cover(mut self, covered: Option<wire::TaskHandle>) -> Self {
        self.covered = covered;
        self
    }

    /// The pending sweep task that holds this breach, as a peer holds a flex
    /// claim.
    #[must_use]
    pub const fn covered(&self) -> Option<&wire::TaskHandle> {
        self.covered.as_ref()
    }
}

/// Completed repository-document measurements.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RepoDocsEvidence {
    measurements: Vec<DocMeasurement>,
}

impl RepoDocsEvidence {
    /// Canonicalize one complete measurement set.
    ///
    /// # Errors
    ///
    /// Refuses two measurements for the same selected document.
    pub fn new(mut measurements: Vec<DocMeasurement>) -> Result<Self, CoreInputError> {
        measurements.sort();
        if measurements
            .windows(2)
            .any(|pair| pair[0].path == pair[1].path)
        {
            return Err(CoreInputError::new(
                "each selected document requires exactly one measurement",
            ));
        }
        Ok(Self { measurements })
    }

    /// Selected documents in canonical path order.
    #[must_use]
    pub fn measurements(&self) -> &[DocMeasurement] {
        &self.measurements
    }
}

/// Completed layered-configuration admission findings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigKeyEvidence {
    findings: Vec<CoreFinding>,
}

impl ConfigKeyEvidence {
    /// Canonicalize packaged and active-layer admission findings.
    #[must_use]
    pub fn new(mut findings: Vec<CoreFinding>) -> Self {
        findings.sort();
        findings.dedup();
        Self { findings }
    }

    /// Admission findings in canonical order.
    #[must_use]
    pub fn findings(&self) -> &[CoreFinding] {
        &self.findings
    }
}

/// Compiled declaration/consumer inventory for one config safety invariant.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigInventoryEvidence {
    declared: Vec<Vec<String>>,
    observed: Vec<Vec<String>>,
    violations: Vec<CoreFinding>,
}

impl ConfigInventoryEvidence {
    /// Build canonical declaration, observation, and direct-violation sets.
    ///
    /// # Errors
    ///
    /// Refuses empty configuration path segments.
    pub fn new(
        mut declared: Vec<Vec<String>>,
        mut observed: Vec<Vec<String>>,
        mut violations: Vec<CoreFinding>,
    ) -> Result<Self, CoreInputError> {
        for path in declared.iter().chain(&observed) {
            if path.is_empty() || path.iter().any(|part| part.trim().is_empty()) {
                return Err(CoreInputError::new(
                    "configuration paths must contain non-empty segments",
                ));
            }
        }
        declared.sort();
        declared.dedup();
        observed.sort();
        observed.dedup();
        violations.sort();
        violations.dedup();
        Ok(Self {
            declared,
            observed,
            violations,
        })
    }

    /// Declared governed paths in canonical order.
    #[must_use]
    pub fn declared(&self) -> &[Vec<String>] {
        &self.declared
    }

    /// Compiled consumer paths in canonical order.
    #[must_use]
    pub fn observed(&self) -> &[Vec<String>] {
        &self.observed
    }

    /// Direct schema or resolver violations.
    #[must_use]
    pub fn violations(&self) -> &[CoreFinding] {
        &self.violations
    }
}

/// Gate-specific evidence from one completed observation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", tag = "gate")]
pub enum CoreEvidence {
    MergeIntegrity(MergeIntegrityEvidence),
    PlanningPhase(PlanningPhaseEvidence),
    RepoShape(RepoShapeEvidence),
    Staging(StagingEvidence),
    RepoDocs(RepoDocsEvidence),
    ConfigKeyValidity(ConfigKeyEvidence),
    ConfigFalseDisable(ConfigInventoryEvidence),
    ConfigTrackedTrust(ConfigInventoryEvidence),
}

impl CoreEvidence {
    const fn id(&self) -> GateId {
        match self {
            Self::MergeIntegrity(_) => GateId::MergeIntegrity,
            Self::PlanningPhase(_) => GateId::PlanningPhase,
            Self::RepoShape(_) => GateId::RepoShape,
            Self::Staging(_) => GateId::Staging,
            Self::RepoDocs(_) => GateId::RepoDocs,
            Self::ConfigKeyValidity(_) => GateId::ConfigKeyValidity,
            Self::ConfigFalseDisable(_) => GateId::ConfigFalseDisable,
            Self::ConfigTrackedTrust(_) => GateId::ConfigTrackedTrust,
        }
    }
}

/// Complete or explicitly incomplete acquisition state for one arm.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", tag = "state")]
pub enum CoreObservation {
    Measured { evidence: CoreEvidence },
    Unavailable { reason: String },
    Unsupported { reason: String, recovery: String },
    Unmeasured { reason: String },
    TimedOut { milliseconds: u64 },
    Skipped { reason: String },
}

/// One arm input bound to an immutable selected tree.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CoreArmInput {
    id: GateId,
    tree: Digest,
    observation: CoreObservation,
}

impl CoreArmInput {
    /// Build and validate one immutable arm input.
    ///
    /// # Errors
    ///
    /// Refuses mismatched evidence or empty state detail.
    pub fn new(
        id: GateId,
        tree: Digest,
        observation: CoreObservation,
    ) -> Result<Self, CoreInputError> {
        validate_observation(id, &observation)?;
        Ok(Self {
            id,
            tree,
            observation,
        })
    }

    /// Stable gate identity.
    #[must_use]
    pub const fn id(&self) -> GateId {
        self.id
    }

    /// Immutable selected-tree identity.
    #[must_use]
    pub const fn tree(&self) -> &Digest {
        &self.tree
    }

    /// Typed acquisition result.
    #[must_use]
    pub const fn observation(&self) -> &CoreObservation {
        &self.observation
    }
}

/// One complete core-family input battery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoreInputs {
    tree: Digest,
    arms: Vec<CoreArmInput>,
    bounds: Vec<BoundDecision>,
}

impl CoreInputs {
    /// Admit exactly the eight core arms in compiled battery order.
    ///
    /// # Errors
    ///
    /// Refuses missing, duplicated, reordered, or cross-tree observations.
    pub fn new(arms: Vec<CoreArmInput>) -> Result<Self, CoreInputError> {
        if arms.len() != CORE_GATE_IDS.len() {
            return Err(CoreInputError::new(format!(
                "core battery requires {} arms, received {}",
                CORE_GATE_IDS.len(),
                arms.len()
            )));
        }
        let tree = arms
            .first()
            .expect("the core family has a non-zero compiled size")
            .tree
            .clone();
        for (index, (input, expected)) in arms.iter().zip(CORE_GATE_IDS).enumerate() {
            if input.id != expected {
                return Err(CoreInputError::new(format!(
                    "core arm {index} must be {expected}, received {}",
                    input.id
                )));
            }
            if input.tree != tree {
                return Err(CoreInputError::new(format!(
                    "{} observation is bound to a different immutable tree",
                    input.id
                )));
            }
        }
        Ok(Self {
            tree,
            arms,
            bounds: Vec::new(),
        })
    }

    /// Retain the elastic bounds this acquisition decided while measuring.
    ///
    /// Admission above proves the arms; these decisions are what producing them
    /// cost. They are carried rather than recomputed because a second
    /// resolution could disagree with the first, and the arm was already
    /// judged against the first.
    #[must_use]
    pub fn with_bounds(mut self, bounds: Vec<BoundDecision>) -> Self {
        self.bounds = bounds;
        self
    }

    /// Immutable tree shared by every arm.
    #[must_use]
    pub const fn tree(&self) -> &Digest {
        &self.tree
    }

    /// Inputs in fixed battery order.
    #[must_use]
    pub fn arms(&self) -> &[CoreArmInput] {
        &self.arms
    }

    /// Every elastic bound decided while acquiring these arms.
    ///
    /// Empty is a real answer: it means this acquisition measured no bounded
    /// metric, not that a decision was withheld.
    #[must_use]
    pub fn bounds(&self) -> &[BoundDecision] {
        &self.bounds
    }
}

/// Typed outcome of one evaluated arm.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CoreStanding {
    Passed,
    Failed,
    Unavailable,
    Unsupported,
    Unmeasured,
    TimedOut,
    Skipped,
}

impl CoreStanding {
    /// Whether this state permits the battery to continue.
    #[must_use]
    pub const fn admits(self) -> bool {
        matches!(self, Self::Passed | Self::Skipped)
    }
}

/// One arm's typed report entry.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CoreArmReport {
    id: GateId,
    tree: Digest,
    standing: CoreStanding,
    diagnostic: String,
    observation: CoreObservation,
}

impl CoreArmReport {
    pub(crate) fn validate(&self) -> Result<(), CoreReportDecodeError> {
        let input = CoreArmInput::new(self.id, self.tree.clone(), self.observation.clone())
            .map_err(|error| CoreReportDecodeError::Inconsistent(error.to_string()))?;
        if evaluate::evaluate_core_arm(&input) != *self {
            return Err(CoreReportDecodeError::Inconsistent(format!(
                "{} standing or diagnostic disagrees with its observation",
                self.id
            )));
        }
        Ok(())
    }

    /// Stable gate identity.
    #[must_use]
    pub const fn id(&self) -> GateId {
        self.id
    }

    /// Immutable selected-tree identity.
    #[must_use]
    pub const fn tree(&self) -> &Digest {
        &self.tree
    }

    /// Typed outcome, including every non-completion state.
    #[must_use]
    pub const fn standing(&self) -> CoreStanding {
        self.standing
    }

    /// Stable human-readable diagnostic.
    #[must_use]
    pub fn diagnostic(&self) -> &str {
        &self.diagnostic
    }

    /// Complete typed observation, including any incomplete acquisition state.
    #[must_use]
    pub const fn observation(&self) -> &CoreObservation {
        &self.observation
    }

    /// Completed gate-specific evidence, when acquisition reached measurement.
    #[must_use]
    pub const fn evidence(&self) -> Option<&CoreEvidence> {
        match &self.observation {
            CoreObservation::Measured { evidence } => Some(evidence),
            _ => None,
        }
    }
}

/// Complete ordered report for the core family.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CoreReport {
    version: u64,
    tree: Digest,
    arms: Vec<CoreArmReport>,
}

impl CoreReport {
    /// Current core report schema.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Immutable tree evaluated by every arm.
    #[must_use]
    pub const fn tree(&self) -> &Digest {
        &self.tree
    }

    /// Entries in fixed battery order.
    #[must_use]
    pub fn arms(&self) -> &[CoreArmReport] {
        &self.arms
    }

    /// Whether every arm passed or was explicitly inapplicable.
    #[must_use]
    pub fn admits(&self) -> bool {
        self.arms.iter().all(|arm| arm.standing.admits())
    }

    /// Canonical JSON representation of the complete report.
    ///
    /// # Errors
    ///
    /// Returns the canonical encoder failure.
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        wire::encode(self)
    }

    /// Decode only the current canonical and internally coherent report.
    ///
    /// # Errors
    ///
    /// Refuses malformed JSON, foreign versions, non-canonical bytes, reordered
    /// arms, cross-tree evidence, and any duplicated outcome that does not
    /// equal deterministic re-evaluation.
    pub fn decode(bytes: &[u8]) -> Result<Self, CoreReportDecodeError> {
        #[derive(Deserialize)]
        struct VersionEvidence {
            version: u64,
        }

        let version: VersionEvidence = wire::decode(bytes).map_err(CoreReportDecodeError::Json)?;
        if version.version != CORE_REPORT_VERSION {
            return Err(CoreReportDecodeError::Inconsistent(format!(
                "unsupported core report version {}; expected {CORE_REPORT_VERSION}",
                version.version
            )));
        }
        let report: Self = wire::decode(bytes).map_err(CoreReportDecodeError::Json)?;
        report.validate()?;
        let canonical = report.encode().map_err(CoreReportDecodeError::Json)?;
        if canonical != bytes {
            return Err(CoreReportDecodeError::NonCanonical { canonical });
        }
        Ok(report)
    }

    fn validate(&self) -> Result<(), CoreReportDecodeError> {
        if self.arms.len() != CORE_GATE_IDS.len() {
            return Err(CoreReportDecodeError::Inconsistent(format!(
                "core report requires {} arms, received {}",
                CORE_GATE_IDS.len(),
                self.arms.len()
            )));
        }
        for (index, (arm, expected_id)) in self.arms.iter().zip(CORE_GATE_IDS).enumerate() {
            if arm.id != expected_id || arm.tree != self.tree {
                return Err(CoreReportDecodeError::Inconsistent(format!(
                    "core report arm {index} is out of order or bound to another tree"
                )));
            }
            arm.validate()?;
        }
        Ok(())
    }
}

/// Why retained core report bytes were not admitted.
#[derive(Debug)]
pub enum CoreReportDecodeError {
    Json(serde_json::Error),
    Inconsistent(String),
    NonCanonical { canonical: Vec<u8> },
}

impl CoreReportDecodeError {
    /// Canonical replacement bytes for a non-canonical spelling.
    #[must_use]
    pub fn canonical(&self) -> Option<&[u8]> {
        match self {
            Self::NonCanonical { canonical } => Some(canonical),
            _ => None,
        }
    }
}

impl fmt::Display for CoreReportDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "invalid core report JSON: {error}"),
            Self::Inconsistent(reason) => write!(formatter, "inconsistent core report: {reason}"),
            Self::NonCanonical { .. } => {
                formatter.write_str("core report JSON is not the canonical byte spelling")
            }
        }
    }
}

impl Error for CoreReportDecodeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::Inconsistent(_) | Self::NonCanonical { .. } => None,
        }
    }
}

/// Why an immutable core battery was not admitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoreInputError(String);

impl CoreInputError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for CoreInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for CoreInputError {}
