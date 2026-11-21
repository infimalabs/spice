//! Canonical topical terms from authored prose.
//!
//! Authored handles, UUIDs and identity keys do not contribute topical
//! evidence. Ordinary words and short technical terms follow separate rules.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use crate::stop_words::ENGLISH as STOP;

/// Occurrence counts keyed by canonical term, in spelling order.
pub type Counts = BTreeMap<String, u32>;

/// Characters a run may have and still be considered for a term.
const RUN_CHARS: RangeInclusive<usize> = 3..=32;
/// Fewest characters of an ordinary term after folding.
const MIN_TERM_CHARS: usize = 4;
/// A shorter hexadecimal prefix counts as a digest only when it contains a digit.
const MIN_HEX_DIGEST: usize = 12;
const MIN_MIXED_HEX_DIGEST: usize = 7;
/// Message mechanics, never the subject of the work.
pub const PROTOCOL_NOISE: [&str; 4] = ["syn", "ack", "nak", "fin"];

fn inert(lower: &str) -> bool {
    STOP.binary_search(&lower).is_ok() || PROTOCOL_NOISE.contains(&lower)
}

pub fn word(c: char) -> bool {
    c.is_alphanumeric()
}

fn letter(c: char) -> bool {
    c.is_alphabetic() && !c.is_numeric()
}

pub fn space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// A digit of the vowel-free base-52 alphabet moment keys are written in.
fn key_digit(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() && !b"AEIOUaeiou".contains(&byte)
}

/// Whether a whole run is an identity: a moment key, or a hexadecimal digest.
fn identity(run: &str) -> bool {
    let bytes = run.as_bytes();
    ((8..=9).contains(&bytes.len()) && bytes.iter().copied().all(key_digit))
        || (bytes.iter().all(u8::is_ascii_hexdigit)
            && (bytes.len() >= MIN_HEX_DIGEST
                || (bytes.len() >= MIN_MIXED_HEX_DIGEST && bytes.iter().any(u8::is_ascii_digit))))
}

/// Whether nothing, or no word character, follows `end`.
fn closed(text: &str, end: usize) -> bool {
    !text[end..].chars().next().is_some_and(word)
}

/// The end of a `HANDLE-key` reference starting at `at`.
fn handle_end(text: &str, at: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if !bytes[at].is_ascii_uppercase() {
        return None;
    }
    let stem = bytes[at..]
        .iter()
        .take_while(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        .count();
    let key = at + stem + 1;
    if !matches!(bytes.get(key - 1), Some(b'-' | b'_')) {
        return None;
    }
    let digits = bytes[key..]
        .iter()
        .take(9)
        .take_while(|&&byte| key_digit(byte))
        .count();
    [9, 8]
        .into_iter()
        .filter(|&length| length <= digits)
        .map(|length| key + length)
        .find(|&end| closed(text, end))
}

/// The end of a hyphenated UUID starting at `at`.
fn uuid_end(text: &str, at: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut end = at;
    for (group, digits) in [8, 4, 4, 4, 12].into_iter().enumerate() {
        if group > 0 {
            if bytes.get(end) != Some(&b'-') {
                return None;
            }
            end += 1;
        }
        if !bytes
            .get(end..end + digits)?
            .iter()
            .all(u8::is_ascii_hexdigit)
        {
            return None;
        }
        end += digits;
    }
    closed(text, end).then_some(end)
}

/// Byte ranges of authored references, in reading order and disjoint.
fn references(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let end = (bytes[at].is_ascii_alphanumeric()
            && !text[..at].chars().next_back().is_some_and(word))
        .then(|| handle_end(text, at).or_else(|| uuid_end(text, at)))
        .flatten();
        if let Some(end) = end {
            spans.push((at, end));
            at = end;
        } else {
            at += 1;
        }
    }
    spans
}

/// Whether the run at `start..end` is a command-line option's name.
fn option(text: &str, start: usize, end: usize) -> bool {
    let head = &text[..start];
    let Some(before) = head.strip_suffix("--").or_else(|| head.strip_suffix('-')) else {
        return false;
    };
    !text[end..].starts_with(['-', '_', '/'])
        && before
            .chars()
            .next_back()
            .is_none_or(|c| space(c) || "`([{\"'".contains(c))
}

/// Whether a three-character run is jargon by shape rather than a short word.
fn short(run: &str) -> bool {
    let mut chars = run.chars();
    let (Some(first), Some(second), Some(third), None) =
        (chars.next(), chars.next(), chars.next(), chars.next())
    else {
        return false;
    };
    let all = [first, second, third];
    letter(first)
        && (all.iter().any(|c| c.is_numeric())
            || all.iter().all(|c| c.is_uppercase())
            || (all.iter().any(|c| c.is_uppercase())
                && all.iter().any(|c| c.is_lowercase())
                && !(first.is_uppercase() && second.is_lowercase() && third.is_lowercase()))
            || all
                .iter()
                .all(|c| c.is_ascii_lowercase() && !"aeiouy".contains(*c)))
}

