//! The operator's own direction as top-level team cards.

use std::collections::BTreeMap;

use wire::{AgentActivityKind, AgentMessage, CanonicalUuid, MessageDirective, QuoteRecipient};

/// The operator's direction cards one team shows: one per send, however many
/// of its members and deliveries recorded it.
///
/// A direction row carries one context per directive its transcript record
/// delivered. Each context becomes its own card, joined with every other
/// delivery of the same send to one of `members`: the send's fanout names it,
/// else its directive does. The canonical card retains the send's earliest
/// recorded delivery as its source locus, but displays at the directive's
/// operator-authored instant. It names every member that recorded the send, in
/// member order, so a resend or an older page never adds a second card.
#[must_use]
pub(crate) fn direction_cards<'m>(
    messages: impl IntoIterator<Item = &'m AgentMessage>,
    members: &[CanonicalUuid],
) -> Vec<AgentMessage> {
    let mut sends = BTreeMap::<String, (AgentMessage, Vec<QuoteRecipient>)>::new();
    for message in messages.into_iter().filter(|message| {
        message.activity == AgentActivityKind::Direction && members.contains(&message.lane)
    }) {
        for context in &message.directive_contexts {
            let Some(delivery) = &context.delivery else {
                continue;
            };
            let Some(card) = direction_card(message, context) else {
                continue;
            };
            let send = delivery.fanout.map_or_else(
                || context.quote_target.to_string(),
                |fanout| fanout.to_string(),
            );
            let (earliest, recipients) = sends
                .entry(send)
                .or_insert_with(|| (card.clone(), Vec::new()));
            if earliest.direction_superseded_by(&card) {
                *earliest = card;
            }
            for recipient in &context.attribution.recipients {
                if members.contains(&recipient.lane) && !recipients.contains(recipient) {
                    recipients.push(*recipient);
                }
            }
        }
    }
    sends
        .into_values()
        .map(|(mut card, mut recipients)| {
            recipients
                .sort_by_key(|recipient| members.iter().position(|lane| *lane == recipient.lane));
            let authored = card.directive_contexts[0].authored;
            card.authored = authored;
            card.authored_synthetic = false;
            card.observed = None;
            if let Some(context) = card.directive_contexts.first_mut() {
                context.attribution.recipients = recipients;
            }
            card
        })
        .collect()
}
#[must_use]
pub(crate) fn is_fanout_delivery(message: &AgentMessage) -> bool {
    message.directive_contexts.iter().any(|context| {
        context.attribution.recipients.len() > 1
            && context
                .delivery
                .as_ref()
                .is_some_and(|delivery| delivery.fanout.is_some())
    })
}

