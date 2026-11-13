use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{LazyLock, OnceLock};

use pulldown_cmark::{Event, Parser as MarkdownParser, Tag, TagEnd};
use regex::Regex;
#[cfg(not(target_arch = "wasm32"))]
use tree_sitter::{Language, Node, Parser};

use super::{ParseError, canonical_path, source_line};
#[cfg(not(target_arch = "wasm32"))]
use crate::metrics::declaration::RUST_DOC_MARKDOWN_CITATION_ALLOWANCES;

/// One invalid tracked Markdown link target.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MarkdownLinkFinding {
    source_path: String,
    line: u64,
    raw_target: String,
    resolved_path: String,
    expected: String,
    fragment: Option<String>,
}

impl MarkdownLinkFinding {
    #[must_use]
    pub fn source_path(&self) -> &str {
        &self.source_path
    }

    #[must_use]
    pub const fn line(&self) -> u64 {
        self.line
    }

    #[must_use]
    pub fn raw_target(&self) -> &str {
        &self.raw_target
    }

    #[must_use]
    pub fn resolved_path(&self) -> &str {
        &self.resolved_path
    }

    #[must_use]
    pub fn expected(&self) -> &str {
        &self.expected
    }

    #[must_use]
    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }
}

/// Heading slugs and explicit anchor identifiers for tracked Markdown documents.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MarkdownAnchorInventory {
    by_path: BTreeMap<String, BTreeSet<String>>,
}

impl MarkdownAnchorInventory {
    /// Add one tracked Markdown document to the inventory.
    ///
    /// # Errors
    ///
    /// Refuses a noncanonical/non-ASCII path or a duplicate document.
    pub fn insert(&mut self, path: &str, text: &str) -> Result<(), ParseError> {
        let path = canonical_path(path)?;
        require_ascii_path(&path, "tracked Markdown anchor inventory")?;
        if self.by_path.contains_key(&path) {
            return Err(ParseError::collision(
                path.clone(),
                format!("tracked Markdown document {path:?} was indexed more than once"),
            ));
        }
        self.by_path.insert(path, anchor_targets(text));
        Ok(())
    }

    fn contains(&self, path: &str, fragment: &str) -> Option<bool> {
        self.by_path
            .get(path)
            .map(|targets| targets.contains(fragment))
    }
}

/// One Rust doc-comment citation whose Markdown target is not tracked or allowed.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RustDocMarkdownCitationFinding {
    source_path: String,
    line: u64,
    target_path: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl RustDocMarkdownCitationFinding {
    #[must_use]
    pub fn source_path(&self) -> &str {
        &self.source_path
    }

    #[must_use]
    pub const fn line(&self) -> u64 {
        self.line
    }

    #[must_use]
    pub fn target_path(&self) -> &str {
        &self.target_path
    }
}

/// Complete scan of one Rust source's doc-comment Markdown citations.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RustDocMarkdownCitationScan {
    citations: u64,
    findings: Vec<RustDocMarkdownCitationFinding>,
}

#[cfg(not(target_arch = "wasm32"))]
impl RustDocMarkdownCitationScan {
    #[must_use]
    pub const fn citations(&self) -> u64 {
        self.citations
    }

    #[must_use]
    pub fn findings(&self) -> &[RustDocMarkdownCitationFinding] {
        &self.findings
    }
}

