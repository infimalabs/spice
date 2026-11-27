//! Compiled declarations and typed measurements for the toolchain arm family.
//!
//! These ten arms occupy one closed interval in the canonical battery. A
//! declaration fixes tree jurisdiction, configuration provenance, measurement
//! ownership, and every external executable/version contract. A later runner
//! contributes observations only; this module never claims that normalization
//! executed the scanner which produced them.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::manifest::GateId;

/// The only report schema emitted by this compiled arm family.
pub const TOOLCHAIN_REPORT_VERSION: u64 = 1;

/// Select every Rust source the repository formatter must read.
///
/// This is the single path authority shared by the compiled Formatters arm and
/// repository-local verification. Paths are relative to `root`, ordered by the
/// jurisdiction walk, and include sources reached only through `include!`.
///
/// # Errors
///
/// Returns the jurisdiction walk failure that prevented a complete selection.
#[cfg(not(target_arch = "wasm32"))]
pub fn formatter_sources(root: &Path) -> Result<Vec<String>, String> {
    analyze::formatter_sources(root)
}

/// Read the Rust edition governing direct formatter invocation at `root`.
///
/// # Errors
///
/// Returns a missing, unreadable, or malformed root manifest, or a manifest
/// that declares no package or workspace Rust edition.
#[cfg(not(target_arch = "wasm32"))]
pub fn formatter_edition(root: &Path) -> Result<String, String> {
    analyze::formatter_edition(root)
}

/// A compiled source of configuration admitted by one arm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigurationSource {
    source: &'static str,
    field: &'static str,
}

impl ConfigurationSource {
    /// Fixed tracked or compiled source name.
    #[must_use]
    pub const fn source(self) -> &'static str {
        self.source
    }

    /// Exact field or compiled symbol within the source.
    #[must_use]
    pub const fn field(self) -> &'static str {
        self.field
    }
}

/// Complete tree jurisdiction of one compiled arm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TreeJurisdiction {
    input: TreeInput,
    roots: &'static [&'static str],
    includes: &'static [&'static str],
    excludes: &'static [&'static str],
    applicability: &'static str,
}

impl TreeJurisdiction {
    /// Which immutable selected-tree view supplies candidate paths.
    #[must_use]
    pub const fn input(self) -> TreeInput {
        self.input
    }

    /// Roots from which matching begins.
    #[must_use]
    pub const fn roots(self) -> &'static [&'static str] {
        self.roots
    }

    /// Closed include grammar.
    #[must_use]
    pub const fn includes(self) -> &'static [&'static str] {
        self.includes
    }

    /// Closed generated/vendor exclusion grammar.
    #[must_use]
    pub const fn excludes(self) -> &'static [&'static str] {
        self.excludes
    }

    /// Exact condition under which the selected tree is applicable.
    #[must_use]
    pub const fn applicability(self) -> &'static str {
        self.applicability
    }
}

/// Candidate-path source consumed by one arm.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TreeInput {
    /// Paths changed in the selected index relative to its parent.
    SelectedChanges,
    /// Every tracked path in the immutable selected tree.
    CompleteSelectedTree,
}

/// Compiled evidence-normalization implementation recorded in every report arm.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MeasurementEngine {
    /// Version-one validation and ordering implemented by this module.
    ToolchainV1,
}

/// One external executable whose identity is fixed by a declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ToolRequirement {
    name: &'static str,
    program: &'static str,
    version_argv: &'static [&'static str],
    exact_version: &'static str,
    role: &'static str,
}

impl ToolRequirement {
    /// Stable evidence name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Sole executable name. There is no alternate resolver path.
    #[must_use]
    pub const fn program(self) -> &'static str {
        self.program
    }

    /// Fixed version-probe arguments.
    #[must_use]
    pub const fn version_argv(self) -> &'static [&'static str] {
        self.version_argv
    }

    /// Exact admitted probe output.
    #[must_use]
    pub const fn exact_version(self) -> &'static str {
        self.exact_version
    }

    /// Why this executable participates in the arm.
    #[must_use]
    pub const fn role(self) -> &'static str {
        self.role
    }
}

/// One arm declaration in canonical battery order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ToolchainDeclaration {
    id: GateId,
    measurement_engine: MeasurementEngine,
    jurisdiction: TreeJurisdiction,
    configuration: ConfigurationSource,
    tools: &'static [ToolRequirement],
    analysis: &'static [&'static str],
}

impl ToolchainDeclaration {
    /// Stable current gate identity.
    #[must_use]
    pub const fn id(self) -> GateId {
        self.id
    }

    /// Compiled Rust implementation that validates and orders supplied evidence.
    #[must_use]
    pub const fn measurement_engine(self) -> MeasurementEngine {
        self.measurement_engine
    }

    /// Closed selected-tree jurisdiction.
    #[must_use]
    pub const fn jurisdiction(self) -> TreeJurisdiction {
        self.jurisdiction
    }

    /// Sole admitted configuration source.
    #[must_use]
    pub const fn configuration(self) -> ConfigurationSource {
        self.configuration
    }

