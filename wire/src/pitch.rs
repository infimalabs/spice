//! The pitch: one plain sentence saying why a task matters.
//!
//! Every route that writes a task statement admits its pitch through
//! [`admit`] and reads prose findings through [`findings`]; no reader derives
//! one. [`derive`] is the one rule for routes that intentionally synthesize a
//! pitch from current statement text: a submission that omits one, a friction
//! report, a review follow-up, or a demo task.

use crate::rooms::one_line;

/// Most Unicode scalar values an authored pitch may hold: an authoring bound,
/// never a truncation width.
pub const PITCH_CHARS: usize = 160;

/// A mechanical rule an authored pitch broke; each is one typed refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PitchRefusal {
    /// Empty after trimming.
    Empty,
    /// Holds a line break.
    Multiline,
    /// Holds more than [`PITCH_CHARS`] scalar values.
    TooLong {
        /// Scalar values counted.
        chars: usize,
    },
}

impl PitchRefusal {
    /// Typed refusal reason.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Empty => "task::pitch-empty",
            Self::Multiline => "task::pitch-multiline",
            Self::TooLong { .. } => "task::pitch-too-long",
        }
    }

    /// What the author changes to pass.
    #[must_use]
    pub fn remedy(self) -> String {
        match self {
            Self::Empty => {
                "give --pitch one plain sentence saying why this task matters".to_owned()
            }
            Self::Multiline => "keep the pitch on one line with no line break".to_owned(),
            Self::TooLong { chars } => format!(
                "shorten the pitch from {chars} to at most {PITCH_CHARS} characters; the detail \
                 belongs in the description"
            ),
        }
    }
}

/// Admit an authored pitch against the three mechanical rules.
///
/// # Errors
///
/// The first rule broken, in the order empty, multiline, too long.
pub fn admit(pitch: &str) -> Result<(), PitchRefusal> {
    let trimmed = pitch.trim();
    if trimmed.is_empty() {
        return Err(PitchRefusal::Empty);
    }
    if trimmed.contains(['\n', '\r']) {
        return Err(PitchRefusal::Multiline);
    }
    let chars = trimmed.chars().count();
    if chars > PITCH_CHARS {
        return Err(PitchRefusal::TooLong { chars });
    }
    Ok(())
}

/// One place an authored pitch reads as more than one plain sentence.
///
/// These are heuristics over prose, so they hold the statement for review
/// naming the matched text instead of refusing the write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PitchFinding {
    /// Built-in taste name the hold records.
    pub taste: &'static str,
    /// The matched text.
    pub trigger: String,
    /// One-based byte column at which the trigger begins.
    pub column: usize,
    /// Why the match holds the statement.
    pub message: &'static str,
}

/// Built-in taste name for an identifier inside a pitch.
pub const PITCH_IDENTIFIER_TASTE: &str = "pitch-identifier";
/// Built-in taste name for a pitch that runs past one sentence.
pub const PITCH_SENTENCE_TASTE: &str = "pitch-sentence";

const IDENTIFIER_MESSAGE: &str =
    "a pitch names no code, handle, or path; say why the work matters in plain words";
const SENTENCE_MESSAGE: &str = "a pitch is one sentence; move the rest into the description";

/// Prose findings over one admitted pitch, in column order.
#[must_use]
pub fn findings(pitch: &str) -> Vec<PitchFinding> {
    let mut found = Vec::new();
    for (index, token) in tokens(pitch) {
        let bare = token.trim_matches(wrapping_punctuation);
        if identifier_like(bare) {
            let lead = token.len() - token.trim_start_matches(wrapping_punctuation).len();
            found.push(PitchFinding {
                taste: PITCH_IDENTIFIER_TASTE,
                trigger: bare.to_owned(),
                column: index + lead + 1,
                message: IDENTIFIER_MESSAGE,
            });
        }
    }
    if let Some(column) = second_sentence(pitch) {
        found.push(PitchFinding {
            taste: PITCH_SENTENCE_TASTE,
            trigger: pitch[column - 1..].trim().to_owned(),
            column,
            message: SENTENCE_MESSAGE,
        });
    }
    found.sort_by_key(|finding| finding.column);
    found
}

/// Punctuation that wraps a word in prose without being part of it.
fn wrapping_punctuation(character: char) -> bool {
    character.is_ascii_punctuation() && !matches!(character, '`' | '/' | '-' | '_')
}

fn tokens(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.split(char::is_whitespace)
        .scan(0_usize, |offset, token| {
            let start = *offset;
            *offset += token.len() + 1;
            Some((start, token))
        })
        .filter(|(_, token)| !token.is_empty())
}

/// File extensions that mark a token as a file name rather than a word.
const FILE_EXTENSIONS: [&str; 16] = [
    "rs", "md", "toml", "json", "css", "mjs", "js", "ts", "html", "yml", "yaml", "sh", "txt",
    "png", "sql", "lock",
];

/// Whether one whitespace-delimited token reads as code rather than prose: a
/// backtick span, a Rust path, a file path, a task handle or stamp, a file
/// name, or a snake_case or CamelCase symbol. `and/or`, `read/write`, dates,
/// decimals, abbreviations such as `e.g.`, and words ending in a capital run
/// such as `macOS` stay prose.
fn identifier_like(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    if token.contains('`') || token.contains("::") {
        return true;
    }
    let alphanumeric_or = |extra: fn(char) -> bool| {
        token
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || extra(character))
    };
    if token.contains('/') {
        let segments = token.split('/').collect::<Vec<_>>();
        let lettered = segments.iter().any(|segment| {
            segment
                .chars()
                .any(|character| character.is_ascii_alphabetic())
        });
        if lettered && (token.starts_with(['/', '.']) || segments.len() > 2 || token.contains('.'))
        {
            return true;
        }
    }
    if token.parse::<crate::TaskHandle>().is_ok() {
        return true;
    }
    if let Some((stem, extension)) = token.rsplit_once('.')
        && FILE_EXTENSIONS.contains(&extension)
        && !stem.is_empty()
        && stem
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return true;
    }
    if token.contains('_') && alphanumeric_or(|character| character == '_') {
        return true;
    }
    camel_case(token) && alphanumeric_or(|_| false)
}

