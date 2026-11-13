//! Compilation, selection, and calculation for named policy rules.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fmt;

use regex::Regex;

use super::matcher::{
    PathPattern, compile_stem_pattern, file_extension, file_stem, normalize_context_extension,
    normalize_context_identity, normalize_extensions, normalize_identities, normalize_path,
    normalize_paths, normalize_phases,
};
use super::{
    BoundName, MagicRule, OneLetterBehavior, PathSpecificity, PolicyContext, PolicyDocument,
    REGEX_DIALECT_NAME, ResolvedBound, ResolvedMagic, RuleDeclaration, RuleId, RuleSettings,
    RuleSourceKind, ScopeAxis, ScopeSelector, SelectionProvenance, SelectorSpecificity,
};

#[derive(Debug, Clone)]
struct CompiledRule {
    declaration: RuleDeclaration,
    selector: CompiledSelector,
    stem_pattern: Option<Regex>,
}

/// One fully validated runtime policy.
#[derive(Debug, Clone)]
pub struct Policy {
    document: PolicyDocument,
    rules: Vec<CompiledRule>,
}

impl Policy {
    /// Compile one current named policy document.
    ///
    /// # Errors
    ///
    /// Returns the exact current field whose identity, selector, regex, limit,
    /// or ordering cannot be executed without changing its meaning.
    pub fn compile(mut document: PolicyDocument) -> Result<Self, CompileError> {
        validate_document(&document)?;
        document.repo_truth_docs = document
            .repo_truth_docs
            .iter()
            .map(|path| {
                normalize_path(path).map_err(|reason| {
                    CompileError::document("repo_truth_docs", format!("{path:?}: {reason}"))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        document.repo_truth_docs.sort();
        document.repo_truth_docs.dedup();
        document.rules.sort_by_key(|rule| rule.source_order);

        let mut rules = Vec::with_capacity(document.rules.len());
        let mut authored_selectors: Vec<(ScopeSelector, RuleId)> = Vec::new();
        for declaration in &mut document.rules {
            let selector = CompiledSelector::compile(&declaration.id, &declaration.scopes)?;
            declaration.scopes = selector.canonical.clone();
            if declaration.source.kind == RuleSourceKind::Current {
                if let Some((_, earlier)) = authored_selectors
                    .iter()
                    .find(|(candidate, _)| candidate == &declaration.scopes)
                {
                    return Err(CompileError::rule(
                        &declaration.id,
                        "scopes",
                        format!("duplicates earlier selector on rule {earlier}"),
                    ));
                }
                authored_selectors.push((declaration.scopes.clone(), declaration.id.clone()));
            }
            let stem_pattern = declaration
                .stem_pattern
                .as_deref()
                .map(|pattern| compile_stem_pattern(&declaration.id, pattern))
                .transpose()?;
            rules.push(CompiledRule {
                declaration: declaration.clone(),
                selector,
                stem_pattern,
            });
        }
        Ok(Self { document, rules })
    }

    /// Whether one tracked path belongs to the repository-document battery.
    ///
    /// Explicit doctrine paths and generated markdown-depth rules are the only
    /// selectors. Authored bound overrides can change a selected document's
    /// limit, but cannot silently widen the document inventory.
    pub fn selects_repository_document(&self, path: &str) -> Result<bool, ResolveError> {
        let context = NormalizedContext::new(&PolicyContext::new().with_path(path))?;
        if context
            .path
            .as_ref()
            .is_some_and(|path| self.document.repo_truth_docs.binary_search(path).is_ok())
        {
            return Ok(true);
        }
        for rule in self.rules.iter().filter(|rule| {
            rule.declaration.source.kind == RuleSourceKind::GeneratedMarkdownDepth
                && rule
                    .declaration
                    .bounds
                    .contains_key(&BoundName::RepoTruthDocChars)
        }) {
            if rule
                .evaluate(BoundName::RepoTruthDocChars.as_str(), &context)?
                .matched
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The one current serializable declaration behind this compiled policy.
    #[must_use]
    pub const fn document(&self) -> &PolicyDocument {
        &self.document
    }

    /// Resolve one numeric metric.
    ///
    /// # Errors
    ///
    /// Returns a missing selector dimension rather than allowing a constrained
    /// rule to disappear into the global default. Invalid runtime path or
    /// identity spelling and arithmetic overflow are also refused.
    pub fn resolve_bound(
        &self,
        metric: BoundName,
        context: &PolicyContext,
    ) -> Result<ResolvedBound, ResolveError> {
        let normalized = NormalizedContext::new(context)?;
        let selected = self.select(metric.as_str(), &normalized, |rule| {
            rule.declaration.bounds.contains_key(&metric)
        })?;
        let base = self.document.limits.get(metric);
        let Some(candidate) = selected else {
            let ratchet_limit = match self.document.ratchet.explicit(metric) {
                Some(limit) => limit,
                None => scaled(base, self.document.ratchet.ratio, metric.as_str())?,
            };
            return Ok(ResolvedBound {
                metric,
                limit: base,
                ratchet_limit,
                unlimited: false,
                selection: None,
            });
        };

        let settings = candidate
            .rule
            .declaration
            .bounds
            .get(&metric)
            .ok_or_else(|| {
                ResolveError::calculation(metric.as_str(), "selected rule has no metric payload")
            })?;
        let provenance = candidate.provenance(metric.as_str());
        if settings.unlimited || settings.multiplier == 0.0 {
            return Ok(ResolvedBound {
                metric,
                limit: base,
                ratchet_limit: base,
                unlimited: true,
                selection: Some(provenance),
            });
        }

        let mut limit = scaled(base, settings.multiplier, metric.as_str())?.max(1);
        if let Some(minimum) = settings.minimum {
            limit = limit.max(minimum);
        }
        if let Some(maximum) = settings.maximum {
            limit = limit.min(maximum);
        }
        let ratchet_limit = scaled(
            limit,
            settings.ratchet.unwrap_or(self.document.ratchet.ratio),
            metric.as_str(),
        )?
        .max(limit);
        Ok(ResolvedBound {
            metric,
            limit,
            ratchet_limit,
            unlimited: false,
            selection: Some(provenance),
        })
    }

    /// Resolve the magic-number threshold.
    ///
    /// # Errors
    ///
    /// Returns the same typed context refusals as [`Self::resolve_bound`].
    pub fn resolve_magic(&self, context: &PolicyContext) -> Result<ResolvedMagic, ResolveError> {
        const METRIC: &str = "magic.examine_threshold";
        let normalized = NormalizedContext::new(context)?;
        let selected = self.select(METRIC, &normalized, |rule| rule.declaration.magic.is_some())?;
        let Some(candidate) = selected else {
            return Ok(ResolvedMagic {
                metric: METRIC.to_owned(),
                examine_threshold: self.document.magic_examine_threshold,
                selection: None,
            });
        };
        let threshold = candidate
            .rule
            .declaration
            .magic
            .as_ref()
            .map(MagicRule::examine_threshold)
            .ok_or_else(|| {
                ResolveError::calculation(METRIC, "selected rule has no magic payload")
            })?;
        Ok(ResolvedMagic {
            metric: METRIC.to_owned(),
            examine_threshold: threshold,
            selection: Some(candidate.provenance(METRIC)),
        })
    }

    fn select<'a, F>(
        &'a self,
        metric: &str,
        context: &NormalizedContext,
        has_payload: F,
    ) -> Result<Option<Candidate<'a>>, ResolveError>
    where
        F: Fn(&CompiledRule) -> bool,
    {
        let mut selected: Option<Candidate<'a>> = None;
        for rule in self.rules.iter().filter(|rule| has_payload(rule)) {
            let evaluation = rule.evaluate(metric, context)?;
            if !evaluation.matched {
                continue;
            }
            let candidate = Candidate {
                rule,
                specificity: evaluation.specificity,
                basis: evaluation.basis,
            };
            if selected
                .as_ref()
                .is_none_or(|current| candidate.cmp(current).is_gt())
            {
                selected = Some(candidate);
            }
        }
        Ok(selected)
    }
}

impl CompiledRule {
    fn evaluate(
        &self,
        metric: &str,
        context: &NormalizedContext,
    ) -> Result<RuleEvaluation, ResolveError> {
        let requires_path = self.stem_pattern.is_some()
            || self.declaration.one_letter == OneLetterBehavior::Exclude;
        let mut evaluation = self
            .selector
            .evaluate(&self.declaration.id, metric, context)?;
        if requires_path && context.path.is_none() {
            return Err(ResolveError::missing(
                &self.declaration.id,
                metric,
                ScopeAxis::Paths,
            ));
        }
        if let Some(path) = &context.path {
            let stem = file_stem(path);
            if self.declaration.one_letter == OneLetterBehavior::Exclude {
                let matched = stem.chars().count() > 1;
                evaluation.basis.push_str(&format!(
                    "; one-letter=exclude actual={stem:?} match={matched}"
                ));
                evaluation.matched &= matched;
            }
            if let Some(pattern) = &self.stem_pattern {
                let matched = pattern.is_match(&stem);
                evaluation.basis.push_str(&format!(
                    "; stem-pattern={:?} dialect={} actual={stem:?} match={matched}",
                    self.declaration.stem_pattern.as_deref().unwrap_or_default(),
                    REGEX_DIALECT_NAME
                ));
                evaluation.matched &= matched;
            }
        }
        Ok(evaluation)
    }
}

struct Candidate<'a> {
    rule: &'a CompiledRule,
    specificity: SelectorSpecificity,
    basis: String,
}

impl Candidate<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rule
            .declaration
            .priority
            .cmp(&other.rule.declaration.priority)
            .then_with(|| self.specificity.cmp(&other.specificity))
            .then_with(|| {
                other
                    .rule
                    .declaration
                    .source_order
                    .cmp(&self.rule.declaration.source_order)
            })
    }

    fn provenance(&self, metric: &str) -> SelectionProvenance {
        SelectionProvenance {
            rule_id: self.rule.declaration.id.clone(),
            metric: metric.to_owned(),
            source: self.rule.declaration.source.clone(),
            source_order: self.rule.declaration.source_order,
            priority: self.rule.declaration.priority,
            specificity: self.specificity.clone(),
            match_basis: self.basis.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct CompiledSelector {
    canonical: ScopeSelector,
    paths: Vec<PathPattern>,
}

/// One named, compiled applicability selector shared by policy consumers.
#[derive(Debug, Clone)]
pub struct Applicability {
    subject: RuleId,
    selector: CompiledSelector,
}

impl Applicability {
    /// Compile one selector through the current shared scope grammar.
    ///
    /// # Errors
    ///
    /// Returns the exact malformed selector field under `subject`.
    pub fn compile(subject: RuleId, selector: &ScopeSelector) -> Result<Self, CompileError> {
        Ok(Self {
            selector: CompiledSelector::compile(&subject, selector)?,
            subject,
        })
    }

    /// Canonical selector retained after compilation.
    #[must_use]
    pub const fn selector(&self) -> &ScopeSelector {
        &self.selector.canonical
    }

    /// Whether the supplied context satisfies this selector.
    ///
    /// # Errors
    ///
    /// Refuses malformed context and every constrained axis the caller omitted.
    pub fn matches(&self, context: &PolicyContext) -> Result<bool, ResolveError> {
        let normalized = NormalizedContext::new(context)?;
        Ok(self
            .selector
            .evaluate(&self.subject, "applicability", &normalized)?
            .matched)
    }
}

impl PartialEq for Applicability {
    fn eq(&self, other: &Self) -> bool {
        self.subject == other.subject && self.selector.canonical == other.selector.canonical
    }
}

impl Eq for Applicability {}

impl CompiledSelector {
    fn compile(rule: &RuleId, selector: &ScopeSelector) -> Result<Self, CompileError> {
        let paths = normalize_paths(rule, &selector.paths)?;
        let canonical = ScopeSelector {
            paths: paths.iter().map(|path| path.source.clone()).collect(),
            extensions: normalize_extensions(rule, &selector.extensions)?,
            vendors: normalize_identities(rule, ScopeAxis::Vendors, &selector.vendors)?,
            models: normalize_identities(rule, ScopeAxis::Models, &selector.models)?,
            phases: normalize_phases(rule, &selector.phases)?,
        };
        Ok(Self { canonical, paths })
    }

    fn evaluate(
        &self,
        rule: &RuleId,
        metric: &str,
        context: &NormalizedContext,
    ) -> Result<RuleEvaluation, ResolveError> {
        for axis in ScopeAxis::ALL {
            if self.constrained(axis) && !context.has(axis) {
                return Err(ResolveError::missing(rule, metric, axis));
            }
        }

        let mut matched = true;
        let mut parts = Vec::new();
        if !self.paths.is_empty() {
            let actual = context.path.as_deref().unwrap_or_default();
            let axis_match = self.paths.iter().any(|pattern| pattern.matches(actual));
            matched &= axis_match;
            parts.push(format!(
                "paths={:?} actual={actual:?} match={axis_match}",
                self.canonical.paths
            ));
        }
        for (axis, values) in [
            (ScopeAxis::Extensions, &self.canonical.extensions),
            (ScopeAxis::Vendors, &self.canonical.vendors),
            (ScopeAxis::Models, &self.canonical.models),
            (ScopeAxis::Phases, &self.canonical.phases),
        ] {
            if values.is_empty() {
                continue;
            }
            let actual = context.value(axis).unwrap_or_default();
            let axis_match = values.iter().any(|value| value == actual);
            matched &= axis_match;
            parts.push(format!(
                "{}={values:?} actual={actual:?} match={axis_match}",
                axis.context_name()
            ));
        }
        let basis = if parts.is_empty() {
            "scopes=unconstrained".to_owned()
        } else {
            format!("scopes: {}", parts.join("; "))
        };
        Ok(RuleEvaluation {
            matched,
            specificity: self.specificity(context),
            basis,
        })
    }

    fn constrained(&self, axis: ScopeAxis) -> bool {
        !match axis {
            ScopeAxis::Paths => &self.canonical.paths,
            ScopeAxis::Extensions => &self.canonical.extensions,
            ScopeAxis::Vendors => &self.canonical.vendors,
            ScopeAxis::Models => &self.canonical.models,
            ScopeAxis::Phases => &self.canonical.phases,
        }
        .is_empty()
    }

    fn specificity(&self, context: &NormalizedContext) -> SelectorSpecificity {
        let matching_paths = self
            .paths
            .iter()
            .filter(|pattern| {
                context
                    .path
                    .as_deref()
                    .is_some_and(|path| pattern.matches(path))
            })
            .collect::<Vec<_>>();
        let scored_paths: Vec<&PathPattern> = if matching_paths.is_empty() {
            self.paths.iter().collect()
        } else {
            matching_paths
        };
        let path_score = scored_paths
            .into_iter()
            .map(|pattern| pattern.specificity)
            .max()
            .unwrap_or_default();
        SelectorSpecificity {
            constrained_axes: ScopeAxis::ALL
                .into_iter()
                .filter(|axis| self.constrained(*axis))
                .count(),
            path: narrowness_with_path(&self.canonical.paths, path_score),
            extension: narrowness(&self.canonical.extensions),
            vendor: narrowness(&self.canonical.vendors),
            model: narrowness(&self.canonical.models),
            phase: narrowness(&self.canonical.phases),
            normalized_values: [
                self.canonical.paths.clone(),
                self.canonical.extensions.clone(),
                self.canonical.vendors.clone(),
                self.canonical.models.clone(),
                self.canonical.phases.clone(),
            ],
        }
    }
}

struct RuleEvaluation {
    matched: bool,
    specificity: SelectorSpecificity,
    basis: String,
}

#[derive(Debug)]
struct NormalizedContext {
    path: Option<String>,
    extension: Option<String>,
    vendor: Option<String>,
    model: Option<String>,
}

impl NormalizedContext {
    fn new(context: &PolicyContext) -> Result<Self, ResolveError> {
        let path = context
            .path
            .as_deref()
            .map(normalize_path)
            .transpose()
            .map_err(|reason| ResolveError::context(ScopeAxis::Paths, reason))?;
        let extension = match (&context.extension, &path) {
            (Some(extension), _) => Some(normalize_context_extension(extension)?),
            (None, Some(path)) => Some(file_extension(path)),
            (None, None) => None,
        };
        Ok(Self {
            path,
            extension,
            vendor: context
                .vendor
                .as_deref()
                .map(|value| normalize_context_identity(ScopeAxis::Vendors, value))
                .transpose()?,
            model: context
                .model
                .as_deref()
                .map(|value| normalize_context_identity(ScopeAxis::Models, value))
                .transpose()?,
        })
    }

    fn has(&self, axis: ScopeAxis) -> bool {
        self.value(axis).is_some()
    }

    fn value(&self, axis: ScopeAxis) -> Option<&str> {
        match axis {
            ScopeAxis::Paths => self.path.as_deref(),
            ScopeAxis::Extensions => self.extension.as_deref(),
            ScopeAxis::Vendors => self.vendor.as_deref(),
            ScopeAxis::Models => self.model.as_deref(),
            ScopeAxis::Phases => None,
        }
    }
}

/// A current policy document that cannot be compiled without changing meaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    field: String,
    reason: String,
}

impl CompileError {
    fn document(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            reason: reason.into(),
        }
    }

    pub(super) fn rule(rule: &RuleId, field: &str, reason: impl Into<String>) -> Self {
        Self::document(format!("rule[{rule}].{field}"), reason)
    }

    /// Exact current field that was refused.
    #[must_use]
    pub fn field(&self) -> &str {
        &self.field
    }

    /// Why execution would change its meaning.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} is refused: {}", self.field, self.reason)
    }
}

impl std::error::Error for CompileError {}

/// A runtime context or calculation refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveError {
    code: &'static str,
    metric: String,
    rule: Option<RuleId>,
    dimension: Option<ScopeAxis>,
    detail: String,
    recovery: String,
}

impl ResolveError {
    fn missing(rule: &RuleId, metric: &str, dimension: ScopeAxis) -> Self {
        Self {
            code: "policy::context-missing",
            metric: metric.to_owned(),
            rule: Some(rule.clone()),
            dimension: Some(dimension),
            detail: format!("rule {rule} requires {} context", dimension.context_name()),
            recovery: format!(
                "supply PolicyContext::with_{} before resolving {metric}",
                dimension.context_name()
            ),
        }
    }

