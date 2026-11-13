//! Task identities reported by authored message text.

use wire::TaskHandle;

use super::canonical_task_handle;

/// Canonical task handles and their byte ranges in authored text, in order.
///
/// Markdown punctuation is a boundary rather than part of a handle, so prose
/// may name one in backticks, parentheses, or at the end of a sentence. The
/// wire handle parser admits the outer stem/key shape, then `born` admits the
/// fixed-width base-52 task stamp. Every authored occurrence is reported, so
/// the browser replaces each handle where it was written.
#[must_use]
pub fn task_mentions(text: &str) -> Vec<(std::ops::Range<usize>, TaskHandle)> {
    let mut mentions = Vec::new();
    let mut start = None;
    for (index, character) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        if character.is_ascii_alphanumeric() || character == '-' {
            start.get_or_insert(index);
        } else if let Some(start) = start.take()
            && let Some(task) = canonical_task_handle(&text[start..index])
        {
            mentions.push((start..index, task));
        }
    }
    mentions
}
