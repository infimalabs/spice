//! Authored one-line text: task titles and previews of authored steering.
//!
//! A line speaks the message dialect's inline Markdown (code spans, emphasis,
//! strikethrough, math, links, and task chips), but block constructs never
//! apply to it: the source that would open a heading, list, quote, rule, or
//! fence stays literal text. [`plain_text`] reads the same events for string
//! consumers with every inline marker removed and code and math source kept,
//! so a tooltip, label, or diagram node reads what the rendered line shows.

use pulldown_cmark::{CowStr, Event as MarkdownEvent, Parser, Tag, TagEnd};

use super::{OPTIONS, coalesce_text_events, inline_task_chips, math, sanitize_markdown_event};

/// What a rendered line may hold besides text.
pub(crate) enum LineControls<'r> {
    /// A free-standing line: links stay live, and every task `resolves`
    /// admits becomes a chip.
    Live {
        resolves: &'r dyn Fn(&wire::TaskHandle) -> bool,
    },
    /// A line inside another control: links and handles render as their
    /// text, so no control nests inside it.
    Nested,
}

/// Render one authored line as sanitized inline HTML.
pub(crate) fn render_line(markdown: &str, controls: &LineControls<'_>) -> String {
    let math = math::normalize(markdown, &[], &[], OPTIONS);
    let events = line_events(&math.markdown)
        .into_iter()
        .map(sanitize_markdown_event)
        .collect();
    let mut link = 0usize;
    let mut rendered = Vec::new();
    for event in coalesce_text_events(events) {
        match (event, controls) {
            // A line shows an image as its description: an image preview is a
            // control of its own.
            (MarkdownEvent::Start(Tag::Image { .. }) | MarkdownEvent::End(TagEnd::Image), _)
            | (
                MarkdownEvent::Start(Tag::Link { .. }) | MarkdownEvent::End(TagEnd::Link),
                LineControls::Nested,
            ) => {}
            (event @ MarkdownEvent::Start(Tag::Link { .. }), LineControls::Live { .. }) => {
                link += 1;
                rendered.push(event);
            }
            (event @ MarkdownEvent::End(TagEnd::Link), LineControls::Live { .. }) => {
                link = link.saturating_sub(1);
                rendered.push(event);
            }
            (event, LineControls::Live { resolves }) if link == 0 => {
                rendered.extend(inline_task_chips(event, *resolves));
            }
            (event, _) => rendered.push(event),
        }
    }
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, rendered.into_iter());
    html
}

/// The canonical plain-text projection of authored Markdown: one line with
/// inline markers removed and code and math source kept.
pub(crate) fn plain_text(markdown: &str) -> String {
    let math = math::normalize(markdown, &[], &[], OPTIONS);
    let mut text = String::new();
    let mut pending_space = false;
    for event in line_events(&math.markdown) {
        if let MarkdownEvent::Text(value)
        | MarkdownEvent::Code(value)
        | MarkdownEvent::InlineMath(value)
        | MarkdownEvent::Html(value)
        | MarkdownEvent::InlineHtml(value) = event
        {
            for character in value.chars() {
                if character.is_whitespace() {
                    pending_space = !text.is_empty();
                } else {
                    if pending_space {
                        text.push(' ');
                        pending_space = false;
                    }
                    text.push(character);
                }
            }
        }
    }
    text
}

/// The inline events of one authored line, every block construct dissolved
/// into the literal source around its content.
fn line_events(markdown: &str) -> Vec<MarkdownEvent<'_>> {
    let mut events = Vec::new();
    let mut covered = 0usize;
    let mut boundary = true;
    for (event, source) in Parser::new_ext(markdown, OPTIONS).into_offset_iter() {
        let event = match event {
            MarkdownEvent::Start(tag) if !inline_tag(&tag) => {
                boundary = true;
                continue;
            }
            MarkdownEvent::End(tag) if !inline_tag_end(tag) => {
                boundary = true;
                continue;
            }
            // Their source joins the next literal gap.
            MarkdownEvent::Rule | MarkdownEvent::TaskListMarker(_) => {
                boundary = true;
                continue;
            }
            MarkdownEvent::SoftBreak | MarkdownEvent::HardBreak => {
                MarkdownEvent::Text(CowStr::Borrowed(" "))
            }
            MarkdownEvent::DisplayMath(math) => MarkdownEvent::InlineMath(math),
            MarkdownEvent::FootnoteReference(_) => {
                MarkdownEvent::Text(CowStr::Borrowed(&markdown[source.clone()]))
            }
            other => other,
        };
        if boundary {
            push_gap(
                &markdown[covered..source.start.max(covered)],
                false,
                &mut events,
            );
            boundary = false;
        }
        covered = covered.max(source.end);
        events.push(event);
    }
    if boundary {
        push_gap(&markdown[covered..], true, &mut events);
    }
    events
}

/// Literal source between inline content, with its whitespace folded to the
/// single spaces one line keeps.
fn push_gap(gap: &str, last: bool, events: &mut Vec<MarkdownEvent<'_>>) {
    let mut words = gap.split_whitespace();
    let first = words.next();
    let mut text = String::with_capacity(gap.len());
    if !events.is_empty() && gap.starts_with(char::is_whitespace) && !(last && first.is_none()) {
        text.push(' ');
    }
    if let Some(first) = first {
        text.push_str(first);
        for word in words {
            text.push(' ');
            text.push_str(word);
        }
        if !last && gap.ends_with(char::is_whitespace) {
            text.push(' ');
        }
    }
    if !text.is_empty() {
        events.push(MarkdownEvent::Text(text.into()));
    }
}

const fn inline_tag(tag: &Tag<'_>) -> bool {
    matches!(
        tag,
        Tag::Emphasis
            | Tag::Strong
            | Tag::Strikethrough
            | Tag::Superscript
            | Tag::Subscript
            | Tag::Link { .. }
            | Tag::Image { .. }
    )
}

const fn inline_tag_end(tag: TagEnd) -> bool {
    matches!(
        tag,
        TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::Link
            | TagEnd::Image
    )
}

#[cfg(test)]
mod tests;
