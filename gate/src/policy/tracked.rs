//! Parser for the tracked repository policy document.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::OnceLock;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use toml::Value;

use super::matcher::normalize_path;
use super::{
    BoundLimits, BoundName, CompileError, FileClasses, GlobalRatchet, OneLetterBehavior, Policy,
    PolicyDocument, RegexDialect, RuleDeclaration, RuleId, RuleSettings, RuleSource,
    RuleSourceKind, ScopeSelector,
};

/// Stable rack resource carrying the packaged policy document.
pub const PACKAGED_POLICY_SOURCE: &str = "spice://spice.toml";

/// The exact packaged policy bytes linked into this gate build and the rack.
#[must_use]
pub fn packaged_policy_text() -> &'static str {
    ::std::include_str!("../../defaults/spice.toml")
}

static PACKAGED_DEFAULTS: OnceLock<Result<PackagedDefaults, ParseError>> = OnceLock::new();

#[derive(Clone, Debug)]
pub(crate) struct PackagedDefaults {
    regex_dialect: RegexDialect,
    limits: BoundLimits,
    ratchet: GlobalRatchet,
    magic_examine_threshold: u64,
    repo_truth_docs: Vec<String>,
    markdown: MarkdownDefaults,
    name_cluster_threshold: usize,
}

impl PackagedDefaults {
    pub(crate) const fn name_cluster_threshold(&self) -> usize {
        self.name_cluster_threshold
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MagicDefaults {
    examine_threshold: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MarkdownDepthDefaults {
    base_chars: u64,
    max_bounded_chars: u64,
    deepest_chars: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MarkdownBudgetDefaults {
    extensions: Vec<String>,
    documentation_components: Vec<String>,
}

#[derive(Clone, Debug)]
struct MarkdownDefaults {
    base_chars: u64,
    bounded_depths: usize,
    deepest_chars: u64,
    extensions: Vec<String>,
    documentation_components: Vec<String>,
}

/// Field one repository's own document inventory is refused under.
const DECLARED_REPO_TRUTH_FIELD: &str = "policy.repo_truth.docs";

/// Parse the tracked policy document into the sole compiled representation.
///
/// Repository-authored inventory and file classes come from `text`; every
/// packaged value comes from the rack's `spice.toml`. The packaged document
/// itself enters through this same function for a repository with no tracked
/// policy.
///
/// # Errors
///
/// Returns the exact tracked or packaged field whose type or selector has no
/// exact compiled meaning.
pub fn parse_tracked_policy(source: impl Into<String>, text: &str) -> Result<Policy, ParseError> {
    let source = source.into();
    validate_source(&source)?;
    let defaults = packaged_defaults()?;
    let value = parse_document(&source, text)?;
    let root = value
        .as_table()
        .ok_or_else(|| ParseError::new(&source, "<document>", "TOML root must be a table"))?;
    let policy = optional_table(root, "policy", "policy", &source)?;
    let document = PolicyDocument::tracked(
        defaults.regex_dialect,
        defaults.limits.clone(),
        defaults.ratchet.clone(),
        defaults.magic_examine_threshold,
        parse_repo_truth_docs(policy, &source, &defaults.repo_truth_docs)?,
        parse_file_classes(policy, &source)?,
        markdown_depth_rules(PACKAGED_POLICY_SOURCE, &defaults.markdown),
    );
    Policy::compile(document).map_err(|error| compile_error(&source, &error))
}

pub(crate) fn packaged_defaults() -> Result<&'static PackagedDefaults, ParseError> {
    match PACKAGED_DEFAULTS.get_or_init(parse_packaged_defaults) {
        Ok(defaults) => Ok(defaults),
        Err(error) => Err(error.clone()),
    }
}

fn parse_packaged_defaults() -> Result<PackagedDefaults, ParseError> {
    let source = PACKAGED_POLICY_SOURCE;
    let value = parse_document(source, packaged_policy_text())?;
    let root = value
        .as_table()
        .ok_or_else(|| ParseError::new(source, "<document>", "TOML root must be a table"))?;
    reject_unknown(Some(root), &["gate", "policy"], "<document>", source)?;

    let policy = required_table(root, "policy", "policy", source)?;
    reject_unknown(
        Some(policy),
        &[
            "regex_dialect",
            "name_cluster_threshold",
            "repo_truth",
            "file_shape",
            "languages",
            "limits",
            "flex",
            "magic",
            "markdown_depth",
            "markdown_depth_budget",
        ],
        "policy",
        source,
    )?;

    let repo_truth = required_table(policy, "repo_truth", "policy.repo_truth", source)?;
    reject_unknown(Some(repo_truth), &["docs"], "policy.repo_truth", source)?;
    let repo_truth_docs = string_list(
        required_value(repo_truth, "docs", DECLARED_REPO_TRUTH_FIELD, source)?,
        DECLARED_REPO_TRUTH_FIELD,
        source,
        true,
    )?;
    let repo_truth_docs = normalize_inventory(
        repo_truth_docs.iter().map(String::as_str),
        source,
        DECLARED_REPO_TRUTH_FIELD,
    )?;

    let regex_dialect: RegexDialect = decode_value(
        required_value(policy, "regex_dialect", "policy.regex_dialect", source)?,
        "policy.regex_dialect",
        source,
    )?;
    let limits: BoundLimits = decode_value(
        required_value(policy, "limits", "policy.limits", source)?,
        "policy.limits",
        source,
    )?;
    let ratchet: GlobalRatchet = decode_value(
        required_value(policy, "flex", "policy.flex", source)?,
        "policy.flex",
        source,
    )?;
    let magic: MagicDefaults = decode_value(
        required_value(policy, "magic", "policy.magic", source)?,
        "policy.magic",
        source,
    )?;
    let depth: MarkdownDepthDefaults = decode_value(
        required_value(policy, "markdown_depth", "policy.markdown_depth", source)?,
        "policy.markdown_depth",
        source,
    )?;
    let budget: MarkdownBudgetDefaults = decode_value(
        required_value(
            policy,
            "markdown_depth_budget",
            "policy.markdown_depth_budget",
            source,
        )?,
        "policy.markdown_depth_budget",
        source,
    )?;
    let name_cluster_threshold: usize = decode_value(
        required_value(
            policy,
            "name_cluster_threshold",
            "policy.name_cluster_threshold",
            source,
        )?,
        "policy.name_cluster_threshold",
        source,
    )?;
    if name_cluster_threshold == 0 {
        return Err(ParseError::new(
            source,
            "policy.name_cluster_threshold",
            "must be a positive integer",
        ));
    }
    let markdown = markdown_defaults(depth, budget, source)?;
    let file_classes = parse_file_classes(Some(policy), source)?;
    let document = PolicyDocument::tracked(
        regex_dialect,
        limits.clone(),
        ratchet.clone(),
        magic.examine_threshold,
        repo_truth_docs.clone(),
        file_classes,
        markdown_depth_rules(source, &markdown),
    );
    Policy::compile(document).map_err(|error| compile_error(source, &error))?;

    Ok(PackagedDefaults {
        regex_dialect,
        limits,
        ratchet,
        magic_examine_threshold: magic.examine_threshold,
        repo_truth_docs,
        markdown,
        name_cluster_threshold,
    })
}

fn validate_source(source: &str) -> Result<(), ParseError> {
    if source.is_empty() || source.chars().any(char::is_control) {
        return Err(ParseError::new(
            source,
            "<source>",
            "source location must be nonempty text without control characters",
        ));
    }
    Ok(())
}

fn parse_document(source: &str, text: &str) -> Result<Value, ParseError> {
    toml::from_str(text)
        .map_err(|error| ParseError::new(source, "<document>", format!("invalid TOML: {error}")))
}

fn markdown_defaults(
    depth: MarkdownDepthDefaults,
    budget: MarkdownBudgetDefaults,
    source: &str,
) -> Result<MarkdownDefaults, ParseError> {
    if depth.base_chars == 0 {
        return Err(ParseError::new(
            source,
            "policy.markdown_depth.base_chars",
            "must be a positive integer",
        ));
    }
    if depth.max_bounded_chars == 0 {
        return Err(ParseError::new(
            source,
            "policy.markdown_depth.max_bounded_chars",
            "must be a positive integer",
        ));
    }
    if depth.deepest_chars < depth.max_bounded_chars {
        return Err(ParseError::new(
            source,
            "policy.markdown_depth.deepest_chars",
            "must be at least policy.markdown_depth.max_bounded_chars",
        ));
    }
    validate_suffixes(
        &budget.extensions,
        "policy.markdown_depth_budget.extensions",
        source,
    )?;
    if budget.documentation_components.is_empty() {
        return Err(ParseError::new(
            source,
            "policy.markdown_depth_budget.documentation_components",
            "must be a non-empty list of directory-component patterns",
        ));
    }
    for (index, component) in budget.documentation_components.iter().enumerate() {
        if component.is_empty() || component.contains(['/', '\\']) {
            return Err(ParseError::new(
                source,
                format!(
                    "policy.markdown_depth_budget.documentation_components[{}]",
                    index + 1
                ),
                "must be one non-empty directory-component pattern",
            ));
        }
    }
    let bounded_depths_u64 = depth.max_bounded_chars.div_ceil(depth.base_chars);
    let bounded_depths = usize::try_from(bounded_depths_u64).map_err(|_| {
        ParseError::new(
            source,
            "policy.markdown_depth.max_bounded_chars",
            "declares more bounded depths than this executable can represent",
        )
    })?;
    if depth.base_chars.checked_mul(bounded_depths_u64).is_none() {
        return Err(ParseError::new(
            source,
            "policy.markdown_depth",
            "base_chars multiplied by the bounded depth count overflows",
        ));
    }
    Ok(MarkdownDefaults {
        base_chars: depth.base_chars,
        bounded_depths,
        deepest_chars: depth.deepest_chars,
        extensions: budget.extensions,
        documentation_components: budget.documentation_components,
    })
}

/// Resolve the repository-document inventory one policy layer declares.
///
/// A layer that declares nothing takes the packaged inventory, and a layer that
/// declares documents names the whole set it wants measured. Normalization
/// happens here so a compiled policy can never carry a spelling this refusal
/// would have rejected, and so the refusal names the declaring field rather
/// than the compiled document it would have become.
///
/// The two halves are refused separately because they have different authors. A
/// declared path is a repository's spelling and names the field that spelled it;
/// a packaged path is this release's own, and no repository can edit it or
/// repair it, so attributing one to a tracked field would send an operator to a
/// file whose bytes are not at fault. Nothing tracked reaches the packaged
/// branch, which is exactly why it must not borrow the tracked source.
///
///
/// # Errors
///
/// Returns the declared or packaged path that is not a repository-relative
/// spelling.
fn repo_truth_inventory(
    declared: Option<&[String]>,
    packaged: &[String],
    source: &str,
) -> Result<Vec<String>, ParseError> {
    match declared {
        Some(declared) => normalize_inventory(
            declared.iter().map(String::as_str),
            source,
            DECLARED_REPO_TRUTH_FIELD,
        ),
        None => normalize_inventory(
            packaged.iter().map(String::as_str),
            PACKAGED_POLICY_SOURCE,
            DECLARED_REPO_TRUTH_FIELD,
        ),
    }
}

/// Normalize every path in one inventory, attributing a refusal to its author.
fn normalize_inventory<'path>(
    paths: impl Iterator<Item = &'path str>,
    source: &str,
    field: &str,
) -> Result<Vec<String>, ParseError> {
    paths
        .map(|path| {
            normalize_path(path)
                .map_err(|reason| ParseError::new(source, field, format!("{path:?}: {reason}")))
        })
        .collect()
}

fn parse_repo_truth_docs(
    policy: Option<&toml::Table>,
    source: &str,
    packaged: &[String],
) -> Result<Vec<String>, ParseError> {
    let table = optional_nested(policy, "repo_truth", "policy.repo_truth", source)?;
    reject_unknown(table, &["docs"], "policy.repo_truth", source)?;
    let declared = table
        .and_then(|table| table.get("docs"))
        .map(|value| string_list(value, DECLARED_REPO_TRUTH_FIELD, source, true))
        .transpose()?;
    repo_truth_inventory(declared.as_deref(), packaged, source)
}

/// Resolve the three file classes the structural arms select their scopes from.
///
/// Every parsed document requires all three. The packaged document declares
/// generic selections; once a repository authors its own policy, that layer
/// must declare its complete measured set rather than silently inheriting a
/// package selection the repository never chose.
fn parse_file_classes(
    policy: Option<&toml::Table>,
    source: &str,
) -> Result<FileClasses, ParseError> {
    let shape = optional_nested(policy, "file_shape", "policy.file_shape", source)?;
    reject_unknown(shape, &["source_suffixes"], "policy.file_shape", source)?;
    let languages = optional_nested(policy, "languages", "policy.languages", source)?;
    reject_unknown(
        languages,
        &["complexity", "magic"],
        "policy.languages",
        source,
    )?;
    Ok(FileClasses::tracked(
        suffix_list(
            shape,
            "source_suffixes",
            "policy.file_shape.source_suffixes",
            source,
        )?,
        suffix_list(
            languages,
            "complexity",
            "policy.languages.complexity",
            source,
        )?,
        suffix_list(languages, "magic", "policy.languages.magic", source)?,
    ))
}

/// One required selection, spelled the way the extension of a path is spelled.
///
/// The leading period is demanded rather than added, because a selection this
/// silently rewrote would no longer be the text the repository can read back
/// and predict its own measured set from.
fn suffix_list(
    table: Option<&toml::Table>,
    key: &str,
    field: &str,
    source: &str,
) -> Result<Vec<String>, ParseError> {
    let value = table.and_then(|table| table.get(key)).ok_or_else(|| {
        ParseError::new(source, field, "must be declared by this policy document")
    })?;
    let suffixes = string_list(value, field, source, false)?;
    validate_suffixes(&suffixes, field, source)?;
    Ok(suffixes)
}

fn validate_suffixes(suffixes: &[String], field: &str, source: &str) -> Result<(), ParseError> {
    if suffixes.is_empty() {
        return Err(ParseError::new(
            source,
            field,
            "must be a non-empty list of file suffixes",
        ));
    }
    for (index, suffix) in suffixes.iter().enumerate() {
        if !suffix.starts_with('.') || suffix.len() == 1 {
            return Err(ParseError::new(
                source,
                format!("{field}[{}]", index + 1),
                format!("{suffix:?}: must be a file suffix beginning with a period"),
            ));
        }
    }
    Ok(())
}

/// The packaged Markdown depth budgets and documentation-family grants.
///
/// Each bounded depth allows one more base budget than the depth above it, up
/// to the bounded maximum; every depth past that shares the deepest cap, so no
/// packaged Markdown depth is unlimited. A directory component in the
/// documentation family contributes one additional implicit depth. That
/// preserves the budget of paths flattened by one artificial directory without
/// changing any other path. Rule identities name their generated field, so
/// persisted bound state keeps its keys.
fn markdown_depth_rules(source: &str, defaults: &MarkdownDefaults) -> Vec<RuleDeclaration> {
    let mut rules = Vec::new();
    for extension in &defaults.extensions {
        for depth in 0..=defaults.bounded_depths {
            rules.push(markdown_rule(
                source,
                extension,
                depth,
                rules.len(),
                markdown_settings(defaults, depth),
            ));
        }
        for depth in 1..defaults.bounded_depths {
            rules.push(documentation_rule(
                source,
                extension,
                depth,
                rules.len(),
                markdown_settings(defaults, depth + 1),
                &defaults.documentation_components,
            ));
        }
    }
    rules
}

fn markdown_settings(defaults: &MarkdownDefaults, depth: usize) -> RuleSettings {
    if depth >= defaults.bounded_depths {
        return RuleSettings::tracked(
            1.0,
            Some(defaults.deepest_chars),
            Some(defaults.deepest_chars),
            false,
            None,
        );
    }
    let depth = u64::try_from(depth).expect("validated packaged Markdown depth fits u64");
    let budget = defaults.base_chars * (depth + 1);
    RuleSettings::tracked(1.0, Some(budget), Some(budget), false, None)
}

fn markdown_rule(
    source: &str,
    extension: &str,
    depth: usize,
    source_order: usize,
    settings: RuleSettings,
) -> RuleDeclaration {
    let field = format!("policy.markdown_depth_budget.generated[{extension}][{depth}]");
    let matcher = vec!["*"; depth + 1].join("/");
    generated_markdown_rule(
        source,
        extension,
        field,
        source_order,
        vec![matcher],
        settings,
        RuleSourceKind::GeneratedMarkdownDepth,
    )
}

fn documentation_rule(
    source: &str,
    extension: &str,
    depth: usize,
    source_order: usize,
    settings: RuleSettings,
    families: &[String],
) -> RuleDeclaration {
    let mut paths = Vec::with_capacity(families.len() * depth);
    for directory in 0..depth {
        for family in families {
            let mut components = vec!["*"; depth + 1];
            components[directory] = family;
            paths.push(components.join("/"));
        }
    }
    let field = format!("policy.markdown_documentation_family.generated[{extension}][{depth}]");
    generated_markdown_rule(
        source,
        extension,
        field,
        source_order,
        paths,
        settings,
        RuleSourceKind::GeneratedDocumentationFamily,
    )
}

fn generated_markdown_rule(
    source: &str,
    extension: &str,
    field: String,
    source_order: usize,
    paths: Vec<String>,
    settings: RuleSettings,
    kind: RuleSourceKind,
) -> RuleDeclaration {
    RuleDeclaration {
        id: RuleId::generated(source, &field),
        source: RuleSource::tracked(source, field, kind),
        source_order,
        priority: 0,
        scopes: ScopeSelector::tracked(paths, vec![extension.to_owned()]),
        bounds: BTreeMap::from([(BoundName::RepoTruthDocChars, settings)]),
        magic: None,
        stem_pattern: None,
        one_letter: OneLetterBehavior::Exclude,
    }
}

fn required_value<'a>(
    table: &'a toml::Table,
    key: &str,
    field: &str,
    source: &str,
) -> Result<&'a Value, ParseError> {
    table
        .get(key)
        .ok_or_else(|| ParseError::new(source, field, "must be declared by packaged policy"))
}