/// Scan one tracked Markdown source against a complete projected inventory.
///
/// # Errors
///
/// Missing/non-Markdown sources are not-applicable. Non-ASCII paths are a
/// release-owned refusal because Rust lowercase is not Python Unicode
/// `casefold`; substituting it would change target identity. A tracked Markdown
/// target whose contents were unavailable to the anchor inventory is likewise
/// refused rather than treated as a document with no anchors.
pub fn scan_markdown_links(
    source_path: &str,
    text: &str,
    tracked_paths: &[String],
    anchors: &MarkdownAnchorInventory,
) -> Result<Vec<MarkdownLinkFinding>, ParseError> {
    let source_path = canonical_path(source_path)?;
    if !source_path.to_ascii_lowercase().ends_with(".md") {
        return Err(ParseError::NotApplicable {
            reason: format!("tracked source {source_path:?} is not Markdown"),
        });
    }
    require_ascii_path(&source_path, "tracked Markdown source")?;
    let tracked = tracked_case_map(tracked_paths)?;
    let mut findings = Vec::new();
    for (event, range) in MarkdownParser::new(text).into_offset_iter() {
        let destination = match event {
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                dest_url.into_string()
            }
            _ => continue,
        };
        let Some(target) = resolve_target(&source_path, &destination)? else {
            continue;
        };
        let Some(expected_path) = tracked.get(&target.path.to_ascii_lowercase()) else {
            continue;
        };
        let line = source_line(text, range.start);
        if expected_path != &target.path {
            findings.push(MarkdownLinkFinding {
                source_path: source_path.clone(),
                line,
                raw_target: destination.clone(),
                resolved_path: target.path.clone(),
                expected: expected_path.clone(),
                fragment: None,
            });
        }
        let Some(fragment) = target.fragment else {
            continue;
        };
        if !expected_path.to_ascii_lowercase().ends_with(".md") {
            continue;
        }
        let Some(present) = anchors.contains(expected_path, &fragment) else {
            return Err(ParseError::unsupported(
                "tracked Markdown anchor inventory",
                format!("target document {expected_path:?} could not be read"),
                "restore the tracked document and rerun the gate",
            ));
        };
        if !present {
            findings.push(MarkdownLinkFinding {
                source_path: source_path.clone(),
                line,
                raw_target: destination,
                resolved_path: expected_path.clone(),
                expected: format!("heading or explicit anchor #{fragment} in {expected_path}"),
                fragment: Some(fragment),
            });
        }
    }
    findings.sort();
    Ok(findings)
}

