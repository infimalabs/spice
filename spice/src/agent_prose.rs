//! Transcript event shapes promoted into quoteable agent cards.
//!
//! Assistant prose and provider failures are durable authored text. Tool,
//! reasoning, and boundary events are presentation only.
/// Assistant prose one transcript event presents as a quoteable message card.
///
/// Ordinary assistant prose presents itself unless its text is blank: a blank
/// block carries no words and no media, which decompose from that text.
/// Streamed prose presents when it carries an acknowledgement or a placed SYN
/// packet, since that visible block is the packet's source. A tool call's
/// intent is call metadata and never presents as prose.
#[must_use]
pub fn quoteable(decoded: &listen::Event) -> Option<&listen::Prose> {
    match decoded {
        listen::Event::Prose(prose)
            if prose.role == listen::ProseRole::Assistant && !prose.text.trim().is_empty() =>
        {
            Some(prose)
        }
        listen::Event::StreamedProse(prose)
            if prose.role == listen::ProseRole::Assistant
                && (steer::has_acknowledgement(&prose.text)
                    || steer::ack::scan::has_message(&prose.text)) =>
        {
            Some(prose)
        }
        _ => None,
    }
}

/// Exact authored text one transcript event may expose as a durable quote.
#[must_use]
pub fn quoteable_text(decoded: &listen::Event) -> Option<&str> {
    quoteable(decoded)
        .map(|prose| prose.text.as_str())
        .or_else(|| match decoded {
            listen::Event::Failure(failure) if !failure.message.trim().is_empty() => {
                Some(failure.message.as_str())
            }
            _ => None,
        })
}
