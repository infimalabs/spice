//! One body DTO per fact kind.
//!
//! Every kind in the closed vocabulary registers exactly one body here, and the
//! registry in [`crate::fact`] binds them. A body's fields are the nouns its
//! own kind names, or the head content that kind points at; a field anchored in
//! neither does not belong in the body.
//!
//! Bodies flatten beside the discriminant, so their field names are the fact
//! object's own keys and cannot collide with `kind`.

use serde::{Deserialize, Serialize};

use crate::envelope::{Actor, Seq};
use crate::head::TranscriptSourceHead;
use crate::ident::{CanonicalUuid, Digest, Instant};
use crate::witness::{Phase, TaskStamp};
use crate::{Media, OccupantAddress};

#[path = "body/transcript.rs"]
mod transcript;

pub use transcript::{JournalIngested, TranscriptPosition, TranscriptSpan, TranscriptUsage};

mod publication;
pub use publication::{ProofSealed, PublishedTarget, RackAdmitted, ReleasePublished};

mod task;
pub use task::*;

mod team_departure;
pub use team_departure::TeamDepartureNoticed;

/// The global app settings changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalSettingsChanged {
    /// Whether supported agent vendors launch in fast mode.
    pub fast_mode: bool,
}

impl GlobalSettingsChanged {
    /// A settings change that records global fast mode.
    #[must_use]
    pub const fn new(fast_mode: bool) -> Self {
        Self { fast_mode }
    }
}

/// One intended member of a target-specific steering fanout.
///
/// The directive identity lets a reader distinguish a durable sibling from a
/// target the client merely intended to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteeringRecipient {
    /// Lane the sibling direction addresses.
    pub lane: CanonicalUuid,
    /// Stable identity of that lane's sibling direction.
    pub directive: CanonicalUuid,
}

/// Durable identity shared by every input on one peer conversation line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerLineRef {
    /// Independent line root. The opening directive uses this identity.
    pub root: CanonicalUuid,
    /// Root whose exact input opened this linked child, when any.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub parent: Option<CanonicalUuid>,
    /// First peer input on this line.
    pub opening: CanonicalUuid,
}

/// Meaning of one peer body on its line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PeerBodyKind {
    /// A SYN opened the independent line.
    Open,
    /// A substantive ACK continued the line.
    Continue,
    /// A FIN closed the line while carrying a final body.
    Final,
    /// A NAK declined the line while carrying a reason.
    Decline,
}

impl PeerBodyKind {
    /// Canonical control spelling persisted beside a peer directive.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Continue => "continue",
            Self::Final => "final",
            Self::Decline => "decline",
        }
    }

    /// Read the canonical control spelling.
    #[must_use]
    pub fn read(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "continue" => Some(Self::Continue),
            "final" => Some(Self::Final),
            "decline" => Some(Self::Decline),
            _ => None,
        }
    }
}

/// Why one substantive peer body became input to its recipient lane.
///
/// The admitting write records the cause once and nothing recomputes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PeerAdmission {
    /// The recipient lane was seated and heard the body in its running session.
    Active,
    /// The body joined the exact committed start of the recipient's session.
    Cohort,
    /// Sender and recipient shared an unfinished round.
    SameRound,
    /// The opening sender's one exact return on its line.
    FirstReturn,
    /// A counted start outside any round spent one token of the ordered pair.
    PairToken(PairSpend),
}

/// The ordered pair's balance left by one counted start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairSpend {
    /// Tokens the pair holds after this spend.
    pub remaining: u8,
    /// Accrual instant `remaining` is reckoned from.
    pub frontier: Instant,
}

/// A directive was created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteerSent {
    /// Durable directive identity, equal to the command that created it.
    pub directive: CanonicalUuid,
    /// Operator submit action that may fan out into target-specific directives.
    ///
    /// Every operator send carries it; peer and resident directives omit it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub fanout: Option<CanonicalUuid>,
    /// Directive key.
    pub key: String,
    /// Actor the directive addresses.
    pub recipient: Actor,
    /// Directive text.
    pub text: String,
    /// Actor that authored the directive.
    pub sender: Actor,
    /// Anchor task whose room context a peer route froze at publication.
    ///
    /// Absent on operator direction, peer replies, direct peer routes, and
    /// every message older than room routes.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub room: Option<crate::rooms::RoomAnchor>,
    /// Steering control the directive drives; absent on plain prose.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub control: Option<String>,
    /// Admitted browsable images carried with this direction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Media>,
    /// Intended target-specific siblings of this direction, including itself.
    ///
    /// Readers expose only siblings whose directive identity was durably
    /// published with this fanout.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recipients: Vec<SteeringRecipient>,
    /// Instant the directive expires without a disposition.
    pub expires: Instant,
}

