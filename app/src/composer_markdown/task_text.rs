//! Authored task text: descriptions, acceptance criteria, observations, and
//! closure validation.
//!
//! Task text is Markdown written by operators and agents with the same intent
//! as message prose, so it renders in the one message dialect: math, Mermaid
//! fences, footnotes, tables, images, and the same sanitization. The detail
//! surface owns only two differences: headings nest under its own section
//! headings, and only handles that resolve on the admitted board become task
//! chips, so no chip leads nowhere.

use wire::TaskStamp;

use super::{BTreeMap, Finish, RenderedMarkdown, render};

/// Render one authored task text field.
///
/// `quotes` presents the field's quote links, as the resident resolved them;
/// `resolves` answers whether a task stamp is present on the admitted board.
pub(crate) fn render_task_text(
    markdown: &str,
    quotes: &[wire::QuotePresentation],
    resolves: impl Fn(&TaskStamp) -> bool,
) -> RenderedMarkdown {
    let admitted = |task: &wire::TaskHandle| task.born().is_ok_and(|stamp| resolves(&stamp));
    render(
        markdown,
        super::Annotations::default(),
        quotes,
        &super::QuoteAgents::new(),
        &BTreeMap::new(),
        &[],
        Finish::TaskText {
            resolves: &admitted,
        },
    )
}

#[cfg(test)]
mod tests;
