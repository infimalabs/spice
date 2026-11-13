//! The compiled environment law both environment arms hold one line to.
//!
//! [`super::PACKAGED_ENV_PROFILE`] ships its patterns in a Python `re`
//! dialect, and two constructs of that dialect have no equivalent in the
//! engine this build compiles with. Both are answered here rather than
//! dropped. The escape guard `(?<!\\)` is lifted out of the spelling and
//! re-applied to every match it would have rejected. A quoted literal the
//! profile holds together with a back-reference is compiled once per quote
//! character, which accepts exactly the literals one back-referenced spelling
//! accepts.
//!
//! A packaged spelling this module cannot compile is a defect in the profile
//! rather than an observation about a tree, so it panics at first use and a
//! test compiles every packaged spelling to prove that cannot ship.
//!
//! Nothing here reads a file or produces a finding. [`super::analyze`] owns
//! selection and outcome; this module owns what source text says about the
//! environment, including literal-only Rust concatenations that cross lines.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;
use tree_sitter::{Language, Parser};

use super::{ENV_NAME_LITERAL_MARKER, PACKAGED_ENV_PROFILE, column_of};

/// The compiled form of the packaged environment include globs, in order.
pub(super) const ENV_EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "cs", "go", "h", "hpp", "java", "js", "kt", "m", "mm", "rs", "scala",
    "swift", "ts", "lua", "php", "py", "rb", "bash", "sh", "zsh",
];

/// The literal shape every environment name in the ledger has.
const NAME_LITERAL: &str = "[A-Z_][A-Z0-9_]*";

/// The escape guard the packaged shell spellings open with.
const ESCAPE_GUARD: &str = r"(?<!\\)";

/// The hole a template carries where the captured name belongs.
const NAME_HOLE: &str = "{name}";

/// The hole a template carries where a quoted captured name belongs.
const QUOTED_HOLE: &str = "{quoted}";

/// The family whose destructuring idiom binds several names at once.
const JAVASCRIPT: &str = "javascript";

/// Access-site name templates per family, in profile order.
///
/// These read the literal name out of an access rather than deciding whether
/// the access is one, which is what [`access_patterns`] does. A family absent
/// here has access sites the ledger reads no name from.
///
/// [`access_patterns`]: super::PackagedEnvironmentProfile::access_patterns
const ACCESS_NAME_TEMPLATES: &[(&str, &[&str])] = &[
    (
        "python",
        &[
            r"\bos\.(?:getenv|putenv|unsetenv)\(\s*{quoted}",
            r"\bos\.environ\s*\[\s*{quoted}",
            r"\bos\.environ\.(?:get|pop|setdefault)\(\s*{quoted}",
        ],
    ),
    (
        "csharp",
        &[concat!(
            r"\b(?:System\.)?Environment\.",
            r"(?:GetEnvironmentVariable|SetEnvironmentVariable)\(\s*{quoted}"
        )],
    ),
    ("lua", &[r"\bos\.getenv\(\s*{quoted}"]),
    (
        "javascript",
        &[
            r"\bprocess\.env\s*\[\s*{quoted}\s*\]",
            r"\bprocess\.env\.{name}\b",
        ],
    ),
    (
        "shell",
        &[
            r"\bexport\s+{name}=",
            r"(?<!\\)\$\{{name}(?::[^}]*)?\}",
            r"(?<!\\)\${name}\b",
        ],
    ),
];

/// Declaration-site templates that bind one environment name to a symbol.
const DECLARATION_NAME_TEMPLATES: &[&str] = &[
    r"\b[A-Z][A-Z0-9_]*_ENV\s*=\s*(?:\(\s*)?{quoted}",
    r"\b(?:bin_env|thread_id_env)\s*=\s*{quoted}",
];

/// The idioms that make a line about the environment without naming an access.
const CONTEXT_TEMPLATE: &str = concat!(
    r"\bbase_env\b|\bos\.environ\b|\benviron\b|",
    r"\b(?:setenv|delenv|putenv|unsetenv)\b|",
    r"\benv\s*(?:\[|\.get\(|\.pop\(|\.setdefault\()|process\.env"
);

/// Every quoted literal a line already about the environment names.
const CONTEXT_NAME_TEMPLATE: &str = QUOTED_HOLE;

