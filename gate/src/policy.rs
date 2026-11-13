//! Named, compiled policy rules and deterministic selection evidence.
//!
//! Runtime policy accepts only [`PolicyDocument`], whose rules carry stable
//! identities and source order. [`parse_tracked_policy`] reads the tracked
//! document into this one representation and compiles every path and stem
//! matcher before returning.

mod config;
mod matcher;
mod runtime;
mod tracked;

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::de::{Deserializer, Error as _};
use serde::{Deserialize, Serialize, Serializer};

pub use config::{
    FileShapePolicy, LanguagesPolicy, NARROWEST_FILE_CLASSES, PackagePolicy, RepoTruthPolicy,
};
/// The repository-path and stem dialects, shared with the native-only arms.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use matcher::{PathPattern, compile_stem_expression};
pub use runtime::{Applicability, CompileError, Policy, ResolveError};
pub(crate) use tracked::packaged_defaults;
pub use tracked::{PACKAGED_POLICY_SOURCE, ParseError, packaged_policy_text, parse_tracked_policy};

/// The explicitly supported stem-pattern dialect.
pub const REGEX_DIALECT_NAME: &str = "rust-regex-1";

/// Validate one repository-relative path selector in the shared policy dialect.
///
/// Configuration domains that retain authored selectors can call this without
/// compiling or exposing the matcher's runtime representation.
///
/// # Errors
///
/// Returns the same precise path or glob diagnostic used by compiled rules.
pub fn validate_tracked_path_selector(raw: &str) -> Result<(), String> {
    matcher::PathPattern::tracked(raw).map(|_| ())
}

/// Match one normalized repository path with the tracked selector dialect.
///
/// # Errors
///
/// Returns the same selector diagnostic as [`validate_tracked_path_selector`]
/// when `raw` is not an admitted tracked path selector.
pub fn tracked_path_selector_matches(raw: &str, path: &str) -> Result<bool, String> {
    matcher::PathPattern::tracked(raw).map(|pattern| pattern.matches(path))
}

/// The three file classes the structural arms select their scopes from.
///
/// Each arm names one of these in its `ScopeDeclaration`, and none of them may
/// be empty: an arm offered no paths reports a clean measurement over nothing,
/// which is indistinguishable from a repository that genuinely passed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileClasses {
    file_shape: Vec<String>,
    complexity: Vec<String>,
    magic: Vec<String>,
}

impl FileClasses {
    /// Suffixes the file-shape arm measures, in declared order.
    #[must_use]
    pub fn file_shape(&self) -> &[String] {
        &self.file_shape
    }

    /// Suffixes the complexity arm measures, in declared order.
    #[must_use]
    pub fn complexity(&self) -> &[String] {
        &self.complexity
    }

    /// Suffixes the magic-number arm measures, in declared order.
    #[must_use]
    pub fn magic(&self) -> &[String] {
        &self.magic
    }

    pub(crate) fn tracked(
        file_shape: Vec<String>,
        complexity: Vec<String>,
        magic: Vec<String>,
    ) -> Self {
        Self {
            file_shape,
            complexity,
            magic,
        }
    }
}

/// The six numeric policy metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundName {
    /// Maximum lines in one file.
    FileLoc,
    /// Maximum bytes in one file.
    FileBytes,
    /// Maximum cyclomatic complexity of one routine.
    RoutineCcn,
    /// Maximum lines in one routine.
    RoutineLength,
    /// Maximum commit-message line width.
    CommitMessageWrap,
    /// Maximum characters in one repository-truth document.
    RepoTruthDocChars,
}

impl BoundName {
    /// Every numeric metric in declaration order.
    pub const ALL: [Self; 6] = [
        Self::FileLoc,
        Self::FileBytes,
        Self::RoutineCcn,
        Self::RoutineLength,
        Self::CommitMessageWrap,
        Self::RepoTruthDocChars,
    ];

    /// Stable metric spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FileLoc => "file_loc",
            Self::FileBytes => "file_bytes",
            Self::RoutineCcn => "routine_ccn",
            Self::RoutineLength => "routine_length",
            Self::CommitMessageWrap => "commit_message_wrap",
            Self::RepoTruthDocChars => "repo_truth_doc_chars",
        }
    }
}