/// One directive of a direction row as a card of its own: the context's quote
/// alone.
fn direction_card(message: &AgentMessage, context: &MessageDirective) -> Option<AgentMessage> {
    let start = usize::try_from(context.start).ok()?;
    let end = usize::try_from(context.end).ok()?;
    let text = message.text.get(start..end)?.to_owned();
    Some(AgentMessage {
        directive_contexts: vec![MessageDirective {
            start: 0,
            end: context.end - context.start,
            ..context.clone()
        }],
        text,
        quote_target: None,
        ..message.clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(last: u8) -> CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131163{last:02x}")
            .parse()
            .expect("fixture uuid")
    }

    // Principal direction fixture.
    fn context(key: &str, text: &str, fanout: Option<CanonicalUuid>) -> MessageDirective {
        MessageDirective {
            start: 0,
            end: u32::try_from(text.len()).expect("fixture length"),
            quote_target: wire::QuoteTarget::Steering {
                key: key.to_owned(),
                digest: wire::QuoteTarget::text_digest(text),
            },
            media_quotes: Vec::new(),
            attribution: wire::QuoteAttribution {
                author: wire::QuoteAuthor::Directive {
                    actor: wire::Actor::TEST_PRINCIPAL,
                },
                recipients: Vec::new(),
                packet: false,
            },
            authored: "2030-04-12T18:44:08.000Z".parse().expect("fixture instant"),
            badge: None,
            review: None,
            line: None,
            delivery: Some(wire::MessageDelivery {
                fanout,
                provisional: false,
            }),
        }
    }

    /// One delivery row on `lane` at `offset`, recording `quotes` in order.
    fn row(
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
        offset: u64,
        quotes: &[(&str, &str, Option<CanonicalUuid>)],
    ) -> AgentMessage {
        let mut text = String::new();
        let mut contexts = Vec::new();
        for (key, quote, fanout) in quotes {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            let start = u32::try_from(text.len()).expect("fixture length");
            text.push_str(quote);
            let mut context = context(key, quote, *fanout);
            context.start = start;
            context.end = u32::try_from(text.len()).expect("fixture length");
            context.attribution.recipients.push(QuoteRecipient {
                lane,
                occupant: None,
            });
            contexts.push(context);
        }
        AgentMessage {
            prose_origin: None,
            lane,
            author: occupant,
            locus: wire::MessageLocus {
                member: 1,
                offset,
                length: 1,
                event: 0,
            },
            // Delivery order is time order, whatever lane recorded it.
            authored: format!("2030-04-12T18:44:08.{offset:03}Z")
                .parse()
                .expect("fixture instant"),
            authored_synthetic: false,
            observed: None,
            activity: AgentActivityKind::Direction,
            compaction_method: None,
            final_answer: false,
            acknowledgement: None,
            text,
            provider_error: None,
            quote_target: None,
            directive_contexts: contexts,
            sends: Vec::new(),
            quotes: Vec::new(),
            automatic_speech: None,
            media: Vec::new(),
            media_quotes: Vec::new(),
            markdown_images: Vec::new(),
            context_bytes: 0,
        }
    }

    fn receivers(card: &AgentMessage) -> Vec<CanonicalUuid> {
        card.directive_contexts[0]
            .attribution
            .recipients
            .iter()
            .map(|recipient| recipient.lane)
            .collect()
    }

    #[test]
    fn one_send_is_one_card_naming_every_member_at_its_first_delivery() {
        let (first, second, outside) = (uuid(0xa1), uuid(0xa2), uuid(0xa3));
        let fanout = Some(uuid(0xf1));
        let send = ("dW4Wl4fRP", "> Ship it", fanout);
        let rows = [
            row(second, uuid(0xb2), 40, &[send]),
            row(first, uuid(0xb1), 70, &[send]),
            // A resend later on the same lane is the same send.
            row(second, uuid(0xb2), 90, &[send]),
            row(outside, uuid(0xb3), 10, &[send]),
        ];

        let cards = direction_cards(&rows, &[first, second]);

        assert_eq!(cards.len(), 1);
        let card = &cards[0];
        assert_eq!((card.lane, card.locus.offset), (second, 40));
        assert_eq!(receivers(card), vec![first, second]);
        assert_eq!(card.text, "> Ship it");
        assert!(card.quote_target.is_none());
        assert!(wire::valid_message_directives(
            &card.text,
            &card.directive_contexts
        ));
        assert!(is_fanout_delivery(card));
    }

    #[test]
    fn transcript_delivery_supersedes_the_provisional_send_in_every_arrival_order() {
        let lane = uuid(0xa1);
        let send = ("dW4Wl4fRP", "> Ship it", Some(uuid(0xf1)));
        let mut provisional = row(lane, uuid(0xb1), 10, &[send]);
        provisional
            .directive_contexts
            .first_mut()
            .and_then(|context| context.delivery.as_mut())
            .expect("fixture direction delivery")
            .provisional = true;
        let delivered = row(lane, uuid(0xb1), 90, &[send]);

        for rows in [[&provisional, &delivered], [&delivered, &provisional]] {
            let cards = direction_cards(rows, &[lane]);
            let [card] = cards.as_slice() else {
                panic!("one canonical direction card: {cards:?}");
            };
            assert_eq!(card.locus.offset, delivered.locus.offset);
            assert!(
                !card.directive_contexts[0]
                    .delivery
                    .as_ref()
                    .expect("direction delivery")
                    .provisional
            );
            assert!(!is_fanout_delivery(card));
            assert_eq!(
                crate::runtime::agent_message_card_identity(card),
                "agent-018f82d4-6f59-7d06-92e3-bce9131163a1-direction-dW4Wl4fRP"
            );
        }
    }

    #[test]
    fn a_response_after_its_request_stays_newer_than_the_delayed_direction_card() {
        let lane = uuid(0xa1);
        let mut delivery = row(
            lane,
            uuid(0xb1),
            90,
            &[("dW4Wl4fRP", "> Ship it", Some(uuid(0xf1)))],
        );
        delivery.observed = Some(
            "2030-04-12T18:44:08.100Z"
                .parse()
                .expect("fixture observation instant"),
        );
        delivery.authored_synthetic = true;
        let mut response = row(lane, uuid(0xb1), 50, &[]);
        response.activity = AgentActivityKind::Prose;

        let card = direction_cards([&delivery], &[lane])
            .pop()
            .expect("direction card");

        assert_eq!(
            card.display_millis(),
            delivery.directive_contexts[0]
                .authored
                .get()
                .as_millisecond()
        );
        assert!(!card.authored_synthetic);
        assert!(card.chronology_key() < response.chronology_key());
    }

    #[test]
    fn each_directive_one_record_delivered_is_its_own_card() {
        let lane = uuid(0xa1);
        let rows = [row(
            lane,
            uuid(0xb1),
            40,
            &[
                ("dW4Wl4fRP", "> First", Some(uuid(0xf1))),
                ("dW4Wl4fRQ", "> Second", None),
            ],
        )];

        let mut texts = direction_cards(&rows, &[lane])
            .into_iter()
            .map(|card| {
                assert!(wire::valid_message_directives(
                    &card.text,
                    &card.directive_contexts
                ));
                card.text
            })
            .collect::<Vec<_>>();
        texts.sort();

        assert_eq!(texts, ["> First", "> Second"]);
    }

    #[test]
    fn a_team_without_a_receiving_member_shows_no_card() {
        let rows = [row(
            uuid(0xa1),
            uuid(0xb1),
            40,
            &[("dW4Wl4fRP", "> Ship", None)],
        )];

        assert!(direction_cards(&rows, &[uuid(0xa2)]).is_empty());
    }
}