/// The JavaScript idiom that binds several environment names in one pattern.
const DESTRUCTURED_TEMPLATE: &str = r"\{(?P<body>[^}]+)\}\s*=\s*process\.env\b";

/// One environment access one line commits, at the column it commits it.
pub(super) struct Access {
    /// One-based character column the access begins at.
    pub(super) column: u32,
    /// Literal environment name, or the family idiom for a dynamic read.
    pub(super) name: String,
}

/// One compiled pattern and the escape guard its packaged spelling asked for.
struct Guarded {
    pattern: Regex,
    guarded: bool,
}

impl Guarded {
    /// Compile one packaged spelling, lifting its escape guard out of it.
    fn compile(spelling: &str) -> Self {
        let (guarded, body) = spelling
            .strip_prefix(ESCAPE_GUARD)
            .map_or((false, spelling), |body| (true, body));
        let pattern = Regex::new(body).unwrap_or_else(|error| {
            panic!("packaged environment pattern {spelling:?} does not compile: {error}")
        });
        Self { pattern, guarded }
    }

    /// Whether the guard admits one match beginning at `start`.
    ///
    /// The profile spells this as a negative look-behind for one backslash,
    /// so a match the source escaped is not an access.
    fn admits(&self, line: &str, start: usize) -> bool {
        !self.guarded || !line[..start].ends_with('\\')
    }

    /// Where every admitted match of this pattern begins in one line.
    fn starts(&self, line: &str) -> Vec<usize> {
        self.pattern
            .find_iter(line)
            .map(|found| found.start())
            .filter(|start| self.admits(line, *start))
            .collect()
    }

    /// Every admitted `name` capture of this pattern, by where the name begins.
    fn names(&self, line: &str) -> Vec<(usize, String)> {
        self.pattern
            .captures_iter(line)
            .filter_map(|captures| {
                let whole = captures.get(0)?;
                let name = captures.name("name")?;
                self.admits(line, whole.start())
                    .then(|| (name.start(), name.as_str().to_owned()))
            })
            .collect()
    }
}

/// The language family one packaged extension belongs to, if any.
///
/// An extension in the include set with no family is scanned all the same: the
/// watchlist reads it, and only the family-scoped access gate stays silent,
/// because there is no idiom to recognize an access by.
pub(super) fn family(extension: &str) -> Option<&'static str> {
    let suffix = format!(".{extension}");
    PACKAGED_ENV_PROFILE
        .family_suffixes
        .iter()
        .find(|(_, suffixes)| suffixes.contains(&suffix.as_str()))
        .map(|(family, _)| *family)
}

/// Every one-based line of one decoded file the waiver marker covers.
///
/// A line carrying the marker is waived outright. Beyond that the marker waives
/// the whole statement it sits in, and a standalone marker comment waives the
/// statement that follows it, so a read spread over several lines does not have
/// to repeat the waiver on whichever line the scanner happens to catch.
pub(super) fn waived_lines(text: &str) -> BTreeSet<u32> {
    let marker = PACKAGED_ENV_PROFILE.allow_marker;
    let lines: Vec<&str> = text.lines().collect();
    let carried: BTreeSet<usize> = numbered(&lines, |line| line.contains(marker));
    let standalone: BTreeSet<usize> = numbered(&lines, |line| standalone_waiver(line, marker));
    let mut waived = carried.clone();
    for (start, end) in statement_spans(&lines) {
        if standalone.contains(&(start - 1)) || (start..=end).any(|line| carried.contains(&line)) {
            waived.extend(start..=end);
        }
    }
    waived
        .into_iter()
        .filter_map(|line| u32::try_from(line).ok())
        .collect()
}