/// A lane's answer to a live peer message opened a reply to the lane that told it.
///
/// Minted in the same write that retires the answered peer message, so the
/// reply exists exactly when the answer does. The reply is ordinary steering
/// for the lane that told the peer message; answering it earns no reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteerReplied {
    /// Durable reply identity, derived from the answered directive.
    pub directive: CanonicalUuid,
    /// Reply key.
    pub key: String,
    /// Lane the reply addresses: the lane that told the answered peer message.
    pub lane: CanonicalUuid,
    /// Lane that answered the peer message, speaking as the reply's sender.
    pub from: CanonicalUuid,
    /// Key of the answered peer message.
    pub answered: String,
    /// How the answering lane answered.
    pub answer: crate::PeerAnswer,
    /// The answer as spoken: marker, answered key, and the header's clause.
    pub text: String,
    /// Instant the reply expires without a disposition.
    pub expires: Instant,
}

impl SteerReplied {
    /// The directive this reply opens, as the publication steering folds.
    #[must_use]
    pub fn as_sent(&self) -> SteerSent {
        SteerSent {
            directive: self.directive,
            fanout: None,
            key: self.key.clone(),
            recipient: Actor::Lane(self.lane),
            text: self.text.clone(),
            sender: Actor::Lane(self.from),
            room: None,
            control: Some(crate::peer_reply_control(&self.answered, self.answer)),
            attachments: Vec::new(),
            recipients: Vec::new(),
            expires: self.expires,
        }
    }
}

/// Where one peer message body lies in the decoded text of the transcript
/// event its fact's locus names, and the digest of that text, so the
/// transcript and the journal can each check or rebuild the other.
///
/// The decoded text is the event's assistant text for authored prose and the
/// captured submission text for an outbox write. Range and digest are one
/// value, so a fact never records either without the other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodySpan {
    /// Half-open byte range of the body within the event's decoded text.
    pub range: crate::rooms::TextRange,
    /// [`crate::QuoteTarget::text_digest`] of the body text.
    pub digest: Digest,
}

impl BodySpan {
    /// The span of `body`, which is exactly the bytes at `range` of its
    /// event's decoded text.
    #[must_use]
    pub fn new(range: std::ops::Range<usize>, body: &str) -> Self {
        debug_assert_eq!(range.len(), body.len(), "a body is its whole range");
        Self {
            range: crate::rooms::TextRange {
                start: range.start as u64,
                end: range.end as u64,
            },
            digest: crate::QuoteTarget::text_digest(body),
        }
    }
}

/// An authored SYN packet opened one peer message to one exact recipient.
///
/// The packet's assistant transcript record is the source evidence, and the
/// resident journals one fact per admitted recipient in a single guarded
/// write. The directive identity is derived from the source packet and the
/// address the route named, so rereading the same packet never opens a second
/// message. The opened directive is ordinary peer steering bound to the
/// recipient lane, which whichever occupant sits there hears and answers;
/// answering it replies to `from`. A route naming an earlier occupant of a
/// lane the residency now seats with a proved successor delivers to that
/// successor and keeps the named address as `addressed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteerMessaged {
    /// Durable message identity, derived from the source packet and recipient.
    pub directive: CanonicalUuid,
    /// Message key.
    pub key: String,
    /// Recipient lane, the message's destination.
    pub lane: CanonicalUuid,
    /// Recipient occupant the residency seated when the packet published: the
    /// route's evidence, not its destination.
    pub occupant: CanonicalUuid,
    /// Lane whose occupant authored the packet.
    pub from: CanonicalUuid,
    /// Occupant whose captured transcript authored the packet.
    pub sender: CanonicalUuid,
    /// The packet's exact body.
    pub text: String,
    /// Assistant transcript locus of the packet's SYN header.
    #[serde(with = "sourced_transcript_locus")]
    pub locus: TranscriptLocus,
    /// Where the body lies in the event `locus` names, and its digest.
    pub span: BodySpan,
    /// Anchor task whose room context the route named, frozen at publication;
    /// absent on a direct route and on every message older than room routes.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub room: Option<crate::rooms::RoomAnchor>,
    /// The earlier address the route named when `occupant` is its proved
    /// successor on the same lane; absent when the route named `occupant`'s
    /// own address.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub addressed: Option<OccupantAddress>,
    /// Peer line this body opens or continues.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub line: Option<PeerLineRef>,
    /// Body meaning on the line.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub peer_body: Option<PeerBodyKind>,
    /// Exact input directives this body answers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covered: Vec<CanonicalUuid>,
    /// Why the body was admitted; absent on a body admitted before causes
    /// were recorded.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub admission: Option<PeerAdmission>,
    /// Instant the message expires without a disposition.
    pub expires: Instant,
}

