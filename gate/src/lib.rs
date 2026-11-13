//! Hermetic repository enforcement.
//!
//! The gate reads one selected repository and its Git-common quality-state
//! ledger. A report remains a function of the selected tree, one current
//! [`Contract`], and the exact run-start ledger; proposed state is published
//! separately and atomically only after the enclosing run accepts.
//!
//! [`jurisdiction::Malady`] and [`ratchet::Malady`] are two closed sets under
//! one noun, and they stay module-qualified rather than being flattened here.
//! An ignore line and a ratchet line fail in ways that have nothing to say to
//! each other, and the qualifier is what keeps a reader from believing
//! otherwise.

#[cfg(not(target_arch = "wasm32"))]
mod action_input;
#[cfg(not(target_arch = "wasm32"))]
mod action_spec;
pub mod battery;
pub mod camp;
mod change_selection;
pub mod config;
pub mod core;
#[cfg(not(target_arch = "wasm32"))]
pub mod evidence_store;
#[cfg(not(target_arch = "wasm32"))]
mod execution_diagnostics;
#[cfg(not(target_arch = "wasm32"))]
pub mod git;
#[cfg(not(target_arch = "wasm32"))]
mod held_root;
pub mod hygiene;
pub mod input;
pub mod jurisdiction;
mod long_lines;
pub mod manifest;
pub mod measure;
pub mod message;
pub mod metrics;
mod permissions;
pub mod policy;
pub mod ratchet;
pub mod report;
pub mod state;
#[doc(hidden)]
#[cfg(not(target_arch = "wasm32"))]
pub mod test_support {
    pub use crate::structural::rust_test_item_sources;
    pub use gix;
}
#[cfg(not(target_arch = "wasm32"))]
mod structural;
pub mod test_evidence;
#[cfg(test)]
mod test_repository;
pub mod toolchain;
#[cfg(not(target_arch = "wasm32"))]
pub mod tree;
pub mod verdict;
#[cfg(not(target_arch = "wasm32"))]
mod workspace_tests;

/// Hard ceiling on canonical workspace-test shard workers.
pub const WORKSPACE_TEST_CONCURRENCY_CAP: usize = 4;
/// Higher lane ceiling for test executables the prebuild already built.
///
/// Six lanes parallelize already-built test processes without admitting six
/// cold Rust compilers.
pub const PREBUILT_WORKSPACE_TEST_CONCURRENCY_CAP: usize = 6;

