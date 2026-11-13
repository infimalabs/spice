//! Compiled declarations and typed measurements for hygiene analysis.
//!
//! The five arms in this module run versioned Rust analyzers. Observations
//! retain each analysis stage instead of translating absence, parse errors, or
//! provider failures into a clean scan.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};
use wire::TRACKED_POLICY_FILE;

use crate::manifest::GateId;

// Analysis reads a walked selected tree, so it is native for the same reason
// acquisition is. Nothing on the browser target can produce a jurisdiction to
// analyze, and now that acquisition is the only door into the analyzers there
// is no longer an exported symbol holding them on that target.
#[cfg(not(target_arch = "wasm32"))]
mod acquire;
#[cfg(not(target_arch = "wasm32"))]
mod analyze;
mod measure;

#[cfg(not(target_arch = "wasm32"))]
pub use acquire::{HYGIENE_SOURCE_FILE_BYTE_LIMIT, HygieneSource, acquire_hygiene};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use acquire::{HygieneDocument, acquire_hygiene_arm, acquire_hygiene_arm_reached};
#[cfg(not(target_arch = "wasm32"))]
pub use analyze::{HygieneAnalysisError, HygieneConfiguration};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use analyze::{analyze_hygiene, analyze_hygiene_arm};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use measure::{measure_hygiene, measure_hygiene_arm};

/// The only report schema emitted by this compiled arm family.
pub const HYGIENE_REPORT_VERSION: u64 = 1;
const NATIVE_ANALYZER_VERSION: &str = "spice-gate-hygiene-v1";

/// One tracked or compiled configuration input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HygieneConfigurationSource {
    source: &'static str,
    field: &'static str,
}

impl HygieneConfigurationSource {
    /// Tracked document or compiled namespace.
    #[must_use]
    pub const fn source(self) -> &'static str {
        self.source
    }

    /// Exact field or compiled symbol.
    #[must_use]
    pub const fn field(self) -> &'static str {
        self.field
    }
}

/// Complete selected-tree jurisdiction for one hygiene arm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HygieneJurisdiction {
    roots: &'static [&'static str],
    includes: &'static [&'static str],
    excludes: &'static [&'static str],
    applicability: &'static str,
}

impl HygieneJurisdiction {
    /// Roots used to classify production and test inputs.
    #[must_use]
    pub const fn roots(self) -> &'static [&'static str] {
        self.roots
    }

    /// Closed selected-file grammar.
    #[must_use]
    pub const fn includes(self) -> &'static [&'static str] {
        self.includes
    }

    /// Closed generated/vendor exclusion grammar.
    #[must_use]
    pub const fn excludes(self) -> &'static [&'static str] {
        self.excludes
    }

    /// Exact applicability boundary.
    #[must_use]
    pub const fn applicability(self) -> &'static str {
        self.applicability
    }
}

/// Native traversal performed by an arm.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HygieneTraversal {
    /// Construct production and test import graphs.
    ImportGraph,
    /// Extract declarations and fixed-point references from the graph.
    SymbolGraph,
    /// Classify production top-level Python declarations.
    PythonDeclarations,
    /// Classify Python tests and their effective assertions.
    TestAssertions,
    /// Resolve private imports and private assertion targets.
    PrivateBoundaries,
}

/// Versioned compiled analyzer selected by one declaration.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HygieneAnalyzer {
    /// Python import graph and importer analysis.
    PythonImportGraph,
    /// Python definitions and fixed-point reference analysis.
    PythonSymbolGraph,
    /// Production top-level Python declaration classification.
    PythonUnusedSymbols,
    /// Python test and effective-assertion classification.
    PythonTestAssertions,
    /// Python private-import and assertion-target resolution.
    PythonPrivateBoundaries,
}