/// Whether a token has a CamelCase hump: a lowercase letter, then an
/// uppercase one that itself leads a lowercase run, as `readGuard` and
/// `TaskStamp` do. A trailing all-caps run is prose, so `macOS`, `iOS`, and
/// `tvOS` are words, not symbols.
fn camel_case(token: &str) -> bool {
    let mut characters = token.chars().peekable();
    let mut previous_lower = false;
    while let Some(character) = characters.next() {
        if previous_lower
            && character.is_ascii_uppercase()
            && characters.peek().is_some_and(char::is_ascii_lowercase)
        {
            return true;
        }
        previous_lower = character.is_ascii_lowercase();
    }
    false
}

/// One-based byte column where a second sentence begins, when the pitch runs
/// past one: a terminator, whitespace, then text opening with an uppercase
/// letter, a digit, or a quote, so `e.g. this` and `3.5 seconds` stay prose.
fn second_sentence(pitch: &str) -> Option<usize> {
    let trimmed_end = pitch.trim_end();
    for (index, terminator) in trimmed_end.match_indices(['.', '?', '!']) {
        let end = index + terminator.len();
        if end >= trimmed_end.len() {
            break;
        }
        let rest = &trimmed_end[end..];
        let next = rest.trim_start();
        let opens_sentence = next.chars().next().is_some_and(|c| {
            c.is_uppercase() || c.is_ascii_digit() || matches!(c, '"' | '\'' | '(')
        });
        if rest.starts_with(char::is_whitespace) && opens_sentence {
            return Some(end + (rest.len() - next.len()) + 1);
        }
    }
    None
}

/// Derive a pitch once, at a write with no author to ask, from the title and
/// description: the first description sentence longer than a one-word label,
/// cut to [`PITCH_CHARS`], else the title, else the description itself.
#[must_use]
pub fn derive(title: &str, description: Option<&str>) -> String {
    let Some(description) = description.map(str::trim).filter(|text| !text.is_empty()) else {
        return one_line(title, PITCH_CHARS);
    };
    let mut start = 0;
    let mut selected = None;
    for (index, punctuation) in description.match_indices(['.', '?', '!']) {
        let end = index + punctuation.len();
        if end < description.len() && !description[end..].starts_with(char::is_whitespace) {
            continue;
        }
        let sentence = description[start..end].trim();
        if sentence.split_whitespace().nth(1).is_some() {
            selected = Some(sentence);
            break;
        }
        start = end;
    }
    let sentence = selected
        .or_else(|| {
            let trailing = description[start..].trim();
            trailing.split_whitespace().nth(1).map(|_| trailing)
        })
        .unwrap_or(if title.trim().is_empty() {
            description
        } else {
            title
        });
    one_line(sentence, PITCH_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_mechanical_rules_refuse_in_order() {
        assert_eq!(admit("  \t"), Err(PitchRefusal::Empty));
        assert_eq!(admit("why\nnot"), Err(PitchRefusal::Multiline));
        let long = "é".repeat(PITCH_CHARS + 1);
        assert_eq!(
            admit(&long),
            Err(PitchRefusal::TooLong {
                chars: PITCH_CHARS + 1
            })
        );
        assert_eq!(admit(&"é".repeat(PITCH_CHARS)), Ok(()));
    }

    #[test]
    fn prose_findings_name_identifiers_and_a_second_sentence() {
        let found =
            findings("Readers lose the why in `resolve_targets` and wire::body. Fix it now.");
        let tastes = found
            .iter()
            .map(|finding| (finding.taste, finding.trigger.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            tastes,
            [
                (PITCH_IDENTIFIER_TASTE, "`resolve_targets`"),
                (PITCH_IDENTIFIER_TASTE, "wire::body"),
                (PITCH_SENTENCE_TASTE, "Fix it now."),
            ]
        );
        assert_eq!(found[2].column, 59);
    }

    #[test]
    fn plain_prose_with_slashes_and_dates_passes() {
        assert!(
            findings("Operators read and/or write on 10/22 without losing the why.").is_empty()
        );
        assert!(findings("A sentence that ends in a quote reads fine, e.g. this one.").is_empty());
        assert!(findings("Why 3.5 seconds matters to a reader who waits.").is_empty());
        assert!(findings("One word fits macOS, iOS, and Windows alike.").is_empty());
    }

    #[test]
    fn a_hump_followed_by_lowercase_is_a_symbol() {
        let triggers = |pitch: &str| {
            findings(pitch)
                .into_iter()
                .map(|finding| finding.trigger)
                .collect::<Vec<_>>()
        };
        assert_eq!(triggers("The readGuard hides the why."), ["readGuard"]);
        assert_eq!(triggers("A TaskStamp alone reads as code."), ["TaskStamp"]);
    }

    #[test]
    fn derivation_prefers_the_first_real_sentence_and_falls_back_to_the_title() {
        assert_eq!(
            derive("Title", Some("Label. Readers lose the why. Then more.")),
            "Readers lose the why."
        );
        assert_eq!(derive("Title", Some("Label.")), "Title");
        assert_eq!(derive("Title", None), "Title");
        let long = "word ".repeat(80);
        let cut = derive("Title", Some(&long));
        assert_eq!(cut.chars().count(), PITCH_CHARS);
        assert!(cut.ends_with('…'));
    }
}