fn required_table<'a>(
    parent: &'a toml::Table,
    key: &str,
    field: &str,
    source: &str,
) -> Result<&'a toml::Table, ParseError> {
    required_value(parent, key, field, source)?
        .as_table()
        .ok_or_else(|| ParseError::new(source, field, "must be a table"))
}

fn decode_value<T: DeserializeOwned>(
    value: &Value,
    field: &str,
    source: &str,
) -> Result<T, ParseError> {
    value
        .clone()
        .try_into()
        .map_err(|error| ParseError::new(source, field, format!("invalid value: {error}")))
}

fn optional_nested<'a>(
    parent: Option<&'a toml::Table>,
    key: &str,
    field: &str,
    source: &str,
) -> Result<Option<&'a toml::Table>, ParseError> {
    let Some(parent) = parent else {
        return Ok(None);
    };
    optional_table(parent, key, field, source)
}

fn optional_table<'a>(
    parent: &'a toml::Table,
    key: &str,
    field: &str,
    source: &str,
) -> Result<Option<&'a toml::Table>, ParseError> {
    parent
        .get(key)
        .map(|value| {
            value
                .as_table()
                .ok_or_else(|| ParseError::new(source, field, "must be a table"))
        })
        .transpose()
}

fn reject_unknown(
    table: Option<&toml::Table>,
    allowed: &[&str],
    field: &str,
    source: &str,
) -> Result<(), ParseError> {
    let Some(table) = table else {
        return Ok(());
    };
    if let Some(key) = table.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(ParseError::new(
            source,
            format!("{field}.{key}"),
            format!("unknown field; expected one of: {}", allowed.join(", ")),
        ));
    }
    Ok(())
}

