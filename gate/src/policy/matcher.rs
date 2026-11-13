//! Exact supported path and stem matching dialect.

use regex::Regex;

use super::runtime::{CompileError, ResolveError};
use super::{PathSpecificity, REGEX_DIALECT_NAME, RuleId, ScopeAxis};

#[derive(Debug, Clone)]
pub(crate) struct PathPattern {
    pub(super) source: String,
    literal: bool,
    expressions: Vec<Regex>,
    pub(super) specificity: PathSpecificity,
}

impl PathPattern {
    /// Compile one tracked repository-path selector outside the rule model.
    ///
    /// Every tracked selector shares this contract, so a rule scope and a
    /// `[policy]` path list cannot drift into two dialects. Nothing here is
    /// gitignore syntax: a leading `!` is an ordinary character, not a
    /// negation, and a pattern spelled that way selects the file literally
    /// named that or nothing at all.
    pub(crate) fn tracked(raw: &str) -> Result<Self, String> {
        let source = normalize_path(raw).map_err(|reason| format!("{raw:?}: {reason}"))?;
        let literal = !has_glob_magic(&source);
        let expressions = if literal {
            Vec::new()
        } else {
            leading_double_star_variants(&source)
                .into_iter()
                .map(|variant| {
                    translate_fnmatch(variant).and_then(|translated| {
                        Regex::new(&translated).map_err(|error| error.to_string())
                    })
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|reason| format!("pattern {source:?} is unsupported: {reason}"))?
        };
        let specificity = path_specificity(&source);
        Ok(Self {
            source,
            literal,
            expressions,
            specificity,
        })
    }

    fn compile(rule: &RuleId, raw: &str) -> Result<Self, CompileError> {
        Self::tracked(raw).map_err(|reason| CompileError::rule(rule, "scopes.paths", reason))
    }

    pub(crate) fn matches(&self, path: &str) -> bool {
        if self.literal {
            let prefix = self.source.trim_end_matches('/');
            path == prefix
                || path
                    .strip_prefix(prefix)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        } else {
            self.expressions
                .iter()
                .any(|expression| expression.is_match(path))
        }
    }

    /// Whether this selector matches the path itself or any ancestor of it.
    ///
    /// A selector that names a directory root selects that directory's whole
    /// subtree. A literal pattern already does so on its own; a glob pattern
    /// matches one concrete directory, so its descendants are recognized by
    /// asking the same question of each ancestor in turn.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn matches_or_ancestor(&self, path: &str) -> bool {
        let mut candidate = path;
        loop {
            if self.matches(candidate) {
                return true;
            }
            match candidate.rfind('/') {
                Some(slash) => candidate = &candidate[..slash],
                None => return false,
            }
        }
    }

    /// Whether this selector matches a strict ancestor of the path.
    ///
    /// This is the scanning question rather than the membership question: a
    /// declared root is the thing being scanned, so the root itself is not one
    /// of its own findings.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn matches_ancestor(&self, path: &str) -> bool {
        path.rsplit_once('/')
            .is_some_and(|(parent, _)| self.matches_or_ancestor(parent))
    }
}

pub(super) fn compile_stem_pattern(rule: &RuleId, pattern: &str) -> Result<Regex, CompileError> {
    compile_stem_expression(pattern)
        .map_err(|reason| CompileError::rule(rule, "stem_pattern", reason))
}

pub(crate) fn compile_stem_expression(pattern: &str) -> Result<Regex, String> {
    let anchored = format!(r"\A(?:{pattern})\z");
    Regex::new(&anchored).map_err(|error| {
        format!(
            concat!(
                "pattern {:?} is unsupported by {}: ",
                "{}; rewrite it in Rust regex 1.x syntax"
            ),
            pattern, REGEX_DIALECT_NAME, error
        )
    })
}

