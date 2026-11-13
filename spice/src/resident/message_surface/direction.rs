//! The operator's own direction, presented where a lane's transcript records
//! receiving it.
//!
//! Spice delivers direction as operator-role prose whose sealed headers name
//! each directive's key. Only the journal authenticates what a header names:
//! a key presents as the operator's card when its creating fact is a plain
//! operator direction to the receiving lane, never a peer note, a packet, or a
//! resident-generated reminder. Records that carry no such header (bootstrap
//! prompts, notices, briefings, room catch-up) present nothing.

use super::acknowledgement::{PresentedDirectiveContext, append_acknowledgement_quote};
use super::*;

#[cfg(test)]
mod tests;

/// Keys of the direction one operator-role text delivers, in header order:
/// every sealed header that names no peer sender. A post-compaction replay
/// repeats direction its lane already received and delivers none.
fn delivered_keys(text: &str) -> Vec<steer::Key> {
    if steer::inbox::replays(text) {
        return Vec::new();
    }
    let peers = steer::inbox::peer_keys(text).collect::<BTreeSet<_>>();
    let mut keys = Vec::new();
    for key in steer::inbox::keys(text).filter(|key| !peers.contains(key)) {
        if let Ok(key) = key.parse::<steer::Key>()
            && !keys.contains(&key)
        {
            keys.push(key);
        }
    }
    keys
}

pub(super) fn operator_text(event: &listen::Event) -> Option<&str> {
    match event {
        listen::Event::Prose(prose) if prose.role == listen::ProseRole::Operator => {
            Some(prose.text.as_str())
        }
        _ => None,
    }
}

/// Every key the operator-role prose among `events` delivers.
pub(super) fn delivered<'a>(
    events: impl IntoIterator<Item = &'a listen::Event>,
) -> BTreeSet<steer::Key> {
    events
        .into_iter()
        .filter_map(operator_text)
        .flat_map(delivered_keys)
        .collect()
}

/// Present the plain operator direction one operator-role record delivered
/// to the event's lane, one quote per directive in header order. Each
/// context carries its directive's attribution and the send's fanout.
pub(super) fn present(
    text: &str,
    steering: Option<&PresentationDirectives>,
    event: PresentationEvent,
) -> Result<Option<serve::stream::MessagePresentation>, String> {
    let Some(steering) = steering else {
        return Ok(None);
    };
    let mut rendered = String::new();
    let mut contexts = Vec::new();
    for key in delivered_keys(text) {
        let Some(directive) = steering.directives.get(&key) else {
            continue;
        };
        if !steering.operator_sent(directive)
            || matches!(
                directive.attribution.author,
                wire::QuoteAuthor::Agent { .. }
            )
            || directive.attribution.packet
            || directive.lane != event.lane
        {
            continue;
        }
        if !rendered.is_empty() {
            rendered.push_str("\n\n");
        }
        let start = rendered.len();
        append_acknowledgement_quote(&mut rendered, &directive.text);
        contexts.push(PresentedDirectiveContext {
            key,
            range: start..rendered.len(),
        });
    }
    if contexts.is_empty() {
        return Ok(None);
    }
    let presented = Presentation {
        text: rendered,
        contexts,
        sends: Vec::new(),
        acknowledgement: None,
    };
    let directive_contexts = directive_context::hydrate_directive_contexts(&presented, steering)?
        .into_iter()
        .zip(&presented.contexts)
        .map(|(mut context, presented)| {
            context.delivery = Some(wire::MessageDelivery {
                fanout: steering.directives[&presented.key].fanout,
                provisional: false,
            });
            context
        })
        .collect();
    Ok(Some(serve::stream::MessagePresentation {
        text: presented.text,
        acknowledgement: None,
        directive_contexts,
        sends: Vec::new(),
    }))
}