impl fmt::Display for BoundName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for BoundName {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|bound| bound.as_str() == value)
            .ok_or_else(|| format!("unknown policy metric {value:?}"))
    }
}

/// Base values for every numeric policy metric.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundLimits {
    file_loc: u64,
    file_bytes: u64,
    routine_ccn: u64,
    routine_length: u64,
    commit_message_wrap: u64,
    repo_truth_doc_chars: u64,
}

impl BoundLimits {
    /// Base value for one metric.
    #[must_use]
    pub const fn get(&self, bound: BoundName) -> u64 {
        match bound {
            BoundName::FileLoc => self.file_loc,
            BoundName::FileBytes => self.file_bytes,
            BoundName::RoutineCcn => self.routine_ccn,
            BoundName::RoutineLength => self.routine_length,
            BoundName::CommitMessageWrap => self.commit_message_wrap,
            BoundName::RepoTruthDocChars => self.repo_truth_doc_chars,
        }
    }
}

/// Global ratchet ratio plus the four per-bound overrides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalRatchet {
    ratio: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_loc: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    routine_ccn: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    routine_length: Option<u64>,
}

impl GlobalRatchet {
    /// Global ratchet ratio used when no per-rule ratio is present.
    #[must_use]
    pub const fn ratio(&self) -> f64 {
        self.ratio
    }

    fn explicit(&self, bound: BoundName) -> Option<u64> {
        match bound {
            BoundName::FileLoc => self.file_loc,
            BoundName::FileBytes => self.file_bytes,
            BoundName::RoutineCcn => self.routine_ccn,
            BoundName::RoutineLength => self.routine_length,
            BoundName::CommitMessageWrap | BoundName::RepoTruthDocChars => None,
        }
    }
}

/// Marker serialized with current policy documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegexDialect {
    /// Syntax and Unicode behavior of the Rust `regex` 1.x crate.
    #[serde(rename = "rust-regex-1")]
    RustRegex1,
}

/// Stable identity of one current rule.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuleId(String);

impl RuleId {
    /// Identity spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn generated(source: &str, field: &str) -> Self {
        Self(format!(
            "generated-rule:{}:{}",
            encode_id_component(source),
            encode_id_component(field)
        ))
    }
}

impl FromStr for RuleId {
    type Err = RuleIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if !value.is_empty() && value.is_ascii() && value.bytes().all(is_rule_id_byte) {
            Ok(Self(value.to_owned()))
        } else {
            Err(RuleIdError(value.to_owned()))
        }
    }
}

impl fmt::Display for RuleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for RuleId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for RuleId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

/// Text that cannot be a current rule identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleIdError(String);

impl fmt::Display for RuleIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            concat!(
                "rule ID must be nonempty ASCII using letters, digits, ",
                "'.', '_', ':', '%', or '-': {:?}"
            ),
            self.0
        )
    }
}

impl std::error::Error for RuleIdError {}

/// How a named current rule came into being.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuleSourceKind {
    /// One rule generated from the packaged markdown-depth budget.
    GeneratedMarkdownDepth,
    /// One rule granting a documentation directory its implicit extra depth.
    GeneratedDocumentationFamily,
    /// A rule authored directly in the current named schema.
    Current,
}

/// Source evidence retained on one named rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSource {
    location: String,
    field: String,
    kind: RuleSourceKind,
}

impl RuleSource {
    /// Source document location.
    #[must_use]
    pub fn location(&self) -> &str {
        &self.location
    }

    /// Exact source field that produced the rule.
    #[must_use]
    pub fn field(&self) -> &str {
        &self.field
    }

    /// Rule origin class.
    #[must_use]
    pub const fn kind(&self) -> RuleSourceKind {
        self.kind
    }

    pub(crate) fn tracked(location: &str, field: String, kind: RuleSourceKind) -> Self {
        Self {
            location: location.to_owned(),
            field,
            kind,
        }
    }
}

/// Universal selector axes, in resolver precedence order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScopeAxis {
    /// Repository-relative path.
    Paths,
    /// File suffix.
    Extensions,
    /// Effective agent vendor.
    Vendors,
    /// Effective agent model.
    Models,
    /// Gate phase.
    Phases,
}

