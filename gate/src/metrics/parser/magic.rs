use std::collections::BTreeSet;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use super::{ParseError, canonical_path, python_splitlines};

const SUPPORTED_SUFFIXES: &[&str] = &[
    ".c", ".cc", ".cpp", ".go", ".h", ".hpp", ".java", ".kt", ".m", ".mm", ".rs", ".scala",
    ".swift", ".ts",
];
/// Source classes whose comparisons need AST parent semantics this parser lacks.
const AST_PARENT_SUFFIXES: &[&str] = &[".py", ".cs", ".js"];

/// One new comparison-adjacent literal absent from the baseline file.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MagicFinding {
    path: String,
    line: u64,
    literal: String,
}

impl MagicFinding {
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub const fn line(&self) -> u64 {
        self.line
    }

    #[must_use]
    pub fn literal(&self) -> &str {
        &self.literal
    }
}

/// Detect C-grammar magic-number regressions.
///
/// Python, C#, and JavaScript need AST/tree-sitter parent classifications.
/// They are refused by field until an exact native parser is compiled;
/// substituting this comparison scanner would silently weaken them.
pub fn scan_magic_regressions(
    path: &str,
    current: &str,
    baseline: Option<&str>,
    examine_threshold: u64,
) -> Result<Vec<MagicFinding>, ParseError> {
    let path = canonical_path(path)?;
    if examine_threshold == 0 {
        return Err(ParseError::malformed(
            "magic.examine_threshold",
            "threshold must be positive",
        ));
    }
    admit_suffix(&path)?;
    let baseline_literals = baseline
        .map(|text| scan_literals(&path, text, examine_threshold))
        .transpose()?
        .unwrap_or_default()
        .into_iter()
        .map(|finding| finding.literal)
        .collect::<BTreeSet<_>>();
    let current = scan_literals(&path, current, examine_threshold)?;
    Ok(current
        .into_iter()
        .filter(|finding| !baseline_literals.contains(&finding.literal))
        .collect())
}

/// Whether this build can read the magic-number class a path belongs to.
///
/// The scanner asks this on the way in, and the acquiring arm asks it before it
/// asks anything else about a path, so both settle a class the same way. It is
/// separate from scanning because it needs neither side of the comparison: which
/// classes are readable is a property of the compiled parser, answerable for a
/// path holding no bytes this build can reach.
///
/// # Errors
///
/// Returns the unsupported refusal for a class that needs AST parent semantics,
/// and the inapplicability for a path in no declared magic class at all. A path
/// this parser reads exactly returns nothing.
pub fn admit_magic_source(path: &str) -> Result<(), ParseError> {
    admit_suffix(&canonical_path(path)?)
}

fn admit_suffix(path: &str) -> Result<(), ParseError> {
    let suffix = Path::new(path)
        .extension()
        .map(|value| format!(".{}", value.to_string_lossy().to_ascii_lowercase()))
        .unwrap_or_default();
    if SUPPORTED_SUFFIXES.contains(&suffix.as_str()) {
        return Ok(());
    }
    if AST_PARENT_SUFFIXES.contains(&suffix.as_str()) {
        return Err(ParseError::unsupported(
            "policy.languages.magic",
            format!("source class {suffix:?} requires AST/tree-sitter parent semantics"),
            "compile an exact native parser for this source class or remove it from magic scope",
        ));
    }
    Err(ParseError::NotApplicable {
        reason: format!("path {path:?} has no declared magic-number source class"),
    })
}

fn scan_literals(path: &str, text: &str, threshold: u64) -> Result<Vec<MagicFinding>, ParseError> {
    let grammar = Grammar::of(path);
    let mut findings = Vec::new();
    let mut open = None;
    for (offset, raw_line) in python_splitlines(text).into_iter().enumerate() {
        let line = executable_text(raw_line, &mut open, grammar);
        findings.extend(line_findings(
            path,
            u64::try_from(offset + 1).expect("line number fits in u64"),
            &line,
            threshold,
        )?);
    }
    findings.sort();
    Ok(findings)
}