pub(super) fn normalize_paths(
    rule: &RuleId,
    values: &[String],
) -> Result<Vec<PathPattern>, CompileError> {
    let mut normalized = values
        .iter()
        .map(|value| PathPattern::compile(rule, value))
        .collect::<Result<Vec<_>, _>>()?;
    normalized.sort_by(|left, right| left.source.cmp(&right.source));
    normalized.dedup_by(|left, right| left.source == right.source);
    Ok(normalized)
}

pub(super) fn normalize_extensions(
    rule: &RuleId,
    values: &[String],
) -> Result<Vec<String>, CompileError> {
    let mut normalized = values
        .iter()
        .map(|raw| {
            let value = raw.trim().to_ascii_lowercase();
            if !raw.is_ascii()
                || value.len() < 2
                || !value.starts_with('.')
                || value.contains(['/', '\\'])
                || value[1..].contains('.')
            {
                return Err(CompileError::rule(
                    rule,
                    "scopes.extensions",
                    format!("contains unsupported suffix {raw:?}; use one ASCII final suffix"),
                ));
            }
            Ok(value)
        })
        .collect::<Result<Vec<_>, _>>()?;
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

pub(super) fn normalize_identities(
    rule: &RuleId,
    axis: ScopeAxis,
    values: &[String],
) -> Result<Vec<String>, CompileError> {
    let mut normalized = values
        .iter()
        .map(|raw| {
            let value = raw.trim().to_ascii_lowercase();
            if !raw.is_ascii() || value.is_empty() {
                return Err(CompileError::rule(
                    rule,
                    &format!("scopes.{}", axis.selector_name()),
                    format!("contains unsupported identity {raw:?}; use nonempty ASCII"),
                ));
            }
            Ok(value)
        })
        .collect::<Result<Vec<_>, _>>()?;
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

pub(super) fn normalize_path(raw: &str) -> Result<String, String> {
    let replaced = raw.trim().replace('\\', "/");
    let value = replaced.strip_prefix("./").unwrap_or(&replaced).to_owned();
    let first = value.split('/').next().unwrap_or_default();
    if value.is_empty()
        || value == "."
        || value.starts_with('/')
        || (first.len() == 2 && first.ends_with(':'))
        || value.split('/').any(|part| part == "..")
        || value.chars().any(char::is_control)
    {
        return Err(
            "must be a nonempty repository-relative path without '..' or control characters"
                .to_owned(),
        );
    }
    Ok(value)
}

pub(super) fn normalize_context_extension(raw: &str) -> Result<String, ResolveError> {
    let value = raw.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    if !value.is_ascii()
        || value.len() < 2
        || !value.starts_with('.')
        || value.contains(['/', '\\'])
        || value[1..].contains('.')
    {
        return Err(ResolveError::context(
            ScopeAxis::Extensions,
            format!("unsupported extension {raw:?}"),
        ));
    }
    Ok(value.to_ascii_lowercase())
}

pub(super) fn normalize_context_identity(
    axis: ScopeAxis,
    raw: &str,
) -> Result<String, ResolveError> {
    let value = raw.trim().to_ascii_lowercase();
    if !raw.is_ascii() || value.is_empty() {
        return Err(ResolveError::context(
            axis,
            format!("unsupported identity {raw:?}"),
        ));
    }
    Ok(value)
}

fn has_glob_magic(value: &str) -> bool {
    value.contains(['*', '?', '['])
}

fn path_specificity(pattern: &str) -> PathSpecificity {
    PathSpecificity {
        priority: 0,
        literal_pattern: !has_glob_magic(pattern),
        literal_characters: pattern
            .chars()
            .filter(|character| !matches!(character, '*' | '?' | '[' | ']' | '!'))
            .count(),
        segments: pattern
            .split('/')
            .filter(|segment| !segment.is_empty())
            .count(),
        pattern_length: pattern.chars().count(),
    }
}

fn leading_double_star_variants(pattern: &str) -> Vec<&str> {
    let mut variants = vec![pattern];
    let mut shortened = pattern;
    while let Some(rest) = shortened.strip_prefix("**/") {
        shortened = rest;
        variants.push(shortened);
    }
    variants
}

fn translate_fnmatch(pattern: &str) -> Result<String, String> {
    let characters: Vec<char> = pattern.chars().collect();
    let mut translated = String::from(r"\A(?s:");
    let mut index = 0;
    while index < characters.len() {
        match characters[index] {
            '*' => {
                translated.push_str(".*");
                index += 1;
                while characters.get(index) == Some(&'*') {
                    index += 1;
                }
            }
            '?' => {
                translated.push('.');
                index += 1;
            }
            '[' => {
                let Some((class, next)) = translate_class(&characters, index)? else {
                    translated.push_str(r"\[");
                    index += 1;
                    continue;
                };
                translated.push_str(&class);
                index = next;
            }
            character => {
                translated.push_str(&regex::escape(&character.to_string()));
                index += 1;
            }
        }
    }
    translated.push_str(r")\z");
    Ok(translated)
}

fn translate_class(characters: &[char], opening: usize) -> Result<Option<(String, usize)>, String> {
    let mut closing = opening + 1;
    if characters.get(closing) == Some(&'!') {
        closing += 1;
    }
    if characters.get(closing) == Some(&']') {
        closing += 1;
    }
    while closing < characters.len() && characters[closing] != ']' {
        closing += 1;
    }
    if closing == characters.len() {
        return Ok(None);
    }
    let mut stuff = characters[opening + 1..closing].to_vec();
    let negated = stuff.first() == Some(&'!');
    if negated {
        stuff.remove(0);
    }
    if stuff.is_empty() {
        return Err("empty character classes have no exact Rust-regex spelling".to_owned());
    }
    if stuff.iter().any(|character| !character.is_ascii()) {
        return Err("non-ASCII character classes require Python case/range semantics".to_owned());
    }
    let mut class = String::from("[");
    if negated {
        class.push('^');
    }
    let mut index = 0;
    while index < stuff.len() {
        if index + 2 < stuff.len() && stuff[index + 1] == '-' {
            let start = stuff[index];
            let end = stuff[index + 2];
            if start == '-' || end == '-' || start > end {
                return Err(format!("unsupported character-class range {start}-{end}"));
            }
            push_class_literal(&mut class, start);
            class.push('-');
            push_class_literal(&mut class, end);
            index += 3;
        } else {
            push_class_literal(&mut class, stuff[index]);
            index += 1;
        }
    }
    class.push(']');
    Ok(Some((class, closing + 1)))
}

fn push_class_literal(into: &mut String, character: char) {
    if matches!(character, '\\' | ']' | '[' | '^' | '-' | '&' | '~' | '|') {
        into.push('\\');
    }
    into.push(character);
}

pub(super) fn file_extension(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    let Some(dot) = file.rfind('.') else {
        return String::new();
    };
    if dot == 0 || dot + 1 == file.len() {
        String::new()
    } else {
        file[dot..].to_ascii_lowercase()
    }
}

pub(super) fn file_stem(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    let Some(dot) = file.rfind('.') else {
        return file.to_owned();
    };
    if dot == 0 {
        file.to_owned()
    } else {
        file[..dot].to_owned()
    }
}

pub(super) fn normalize_phases(
    rule: &RuleId,
    values: &[String],
) -> Result<Vec<String>, CompileError> {
    let mut normalized = values
        .iter()
        .map(|raw| raw.trim().to_ascii_lowercase().replace('_', "-"))
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    for phase in &normalized {
        validate_phase(rule, phase)?;
    }
    Ok(normalized)
}

fn validate_phase(rule: &RuleId, phase: &str) -> Result<(), CompileError> {
    if is_supported_phase(phase) {
        Ok(())
    } else {
        Err(CompileError::rule(
            rule,
            "scopes.phases",
            format!(
                "contains unsupported phase {phase:?}; expected pre-commit or pre-commit-success"
            ),
        ))
    }
}

fn is_supported_phase(phase: &str) -> bool {
    phase == "pre-commit" || phase == "pre-commit-success"
}