impl HygieneAnalyzer {
    const fn evidence_name(self) -> &'static str {
        match self {
            Self::PythonImportGraph => "python-import-graph",
            Self::PythonSymbolGraph => "python-symbol-graph",
            Self::PythonUnusedSymbols => "python-unused-symbols",
            Self::PythonTestAssertions => "python-test-assertions",
            Self::PythonPrivateBoundaries => "python-private-boundaries",
        }
    }

    const fn component(self) -> &'static str {
        match self {
            Self::PythonImportGraph => {
                concat!("gate::hygiene::HygieneAnalyzer::", "PythonImportGraph")
            }
            Self::PythonSymbolGraph => {
                concat!("gate::hygiene::HygieneAnalyzer::", "PythonSymbolGraph")
            }
            Self::PythonUnusedSymbols => {
                concat!("gate::hygiene::HygieneAnalyzer::", "PythonUnusedSymbols")
            }
            Self::PythonTestAssertions => {
                concat!("gate::hygiene::HygieneAnalyzer::", "PythonTestAssertions")
            }
            Self::PythonPrivateBoundaries => concat!(
                "gate::hygiene::HygieneAnalyzer::",
                "PythonPrivateBoundaries"
            ),
        }
    }
}

/// One native declaration in fixed battery order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HygieneDeclaration {
    id: GateId,
    analyzer: HygieneAnalyzer,
    jurisdiction: HygieneJurisdiction,
    configuration: &'static [HygieneConfigurationSource],
    traversals: &'static [HygieneTraversal],
    provider_contract: Option<&'static str>,
}

impl HygieneDeclaration {
    /// Stable current gate identity.
    #[must_use]
    pub const fn id(self) -> GateId {
        self.id
    }

    /// Rust component that owns policy and parsing.
    #[must_use]
    pub const fn native_component(self) -> &'static str {
        self.analyzer.component()
    }

    /// Stable analyzer evidence name.
    #[must_use]
    pub const fn analyzer(self) -> HygieneAnalyzer {
        self.analyzer
    }

    /// Closed selected-tree jurisdiction.
    #[must_use]
    pub const fn jurisdiction(self) -> HygieneJurisdiction {
        self.jurisdiction
    }

    /// All admitted configuration sources.
    #[must_use]
    pub const fn configuration(self) -> &'static [HygieneConfigurationSource] {
        self.configuration
    }

    /// Required native traversals in execution order.
    #[must_use]
    pub const fn traversals(self) -> &'static [HygieneTraversal] {
        self.traversals
    }

    /// Exact-version law for optional external providers.
    #[must_use]
    pub const fn provider_contract(self) -> Option<&'static str> {
        self.provider_contract
    }
}

/// Declaration metadata that names the tracked policy's exclusion key rather
/// than a path pattern; acquisition reads those exclusions from the parsed
/// contract instead of compiling this entry as a rule.
const TRACKED_EXCLUSION_SOURCE: &str = "policy.exclude";
const COMMON_EXCLUDES: &[&str] = &[
    ".git/**",
    ".spice*/**",
    ".venv/**",
    ".ruff_cache/**",
    ".pytest_cache/**",
    "**/__pycache__/**",
    "node_modules/**",
    "site-packages/**",
    TRACKED_EXCLUSION_SOURCE,
];
const PYTHON_GRAPH_CONFIG: &[HygieneConfigurationSource] = &[
    HygieneConfigurationSource {
        source: "compiled",
        field: "gate::hygiene::PYTHON_PRODUCTION_ROOTS_V1",
    },
    HygieneConfigurationSource {
        source: TRACKED_POLICY_FILE,
        field: concat!(
            "policy.test_paths -> ",
            "pyproject.toml:tool.pytest.ini_options.testpaths -> tests"
        ),
    },
    HygieneConfigurationSource {
        source: TRACKED_POLICY_FILE,
        field: "policy.exclude + policy.reachability_providers",
    },
];
const PYTHON_UNUSED_CONFIG: &[HygieneConfigurationSource] = &[
    HygieneConfigurationSource {
        source: "pyproject.toml",
        field: "project.scripts + project.gui-scripts + project.entry-points",
    },
    HygieneConfigurationSource {
        source: "compiled",
        field: "gate::hygiene::PYTHON_UNUSED_EXEMPTIONS_V1",
    },
    HygieneConfigurationSource {
        source: TRACKED_POLICY_FILE,
        field: "commands + policy.test_paths + policy.exclude",
    },
];
const ASSERTION_CONFIG: &[HygieneConfigurationSource] = &[HygieneConfigurationSource {
    source: TRACKED_POLICY_FILE,
    field: concat!(
        "policy.test_paths + policy.exclude + policy.assertion_helpers + ",
        "policy.debt.assertion_free_tests"
    ),
}];
const PRIVATE_CONFIG: &[HygieneConfigurationSource] = &[HygieneConfigurationSource {
    source: TRACKED_POLICY_FILE,
    field: concat!(
        "policy.test_paths + policy.exclude + ",
        "policy.internal_couplings"
    ),
}];
const GRAPH_TRAVERSAL: &[HygieneTraversal] = &[HygieneTraversal::ImportGraph];
const SYMBOL_TRAVERSALS: &[HygieneTraversal] =
    &[HygieneTraversal::ImportGraph, HygieneTraversal::SymbolGraph];
