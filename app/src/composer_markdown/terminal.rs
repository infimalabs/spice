//! Give terminal prose on an assistant card sentence punctuation.
//!
//! The app stacks cards newest-first, so whatever a trailing colon introduces
//! lands above it and the colon points at nothing. A terminal colon therefore
//! becomes a period, and terminal prose without punctuation gains one. Only
//! the final text of a card that ends in prose changes: a card ending in code,
//! a quote, a table, a link, an image, math, or markup stays verbatim.

use pulldown_cmark::{CowStr, Event, TagEnd};

pub(super) fn period(events: &mut [Event<'_>]) {
    for event in events.iter_mut().rev() {
        match event {
            Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::Item
                | TagEnd::List(_)
                | TagEnd::Emphasis
                | TagEnd::Strong
                | TagEnd::Strikethrough,
            ) => {}
            Event::Text(text) => {
                let body = text.trim_end();
                let Some(last) = body.chars().next_back() else {
                    return;
                };
                if last == ':' || !has_terminal_punctuation(last) {
                    let stem = body.strip_suffix(':').unwrap_or(body);
                    let presented = format!("{stem}.{}", &text[body.len()..]);
                    *text = CowStr::from(presented);
                }
                return;
            }
            _ => return,
        }
    }
}

/// ASCII punctuation and every non-ASCII punctuation character emitted by
/// pulldown-cmark's smart-punctuation transform.
fn has_terminal_punctuation(character: char) -> bool {
    character.is_ascii_punctuation()
        || matches!(
            character,
            '\u{2018}'
                | '\u{2019}'
                | '\u{201c}'
                | '\u{201d}'
                | '\u{2013}'
                | '\u{2014}'
                | '\u{2026}'
        )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::{
        SIGNOFF_SLOT, render_attributed_received, render_prose_card, render_received,
    };

    #[test]
    fn a_prose_card_presents_its_terminal_prose_with_sentence_punctuation() {
        let card = |markdown: &str| {
            render_prose_card(
                markdown,
                &[],
                &[],
                &[],
                &crate::composer_markdown::QuoteAgents::new(),
                &BTreeMap::new(),
            )
            .html
            .replacen(SIGNOFF_SLOT, "", 1)
        };
        let cases = [
            (
                "Reading the show call path",
                "<p>Reading the show call path.</p>\n",
            ),
            ("Reading the path  \n", "<p>Reading the path.</p>\n"),
            (
                "Reading the show call path:",
                "<p>Reading the show call path.</p>\n",
            ),
            ("Reading the path:  \n", "<p>Reading the path.</p>\n"),
            (
                "Note: see https://example.test:8767 at 10:30 first:",
                "<p>Note: see https://example.test:8767 at 10:30 first.</p>\n",
            ),
            ("**Next:**", "<p><strong>Next.</strong></p>\n"),
            (
                "Steps:\n\n- read\n- check:",
                "<p>Steps:</p>\n<ul>\n<li>read</li>\n<li>check.</li>\n</ul>\n",
            ),
            (
                "Steps\n\n- read\n- check",
                "<p>Steps</p>\n<ul>\n<li>read</li>\n<li>check.</li>\n</ul>\n",
            ),
            (
                "Run `spice task next:`",
                "<p>Run <code>spice task next:</code></p>\n",
            ),
            ("Run:\n\n```\nkey:\n```", "<p>Run:</p>\n"),
            ("Run:\n\n```\nkey:\n```", "<pre><code>key:\n</code></pre>\n"),
            ("Quoted:\n\n> wait:", "<p>wait:</p>\n"),
            (
                "Colons: inside, then a stop.",
                "<p>Colons: inside, then a stop.</p>\n",
            ),
            ("Still waiting?", "<p>Still waiting?</p>\n"),
            ("A pause…", "<p>A pause…</p>\n"),
        ];
        for (markdown, expected) in cases {
            assert!(card(markdown).contains(expected), "{markdown:?}");
        }
        assert_eq!(
            render_received("Nested quote bodies stay verbatim:").html,
            "<p>Nested quote bodies stay verbatim:</p>\n"
        );
    }

    #[test]
    fn attributed_received_text_leads_with_signoff_without_rewriting_the_error() {
        let error = "Codex error event: Codex is at capacity, please try again later (code=turn_rate_limit_exceeded)";
        let html = render_attributed_received(error).html;
        let signoff = html.find(SIGNOFF_SLOT).expect("signoff slot is present");
        let words = html.find(error).expect("error text is present");
        assert!(signoff < words, "{html}");
        assert!(html.contains(&format!("<p>{error}</p>")), "{html}");
    }
}
