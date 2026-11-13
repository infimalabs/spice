//! Directives as durable state: publication, presentation, disposition, and expiry.
//!
//! Operator steering is state, not a message that scrolled past. A directive is
//! appended once and then retires only through a typed disposition, so what a
//! lane still owes is a question about facts rather than about whether a
//! terminal happened to be watched.
//!
//! Silence changes the presentation of the published directive without changing
//! its identity. Each named transition is retained in the directive's audit,
//! while acknowledgment, refusal, cancellation, lapse, and recovery address the
//! same UUID and human key throughout.
//!
//! Everything here reduces facts into standing. It appends nothing: the command
//! door mints directives, the scanner mints retirements, and this decides what
//! those facts mean together — which is why replaying the same facts always
//! produces the same pending set.

use std::collections::BTreeMap;

use wire::body::{
    SteerAccepted, SteerAcked, SteerCanceled, SteerDeclined, SteerEscalated, SteerLapsed,
    SteerMissed, SteerRecovered, SteerSent, SteeringStage, SteeringTransition, TasteDecision,
    TasteJudged, TasteReminded,
};
use wire::steering::SteerDeadLettered;
use wire::{Actor, AgentMediaReference, CanonicalUuid, Disposition, Instant, Media};

use crate::ack::keys::Key;
use crate::ack::scan::Standing;

mod refusal;
#[cfg(not(target_arch = "wasm32"))]
mod replay;

pub use refusal::{Refusal, ReplayError};
#[cfg(not(target_arch = "wasm32"))]
pub use replay::{AuthorityIndex, affected_key};

/// One stable directive, as the facts left it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directive {
    directive: CanonicalUuid,
    fanout: Option<CanonicalUuid>,
    key: Key,
    recipient: Actor,
    text: String,
    sender: Actor,
    room: Option<wire::rooms::RoomAnchor>,
    control: Option<String>,
    attachments: Vec<Media>,
    recipients: Vec<wire::body::SteeringRecipient>,
    expires: Instant,
    published: Option<Instant>,
    stage: SteeringStage,
    escalations: Vec<SteeringTransition>,
    disposition: Disposition,
}

impl Directive {
    /// Durable identity of this directive.
    #[must_use]
    pub const fn directive(&self) -> CanonicalUuid {
        self.directive
    }

    /// Operator submit action shared by target-specific fanout directives.
    #[must_use]
    pub const fn fanout(&self) -> Option<CanonicalUuid> {
        self.fanout
    }

    /// Intended target-specific siblings, including this directive.
    ///
    /// Consumers must verify each identity against durable standing before
    /// claiming the sibling received the direction.
    #[must_use]
    pub fn recipients(&self) -> &[wire::body::SteeringRecipient] {
        &self.recipients
    }

    /// The key this directive is quoted by.
    #[must_use]
    pub const fn key(&self) -> &Key {
        &self.key
    }

    /// The actor this directive is addressed to.
    #[must_use]
    pub const fn recipient(&self) -> &Actor {
        &self.recipient
    }

    /// The direction itself.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The actor that sent this direction.
    #[must_use]
    pub const fn sender(&self) -> &Actor {
        &self.sender
    }

    /// Anchor task whose room context a peer route froze at publication.
    #[must_use]
    pub fn room(&self) -> Option<&wire::rooms::RoomAnchor> {
        self.room.as_ref()
    }

    /// The control word the operator attached, if any.
    #[must_use]
    pub fn control(&self) -> Option<&str> {
        self.control.as_deref()
    }

    /// Exact typed attachments retained with this directive.
    #[must_use]
    pub fn attachments(&self) -> &[Media] {
        &self.attachments
    }

    /// Compact agent-facing references to these attachments in source order.
    ///
    /// # Errors
    ///
    /// Returns the shared reference error if retained authority is malformed.
    pub fn media_references(
        &self,
    ) -> Result<Vec<AgentMediaReference>, wire::media::AgentMediaReferenceError> {
        wire::media::agent_media_references(self.directive, &self.attachments)
    }