/// Scan Markdown paths named in Rust doc comments against tracked paths.
///
/// A Markdown path spelled in doc text is a citation whether or not it sits in
/// a code span, because a code span is typography and carries no intent. The
/// prose this arm exists to catch can ground a rule in `docs/README.md`
/// without a code span just as readily as with one, so reading only the
/// decorated spelling would refuse citations that were merely typeset and admit
/// the ones doing the work.
///
/// What separates a citation from an incidental mention is therefore not the
/// punctuation around the name but whether the name resolves. Naming a document
/// in doc text sends the reader to it, so it has to be one they can open here.
/// A spelling absent from the live inventory is clean only when an exact
/// compiled declaration says who owns it instead -- an external document this
/// repository does not track, or a path a test plants rather than one a reader
/// is being sent to.
///
/// Line and block doc comments are both admitted. Ordinary comments and string
/// literals are not doc text and never enter the candidate set.
///
/// # Errors
///
/// Non-Rust sources are not applicable. Noncanonical or non-ASCII source,
/// target, or inventory paths refuse rather than acquiring a different path
/// identity from the one reports retain.
#[cfg(not(target_arch = "wasm32"))]
pub fn scan_rust_doc_markdown_citations(
    source_path: &str,
    text: &str,
    tracked_paths: &[String],
) -> Result<RustDocMarkdownCitationScan, ParseError> {
    let source_path = canonical_path(source_path)?;
    if !source_path.to_ascii_lowercase().ends_with(".rs") {
        return Err(ParseError::NotApplicable {
            reason: format!("tracked source {source_path:?} is not Rust"),
        });
    }
    require_ascii_path(&source_path, "tracked Rust source")?;
    let mut parser = Parser::new();
    parser
        .set_language(&Language::new(tree_sitter_rust::LANGUAGE))
        .map_err(|error| {
            ParseError::malformed(
                format!("Rust doc-comment grammar for {source_path:?}"),
                error.to_string(),
            )
        })?;
    let tree = parser.parse(text, None).ok_or_else(|| {
        ParseError::malformed(
            format!("Rust doc-comment source {source_path:?}"),
            "grammar produced no tree",
        )
    })?;
    let tracked = tracked_case_map(tracked_paths)?
        .into_values()
        .collect::<BTreeSet<_>>();
    let mut citations = 0_u64;
    let mut findings = Vec::new();
    visit_rust_doc_comments(
        tree.root_node(),
        text,
        &source_path,
        &tracked,
        &mut citations,
        &mut findings,
    )?;
    findings.sort();
    Ok(RustDocMarkdownCitationScan {
        citations,
        findings,
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn visit_rust_doc_comments(
    node: Node<'_>,
    text: &str,
    source_path: &str,
    tracked: &BTreeSet<String>,
    citations: &mut u64,
    findings: &mut Vec<RustDocMarkdownCitationFinding>,
) -> Result<(), ParseError> {
    if node.kind() == "doc_comment" {
        let fragment = node.utf8_text(text.as_bytes()).map_err(|error| {
            ParseError::malformed(
                format!("Rust doc comment in {source_path:?}"),
                error.to_string(),
            )
        })?;
        for captures in rust_doc_citation_regex().captures_iter(fragment) {
            let matched = captures.get(1).expect("citation regex has one capture");
            let raw = matched.as_str();
            let target_path = canonical_path(raw)?;
            require_ascii_path(&target_path, "Rust doc Markdown target")?;
            *citations = citations.saturating_add(1);
            let allowed = tracked.contains(&target_path)
                || RUST_DOC_MARKDOWN_CITATION_ALLOWANCES
                    .into_iter()
                    .any(|allowance| allowance.matches(source_path, &target_path));
            if !allowed {
                findings.push(RustDocMarkdownCitationFinding {
                    source_path: source_path.to_owned(),
                    line: rust_source_line(text, node.start_byte() + matched.start()),
                    target_path,
                });
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_rust_doc_comments(child, text, source_path, tracked, citations, findings)?;
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn rust_source_line(text: &str, byte_offset: usize) -> u64 {
    let newlines = text.as_bytes()[..byte_offset]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count();
    u64::try_from(newlines + 1).expect("line number fits in u64")
}

fn anchor_targets(text: &str) -> BTreeSet<String> {
    let mut targets = BTreeSet::new();
    let mut used_heading_slugs = BTreeSet::new();
    let mut heading = None;
    for event in MarkdownParser::new(text) {
        match event {
            Event::Start(Tag::Heading { .. }) => heading = Some(String::new()),
            Event::End(TagEnd::Heading(_)) => {
                if let Some(text) = heading.take() {
                    let base = heading_slug(&text);
                    let mut slug = base.clone();
                    let mut suffix = 1_u64;
                    while !used_heading_slugs.insert(slug.clone()) {
                        slug = format!("{base}-{suffix}");
                        suffix = suffix.saturating_add(1);
                    }
                    targets.insert(slug);
                }
            }
            Event::Text(text) | Event::Code(text) | Event::FootnoteReference(text) => {
                if let Some(heading) = heading.as_mut() {
                    heading.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(heading) = heading.as_mut() {
                    heading.push(' ');
                }
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                for captures in explicit_anchor_regex().captures_iter(&html) {
                    let id = captures
                        .get(1)
                        .or_else(|| captures.get(2))
                        .or_else(|| captures.get(3))
                        .expect("explicit anchor regex captures one id");
                    targets.insert(id.as_str().to_owned());
                }
            }
            _ => {}
        }
    }
    targets
}

/// Derive a heading's anchor as the published renderer does: keep letters,
/// marks, digits, connector punctuation and hyphens, turn each space into a
/// hyphen, drop everything else, and lowercase what remains.
fn heading_slug(text: &str) -> String {
    let text = text.trim();
    let mut slug = String::with_capacity(text.len());
    let mut kept = 0;
    for dropped in dropped_slug_regex().find_iter(text) {
        push_slug_run(&mut slug, &text[kept..dropped.start()]);
        kept = dropped.end();
    }
    push_slug_run(&mut slug, &text[kept..]);
    slug
}

fn push_slug_run(slug: &mut String, run: &str) {
    for character in run.chars() {
        if character == ' ' {
            slug.push('-');
        } else {
            slug.extend(character.to_lowercase());
        }
    }
}

fn dropped_slug_regex() -> &'static Regex {
    static REGEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"[^\p{Alphabetic}\p{M}\p{Nd}\p{Pc}\- ]+")
            .expect("compiled heading slug regex is valid")
    });
    &REGEX
}

fn explicit_anchor_regex() -> &'static Regex {
    static REGEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?is)<a\b[^>]*\bid\s*=\s*(?:"([^"]+)"|'([^']+)'|([^\s>]+))"#)
            .expect("compiled explicit Markdown anchor regex is valid")
    });
    &REGEX
}