impl SteerMessaged {
    /// The directive this message opens, as the publication steering folds.
    #[must_use]
    pub fn as_sent(&self) -> SteerSent {
        SteerSent {
            directive: self.directive,
            fanout: None,
            recipients: Vec::new(),
            key: self.key.clone(),
            recipient: Actor::Lane(self.lane),
            text: self.text.clone(),
            sender: Actor::Lane(self.from),
            room: self.room.clone(),
            control: self.line.map(|line| {
                crate::peer_line_control(
                    line,
                    self.peer_body.unwrap_or(PeerBodyKind::Continue),
                    &self.covered,
                )
            }),
            attachments: Vec::new(),
            expires: self.expires,
        }
    }
}

/// An authored SYN packet opened one peer directive while its proved
/// destination lane was vacant.
///
/// The original occupant address proves the logical lane but is not a seated
/// recipient. The directive stays live for that lane's next occupant without
/// changing its key or expiry. A recorded `admission` is the wake its
/// admission committed and holds until the directive is disposed; a body
/// without one wakes only while its room's current round binds sender and
/// recipient, as the resident rechecks at every wake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteerQueued {
    /// Durable message identity, derived from the source packet and address.
    pub directive: CanonicalUuid,
    /// Message key.
    pub key: String,
    /// Vacant logical lane that inherits this directive at its next seat.
    pub lane: CanonicalUuid,
    /// Exact historical address whose unique birth proved `lane`.
    pub recipient: OccupantAddress,
    /// Lane whose occupant authored the packet.
    pub from: CanonicalUuid,
    /// Occupant whose captured transcript authored the packet.
    pub sender: CanonicalUuid,
    /// The packet's exact body.
    pub text: String,
    /// Assistant transcript locus of the packet's header.
    #[serde(with = "sourced_transcript_locus")]
    pub locus: TranscriptLocus,
    /// Where the body lies in the event `locus` names, and its digest.
    pub span: BodySpan,
    /// Frozen room context, when the route named one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub room: Option<crate::rooms::RoomAnchor>,
    /// Peer line this queued body opens or continues.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub line: Option<PeerLineRef>,
    /// Body meaning on the line.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub peer_body: Option<PeerBodyKind>,
    /// Exact input directives this body answers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covered: Vec<CanonicalUuid>,
    /// Why the body was admitted; absent on a body admitted before causes
    /// were recorded.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub admission: Option<PeerAdmission>,
    /// Instant the queued directive expires without a disposition.
    pub expires: Instant,
}

impl SteerQueued {
    /// The lane-scoped directive this queued route opens.
    #[must_use]
    pub fn as_sent(&self) -> SteerSent {
        SteerSent {
            directive: self.directive,
            fanout: None,
            recipients: Vec::new(),
            key: self.key.clone(),
            recipient: Actor::Lane(self.lane),
            text: self.text.clone(),
            sender: Actor::Lane(self.from),
            room: self.room.clone(),
            control: self.line.map(|line| {
                crate::peer_line_control(
                    line,
                    self.peer_body.unwrap_or(PeerBodyKind::Continue),
                    &self.covered,
                )
            }),
            attachments: Vec::new(),
            expires: self.expires,
        }
    }
}

/// One authored control transition on a peer line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PeerLineAction {
    /// Exact nod or empty ACK: seen, with no new receipt duty.
    Reaction,
    /// Substantive ACK continued the line.
    Continue,
    /// FIN closed the line.
    Finish,
    /// NAK declined the line.
    Decline,
    /// Guarded resident reconciliation closed a fully settled line.
    AutomaticFinish,
    /// One of the line's inputs retired unanswered: it lapsed, was
    /// dead-lettered or was cancelled. Elapsed time and failed delivery prove
    /// no clean completion, so this ending is named apart from FIN, NAK and
    /// the automatic close.
    Unanswered,
}