    /// When this directive lapses if nothing retires it first.
    #[must_use]
    pub const fn expires(&self) -> Instant {
        self.expires
    }

    /// Original publication instant, present on directives replayed from a journal.
    #[must_use]
    pub const fn published(&self) -> Option<Instant> {
        self.published
    }

    /// Current named presentation state.
    #[must_use]
    pub const fn stage(&self) -> SteeringStage {
        self.stage
    }

    /// Complete ordered presentation-transition audit.
    #[must_use]
    pub fn escalations(&self) -> &[SteeringTransition] {
        &self.escalations
    }

    /// The state this directive currently sits in.
    #[must_use]
    pub const fn disposition(&self) -> Disposition {
        self.disposition
    }

    fn from_head(head: &wire::SteeringHead) -> Result<Self, Refusal> {
        let key = read("key", &head.key)?;
        admit_attachments(head.directive, &head.attachments)?;
        Ok(Self {
            directive: head.directive,
            fanout: head.fanout,
            key,
            recipient: head.recipient.clone(),
            text: head.text.clone(),
            sender: head.sender.clone(),
            room: head.room.clone(),
            control: head.control.clone(),
            attachments: head.attachments.clone(),
            recipients: head.recipients.clone(),
            expires: head.expires,
            published: Some(head.published),
            stage: head.stage,
            escalations: head.escalations.clone(),
            disposition: head.disposition,
        })
    }
}