/// Every environment access one line commits, by one-based character column.
///
/// The line is read by two extractors that compose rather than pre-empt each
/// other. The watchlist names the literals it caught; the family access gate
/// finds the idioms an access is spelled with, which is what catches a read
/// under a name nobody wrote down. Each literal belongs to the access it sits
/// inside, which is the last gate match beginning at or before it, so a read
/// naming a watchlisted literal inline is the one access it reads as, not two.
///
/// A gate match owning no literal is a dynamic read and reports under the
/// family idiom. A literal owned by no gate match, which is every literal in a
/// file of no declared family, reports under its own name. Nothing else is
/// suppressed: two reads on one line are two accesses, whichever extractor
/// names them.
pub(super) fn accesses(family: Option<&str>, line: &str) -> Vec<Access> {
    let exemptions = literal_exemptions(line);
    let literals: BTreeSet<(usize, String)> = watchlist()
        .iter()
        .flat_map(|matcher| matcher.names(line))
        .filter(|(_, name)| !exemptions.contains(name))
        .collect();
    let gated = family.filter(|_| PACKAGED_ENV_PROFILE.access_gate);
    let gates: BTreeSet<usize> = gated
        .map(|family| gate_starts(family, line))
        .unwrap_or_default();

    let mut owning: BTreeSet<usize> = BTreeSet::new();
    let mut found: Vec<Access> = Vec::new();
    for (start, name) in literals {
        if let Some(gate) = gates.range(..=start).next_back() {
            owning.insert(*gate);
        }
        if let Some(column) = column_of(line, start) {
            found.push(Access { column, name });
        }
    }
    if let Some(family) = gated {
        let idiom = finding_name(family);
        found.extend(gates.difference(&owning).filter_map(|start| {
            column_of(line, *start).map(|column| Access {
                column,
                name: idiom.clone(),
            })
        }));
    }
    found.sort_by(|left, right| (left.column, &left.name).cmp(&(right.column, &right.name)));
    found
}

/// Where every admitted access-gate match for one family begins in one line.
fn gate_starts(family: &str, line: &str) -> BTreeSet<usize> {
    access_gate()
        .iter()
        .find(|(named, _)| *named == family)
        .map(|(_, patterns)| patterns.as_slice())
        .unwrap_or_default()
        .iter()
        .flat_map(|matcher| matcher.starts(line))
        .collect()
}

/// Every environment name one line references, deduplicated and ordered.
///
/// A reference is any of four things: a literal read
/// at an access site of the line's own family, a literal bound to a declaration
/// symbol, any quoted literal on a line already about the environment, and any
/// literal the watchlist recognizes. Waivers do not enter here, because a
/// waived read is still a name the ledger has to account for.
///
/// One explicit current-port assertion narrows only the broad literal fallback:
/// `env-policy: not-an-env NAME` removes that exact name when it was inferred
/// solely from the watchlist or packaged declaration list. A name grounded by
/// access, declaration, or environment context remains a reference even beside
/// the assertion, so it cannot turn the access waiver into a ledger waiver.
pub(super) fn references(family: Option<&str>, line: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if let Some(family) = family {
        let access = access_names()
            .iter()
            .find(|(named, _)| *named == family)
            .map(|(_, patterns)| patterns.as_slice())
            .unwrap_or_default();
        extend(&mut names, access, line);
        if family == JAVASCRIPT {
            names.extend(destructured_names(line));
        }
    }
    extend(&mut names, declaration_names(), line);
    if context().is_match(line) || line.contains(PACKAGED_ENV_PROFILE.allow_marker) {
        extend(&mut names, context_names(), line);
    }
    let exemptions = literal_exemptions(line);
    let mut inferred = BTreeSet::new();
    extend(&mut inferred, watchlist(), line);
    extend(&mut inferred, declared_names(), line);
    names.extend(inferred.difference(&exemptions).cloned());
    names.retain(|name| is_ledger_name(name));
    names
}

/// Every environment-name reference in one file, with its one-based line.
///
/// Ordinary references follow one-line rules. Rust adds one
/// closed construction the line scanner cannot see: a literal-only `concat!`
/// whose fragments reconstruct a watchlisted or declared ledger name. The
/// macro may wrap across lines, and its reference belongs to the line where the
/// macro opens. An exact non-environment assertion anywhere in the statement
/// containing that macro exempts the reconstructed literal just as it exempts
/// a plain literal.
pub(super) fn references_in(
    extension: Option<&str>,
    family: Option<&str>,
    text: &str,
) -> BTreeSet<(u32, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = BTreeSet::new();
    for (index, line) in lines.iter().enumerate() {
        let Ok(number) = u32::try_from(index + 1) else {
            continue;
        };
        found.extend(
            references(family, line)
                .into_iter()
                .map(|name| (number, name)),
        );
    }
    if extension != Some("rs") {
        return found;
    }

    for concatenation in rust_concatenations(text) {
        let start = concatenation.start;
        let scope_start = concatenation.scope_start;
        let scope_end = concatenation.scope_end;
        let name = concatenation.name;
        if !is_inferred_reference(&name) {
            continue;
        }
        let exempt = lines[scope_start as usize - 1..scope_end as usize]
            .iter()
            .flat_map(|line| literal_exemptions(line))
            .any(|literal| literal == name);
        if !exempt {
            found.insert((start, name));
        }
    }
    found
}