fn string_list(
    value: &Value,
    field: &str,
    source: &str,
    allow_empty: bool,
) -> Result<Vec<String>, ParseError> {
    let values = value
        .as_array()
        .ok_or_else(|| ParseError::new(source, field, "must be a list of strings"))?;
    if values.is_empty() && !allow_empty {
        return Err(ParseError::new(
            source,
            field,
            "must be a non-empty list of non-empty strings",
        ));
    }
    let mut normalized = Vec::new();
    for (index, value) in values.iter().enumerate() {
        let item = value.as_str().ok_or_else(|| {
            ParseError::new(
                source,
                format!("{field}[{}]", index + 1),
                "must be a non-empty string",
            )
        })?;
        let item = item.trim();
        if item.is_empty() {
            return Err(ParseError::new(
                source,
                format!("{field}[{}]", index + 1),
                "must be a non-empty string",
            ));
        }
        if !normalized.iter().any(|existing| existing == item) {
            normalized.push(item.to_owned());
        }
    }
    Ok(normalized)
}

fn compile_error(source: &str, error: &CompileError) -> ParseError {
    ParseError::new(source, format!("policy.{}", error.field()), error.reason())
}

/// A tracked policy field that has no exact compiled meaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    source: String,
    field: String,
    reason: String,
}

impl ParseError {
    fn new(source: impl Into<String>, field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            field: field.into(),
            reason: reason.into(),
        }
    }

    /// Source document location.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Exact tracked field.
    #[must_use]
    pub fn field(&self) -> &str {
        &self.field
    }

    /// Why the field has no exact compiled meaning.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{} is refused by tracked policy parsing: {}",
            self.source, self.field, self.reason
        )
    }
}

impl std::error::Error for ParseError {}
