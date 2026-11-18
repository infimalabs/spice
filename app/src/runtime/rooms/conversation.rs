//! Room conversation authority: each substantive publication and
//! actor-addressed input share one lifecycle projection.
//!
//! A room snapshot carries only the facts behind current heads, so every move
//! sets the state its fact names instead of replaying a guarded transition.

use std::collections::{BTreeMap, BTreeSet};

use wire::body::{
    PeerLineAction, PeerLineRef, SteerAccepted, SteerAcked, SteerCanceled, SteerDeclined,
    SteerLapsed, SteerLineTransition, SteerMessaged, SteerQueued, SteerRecovered, SteerSent,
};
use wire::rooms::{PrincipalInputRef, RoomAnchor, RoomAuthorityStamp, RoomPublication};
use wire::steering::SteerDeadLettered;
use wire::{Actor, CanonicalUuid, Disposition, FactKind, Instant, Seq, StreamFact};

use super::RoomPositioned;

/// One admitted publication with the directives its write opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversation {
    /// Journal position of the admitting fact.
    pub position: Seq,
    /// Author, verb, source, and frozen routes.
    pub publication: RoomPublication,
    /// Directives delivering its lane routes, or an answer's lane return.
    pub directed: Vec<CanonicalUuid>,
    /// Principal-composed text; a lane's bytes stay in its transcript.
    pub text: Option<String>,
}

/// One principal-addressed input and its optional root publication source.
#[derive(Clone, Debug)]
pub struct PrincipalInboxItem<'a> {
    /// Exact input identity accepted by line-control commands.
    pub input: PrincipalInputRef,
    /// Current actor-addressed steering standing.
    pub delivery: &'a RoomPositioned<Delivery>,
    /// Source publication for a root route or the recursive input's line root.
    pub conversation: Option<&'a Conversation>,
}

/// One actor-addressed input's native steering standing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    /// Journal position that created this input; lifecycle moves never reorder it.
    pub created: Seq,
    /// Steering key the recipient acknowledges.
    pub key: String,
    /// Exact actor that owes the response.
    pub recipient: Actor,
    /// Actor that authored the input.
    pub sender: Actor,
    /// Queued for a vacant lane, unheard by any seat.
    pub queued: bool,
    /// Current steering disposition.
    pub disposition: Disposition,
    /// Text the delivery carried.
    pub text: String,
    /// Journal instant that committed the creating delivery.
    pub authored: Instant,
    /// Current expiry, advanced by recovery.
    pub expires: Instant,
    /// Latest transition that covered this input or ended its line.
    pub line_action: Option<PeerLineAction>,
    /// New input carrying the transition's substantive body, when admitted.
    pub line_body: Option<CanonicalUuid>,
    /// Conversation-line identity carried by the input, when any.
    pub line: Option<PeerLineRef>,
    /// Principal submit action shared by every target-specific direction.
    ///
    /// Present only for a principal direction; peer and resident traffic omit
    /// it.
    pub principal_send: Option<CanonicalUuid>,
    /// Bounded image payloads the principal submitted with this direction.
    ///
    /// These remain only until the transcript-backed card lands.
    pub attachments: Vec<wire::Media>,
    /// Task whose room the direction speaks in, when it has one.
    pub room: Option<RoomAnchor>,
}

/// Capability-local conversation heads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConversationProjection {
    publications: BTreeMap<RoomAuthorityStamp, Conversation>,
    publication_inputs: BTreeMap<CanonicalUuid, RoomAuthorityStamp>,
    deliveries: BTreeMap<CanonicalUuid, RoomPositioned<Delivery>>,
    keys: BTreeMap<String, CanonicalUuid>,
    /// Directives some room record or conversation names; a directive opens
    /// in the write that names it, so an unnamed one never joins later.
    named: BTreeSet<CanonicalUuid>,
}

#[cfg(any(test, target_arch = "wasm32"))]
/// Exact authored packet body whose source range a room publication names.
///
/// Presented card offsets intentionally do not participate: protocol and
/// acknowledgement presentation rewrites those offsets.
pub(crate) fn room_message_body(
    sends: &[wire::MessageSend],
    source: wire::rooms::TextRange,
) -> Option<&str> {
    sends
        .iter()
        .find(|send| send.source == source)
        .map(|send| send.body.as_str())
}