fn line_findings(
    path: &str,
    line_number: u64,
    line: &str,
    threshold: u64,
) -> Result<Vec<MagicFinding>, ParseError> {
    let mut candidates = Vec::new();
    for number in number_regex().find_iter(line) {
        if number_end_admitted(line, number.end())
            && let Some(operator) = comparison_before(line, number.start())
        {
            candidates.push((operator, number.end(), number.as_str()));
        }
        if number_start_admitted(line, number.start())
            && let Some(operator_end) = comparison_after(line, number.end())
        {
            candidates.push((number.start(), operator_end, number.as_str()));
        }
    }
    candidates.sort_by_key(|candidate| (candidate.0, candidate.1));
    let mut findings = Vec::new();
    let mut cursor = 0;
    for (start, end, candidate) in candidates {
        if start < cursor {
            continue;
        }
        cursor = end;
        let literal = candidate.replace('_', "");
        let value = literal.parse::<f64>().map_err(|_| {
            ParseError::malformed(
                format!("{path}:{line_number}"),
                format!("numeric token {literal:?} is not finite numeric text"),
            )
        })?;
        if value.is_finite() && value.abs() >= threshold as f64 {
            findings.push(MagicFinding {
                path: path.to_owned(),
                line: line_number,
                literal,
            });
        }
    }
    Ok(findings)
}

fn number_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"-?\d[\d_]*(?:\.\d+)?").expect("compiled numeric-token regex is valid")
    })
}

fn number_start_admitted(line: &str, start: usize) -> bool {
    let before = line[..start].chars().next_back();
    !before.is_some_and(|value| value == '.' || value == '_' || value.is_alphanumeric())
}

fn number_end_admitted(line: &str, end: usize) -> bool {
    let after = line[end..].chars().next();
    !after.is_some_and(|value| value == '.' || value == '_' || value.is_alphanumeric())
}

fn comparison_before(line: &str, number_start: usize) -> Option<usize> {
    let prefix = line[..number_start].trim_end();
    for operator in ["===", "!==", "<=", ">=", "==", "!=", "<", ">"] {
        let Some(before_operator) = prefix.strip_suffix(operator) else {
            continue;
        };
        let start = before_operator.len();
        let before = before_operator.chars().next_back();
        if before.is_none_or(|value| !matches!(value, '=' | '<' | '>' | '!')) {
            return Some(start);
        }
    }
    None
}

fn comparison_after(line: &str, number_end: usize) -> Option<usize> {
    let suffix = &line[number_end..];
    let leading = suffix.len() - suffix.trim_start().len();
    let suffix = &suffix[leading..];
    for operator in ["===", "!==", "<=", ">=", "==", "!=", "<", ">"] {
        if !suffix.starts_with(operator) {
            continue;
        }
        let after = suffix[operator.len()..].chars().next();
        let compound = matches!(operator, "<" | ">")
            && after.is_some_and(|value| matches!(value, '<' | '>' | '='));
        if !compound {
            return Some(number_end + leading + operator.len());
        }
    }
    None
}

/// The string-literal grammar of one magic source class.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Grammar {
    /// Raw, byte, and C string prefixes; `'` opens a char literal or is a
    /// lifetime.
    Rust,
    /// `'` opens a string exactly as `"` does.
    TypeScript,
    /// `'` opens a char literal.
    CFamily,
}

impl Grammar {
    fn of(path: &str) -> Self {
        let extension = Path::new(path)
            .extension()
            .map(|value| value.to_string_lossy().to_ascii_lowercase());
        match extension.as_deref() {
            Some("rs") => Self::Rust,
            Some("ts") => Self::TypeScript,
            _ => Self::CFamily,
        }
    }
}

/// A literal whose contents may continue past the end of a line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Literal {
    /// A backtick literal.
    Template,
    /// A quoted string with backslash escapes, closed by this quote.
    Quoted(u8),
    /// A Rust raw string, closed by `"` and this many `#`.
    Raw(usize),
}

impl Literal {
    const fn delimiter(self) -> char {
        match self {
            Self::Template => '`',
            Self::Quoted(quote) => quote as char,
            Self::Raw(_) => '"',
        }
    }

    /// Where this literal closes on `line` from `from`, if it does.
    fn end(self, line: &str, from: usize) -> Option<usize> {
        match self {
            Self::Template => line[from..].find('`').map(|end| from + end + 1),
            Self::Quoted(quote) => quoted_end(line.as_bytes(), from, quote),
            Self::Raw(hashes) => raw_end(line.as_bytes(), from, hashes),
        }
    }
}