    /// External tool requirements, empty for tool-free observation contracts.
    #[must_use]
    pub const fn tools(self) -> &'static [ToolRequirement] {
        self.tools
    }

    /// Fixed external invocations, compiled scans, or supplied categories.
    #[must_use]
    pub const fn analysis(self) -> &'static [&'static str] {
        self.analysis
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod acquire;
#[cfg(not(target_arch = "wasm32"))]
mod analyze;
#[cfg(not(target_arch = "wasm32"))]
mod captured;
mod declarations;
#[cfg(not(target_arch = "wasm32"))]
mod environment;
#[cfg(not(target_arch = "wasm32"))]
pub use acquire::{RimCapture, RimDirectoryError, ToolchainSource, acquire_toolchain};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use acquire::{
    acquire_toolchain_arm, acquire_toolchain_arm_tools, acquire_toolchain_observed,
    toolchain_executable_authority,
};
pub use declarations::*;

/// Stable selected-source and Cargo output roots completed by native analysis.
///
/// Values can only be minted inside the toolchain acquisition that synchronized
/// and successfully checked the selected source. A later coordinator can reuse
/// the same immutable paths without recapturing live repository bytes.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RustAnalysisWorkspace {
    source: PathBuf,
    source_lock: PathBuf,
    cargo_target: PathBuf,
    cargo_build: PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl RustAnalysisWorkspace {
    /// Stable immutable source root checked by native analysis.
    #[must_use]
    pub fn source(&self) -> &Path {
        &self.source
    }

    /// Shared Cargo target root owned by the live worktree.
    #[must_use]
    pub fn cargo_target(&self) -> &Path {
        &self.cargo_target
    }

    /// Shared Cargo build root populated by native analysis.
    #[must_use]
    pub fn cargo_build(&self) -> &Path {
        &self.cargo_build
    }
}

/// Recover the stable paths whose source and outputs a successful native arm completed.
///
/// This remains crate-private so only an executor that observed that success can
/// mint the typed handoff used by a later canonical test coordinator.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn completed_rust_analysis_workspace(
    repository: &Path,
) -> Result<RustAnalysisWorkspace, String> {
    analyze::rust_analysis_workspace(repository)
}

/// Extensions selected by the compiled prose scanner, without their leading dot.
///
/// Kept beside the toolchain surface because both the scanner and the atomic
/// repository capture need the same class: the first decides which staged
/// changes Taste reads, and the second retains the parent bytes that can prove
/// a merge merely inherited one of those files.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const TASTE_EXTENSIONS: &[&str] = &["md", "txt", "rst"];

/// File suffixes whose committed blobs can prove environment-arm authorship.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn environment_baseline_suffixes() -> Vec<String> {
    PACKAGED_ENV_INCLUDES
        .iter()
        .map(|include| {
            include
                .strip_prefix("**/*")
                .filter(|suffix| suffix.starts_with('.'))
                .unwrap_or_else(|| {
                    panic!("packaged environment include {include:?} is not a suffix glob")
                })
                .to_owned()
        })
        .collect()
}

/// What one resolved executable did instead of identifying itself.
///
/// A program that was found is not a program that was absent, so the reason it
/// could not be identified is retained rather than folded into
/// [`ToolEvidence::Missing`]. Each variant is a fact about the probe this
/// machine ran, and none of them admits anything: an arm holding one is
/// refused, never skipped.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "probe", rename_all = "kebab-case")]
pub enum ProbeFailure {
    /// The resolved file could not be read for its digest.
    Unreadable,
    /// The declared probe could not be launched at all.
    NotStarted,
    /// The probe outlived the deadline the contract bounds it with.
    TimedOut,
    /// The probe ended on a signal rather than exiting on its own.
    Signaled,
    /// The probe exited without success.
    Refused {
        /// Exact status the probe exited with.
        code: i32,
    },
    /// The probe printed no complete version to identify the program by.
    Incomplete,
    /// The probe printed bytes that are not text.
    Undecodable,
}

impl fmt::Display for ProbeFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable => formatter.write_str("could not be read"),
            Self::NotStarted => formatter.write_str("could not be launched"),
            Self::TimedOut => formatter.write_str("outlived its bound"),
            Self::Signaled => formatter.write_str("ended on a signal"),
            Self::Refused { code } => write!(formatter, "exited with {code}"),
            Self::Incomplete => formatter.write_str("printed no complete version"),
            Self::Undecodable => formatter.write_str("printed bytes that are not text"),
        }
    }
}