/// One literal-only Rust concatenation recovered from the syntax tree.
struct RustConcatenation {
    start: u32,
    scope_start: u32,
    scope_end: u32,
    name: String,
}

/// Every literal-only `concat!` in executable Rust syntax.
fn rust_concatenations(text: &str) -> Vec<RustConcatenation> {
    let mut parser = rust_parser();
    let tree = parser
        .parse(text, None)
        .expect("an unbounded Rust parse completes");
    let mut found = Vec::new();
    let mut pending = vec![tree.root_node()];
    while let Some(node) = pending.pop() {
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
        if node.kind() != "macro_invocation" {
            continue;
        }
        let Some(macro_name) = node.child_by_field_name("macro") else {
            continue;
        };
        let Some(spelling) = text.get(macro_name.byte_range()) else {
            continue;
        };
        if spelling.rsplit("::").next() != Some("concat") {
            continue;
        }
        let mut cursor = node.walk();
        let Some(body) = node
            .named_children(&mut cursor)
            .find(|child| child.kind() == "token_tree")
        else {
            continue;
        };
        let mut parts = Vec::new();
        let mut wants_part = true;
        let mut valid = true;
        let mut cursor = body.walk();
        for token in body.children(&mut cursor) {
            match token.kind() {
                "(" | ")" | "[" | "]" | "{" | "}" => {}
                "line_comment" | "block_comment" => {}
                "," if !wants_part => wants_part = true,
                "string_literal" | "raw_string_literal" if wants_part => {
                    let mut cursor = token.walk();
                    let mut children = token.named_children(&mut cursor);
                    let Some(content) = children.next() else {
                        valid = false;
                        break;
                    };
                    if content.kind() != "string_content" || children.next().is_some() {
                        valid = false;
                        break;
                    }
                    let Some(content) = text.get(content.byte_range()) else {
                        valid = false;
                        break;
                    };
                    if content.is_empty() || !content.bytes().all(is_environment_fragment_byte) {
                        valid = false;
                        break;
                    }
                    parts.push(content);
                    wants_part = false;
                }
                _ => {
                    valid = false;
                    break;
                }
            }
        }
        if !valid || parts.len() < 2 {
            continue;
        }
        let name = parts.concat();
        let Ok(start) = u32::try_from(node.start_position().row + 1) else {
            continue;
        };
        let scope = rust_syntax_scope(node);
        let Ok(scope_start) = u32::try_from(scope.start_position().row + 1) else {
            continue;
        };
        let Ok(scope_end) = u32::try_from(scope.end_position().row + 1) else {
            continue;
        };
        found.push(RustConcatenation {
            start,
            scope_start,
            scope_end,
            name,
        });
    }
    found
}

