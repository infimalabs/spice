//! Canonical authored-message composition shared by lane and Draft transports.

use std::borrow::Borrow;

use wire::Media;

/// One canonical quote target and the reply the operator typed beneath it.
pub(crate) struct QuotedReply<'a, A = Media> {
    /// Canonical resident-supplied source target.
    pub(crate) target: &'a str,
    /// Reply body shown under the link, blank when the operator wrote none.
    pub(crate) body: &'a str,
    /// Attachments pasted into this quoted reply composer.
    pub(crate) attachments: &'a [A],
}

/// Join every visible quote shard, the primary body, and their attachments.
///
/// The quote shards precede the primary composer in the visible vertical stack,
/// so their document parts precede the primary body here as well. Each
/// attachment follows the text block whose composer received it. Parts are
/// separated by the blank line Markdown reads as a block boundary, and a part
/// that would contribute nothing is dropped rather than emitted empty.
pub(crate) fn compose<A: Borrow<Media>>(
    body: &str,
    quotes: &[QuotedReply<'_, A>],
    attachments: &[A],
) -> String {
    compose_with_media(body, quotes, attachments, true)
}

/// Compose the same visible shards while carrying their admitted media
/// separately instead of embedding their authenticated references in Markdown.
pub(crate) fn compose_without_media<A: Borrow<Media>>(
    body: &str,
    quotes: &[QuotedReply<'_, A>],
    attachments: &[A],
) -> String {
    compose_with_media(body, quotes, attachments, false)
}

fn compose_with_media<A: Borrow<Media>>(
    body: &str,
    quotes: &[QuotedReply<'_, A>],
    attachments: &[A],
    references: bool,
) -> String {
    let mut parts = Vec::new();
    for quoted in quotes {
        let link = format!("[quote]({})", quoted.target);
        let body = quoted.body.trim();
        parts.push(if body.is_empty() {
            link
        } else {
            format!("{link}\n\n{body}")
        });
        if references {
            parts.extend(
                quoted
                    .attachments
                    .iter()
                    .map(|attachment| attachment_markdown(attachment.borrow())),
            );
        }
    }
    let body = body.trim();
    if !body.is_empty() {
        parts.push(body.to_owned());
    }
    if references {
        parts.extend(
            attachments
                .iter()
                .map(|attachment| attachment_markdown(attachment.borrow())),
        );
    }
    parts.join("\n\n")
}

fn attachment_markdown(attachment: &Media) -> String {
    attachment
        .reference_markdown()
        .expect("composer attachments passed admission before storage")
}

/// Normalize line endings, then prefix every line as one Markdown block quote.
///
/// The trim happens to the source and never to the joined result. Trimming the
/// result takes the space off the marker on a final blank line, leaving a bare
/// `>` on the last line, and it collapses whitespace-only text to a lone `>`
/// instead of to nothing, which is a directive the caller then has no reason to
/// suppress. Trimming the source empties that case, so callers can drop it.
pub(crate) fn block_quote(raw: &str) -> String {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    trimmed
        .lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}
