//! Reducing a journal's steering facts into standing.
//!
//! The ledger itself is pure and compiles everywhere; this arm reads the
//! journal's envelopes, folds each steering fact in order, and projects the
//! bounded steering index the resident serves. It exists only where a journal
//! does, so the wasm build never sees it.

use std::collections::BTreeMap;

use wire::body::{
    SteerAccepted, SteerAcked, SteerCanceled, SteerDeclined, SteerEscalated, SteerLapsed,
    SteerMessaged, SteerMissed, SteerQueued, SteerRecovered, SteerReplied, SteerSent,
    TasteDecision, TasteJudged, TasteReminded,
};
use wire::steering::SteerDeadLettered;
use wire::{CanonicalUuid, Instant};

use super::{Directive, Ledger, Refusal, ReplayError, Steering};
use crate::ack::keys::Key;

/// One canonical replay projected into the bounded journal steering index.
#[derive(Debug)]
pub struct AuthorityIndex {
    heads: Vec<journal::JournalHead<wire::SteeringHead>>,
    misses: Vec<journal::JournalHead<SteerMissed>>,
}

impl AuthorityIndex {
    /// Current directive heads in immutable publication order.
    #[must_use]
    pub fn heads(&self) -> &[journal::JournalHead<wire::SteeringHead>] {
        &self.heads
    }

    /// Canonical missed-recognition evidence indexed during replay.
    #[must_use]
    pub fn misses(&self) -> &[journal::JournalHead<SteerMissed>] {
        &self.misses
    }
}

struct AuthorityPosition {
    origin: wire::Origin,
    created: wire::Seq,
    head: wire::Seq,
}

type AuthorityReplay = (
    Ledger,
    BTreeMap<CanonicalUuid, AuthorityPosition>,
    Vec<journal::JournalHead<SteerMissed>>,
);

impl Directive {
    /// Project this directive as the head the steering index publishes.
    fn to_head(&self, origin: wire::Origin, created: wire::Seq) -> wire::SteeringHead {
        wire::SteeringHead {
            directive: self.directive,
            fanout: self.fanout,
            recipients: self.recipients.clone(),
            key: self.key.to_string(),
            recipient: self.recipient.clone(),
            text: self.text.clone(),
            sender: self.sender.clone(),
            room: self.room.clone(),
            control: self.control.clone(),
            attachments: self.attachments.clone(),
            published: self
                .published
                .expect("an authoritative directive has a publication instant"),
            expires: self.expires,
            stage: self.stage,
            escalations: self.escalations.clone(),
            disposition: self.disposition,
            origin,
            created,
            minted_micros: self.key.minted_micros(),
        }
    }
}

impl Ledger {
    /// Rebuild standing from one canonical journal snapshot.
    ///
    /// # Errors
    ///
    /// Returns the exact fact position whose steering transition contradicts
    /// the durable stream. A body cannot be the reason: the snapshot holds only
    /// bodies its registered DTO already accepted.
    pub fn from_snapshot(snapshot: &journal::FactSnapshot) -> Result<Self, ReplayError> {
        Self::from_facts(snapshot.facts())
    }