impl ConversationProjection {
    /// One admitted publication.
    pub fn conversation(&self, publication: &RoomAuthorityStamp) -> Option<&Conversation> {
        self.publications.get(publication)
    }

    /// Principal inputs still actionable by `principal`, in input admission
    /// order. A lapsed input remains actionable because it can recover.
    pub fn principal_inbox(&self, principal: CanonicalUuid) -> Vec<PrincipalInboxItem<'_>> {
        let mut open = self
            .deliveries
            .iter()
            .filter(|(_, delivery)| {
                delivery.value.recipient == Actor::Principal(principal)
                    && matches!(
                        delivery.value.disposition,
                        Disposition::Live | Disposition::Lapsed
                    )
            })
            .map(|(directive, delivery)| {
                let source = self.publication_inputs.get(directive).or_else(|| {
                    delivery
                        .value
                        .line
                        .as_ref()
                        .and_then(|line| self.publication_inputs.get(&line.root))
                });
                PrincipalInboxItem {
                    input: PrincipalInputRef {
                        directive: *directive,
                        principal,
                    },
                    delivery,
                    conversation: source.and_then(|source| self.publications.get(source)),
                }
            })
            .collect::<Vec<_>>();
        open.sort_by_key(|entry| entry.delivery.value.created);
        open
    }

    /// One named actor-addressed input.
    pub fn delivery(&self, directive: &CanonicalUuid) -> Option<&RoomPositioned<Delivery>> {
        self.deliveries.get(directive)
    }

    /// One named actor-addressed input by the steering key it carries.
    pub fn keyed_delivery(&self, key: &str) -> Option<&RoomPositioned<Delivery>> {
        self.deliveries.get(self.keys.get(key)?)
    }

    /// Live principal sends whose receiving transcript has not yet supplied
    /// their delivery source. Rendering projects them at their final card identity.
    pub fn provisional_directions(&self) -> impl Iterator<Item = &RoomPositioned<Delivery>> {
        self.deliveries.values().filter(|delivery| {
            delivery.value.principal_send.is_some()
                && delivery.value.disposition == Disposition::Live
        })
    }

    /// Drop provisional image payloads after their transcript-backed card has
    /// landed. Returns whether any retained bytes were released.
    pub(super) fn release_delivered_media(
        &mut self,
        delivered: impl Fn(CanonicalUuid, &str) -> bool,
    ) -> bool {
        let mut changed = false;
        for delivery in self.deliveries.values_mut() {
            let Some(lane) = delivery.value.recipient.lane() else {
                continue;
            };
            if !delivery.value.attachments.is_empty() && delivered(lane, &delivery.value.key) {
                delivery.value.attachments.clear();
                changed = true;
            }
        }
        changed
    }

    /// Mark directives a room record names as deliveries to keep.
    pub(super) fn name(&mut self, directed: &[CanonicalUuid]) {
        self.named.extend(directed.iter().copied());
    }

    /// Admit one publication and index each deterministic root-route input.
    pub(super) fn admit(
        &mut self,
        position: Seq,
        _at: Instant,
        publication: RoomPublication,
        directed: Vec<CanonicalUuid>,
        _expires: Instant,
        text: Option<String>,
    ) -> bool {
        let stamp = publication.publication().clone();
        if self.publications.contains_key(&stamp) {
            return false;
        }
        self.name(&directed);
        for (principal, _) in publication.principal_routes() {
            let Some(input) = publication.principal_input(principal) else {
                continue;
            };
            self.publication_inputs
                .insert(input.directive, stamp.clone());
            self.name(&[input.directive]);
        }
        self.publications.insert(
            stamp,
            Conversation {
                position,
                publication,
                directed,
                text,
            },
        );
        true
    }

    /// Fold one actor-addressed delivery opening or lifecycle move.
    pub(super) fn apply_delivery(&mut self, fact: &StreamFact) -> bool {
        let position = fact.seq();
        match fact.kind() {
            FactKind::SteerSent => fact
                .read::<SteerSent>()
                .is_some_and(|body| self.deliver_sent(position, *fact.at(), body, false)),
            FactKind::SteerMessaged => fact
                .read::<SteerMessaged>()
                .is_some_and(|body| self.deliver_sent(position, *fact.at(), body.as_sent(), false)),
            FactKind::SteerQueued => fact
                .read::<SteerQueued>()
                .is_some_and(|body| self.deliver_sent(position, *fact.at(), body.as_sent(), true)),
            FactKind::SteerAcked => fact
                .read::<SteerAcked>()
                .is_some_and(|body| self.dispose(position, &body.key, Disposition::Acked)),
            FactKind::SteerAccepted => fact
                .read::<SteerAccepted>()
                .is_some_and(|body| self.dispose(position, &body.key, Disposition::Acked)),
            FactKind::SteerDeclined => fact
                .read::<SteerDeclined>()
                .is_some_and(|body| self.dispose(position, &body.key, Disposition::Declined)),
            FactKind::SteerLapsed => fact
                .read::<SteerLapsed>()
                .is_some_and(|body| self.dispose(position, &body.key, Disposition::Lapsed)),
            FactKind::SteerDeadLettered => fact
                .read::<SteerDeadLettered>()
                .is_some_and(|body| self.dispose(position, &body.key, Disposition::DeadLettered)),
            FactKind::SteerCanceled => fact
                .read::<SteerCanceled>()
                .is_some_and(|body| self.dispose(position, &body.key, Disposition::Canceled)),
            FactKind::SteerRecovered => fact
                .read::<SteerRecovered>()
                .is_some_and(|body| self.recover(position, &body.key, body.expires)),
            FactKind::SteerLineTransition => fact
                .read::<SteerLineTransition>()
                .is_some_and(|body| self.transition(position, body)),
            _ => false,
        }
    }

    fn deliver_sent(
        &mut self,
        position: Seq,
        authored: Instant,
        body: SteerSent,
        queued: bool,
    ) -> bool {
        let principal_send = (matches!(body.sender, Actor::Principal(_))
            && matches!(body.recipient, Actor::Lane(_)))
        .then_some(body.fanout)
        .flatten();
        let attachments = if principal_send.is_some() {
            body.attachments
        } else {
            Vec::new()
        };
        let line = body
            .control
            .as_deref()
            .and_then(wire::parse_peer_line_control)
            .map(|(line, _, _)| line);
        let directive = body.directive;
        let principal_input = matches!(body.recipient, Actor::Principal(_));
        let mut changed = self.deliver(
            position,
            directive,
            Delivery {
                created: position,
                key: body.key,
                recipient: body.recipient,
                sender: body.sender,
                queued,
                disposition: Disposition::Live,
                text: body.text,
                authored,
                expires: body.expires,
                line_action: None,
                line_body: None,
                line,
                principal_send,
                attachments,
                room: body.room,
            },
        );
        if principal_input {
            changed |= self.named.insert(directive);
        }
        changed
    }

    fn transition(&mut self, position: Seq, transition: SteerLineTransition) -> bool {
        self.deliveries
            .get_mut(&transition.input)
            .is_some_and(|delivery| {
                let changed = delivery.value.line_action != Some(transition.action)
                    || delivery.value.line_body != transition.body;
                delivery.value.line_action = Some(transition.action);
                delivery.value.line_body = transition.body;
                delivery.position = position;
                changed
            })
    }

    fn deliver(&mut self, position: Seq, directive: CanonicalUuid, value: Delivery) -> bool {
        let key_changed = self.keys.insert(value.key.clone(), directive) != Some(directive);
        let delivery = RoomPositioned { position, value };
        let delivery_changed = self.deliveries.get(&directive) != Some(&delivery);
        self.deliveries.insert(directive, delivery);
        key_changed || delivery_changed
    }

    /// Move one delivery; an unnamed delivery that retires is dropped, since
    /// no later record can name it.
    fn dispose(&mut self, position: Seq, key: &str, disposition: Disposition) -> bool {
        let Some(directive) = self.keys.get(key).copied() else {
            return false;
        };
        if disposition != Disposition::Live && !self.named.contains(&directive) {
            let key_removed = self.keys.remove(key).is_some();
            let delivery_removed = self.deliveries.remove(&directive).is_some();
            return key_removed || delivery_removed;
        }
        self.deliveries.get_mut(&directive).is_some_and(|delivery| {
            let changed = delivery.value.disposition != disposition;
            delivery.value.disposition = disposition;
            delivery.position = position;
            changed
        })
    }

    fn recover(&mut self, position: Seq, key: &str, expires: Instant) -> bool {
        let Some(directive) = self.keys.get(key).copied() else {
            return false;
        };
        self.deliveries.get_mut(&directive).is_some_and(|delivery| {
            let changed = delivery.value.disposition != Disposition::Live
                || delivery.value.expires != expires
                || delivery.value.line_action.is_some()
                || delivery.value.line_body.is_some();
            delivery.value.disposition = Disposition::Live;
            delivery.value.expires = expires;
            delivery.value.line_action = None;
            delivery.value.line_body = None;
            delivery.position = position;
            changed
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(last: u8) -> CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131163{last:02x}")
            .parse()
            .expect("fixture uuid")
    }
    fn send(start: u64, body: &str, presented: (u32, u32)) -> wire::MessageSend {
        let id = uuid(0xf1);
        wire::MessageSend {
            start: presented.0,
            end: presented.1,
            from: id,
            sender: id,
            recipients: Vec::new(),
            lanes: Vec::new(),
            operator: true,
            contexts: Vec::new(),
            source: wire::rooms::TextRange {
                start,
                end: start + u64::try_from(body.len()).expect("fixture body length fits"),
            },
            body: body.to_owned(),
        }
    }

    #[test]
    fn room_message_body_uses_source_identity_after_presentation_rewrites_offsets() {
        let sends = [
            send(41, "First packet.", (2, 17)),
            send(84, "Steering. Directive stays whole.", (21, 57)),
        ];

        assert_eq!(
            room_message_body(
                &sends,
                wire::rooms::TextRange {
                    start: 84,
                    end: 116,
                }
            ),
            Some("Steering. Directive stays whole.")
        );
        assert_eq!(
            room_message_body(&sends, wire::rooms::TextRange { start: 0, end: 4 }),
            None
        );
    }

    fn instant(second: u8) -> Instant {
        format!("2030-04-12T18:44:{second:02}.000Z")
            .parse()
            .expect("fixture instant")
    }

    // Principal-authored fact fixture.
    fn stream<B: wire::Body>(position: u64, at: Instant, body: B) -> StreamFact {
        let opaque =
            wire::decode(&wire::encode(&wire::Fact { body }).expect("encode fixture fact"))
                .expect("decode opaque fixture fact");
        StreamFact::from(wire::Envelope {
            v: wire::WireVersion::CURRENT,
            seq: Seq::new(position).expect("positive fixture position"),
            at,
            actor: wire::Actor::TEST_PRINCIPAL,
            origin: "fixture:provisional-direction"
                .parse()
                .expect("fixture origin"),
            cause: None,
            fact: opaque,
        })
    }

    fn attachment() -> wire::Media {
        wire::Media {
            name: "submitted.png".to_owned(),
            content_type: "image/png".to_owned(),
            data_url: "data:image/png;base64,iVBORw0KGgo=".to_owned(),
        }
    }

    fn sent(key: &str, fanout: Option<CanonicalUuid>, from: Option<CanonicalUuid>) -> SteerSent {
        SteerSent {
            directive: uuid(key.as_bytes()[key.len() - 1] % 16),
            fanout,
            recipients: Vec::new(),
            key: key.to_owned(),
            recipient: Actor::Lane(uuid(0xa1)),
            text: format!("Direction {key}"),
            sender: from.map_or(Actor::TEST_PRINCIPAL, Actor::Lane),
            room: None,
            control: None,
            attachments: vec![attachment()],
            expires: instant(59),
        }
    }

    #[test]
    fn only_a_live_principal_send_projects_as_a_provisional_direction() {
        let mut projection = ConversationProjection::default();
        let principal = stream(1, instant(1), sent("dW4Wl4fRP", Some(uuid(0xf1)), None));
        let peer = stream(2, instant(2), sent("dW4Wl4fRQ", None, Some(uuid(0xb1))));
        let background = stream(3, instant(3), sent("dW4Wl4fRR", None, None));

        assert!(projection.apply_delivery(&principal));
        assert!(projection.apply_delivery(&peer));
        assert!(projection.apply_delivery(&background));

        {
            let directions = projection.provisional_directions().collect::<Vec<_>>();
            let [direction] = directions.as_slice() else {
                panic!("only the principal send projects: {directions:?}");
            };
            assert_eq!(direction.value.key, "dW4Wl4fRP");
            assert_eq!(direction.value.authored, instant(1));
            assert_eq!(direction.value.principal_send, Some(uuid(0xf1)));
            assert_eq!(direction.value.attachments, vec![attachment()]);
            let card = crate::runtime::provisional_direction(direction, uuid(0xb1), Vec::new());
            // The delivered card names the fact's author and the receiving lane
            // as a whole; matching it keeps the body's raw nodes, and so its
            // images, across delivery.
            assert_eq!(
                card.directive_contexts[0].attribution,
                wire::QuoteAttribution {
                    author: wire::QuoteAuthor::Directive {
                        actor: wire::Actor::TEST_PRINCIPAL,
                    },
                    recipients: vec![wire::QuoteRecipient {
                        lane: uuid(0xa1),
                        occupant: None,
                    }],
                    packet: false,
                }
            );
            assert_eq!(card.media, vec![attachment()]);
            let expected_quote = wire::MediaQuote {
                target: wire::QuoteTarget::SteeringMedia {
                    key: "dW4Wl4fRP".to_owned(),
                    ordinal: 0,
                    digest: attachment().identity().expect("fixture media identity"),
                },
                name: "submitted.png".to_owned(),
            };
            assert_eq!(card.media_quotes, vec![expected_quote.clone()]);
            assert_eq!(
                card.directive_contexts[0].media_quotes,
                vec![expected_quote]
            );
        }

        assert!(
            projection
                .release_delivered_media(|lane, key| { lane == uuid(0xa1) && key == "dW4Wl4fRP" })
        );
        let direction = projection
            .provisional_directions()
            .next()
            .expect("live delivery remains provisional");
        assert!(direction.value.attachments.is_empty());
        assert!(!projection.release_delivered_media(|_, _| true));
        assert!(projection.apply_delivery(&stream(
            4,
            instant(4),
            SteerCanceled {
                key: "dW4Wl4fRP".to_owned(),
                recipient: Actor::Lane(uuid(0xa1)),
                reason: None,
            },
        )));
        assert!(projection.provisional_directions().next().is_none());
    }

    #[test]
    fn an_invitation_delivery_keeps_its_room_and_acceptance_retires_it() {
        let mut projection = ConversationProjection::default();
        let room: wire::TaskStamp = "1kKRRsDPx".parse().expect("fixture room");
        let mut invitation = sent("dW4Wl4fRS", None, Some(uuid(0xb1)));
        invitation.room = Some(room.clone());
        let directive = invitation.directive;
        assert!(projection.apply_delivery(&stream(1, instant(1), invitation)));
        let delivery = projection
            .keyed_delivery("dW4Wl4fRS")
            .expect("the invitation is a keyed delivery");
        assert_eq!(delivery.value.room, Some(room));
        assert_eq!(delivery.value.disposition, Disposition::Live);

        assert!(projection.apply_delivery(&stream(
            2,
            instant(2),
            SteerAccepted {
                directive,
                key: "dW4Wl4fRS".to_owned(),
                lane: uuid(0xa1),
            },
        )));
        assert!(
            projection.keyed_delivery("dW4Wl4fRS").is_none(),
            "an accepted invitation no record names retires like an acknowledged one"
        );
    }
}