impl ScopeAxis {
    const ALL: [Self; 5] = [
        Self::Paths,
        Self::Extensions,
        Self::Vendors,
        Self::Models,
        Self::Phases,
    ];

    /// Stable singular context dimension.
    #[must_use]
    pub const fn context_name(self) -> &'static str {
        match self {
            Self::Paths => "path",
            Self::Extensions => "extension",
            Self::Vendors => "vendor",
            Self::Models => "model",
            Self::Phases => "phase",
        }
    }

    pub(crate) const fn selector_name(self) -> &'static str {
        match self {
            Self::Paths => "paths",
            Self::Extensions => "extensions",
            Self::Vendors => "vendors",
            Self::Models => "models",
            Self::Phases => "phases",
        }
    }
}

impl fmt::Display for ScopeAxis {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.context_name())
    }
}

/// OR-within, AND-across rule selector.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScopeSelector {
    paths: Vec<String>,
    extensions: Vec<String>,
    vendors: Vec<String>,
    models: Vec<String>,
    phases: Vec<String>,
}

impl ScopeSelector {
    /// Canonical path alternatives after compilation.
    #[must_use]
    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    /// Canonical extension alternatives after compilation.
    #[must_use]
    pub fn extensions(&self) -> &[String] {
        &self.extensions
    }

    /// Canonical vendor alternatives after compilation.
    #[must_use]
    pub fn vendors(&self) -> &[String] {
        &self.vendors
    }

    /// Canonical model alternatives after compilation.
    #[must_use]
    pub fn models(&self) -> &[String] {
        &self.models
    }

    /// Canonical phase alternatives after compilation.
    #[must_use]
    pub fn phases(&self) -> &[String] {
        &self.phases
    }

    pub(crate) fn tracked(paths: Vec<String>, extensions: Vec<String>) -> Self {
        Self {
            paths,
            extensions,
            vendors: Vec::new(),
            models: Vec::new(),
            phases: Vec::new(),
        }
    }
}

/// Per-metric settings applied in fixed arithmetic order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuleSettings {
    multiplier: f64,
    minimum: Option<u64>,
    maximum: Option<u64>,
    unlimited: bool,
    ratchet: Option<f64>,
}

impl Default for RuleSettings {
    fn default() -> Self {
        Self {
            multiplier: 1.0,
            minimum: None,
            maximum: None,
            unlimited: false,
            ratchet: None,
        }
    }
}

impl RuleSettings {
    /// Multiplier applied before minimum and maximum clamps.
    #[must_use]
    pub const fn multiplier(&self) -> f64 {
        self.multiplier
    }

    /// Optional lower clamp.
    #[must_use]
    pub const fn minimum(&self) -> Option<u64> {
        self.minimum
    }

    /// Optional upper clamp.
    #[must_use]
    pub const fn maximum(&self) -> Option<u64> {
        self.maximum
    }

    /// Whether this metric is exempt under the rule.
    #[must_use]
    pub const fn unlimited(&self) -> bool {
        self.unlimited
    }

    /// Optional rule-local ratchet ratio.
    #[must_use]
    pub const fn ratchet(&self) -> Option<f64> {
        self.ratchet
    }

    pub(crate) fn tracked(
        multiplier: f64,
        minimum: Option<u64>,
        maximum: Option<u64>,
        unlimited: bool,
        ratchet: Option<f64>,
    ) -> Self {
        Self {
            multiplier,
            minimum,
            maximum,
            unlimited,
            ratchet,
        }
    }
}

/// Per-rule magic-number threshold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MagicRule {
    examine_threshold: u64,
}

impl MagicRule {
    /// Threshold below which a number is not examined.
    #[must_use]
    pub const fn examine_threshold(&self) -> u64 {
        self.examine_threshold
    }
}

/// Whether a rule admits a one-character file stem.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OneLetterBehavior {
    /// One-character stems participate normally.
    #[default]
    Include,
    /// One-character stems do not match this rule.
    Exclude,
}