    /// Rebuild standing from one canonical ordered fact iterator.
    pub fn from_facts<'a>(
        facts: impl IntoIterator<Item = &'a wire::Envelope<wire::AnyFact>>,
    ) -> Result<Self, ReplayError> {
        replay(facts).map(|(ledger, _, _)| ledger)
    }

    /// Advance standing from one ordered suffix of authoritative facts.
    ///
    /// Non-steering facts are ignored. This is the incremental counterpart to
    /// [`Self::from_facts`]: a resident can retain one ledger and apply only
    /// facts newer than its admitted cursor. A refused fact rolls back every
    /// steering transition earlier in the same suffix.
    pub fn advance<'a>(
        &mut self,
        facts: impl IntoIterator<Item = &'a wire::Envelope<wire::AnyFact>>,
    ) -> Result<(), ReplayError> {
        let order_len = self.order.len();
        let mut originals = BTreeMap::<Key, Option<Directive>>::new();
        for envelope in facts {
            if let Some(key) = affected_key(envelope) {
                originals
                    .entry(key.clone())
                    .or_insert_with(|| self.directives.get(&key).cloned());
            }
            if let Err(error) = self.apply_envelope(envelope) {
                self.order.truncate(order_len);
                for (key, original) in originals {
                    match original {
                        Some(directive) => {
                            self.directives.insert(key, directive);
                        }
                        None => {
                            if let Some(opened) = self.directives.remove(&key) {
                                self.identities.remove(&opened.directive);
                            }
                        }
                    }
                }
                return Err(error);
            }
        }
        Ok(())
    }

    fn apply_envelope(
        &mut self,
        envelope: &wire::Envelope<wire::AnyFact>,
    ) -> Result<Option<AppliedSteering>, ReplayError> {
        let folded = match self.open_envelope(envelope) {
            Some(opened) => opened,
            None => match self.transition_envelope(envelope) {
                Some(transitioned) => transitioned,
                None if envelope.fact.kind().affects_steering() => {
                    return Err(unhandled_authority_fact(envelope));
                }
                None => return Ok(None),
            },
        };
        let (result, applied) = folded;
        result.map_err(|error| ReplayError {
            seq: envelope.seq,
            kind: envelope.fact.kind(),
            detail: error.to_string(),
        })?;
        Ok(Some(applied))
    }

    /// Fold one fact that opens a directive, when it is one.
    fn open_envelope(
        &mut self,
        envelope: &wire::Envelope<wire::AnyFact>,
    ) -> Option<(Result<(), Refusal>, AppliedSteering)> {
        let fact = &envelope.fact;
        let at = Some(envelope.at);
        let sent = fact
            .read::<SteerSent>()
            .or_else(|| fact.read::<SteerReplied>().map(|body| body.as_sent()))
            .or_else(|| fact.read::<SteerMessaged>().map(|body| body.as_sent()))
            .or_else(|| fact.read::<SteerQueued>().map(|body| body.as_sent()));
        let (result, affected) = if let Some(body) = sent {
            let opened = Some(body.directive);
            (self.absorb_recorded(Steering::Sent(&body), at), opened)
        } else if let Some(body) = fact.read::<TasteReminded>() {
            let opened = Some(body.directive);
            (self.absorb_recorded(Steering::Reminded(&body), at), opened)
        } else {
            let body = fact.read::<TasteJudged>()?;
            let opened = match &body.decision {
                TasteDecision::Confirmed { directive, .. } => Some(*directive),
                TasteDecision::Cleared => None,
            };
            (self.absorb_recorded(Steering::Judged(&body), at), opened)
        };
        let applied = AppliedSteering {
            affected,
            created: affected.is_some(),
            missed: None,
        };
        Some((result, applied))
    }

    /// Fold one fact that moves an existing directive, when it is one.
    fn transition_envelope(
        &mut self,
        envelope: &wire::Envelope<wire::AnyFact>,
    ) -> Option<(Result<(), Refusal>, AppliedSteering)> {
        let fact = &envelope.fact;
        let at = Some(envelope.at);
        let mut missed = None;
        let (result, affected) = if let Some(body) = fact.read::<SteerEscalated>() {
            let moved = Some(body.directive);
            (self.absorb_recorded(Steering::Escalated(&body), at), moved)
        } else if let Some(body) = fact.read::<SteerRecovered>() {
            let moved = Some(body.directive);
            (self.absorb_recorded(Steering::Recovered(&body), at), moved)
        } else if let Some(body) = fact.read::<SteerAcked>() {
            let moved = Some(body.directive);
            (self.absorb_recorded(Steering::Acked(&body), at), moved)
        } else if let Some(body) = fact.read::<SteerAccepted>() {
            let moved = Some(body.directive);
            (self.absorb_recorded(Steering::Accepted(&body), at), moved)
        } else if let Some(body) = fact.read::<SteerDeclined>() {
            let moved = Some(body.directive);
            (self.absorb_recorded(Steering::Declined(&body), at), moved)
        } else if let Some(body) = fact.read::<SteerMissed>() {
            let result = self.absorb_recorded(Steering::Missed(&body), at);
            missed = Some(body);
            (result, None)
        } else if let Some(body) = fact.read::<SteerLapsed>() {
            let moved = self.keyed_directive(&body.key);
            (self.absorb_recorded(Steering::Lapsed(&body), at), moved)
        } else if let Some(body) = fact.read::<SteerDeadLettered>() {
            let moved = Some(body.directive);
            (
                self.absorb_recorded(Steering::DeadLettered(&body), at),
                moved,
            )
        } else {
            let body = fact.read::<SteerCanceled>()?;
            let moved = self.keyed_directive(&body.key);
            (self.absorb_recorded(Steering::Canceled(&body), at), moved)
        };
        let applied = AppliedSteering {
            affected,
            created: false,
            missed,
        };
        Some((result, applied))
    }

    /// The stable identity of the directive a key-addressed fact names.
    fn keyed_directive(&self, key: &str) -> Option<CanonicalUuid> {
        let key = key.parse::<Key>().ok()?;
        self.directives
            .get(&key)
            .map(|directive| directive.directive)
    }

    /// Rebuild the singular bounded steering authority from immutable facts.
    ///
    /// # Errors
    ///
    /// Returns the same exact contradictory fact refusal as [`Self::from_snapshot`].
    pub fn authority_index(
        snapshot: &journal::FactSnapshot,
    ) -> Result<AuthorityIndex, ReplayError> {
        Self::authority_index_from_facts(snapshot.facts())
    }

    /// Rebuild the singular bounded steering authority from selected facts.
    ///
    /// Facts retain their global journal positions but need not include facts
    /// that cannot affect steering authority.
    pub fn authority_index_from_facts<'a>(
        facts: impl IntoIterator<Item = &'a wire::Envelope<wire::AnyFact>>,
    ) -> Result<AuthorityIndex, ReplayError> {
        let (ledger, positions, misses) = replay(facts)?;
        let heads = ledger
            .order
            .iter()
            .filter_map(|key| ledger.directives.get(key))
            .map(|directive| {
                let position = positions
                    .get(&directive.directive)
                    .expect("every replayed directive has authority positions");
                journal::JournalHead::new(
                    position.head,
                    directive.to_head(position.origin.clone(), position.created),
                )
            })
            .collect();
        Ok(AuthorityIndex { heads, misses })
    }

    /// Build one new operator directive head without consulting other history.
    pub fn sent_head(
        body: &SteerSent,
        origin: wire::Origin,
        published: Instant,
        created: wire::Seq,
    ) -> Result<wire::SteeringHead, Refusal> {
        let mut ledger = Self::new();
        ledger.absorb_recorded(Steering::Sent(body), Some(published))?;
        Ok(ledger
            .directive(body.directive)
            .expect("sent fact created its directive")
            .to_head(origin, created))
    }

    /// Build one new peer-reply directive head without unrelated history.
    pub fn replied_head(
        body: &SteerReplied,
        origin: wire::Origin,
        published: Instant,
        created: wire::Seq,
    ) -> Result<wire::SteeringHead, Refusal> {
        Self::sent_head(&body.as_sent(), origin, published, created)
    }

    /// Build one new taste-reminder directive head without unrelated history.
    pub fn reminded_head(
        body: &TasteReminded,
        origin: wire::Origin,
        published: Instant,
        created: wire::Seq,
    ) -> Result<wire::SteeringHead, Refusal> {
        let mut ledger = Self::new();
        ledger.absorb_recorded(Steering::Reminded(body), Some(published))?;
        Ok(ledger
            .directive(body.directive)
            .expect("reminder fact created its directive")
            .to_head(origin, created))
    }

    /// Build one judge-confirmed taste directive without unrelated history.
    pub fn judged_head(
        body: &TasteJudged,
        origin: wire::Origin,
        published: Instant,
        created: wire::Seq,
    ) -> Result<wire::SteeringHead, Refusal> {
        let TasteDecision::Confirmed { directive, .. } = &body.decision else {
            panic!("a cleared taste judgment does not create a steering head");
        };
        let mut ledger = Self::new();
        ledger.absorb_recorded(Steering::Judged(body), Some(published))?;
        Ok(ledger
            .directive(*directive)
            .expect("confirmed judgment created its directive")
            .to_head(origin, created))
    }

    /// Apply one transition to one indexed head without replaying other facts.
    pub fn transition_head(
        current: &wire::SteeringHead,
        transition: Steering<'_>,
        at: Instant,
    ) -> Result<wire::SteeringHead, Refusal> {
        let mut ledger = Self::new();
        ledger.place(Directive::from_head(current)?);
        ledger.absorb_recorded(transition, Some(at))?;
        Ok(ledger
            .directive(current.directive)
            .expect("transition retains the stable directive")
            .to_head(current.origin.clone(), current.created))
    }
}