    pub(super) fn context(dimension: ScopeAxis, detail: impl Into<String>) -> Self {
        Self {
            code: "policy::context-invalid",
            metric: String::new(),
            rule: None,
            dimension: Some(dimension),
            detail: detail.into(),
            recovery: format!(
                "supply one canonical {} through PolicyContext::with_{}",
                dimension.context_name(),
                dimension.context_name()
            ),
        }
    }

    fn calculation(metric: &str, detail: impl Into<String>) -> Self {
        Self {
            code: "policy::calculation-refused",
            metric: metric.to_owned(),
            rule: None,
            dimension: None,
            detail: detail.into(),
            recovery: "reduce the configured multiplier or bound".to_owned(),
        }
    }

    /// Stable refusal code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    /// Metric being resolved, when selection had begun.
    #[must_use]
    pub fn metric(&self) -> &str {
        &self.metric
    }

    /// Rule requiring unavailable context.
    #[must_use]
    pub const fn rule(&self) -> Option<&RuleId> {
        self.rule.as_ref()
    }

    /// Missing or malformed dimension.
    #[must_use]
    pub const fn dimension(&self) -> Option<ScopeAxis> {
        self.dimension
    }

    /// Concrete recovery action.
    #[must_use]
    pub fn recovery(&self) -> &str {
        &self.recovery
    }
}

impl fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.metric.is_empty() {
            write!(
                formatter,
                "{}: {}; {}",
                self.code, self.detail, self.recovery
            )
        } else {
            write!(
                formatter,
                "{} for {}: {}; {}",
                self.code, self.metric, self.detail, self.recovery
            )
        }
    }
}

