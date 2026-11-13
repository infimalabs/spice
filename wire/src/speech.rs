//! Text rules shared by the resident's Speak selection and the browser's
//! narration, so both voices cut a message in the same place and say the same
//! words for it.

use crate::AutomaticSpeech;

impl AutomaticSpeech {
    /// The body of one trimmed Markdown list-item line, or `None` for prose.
    ///
    /// Automatic Speak and Narrate voice a message's edge paragraphs only up to
    /// its first list: items are detail to read on the card, and a list that
    /// closes a message would otherwise be voiced whole as its last paragraph.
    #[must_use]
    pub fn list_item(line: &str) -> Option<&str> {
        let after_marker = match line.strip_prefix(['-', '*', '+']) {
            Some(rest) => rest,
            None => {
                let digits = line.bytes().take_while(u8::is_ascii_digit).count();
                if !(1..=9).contains(&digits) {
                    return None;
                }
                line[digits..].strip_prefix(['.', ')'])?
            }
        };
        after_marker.strip_prefix([' ', '\t']).map(str::trim_start)
    }

    /// One Markdown line as words to say.
    ///
    /// Heading and list markers go, and so do emphasis, strike, and code
    /// delimiters, which a voice would otherwise read as punctuation names.
    /// Characters that carry meaning stay: `snake_case`, spaced `2 * 3`, and a
    /// single `~5`.
    #[must_use]
    pub fn plain_line(line: &str) -> String {
        let line = line.trim();
        let line = Self::list_item(line)
            .or_else(|| heading_text(line))
            .unwrap_or(line);
        let mut plain = String::with_capacity(line.len());
        let mut cursor = 0;
        while let Some(character) = line[cursor..].chars().next() {
            let end = if matches!(character, '*' | '_' | '~' | '`') {
                line[cursor..]
                    .find(|other| other != character)
                    .map_or(line.len(), |run| cursor + run)
            } else {
                cursor + character.len_utf8()
            };
            let before = line[..cursor].chars().next_back();
            let after = line[end..].chars().next();
            let delimiter = match character {
                '`' => true,
                '~' => end - cursor >= 2,
                '*' => {
                    !(before.is_none_or(char::is_whitespace)
                        && after.is_none_or(char::is_whitespace))
                }
                '_' => {
                    !(before.is_some_and(char::is_alphanumeric)
                        && after.is_some_and(char::is_alphanumeric))
                }
                _ => false,
            };
            if !delimiter {
                plain.push_str(&line[cursor..end]);
            }
            cursor = end;
        }
        plain
    }
}

/// The text of an ATX heading line; `#123` without a space is not a heading.
fn heading_text(line: &str) -> Option<&str> {
    let hashes = line.bytes().take_while(|&byte| byte == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    line[hashes..]
        .strip_prefix([' ', '\t'])
        .map(str::trim_start)
}

#[cfg(test)]
mod tests {
    use crate::AutomaticSpeech;

    #[test]
    fn plain_line_drops_markers_and_keeps_meaningful_characters() {
        for (line, spoken) in [
            ("- **Change:** one `line`.", "Change: one line."),
            ("## _Checked_ ~~old~~ new", "Checked old new"),
            ("1) __init__ and *it*", "init and it"),
            (
                "snake_case, x_7a12bc99, 2 * 3, ~5 ms",
                "snake_case, x_7a12bc99, 2 * 3, ~5 ms",
            ),
            ("#123 stays a reference", "#123 stays a reference"),
        ] {
            assert_eq!(AutomaticSpeech::plain_line(line), spoken, "{line}");
        }
    }
}