/// The executable syntax unit containing one macro invocation.
fn rust_syntax_scope(mut node: tree_sitter::Node<'_>) -> tree_sitter::Node<'_> {
    let invocation = node;
    while let Some(parent) = node.parent() {
        node = parent;
        if matches!(
            node.kind(),
            "let_declaration" | "expression_statement" | "const_item" | "static_item"
        ) {
            return node;
        }
    }
    invocation
}

/// One parser bound to the packaged Rust grammar.
fn rust_parser() -> Parser {
    let mut parser = Parser::new();
    let language = Language::new(tree_sitter_rust::LANGUAGE);
    parser
        .set_language(&language)
        .expect("packaged Rust grammar is compatible");
    parser
}

/// Whether one byte can belong to an environment-name literal fragment.
fn is_environment_fragment_byte(byte: u8) -> bool {
    byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b"_"[0]
}

/// Whether a reconstructed literal is one the ordinary ledger would infer.
fn is_inferred_reference(name: &str) -> bool {
    if !is_ledger_name(name) {
        return false;
    }
    let literal = format!("\"{name}\"");
    watchlist().iter().chain(declared_names()).any(|matcher| {
        matcher
            .names(&literal)
            .into_iter()
            .any(|(_, found)| found == name)
    })
}

/// Exact ledger-shaped names asserted to be non-environment literals on a line.
///
/// A marker without a following name exempts nothing. Repeating the marker can
/// name several literals without broadening any one assertion to the whole line.
fn literal_exemptions(line: &str) -> BTreeSet<String> {
    line.split(ENV_NAME_LITERAL_MARKER)
        .skip(1)
        .filter_map(|suffix| suffix.split_whitespace().next())
        .filter(|name| is_ledger_name(name))
        .map(str::to_owned)
        .collect()
}

/// Add every name one group of patterns reads out of one line.
fn extend(names: &mut BTreeSet<String>, matchers: &[Guarded], line: &str) {
    for matcher in matchers {
        names.extend(matcher.names(line).into_iter().map(|(_, name)| name));
    }
}

/// Every name one JavaScript destructuring binding names.
fn destructured_names(line: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for captures in destructured().captures_iter(line) {
        let Some(body) = captures.name("body") else {
            continue;
        };
        for part in body.as_str().split(',') {
            let part = part.trim();
            if part.is_empty() || part.starts_with("...") {
                continue;
            }
            let bound = part.split_once(':').map_or(part, |(before, _)| before);
            let bound = bound.split_once('=').map_or(bound, |(before, _)| before);
            names.insert(bound.trim().to_owned());
        }
    }
    names
}

/// Whether one captured literal reads as a name the ledger accounts for.
///
/// Spelled out rather than compiled because the rule is a shape, not a search:
/// upper case, digits, and underscores, opening on a letter or an underscore
/// and never closing on one.
fn is_ledger_name(name: &str) -> bool {
    let opens = name
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_uppercase() || first == '_');
    opens
        && !name.ends_with('_')
        && name.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

/// The stable finding name one access family reports under.
///
/// The packaged table names every family it declares an idiom for; any other
/// family reports under the default spelled here rather than one invented at
/// the call site.
fn finding_name(family: &str) -> String {
    PACKAGED_ENV_PROFILE
        .finding_names
        .iter()
        .find(|(named, _)| *named == family)
        .map_or_else(
            || format!("{family} env access"),
            |(_, name)| (*name).to_owned(),
        )
}

/// The one-based line numbers of every line one predicate holds for.
fn numbered(lines: &[&str], predicate: impl Fn(&str) -> bool) -> BTreeSet<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| predicate(line))
        .map(|(index, _)| index + 1)
        .collect()
}

/// Whether one line is a comment whose only business is the waiver.
fn standalone_waiver(line: &str, marker: &str) -> bool {
    let stripped = line.trim();
    stripped.contains(marker) && (stripped.starts_with('#') || stripped.starts_with("//"))
}

/// Every one-based inclusive line span one statement of a file occupies.
///
/// A statement runs until its delimiters balance and it stops asking for a
/// continuation line, which is what lets a waiver written above a multi-line
/// call cover the argument the access is spelled in.
fn statement_spans(lines: &[&str]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut opened: Option<usize> = None;
    let mut depth: i32 = 0;
    for (index, raw) in lines.iter().enumerate() {
        let number = index + 1;
        if opened.is_none() && raw.trim().is_empty() {
            continue;
        }
        let start = *opened.get_or_insert(number);
        let structural = structural_line(raw);
        depth = (depth + delimiter_delta(&structural)).max(0);
        if depth == 0 && !structural.trim_end().ends_with('\\') {
            spans.push((start, number));
            opened = None;
        }
    }
    if let Some(start) = opened {
        spans.push((start, lines.len()));
    }
    spans
}

/// One line with its string contents and trailing comment removed.
///
/// Only the structure decides where a statement ends, so a bracket inside a
/// string literal or a comment cannot open a span that never closes.
fn structural_line(line: &str) -> String {
    let mut structural = String::new();
    let mut characters = line.chars().peekable();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    while let Some(character) = characters.next() {
        if let Some(open) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == open {
                quote = None;
            }
            continue;
        }
        match character {
            '#' => break,
            '/' if characters.peek() == Some(&'/') => break,
            '\'' | '"' => quote = Some(character),
            _ => structural.push(character),
        }
    }
    structural
}

/// How far one structural line opens or closes the delimiters it contains.
fn delimiter_delta(line: &str) -> i32 {
    line.chars().fold(0, |depth, character| match character {
        '(' | '[' | '{' => depth + 1,
        ')' | ']' | '}' => depth - 1,
        _ => depth,
    })
}