/// Operational evidence for one declared external tool.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "discovery", rename_all = "kebab-case")]
pub enum ToolEvidence {
    /// The sole declared program was not found.
    Missing {
        /// Stable declaration name.
        name: String,
        /// Exact program name that was searched.
        requested_program: String,
    },
    /// One executable was resolved, and its declared probe did not identify it.
    ///
    /// Distinct from [`Self::Missing`], which says the search path holds no such
    /// program at all. This names the exact file that was found and what the
    /// probe did instead of printing a version, so a broken tool is never
    /// reported as an absent one.
    Unprobed {
        /// Stable declaration name.
        name: String,
        /// Exact program name that was requested.
        requested_program: String,
        /// Resolved executable path retained as evidence.
        resolved_program: String,
        /// What the declared probe did instead of identifying the program.
        failure: ProbeFailure,
    },
    /// One executable was resolved and probed.
    Identified {
        /// Stable declaration name.
        name: String,
        /// Exact program name that was requested.
        requested_program: String,
        /// Resolved executable path retained as evidence.
        resolved_program: String,
        /// Exactly what the version probe printed, retained unmodified.
        version: String,
        /// Digest of the resolved executable bytes.
        digest: String,
    },
}

impl ToolEvidence {
    fn name(&self) -> &str {
        match self {
            Self::Missing { name, .. }
            | Self::Unprobed { name, .. }
            | Self::Identified { name, .. } => name,
        }
    }

    fn requested_program(&self) -> &str {
        match self {
            Self::Missing {
                requested_program, ..
            }
            | Self::Unprobed {
                requested_program, ..
            }
            | Self::Identified {
                requested_program, ..
            } => requested_program,
        }
    }
}

/// One structured finding produced by a declared arm.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ArmFinding {
    /// Selected-tree-relative path, absent only for a repository-wide finding.
    pub path: Option<String>,
    /// One-based line, when the analyzer has a source locus.
    pub line: Option<u32>,
    /// One-based column, when the analyzer has a source locus.
    pub column: Option<u32>,
    /// Gate-specific retained evidence.
    pub detail: FindingDetail,
}

/// Closed structured evidence vocabulary for the eight arms.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum FindingDetail {
    /// Source differs from the formatter's canonical bytes.
    Formatting { formatter: String },
    /// Workstation-local absolute path entered tracked text.
    LocalPath,
    /// A discouraged prose token has a compiled replacement.
    Taste { word: String, replacement: String },
    /// Source-located Rust/Wasm app analysis output.
    AppWebOutput { code: String, message: String },
    /// Source-located native Rust compiler diagnostic.
    RustTypeOutput { code: String, message: String },
    /// JavaScript declaration has no admitted references.
    JavascriptUnused {
        symbol: String,
        declaration_kind: String,
        status: JavascriptUnusedStatus,
        reason: String,
        reference_count: u32,
        test_reference_count: u32,
    },
    /// C# declaration has no admitted references.
    CsharpUnused {
        symbol: String,
        declaration_kind: String,
        status: CsharpUnusedStatus,
        reason: String,
        reference_count: u32,
    },
    /// Pyright diagnostic.
    PythonType { code: String, message: String },
    /// Environment access violates the compiled access law.
    EnvironmentAccess { name: String, source: String },
    /// Environment name has inconsistent declaration/reference evidence.
    EnvironmentLedger {
        mismatch: EnvironmentLedgerKind,
        name: String,
        references: Vec<SourceLocus>,
    },
}

/// Finding-bearing status for a JavaScript declaration.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum JavascriptUnusedStatus {
    /// No production or test reference remains after declaration accounting.
    CandidateUnused,
    /// References exist only in configured test paths.
    TestOnly,
}

/// Finding-bearing status for a C# declaration.
///
/// A C# member can be reached by a framework rather than by name, so a bare
/// absence of references is never reported as a deletion. A declaration that
/// policy or the language holds live is withdrawn before it becomes a row, so
/// the one status here is the only thing a row can say: every C# finding this
/// arm emits is a candidacy, and a retention cannot be spelled as one.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CsharpUnusedStatus {
    /// No reference remains after the declaration's own name is discounted.
    CandidateUnused,
}

/// Exact environment-ledger mismatch kind.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnvironmentLedgerKind {
    /// A source reference has no configured declaration.
    Unaccounted,
    /// A configured declaration has no source reference.
    Stale,
}

/// One retained source locus nested beneath a repository-wide finding.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SourceLocus {
    /// Selected-tree-relative path.
    pub path: String,
    /// One-based source line.
    pub line: u32,
}

impl FindingDetail {
    const fn gate(&self) -> GateId {
        match self {
            Self::Formatting { .. } => GateId::Formatters,
            Self::LocalPath => GateId::LocalPaths,
            Self::Taste { .. } => GateId::Taste,
            Self::AppWebOutput { .. } => GateId::AppWebTypecheck,
            Self::RustTypeOutput { .. } => GateId::RustTypecheck,
            Self::JavascriptUnused { .. } => GateId::JavascriptUnused,
            Self::CsharpUnused { .. } => GateId::CsharpUnusedCandidates,
            Self::PythonType { .. } => GateId::PythonTypecheck,
            Self::EnvironmentAccess { .. } => GateId::EnvPolicy,
            Self::EnvironmentLedger { .. } => GateId::EnvNameLedger,
        }
    }
}