const UNUSED_TRAVERSALS: &[HygieneTraversal] = &[
    HygieneTraversal::ImportGraph,
    HygieneTraversal::SymbolGraph,
    HygieneTraversal::PythonDeclarations,
];
const ASSERTION_TRAVERSAL: &[HygieneTraversal] = &[HygieneTraversal::TestAssertions];
const PRIVATE_TRAVERSALS: &[HygieneTraversal] = &[
    HygieneTraversal::TestAssertions,
    HygieneTraversal::PrivateBoundaries,
];
const EXACT_PROVIDER_CONTRACT: &str = concat!(
    "tracked provider declarations must name one program, exact version, ",
    "version probe, digest, arguments, scope, and timeout"
);

/// All five declarations in their fixed final battery interval.
pub const HYGIENE_ARMS: [HygieneDeclaration; 5] = [
    HygieneDeclaration {
        id: GateId::Reachability,
        analyzer: HygieneAnalyzer::PythonImportGraph,
        jurisdiction: HygieneJurisdiction {
            roots: &[
                "spice/cli/entry.py",
                "spice/serve/app.py",
                "spice/hooks/precommit.py",
                "spice/hooks/commitmsg.py",
                "spice/hooks/refguard.py",
                "spice/hooks/install.py",
                "spice/agent/cli.py",
                "spice/agent/judgeadapter.py",
                "spice/release.py",
                "configured test roots",
            ],
            includes: &["pyproject.toml", TRACKED_POLICY_FILE, "**/*.py", "**/*.pyi"],
            excludes: COMMON_EXCLUDES,
            applicability: concat!(
                "a selected Python package, production root, and test root ",
                "are all present"
            ),
        },
        configuration: PYTHON_GRAPH_CONFIG,
        traversals: GRAPH_TRAVERSAL,
        provider_contract: Some(EXACT_PROVIDER_CONTRACT),
    },
    HygieneDeclaration {
        id: GateId::SymbolReachability,
        analyzer: HygieneAnalyzer::PythonSymbolGraph,
        jurisdiction: HygieneJurisdiction {
            roots: &["production-reachable modules", "configured test roots"],
            includes: &["pyproject.toml", TRACKED_POLICY_FILE, "**/*.py", "**/*.pyi"],
            excludes: COMMON_EXCLUDES,
            applicability: concat!(
                "reachability applies and at least one production-reachable ",
                "module declares a function, class, or method"
            ),
        },
        configuration: PYTHON_GRAPH_CONFIG,
        traversals: SYMBOL_TRAVERSALS,
        provider_contract: Some(EXACT_PROVIDER_CONTRACT),
    },
    HygieneDeclaration {
        id: GateId::PythonUnused,
        analyzer: HygieneAnalyzer::PythonUnusedSymbols,
        jurisdiction: HygieneJurisdiction {
            roots: &[
                "production-reachable Python package",
                "configured test roots",
            ],
            includes: &["pyproject.toml", TRACKED_POLICY_FILE, "**/*.py", "**/*.pyi"],
            excludes: COMMON_EXCLUDES,
            applicability: concat!(
                "a selected Python package contains a production-reachable ",
                "top-level function or class"
            ),
        },
        configuration: PYTHON_UNUSED_CONFIG,
        traversals: UNUSED_TRAVERSALS,
        provider_contract: None,
    },
    HygieneDeclaration {
        id: GateId::AssertionFreeTests,
        analyzer: HygieneAnalyzer::PythonTestAssertions,
        jurisdiction: HygieneJurisdiction {
            roots: &["configured test roots"],
            includes: &["test*.py", "*_test.py"],
            excludes: COMMON_EXCLUDES,
            applicability: "at least one selected Python test function",
        },
        configuration: ASSERTION_CONFIG,
        traversals: ASSERTION_TRAVERSAL,
        provider_contract: None,
    },
    HygieneDeclaration {
        id: GateId::PrivateInternals,
        analyzer: HygieneAnalyzer::PythonPrivateBoundaries,
        jurisdiction: HygieneJurisdiction {
            roots: &["configured test roots", "selected production packages"],
            includes: &[TRACKED_POLICY_FILE, "test*.py", "*_test.py"],
            excludes: COMMON_EXCLUDES,
            applicability: concat!(
                "at least one selected Python test and production package ",
                "boundary are present"
            ),
        },
        configuration: PRIVATE_CONFIG,
        traversals: PRIVATE_TRAVERSALS,
        provider_contract: None,
    },
];