/// One named current rule declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleDeclaration {
    id: RuleId,
    source: RuleSource,
    source_order: usize,
    priority: i32,
    #[serde(default)]
    scopes: ScopeSelector,
    #[serde(default)]
    bounds: BTreeMap<BoundName, RuleSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    magic: Option<MagicRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stem_pattern: Option<String>,
    #[serde(default)]
    one_letter: OneLetterBehavior,
}

impl RuleDeclaration {
    /// Stable rule identity.
    #[must_use]
    pub const fn id(&self) -> &RuleId {
        &self.id
    }

    /// Source evidence.
    #[must_use]
    pub const fn source(&self) -> &RuleSource {
        &self.source
    }

    /// Stable source order, where a lower value wins an otherwise exact tie.
    #[must_use]
    pub const fn source_order(&self) -> usize {
        self.source_order
    }

    /// Explicit resolver priority.
    #[must_use]
    pub const fn priority(&self) -> i32 {
        self.priority
    }

    /// Universal selector.
    #[must_use]
    pub const fn scopes(&self) -> &ScopeSelector {
        &self.scopes
    }

    /// Per-metric settings.
    #[must_use]
    pub const fn bounds(&self) -> &BTreeMap<BoundName, RuleSettings> {
        &self.bounds
    }

    /// Optional magic threshold.
    #[must_use]
    pub const fn magic(&self) -> Option<&MagicRule> {
        self.magic.as_ref()
    }

    /// Optional full-match stem pattern.
    #[must_use]
    pub fn stem_pattern(&self) -> Option<&str> {
        self.stem_pattern.as_deref()
    }

    /// One-character stem behavior.
    #[must_use]
    pub const fn one_letter(&self) -> OneLetterBehavior {
        self.one_letter
    }
}

/// The sole serializable current policy-rule representation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDocument {
    regex_dialect: RegexDialect,
    limits: BoundLimits,
    ratchet: GlobalRatchet,
    magic_examine_threshold: u64,
    #[serde(default)]
    repo_truth_docs: Vec<String>,
    file_classes: FileClasses,
    rules: Vec<RuleDeclaration>,
}

impl PolicyDocument {
    /// Declared regex dialect.
    #[must_use]
    pub const fn regex_dialect(&self) -> RegexDialect {
        self.regex_dialect
    }

    /// Base metric values.
    #[must_use]
    pub const fn limits(&self) -> &BoundLimits {
        &self.limits
    }

    /// Global ratchet policy.
    #[must_use]
    pub const fn ratchet(&self) -> &GlobalRatchet {
        &self.ratchet
    }

    /// Global magic-number threshold.
    #[must_use]
    pub const fn magic_examine_threshold(&self) -> u64 {
        self.magic_examine_threshold
    }

    /// Explicit repository-truth documents in canonical path order.
    #[must_use]
    pub fn repo_truth_docs(&self) -> &[String] {
        &self.repo_truth_docs
    }

    /// File classes the structural arms select their scopes from.
    #[must_use]
    pub const fn file_classes(&self) -> &FileClasses {
        &self.file_classes
    }

    /// Named rules in stable source order.
    #[must_use]
    pub fn rules(&self) -> &[RuleDeclaration] {
        &self.rules
    }

    /// One tracked document over the defaults parsed from the packaged
    /// `spice.toml`.
    pub(crate) fn tracked(
        regex_dialect: RegexDialect,
        limits: BoundLimits,
        ratchet: GlobalRatchet,
        magic_examine_threshold: u64,
        repo_truth_docs: Vec<String>,
        file_classes: FileClasses,
        rules: Vec<RuleDeclaration>,
    ) -> Self {
        Self {
            regex_dialect,
            limits,
            ratchet,
            magic_examine_threshold,
            repo_truth_docs,
            file_classes,
            rules,
        }
    }
}

/// Runtime context available to rule selection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicyContext {
    path: Option<String>,
    extension: Option<String>,
    vendor: Option<String>,
    model: Option<String>,
}