/// Typed result of one selected arm.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArmOutcome {
    /// Analysis completed without findings.
    Passed,
    /// Analysis completed with structured policy findings.
    Findings,
    /// A declared executable was not discovered, or was discovered and could
    /// not be identified by the probe it declares.
    ToolUnavailable,
    /// A discovered executable did not match its exact version contract.
    UnsupportedVersion,
    /// The declaration's project marker or file class was absent.
    NotApplicable,
    /// A compiled/tracked explicit exclusion removed the arm from this tree.
    Excluded,
    /// Bounded analysis exceeded its compiled deadline.
    Timeout,
    /// Bounded analysis was cancelled after its process started.
    Cancelled,
    /// Analysis output or selected source could not be parsed.
    ParseFailure,
    /// A direct analysis process returned an unexplained nonzero completion.
    NonzeroAnalysis,
    /// This build compiles no analyzer for the declared analysis.
    ///
    /// Distinct from [`Self::NotApplicable`] and [`Self::Excluded`], which say
    /// the tree does not need the arm. This says the arm is owed and unpaid, so
    /// it refuses the battery instead of passing an unmeasured tree.
    Unsupported,
}

impl ArmOutcome {
    /// Only contract-declared absence and exclusion are skips.
    #[must_use]
    pub const fn is_skip(self) -> bool {
        matches!(self, Self::NotApplicable | Self::Excluded)
    }
}

/// Measured wall time for one bounded external analysis.
///
/// The complete observation bound includes runtime, output drain, reap, and
/// termination grace for every external process the analysis observed. Most
/// arms invoke one process; a workspace-wide native analysis may invoke one
/// bounded Cargo unit per selected package target. Retaining the aggregate
/// bound beside aggregate elapsed time lets measurement reject evidence that
/// claims the observed waits outlived their combined contract.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArmTiming {
    /// Milliseconds observed around the complete blocking invocation set.
    pub elapsed_ms: u64,
    /// Maximum milliseconds the complete set of bounded waits may consume.
    pub observation_bound_ms: u64,
}

/// Raw runner observation to be measured against the compiled declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArmObservation {
    /// Stable current gate identity.
    pub gate_id: GateId,
    /// Candidate-path source actually authorized for this observation.
    pub tree_input: TreeInput,
    /// Typed operational result.
    pub outcome: ArmOutcome,
    /// Exact human-readable cause; never synthesized from exit status alone.
    pub reason: String,
    /// External tool evidence in any input order.
    pub tools: Vec<ToolEvidence>,
    /// Structured findings in any input order.
    pub findings: Vec<ArmFinding>,
    /// Nonzero completion code, present only for `NonzeroAnalysis`.
    pub exit_code: Option<i32>,
    /// Wall time of the analysis process, when one was invoked.
    pub timing: Option<ArmTiming>,
}

/// Canonical retained measurement of one arm.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArmMeasurement {
    /// Stable current gate identity.
    pub gate_id: GateId,
    /// Typed operational result.
    pub outcome: ArmOutcome,
    /// Exact retained cause.
    pub reason: String,
    /// Sole configuration source.
    pub configuration_source: String,
    /// Exact field or compiled symbol used from the source.
    pub configuration_field: String,
    /// Rust component and schema that validated and ordered this evidence.
    pub measurement_engine: MeasurementEngine,
    /// Candidate-path source fixed by the declaration.
    pub tree_input: TreeInput,
    /// Complete roots retained in declaration order.
    pub roots: Vec<String>,
    /// Complete includes retained in declaration order.
    pub includes: Vec<String>,
    /// Complete exclusions retained in declaration order.
    pub excludes: Vec<String>,
    /// Exact applicability predicate.
    pub applicability: String,
    /// External evidence in declaration order.
    pub tools: Vec<ToolEvidence>,
    /// Findings sorted by path, locus, and typed detail.
    pub findings: Vec<ArmFinding>,
    /// Nonzero completion code when that state was observed.
    pub exit_code: Option<i32>,
    /// Bounded wall time of the analysis process, when one was invoked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timing: Option<ArmTiming>,
}

impl ArmMeasurement {
    pub(crate) fn validate(&self) -> Result<(), ToolchainError> {
        let declaration =
            declaration(self.gate_id).ok_or(ToolchainError::ForeignGate(self.gate_id))?;
        let observed = ArmObservation {
            gate_id: self.gate_id,
            tree_input: self.tree_input,
            outcome: self.outcome,
            reason: self.reason.clone(),
            tools: self.tools.clone(),
            findings: self.findings.clone(),
            exit_code: self.exit_code,
            timing: self.timing,
        };
        let expected = measure_arm(declaration, observed)?;
        if expected != *self {
            return Err(ToolchainError::InvalidToolEvidence {
                gate: self.gate_id,
                evidence: "retained measurement disagrees with its compiled declaration".to_owned(),
            });
        }
        Ok(())
    }
}

/// Canonical ordered report for the complete ten-arm family.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ToolchainReport {
    /// Current report schema.
    pub version: u64,
    /// Exactly eight measurements in compiled order.
    pub arms: Vec<ArmMeasurement>,
}

