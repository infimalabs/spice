//! Quote presentations a committed send fills from the cards this runtime
//! already holds, so its provisional card renders complete before the
//! transcript echo supplies the resident's own resolution.

use pulldown_cmark::{Event as MarkdownEvent, Parser, Tag, TagEnd};
use wire::{AgentMessage, QuoteAttribution, QuoteAuthor, QuotePresentation, QuoteTarget};

use super::rooms::ConversationProjection;

/// Structural quote links in message order: ordinary Markdown links whose
/// complete visible label is `quote` and whose destination names the quote
/// authority, exactly as the resident recognizes them.
pub(super) fn structural_quote_targets(text: &str) -> Vec<String> {
    if !text.contains("spice://quote/") {
        return Vec::new();
    }
    let mut targets = Vec::new();
    let mut open: Option<(String, bool)> = None;
    for event in Parser::new(text) {
        match event {
            MarkdownEvent::Start(Tag::Link { dest_url, .. }) => {
                open = Some((dest_url.into_string(), false));
            }
            MarkdownEvent::Text(label) if open.is_some() => {
                let Some((_, admitted)) = open.as_mut() else {
                    unreachable!("the match guard observed an open link");
                };
                if !*admitted && label.as_ref() == "quote" {
                    *admitted = true;
                } else {
                    open = None;
                }
            }
            MarkdownEvent::End(TagEnd::Link) => {
                if let Some((target, true)) = open.take()
                    && target.starts_with("spice://quote/")
                {
                    targets.push(target);
                }
            }
            _ if open.is_some() => open = None,
            _ => {}
        }
    }
    targets
}

/// The presentations `text`'s quote links take from retained cards, in
/// message order, skipping every link whose source this page does not hold
/// under the exact target it names. A skipped link stays bare until the
/// transcript echo resolves it.
pub(super) fn retained_presentations(
    text: &str,
    cards: &[std::rc::Rc<AgentMessage>],
    conversation: &ConversationProjection,
) -> Vec<QuotePresentation> {
    structural_quote_targets(text)
        .into_iter()
        .filter_map(|target| {
            let parsed = target.parse::<QuoteTarget>().ok()?;
            match &parsed {
                QuoteTarget::Tape { .. } => retained_prose(&target, &parsed, cards),
                QuoteTarget::Steering { key, .. } => {
                    retained_direction(&target, &parsed, key, cards, conversation)
                }
                QuoteTarget::Room { .. }
                | QuoteTarget::TapeMedia { .. }
                | QuoteTarget::SteeringMedia { .. } => None,
            }
        })
        .collect()
}

/// A retained prose card presenting itself as the quoted source.
///
/// The card's own `quote_target` is the resident's authentication of its
/// presented text: the digest names the authored prose the page never sees,
/// so target equality, digest included, is the whole admission.
fn retained_prose(
    target: &str,
    parsed: &QuoteTarget,
    cards: &[std::rc::Rc<AgentMessage>],
) -> Option<QuotePresentation> {
    let card = cards
        .iter()
        .find(|card| card.quote_target.as_ref() == Some(parsed))?;
    let text = wire::media::with_media_payloads(&card.text, &card.media)
        .ok()?
        .into_owned();
    Some(QuotePresentation::Resolved {
        target: target.to_owned(),
        text,
        attribution: QuoteAttribution {
            author: QuoteAuthor::Agent {
                lane: card.lane,
                occupant: card.author,
            },
            recipients: Vec::new(),
            packet: false,
        },
        disposition: None,
        quotes: card.quotes.clone(),
        media_quotes: card.media_quotes.clone(),
    })
}

/// A retained delivered direction presenting its exact delivery text under
/// the attribution its card already wears.
fn retained_direction(
    target: &str,
    parsed: &QuoteTarget,
    key: &str,
    cards: &[std::rc::Rc<AgentMessage>],
    conversation: &ConversationProjection,
) -> Option<QuotePresentation> {
    let delivery = conversation.keyed_delivery(key)?;
    if QuoteTarget::text_digest(&delivery.value.text) != *parsed.digest() {
        return None;
    }
    let (card, context) = cards.iter().find_map(|card| {
        if card.activity != wire::AgentActivityKind::Direction {
            return None;
        }
        card.directive_contexts
            .iter()
            .find(|context| context.quote_target == *parsed)
            .map(|context| (card, context))
    })?;
    let text = wire::media::with_media_payloads(&delivery.value.text, &delivery.value.attachments)
        .ok()?
        .into_owned();
    Some(QuotePresentation::Resolved {
        target: target.to_owned(),
        text,
        attribution: context.attribution.clone(),
        disposition: Some(delivery.value.disposition),
        quotes: card.quotes.clone(),
        media_quotes: context.media_quotes.clone(),
    })
}