impl std::error::Error for ResolveError {}

fn validate_document(document: &PolicyDocument) -> Result<(), CompileError> {
    for bound in BoundName::ALL {
        if document.limits.get(bound) == 0 {
            return Err(CompileError::document(
                format!("limits.{bound}"),
                "must be a positive integer",
            ));
        }
    }
    if !document.ratchet.ratio.is_finite() || document.ratchet.ratio < 1.0 {
        return Err(CompileError::document(
            "ratchet.ratio",
            "must be a finite number >= 1.0",
        ));
    }
    for bound in BoundName::ALL {
        if let Some(value) = document.ratchet.explicit(bound)
            && value < document.limits.get(bound)
        {
            return Err(CompileError::document(
                format!("ratchet.{bound}"),
                format!("must be >= limits.{bound}"),
            ));
        }
    }
    if document.magic_examine_threshold == 0 {
        return Err(CompileError::document(
            "magic_examine_threshold",
            "must be a positive integer",
        ));
    }
    for (field, suffixes) in [
        (
            "file_classes.file_shape",
            document.file_classes.file_shape(),
        ),
        (
            "file_classes.complexity",
            document.file_classes.complexity(),
        ),
        ("file_classes.magic", document.file_classes.magic()),
    ] {
        if suffixes.is_empty() {
            return Err(CompileError::document(
                field,
                "must select at least one suffix; an arm offered no paths reports \
                 a clean measurement over nothing",
            ));
        }
        for suffix in suffixes {
            if !suffix.starts_with('.') || suffix.len() == 1 {
                return Err(CompileError::document(
                    field,
                    format!("{suffix:?}: must be a file suffix beginning with a period"),
                ));
            }
        }
    }

    let mut ids = BTreeSet::new();
    let mut orders = BTreeSet::new();
    for rule in &document.rules {
        if !ids.insert(rule.id.clone()) {
            return Err(CompileError::rule(
                &rule.id,
                "id",
                "duplicates an earlier rule ID",
            ));
        }
        if !orders.insert(rule.source_order) {
            return Err(CompileError::rule(
                &rule.id,
                "source_order",
                format!("duplicates source order {}", rule.source_order),
            ));
        }
        validate_source(rule)?;
        for (bound, settings) in &rule.bounds {
            validate_settings(&rule.id, *bound, settings)?;
        }
        if rule
            .magic
            .as_ref()
            .is_some_and(|magic| magic.examine_threshold == 0)
        {
            return Err(CompileError::rule(
                &rule.id,
                "magic.examine_threshold",
                "must be a positive integer",
            ));
        }
    }
    Ok(())
}