impl ToolchainReport {
    /// Deterministic JSON evidence with one trailing newline.
    #[must_use]
    pub fn canonical_json(&self) -> Vec<u8> {
        let mut bytes =
            serde_json::to_vec(self).expect("toolchain reports contain only JSON values");
        bytes.push(b'\n');
        bytes
    }
}

/// Why observations could not become one canonical report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolchainError {
    /// Observation named a gate outside this family.
    ForeignGate(GateId),
    /// Two observations named the same gate.
    DuplicateGate(GateId),
    /// The complete family omitted a gate.
    MissingGate(GateId),
    /// A reason was empty or multiline.
    InvalidReason(GateId),
    /// Findings did not match the outcome or gate.
    InvalidFindings { gate: GateId, evidence: String },
    /// Completion code did not match the outcome.
    InvalidExitCode(GateId),
    /// Candidate-path source was not admitted by the arm declaration.
    InvalidTreeInput(GateId),
    /// Process timing was absent, unbounded, or belonged to another arm.
    InvalidTiming { gate: GateId, evidence: String },
    /// Tool evidence did not match the declaration or outcome.
    InvalidToolEvidence { gate: GateId, evidence: String },
}

impl fmt::Display for ToolchainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ForeignGate(gate) => {
                write!(formatter, "{} is not a toolchain arm", gate.as_str())
            }
            Self::DuplicateGate(gate) => {
                write!(formatter, "{} has duplicate observations", gate.as_str())
            }
            Self::MissingGate(gate) => write!(formatter, "{} has no observation", gate.as_str()),
            Self::InvalidReason(gate) => {
                write!(
                    formatter,
                    "{} has an empty or multiline reason",
                    gate.as_str()
                )
            }
            Self::InvalidFindings { gate, evidence } => {
                write!(
                    formatter,
                    "{} findings are invalid: {evidence}",
                    gate.as_str()
                )
            }
            Self::InvalidExitCode(gate) => {
                write!(
                    formatter,
                    "{} has an outcome-inconsistent exit code",
                    gate.as_str()
                )
            }
            Self::InvalidTreeInput(gate) => {
                write!(formatter, "{} has an invalid tree input", gate.as_str())
            }
            Self::InvalidTiming { gate, evidence } => {
                write!(
                    formatter,
                    "{} process timing is invalid: {evidence}",
                    gate.as_str()
                )
            }
            Self::InvalidToolEvidence { gate, evidence } => {
                write!(
                    formatter,
                    "{} tool evidence is invalid: {evidence}",
                    gate.as_str()
                )
            }
        }
    }
}

impl Error for ToolchainError {}