fn tracked_case_map(paths: &[String]) -> Result<BTreeMap<String, String>, ParseError> {
    let mut tracked = BTreeMap::new();
    for path in paths {
        let path = canonical_path(path)?;
        require_ascii_path(&path, "tracked path inventory")?;
        let folded = path.to_ascii_lowercase();
        if let Some(previous) = tracked.insert(folded.clone(), path.clone())
            && previous != path
        {
            return Err(ParseError::collision(
                folded,
                format!("tracked paths {previous:?} and {path:?} differ only by case"),
            ));
        }
    }
    Ok(tracked)
}

fn require_ascii_path(path: &str, field: &str) -> Result<(), ParseError> {
    if path.is_ascii() {
        Ok(())
    } else {
        Err(ParseError::unsupported(
            field,
            format!("path {path:?} requires Python Unicode casefold semantics"),
            "rename the tracked path to ASCII or compile an exact Unicode casefold provider",
        ))
    }
}

fn url_scheme_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^[A-Za-z][A-Za-z0-9+.-]*:").expect("compiled URL scheme regex is valid")
    })
}

/// A Markdown path in doc text, decorated or bare.
///
/// The trailing `\b` ends the match at the name. Without it the pattern reads a
/// Markdown name out of the middle of any longer suffix, so a bundle spelled
/// `page.mdoc` would be refused for not tracking a document nobody wrote.
#[cfg(not(target_arch = "wasm32"))]
fn rust_doc_citation_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"((?:[A-Za-z0-9_.-]+/)*[A-Za-z0-9_.-]+\.md)\b")
            .expect("compiled Rust doc Markdown citation regex is valid")
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedTarget {
    path: String,
    fragment: Option<String>,
}

fn resolve_target(source: &str, raw: &str) -> Result<Option<ResolvedTarget>, ParseError> {
    let stripped = raw.trim();
    if stripped.is_empty() || url_scheme_regex().is_match(stripped) || stripped.starts_with('/') {
        return Ok(None);
    }
    let (without_fragment, raw_fragment) = stripped
        .split_once('#')
        .map_or((stripped, None), |(path, fragment)| (path, Some(fragment)));
    let fragment = raw_fragment
        .filter(|fragment| !fragment.is_empty())
        .map(decode_fragment)
        .transpose()?;
    let path_target = without_fragment
        .split_once('?')
        .map_or(without_fragment, |(path, _)| path)
        .trim();
    if path_target.is_empty() {
        return Ok(fragment.map(|fragment| ResolvedTarget {
            path: source.to_owned(),
            fragment: Some(fragment),
        }));
    }
    require_ascii_path(path_target, "markdown link target")?;
    let parent = Path::new(source).parent().unwrap_or_else(|| Path::new(""));
    Ok(normalize_relative(&parent.to_string_lossy(), path_target)?
        .map(|path| ResolvedTarget { path, fragment }))
}

fn decode_fragment(fragment: &str) -> Result<String, ParseError> {
    if !fragment.as_bytes().contains(&b'%') {
        return Ok(fragment.to_owned());
    }
    let bytes = fragment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes[offset] != b'%' {
            decoded.push(bytes[offset]);
            offset += 1;
            continue;
        }
        let Some(pair) = bytes.get(offset + 1..offset + 3) else {
            return Err(ParseError::malformed(
                "Markdown link fragment",
                format!("{fragment:?} ends with an incomplete percent escape"),
            ));
        };
        let Some(high) = hex_value(pair[0]) else {
            return Err(ParseError::malformed(
                "Markdown link fragment",
                format!("{fragment:?} contains a nonhex percent escape"),
            ));
        };
        let Some(low) = hex_value(pair[1]) else {
            return Err(ParseError::malformed(
                "Markdown link fragment",
                format!("{fragment:?} contains a nonhex percent escape"),
            ));
        };
        decoded.push((high << 4) | low);
        offset += 3;
    }
    String::from_utf8(decoded).map_err(|error| {
        ParseError::malformed(
            "Markdown link fragment",
            format!("{fragment:?} does not decode as UTF-8: {error}"),
        )
    })
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn normalize_relative(parent: &str, target: &str) -> Result<Option<String>, ParseError> {
    let mut parts = Vec::new();
    for part in parent.split('/').chain(target.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Ok(None);
                }
            }
            value => parts.push(value),
        }
    }
    if parts.is_empty() {
        Ok(Some(".".to_owned()))
    } else {
        canonical_path(&parts.join("/")).map(Some)
    }
}