impl PeerLineAction {
    /// Whether this transition establishes the line's one terminal authority.
    #[must_use]
    pub const fn terminal(self) -> bool {
        matches!(
            self,
            Self::Finish | Self::Decline | Self::AutomaticFinish | Self::Unanswered
        )
    }

    /// Whether this transition answers its input. An unanswered ending
    /// retires the line around an input no control ever covered.
    #[must_use]
    pub const fn covers(self) -> bool {
        !matches!(self, Self::Unanswered)
    }
}

/// A keyed control changed one exact peer line input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteerLineTransition {
    /// Line whose state changed.
    pub line: CanonicalUuid,
    /// Exact input directive the control covered, or the input whose
    /// unanswered retirement ended the line.
    pub input: CanonicalUuid,

    /// State transition.
    pub action: PeerLineAction,
    /// New peer directive carrying the substantive body, when admitted.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub body: Option<CanonicalUuid>,
}

/// An authored SYN packet resolved against the residency and published to no one.
///
/// The refusal is the packet's settled judgment: a later reread of the same
/// source record, including startup recovery, finds it and leaves the packet
/// unpublished even when the cause of the refusal has since changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteerWithheld {
    /// Lane whose occupant authored the packet.
    pub from: CanonicalUuid,
    /// Occupant whose captured transcript authored the packet.
    pub sender: CanonicalUuid,
    /// Assistant transcript locus of the packet's SYN header.
    #[serde(with = "sourced_transcript_locus")]
    pub locus: TranscriptLocus,
    /// Stable refusal reason.
    pub reason: String,
    /// What made the packet unpublishable when it was judged.
    pub detail: String,
}

/// How urgently one stable directive is currently presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SteeringStage {
    /// The original publication has not crossed its first silence horizon.
    Published,
    /// Silence crossed the first horizon.
    Reminded,
    /// Silence crossed the second horizon.
    Escalated,
    /// Silence crossed the final pre-expiry horizon.
    Overdue,
}

impl SteeringStage {
    /// Stable lowercase spelling used on operator surfaces.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Published => "published",
            Self::Reminded => "reminded",
            Self::Escalated => "escalated",
            Self::Overdue => "overdue",
        }
    }

    /// The next presentation state, absent at the last pre-expiry state.
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self {
            Self::Published => Some(Self::Reminded),
            Self::Reminded => Some(Self::Escalated),
            Self::Escalated => Some(Self::Overdue),
            Self::Overdue => None,
        }
    }
}

impl std::fmt::Display for SteeringStage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One durable presentation transition in a directive's audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteeringTransition {
    /// State entered at this transition.
    pub stage: SteeringStage,
    /// Journal instant at which the transition was recorded.
    pub at: Instant,
}

/// One stable directive crossed a named silence horizon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteerEscalated {
    /// Durable identity retained from publication.
    pub directive: CanonicalUuid,
    /// Human key retained from publication.
    pub key: String,
    /// Actor the directive addresses.
    pub recipient: Actor,
    /// Named presentation state entered by this fact.
    pub stage: SteeringStage,
}

/// A lapsed directive was restored to live standing exactly once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteerRecovered {
    /// Durable directive identity retained by the recovery.
    pub directive: CanonicalUuid,
    /// Human-sized key retained by the recovery.
    pub key: String,
    /// Actor the directive remains addressed to.
    pub recipient: Actor,
    /// Expiry that proves the failed standing being recovered.
    pub lapsed_at: Instant,
    /// Fresh bounded expiry assigned to the restored directive.
    pub expires: Instant,
}

/// Exact assistant-prose evidence inside one durable lane-tape record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptLocus {
    /// Opaque identity of the durable tape generation carrying this record.
    ///
    /// Evidence derived without a durable tape generation, such as the current
    /// taste surface, omits it. Fact families whose authority comes from a
    /// captured transcript enforce presence on their own body codec.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub source: Option<CanonicalUuid>,
    /// Tape member containing the complete vendor record.
    pub member: u64,
    /// Byte offset of the record within its member.
    pub offset: u64,
    /// Byte length of the complete vendor record.
    pub length: u64,
    /// Zero-based typed event index inside the decoded record.
    pub event: u32,
    /// One-based assistant-prose line carrying the token.
    pub line: u32,
    /// One-based byte column at which the token begins.
    pub column: u32,
}

/// Enforce source qualification on authority families that only current tape
/// records may create while retaining the shared optional locus for taste
/// evidence whose current contract has no tape generation.
mod sourced_transcript_locus {
    use serde::{Deserialize as _, Deserializer, Serialize as _, Serializer};