/// Every regex spelling one template stands for, in quote-character order.
///
/// A template naming a quoted literal compiles twice, once per quote character,
/// because the profile holds the opening and closing quote equal with a
/// back-reference this engine has none of.
fn spellings(template: &str) -> Vec<String> {
    let named = template.replace(NAME_HOLE, &format!("(?P<name>{NAME_LITERAL})"));
    if !named.contains(QUOTED_HOLE) {
        return vec![named];
    }
    ['"', '\'']
        .into_iter()
        .map(|quote| {
            named.replace(
                QUOTED_HOLE,
                &format!("{quote}(?P<name>{NAME_LITERAL}){quote}"),
            )
        })
        .collect()
}

/// Compile every spelling of every template, in template order.
fn compiled(templates: &[&str]) -> Vec<Guarded> {
    templates
        .iter()
        .flat_map(|template| spellings(template))
        .map(|spelling| Guarded::compile(&spelling))
        .collect()
}

/// Compile one watchlist pattern into its quoted form.
///
/// Watchlist and declared names admit a literal opened and closed by different
/// quote characters. This is deliberately not the back-referenced form used at
/// access sites.
fn quoted_watch(patterns: impl IntoIterator<Item = String>) -> Vec<Guarded> {
    patterns
        .into_iter()
        .map(|pattern| Guarded::compile(&format!(r#"["'](?P<name>{pattern})["']"#)))
        .collect()
}

/// The compiled watchlist of packaged environment-name patterns.
fn watchlist() -> &'static [Guarded] {
    static WATCHLIST: OnceLock<Vec<Guarded>> = OnceLock::new();
    WATCHLIST.get_or_init(|| {
        quoted_watch(
            PACKAGED_ENV_PROFILE
                .name_patterns
                .iter()
                .map(|pattern| (*pattern).to_owned()),
        )
    })
}

/// The compiled exact declarations the packaged ledger accounts for.
fn declared_names() -> &'static [Guarded] {
    static DECLARED: OnceLock<Vec<Guarded>> = OnceLock::new();
    DECLARED.get_or_init(|| {
        quoted_watch(
            PACKAGED_ENV_PROFILE
                .declared_names
                .iter()
                .map(|name| regex::escape(name)),
        )
    })
}

/// The compiled access-site gate, family by family, in packaged order.
fn access_gate() -> &'static [(&'static str, Vec<Guarded>)] {
    static GATE: OnceLock<Vec<(&'static str, Vec<Guarded>)>> = OnceLock::new();
    GATE.get_or_init(|| {
        PACKAGED_ENV_PROFILE
            .access_patterns
            .iter()
            .map(|(family, patterns)| (*family, compiled(patterns)))
            .collect()
    })
}

/// The compiled access-site name readers, family by family.
fn access_names() -> &'static [(&'static str, Vec<Guarded>)] {
    static NAMES: OnceLock<Vec<(&'static str, Vec<Guarded>)>> = OnceLock::new();
    NAMES.get_or_init(|| {
        ACCESS_NAME_TEMPLATES
            .iter()
            .map(|(family, templates)| (*family, compiled(templates)))
            .collect()
    })
}

/// The compiled declaration-site name readers.
fn declaration_names() -> &'static [Guarded] {
    static DECLARATIONS: OnceLock<Vec<Guarded>> = OnceLock::new();
    DECLARATIONS.get_or_init(|| compiled(DECLARATION_NAME_TEMPLATES))
}

/// The compiled detector for a line already about the environment.
fn context() -> &'static Regex {
    static CONTEXT: OnceLock<Regex> = OnceLock::new();
    CONTEXT.get_or_init(|| {
        Regex::new(CONTEXT_TEMPLATE).expect("compiled environment context pattern is valid")
    })
}

/// The compiled quoted-literal reader for an environment-context line.
fn context_names() -> &'static [Guarded] {
    static NAMES: OnceLock<Vec<Guarded>> = OnceLock::new();
    NAMES.get_or_init(|| compiled(&[CONTEXT_NAME_TEMPLATE]))
}

/// The compiled JavaScript destructuring reader.
fn destructured() -> &'static Regex {
    static DESTRUCTURED: OnceLock<Regex> = OnceLock::new();
    DESTRUCTURED.get_or_init(|| {
        Regex::new(DESTRUCTURED_TEMPLATE).expect("compiled destructuring pattern is valid")
    })
}
