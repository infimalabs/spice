//! Bounded text slices for browser system synthesis.

/// Conservative browser utterance ceiling in JavaScript UTF-16 code units.
///
/// Android's platform synthesizer admits at most 4,000 characters. Web Speech
/// does not expose that limit, so browser requests leave room below it while
/// the desktop shell keeps using its native unbounded path.
pub(super) const MAX_BROWSER_UTTERANCE_UTF16: usize = 3_000;

/// Borrowed chunks for one browser speech request.
pub(super) fn browser(text: &str) -> Chunks<'_> {
    Chunks::new(text, MAX_BROWSER_UTTERANCE_UTF16)
}

/// One exact partition of text, preserving every byte in source order.
pub(super) struct Chunks<'a> {
    remaining: Option<&'a str>,
    limit: usize,
}

impl<'a> Chunks<'a> {
    fn new(text: &'a str, limit: usize) -> Self {
        debug_assert!(limit >= 2, "every Unicode scalar must fit one chunk");
        Self {
            remaining: Some(text),
            limit,
        }
    }
}

impl<'a> Iterator for Chunks<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        let remaining = self.remaining.take()?;
        if remaining.is_empty() {
            return Some(remaining);
        }
        let end = bounded_end(remaining, self.limit);
        let (chunk, rest) = remaining.split_at(end);
        if !rest.is_empty() {
            self.remaining = Some(rest);
        }
        Some(chunk)
    }
}

/// The strongest authored boundary at or before the UTF-16 ceiling.
fn bounded_end(text: &str, limit: usize) -> usize {
    let mut units = 0;
    let mut hard = 0;
    let mut whitespace = None;
    let mut sentence = None;
    let mut paragraph = None;
    let mut sentence_pending = false;
    let mut line_breaks = 0;

    for (offset, character) in text.char_indices() {
        let next = units + character.len_utf16();
        if next > limit {
            break;
        }
        units = next;
        let end = offset + character.len_utf8();
        hard = end;

        if character == '\n' {
            line_breaks += 1;
        } else if !character.is_whitespace() {
            line_breaks = 0;
        }
        if line_breaks >= 2 {
            paragraph = Some(end);
        }

        if character.is_whitespace() {
            whitespace = Some(end);
            if sentence_pending {
                sentence = Some(end);
            }
        } else {
            sentence_pending = is_sentence_end(character);
        }
    }

    if hard == text.len() {
        return hard;
    }
    paragraph.or(sentence).or(whitespace).unwrap_or(hard)
}

const fn is_sentence_end(character: char) -> bool {
    matches!(character, '.' | '!' | '?' | '…' | '。' | '！' | '？')
}

#[cfg(test)]
mod tests {
    use super::{Chunks, MAX_BROWSER_UTTERANCE_UTF16, browser};

    #[test]
    fn short_and_empty_text_each_stay_one_exact_utterance() {
        assert_eq!(browser("short text").collect::<Vec<_>>(), ["short text"]);
        assert_eq!(browser("").collect::<Vec<_>>(), [""]);
    }

    #[test]
    fn authored_boundaries_win_in_paragraph_sentence_whitespace_order() {
        assert_eq!(
            Chunks::new("p\n\nSentence ends. tail", 20)
                .next()
                .expect("paragraph chunk"),
            "p\n\n"
        );
        assert_eq!(
            Chunks::new("Sentence ends. next words", 20)
                .next()
                .expect("sentence chunk"),
            "Sentence ends. "
        );
        assert_eq!(
            Chunks::new("alpha beta-gamma", 10)
                .next()
                .expect("whitespace chunk"),
            "alpha "
        );
    }

    #[test]
    fn unicode_and_unbroken_text_remain_complete_and_bounded() {
        let text = format!("{}{}", "😀".repeat(1_501), "x".repeat(3_001));
        let chunks: Vec<&str> = browser(&text).collect();
        assert_eq!(chunks.concat(), text);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| !chunk.is_empty()));
        assert!(
            chunks
                .iter()
                .all(|chunk| { chunk.encode_utf16().count() <= MAX_BROWSER_UTTERANCE_UTF16 })
        );
    }
}