    use super::TranscriptLocus;

    pub(super) fn serialize<S>(locus: &TranscriptLocus, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if locus.source.is_none() {
            return Err(serde::ser::Error::custom(
                "source-qualified transcript locus omits its tape generation",
            ));
        }
        locus.serialize(serializer)
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<TranscriptLocus, D::Error>
    where
        D: Deserializer<'de>,
    {
        let locus = TranscriptLocus::deserialize(deserializer)?;
        if locus.source.is_none() {
            return Err(serde::de::Error::custom(
                "source-qualified transcript locus omits its tape generation",
            ));
        }
        Ok(locus)
    }
}

/// Durable authority that retired one steering input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SteeringEvidence {
    /// A lane's authenticated transcript carried the control.
    Transcript {
        /// Resident generation that authenticated the lane.
        epoch: CanonicalUuid,
        /// Exact assistant-prose location carrying the control.
        #[serde(with = "sourced_transcript_locus")]
        locus: TranscriptLocus,
    },
    /// An admitted command carried the control.
    Command {
        /// Durable command identity.
        command: CanonicalUuid,
    },
}
impl SteeringEvidence {
    /// Authenticated transcript evidence, when a lane tape carried the control.
    #[must_use]
    pub const fn transcript(&self) -> Option<(CanonicalUuid, TranscriptLocus)> {
        match self {
            Self::Transcript { epoch, locus } => Some((*epoch, *locus)),
            Self::Command { .. } => None,
        }
    }

    /// Admitted command identity, when a command carried the control.
    #[must_use]
    pub const fn command(&self) -> Option<CanonicalUuid> {
        match self {
            Self::Command { command } => Some(*command),
            Self::Transcript { .. } => None,
        }
    }
}

/// A live directive was acknowledged by its exact recipient.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteerAcked {
    /// Durable directive identity retired by this recognition.
    pub directive: CanonicalUuid,
    /// Acknowledged directive key.
    pub key: String,
    /// Actor that acknowledged.
    pub recipient: Actor,
    /// Authority that carried the acknowledgement.
    pub evidence: SteeringEvidence,
}

/// A live typed invitation was accepted through the command door.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteerAccepted {
    /// Durable directive identity retired by this acceptance.
    pub directive: CanonicalUuid,
    /// Accepted directive key.
    pub key: String,
    /// Lane that accepted.
    pub lane: CanonicalUuid,
}

/// A live directive was declined by its exact recipient.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteerDeclined {
    /// Durable directive identity retired by this recognition.
    pub directive: CanonicalUuid,
    /// Declined directive key.
    pub key: String,
    /// Actor that declined.
    pub recipient: Actor,
    /// Authority that carried the refusal.
    pub evidence: SteeringEvidence,
    /// Stated reason the directive cannot be done.
    pub reason: String,
}

/// Why a transcript acknowledgment recognition retired nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AcknowledgmentMiss {
    /// The acknowledgment sat inside quoted prose or code.
    Quoted,
    /// The named token was not a steering key.
    Malformed,
    /// No directive carried the named key.
    Unknown,
    /// The named directive had already retired.
    Historical,
}

/// An ACK named no live directive; the unmatched key was recorded and nothing retired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteerMissed {
    /// Unmatched key the acknowledgement named.
    pub key: String,
    /// Lane that named it.
    pub lane: CanonicalUuid,
    /// Resident generation that authenticated the lane.
    pub epoch: CanonicalUuid,
    /// Transcript locus the unmatched key appeared at.
    #[serde(with = "sourced_transcript_locus")]
    pub locus: TranscriptLocus,
    /// Scanner classification that prevented retirement.
    pub miss: AcknowledgmentMiss,
}

/// A directive reached expiry without a valid disposition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteerLapsed {
    /// Expired directive key.
    pub key: String,
    /// Actor the directive addressed.
    pub recipient: Actor,
    /// Expiry that passed.
    pub expires: Instant,
}

/// A live directive was canceled without an agent disposition: by the
/// operator, or by its own sender lane with `RST`. The envelope actor names
/// which.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteerCanceled {
    /// Canceled directive key.
    pub key: String,
    /// Actor the directive addressed.
    pub recipient: Actor,
    /// Why its canceler withdrew it, when it said.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub reason: Option<String>,
}

mod interaction;
pub use interaction::{
    ConsentAsked, ConsentDenied, ConsentGranted, ConsentLapsed, ConsentQueueCause, ConsentQueued,
};

#[path = "body/lane.rs"]
mod lane;