/// State retained by a typed analysis stage.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceState {
    /// Stage completed and its counts are usable.
    Completed,
    /// Stage has not produced a measurement.
    Unmeasured,
    /// Stage exceeded its declared deadline.
    TimedOut,
    /// Stage rejected source or provider output.
    ParseFailed,
    /// Stage ended for another explicit cause.
    Failed,
}

/// Analyzer or provider discovery retained before analysis begins.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "discovery", rename_all = "kebab-case")]
pub enum AnalyzerEvidence {
    /// A compiled analyzer or declared provider was unavailable.
    Unavailable {
        /// Stable analyzer/provider name.
        name: String,
        /// Native component or declared executable.
        requested: String,
    },
    /// A compiled analyzer or declared provider was identified.
    Identified {
        /// Stable analyzer/provider name.
        name: String,
        /// Native component or declared executable.
        requested: String,
        /// Resolved component or executable identity.
        resolved: String,
        /// Exact declared version.
        declared_version: String,
        /// Unmodified observed version.
        observed_version: String,
        /// Digest of native build or external executable bytes.
        digest: String,
    },
}

impl AnalyzerEvidence {
    fn name(&self) -> &str {
        match self {
            Self::Unavailable { name, .. } | Self::Identified { name, .. } => name,
        }
    }

    fn is_native_for(&self, declaration: HygieneDeclaration) -> bool {
        matches!(
            self,
            Self::Identified {
                name,
                requested,
                resolved,
                declared_version,
                observed_version,
                digest,
            } if name == declaration.analyzer.evidence_name()
                && requested == declaration.analyzer.component()
                && !resolved.is_empty()
                && declared_version == NATIVE_ANALYZER_VERSION
                && observed_version == NATIVE_ANALYZER_VERSION
                && !digest.is_empty()
        )
    }

    fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }

    fn is_unsupported(&self) -> bool {
        matches!(
            self,
            Self::Identified {
                declared_version,
                observed_version,
                ..
            } if declared_version != observed_version
        )
    }

    fn is_supported_provider(&self, provider: &str) -> bool {
        matches!(
            self,
            Self::Identified {
                name,
                requested,
                resolved,
                declared_version,
                observed_version,
                digest,
            } if name == provider
                && !requested.is_empty()
                && !resolved.is_empty()
                && !declared_version.is_empty()
                && declared_version == observed_version
                && !digest.is_empty()
        )
    }
}

/// Release that owns every grammar this family decides with.
#[cfg(not(target_arch = "wasm32"))]
const CURRENT_RELEASE: &str = "current";
/// Layer of a law the battery ships rather than reads from a repository.
#[cfg(not(target_arch = "wasm32"))]
const COMPILED_LAYER: &str = "compiled";
/// Document a compiled law is attributed to.
#[cfg(not(target_arch = "wasm32"))]
const COMPILED_DOCUMENT: &str = "<compiled-gate-battery>";

/// Exact origin of one applicability or exclusion decision.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct HygieneProvenance {
    /// Release that owns the source grammar.
    pub release: String,
    /// Compiled, repository, worktree, or system layer.
    pub layer: String,
    /// Exact source document.
    pub document: String,
    /// Exact structural field or compiled symbol.
    pub field: String,
}

impl HygieneProvenance {
    fn is_complete(&self) -> bool {
        !self.release.is_empty()
            && !self.layer.is_empty()
            && !self.document.is_empty()
            && !self.field.is_empty()
    }
}