fn unhandled_authority_fact(envelope: &wire::Envelope<wire::AnyFact>) -> ReplayError {
    ReplayError {
        seq: envelope.seq,
        kind: envelope.fact.kind(),
        detail: "fact kind is classified as steering authority but has no reducer arm".to_owned(),
    }
}

/// Stable key whose steering head this fact can change.
///
/// This is the reducer's exhaustive authority classification; consumers use it
/// instead of maintaining a second fact-kind inventory.
#[must_use]
pub fn affected_key(envelope: &wire::Envelope<wire::AnyFact>) -> Option<Key> {
    let fact = &envelope.fact;
    match fact.kind() {
        wire::FactKind::SteerSent => fact
            .read::<SteerSent>()
            .and_then(|body| body.key.parse().ok()),
        wire::FactKind::SteerReplied => fact
            .read::<SteerReplied>()
            .and_then(|body| body.key.parse().ok()),
        wire::FactKind::SteerMessaged => fact
            .read::<SteerMessaged>()
            .and_then(|body| body.key.parse().ok()),
        wire::FactKind::SteerQueued => fact
            .read::<SteerQueued>()
            .and_then(|body| body.key.parse().ok()),
        wire::FactKind::TasteReminded => fact
            .read::<TasteReminded>()
            .and_then(|body| body.key.parse().ok()),
        wire::FactKind::TasteJudged => fact.read::<TasteJudged>().and_then(|body| {
            let TasteDecision::Confirmed { key, .. } = &body.decision else {
                return None;
            };
            key.parse().ok()
        }),
        wire::FactKind::SteerEscalated => fact
            .read::<SteerEscalated>()
            .and_then(|body| body.key.parse().ok()),
        wire::FactKind::SteerRecovered => fact
            .read::<SteerRecovered>()
            .and_then(|body| body.key.parse().ok()),
        wire::FactKind::SteerAcked => fact
            .read::<SteerAcked>()
            .and_then(|body| body.key.parse().ok()),
        wire::FactKind::SteerAccepted => fact
            .read::<SteerAccepted>()
            .and_then(|body| body.key.parse().ok()),
        wire::FactKind::SteerDeclined => fact
            .read::<SteerDeclined>()
            .and_then(|body| body.key.parse().ok()),
        wire::FactKind::SteerLapsed => fact
            .read::<SteerLapsed>()
            .and_then(|body| body.key.parse().ok()),
        wire::FactKind::SteerDeadLettered => fact
            .read::<SteerDeadLettered>()
            .and_then(|body| body.key.parse().ok()),
        wire::FactKind::SteerCanceled => fact
            .read::<SteerCanceled>()
            .and_then(|body| body.key.parse().ok()),
        _ => None,
    }
}

struct AppliedSteering {
    affected: Option<CanonicalUuid>,
    created: bool,
    missed: Option<SteerMissed>,
}

fn replay<'a>(
    facts: impl IntoIterator<Item = &'a wire::Envelope<wire::AnyFact>>,
) -> Result<AuthorityReplay, ReplayError> {
    let mut ledger = Ledger::new();
    let mut positions = BTreeMap::new();
    let mut misses = Vec::new();
    for envelope in facts {
        let Some(applied) = ledger.apply_envelope(envelope)? else {
            continue;
        };
        if let Some(missed) = applied.missed {
            misses.push(journal::JournalHead::new(envelope.seq, missed));
        }
        if let Some(directive) = applied.affected {
            if applied.created {
                positions.insert(
                    directive,
                    AuthorityPosition {
                        origin: envelope.origin.clone(),
                        created: envelope.seq,
                        head: envelope.seq,
                    },
                );
            } else {
                positions
                    .get_mut(&directive)
                    .expect("a transition follows one creating fact")
                    .head = envelope.seq;
            }
        }
    }
    Ok((ledger, positions, misses))
}