/// Measure a complete, unordered observation set into the fixed arm order.
///
/// # Errors
///
/// Returns the first foreign/duplicate/missing arm or inconsistent typed
/// observation. No partial report is returned.
pub fn measure_toolchain(
    observations: impl IntoIterator<Item = ArmObservation>,
) -> Result<ToolchainReport, ToolchainError> {
    let mut remaining: Vec<ArmObservation> = observations.into_iter().collect();
    let mut seen = BTreeSet::new();
    for observation in &remaining {
        if declaration(observation.gate_id).is_none() {
            return Err(ToolchainError::ForeignGate(observation.gate_id));
        }
        if !seen.insert(observation.gate_id) {
            return Err(ToolchainError::DuplicateGate(observation.gate_id));
        }
    }

    let mut arms = Vec::with_capacity(TOOLCHAIN_ARMS.len());
    for arm in TOOLCHAIN_ARMS {
        let Some(index) = remaining
            .iter()
            .position(|observation| observation.gate_id == arm.id)
        else {
            return Err(ToolchainError::MissingGate(arm.id));
        };
        arms.push(measure_arm(arm, remaining.swap_remove(index))?);
    }
    debug_assert!(remaining.is_empty());
    Ok(ToolchainReport {
        version: TOOLCHAIN_REPORT_VERSION,
        arms,
    })
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn measure_toolchain_arm(
    observation: ArmObservation,
) -> Result<ArmMeasurement, ToolchainError> {
    let declaration =
        declaration(observation.gate_id).ok_or(ToolchainError::ForeignGate(observation.gate_id))?;
    measure_arm(declaration, observation)
}

fn declaration(gate: GateId) -> Option<ToolchainDeclaration> {
    TOOLCHAIN_ARMS.into_iter().find(|arm| arm.id == gate)
}

fn measure_arm(
    declaration: ToolchainDeclaration,
    mut observation: ArmObservation,
) -> Result<ArmMeasurement, ToolchainError> {
    let gate = declaration.id;
    let declared_input = declaration.jurisdiction.input;
    if observation.tree_input != declared_input
        && !(gate == GateId::Formatters
            && observation.tree_input == TreeInput::SelectedChanges
            && declared_input == TreeInput::CompleteSelectedTree)
    {
        return Err(ToolchainError::InvalidTreeInput(gate));
    }
    if observation.reason.is_empty() || observation.reason.contains(['\n', '\r']) {
        return Err(ToolchainError::InvalidReason(gate));
    }
    validate_exit_code(gate, observation.outcome, observation.exit_code)?;
    validate_timing(gate, observation.outcome, observation.timing)?;
    normalize_findings(&mut observation.findings);
    validate_findings(gate, observation.outcome, &observation.findings)?;
    order_and_validate_tools(declaration, &observation.outcome, &mut observation.tools)?;
    observation.findings.sort();
    if observation
        .findings
        .windows(2)
        .any(|pair| pair[0] == pair[1])
    {
        return Err(ToolchainError::InvalidFindings {
            gate,
            evidence: "duplicate structured finding".to_owned(),
        });
    }

    let jurisdiction = declaration.jurisdiction;
    let configuration = declaration.configuration;
    Ok(ArmMeasurement {
        gate_id: gate,
        outcome: observation.outcome,
        reason: observation.reason,
        configuration_source: configuration.source.to_owned(),
        configuration_field: configuration.field.to_owned(),
        measurement_engine: declaration.measurement_engine,
        tree_input: observation.tree_input,
        roots: owned(jurisdiction.roots),
        includes: owned(jurisdiction.includes),
        excludes: owned(jurisdiction.excludes),
        applicability: jurisdiction.applicability.to_owned(),
        tools: observation.tools,
        findings: observation.findings,
        exit_code: observation.exit_code,
        timing: observation.timing,
    })
}

fn owned(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn validate_exit_code(
    gate: GateId,
    outcome: ArmOutcome,
    exit_code: Option<i32>,
) -> Result<(), ToolchainError> {
    let valid = match outcome {
        ArmOutcome::NonzeroAnalysis => exit_code.is_some_and(|code| code != 0),
        _ => exit_code.is_none(),
    };
    if valid {
        Ok(())
    } else {
        Err(ToolchainError::InvalidExitCode(gate))
    }
}

fn validate_timing(
    gate: GateId,
    outcome: ArmOutcome,
    timing: Option<ArmTiming>,
) -> Result<(), ToolchainError> {
    let invoked = matches!(
        outcome,
        ArmOutcome::Passed
            | ArmOutcome::Findings
            | ArmOutcome::Timeout
            | ArmOutcome::Cancelled
            | ArmOutcome::NonzeroAnalysis
    );
    let valid = match (gate, timing) {
        (GateId::RustTypecheck, Some(timing)) => {
            timing.observation_bound_ms > 0
                && timing.elapsed_ms <= timing.observation_bound_ms
                && !matches!(
                    outcome,
                    ArmOutcome::ToolUnavailable
                        | ArmOutcome::UnsupportedVersion
                        | ArmOutcome::NotApplicable
                        | ArmOutcome::Excluded
                )
        }
        (GateId::RustTypecheck, None) => !invoked,
        (_, None) => true,
        (_, Some(_)) => false,
    };
    if valid {
        Ok(())
    } else {
        Err(ToolchainError::InvalidTiming {
            gate,
            evidence: "only an invoked rust-typecheck carries elapsed time within its bound"
                .to_owned(),
        })
    }
}

fn validate_findings(
    gate: GateId,
    outcome: ArmOutcome,
    findings: &[ArmFinding],
) -> Result<(), ToolchainError> {
    if (outcome == ArmOutcome::Findings) != !findings.is_empty() {
        return Err(ToolchainError::InvalidFindings {
            gate,
            evidence: "only findings outcomes carry a nonempty finding list".to_owned(),
        });
    }
    for finding in findings {
        let path_is_valid = finding.path.as_deref().is_some_and(valid_relative_path);
        let is_repository_wide = matches!(finding.detail, FindingDetail::EnvironmentLedger { .. });
        if path_is_valid == is_repository_wide
            || finding.line == Some(0)
            || finding.column == Some(0)
            || (finding.column.is_some() && finding.line.is_none())
            || (is_repository_wide && (finding.line.is_some() || finding.column.is_some()))
            || finding.detail.gate() != gate
            || !valid_finding_detail(&finding.detail)
        {
            return Err(ToolchainError::InvalidFindings {
                gate,
                evidence: format!("malformed or gate-mismatched finding at {:?}", finding.path),
            });
        }
    }
    Ok(())
}

fn normalize_findings(findings: &mut [ArmFinding]) {
    for finding in findings {
        if let FindingDetail::EnvironmentLedger { references, .. } = &mut finding.detail {
            references.sort();
        }
    }
}

fn valid_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with(['/', '\\'])
        && !path.starts_with("./")
        && !path.contains(['\\', '\n', '\r'])
        && !path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}

/// The one-based character column one byte offset names in a line.
///
/// Shared by every scanner that reports one, because a column is what tells two
/// otherwise identical findings on one line apart and admission refuses the
/// second of any pair that agrees in every field. It lives beside the scanners
/// it serves, which this target does not compile.
#[cfg(not(target_arch = "wasm32"))]
fn column_of(line: &str, start: usize) -> Option<u32> {
    u32::try_from(line.get(..start)?.chars().count() + 1).ok()
}

fn valid_finding_detail(detail: &FindingDetail) -> bool {
    match detail {
        FindingDetail::Formatting { formatter } => !formatter.is_empty(),
        FindingDetail::LocalPath => true,
        FindingDetail::Taste { word, replacement } => {
            !word.is_empty() && !replacement.contains(['\n', '\r'])
        }
        FindingDetail::AppWebOutput { code, message }
        | FindingDetail::RustTypeOutput { code, message }
        | FindingDetail::PythonType { code, message } => !code.is_empty() && !message.is_empty(),
        FindingDetail::JavascriptUnused {
            symbol,
            declaration_kind,
            status,
            reason,
            reference_count,
            test_reference_count,
            ..
        } => {
            let kind_is_valid = matches!(
                declaration_kind.as_str(),
                "class" | "function" | "constant" | "let" | "variable"
            );
            let status_is_valid = match status {
                JavascriptUnusedStatus::CandidateUnused => {
                    reason == "no_references_outside_declaration"
                        && matches!((*reference_count, *test_reference_count), (1, 0) | (0, 1))
                }
                JavascriptUnusedStatus::TestOnly => {
                    reason == "references_only_in_tests"
                        && *reference_count == 1
                        && *test_reference_count > 0
                }
            };
            !symbol.is_empty() && kind_is_valid && status_is_valid
        }
        FindingDetail::CsharpUnused {
            symbol,
            declaration_kind,
            status,
            reason,
            reference_count,
        } => {
            let kind_is_valid = matches!(
                declaration_kind.as_str(),
                "class"
                    | "interface"
                    | "struct"
                    | "enum"
                    | "method"
                    | "property"
                    | "field"
                    | "event"
            );
            let status_is_valid = match status {
                // A candidate is the declaration's own name and nothing else.
                CsharpUnusedStatus::CandidateUnused => {
                    reason == "no_references_outside_declaration" && *reference_count == 1
                }
            };
            !symbol.is_empty() && kind_is_valid && status_is_valid
        }
        FindingDetail::EnvironmentAccess { name, source } => {
            !name.is_empty() && !source.is_empty() && !source.contains(['\n', '\r'])
        }
        FindingDetail::EnvironmentLedger {
            mismatch,
            name,
            references,
        } => {
            let loci_are_valid = references
                .iter()
                .all(|reference| valid_relative_path(&reference.path) && reference.line > 0);
            !name.is_empty()
                && loci_are_valid
                && references.windows(2).all(|pair| pair[0] < pair[1])
                && match mismatch {
                    EnvironmentLedgerKind::Unaccounted => !references.is_empty(),
                    EnvironmentLedgerKind::Stale => references.is_empty(),
                }
        }
    }
}

fn order_and_validate_tools(
    declaration: ToolchainDeclaration,
    outcome: &ArmOutcome,
    evidence: &mut [ToolEvidence],
) -> Result<(), ToolchainError> {
    let gate = declaration.id;
    let declared = declaration.tools;
    if declared.is_empty() {
        return validate_tool_free_state(gate, outcome, evidence);
    }
    validate_tool_rows(gate, declared, evidence)?;
    evidence.sort_by_key(|item| declared_tool_position(declared, item));
    validate_external_tool_outcome(gate, declared, outcome, evidence)
}

fn validate_tool_free_state(
    gate: GateId,
    outcome: &ArmOutcome,
    evidence: &[ToolEvidence],
) -> Result<(), ToolchainError> {
    if !evidence.is_empty() {
        return Err(ToolchainError::InvalidToolEvidence {
            gate,
            evidence: "tool-free arm declared no external executable".to_owned(),
        });
    }
    if matches!(
        outcome,
        ArmOutcome::ToolUnavailable | ArmOutcome::UnsupportedVersion
    ) {
        return Err(ToolchainError::InvalidToolEvidence {
            gate,
            evidence: "tool-free arm cannot report an external-tool state".to_owned(),
        });
    }
    Ok(())
}

fn validate_tool_rows(
    gate: GateId,
    declared: &[ToolRequirement],
    evidence: &[ToolEvidence],
) -> Result<(), ToolchainError> {
    let mut names = BTreeSet::new();
    for item in evidence {
        if !names.insert(item.name()) {
            return Err(ToolchainError::InvalidToolEvidence {
                gate,
                evidence: format!("duplicate {} evidence", item.name()),
            });
        }
        let Some(requirement) = declared.iter().find(|tool| tool.name == item.name()) else {
            return Err(ToolchainError::InvalidToolEvidence {
                gate,
                evidence: format!("undeclared {} evidence", item.name()),
            });
        };
        if item.requested_program() != requirement.program {
            return Err(ToolchainError::InvalidToolEvidence {
                gate,
                evidence: format!("{} used an undeclared program", item.name()),
            });
        }
        if let ToolEvidence::Identified {
            resolved_program,
            version,
            digest,
            ..
        } = item
            && (resolved_program.is_empty() || version.is_empty() || digest.is_empty())
        {
            return Err(ToolchainError::InvalidToolEvidence {
                gate,
                evidence: format!("{} has incomplete identity evidence", item.name()),
            });
        }
        if let ToolEvidence::Unprobed {
            resolved_program, ..
        } = item
            && resolved_program.is_empty()
        {
            return Err(ToolchainError::InvalidToolEvidence {
                gate,
                evidence: format!("{} named no program its probe failed on", item.name()),
            });
        }
    }
    Ok(())
}

fn declared_tool_position(declared: &[ToolRequirement], evidence: &ToolEvidence) -> usize {
    declared
        .iter()
        .position(|tool| tool.name == evidence.name())
        .expect("unknown tools were refused before ordering")
}

fn validate_external_tool_outcome(
    gate: GateId,
    declared: &[ToolRequirement],
    outcome: &ArmOutcome,
    evidence: &[ToolEvidence],
) -> Result<(), ToolchainError> {
    match outcome {
        ArmOutcome::NotApplicable | ArmOutcome::Excluded => {
            if evidence.is_empty() {
                Ok(())
            } else {
                Err(ToolchainError::InvalidToolEvidence {
                    gate,
                    evidence: "pre-discovery skip state carried tool evidence".to_owned(),
                })
            }
        }
        ArmOutcome::ToolUnavailable => {
            require_all_declared(gate, declared, evidence)?;
            let unusable = evidence
                .iter()
                .any(|item| !matches!(item, ToolEvidence::Identified { .. }));
            let identified_are_exact = evidence.iter().zip(declared).all(|(item, tool)| {
                identified_version(item).is_none_or(|version| version == tool.exact_version)
            });
            if unusable && identified_are_exact {
                Ok(())
            } else {
                Err(ToolchainError::InvalidToolEvidence {
                    gate,
                    evidence: concat!(
                        "tool-unavailable result requires an unusable program and exact versions ",
                        "for every identified program"
                    )
                    .to_owned(),
                })
            }
        }
        ArmOutcome::UnsupportedVersion => {
            require_all_identified(gate, declared, evidence)?;
            if evidence
                .iter()
                .zip(declared)
                .any(|(item, tool)| identified_version(item) != Some(tool.exact_version))
            {
                Ok(())
            } else {
                Err(ToolchainError::InvalidToolEvidence {
                    gate,
                    evidence: "unsupported-version result matched every exact version".to_owned(),
                })
            }
        }
        ArmOutcome::Unsupported => require_all_declared(gate, declared, evidence),
        ArmOutcome::Passed
        | ArmOutcome::Findings
        | ArmOutcome::Timeout
        | ArmOutcome::Cancelled
        | ArmOutcome::ParseFailure
        | ArmOutcome::NonzeroAnalysis => {
            require_all_identified(gate, declared, evidence)?;
            if evidence
                .iter()
                .zip(declared)
                .all(|(item, tool)| identified_version(item) == Some(tool.exact_version))
            {
                Ok(())
            } else {
                Err(ToolchainError::InvalidToolEvidence {
                    gate,
                    evidence: "operational result did not use every exact tool version".to_owned(),
                })
            }
        }
    }
}

fn require_all_declared(
    gate: GateId,
    declared: &[ToolRequirement],
    evidence: &[ToolEvidence],
) -> Result<(), ToolchainError> {
    if evidence.len() == declared.len() {
        Ok(())
    } else {
        Err(ToolchainError::InvalidToolEvidence {
            gate,
            evidence: "one evidence row is required for every declared tool".to_owned(),
        })
    }
}

fn require_all_identified(
    gate: GateId,
    declared: &[ToolRequirement],
    evidence: &[ToolEvidence],
) -> Result<(), ToolchainError> {
    require_all_declared(gate, declared, evidence)?;
    if evidence
        .iter()
        .all(|item| matches!(item, ToolEvidence::Identified { .. }))
    {
        Ok(())
    } else {
        Err(ToolchainError::InvalidToolEvidence {
            gate,
            evidence: "identified evidence is required for every declared tool".to_owned(),
        })
    }
}

/// The version one row offers its exact-version contract, or `None`.
///
/// Only an identified program has a version to offer. A missing one was never
/// found and an unprobed one never printed a version this family will guess at,
/// so both decline rather than supplying an empty string a contract could
/// accidentally match.
fn identified_version(evidence: &ToolEvidence) -> Option<&str> {
    match evidence {
        ToolEvidence::Identified { version, .. } => Some(contracted_version(version)),
        ToolEvidence::Missing { .. } | ToolEvidence::Unprobed { .. } => None,
    }
}

/// One printed version with exactly one trailing line terminator removed.
///
/// Evidence retains exactly what the probe printed, because the printed bytes
/// are the identity. The exact-version contracts are written without a
/// terminator, so the single normalization they imply happens here, at the only
/// place a version is ever compared, and never in the record. Removing exactly
/// one terminator keeps a version that really does end in a blank line
/// distinguishable from one that merely ends its line.
fn contracted_version(version: &str) -> &str {
    let line = version.strip_suffix('\n').unwrap_or(version);
    line.strip_suffix('\r').unwrap_or(line)
}

#[cfg(test)]
pub(crate) mod exit_status;
#[cfg(test)]
pub(crate) mod scratch;
#[cfg(test)]
use scratch::*;