/// The executable text of one line. Literal contents are blanked to their
/// delimiters so the code on either side stays apart, comments outside
/// literals are dropped, and a literal left open at the line's end carries
/// into the next through `open`.
///
/// Every position the text is cut at holds an ASCII delimiter, so no cut
/// falls inside a multibyte character.
fn executable_text(line: &str, open: &mut Option<Literal>, grammar: Grammar) -> String {
    let bytes = line.as_bytes();
    let mut kept = String::with_capacity(line.len());
    let mut index = 0;
    loop {
        if let Some(literal) = *open {
            let Some(end) = literal.end(line, index) else {
                return kept;
            };
            kept.push(literal.delimiter());
            index = end;
            *open = None;
        }
        let mut cursor = index;
        let (start, opener, literal) = loop {
            let Some(&byte) = bytes.get(cursor) else {
                kept.push_str(&line[index..]);
                return kept;
            };
            let next = bytes.get(cursor + 1).copied();
            match byte {
                b'/' if next == Some(b'/') => {
                    kept.push_str(&line[index..cursor]);
                    return kept;
                }
                b'/' if next == Some(b'*') => match line[cursor + 2..].find("*/") {
                    Some(end) => {
                        kept.push_str(&line[index..cursor]);
                        cursor += end + 4;
                        index = cursor;
                    }
                    // A block comment left open stays code, as it always has.
                    None => cursor += 2,
                },
                b'`' => break (cursor, 1, Literal::Template),
                b'"' => break (cursor, 1, Literal::Quoted(b'"')),
                b'\'' if grammar == Grammar::TypeScript => {
                    break (cursor, 1, Literal::Quoted(b'\''));
                }
                b'\'' => cursor += char_literal_len(&line[cursor..]).unwrap_or(1),
                b'b' | b'c' | b'r'
                    if grammar == Grammar::Rust && identifier_boundary(bytes, cursor) =>
                {
                    if let Some((length, literal)) = rust_prefixed(&bytes[cursor..]) {
                        break (cursor, length, literal);
                    }
                    cursor += 1;
                }
                _ => cursor += 1,
            }
        };
        kept.push_str(&line[index..start]);
        kept.push(literal.delimiter());
        index = start + opener;
        *open = Some(literal);
    }
}

/// Where a quoted string closes, if it closes on this line.
fn quoted_end(bytes: &[u8], from: usize, quote: u8) -> Option<usize> {
    let mut cursor = from;
    while let Some(&byte) = bytes.get(cursor) {
        if byte == b'\\' {
            cursor += 2;
        } else if byte == quote {
            return Some(cursor + 1);
        } else {
            cursor += 1;
        }
    }
    None
}

/// Where a raw string closes, if it closes on this line.
fn raw_end(bytes: &[u8], from: usize, hashes: usize) -> Option<usize> {
    let mut cursor = from;
    while let Some(offset) = bytes[cursor..].iter().position(|byte| *byte == b'"') {
        let quote = cursor + offset;
        let end = quote + 1 + hashes;
        if bytes
            .get(quote + 1..end)
            .is_some_and(|tail| tail.iter().all(|byte| *byte == b'#'))
        {
            return Some(end);
        }
        cursor = quote + 1;
    }
    None
}

/// The length of the char literal `rest` opens with, or `None` when its `'`
/// is something else, such as a Rust lifetime or label.
fn char_literal_len(rest: &str) -> Option<usize> {
    let mut characters = rest[1..].char_indices();
    let (_, first) = characters.next()?;
    let close = match first {
        '\'' => return None,
        '\\' => {
            characters.next()?;
            characters.find(|(_, character)| *character == '\'')?.0
        }
        _ => match characters.next()? {
            (offset, '\'') => offset,
            _ => return None,
        },
    };
    Some(close + 2)
}

/// Whether a Rust string prefix may start at `at`, rather than end an
/// identifier.
fn identifier_boundary(bytes: &[u8], at: usize) -> bool {
    at == 0 || {
        let before = bytes[at - 1];
        !(before.is_ascii_alphanumeric() || before == b'_' || !before.is_ascii())
    }
}