/// The term one authored run of word characters stands for, if any.
fn canonical(run: &str, short: impl Fn(&str) -> bool) -> Option<String> {
    let length = run.chars().count();
    if !RUN_CHARS.contains(&length) || identity(run) {
        return None;
    }
    let mut term = run.to_lowercase();
    if inert(&term) {
        return None;
    }
    let plain = |term: &str| term.chars().all(|c| word(c) && !c.is_uppercase());
    if length == 3 {
        return (short(run) && plain(&term)).then_some(term);
    }
    if run.chars().any(char::is_numeric) {
        return (run.chars().next().is_some_and(letter) && plain(&term)).then_some(term);
    }
    let chars = term.chars().count();
    if term.ends_with("ies") && chars >= MIN_TERM_CHARS + 3 {
        term.truncate(term.len() - 3);
        term.push('y');
    } else if term.ends_with('s') && chars >= 4 {
        if chars == 4 {
            // A plural of a three-character word is admitted only as that
            // word's own jargon shape, judged on the authored spelling.
            term.pop();
            let singular = &run[..run.char_indices().next_back()?.0];
            return (short(singular) && !inert(&term)).then_some(term);
        }
        if !term[..term.len() - 1].ends_with('s') {
            term.pop();
        }
    }
    if term.ends_with("ie") && term.chars().count() >= MIN_TERM_CHARS + 2 {
        term.truncate(term.len() - 2);
        term.push('y');
    }
    (term.chars().count() >= MIN_TERM_CHARS
        && term.chars().all(|c| letter(c) && !c.is_uppercase())
        && STOP.binary_search(&term.as_str()).is_err())
    .then_some(term)
}

/// Canonical terms of `text` in reading order, repeats included.
#[must_use]
pub fn terms(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    scan(text, |_, _, term| terms.extend(term));
    terms
}

/// Whether a lowercase topic component spells a term already admitted from prose.
/// Stored names have lost the capitalization that admitted short terms such as API.
#[must_use]
pub fn names_term(name: &str, term: &str) -> bool {
    name.bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && canonical(name, |singular| singular == term).as_deref() == Some(term)
}

/// Lowercased authored words of `text` that read as `term`, in reading order.
/// A term is a matching form, such as `canva` for `canvas`; these spell it.
#[must_use]
pub fn spellings(text: &str, term: &str) -> Vec<String> {
    let mut words = Vec::new();
    scan(text, |start, end, read| {
        if read.as_deref() == Some(term) {
            words.push(text[start..end].to_lowercase());
        }
    });
    words
}

/// Byte ranges of the maximal runs of word characters.
pub fn runs(text: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut chars = text.char_indices().peekable();
    std::iter::from_fn(move || {
        let (start, first) = chars.find(|&(_, c)| word(c))?;
        let mut end = start + first.len_utf8();
        while let Some((at, next)) = chars.next_if(|&(_, c)| word(c)) {
            end = at + next.len_utf8();
        }
        Some((start, end))
    })
}

fn scan(text: &str, mut each: impl FnMut(usize, usize, Option<String>)) {
    let references = references(text);
    let mut reference = 0;
    for (start, end) in runs(text) {
        while references
            .get(reference)
            .is_some_and(|span| span.1 <= start)
        {
            reference += 1;
        }
        let referenced = references.get(reference).is_some_and(|span| span.0 < end);
        let term = (!referenced && !option(text, start, end))
            .then(|| canonical(&text[start..end], short))
            .flatten();
        each(start, end, term);
    }
}

/// Term counts over separately authored texts; no term spans two of them.
#[must_use]
pub fn counts<T: AsRef<str>>(texts: impl IntoIterator<Item = T>) -> Counts {
    let mut counts = Counts::new();
    for text in texts {
        for term in terms(text.as_ref()) {
            *counts.entry(term).or_default() += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::terms;

    fn read(text: &str) -> Vec<String> {
        terms(text)
    }

    #[test]
    fn ordinary_words_need_four_letters_and_fold_only_simple_plurals() {
        assert_eq!(
            read("The queries of two policies map onto classes, a bus and movies"),
            ["query", "policy", "classe", "movie"]
        );
    }

    #[test]
    fn three_character_runs_are_admitted_only_as_jargon() {
        assert_eq!(
            read("CLI and Cli met a cat via tmp, x86 and TLS"),
            ["cli", "tmp", "x86", "tls"]
        );
        assert_eq!(read("two PRs, three cats, many tmps"), ["prs", "tmp"]);
    }

    #[test]
    fn numbers_survive_only_inside_letter_led_jargon() {
        assert_eq!(
            read("sqlite3 beats 2fast and 1234 on WebView2"),
            ["sqlite3", "beat", "webview2"]
        );
    }

    #[test]
    fn references_and_identities_are_not_topical() {
        assert_eq!(
            read("TOPICFL-dYyrX0dw2 follows dYyrX0dw2 at deadbeef1234 in 0123abc"),
            ["follow"]
        );
        assert_eq!(
            read("lane 390eb0d5-26e4-423a-af32-8ec72e9b2bc5 seats; TOPICFL-short stays"),
            ["lane", "seat", "topicfl", "short", "stay"]
        );
    }

    #[test]
    fn option_names_are_dropped_but_compound_words_are_kept() {
        assert_eq!(
            read("run --locked (-verbose) with mid-flight --dry-run paths -cache/"),
            ["flight", "path", "cache"]
        );
    }

    #[test]
    fn protocol_markers_and_stop_words_are_inert() {
        assert_eq!(read("SYN then ACK became FINs before synapse"), ["synapse"]);
    }
}
