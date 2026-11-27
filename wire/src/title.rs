//! The title: the short name a task carries on the board and in its landing
//! commit subject.
//!
//! Every route that writes an authored title admits it through [`admit`]; a
//! route that synthesizes a title from other text with no author to ask cuts
//! it once through [`fit`] and keeps the full text elsewhere.

use crate::rooms::one_line;

/// Most Unicode scalar values a task title may hold: an authoring bound,
/// never a truncation width for authored text.
pub const TITLE_CHARS: usize = 160;

/// An authored title that holds more than [`TITLE_CHARS`] scalar values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TitleTooLong {
    /// Scalar values counted after trimming.
    pub chars: usize,
}

impl TitleTooLong {
    /// Typed refusal reason.
    pub const REASON: &'static str = "task::title-too-long";

    /// What the author changes to pass.
    #[must_use]
    pub fn remedy(self) -> String {
        format!(
            "shorten the title from {} to at most {TITLE_CHARS} characters; the detail belongs \
             in the pitch or description",
            self.chars
        )
    }
}

/// Admit an authored title against the length bound.
///
/// # Errors
///
/// The trimmed title holds more than [`TITLE_CHARS`] scalar values.
pub fn admit(title: &str) -> Result<(), TitleTooLong> {
    let chars = title.trim().chars().count();
    if chars > TITLE_CHARS {
        return Err(TitleTooLong { chars });
    }
    Ok(())
}

/// A title synthesized once from `text` at a write with no author to ask:
/// `text` itself when it admits, else whitespace collapsed and cut to
/// [`TITLE_CHARS`] with an ellipsis.
#[must_use]
pub fn fit(text: &str) -> String {
    if admit(text).is_ok() {
        return text.to_owned();
    }
    one_line(text, TITLE_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bound_admits_exactly_its_width_and_refuses_one_more() {
        assert_eq!(admit(&"é".repeat(TITLE_CHARS)), Ok(()));
        assert_eq!(
            admit(&format!("  {}  ", "é".repeat(TITLE_CHARS))),
            Ok(()),
            "surrounding whitespace is not part of the title"
        );
        assert_eq!(
            admit(&"é".repeat(TITLE_CHARS + 1)),
            Err(TitleTooLong {
                chars: TITLE_CHARS + 1
            })
        );
    }

    #[test]
    fn a_synthesized_title_always_admits() {
        let report = "word ".repeat(TITLE_CHARS);
        let title = fit(&report);
        assert_eq!(fit("short  report"), "short  report");
        assert!(title.ends_with('…'));
    }
}