/// One hygiene candidate a walk rule removed before any analyzer saw it.
///
/// This is a path the selected tree contains and the analysis never measured.
/// It is deliberately not an [`ExclusionEvidence`]: that one retires a whole
/// arm, while this one retires a single file or one unentered subtree and
/// leaves the arm measuring.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SuppressedPath {
    /// Selected-tree-relative path that reached no analyzer.
    pub path: String,
    /// Whether one file was removed or a whole subtree was never entered.
    ///
    /// A rule that prunes a directory removes every candidate beneath it
    /// without naming one, and the walk cannot say how many because it stops at
    /// the directory. The path alone does not separate the two cases: a
    /// directory may be named `generated.py` and be no candidate at all. This
    /// is [`crate::jurisdiction::Shape::as_str`], the spelling the inventory
    /// already uses.
    pub shape: String,
    /// Exact walk-rule text that decided the removal.
    pub rule: String,
    /// Exact release, layer, document, and field that supplied the rule.
    pub provenance: HygieneProvenance,
}

impl SuppressedPath {
    fn is_complete(&self) -> bool {
        !self.path.is_empty()
            && !self.shape.is_empty()
            && !self.rule.is_empty()
            && self.provenance.is_complete()
    }
}

/// Evidence that a compiled or tracked exclusion selected an explicit skip.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ExclusionEvidence {
    /// Exact release, layer, document, and field.
    pub provenance: HygieneProvenance,
    /// Why this selected tree is excluded.
    pub reason: String,
}

/// How much of the test suite one module holds, counted both ways.
///
/// A lane runs the tests it believes its change touches, and that belief is a
/// direct-import view: `imported_by` counts only the test modules that name
/// this one themselves, while `reached_by` follows import chains to their end.
/// For a shared module the two diverge by an order of magnitude, and that gap
/// is the whole measurement. Every test module that imports a module directly
/// also reaches it, so `imported_by` can never exceed `reached_by`.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ModuleReach {
    /// Selected path of the reached module.
    pub path: String,
    /// Test modules reaching this one through any chain of imports.
    pub reached_by: u32,
    /// Test modules naming this one in their own imports.
    pub imported_by: u32,
}

/// Typed evidence from one analysis stage.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "evidence", rename_all = "kebab-case")]
pub enum AnalysisEvidence {
    /// File-language applicability was measured.
    LanguageApplicability {
        /// Language selected by the declaration.
        language: String,
        /// Number of selected source files.
        selected_files: u32,
        /// Selected project markers in canonical order.
        project_markers: Vec<String>,
        /// Whether the declaration's applicability predicate holds.
        applicable: bool,
        /// Exact origin of the applicability law.
        provenance: HygieneProvenance,
    },
    /// Production and test graph construction evidence.
    GraphConstruction {
        /// Production roots in canonical order.
        production_roots: Vec<String>,
        /// Test roots in canonical order.
        test_roots: Vec<String>,
        /// Distinct graph nodes.
        nodes: u32,
        /// Distinct directed graph edges.
        edges: u32,
        /// Every reached module, widest suite reach first.
        reach: Vec<ModuleReach>,
    },
    /// Declaration/reference extraction evidence.
    SymbolExtraction {
        /// Extracted declarations.
        declarations: u32,
        /// References from production-reachable modules.
        production_references: u32,
        /// References from tests.
        test_references: u32,
    },
    /// Test and effective-assertion classification evidence.
    TestClassification {
        /// Selected test files.
        files: u32,
        /// Classified top-level or `Test*` methods.
        tests: u32,
        /// Effective non-static assertions.
        assertions: u32,
        /// Configured assertion helpers in canonical order.
        helpers: Vec<String>,
    },
    /// Private import/assertion boundary resolution evidence.
    PrivateBoundaryResolution {
        /// Selected production package boundaries.
        packages: Vec<String>,
        /// Resolved private import targets.
        private_imports: u32,
        /// Resolved private assertion targets.
        private_assertions: u32,
    },
    /// Configured or compiled finding threshold and measured count.
    PolicyDecision {
        /// Exact source field or compiled zero-findings law.
        field: String,
        /// Maximum admitted findings.
        limit: u32,
        /// Structured findings observed before the decision.
        observed: u32,
    },
    /// Native analyzer or declared external-provider identity.
    Analyzer(AnalyzerEvidence),
    /// Deadline result for the complete arm.
    Deadline {
        /// Declared analysis deadline.
        limit_ms: u64,
        /// Measured elapsed duration.
        elapsed_ms: u64,
        /// Completed or timed-out state.
        state: EvidenceState,
    },
    /// Parser result over source and provider documents.
    Parse {
        /// Number of successfully parsed documents.
        documents: u32,
        /// Completed or parse-failed state.
        state: EvidenceState,
        /// Selected-tree-relative failed document, if any.
        failed_path: Option<String>,
        /// Exact parser diagnostic, if any.
        diagnostic: Option<String>,
    },
    /// Terminal measurement state.
    Measurement {
        /// Completed, unmeasured, or failed state.
        state: EvidenceState,
        /// Stage that supplied the terminal state.
        stage: String,
    },
    /// Candidates this arm never measured because a walk rule removed them.
    PathSuppression {
        /// Every suppressed candidate in canonical order.
        suppressed: Vec<SuppressedPath>,
    },
    /// Explicit skip provenance.
    Exclusion(ExclusionEvidence),
}