fn validate_source(rule: &RuleDeclaration) -> Result<(), CompileError> {
    for (field, value) in [
        ("source.location", &rule.source.location),
        ("source.field", &rule.source.field),
    ] {
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(CompileError::rule(
                &rule.id,
                field,
                "must be nonempty text without control characters",
            ));
        }
    }
    Ok(())
}

fn validate_settings(
    rule: &RuleId,
    bound: BoundName,
    settings: &RuleSettings,
) -> Result<(), CompileError> {
    let field = format!("bounds.{bound}");
    if !settings.multiplier.is_finite() || settings.multiplier < 0.0 {
        return Err(CompileError::rule(
            rule,
            &format!("{field}.multiplier"),
            "must be a finite number >= 0.0",
        ));
    }
    if settings.minimum == Some(0) {
        return Err(CompileError::rule(
            rule,
            &format!("{field}.minimum"),
            "must be a positive integer",
        ));
    }
    if settings.maximum == Some(0) {
        return Err(CompileError::rule(
            rule,
            &format!("{field}.maximum"),
            "must be a positive integer",
        ));
    }
    if matches!((settings.minimum, settings.maximum), (Some(min), Some(max)) if min > max) {
        return Err(CompileError::rule(
            rule,
            &field,
            "minimum must be <= maximum",
        ));
    }
    if settings
        .ratchet
        .is_some_and(|value| !value.is_finite() || value < 1.0)
    {
        return Err(CompileError::rule(
            rule,
            &format!("{field}.ratchet"),
            "must be a finite number >= 1.0",
        ));
    }
    Ok(())
}

fn narrowness(values: &[String]) -> (bool, i64) {
    (!values.is_empty(), -(values.len() as i64))
}

fn narrowness_with_path(
    values: &[String],
    specificity: PathSpecificity,
) -> (bool, PathSpecificity, i64) {
    (
        !values.is_empty(),
        specificity,
        if values.is_empty() {
            0
        } else {
            -(values.len() as i64)
        },
    )
}

fn scaled(base: u64, multiplier: f64, metric: &str) -> Result<u64, ResolveError> {
    let scaled = (base as f64) * multiplier;
    if !scaled.is_finite() || scaled >= u64::MAX as f64 {
        return Err(ResolveError::calculation(
            metric,
            format!("{base} multiplied by {multiplier} exceeds u64"),
        ));
    }
    Ok(scaled as u64)
}