pub use battery::{
    ActorEvidence, ArmDisposition, ArmEvidence, ArmExecution, ArmFamily,
    BATTERY_REFUSAL_FOLD_MAX_BYTES, BatteryArmResult, BatteryContext, BatteryError,
    BatteryExecutor, BatteryInputs, BatteryManifest, BatteryManifestArm, BatteryRefusalFold,
    BatteryReport, BatteryScope, BatteryState, BatteryToolEvidence, BoundEvidence,
    CANONICAL_BATTERY_MANIFEST, CompiledArm, ConfigurationEvidence, FocusedArmReport,
    GATE_BATTERY_VERSION, LatchEvidence, ResultProvenance, canonical_gate_set_digest, run_battery,
    run_battery_arm, run_focused_arm,
};
#[cfg(not(target_arch = "wasm32"))]
pub use battery::{
    BatteryCapture, BatteryStateRun, CaptureAuthorities, InputMode, RepositoryExecutionEvent,
    StateRunScope,
};
pub use camp::{
    BoundRelation, CAMP_REPORT_VERSION, Camp, CampError, CampFinding, CampProgress, CampReport,
    CampRunError, CampStanding, DistributionReport, LocusDistance, SharedBound,
};
#[cfg(not(target_arch = "wasm32"))]
pub use change_selection::compile_language_reach;
pub use change_selection::{
    CHANGE_SELECTION_REPORT_VERSION, ChangeArmReach, ChangeReachAuthority, ChangeReachEdge,
    ChangeReachMode, ChangeReachObject, ChangeReachOrigin, ChangeSelectionEdge,
    ChangeSelectionError, ChangeSelectionMode, ChangeSelectionPolicy, ChangeSelectionPolicyInputs,
    ChangeSelectionReport, select_change_gates, select_current_source_change_gates,
};
pub use config::{Config, WalkConfig};
#[cfg(not(target_arch = "wasm32"))]
pub use core::{
    ActorCapture, CONFIG_KEY_DECLARATIONS, ClaimCapture, ConfigKeyDeclaration, CoreSource,
    acquire_core,
};
pub use core::{
    CORE_GATE_IDS, CORE_REPORT_VERSION, ConfigInventoryEvidence, ConfigKeyEvidence,
    CoreArmDeclaration, CoreArmInput, CoreArmReport, CoreEvidence, CoreFinding, CoreInputError,
    CoreInputs, CoreObservation, CoreReport, CoreReportDecodeError, CoreStanding,
    CoreTimeoutContract, CoreToolContract, DocMeasurement, GitCommandContract,
    MergeIntegrityEvidence, MergeTreeCompletion, PlanningPhaseEvidence, RepoDocLimit,
    RepoDocsEvidence, RepoShapeEvidence, StagingEvidence, core_declarations, evaluate_core,
    lacks_commit_authority,
};
#[cfg(not(target_arch = "wasm32"))]
pub use evidence_store::{
    ActionFingerprintAuthority, ActionFingerprintAuthorityFields, ActionFingerprintManifest,
    ActionFingerprintManifestFields, ActionFingerprintResolution, ActionKey, AdmissionAction,
    AdmissionBundle, AdmissionBundleFields, AdmissionCandidate, AdmissionConsumption, AdmissionId,
    AdmissionLiveCheck, AdmissionTransactionId, CollectionReport as EvidenceCollectionReport,
    EvidenceLease, EvidenceStore, EvidenceStoreError, LeaseAcquisition, PassReceipt,
    PassReceiptFields, ProducerRecord as EvidenceProducerRecord,
    Publication as EvidencePublication, ReceiptExpectation, ReceiptId, ReceiptStats,
    derive_action_key,
};
#[cfg(not(target_arch = "wasm32"))]
pub use execution_diagnostics::{
    CargoArtifactCounts, ExecutionDiagnostics, ExecutionPhase, OUTPUT_TAIL_LIMIT, PhaseTiming,
    observe_execution,
};
pub use hygiene::{
    AnalysisEvidence, AnalyzerEvidence, EvidenceState, ExclusionEvidence, HYGIENE_ARMS,
    HYGIENE_REPORT_VERSION, HygieneAnalyzer, HygieneConfigurationSource, HygieneDeclaration,
    HygieneError, HygieneFinding, HygieneFindingDetail, HygieneJurisdiction, HygieneMeasurement,
    HygieneOutcome, HygieneProvenance, HygieneReport, HygieneTraversal, ModuleReach,
    PythonUnusedStatus, SuppressedPath,
};
#[cfg(not(target_arch = "wasm32"))]
pub use hygiene::{
    HYGIENE_SOURCE_FILE_BYTE_LIMIT, HygieneAnalysisError, HygieneConfiguration, HygieneSource,
    acquire_hygiene,
};
#[cfg(not(target_arch = "wasm32"))]
pub use input::repository_rule_policy;
pub use input::{
    Contract, ContractError, ExecutableDeclaration, GateSet, GateSetError, LONG_LINE_LIMIT,
    ParsedContract, PolicyError, ToolError, ToolIdentity, declared_tools,
};
pub use jurisdiction::{
    Admission, Entry, Ignore, IgnoreError, Jurisdiction, Rule, Shape, WalkError,
};
// Action capture and specification depend on native repository acquisition.
#[cfg(not(target_arch = "wasm32"))]
pub use action_input::{
    ACTION_INPUT_MANIFEST_VERSION, ActionInputClosure, ActionInputError, ActionInputExpansion,
    ActionInputInventory, ActionInputManifest, ActionInputManifestEntry, ActionNonCacheable,
    ActionNonFileInput, ActionNonFileKind, ActionReuseClass, GitFileMode, compile_action_input,
    compile_explicit_action_input, gate_action_reuse_class,
};
#[cfg(not(target_arch = "wasm32"))]
pub use action_spec::{
    ACTION_KEY_VERSION, ACTION_SPEC_VERSION, ActionKeyAuthority, ActionPolicyAuthority, ActionSpec,
    ActionSpecFields, ActionToolRequirement, derive_authority_bound_action_key,
};
pub use long_lines::LongLineFinding;
pub use manifest::{GateId, OriginLayer};
pub use measure::{
    Bounds, JurisdictionMode, MeasureError, Measurement, Metric, MetricError, Parser, Policy,
    measurement_digest, render_measurements,
};
pub use message::{
    CommitMessageError, CommitMessagePolicy, OriginTrailer, OriginTrailerError, TrailerRejection,
    validate_commit_message,
};
pub use policy::{
    Applicability, BoundLimits, BoundName, CompileError as PolicyCompileError, FileClasses,
    FileShapePolicy, GlobalRatchet, LanguagesPolicy, MagicRule, NARROWEST_FILE_CLASSES,
    OneLetterBehavior, PACKAGED_POLICY_SOURCE, PackagePolicy, ParseError as PolicyParseError,
    Policy as RulePolicy, PolicyContext, PolicyDocument, RegexDialect, RepoTruthPolicy,
    ResolveError as PolicyResolveError, ResolvedBound, ResolvedMagic, RuleDeclaration, RuleId,
    RuleSettings, RuleSource, RuleSourceKind, ScopeAxis, ScopeSelector, SelectionProvenance,
    SelectorSpecificity, packaged_policy_text, parse_tracked_policy, tracked_path_selector_matches,
    validate_tracked_path_selector,
};
pub use ratchet::{Finding, Movement, Ratchet, RatchetError};
pub use report::{
    GATE_REPORT_VERSION, GateReport, ReportDecodeError, ReportFloor, ReportMeasurement,
};
pub use state::{
    ActorInputs, ActorResolution, ActorSource, BoundDecision, BoundError, BoundRule, ClaimDecision,
    ClaimStatus, CurrentState, JudgedLatches, KeyError, LatchDebt, LatchEntry, LatchInheritance,
    LatchKind, LatchObservation, LatchOrigin, LatchTransition, PeerClaim, PendingState, RenameMap,
    StateDecision, StateError, StateErrorKind, StateKey, StateLedger, StateReceipt,
    current_state_lock_path, current_state_path, latch_age, read_current_state,
};
#[cfg(not(target_arch = "wasm32"))]
pub use structural::{
    StructuralCapture, StructuralMatch, StructuralPattern, StructuralScanError, scan_structural,
};
pub use test_evidence::{
    CoverageCompleteness, TEST_SELECTION_REPORT_VERSION, TestEvidenceError, TestSelectionEdge,
    TestSelectionReport,
};
pub use toolchain::{
    ArmFinding, ArmMeasurement, ArmObservation, ArmOutcome, ArmTiming, ConfigurationSource,
    CsharpUnusedStatus, ENV_NAME_LITERAL_MARKER, EnvironmentLedgerKind, FindingDetail,
    JavascriptUnusedStatus, LOCAL_PATH_MARKER, MeasurementEngine, PACKAGED_ENV_INCLUDES,
    PACKAGED_ENV_PROFILE, PACKAGED_JAVASCRIPT_EXEMPTIONS, PACKAGED_SCANNER_EXCLUDES,
    PACKAGED_TASTE_WORDS, PACKAGED_TEST_PATHS, PackagedEnvironmentProfile, ProbeFailure,
    SourceLocus, TOOLCHAIN_ARMS, TOOLCHAIN_REPORT_VERSION, ToolEvidence, ToolRequirement,
    ToolchainDeclaration, ToolchainError, ToolchainReport, TreeInput, TreeJurisdiction,
    measure_toolchain,
};
#[cfg(not(target_arch = "wasm32"))]
pub use toolchain::{
    RimCapture, RimDirectoryError, RustAnalysisWorkspace, ToolchainSource, acquire_toolchain,
    formatter_edition, formatter_sources,
};
#[cfg(not(target_arch = "wasm32"))]
pub use tree::{SelectedFileMode, SelectedTree, Selector, TreeError};
pub use verdict::{
    CurrentRun, GATE_RECEIPT_VERSION, PublishError, ReceiptDecodeError, ReceiptTool, Run, RunError,
    Standing, Verdict,
};
#[cfg(not(target_arch = "wasm32"))]
pub use workspace_tests::{
    CargoExternalSourceAuthority, CargoGateShard, CargoGateShardKind, CargoGateShardPlan,
    CargoGateShardPlanning, CargoGateShardResult, WorkspaceTestBudget, WorkspaceTestError,
    WorkspaceTestRoots, cargo_declared_compile_time_sources_at,
    cargo_workspace_base_inventory_required, cargo_workspace_tests_applicable,
    plan_cargo_gate_shards_at, run_cargo_gate_shard_at,
    run_selected_with_budget_at as run_selected_workspace_tests_with_budget_at,
    windows_typechecks_declared_at,
};