/// Classification of one unused Python declaration.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PythonUnusedStatus {
    /// No production or test references exist.
    CandidateUnused,
    /// References exist only in tests.
    TestOnly,
}

/// Closed finding identity for the five hygiene arms.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum HygieneFindingDetail {
    /// A module is reachable only from tests.
    ModuleReachability {
        /// Native or declared provider name.
        provider: String,
        /// Dotted module or provider subject.
        subject: String,
        /// Test importers in canonical order.
        imported_by: Vec<String>,
    },
    /// A function, class, or method is reachable only from tests.
    SymbolReachability {
        /// Native or declared provider name.
        provider: String,
        /// Dotted module.
        module: String,
        /// Symbol within the module.
        symbol: String,
        /// Function, class, method, or declared provider kind.
        symbol_kind: String,
        /// Test importers in canonical order.
        imported_by: Vec<String>,
    },
    /// A top-level Python declaration lacks a production reference.
    PythonUnused {
        /// Dotted module.
        module: String,
        /// Top-level symbol.
        symbol: String,
        /// Function or class.
        declaration_kind: String,
        /// Candidate-unused or test-only state.
        status: PythonUnusedStatus,
        /// Exact classification reason.
        reason: String,
        /// Test referrers in canonical order.
        test_references: Vec<String>,
    },
    /// A test contains no effective assertion.
    AssertionFree {
        /// Top-level test or `TestClass.test_method`.
        test_name: String,
    },
    /// A test couples to a private production boundary.
    PrivateCoupling {
        /// Test owning the coupling, or `<module>`.
        test_name: String,
        /// Private import or private assertion.
        coupling_kind: String,
        /// Exact private target.
        target: String,
    },
}

impl HygieneFindingDetail {
    const fn gate(&self) -> GateId {
        match self {
            Self::ModuleReachability { .. } => GateId::Reachability,
            Self::SymbolReachability { .. } => GateId::SymbolReachability,
            Self::PythonUnused { .. } => GateId::PythonUnused,
            Self::AssertionFree { .. } => GateId::AssertionFreeTests,
            Self::PrivateCoupling { .. } => GateId::PrivateInternals,
        }
    }

    fn canonicalize(&mut self) {
        match self {
            Self::ModuleReachability { imported_by, .. }
            | Self::SymbolReachability { imported_by, .. } => {
                imported_by.sort();
                imported_by.dedup();
            }
            Self::PythonUnused {
                test_references, ..
            } => {
                test_references.sort();
                test_references.dedup();
            }
            Self::AssertionFree { .. } | Self::PrivateCoupling { .. } => {}
        }
    }
}

/// One structured finding with stable selected-tree identity.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct HygieneFinding {
    /// Selected-tree-relative path.
    pub path: String,
    /// One-based source line when the finding has a source locus.
    pub line: Option<u32>,
    /// Gate-specific stable identity.
    pub detail: HygieneFindingDetail,
}

