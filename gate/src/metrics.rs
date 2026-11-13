//! Compiled structural metric arms and their complete evidence vocabulary.
//!
//! This module declares the sole metric identities and parsers for file,
//! routine, document, magic-number, and Markdown measurements. Every selected
//! scope produces an explicit
//! [`MetricEvidence`] state; a missing parser, tool, file class, or policy
//! context is therefore evidence and never an omitted row or invented zero.

#[cfg(not(target_arch = "wasm32"))]
mod acquire;
#[cfg(not(target_arch = "wasm32"))]
mod baseline;
mod declaration;
mod evidence;
mod parser;
mod policy_binding;
mod state_binding;

#[cfg(not(target_arch = "wasm32"))]
pub use acquire::{MetricAcquisitionError, MetricSource, acquire_metrics};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use acquire::{acquire_metric_arm_with_bounds, acquire_metrics_with_bounds};
#[cfg(not(target_arch = "wasm32"))]
pub use baseline::{BaselineCapture, CommittedTexts};
pub use declaration::{
    ArmDeclaration, KeyShape, MetricDeclaration, MetricId, MetricUnit, ParserDeclaration, ParserId,
    RUST_DOC_MARKDOWN_CITATION_ALLOWANCES, RustDocMarkdownCitationAllowance, STRUCTURAL_ARMS,
    STRUCTURAL_METRICS, ScopeDeclaration,
};
pub use evidence::{
    EvidenceError, EvidenceState, FunctionKey, LatchBinding, LatchLimits, LatchOutcome,
    MEASUREMENT_FINDING_LIMIT, MetricEvidence, MetricFinding, MetricKey, MetricMeasurement,
    MetricReport, ParserEvidence, RuleBinding, STRUCTURAL_METRIC_REPORT_VERSION,
    StructuralArmEvidence,
};
#[cfg(not(target_arch = "wasm32"))]
pub use parser::{
    ComplexityRoutine, RustDocMarkdownCitationFinding, RustDocMarkdownCitationScan,
    measure_complexity, scan_rust_doc_markdown_citations,
};
pub use parser::{
    FileShapeMeasurement, MagicFinding, MarkdownAnchorInventory, MarkdownLinkFinding, ParseError,
    RepositoryDocumentMeasurement, admit_magic_source, measure_file_shape,
    measure_repository_document, scan_magic_regressions, scan_markdown_links,
};
pub use policy_binding::{
    PolicyBindingError, ResolvedMagicMetric, ResolvedMetricRule, resolve_magic_metric,
    resolve_metric_rule,
};
pub use state_binding::{MetricStateBinding, StateBindingError};
