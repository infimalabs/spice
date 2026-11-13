//! Canonical browser-card reduction for transcript message streams.

use super::*;

use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct MessageCardKey {
    lane: CanonicalUuid,
    locus: MessageLocus,
}

impl From<&AgentMessage> for MessageCardKey {
    fn from(message: &AgentMessage) -> Self {
        Self {
            lane: message.lane,
            locus: message.locus,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MessageCardKind {
    Prose,
    Failure,
    ToolMedia,
    Compaction,
    Session,
    SignOut,
    Direction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MessageCardFingerprint<'a> {
    kind: MessageCardKind,
    author: CanonicalUuid,
    authored: &'a wire::Instant,
    authored_synthetic: bool,
    observed: Option<&'a wire::Instant>,
    text: &'a str,
    compaction_method: Option<&'a str>,
    final_answer: bool,
    prose_origin: Option<&'a wire::MessageProseOrigin>,
    provider_error: Option<&'a wire::AgentProviderError>,
    acknowledgement: Option<wire::MessageAcknowledgement>,
    automatic_speech: Option<&'a wire::AutomaticSpeech>,
    media: &'a [wire::Media],
    markdown_images: &'a [wire::MarkdownImageBinding],
}

fn card_kind(message: &AgentMessage) -> Option<MessageCardKind> {
    match message.activity {
        AgentActivityKind::Prose => Some(MessageCardKind::Prose),
        AgentActivityKind::Failure => Some(MessageCardKind::Failure),
        AgentActivityKind::Tool if !message.media.is_empty() => Some(MessageCardKind::ToolMedia),
        AgentActivityKind::Tool
        | AgentActivityKind::OutboxTransport
        | AgentActivityKind::Reasoning
        | AgentActivityKind::Turn => None,
        AgentActivityKind::Compaction => Some(MessageCardKind::Compaction),
        AgentActivityKind::Session => Some(MessageCardKind::Session),
        AgentActivityKind::SignOut => Some(MessageCardKind::SignOut),
        AgentActivityKind::Direction => Some(MessageCardKind::Direction),
    }
}

fn fingerprint(message: &AgentMessage) -> Option<MessageCardFingerprint<'_>> {
    Some(MessageCardFingerprint {
        kind: card_kind(message)?,
        author: message.author,
        authored: &message.authored,
        authored_synthetic: message.authored_synthetic,
        observed: message.observed.as_ref(),
        text: &message.text,
        compaction_method: message.compaction_method.as_deref(),
        final_answer: message.final_answer,
        prose_origin: message.prose_origin.as_ref(),
        provider_error: message.provider_error.as_ref(),
        acknowledgement: message.acknowledgement,
        automatic_speech: message.automatic_speech.as_ref(),
        media: &message.media,
        markdown_images: &message.markdown_images,
    })
}

#[derive(Clone, PartialEq, Eq, Default)]
pub(super) struct MessageCards {
    entries: Vec<Rc<AgentMessage>>,
    limit: Option<usize>,
    /// Operator sends whose delivered direction card this store has ever
    /// merged, by receiving lane and steering key.
    ///
    /// A delivered card may later leave `entries` under lane retention or a
    /// compaction horizon while its directive is still live; this memory is
    /// what keeps the source-less provisional card from returning in its
    /// place. Keys are never reused, so nothing here is ever forgotten.
    delivered: BTreeSet<(CanonicalUuid, String)>,
}

impl MessageCards {
    pub(super) fn entries(&self) -> &[Rc<AgentMessage>] {
        &self.entries
    }

    pub(super) const fn limit(&self) -> Option<usize> {
        self.limit
    }

    /// Whether a delivered direction card for `key` on `lane` was ever merged.
    pub(super) fn delivered_direction(&self, lane: CanonicalUuid, key: &str) -> bool {
        self.delivered.contains(&(lane, key.to_owned()))
    }

    fn remember_delivered(&mut self, message: &AgentMessage) {
        if message.activity != AgentActivityKind::Direction {
            return;
        }
        for context in &message.directive_contexts {
            let delivered = context
                .delivery
                .as_ref()
                .is_some_and(|delivery| !delivery.provisional);
            if let (true, wire::QuoteTarget::Steering { key, .. }) =
                (delivered, &context.quote_target)
            {
                self.delivered.insert((message.lane, key.clone()));
            }
        }
    }

    /// Replace the retained projection with one authoritative bounded snapshot.
    ///
    /// Equal source-and-content entries retain their allocation identity. Batch
    /// order does not become display order; every path uses the same chronology.
    pub(super) fn snapshot(&mut self, limit: usize, incoming: &[AgentMessage]) -> bool {
        let mut next = Vec::new();
        let mut seen = HashSet::new();
        for message in incoming {
            self.remember_delivered(message);
            let key = MessageCardKey::from(message);
            if fingerprint(message).is_none() || !seen.insert(key) {
                continue;
            }
            let retained = self
                .entries
                .iter()
                .find(|existing| {
                    MessageCardKey::from(existing.as_ref()) == key
                        && fingerprint(existing) == fingerprint(message)
                })
                .cloned()
                .unwrap_or_else(|| Rc::new(message.clone()));
            next.push(retained);
        }
        reconcile_prose(&mut next);
        next.sort_by_key(|message| message.chronology_key());
        trim_all_lanes(&mut next, limit);
        let changed = self.entries != next;
        self.entries = next;
        self.limit = Some(limit);
        changed
    }

    /// Merge a same-scope subscription head without discarding hydrated history.
    pub(super) fn reconnect_snapshot(&mut self, limit: usize, incoming: &[AgentMessage]) -> bool {
        let previous = self.entries.clone();
        self.limit = Some(limit);
        let _ = self.merge(incoming);
        trim_all_lanes(&mut self.entries, limit);
        self.entries != previous
    }

    /// Whether a retained-history interval can only extend the visible edge.
    ///
    /// Exact overlaps are harmless, as is either representation of a
    /// compaction whose other representation is retained. Every other new card
    /// must precede the oldest retained card in its lane so history cannot
    /// rewrite a measured middle epoch.
    pub(super) fn admits_history(&self, incoming: &[AgentMessage]) -> bool {
        let mut seen = HashSet::new();
        incoming.iter().all(|message| {
            let Some(next) = fingerprint(message) else {
                return true;
            };
            let key = MessageCardKey::from(message);
            if !seen.insert(key) {
                return false;
            }
            if self.entries.iter().any(|existing| {
                message.prose_superseded_by(existing) || existing.prose_superseded_by(message)
            }) {
                return true;
            }
            if let Some(existing) = self
                .entries
                .iter()
                .find(|existing| MessageCardKey::from(existing.as_ref()) == key)
            {
                return fingerprint(existing) == Some(next);
            }
            self.entries
                .iter()
                .filter(|existing| existing.lane == message.lane)
                .min_by_key(|existing| existing.chronology_key())
                .is_none_or(|oldest| message.chronology_key() < oldest.chronology_key())
        })
    }

    /// Drop hydrated cards below a resident-named replacement history edge.
    ///
    /// A source rebase can invalidate a deep cursor while preserving the live
    /// retained head. Cards below that head belong to the invalid interval and
    /// must leave before paging resumes from the replacement edge.
    pub(super) fn rebase_history(
        &mut self,
        lane: CanonicalUuid,
        before: Option<MessageLocus>,
    ) -> bool {
        let floor = before.and_then(|before| {
            self.entries
                .iter()
                .find(|message| message.lane == lane && message.locus == before)
                .map(|message| message.chronology_key())
        });
        let previous = self.entries.len();
        self.entries.retain(|message| {
            message.lane != lane || floor.is_some_and(|floor| message.chronology_key() >= floor)
        });
        self.entries.len() != previous
    }

    /// Merge live or paged records through the same identity and fingerprint law.
    pub(super) fn merge<'a>(
        &mut self,
        incoming: impl IntoIterator<Item = &'a AgentMessage>,
    ) -> bool {
        let previous = self.entries.clone();
        let mut mutated = false;
        let mut touched = BTreeSet::new();
        for message in incoming {
            self.remember_delivered(message);
            if self
                .entries
                .iter()
                .any(|existing| message.prose_superseded_by(existing))
            {
                continue;
            }
            touched.insert(message.lane);
            let key = MessageCardKey::from(message);
            let existing = self
                .entries
                .iter()
                .position(|entry| MessageCardKey::from(entry.as_ref()) == key);
            match (existing, fingerprint(message)) {
                (Some(index), Some(next)) if fingerprint(&self.entries[index]) == Some(next) => {}
                (Some(index), Some(_)) => {
                    self.entries[index] = Rc::new(message.clone());
                    mutated = true;
                }
                (Some(index), None) => {
                    self.entries.remove(index);
                    mutated = true;
                }
                (None, Some(_)) => {
                    self.entries.push(Rc::new(message.clone()));
                    mutated = true;
                }
                (None, None) => {}
            }
        }
        mutated |= reconcile_prose(&mut self.entries);
        if mutated {
            self.entries.sort_by_key(|message| message.chronology_key());
            if let Some(limit) = self.limit {
                for lane in touched {
                    trim_agent_message_lane(&mut self.entries, lane, limit);
                }
            }
        }
        // A batch can mutate transiently and reduce to the same projection.
        // Restore its original `Rc`s so a no-op also preserves allocations.
        if self.entries == previous {
            self.entries = previous;
            false
        } else {
            true
        }
    }
}

/// Completed streamed prose wins over its settled representation in either
/// arrival order, including a retained head merged with an older history page.
fn reconcile_prose(entries: &mut Vec<Rc<AgentMessage>>) -> bool {
    let superseded = wire::superseded_prose(entries);
    if superseded.is_empty() {
        return false;
    }
    for position in superseded.into_iter().rev() {
        entries.remove(position);
    }
    true
}

fn trim_all_lanes(messages: &mut Vec<Rc<AgentMessage>>, limit: usize) {
    let lanes = messages
        .iter()
        .map(|message| message.lane)
        .collect::<BTreeSet<_>>();
    for lane in lanes {
        trim_agent_message_lane(messages, lane, limit);
    }
}

pub(super) fn trim_agent_message_lane(
    messages: &mut Vec<Rc<AgentMessage>>,
    lane: CanonicalUuid,
    limit: usize,
) {
    let activities = messages
        .iter()
        .filter(|message| message.lane == lane)
        .map(|message| message.activity)
        .collect::<Vec<_>>();
    let mut discard = AgentActivityKind::retention_start(&activities, limit);
    messages.retain(|message| {
        if message.lane != lane || discard == 0 {
            true
        } else {
            discard -= 1;
            false
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(offset: u64, activity: AgentActivityKind) -> AgentMessage {
        AgentMessage {
            prose_origin: None,
            lane: "018f82d4-6f59-7d06-92e3-bce9131163e5"
                .parse()
                .expect("fixture lane"),
            author: "018f82d4-6f59-7d06-92e3-bce9131163ef"
                .parse()
                .expect("fixture author"),
            locus: MessageLocus {
                member: 1,
                offset,
                length: 1,
                event: 0,
            },
            authored: "2030-04-12T18:44:09.271Z".parse().expect("fixture instant"),
            authored_synthetic: false,
            observed: None,
            activity,
            compaction_method: None,
            final_answer: false,
            acknowledgement: None,
            text: offset.to_string(),
            provider_error: None,
            quote_target: None,
            directive_contexts: Vec::new(),
            sends: Vec::new(),
            quotes: Vec::new(),
            automatic_speech: None,
            media: Vec::new(),
            media_quotes: Vec::new(),
            markdown_images: Vec::new(),
            context_bytes: 0,
        }
    }

    #[test]
    fn turn_events_do_not_displace_final_answers_in_bounded_card_history() {
        let mut cards = MessageCards::default();
        let mut final_answer = message(1, AgentActivityKind::Prose);
        final_answer.final_answer = true;
        let turn = message(2, AgentActivityKind::Turn);
        let next = message(3, AgentActivityKind::Prose);
        let history = [final_answer, turn, next];

        cards.snapshot(2, &history);
        assert_eq!(
            cards
                .entries()
                .iter()
                .map(|entry| entry.locus.offset)
                .collect::<Vec<_>>(),
            [1, 3],
        );
        assert!(cards.entries()[0].final_answer);

        let mut live = MessageCards::default();
        live.snapshot(2, &history[..1]);
        live.merge(&history[1..]);
        assert_eq!(live.entries(), cards.entries());
    }

    #[test]
    fn live_terminal_revision_replaces_the_same_card_identity() {
        let mut cards = MessageCards::default();
        let ordinary = message(1, AgentActivityKind::Prose);
        cards.snapshot(10, std::slice::from_ref(&ordinary));

        let mut terminal = ordinary;
        terminal.final_answer = true;
        terminal.automatic_speech = Some(wire::AutomaticSpeech {
            kind: wire::AutomaticSpeechKind::Final,
            text: terminal.text.clone(),
        });
        assert!(cards.merge(std::iter::once(&terminal)));
        assert_eq!(cards.entries().len(), 1);
        assert!(cards.entries()[0].final_answer);
        assert_eq!(
            cards.entries()[0].automatic_speech,
            terminal.automatic_speech
        );
    }

    #[test]
    fn semantic_acknowledgement_revises_the_same_live_card_identity() {
        let mut cards = MessageCards::default();
        let mut pending = message(1, AgentActivityKind::Prose);
        pending.acknowledgement = Some(wire::MessageAcknowledgement::Pending);
        cards.snapshot(10, std::slice::from_ref(&pending));

        let mut acknowledged = pending.clone();
        acknowledged.acknowledgement = Some(wire::MessageAcknowledgement::Acknowledged);
        assert!(cards.merge(std::iter::once(&acknowledged)));
        assert_eq!(cards.entries().len(), 1);
        assert_eq!(
            cards.entries()[0].acknowledgement,
            acknowledged.acknowledgement
        );

        let mut declined = acknowledged;
        declined.acknowledgement = Some(wire::MessageAcknowledgement::Declined);
        assert!(cards.merge(std::iter::once(&declined)));
        assert_eq!(cards.entries().len(), 1);
        assert_eq!(cards.entries()[0].acknowledgement, declined.acknowledgement);
        assert_eq!(cards.entries()[0].text, pending.text);
        assert!(cards.entries()[0].automatic_speech.is_none());
    }

    /// No budget is small enough to drop what an agent just said.
    #[test]
    fn a_zero_card_budget_still_keeps_the_message_floor() {
        let mut cards = MessageCards::default();
        cards.snapshot(0, &[]);
        let prose = message(1, AgentActivityKind::Prose);

        assert!(cards.merge(std::iter::once(&prose)));
        assert!(
            !cards.merge(std::iter::once(&prose)),
            "replaying the same card changes nothing",
        );
        assert_eq!(cards.entries().len(), 1);
    }

    /// A loud peer spends its own floor, never a quiet agent's.
    #[test]
    fn a_busy_lane_does_not_evict_a_quiet_lanes_floor() {
        let quiet = "018f82d4-6f59-7d06-92e3-bce9131163e6"
            .parse::<CanonicalUuid>()
            .expect("quiet lane");
        let mut incoming = (0..5)
            .map(|offset| {
                let mut card = message(offset, AgentActivityKind::Prose);
                card.lane = quiet;
                card
            })
            .collect::<Vec<_>>();
        incoming.extend((100..300).map(|offset| message(offset, AgentActivityKind::Prose)));
        let mut cards = MessageCards::default();
        cards.snapshot(wire::MESSAGE_FLOOR, &incoming);

        assert_eq!(
            cards
                .entries()
                .iter()
                .filter(|card| card.lane == quiet)
                .count(),
            5,
            "a lane below the floor keeps everything it has said",
        );
        assert_eq!(
            cards
                .entries()
                .iter()
                .filter(|card| card.lane != quiet)
                .count(),
            wire::MESSAGE_FLOOR,
        );
    }

    /// The epoch a compaction closed is all this agent has said so far.
    #[test]
    fn a_fresh_compaction_does_not_evict_the_message_floor() {
        let mut incoming = (0..40)
            .map(|offset| message(offset, AgentActivityKind::Prose))
            .collect::<Vec<_>>();
        incoming.push(message(40, AgentActivityKind::Compaction));
        let mut cards = MessageCards::default();
        cards.snapshot(wire::MESSAGE_FLOOR + 1, &incoming);

        assert_eq!(
            cards
                .entries()
                .iter()
                .filter(|card| card.activity.is_prose())
                .count(),
            wire::MESSAGE_FLOOR,
        );
        assert_eq!(
            cards
                .entries()
                .iter()
                .filter(|card| card.activity == AgentActivityKind::Compaction)
                .count(),
            1,
            "the boundary is carried beside the floor, not instead of it",
        );
    }

    #[test]
    fn origin_metadata_refresh_allows_later_streamed_history_to_reconcile() {
        let old = message(2, AgentActivityKind::Prose);
        let mut settled = old.clone();
        settled.prose_origin = Some(wire::MessageProseOrigin {
            source: settled.lane,
            message: "provider-7".to_owned(),
            timestamp: 200,
            block: 1,
            streamed: false,
        });
        let mut streamed = settled.clone();
        streamed.locus.offset = 1;
        streamed.prose_origin.as_mut().expect("origin").streamed = true;
        let mut cards = MessageCards::default();
        cards.snapshot(10, &[old]);
        cards.merge([&settled]);
        cards.merge([&streamed]);
        assert_eq!(cards.entries(), &[Rc::new(streamed.clone())]);
        let mut terminal = streamed;
        terminal.final_answer = true;
        cards.merge([&terminal, &settled]);
        assert_eq!(cards.entries(), &[Rc::new(terminal)]);
    }

    // Principal direction fixture.
    fn direction(offset: u64, key: &str, provisional: bool) -> AgentMessage {
        let mut card = message(offset, AgentActivityKind::Direction);
        card.directive_contexts = vec![wire::MessageDirective {
            start: 0,
            end: u32::try_from(card.text.len()).expect("fixture length"),
            quote_target: wire::QuoteTarget::Steering {
                key: key.to_owned(),
                digest: wire::QuoteTarget::text_digest(&card.text),
            },
            media_quotes: Vec::new(),
            attribution: wire::QuoteAttribution {
                author: wire::QuoteAuthor::Directive {
                    actor: wire::Actor::TEST_PRINCIPAL,
                },
                recipients: Vec::new(),
                packet: false,
            },
            authored: card.authored,
            badge: None,
            review: None,
            line: None,
            delivery: Some(wire::MessageDelivery {
                fanout: None,
                provisional,
            }),
        }];
        card
    }

    #[test]
    fn a_delivered_direction_stays_remembered_after_retention_drops_its_card() {
        let lane = message(0, AgentActivityKind::Prose).lane;
        let mut cards = MessageCards::default();
        cards.snapshot(wire::MESSAGE_FLOOR, &[]);
        assert!(!cards.delivered_direction(lane, "1kJK6N3Rx"));

        cards.merge([&direction(1, "1kJK6N3Rx", false)]);
        assert!(cards.delivered_direction(lane, "1kJK6N3Rx"));

        let later = (2..wire::MESSAGE_FLOOR as u64 + 20)
            .map(|offset| message(offset, AgentActivityKind::Prose))
            .collect::<Vec<_>>();
        cards.merge(later.iter());
        assert!(
            !cards
                .entries()
                .iter()
                .any(|card| card.activity == AgentActivityKind::Direction),
            "retention trimmed the delivered card"
        );
        assert!(cards.delivered_direction(lane, "1kJK6N3Rx"));
    }

    #[test]
    fn a_restored_snapshot_remembers_its_delivered_directions_but_not_provisional_ones() {
        let lane = message(0, AgentActivityKind::Prose).lane;
        let mut cards = MessageCards::default();
        cards.snapshot(
            10,
            &[
                direction(1, "1kJK6N3Rx", false),
                direction(2, "1kJK6M0Vx", true),
            ],
        );
        assert!(cards.delivered_direction(lane, "1kJK6N3Rx"));
        assert!(
            !cards.delivered_direction(lane, "1kJK6M0Vx"),
            "a provisional card is not delivery"
        );
        let other: CanonicalUuid = "018f82d4-6f59-7d06-92e3-bce9131163aa"
            .parse()
            .expect("fixture lane");
        assert!(!cards.delivered_direction(other, "1kJK6N3Rx"));
    }
}