/// The prefix and opening quote of a Rust byte, C, or raw string at the
/// start of `rest`, and the literal it opens.
fn rust_prefixed(rest: &[u8]) -> Option<(usize, Literal)> {
    let prefix = match rest {
        [b'b' | b'c', b'"', ..] => return Some((2, Literal::Quoted(b'"'))),
        [b'b' | b'c', b'r', ..] => 2,
        [b'r', ..] => 1,
        _ => return None,
    };
    let hashes = rest[prefix..]
        .iter()
        .take_while(|byte| **byte == b'#')
        .count();
    (rest.get(prefix + hashes) == Some(&b'"'))
        .then_some((prefix + hashes + 1, Literal::Raw(hashes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_ascii_text_before_a_number_preserves_comparison_findings() {
        let findings = scan_magic_regressions(
            "fixture.rs",
            "let label = \"0·1·0\";\nif count == 100 { return; }\n",
            None,
            10,
        )
        .expect("non-ASCII text cannot split comparison scanning");

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line(), 2);
        assert_eq!(findings[0].literal(), "100");
    }

    fn found(path: &str, text: &str) -> Vec<(u64, String)> {
        scan_magic_regressions(path, text, None, 10)
            .expect("fixture scans")
            .into_iter()
            .map(|finding| (finding.line(), finding.literal().to_owned()))
            .collect()
    }

    fn at(line: u64, literal: &str) -> (u64, String) {
        (line, literal.to_owned())
    }

    #[test]
    fn markup_inside_a_string_is_not_a_comparison() {
        assert_eq!(
            found(
                "guide.rs",
                "page.push_str(\"<p>26 withdrawals</p><b>40</b>\");\n"
            ),
            []
        );
    }

    #[test]
    fn code_beside_a_string_on_one_line_is_still_scanned() {
        assert_eq!(
            found(
                "fixture.rs",
                "if n < 12 && s == \"x > 34\" || m > 56 { return; }\n"
            ),
            [at(1, "12"), at(1, "56")]
        );
    }

    #[test]
    fn escaped_quotes_and_backslashes_stay_inside_their_string() {
        assert_eq!(
            found(
                "fixture.c",
                "s = \"say \\\" < 21 \\\\\"; if (n > 77) {}\nt = \"\\\\\"; if (n == 31) {}\n"
            ),
            [at(1, "77"), at(2, "31")]
        );
    }

    #[test]
    fn a_hashed_raw_string_spans_lines_until_its_matching_close() {
        assert_eq!(
            found(
                "fixture.rs",
                "let s = br##\"a \"# < 40\nb > 60 \"#\nc == 70 \"##; if n >= 88 {}\nif m > 99 {}\n"
            ),
            [at(3, "88"), at(4, "99")]
        );
    }

    #[test]
    fn a_quote_char_literal_does_not_open_a_string() {
        assert_eq!(
            found(
                "fixture.rs",
                "if c == '\"' && n > 55 {}\nif c == '\\'' && n > 66 {}\nif c == '\\\\' && n > 44 {}\n"
            ),
            [at(1, "55"), at(2, "66"), at(3, "44")]
        );
    }

    #[test]
    fn a_lifetime_is_code_and_does_not_open_a_char_literal() {
        assert_eq!(
            found(
                "fixture.rs",
                "fn f<'a>(x: &'a str) -> bool { x.len() > 44 && \"y\" != \"z > 50\" }\n"
            ),
            [at(1, "44")]
        );
    }

    #[test]
    fn comment_markers_inside_a_string_do_not_start_a_comment() {
        assert_eq!(
            found(
                "fixture.go",
                "u := \"http://host/*x\"; if n > 66 { return } // n > 77\n"
            ),
            [at(1, "66")]
        );
    }

    #[test]
    fn a_backtick_literal_spans_lines_and_hides_its_contents() {
        assert_eq!(
            found(
                "fixture.go",
                "q := \x60a > 20\n\"b < 30\x60; if n > 40 {}\n"
            ),
            [at(2, "40")]
        );
    }

    #[test]
    fn a_typescript_single_quoted_string_hides_a_double_quote() {
        assert_eq!(
            found(
                "fixture.ts",
                "const s = 'say \"hi > 20'; if (n > 30) {}\nif (m > 50) {}\n"
            ),
            [at(1, "30"), at(2, "50")]
        );
    }
}