/// Typed result of one hygiene arm.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HygieneOutcome {
    /// Analysis completed without findings.
    Passed,
    /// Analysis completed with structured findings.
    Findings,
    /// Applicability predicate was false.
    NotApplicable,
    /// A compiled or tracked exclusion selected the arm out.
    Excluded,
    /// Native analyzer or declared provider was unavailable.
    Unavailable,
    /// Analyzer/provider version violated its exact contract.
    Unsupported,
    /// Required analysis never produced a measurement.
    Unmeasured,
    /// Analysis exceeded its declared deadline.
    Timeout,
    /// Source or provider output could not be parsed.
    ParseFailure,
    /// Analysis ended for another explicit cause.
    Failed,
}

impl HygieneOutcome {
    /// Only declared absence and exclusion are skips.
    #[must_use]
    pub const fn is_skip(self) -> bool {
        matches!(self, Self::NotApplicable | Self::Excluded)
    }
}

/// Raw native-run observation measured against one declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HygieneObservation {
    /// Stable current gate identity.
    pub(crate) gate_id: GateId,
    /// Typed terminal outcome.
    pub(crate) outcome: HygieneOutcome,
    /// Exact one-line cause.
    pub(crate) reason: String,
    /// Typed stage evidence in any input order.
    pub(crate) evidence: Vec<AnalysisEvidence>,
    /// Structured findings in any input order.
    pub(crate) findings: Vec<HygieneFinding>,
}

/// Canonical retained measurement of one hygiene arm.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HygieneMeasurement {
    /// Stable current gate identity.
    pub gate_id: GateId,
    /// Typed terminal outcome.
    pub outcome: HygieneOutcome,
    /// Exact retained cause.
    pub reason: String,
    /// Native Rust analyzer identity.
    pub native_engine: String,
    /// Configuration sources retained in declaration order.
    pub configuration: Vec<String>,
    /// Complete roots retained in declaration order.
    pub roots: Vec<String>,
    /// Complete includes retained in declaration order.
    pub includes: Vec<String>,
    /// Complete exclusions retained in declaration order.
    pub excludes: Vec<String>,
    /// Exact applicability predicate.
    pub applicability: String,
    /// Required native traversals in declaration order.
    pub traversals: Vec<HygieneTraversal>,
    /// Exact law for optional providers.
    pub provider_contract: Option<String>,
    /// Typed evidence in canonical order.
    pub evidence: Vec<AnalysisEvidence>,
    /// Findings in canonical identity order.
    pub findings: Vec<HygieneFinding>,
}

impl HygieneMeasurement {
    pub(crate) fn validate(&self) -> Result<(), HygieneError> {
        measure::validate_measurement(self)
    }
}

/// Canonical ordered report for the complete five-arm family.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HygieneReport {
    /// Current report schema.
    pub version: u64,
    /// Exactly five measurements in compiled order.
    pub arms: Vec<HygieneMeasurement>,
}

impl HygieneReport {
    /// Deterministic JSON evidence with one trailing newline.
    #[must_use]
    pub fn canonical_json(&self) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(self).expect("hygiene reports contain only JSON values");
        bytes.push(b'\n');
        bytes
    }
}

/// Why observations could not become a canonical report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HygieneError {
    /// Observation named a gate outside this family.
    ForeignGate(GateId),
    /// Two observations named the same gate.
    DuplicateGate(GateId),
    /// Complete family omitted a gate.
    MissingGate(GateId),
    /// Reason was empty or multiline.
    InvalidReason(GateId),
    /// Findings did not match outcome or gate.
    InvalidFindings { gate: GateId, evidence: String },
    /// Evidence did not match the declaration or outcome.
    InvalidEvidence { gate: GateId, evidence: String },
}

impl fmt::Display for HygieneError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ForeignGate(gate) => {
                write!(formatter, "{} is not a hygiene arm", gate.as_str())
            }
            Self::DuplicateGate(gate) => {
                write!(formatter, "{} has duplicate observations", gate.as_str())
            }
            Self::MissingGate(gate) => {
                write!(formatter, "{} has no observation", gate.as_str())
            }
            Self::InvalidReason(gate) => write!(
                formatter,
                "{} has an empty or multiline reason",
                gate.as_str()
            ),
            Self::InvalidFindings { gate, evidence } => write!(
                formatter,
                "{} findings are invalid: {evidence}",
                gate.as_str()
            ),
            Self::InvalidEvidence { gate, evidence } => write!(
                formatter,
                "{} evidence is invalid: {evidence}",
                gate.as_str()
            ),
        }
    }
}

impl Error for HygieneError {}