pub use lane::{
    CheckpointBoundary, LaneCancelled, LaneCheckpoint, LaneCompacted, LaneDegraded, LaneDrained,
    LaneEffortRetuned, LaneExited, LaneFastRetuned, LaneHeld, LaneImported, LaneKilled,
    LaneLaunchIdentity, LanePark, LaneParked, LaneRemoved, LaneRenewalChanged, LaneRenewed,
    LaneResumed, LaneRetuned, LaneRuntimeRetuned, LaneSeated, LaneSessionDeclared, LaneSignalled,
    LaneSourceBoundary, LaneStarted, LaneTimedOut, LaneUnparked, RtkHealth, RtkHealthState,
    TasteDecision, TasteJudgeAttempt, TasteJudgeReply, TasteJudged, TasteReminded, TeamArranged,
    TeamConfigured, TeamStanding, VendorFailureInput,
};

/// A seated occupant exceeded a call-count or active-silence narration bound.
///
/// One prod is one row. `moments` counts calls since visible prose; `count`
/// is shared by call and elapsed-time thresholds, so the same reminder is not
/// repeated when both become due. Prose or turn completion records a
/// `NarrationCleared` row. The meter reads the newest prod and clearance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NarrationProdded {
    /// Lane whose occupant went silent.
    pub lane: CanonicalUuid,
    /// Occupant that kept working without prose.
    pub occupant: CanonicalUuid,
    /// Tool calls issued since the occupant's last authored prose.
    pub moments: u32,
    /// Ordinal of this prod within the streak, from one.
    pub count: u32,
}

/// A prodded occupant's silent streak ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NarrationCleared {
    /// Lane whose occupant was prodded.
    pub lane: CanonicalUuid,
    /// Occupant the streak belonged to.
    pub occupant: CanonicalUuid,
    /// Tool calls the streak ran to.
    pub moments: u32,
    /// Prods the streak drew.
    pub prods: u32,
    /// What ended the streak.
    pub relief: NarrationRelief,
}

/// What ended a prodded occupant's silent streak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NarrationRelief {
    /// The occupant authored visible prose.
    Prose,
    /// The occupant's turn completed without prose.
    Turn,
}

/// An executable surface digest changed and entered quarantine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustPended {
    /// Executable surface.
    pub surface: String,
    /// Digest that entered quarantine.
    pub digest: Digest,
    /// Canonical worktree whose executable surface produced this digest.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub worktree: Option<String>,
    /// Digest it replaced; absent the first time the surface is seen.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub previous: Option<Digest>,
    /// The exact tracked and local configuration text the digest covers, so
    /// a ruling needs no live producing worktree; absent on pends recorded
    /// before documents were retained.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub documents: Option<TrustDocuments>,
}

/// The executable configuration documents one trust digest frames.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustDocuments {
    /// Tracked policy text, absent when the worktree has no tracked document.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub tracked: Option<String>,
    /// Git-common local configuration text, absent when there is none.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub local: Option<String>,
}

/// The operator approved the exact executable digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustGranted {
    /// Executable surface.
    pub surface: String,
    /// Exact approved digest.
    pub digest: Digest,
}

/// The operator withdrew approval from the exact executable digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRevoked {
    /// Executable surface.
    pub surface: String,
    /// Exact digest approval was withdrawn from.
    pub digest: Digest,
}

/// A forward schema migration, initialization from 0 included, finalized
/// before epoch mint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalMigrated {
    /// Schema the journal left.
    pub from: u32,
    /// Schema the journal reached.
    pub to: u32,
    /// Epoch minted after the migration finalized.
    pub epoch: CanonicalUuid,
}

/// A quiesced backup bundle was produced with its enumerated tape manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalBacked {
    /// Produced backup bundle.
    pub bundle: String,
    /// Enumerated tape manifest carried with the bundle.
    pub manifest: Vec<String>,
}

/// A restore protocol completed and the new epoch recognized the exchanged authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalRestored {
    /// Bundle the restore read.
    pub bundle: String,
    /// New epoch that recognized the exchange.
    pub epoch: CanonicalUuid,
    /// Authority the restore exchanged.
    pub authority: Digest,
}

/// The resident acquired the kernel writer lock and minted its epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpSeated {
    /// Minted resident epoch.
    pub epoch: CanonicalUuid,
}

/// The resident released coordination authority without ending detached lane work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpDark {
    /// Resident epoch releasing authority.
    pub epoch: CanonicalUuid,
}