impl PolicyContext {
    /// Empty context. A constrained candidate will refuse resolution until its
    /// dimensions are supplied.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            path: None,
            extension: None,
            vendor: None,
            model: None,
        }
    }

    /// Supply a repository-relative path.
    #[must_use]
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Supply a file extension when no path is available.
    #[must_use]
    pub fn with_extension(mut self, extension: impl Into<String>) -> Self {
        self.extension = Some(extension.into());
        self
    }

    /// Supply an effective vendor.
    #[must_use]
    pub fn with_vendor(mut self, vendor: impl Into<String>) -> Self {
        self.vendor = Some(vendor.into());
        self
    }

    /// Supply an effective model.
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }
}

/// Lexicographic path-pattern precedence.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct PathSpecificity {
    priority: i32,
    literal_pattern: bool,
    literal_characters: usize,
    segments: usize,
    pattern_length: usize,
}

/// Full universal-selector precedence.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SelectorSpecificity {
    constrained_axes: usize,
    path: (bool, PathSpecificity, i64),
    extension: (bool, i64),
    vendor: (bool, i64),
    model: (bool, i64),
    phase: (bool, i64),
    normalized_values: [Vec<String>; 5],
}

impl SelectorSpecificity {
    /// Number of constrained axes.
    #[must_use]
    pub const fn constrained_axes(&self) -> usize {
        self.constrained_axes
    }
}

/// Evidence for the selected rule and every resolver tie-break input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionProvenance {
    rule_id: RuleId,
    metric: String,
    source: RuleSource,
    source_order: usize,
    priority: i32,
    specificity: SelectorSpecificity,
    match_basis: String,
}

impl SelectionProvenance {
    /// Selected stable rule ID.
    #[must_use]
    pub const fn rule_id(&self) -> &RuleId {
        &self.rule_id
    }

    /// Metric whose payload selected the rule.
    #[must_use]
    pub fn metric(&self) -> &str {
        &self.metric
    }

    /// Selected rule source.
    #[must_use]
    pub const fn source(&self) -> &RuleSource {
        &self.source
    }

    /// Stable source-order tie break.
    #[must_use]
    pub const fn source_order(&self) -> usize {
        self.source_order
    }

    /// Selected priority.
    #[must_use]
    pub const fn priority(&self) -> i32 {
        self.priority
    }

    /// Selected selector specificity.
    #[must_use]
    pub const fn specificity(&self) -> &SelectorSpecificity {
        &self.specificity
    }

    /// Normalized selector and payload basis that matched.
    #[must_use]
    pub fn match_basis(&self) -> &str {
        &self.match_basis
    }
}

/// One resolved numeric metric.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedBound {
    metric: BoundName,
    limit: u64,
    ratchet_limit: u64,
    unlimited: bool,
    selection: Option<SelectionProvenance>,
}

impl ResolvedBound {
    /// Metric name.
    #[must_use]
    pub const fn metric(&self) -> BoundName {
        self.metric
    }

    /// Base or rule-adjusted limit.
    #[must_use]
    pub const fn limit(&self) -> u64 {
        self.limit
    }

    /// Limit after applying the rule-local or global ratchet.
    #[must_use]
    pub const fn ratchet_limit(&self) -> u64 {
        self.ratchet_limit
    }

    /// Whether the metric is exempt.
    #[must_use]
    pub const fn unlimited(&self) -> bool {
        self.unlimited
    }

    /// Selected rule evidence, absent when global policy decided.
    #[must_use]
    pub const fn selection(&self) -> Option<&SelectionProvenance> {
        self.selection.as_ref()
    }
}

/// Resolved magic-number threshold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedMagic {
    metric: String,
    examine_threshold: u64,
    selection: Option<SelectionProvenance>,
}

impl ResolvedMagic {
    /// Stable metric name.
    #[must_use]
    pub fn metric(&self) -> &str {
        &self.metric
    }

    /// Selected threshold.
    #[must_use]
    pub const fn examine_threshold(&self) -> u64 {
        self.examine_threshold
    }

    /// Selected rule evidence, absent when global policy decided.
    #[must_use]
    pub const fn selection(&self) -> Option<&SelectionProvenance> {
        self.selection.as_ref()
    }
}

fn encode_id_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02x}"));
        }
    }
    encoded
}

fn is_rule_id_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'%' | b'-')
}