/// One steering fact, as the reducer reads it.
#[derive(Debug, Clone, Copy)]
pub enum Steering<'a> {
    /// A directive was appended.
    Sent(&'a SteerSent),
    /// A live directive crossed one named silence horizon.
    Escalated(&'a SteerEscalated),
    /// A lapsed directive was restored to live standing.
    Recovered(&'a SteerRecovered),
    /// A directive was heard.
    Acked(&'a SteerAcked),
    /// A typed invitation was accepted through the command door.
    Accepted(&'a SteerAccepted),
    /// A directive was refused with a reason.
    Declined(&'a SteerDeclined),
    /// An acknowledgment did not land.
    Missed(&'a SteerMissed),
    /// A directive reached its expiry unheard.
    Lapsed(&'a SteerLapsed),
    /// A directive was parked because its lane could not start.
    DeadLettered(&'a SteerDeadLettered),
    /// The operator withdrew a directive.
    Canceled(&'a SteerCanceled),
    /// A taste fired on authored prose and opened a directive.
    ///
    /// It reads as a steering fact because it is one. The taste evidence it
    /// also carries is read by the taste plane from the same row, and never by
    /// anything here.
    Reminded(&'a TasteReminded),
    /// A judge-confirmed taste candidate opened a directive.
    ///
    /// Cleared judgments are also replayed through this arm but deliberately
    /// alter no steering standing.
    Judged(&'a TasteJudged),
}

/// What every steering fact so far leaves standing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ledger {
    order: Vec<Key>,
    directives: BTreeMap<Key, Directive>,
    identities: BTreeMap<CanonicalUuid, Key>,
}

impl Ledger {
    /// An empty ledger, before any fact.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Materialize a bounded ledger from authoritative current heads.
    ///
    /// Callers normally pass one directive or one lane's live set. This keeps
    /// existing pure rendering and recognition logic while ensuring retired
    /// history outside the requested keyspace is never loaded.
    pub fn from_authority_heads<'a>(
        heads: impl IntoIterator<Item = &'a wire::SteeringHead>,
    ) -> Result<Self, Refusal> {
        let mut ledger = Self::new();
        for head in heads {
            ledger.open(Directive::from_head(head)?)?;
        }
        Ok(ledger)
    }

    /// Folds one steering fact into standing.
    ///
    /// # Errors
    ///
    /// Returns the [`Refusal`] naming what the fact contradicts. A refusal
    /// leaves standing exactly as it was, so a rejected fact cannot half-apply.
    pub fn absorb(&mut self, steering: Steering<'_>) -> Result<(), Refusal> {
        self.absorb_recorded(steering, None)
    }

    fn absorb_recorded(
        &mut self,
        steering: Steering<'_>,
        at: Option<Instant>,
    ) -> Result<(), Refusal> {
        match steering {
            Steering::Sent(body) => self.send(body, at),
            Steering::Reminded(body) => self.remind(body, at),
            Steering::Judged(body) => self.judged(body, at),
            Steering::Escalated(body) => self.escalate(body, at),
            Steering::Recovered(body) => self.recover(body),
            Steering::Acked(body) => self.dispose(
                &body.key,
                body.directive,
                &body.recipient,
                Disposition::Acked,
            ),
            Steering::Accepted(body) => self.dispose(
                &body.key,
                body.directive,
                &Actor::Lane(body.lane),
                Disposition::Acked,
            ),
            Steering::Declined(body) => self.dispose(
                &body.key,
                body.directive,
                &body.recipient,
                Disposition::Declined,
            ),
            Steering::Canceled(body) => {
                self.retire(&read("key", &body.key)?, Disposition::Canceled)
            }
            Steering::Lapsed(body) => self.lapse(body),
            Steering::DeadLettered(body) => self.dead_letter(body),
            // A missed acknowledgment alters nothing, by law. Its key is not
            // even read: the fact exists precisely because the key was unknown,
            // malformed, quoted, or historical, so refusing it here would
            // refuse the diagnostic instead of recording it.
            Steering::Missed(_) => Ok(()),
        }
    }

    /// The directives a lane still owes an answer for, in journal order.
    ///
    /// Each publication answers as exactly one directive at every urgency.
    #[must_use]
    pub fn pending(&self, lane: CanonicalUuid) -> Vec<&Directive> {
        let recipient = Actor::Lane(lane);
        self.order
            .iter()
            .filter_map(|key| self.directives.get(key))
            .filter(|directive| {
                directive.recipient == recipient && directive.disposition == Disposition::Live
            })
            .collect()
    }

    /// Every directive in journal order, whatever it retired into.
    #[must_use]
    pub fn directives(&self) -> Vec<&Directive> {
        self.order
            .iter()
            .filter_map(|key| self.directives.get(key))
            .collect()
    }

    /// One directive by durable identity, if it exists.
    #[must_use]
    pub fn directive(&self, id: CanonicalUuid) -> Option<&Directive> {
        self.identities
            .get(&id)
            .and_then(|key| self.directives.get(key))
    }

    /// Open standing for one newly published directive.
    ///
    /// Both publication paths land here rather than each stating the admission
    /// for itself. The two refusals are the whole publication identity law -- a
    /// key already standing, and an identity already spent -- so
    /// sharing them is what stops one origin from drifting into accepting a
    /// duplicate the other rejects.
    fn open(&mut self, opened: Directive) -> Result<(), Refusal> {
        if self.directives.contains_key(&opened.key) {
            return Err(Refusal::Duplicate { key: opened.key });
        }
        if self.directive(opened.directive).is_some() {
            return Err(Refusal::IdentityDuplicate {
                directive: opened.directive,
            });
        }
        self.place(opened);
        Ok(())
    }

    fn send(&mut self, body: &SteerSent, published: Option<Instant>) -> Result<(), Refusal> {
        let key = read("key", &body.key)?;
        admit_attachments(body.directive, &body.attachments)?;
        self.open(Directive {
            directive: body.directive,
            fanout: body.fanout,
            key,
            recipient: body.recipient.clone(),
            text: body.text.clone(),
            sender: body.sender.clone(),
            room: body.room.clone(),
            control: body.control.clone(),
            attachments: body.attachments.clone(),
            recipients: body.recipients.clone(),
            expires: body.expires,
            published,
            stage: SteeringStage::Published,
            escalations: Vec::new(),
            disposition: Disposition::Live,
        })
    }

    /// A taste reminder opens standing exactly as an operator send does.
    ///
    /// Everything a directive is asked afterwards -- whether it is live, what
    /// retires it, when it lapses, how it resends, what the inbox renders -- is
    /// answered from the standing this places, so none of those readers needs to
    /// know a taste produced it. That is the point of routing it here: a
    /// taste-only branch downstream is a second definition of "live", and two
    /// definitions are what let a reminder become unretirable.
    ///
    /// A reminder drives no steering control and carries no attachments. It is
    /// prose addressed to a lane, which is what an operator's plainest send is.
    fn remind(&mut self, body: &TasteReminded, published: Option<Instant>) -> Result<(), Refusal> {
        let key = read("key", &body.key)?;
        self.open(Directive {
            directive: body.directive,
            fanout: None,
            key,
            recipient: Actor::Lane(body.lane),
            text: body.text.clone(),
            sender: Actor::Spice,
            room: None,
            control: None,
            attachments: Vec::new(),
            recipients: Vec::new(),
            expires: body.expires,
            published,
            stage: SteeringStage::Published,
            escalations: Vec::new(),
            disposition: Disposition::Live,
        })
    }

    /// Open only the confirmed half of canonical adjudication evidence.
    fn judged(&mut self, body: &TasteJudged, published: Option<Instant>) -> Result<(), Refusal> {
        let TasteDecision::Confirmed {
            directive,
            key,
            text,
            expires,
        } = &body.decision
        else {
            return Ok(());
        };
        self.open(Directive {
            directive: *directive,
            fanout: None,
            key: read("key", key)?,
            recipient: Actor::Lane(body.lane),
            text: text.clone(),
            sender: Actor::Spice,
            room: None,
            control: None,
            attachments: Vec::new(),
            recipients: Vec::new(),
            expires: *expires,
            published,
            stage: SteeringStage::Published,
            escalations: Vec::new(),
            disposition: Disposition::Live,
        })
    }

    fn escalate(&mut self, body: &SteerEscalated, at: Option<Instant>) -> Result<(), Refusal> {
        let key = read("key", &body.key)?;
        let directive = self
            .directives
            .get_mut(&key)
            .ok_or_else(|| Refusal::Unknown { key: key.clone() })?;
        if directive.directive != body.directive {
            return Err(Refusal::Identity {
                key,
                expected: directive.directive,
                observed: body.directive,
            });
        }
        if directive.disposition != Disposition::Live {
            return Err(Refusal::Retired {
                key,
                disposition: directive.disposition,
            });
        }
        if directive.recipient != body.recipient {
            return Err(Refusal::Recipient {
                key,
                expected: directive.recipient.clone(),
                observed: body.recipient.clone(),
            });
        }
        let Some(expected) = directive.stage.next() else {
            return Err(Refusal::FinalStage { key });
        };
        if body.stage != expected {
            return Err(Refusal::Stage {
                key,
                expected,
                observed: body.stage,
            });
        }
        if let Some(at) = at {
            let previous = directive
                .escalations
                .last()
                .map(|transition| transition.at)
                .or(directive.published);
            if let Some(previous) = previous
                && at < previous
            {
                return Err(Refusal::Chronology {
                    key,
                    previous,
                    observed: at,
                });
            }
            directive.escalations.push(SteeringTransition {
                stage: body.stage,
                at,
            });
        }
        directive.stage = body.stage;
        Ok(())
    }

    fn recover(&mut self, body: &SteerRecovered) -> Result<(), Refusal> {
        let key = read("key", &body.key)?;
        let directive = self
            .directives
            .get(&key)
            .ok_or_else(|| Refusal::Unknown { key: key.clone() })?;
        if directive.directive != body.directive {
            return Err(Refusal::Identity {
                key,
                expected: directive.directive,
                observed: body.directive,
            });
        }
        if directive.recipient != body.recipient {
            return Err(Refusal::Recipient {
                key,
                expected: directive.recipient.clone(),
                observed: body.recipient.clone(),
            });
        }
        if !matches!(
            directive.disposition,
            Disposition::Lapsed | Disposition::DeadLettered
        ) {
            return Err(Refusal::Retired {
                key,
                disposition: directive.disposition,
            });
        }
        if directive.expires != body.lapsed_at {
            return Err(Refusal::Expiry {
                key,
                expected: directive.expires,
                observed: body.lapsed_at,
            });
        }
        if body.expires <= body.lapsed_at {
            return Err(Refusal::RecoveryExpiry {
                key,
                lapsed_at: body.lapsed_at,
                expires: body.expires,
            });
        }
        let directive = self
            .directives
            .get_mut(&key)
            .expect("validated directive remains present");
        directive.disposition = Disposition::Live;
        directive.expires = body.expires;
        Ok(())
    }

    fn dispose(
        &mut self,
        text: &str,
        identity: CanonicalUuid,
        recipient: &Actor,
        disposition: Disposition,
    ) -> Result<(), Refusal> {
        let key = read("key", text)?;
        let directive = self
            .directives
            .get(&key)
            .ok_or_else(|| Refusal::Unknown { key: key.clone() })?;
        if directive.directive != identity {
            return Err(Refusal::Identity {
                key,
                expected: directive.directive,
                observed: identity,
            });
        }
        if &directive.recipient != recipient {
            return Err(Refusal::Recipient {
                key,
                expected: directive.recipient.clone(),
                observed: recipient.clone(),
            });
        }
        self.retire(&key, disposition)
    }

    fn lapse(&mut self, body: &SteerLapsed) -> Result<(), Refusal> {
        let key = read("key", &body.key)?;
        let expected = self
            .directives
            .get(&key)
            .ok_or_else(|| Refusal::Unknown { key: key.clone() })?
            .expires;
        if expected != body.expires {
            return Err(Refusal::Expiry {
                key,
                expected,
                observed: body.expires,
            });
        }
        self.retire(&key, Disposition::Lapsed)
    }

    fn dead_letter(&mut self, body: &SteerDeadLettered) -> Result<(), Refusal> {
        self.dispose(
            &body.key,
            body.directive,
            &Actor::Lane(body.lane),
            Disposition::DeadLettered,
        )
    }

    /// Retire the one stable directive addressed by this key.
    fn retire(&mut self, key: &Key, disposition: Disposition) -> Result<(), Refusal> {
        let directive = self
            .directives
            .get_mut(key)
            .ok_or_else(|| Refusal::Unknown { key: key.clone() })?;
        if directive.disposition != Disposition::Live {
            return Err(Refusal::Retired {
                key: key.clone(),
                disposition: directive.disposition,
            });
        }
        directive.disposition = disposition;
        Ok(())
    }

    fn place(&mut self, directive: Directive) {
        self.order.push(directive.key.clone());
        self.identities
            .insert(directive.directive, directive.key.clone());
        self.directives.insert(directive.key.clone(), directive);
    }
}

impl Standing for Ledger {
    fn disposition(&self, key: &Key) -> Option<Disposition> {
        self.directives.get(key).map(Directive::disposition)
    }
}

/// Reads a key out of a wire field, naming the field when it does not spell one.
fn read(field: &'static str, text: &str) -> Result<Key, Refusal> {
    text.parse().map_err(|_| Refusal::Malformed {
        field,
        text: text.to_owned(),
    })
}

fn admit_attachments(directive: CanonicalUuid, attachments: &[Media]) -> Result<(), Refusal> {
    wire::media::validate_media(attachments)
        .map_err(|error| Refusal::InvalidMedia { directive, error })
}

#[cfg(test)]
mod tests;
