//! Current authenticated app stream requests and resident frames.
//!
//! This is one closed DTO family: a client subscribes, continues an explicitly
//! named packed cursor, or asks for a causal thread; the resident returns
//! packed facts/trust/thread answers, capture-backed activity snapshots, later
//! fact or activity updates, or one typed terminal boundary. Every variant
//! carries the current wire and resident epoch. A capability is deliberately
//! absent: it crosses exactly once in [`crate::Hello`].

use serde::{Deserialize, Serialize};

mod prose;
pub use prose::{MessageProseOrigin, superseded_prose};
mod annotation;
mod direction;
pub use annotation::{
    DirectiveBadge, MessageDelivery, MessageDirective, MessageRecipient, MessageReview,
    MessageSend, valid_message_directives, valid_message_sends,
};

/// Canonical control prefix carried by resident-generated peer-review feedback.
pub const REVIEW_FEEDBACK_CONTROL_PREFIX: &str = "review-feedback:";
/// System-owned expectation accompanying each typed review finding.
pub const REVIEW_FEEDBACK_GUIDANCE: &str = concat!(
    "Verbalize and internalize all noteworthy findings as working guidance—there is ",
    "nothing else to capture or act on because follow-ups are already boarded!—continue ",
    "working your current task."
);
/// Name the reviewed task and review fact behind one feedback directive.
///
/// The control is persisted in steering facts, so its position stays decimal.
#[must_use]
pub fn review_feedback_control(task: &TaskStamp, reviewed: Seq) -> String {
    format!("{REVIEW_FEEDBACK_CONTROL_PREFIX}{task}:{}", reviewed.get())
}

/// Decode the reviewed task and review fact from one feedback control.
#[must_use]
pub fn parse_review_feedback_control(control: &str) -> Option<(TaskStamp, Seq)> {
    let body = control.strip_prefix(REVIEW_FEEDBACK_CONTROL_PREFIX)?;
    let (task, reviewed) = body.split_once(':')?;
    let reviewed = reviewed.parse::<u64>().ok().and_then(Seq::new)?;
    Some((task.parse().ok()?, reviewed))
}

#[cfg(test)]
mod review_feedback_control_tests {
    use super::*;

    #[test]
    fn feedback_control_round_trips_only_the_canonical_task_and_review_position() {
        let task: TaskStamp = "1kKRRsDRx".parse().expect("fixture task");
        let reviewed = Seq::new(42).expect("fixture review position");
        let control = review_feedback_control(&task, reviewed);

        assert_eq!(
            parse_review_feedback_control(&control),
            Some((task, reviewed))
        );
        for malformed in [
            "review-feedback:1kKRRsDR:0",
            "review-feedback:1kKRRsDR:42:extra",
            "review-feedback:not-a-task:42",
            "task-generation:1kKRRsDR:42",
        ] {
            assert_eq!(
                parse_review_feedback_control(malformed),
                None,
                "{malformed}"
            );
        }
    }
}

/// Canonical control prefix carried by every directive on a peer line.
pub const PEER_LINE_CONTROL_PREFIX: &str = "peer-line:";

/// Persist one peer line identity, body meaning, and exact covered inputs.
#[must_use]
pub fn peer_line_control(
    line: crate::body::PeerLineRef,
    body: crate::body::PeerBodyKind,
    covered: &[CanonicalUuid],
) -> String {
    let parent = line
        .parent
        .map_or_else(|| "-".to_owned(), |parent| parent.to_string());
    let covered = covered
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{PEER_LINE_CONTROL_PREFIX}{}:{parent}:{}:{}:{covered}",
        line.root,
        line.opening,
        body.as_str()
    )
}

/// Decode the canonical peer-line control.
#[must_use]
pub fn parse_peer_line_control(
    control: &str,
) -> Option<(
    crate::body::PeerLineRef,
    crate::body::PeerBodyKind,
    Vec<CanonicalUuid>,
)> {
    let mut parts = control.strip_prefix(PEER_LINE_CONTROL_PREFIX)?.split(':');
    let root = parts.next()?.parse().ok()?;
    let parent = match parts.next()? {
        "-" => None,
        parent => Some(parent.parse().ok()?),
    };
    let opening = parts.next()?.parse().ok()?;
    let body = crate::body::PeerBodyKind::read(parts.next()?)?;
    let covered = match parts.next()? {
        "" => Vec::new(),
        covered => covered
            .split(',')
            .map(str::parse)
            .collect::<Result<Vec<_>, _>>()
            .ok()?,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((
        crate::body::PeerLineRef {
            root,
            parent,
            opening,
        },
        body,
        covered,
    ))
}

/// Canonical control prefix carried by the resident's delivery of one lane's
/// answer to a peer message back to the lane that told it.
pub const PEER_REPLY_CONTROL_PREFIX: &str = "peer-reply:";

/// How the told lane answered one peer message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum PeerAnswer {
    /// The told lane acknowledged the peer message.
    Ack,
    /// The told lane declined the peer message.
    Nak,
    /// The told lane acknowledged a task invitation and joined the task.
    Joined,
    /// The told lane declined a task invitation; staffing did not change.
    Declined,
    /// The told lane tried to join a task invitation that had already
    /// expired; staffing did not change.
    Expired,
}

impl PeerAnswer {
    /// The answer a terminal lane disposition gives a plain peer message, if
    /// it is one.
    #[must_use]
    pub const fn from_disposition(disposition: crate::Disposition) -> Option<Self> {
        match disposition {
            crate::Disposition::Acked => Some(Self::Ack),
            crate::Disposition::Declined => Some(Self::Nak),
            crate::Disposition::Live
            | crate::Disposition::Lapsed
            | crate::Disposition::DeadLettered
            | crate::Disposition::Canceled => None,
        }
    }

    /// The answer a terminal lane disposition gives a task invitation, if it
    /// is one.
    #[must_use]
    pub const fn from_invitation_disposition(disposition: crate::Disposition) -> Option<Self> {
        match disposition {
            crate::Disposition::Acked => Some(Self::Joined),
            crate::Disposition::Declined => Some(Self::Declined),
            crate::Disposition::Lapsed => Some(Self::Expired),
            crate::Disposition::Live
            | crate::Disposition::DeadLettered
            | crate::Disposition::Canceled => None,
        }
    }

    /// The control marker naming this answer.
    #[must_use]
    pub const fn marker(self) -> &'static str {
        match self {
            Self::Ack => "ACK",
            Self::Nak => "NAK",
            Self::Joined => "JOINED",
            Self::Declined => "DECLINED",
            Self::Expired => "EXPIRED",
        }
    }
}

/// Name the answered peer message key and the answer the told lane gave.
#[must_use]
pub fn peer_reply_control(key: &str, answer: PeerAnswer) -> String {
    format!("{PEER_REPLY_CONTROL_PREFIX}{key}:{}", answer.marker())
}

/// Decode the answered key and answer from one peer-reply control.
#[must_use]
pub fn parse_peer_reply_control(control: &str) -> Option<(&str, PeerAnswer)> {
    let body = control.strip_prefix(PEER_REPLY_CONTROL_PREFIX)?;
    let (key, answered) = body.split_once(':')?;
    if !crate::steering_key(key) {
        return None;
    }
    let answer = match answered {
        "ACK" => PeerAnswer::Ack,
        "NAK" => PeerAnswer::Nak,
        "JOINED" => PeerAnswer::Joined,
        "DECLINED" => PeerAnswer::Declined,
        "EXPIRED" => PeerAnswer::Expired,
        _ => return None,
    };
    Some((key, answer))
}

#[cfg(test)]
mod peer_reply_control_tests {
    use super::*;

    #[test]
    fn peer_reply_control_round_trips_only_an_answered_key_and_lane_answer() {
        for answer in [
            PeerAnswer::Ack,
            PeerAnswer::Nak,
            PeerAnswer::Joined,
            PeerAnswer::Declined,
            PeerAnswer::Expired,
        ] {
            let control = peer_reply_control("dTqdc7whW", answer);
            assert_eq!(
                parse_peer_reply_control(&control),
                Some(("dTqdc7whW", answer))
            );
        }
        for malformed in [
            "peer-reply:dTqdc7whW:LIVE",
            "peer-reply:dTqdc7wh:ACK",
            "peer-reply:dTqdc7whA:ACK",
            "peer-reply::ACK",
            "peer-reply:dTq-dc7:ACK",
            "peer-reply:dTqdc7whW:ACK:extra",
            "review-feedback:dTqdc7whW:ACK",
        ] {
            assert_eq!(parse_peer_reply_control(malformed), None, "{malformed}");
        }
    }
}

/// Newest messages every surface keeps for each agent, whatever else it drops.
///
/// The floor is per agent and independent: a louder peer, a page of older
/// history, a reconnect, or a compaction may add or remove context around it,
/// but none of them may leave an agent showing fewer than this many of its own
/// messages while it has that many. An agent with fewer shows all of them. It
/// is a floor and not a cap: a surface may retain more.
pub const MESSAGE_FLOOR: usize = 25;

use crate::{
    Actor, AnyFact, Body, CanonicalUuid, Envelope, FactKind, Fault, FaultClass, Instant, Name,
    Origin, Packed, QuotaGeneration, QuotePresentation, QuoteTarget, Refusal, RefusalDiagnostic,
    Seq, TaskStamp, TrustHead, WireVersion,
};

/// Stable source position of one decoded event in a lane transcript tape.
///
/// A tape locus identifies the durable vendor record. `event` distinguishes
/// multiple visible activities from that one record without pretending
/// transcript events are journal facts or journal positions. `u32::MAX` is
/// reserved for visible record context that is not itself a decoded event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageLocus {
    /// Tape member holding the vendor record.
    pub member: u64,
    /// Byte offset of the vendor record within the member.
    pub offset: u64,
    /// Number of bytes in the vendor record.
    pub length: u64,
    /// Zero-based decoded event index, or `u32::MAX` for record context.
    pub event: u32,
}

/// One independently pageable lane-local transcript frontier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageHistoryCursor {
    /// Transcript generation that issued this cursor. A located cursor stays
    /// pageable across later generations; a cold bootstrap does not.
    pub generation: u64,
    /// Independently paged member lane.
    pub lane: CanonicalUuid,
    /// Oldest card already admitted, or `None` for a cold bootstrap.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub before: Option<MessageLocus>,
}

/// Availability of one lane at a transcript generation frontier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TranscriptLaneState {
    /// Every durable card through the shared horizon is present.
    Complete,
    /// Older durable cards remain behind this lane's cursor.
    Truncated,
    /// The bounded source walk could not hydrate this lane.
    Unavailable,
}

/// Exact state of one current lane in a transcript generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptLaneFrontier {
    /// Current member lane.
    pub lane: CanonicalUuid,
    /// Hydration state through the generation's shared horizon.
    pub state: TranscriptLaneState,
    /// Oldest admitted card when [`TranscriptLaneState::Truncated`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub before: Option<MessageLocus>,
}

/// One atomic transcript publication through a shared durable horizon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptGeneration {
    /// Monotonic identity inside the resident epoch.
    pub generation: u64,
    /// Durable journal frontier observed after this cohort was reconciled.
    pub horizon: Seq,
    /// Changed or snapshotted cards in total display chronology.
    pub messages: Vec<AgentMessage>,
    /// Explicit state for every lane covered by this publication.
    pub frontiers: Vec<TranscriptLaneFrontier>,
}

/// Visible kind of one agent-authored transcript activity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentActivityKind {
    /// Assistant prose rendered as a conversation card.
    #[default]
    Prose,
    /// A tool call, update, or result rendered in the lane status strip.
    Tool,
    /// A tool transport of natively admitted outbox prose. Status strips use
    /// the authored text; the canonical outbox source owns the conversation
    /// card, so this transport never creates another prose card or speech.
    OutboxTransport,
    /// Vendor-visible reasoning rendered in the lane status strip.
    ///
    /// omp relays mid-turn narration as thinking blocks. Like a tool moment it is lane presence,
    /// not a conversation card: never a final answer, an acknowledgement,
    /// nor an automatic speech candidate, and it consumes no visible-card
    /// retention.
    Reasoning,
    /// Structured provider failure rendered as a conversation card.
    Failure,
    /// A new agent turn rendered as a full-width stream divider.
    Turn,
    /// Context compaction boundary rendered as a full-width stream divider.
    Compaction,
    /// A vendor session opened with no messages, rendered as a full-width
    /// stream divider where the fresh session begins.
    Session,
    /// The vendor signed an account out; the card names the account and the
    /// login the operator must run before the lane can work on it again.
    SignOut,
    /// Plain operator direction the lane's transcript recorded receiving,
    /// rendered as the operator's own conversation card. Its directive
    /// contexts each carry the delivery they present.
    Direction,
}

/// Structured provider cause carried by a failed agent activity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProviderError {
    /// Provider identity reported by the vendor, when supplied.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub provider: Option<String>,
    /// Model identity reported by the vendor, when supplied.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub model: Option<String>,
    /// Provider HTTP status reported by the vendor, when supplied.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub http_status: Option<u16>,
    /// Whether retrying the failed prompt may succeed.
    pub retryable: bool,
}

/// Why one assistant message is eligible for automatic Speak playback, and
/// therefore how much of it the text carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AutomaticSpeechKind {
    /// A vendor-declared completed answer without an acknowledged directive:
    /// the text is its first and last meaningful paragraphs.
    Final,
    /// A hunk answering one or more positively acknowledged steering
    /// directives, final or not: the text is the whole hunk without its
    /// protocol headers, and Narrate voices it in place of the card's edges.
    Acknowledgement,
}

/// Authenticated acknowledgment disposition for one presented agent message.
/// This belongs to the answering speaker, independently of source text and
/// quote ranges. A decline wins over a pending acknowledgment, which wins over
/// a settled acknowledgment when one message answers multiple directives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MessageAcknowledgement {
    /// At least one positively answered directive has not retired yet.
    Pending,
    /// Every positively answered directive represented by this disposition has
    /// retired.
    Acknowledged,
    /// At least one directive was explicitly declined.
    Declined,
}

impl MessageAcknowledgement {
    /// Stable mixed-header precedence used by resident presentation.
    #[must_use]
    pub const fn precedence(self) -> u8 {
        match self {
            Self::Acknowledged => 0,
            Self::Pending => 1,
            Self::Declined => 2,
        }
    }
}

/// Resident-selected text for one automatic Speak delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomaticSpeech {
    /// Eligibility that selected this utterance.
    pub kind: AutomaticSpeechKind,
    /// Cleaned text handed to the configured speech backend.
    pub text: String,
}

impl AgentActivityKind {
    /// Whether this activity belongs in the prose card plane.
    #[must_use]
    pub const fn is_prose(&self) -> bool {
        matches!(self, Self::Prose)
    }

    /// Whether this activity is a conversation card on its own.
    ///
    /// Presence activities (tool moments and reasoning) only become cards when
    /// they carry media, which callers check separately.
    #[must_use]
    pub const fn is_visible(self) -> bool {
        matches!(
            self,
            Self::Prose
                | Self::Failure
                | Self::Turn
                | Self::Compaction
                | Self::Session
                | Self::SignOut
                | Self::Direction
        )
    }

    /// Whether this activity separates adjacent transcript epochs in the UI.
    #[must_use]
    pub const fn is_boundary(self) -> bool {
        matches!(self, Self::Turn | Self::Compaction | Self::Session)
    }

    /// Oldest card ordinal still inside this lane's newest `floor` messages.
    ///
    /// Turn, compaction, and fresh-session cards mark where messages meet; they
    /// are not messages and never satisfy the floor on a message's behalf. A lane with
    /// fewer than `floor` messages answers with its oldest message, so markers
    /// older than anything an operator can read stay prunable.
    #[must_use]
    pub fn message_floor_start(activities: &[Self], floor: usize) -> usize {
        if floor == 0 {
            return activities.len();
        }
        let mut messages = 0;
        let mut start = activities.len();
        for (ordinal, activity) in activities.iter().enumerate().rev() {
            if activity.is_boundary() {
                continue;
            }
            messages += 1;
            start = ordinal;
            if messages == floor {
                break;
            }
        }
        start
    }

    /// Oldest visible-card ordinal retained by one bounded lane history.
    ///
    /// Under capacity pressure, a compaction inside the newest `limit` cards
    /// is the horizon: no fragment of the preceding context epoch survives
    /// below it. Without such a boundary, the deterministic horizon is the
    /// newest `limit` cards. Histories below the limit remain complete.
    ///
    /// Neither horizon may cut into [`MESSAGE_FLOOR`]: an agent that has said
    /// that many things still shows them all, however recently it compacted
    /// and however loud its peers are.
    #[must_use]
    pub fn retention_start(activities: &[Self], limit: usize) -> usize {
        if activities.len() < limit {
            return 0;
        }
        let floor = Self::message_floor_start(activities, MESSAGE_FLOOR);
        let capacity_floor = activities.len().saturating_sub(limit);
        activities[capacity_floor..]
            .iter()
            .position(|activity| *activity == Self::Compaction)
            .map_or(capacity_floor, |boundary| capacity_floor + boundary)
            .min(floor)
    }
}

/// Agent activity projected from one durable transcript record.
///
/// This is an app view over tape evidence, not an authoritative fact. Prose
/// is the default wire shape; tool activity carries its explicit kind and
/// uses `text` for the concise operator-facing preview.
/// The lane says where the transcript belongs, the author is the exact
/// occupant that emitted it, and the locus lets consumers deduplicate
/// reconnects.
///
/// Prose outlives the seat that produced it, so [`Self::author`] names an
/// occupant that need not still be seated anywhere, and a consumer routing a
/// reply may not assume otherwise. When exactly one current member is occupied
/// by the author, a reply follows that member in whichever lane it now occupies.
/// With zero matches the reply belongs to [`Self::lane`], the lane this activity
/// was carried in; multiple matches are ambiguous and resolve there as well.
/// Neither case may select another member by row order, so no unrelated occupant
/// is handed direction it did not earn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMessage {
    /// Lane whose selected transcript carried the activity.
    pub lane: CanonicalUuid,
    /// Occupant identity that authored the activity.
    pub author: CanonicalUuid,
    /// Durable transcript record activity supplying this view.
    pub locus: MessageLocus,
    /// Authored block identity shared by streamed and settled prose.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub prose_origin: Option<MessageProseOrigin>,
    /// Vendor record instant that authored this visible activity.
    pub authored: Instant,
    /// Whether `authored` was synthesized because the vendor record omitted time.
    #[serde(default, skip_serializing_if = "is_false")]
    pub authored_synthetic: bool,
    /// Durable Spice observation instant when the vendor record omitted time.
    ///
    /// This is distinct from `authored`: the synthetic authored value remains
    /// a stable ordering identity, while this instant is the wall-clock time at
    /// which the transcript batch containing the message crossed the journal.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub observed: Option<Instant>,
    /// Visible activity kind. Its absent wire spelling means prose.
    #[serde(default, skip_serializing_if = "AgentActivityKind::is_prose")]
    pub activity: AgentActivityKind,
    /// Compaction strategy the captured completion announced for a compaction
    /// activity; absent when the announcement named none.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub compaction_method: Option<String>,
    /// Whether the vendor explicitly marked this prose as the final answer.
    #[serde(default, skip_serializing_if = "is_false")]
    pub final_answer: bool,
    /// Resident-authenticated acknowledgment disposition for this answer.
    ///
    /// The absent wire spelling means the message answered no owned directive.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub acknowledgement: Option<MessageAcknowledgement>,
    /// Visible assistant prose, provider failure, concise tool preview, or
    /// boundary label.
    pub text: String,
    /// Bytes of text this activity returned into its author's model context: a
    /// tool result's rendered output, whatever length [`Self::text`] previews.
    /// Its absent wire spelling is zero, which every tool call, progress
    /// update, and non-tool activity carries.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub context_bytes: u64,
    /// Structured provider cause reported for a failed activity.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub provider_error: Option<AgentProviderError>,
    /// Canonical target for quoting this card.
    ///
    /// Durable assistant prose and provider failures are quoteable. Presence
    /// and boundary rows omit this field because they do not resolve as
    /// authored transcript text.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub quote_target: Option<QuoteTarget>,
    /// Resident-authenticated directive contexts embedded in this card.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directive_contexts: Vec<MessageDirective>,
    /// Authored messages presented as outgoing quote blocks in
    /// [`Self::text`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sends: Vec<MessageSend>,
    /// Recursively resolved fixed-label quote links found in [`Self::text`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quotes: Vec<QuotePresentation>,
    /// Optional resident-classified automatic Speak delivery.
    ///
    /// Its absence means this activity must remain silent in Speak mode.
    /// Narrate mode continues to use [`Self::text`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub automatic_speech: Option<AutomaticSpeech>,
    /// Admitted media carried beside this exact transcript event.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<crate::Media>,
    /// Exact quote targets for `media`, in the same source order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media_quotes: Vec<crate::MediaQuote>,
    /// Authenticated display-only replacements for exact Markdown image destinations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markdown_images: Vec<crate::MarkdownImageBinding>,
}

impl AgentMessage {
    /// Millisecond used to display and order this message on app surfaces.
    ///
    /// A durable Spice observation is preferred for timestamp-less vendor
    /// records. The synthetic authored instant remains the stable fallback and
    /// its provenance stays explicit in [`Self::authored_synthetic`].
    #[must_use]
    pub fn display_millis(&self) -> i64 {
        self.observed
            .as_ref()
            .unwrap_or(&self.authored)
            .get()
            .as_millisecond()
    }

    /// Total display chronology shared by every message producer and consumer.
    ///
    /// The complete source locus breaks equal-time ties. In particular,
    /// `length` cannot be omitted: source replacement can retain an offset while
    /// changing the record extent, and every projection must still make the
    /// same deterministic ordering decision.
    #[must_use]
    pub fn chronology_key(&self) -> (i64, CanonicalUuid, u64, u64, u64, u32) {
        (
            self.display_millis(),
            self.lane,
            self.locus.member,
            self.locus.offset,
            self.locus.length,
            self.locus.event,
        )
    }
}

const fn is_false(value: &bool) -> bool {
    !*value
}

const fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[cfg(test)]
mod message_retention_tests {
    use super::*;

    fn message(length: u64) -> AgentMessage {
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
                offset: 20,
                length,
                event: 0,
            },
            authored: "2030-04-12T18:44:09.271Z"
                .parse()
                .expect("fixture authored instant"),
            authored_synthetic: false,
            observed: None,
            activity: AgentActivityKind::Prose,
            compaction_method: None,
            final_answer: false,
            acknowledgement: None,
            text: "fixture".to_owned(),
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
    fn bounded_history_starts_at_the_oldest_compaction_inside_its_capacity() {
        let mut activities = vec![AgentActivityKind::Prose; 300];
        activities[20] = AgentActivityKind::Compaction;
        activities[80] = AgentActivityKind::Compaction;
        activities[220] = AgentActivityKind::Compaction;

        assert_eq!(AgentActivityKind::retention_start(&activities, 256), 80);
        assert_eq!(AgentActivityKind::retention_start(&activities, 32), 268);
        assert_eq!(
            AgentActivityKind::retention_start(&activities[..200], 256),
            0,
        );
        assert_eq!(
            AgentActivityKind::retention_start(&activities[..256], 256),
            20,
        );
        assert_eq!(
            AgentActivityKind::retention_start(&activities, 0),
            300 - MESSAGE_FLOOR,
            "no budget is small enough to take an agent's newest messages",
        );
        assert!(AgentActivityKind::Turn.is_boundary());
        assert!(AgentActivityKind::Compaction.is_boundary());
        assert!(!AgentActivityKind::Prose.is_boundary());
        assert!(!AgentActivityKind::Reasoning.is_boundary());
    }

    /// Markers stand between messages; only messages pay the floor.
    #[test]
    fn the_message_floor_counts_messages_and_not_the_markers_among_them() {
        let mut activities = vec![AgentActivityKind::Prose; 40];
        for marker in [5, 12, 30] {
            activities[marker] = AgentActivityKind::Turn;
        }
        activities[35] = AgentActivityKind::Compaction;
        let start = AgentActivityKind::message_floor_start(&activities, MESSAGE_FLOOR);
        assert_eq!(
            activities[start..]
                .iter()
                .filter(|activity| !activity.is_boundary())
                .count(),
            MESSAGE_FLOOR,
        );
        assert!(!activities[start].is_boundary());

        assert_eq!(
            AgentActivityKind::message_floor_start(&activities[..10], MESSAGE_FLOOR),
            0,
            "a lane below the floor answers with everything it has said",
        );
        let exact = vec![AgentActivityKind::Prose; MESSAGE_FLOOR];
        assert_eq!(
            AgentActivityKind::message_floor_start(&exact, MESSAGE_FLOOR),
            0,
        );
        let sliding = vec![AgentActivityKind::Prose; MESSAGE_FLOOR + 1];
        assert_eq!(
            AgentActivityKind::message_floor_start(&sliding, MESSAGE_FLOOR),
            1,
            "the floor slides with the newest message",
        );
        let markers = vec![AgentActivityKind::Turn; 4];
        assert_eq!(
            AgentActivityKind::message_floor_start(&markers, MESSAGE_FLOOR),
            markers.len(),
            "markers alone hold no floor open",
        );
    }

    #[test]
    fn reasoning_is_a_presence_activity_spelled_on_the_wire() {
        assert!(!AgentActivityKind::Reasoning.is_visible());
        assert!(!AgentActivityKind::Reasoning.is_prose());
        assert!(!AgentActivityKind::Tool.is_visible());
        assert!(AgentActivityKind::Prose.is_visible());
        assert!(AgentActivityKind::Turn.is_visible());
        assert!(AgentActivityKind::Compaction.is_visible());
        let mut message = message(7);
        message.activity = AgentActivityKind::Reasoning;
        let encoded = serde_json::to_string(&message).expect("reasoning message encodes");
        assert!(encoded.contains("\"activity\":\"reasoning\""), "{encoded}");
        let decoded: AgentMessage =
            serde_json::from_str(&encoded).expect("reasoning message decodes");
        assert_eq!(decoded.activity, AgentActivityKind::Reasoning);
    }

    #[test]
    fn message_chronology_uses_observation_time_and_the_complete_source_locus() {
        let shorter = message(10);
        let mut longer = message(11);
        assert!(shorter.chronology_key() < longer.chronology_key());

        longer.observed = Some(
            "2030-04-12T18:45:00.000Z"
                .parse()
                .expect("fixture observation instant"),
        );
        assert_eq!(longer.chronology_key().0, longer.display_millis());
        assert!(shorter.chronology_key() < longer.chronology_key());
    }
}

/// A resumable journal position bound to one resident generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamCursor {
    /// Resident generation that assigned the position.
    pub epoch: CanonicalUuid,
    /// Last position the client admitted, or the exact packed continuation.
    pub seq: Seq,
}

/// Subjects the resident can publish on the authenticated app socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ViewSubject {
    /// Canonical fact envelopes in durable journal order.
    Facts,
    /// Current executable-surface trust heads.
    Trust,
    /// Agent-authored prose and tool moments projected from transcript capture.
    Messages,
    /// Redacted provider quota sampled by the resident.
    Quota,
    /// Current room and preparation authority facts at the subscription
    /// horizon; later authority arrives as live facts.
    Rooms,
}

/// One strict client-to-resident stream request.
///
/// `stream` is the closed discriminant. Unknown fields and variants fail the
/// sole wire decoder rather than being ignored or upcast.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "stream", rename_all = "kebab-case", deny_unknown_fields)]
pub enum StreamRequest {
    /// Begin one subscription, optionally after a same-generation cursor.
    Subscribe {
        /// Current wire version.
        v: WireVersion,
        /// Correlation identity for this subscription.
        id: CanonicalUuid,
        /// Resident generation being addressed.
        epoch: CanonicalUuid,
        /// Non-empty, duplicate-free subject set.
        subjects: Vec<ViewSubject>,
        /// Last position already admitted on a reconnect.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        after: Option<StreamCursor>,
    },
    /// Continue one packed subject at its exact explicit cursor.
    Continue {
        /// Current wire version.
        v: WireVersion,
        /// Correlation identity for this request.
        id: CanonicalUuid,
        /// Resident generation being addressed.
        epoch: CanonicalUuid,
        /// Subject whose preceding packed answer overflowed.
        subject: ViewSubject,
        /// Exact continuation reported by that answer.
        cursor: StreamCursor,
    },
    /// Walk one causal thread from an explicit generation-bound position.
    Thread {
        /// Current wire version.
        v: WireVersion,
        /// Correlation identity for this request.
        id: CanonicalUuid,
        /// Resident generation being addressed.
        epoch: CanonicalUuid,
        /// First causal position for this page.
        cursor: StreamCursor,
    },
    /// Fetch one coordinated interval behind explicit lane frontiers.
    MessageHistory {
        /// Current wire version.
        v: WireVersion,
        /// Correlation identity for this request.
        id: CanonicalUuid,
        /// Resident generation being addressed.
        epoch: CanonicalUuid,
        /// Non-empty, duplicate-free lane frontiers from one transcript generation.
        frontiers: Vec<MessageHistoryCursor>,
    },
    /// Ask the resident to prove it is still reading this socket.
    ///
    /// A half-open socket stays open and simply stops delivering, so silence on
    /// its own is not evidence of a fault: a quiet board is silent too. This is
    /// what separates the two. It carries no cursor and asks for no state, so
    /// the answer means one thing only -- the resident read this frame and
    /// wrote back on the same socket.
    ///
    /// The transport's own ping cannot serve here. Browsers expose no way to
    /// send a WebSocket ping frame, so the probe a browser can send has to live
    /// above the transport, which is why this is a stream request rather than a
    /// control frame.
    Ping {
        /// Current wire version.
        v: WireVersion,
        /// Correlation identity for this probe.
        id: CanonicalUuid,
        /// Resident generation being addressed.
        epoch: CanonicalUuid,
    },
}

impl StreamRequest {
    /// Correlation identity carried by every request variant.
    #[must_use]
    pub const fn id(&self) -> CanonicalUuid {
        match self {
            Self::Subscribe { id, .. }
            | Self::Continue { id, .. }
            | Self::Thread { id, .. }
            | Self::MessageHistory { id, .. }
            | Self::Ping { id, .. } => *id,
        }
    }

    /// Admit the request's version, epoch, and any embedded cursor epoch.
    ///
    /// Shape failures, including zero cursors and unknown fields, have already
    /// failed decoding before this method can be called.
    pub fn admit(&self, resident_epoch: CanonicalUuid) -> Result<(), StreamAdmissionError> {
        let (wire, epoch, cursor) = match self {
            Self::Subscribe {
                v, epoch, after, ..
            } => (*v, *epoch, *after),
            Self::Continue {
                v, epoch, cursor, ..
            }
            | Self::Thread {
                v, epoch, cursor, ..
            } => (*v, *epoch, Some(*cursor)),
            Self::MessageHistory { v, epoch, .. } | Self::Ping { v, epoch, .. } => {
                (*v, *epoch, None)
            }
        };
        if !wire.is_current() {
            return Err(StreamAdmissionError::Wire(Refusal::bare(
                RefusalDiagnostic::new(
                    "stream::wire-mismatch",
                    None,
                    Some(WireVersion::CURRENT.to_string()),
                    Some(wire.to_string()),
                ),
            )));
        }
        if epoch != resident_epoch {
            return Err(StreamAdmissionError::Epoch(stale_epoch(
                epoch,
                resident_epoch,
            )));
        }
        if let Some(cursor) = cursor
            && cursor.epoch != resident_epoch
        {
            return Err(StreamAdmissionError::Cursor(stale_epoch(
                cursor.epoch,
                resident_epoch,
            )));
        }
        Ok(())
    }
}

fn stale_epoch(observed: CanonicalUuid, resident: CanonicalUuid) -> Fault {
    Fault {
        class: FaultClass::Protocol,
        reason: "stream::epoch-mismatch".to_owned(),
        detail: format!("stream addressed epoch {observed}; resident epoch is {resident}"),
    }
}

/// A decoded stream request addressed the wrong schema or generation.
///
/// identity-law: non-crossing - local validation state converted to a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamAdmissionError {
    /// The peer did not speak the resident's one current wire.
    Wire(Refusal),
    /// The request addressed a replaced resident generation.
    Epoch(Fault),
    /// An embedded continuation belongs to another generation.
    Cursor(Fault),
}

/// One current trust head together with the fact position that established it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustView {
    /// Fact position stored beside this head.
    pub position: Seq,
    /// Current head body.
    pub head: TrustHead,
}

/// One credential-free fact rendered for an authenticated app view.
///
/// Journal facts retain their complete durable identity internally. This
/// fold carries the same version, position, instant, actor, origin,
/// cause, kind, and non-credential body fields, but `lane::seated` deliberately
/// omits the capability that is valid only in [`crate::Hello`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StreamFact(Envelope<StreamFactBody>);

impl StreamFact {
    /// Wire version stored with the durable fact.
    #[must_use]
    pub const fn wire(&self) -> WireVersion {
        self.0.v
    }

    /// Durable journal position, unchanged by presentation.
    #[must_use]
    pub const fn seq(&self) -> Seq {
        self.0.seq
    }

    /// Commit instant, unchanged by presentation.
    #[must_use]
    pub const fn at(&self) -> &Instant {
        &self.0.at
    }

    /// Principal that caused the durable append.
    #[must_use]
    pub const fn actor(&self) -> &Actor {
        &self.0.actor
    }

    /// Root attribution carried through the durable append.
    #[must_use]
    pub const fn origin(&self) -> &Origin {
        &self.0.origin
    }

    /// Immediately provoking fact, if this is not a causal root.
    #[must_use]
    pub const fn cause(&self) -> Option<Seq> {
        self.0.cause
    }

    /// Closed fact kind retained by the credential-free fold.
    #[must_use]
    pub const fn kind(&self) -> FactKind {
        self.0.fact.kind
    }

    /// Strict kind-specific body fields retained for presentation.
    #[must_use]
    pub const fn body(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.0.fact.body
    }
    /// Durable task affected by this task- or claim-family fact.
    #[must_use]
    pub fn task(&self) -> Option<TaskStamp> {
        let Some(("task" | "claim", _)) = self.kind().as_str().split_once("::") else {
            return None;
        };
        self.body().get("task")?.as_str()?.parse().ok()
    }

    /// Decode this row through the DTO registered for `F`, when its kind agrees.
    ///
    /// Stream facts validate their complete body during admission, so a
    /// matching kind must read back through the same registered DTO. The stream
    /// omits the `lane::seated` socket credential, so that kind is refused at
    /// compile time; read it through [`Self::lane_seated`].
    #[must_use]
    pub fn read<F: Body>(&self) -> Option<F> {
        const {
            assert!(
                !matches!(F::KIND, FactKind::LaneSeated),
                "stream lane::seated omits its credential; read it with StreamFact::lane_seated",
            );
        }
        if self.kind() != F::KIND {
            return None;
        }
        Some(
            crate::fact::read_registered_body(self.body())
                .expect("a stream body validated on decode reads through its registered DTO"),
        )
    }

    /// Decode the public fields of a `lane::seated` row.
    #[must_use]
    pub fn lane_seated(&self) -> Option<StreamLaneSeated> {
        if self.kind() != FactKind::LaneSeated {
            return None;
        }
        Some(
            StreamLaneSeated::deserialize(serde::de::value::MapDeserializer::new(
                self.body()
                    .iter()
                    .map(|(field, value)| (field.as_str(), value)),
            ))
            .expect("a stream lane::seated body validated on decode reads through its stream DTO"),
        )
    }
}

impl From<Envelope<AnyFact>> for StreamFact {
    fn from(envelope: Envelope<AnyFact>) -> Self {
        let Envelope {
            v,
            seq,
            at,
            actor,
            origin,
            cause,
            fact,
        } = envelope;
        let kind = fact.kind();
        let body = stream_fact_body(&fact);
        Self(Envelope {
            v,
            seq,
            at,
            actor,
            origin,
            cause,
            fact: StreamFactBody { kind, body },
        })
    }
}

fn stream_fact_body(fact: &AnyFact) -> serde_json::Map<String, serde_json::Value> {
    let Some(durable) = fact.read::<crate::body::LaneSeated>() else {
        return fact.body().clone();
    };
    let crate::body::LaneSeated {
        lane,
        team,
        capability: _,
        name,
        worktree,
        life,
        vendor,
        model,
        effort,
    } = durable;
    let visible = StreamLaneSeated {
        lane,
        team,
        name,
        worktree,
        life,
        vendor,
        model,
        effort,
    };
    let serde_json::Value::Object(body) =
        serde_json::to_value(visible).expect("stream lane::seated body encodes")
    else {
        unreachable!("stream lane::seated is an object DTO");
    };
    body
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct StreamFactBody {
    kind: FactKind,
    #[serde(flatten)]
    body: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct UncheckedStreamFactBody {
    kind: FactKind,
    #[serde(flatten)]
    body: serde_json::Map<String, serde_json::Value>,
}

impl<'de> Deserialize<'de> for StreamFactBody {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let unchecked = UncheckedStreamFactBody::deserialize(deserializer)?;
        if unchecked.kind == FactKind::LaneSeated {
            validate_exact_body::<StreamLaneSeated>(&unchecked.body)
                .map_err(serde::de::Error::custom)?;
        } else {
            unchecked
                .kind
                .decode_body(&unchecked.body)
                .map(drop)
                .map_err(serde::de::Error::custom)?;
        }
        Ok(Self {
            kind: unchecked.kind,
            body: unchecked.body,
        })
    }
}

/// Public fields of `lane::seated`; the socket credential is intentionally absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamLaneSeated {
    /// Seated lane.
    pub lane: CanonicalUuid,
    /// Team the lane is seated in.
    pub team: CanonicalUuid,
    /// Operator-chosen display name.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub name: Option<String>,
    /// Worktree the lane runs in.
    pub worktree: String,
    /// Lane life policy.
    pub life: String,
    /// Vendor that runs the lane.
    pub vendor: Name,
    /// Requested vendor model.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub model: Option<String>,
    /// Requested reasoning effort.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub effort: Option<String>,
}

fn validate_exact_body<T>(
    body: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), serde_json::Error>
where
    T: Serialize + serde::de::DeserializeOwned,
{
    let supplied = serde_json::Value::Object(body.clone());
    let decoded: T = serde_json::from_value(supplied.clone())?;
    let registered = serde_json::to_value(decoded)?;
    if supplied == registered {
        Ok(())
    } else {
        Err(<serde_json::Error as serde::de::Error>::custom(
            "body fields do not exactly match the stream DTO",
        ))
    }
}

/// One strict resident-to-client stream frame.
///
/// Successful packed answers keep their overflow, continuation, and horizon.
/// Live facts carry their own durable sequence. Lag and shutdown frames carry
/// the last admitted cursor so reconnect has one explicit continuation point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "stream", rename_all = "kebab-case", deny_unknown_fields)]
pub enum StreamFrame {
    /// Initial or continued fact page.
    Facts {
        v: WireVersion,
        id: CanonicalUuid,
        epoch: CanonicalUuid,
        packed: Packed<StreamFact>,
    },
    /// Initial, continued, or refreshed trust-head page.
    Trust {
        v: WireVersion,
        id: CanonicalUuid,
        epoch: CanonicalUuid,
        packed: Packed<TrustView>,
    },
    /// Initial or continued page of the facts behind current room authority,
    /// in journal order through the subscription horizon.
    Rooms {
        v: WireVersion,
        id: CanonicalUuid,
        epoch: CanonicalUuid,
        packed: Packed<StreamFact>,
    },
    /// Current bounded transcript-activity snapshot.
    Messages {
        v: WireVersion,
        id: CanonicalUuid,
        epoch: CanonicalUuid,
        /// Maximum number of newest visible cards retained for each lane by this generation.
        limit: u32,
        /// Atomic snapshot and explicit current-lane frontiers.
        generation: TranscriptGeneration,
    },
    /// One coordinated retained-history interval.
    MessageHistory {
        v: WireVersion,
        id: CanonicalUuid,
        epoch: CanonicalUuid,
        /// Exact frontiers this interval extends.
        requested: Vec<MessageHistoryCursor>,
        /// Atomic globally ordered interval and next requested-lane frontiers.
        generation: TranscriptGeneration,
    },
    /// One causal thread page.
    Thread {
        v: WireVersion,
        id: CanonicalUuid,
        epoch: CanonicalUuid,
        packed: Packed<StreamFact>,
    },
    /// One later durable fact for an active subscription.
    Fact {
        v: WireVersion,
        id: CanonicalUuid,
        epoch: CanonicalUuid,
        fact: StreamFact,
    },
    /// One later atomic transcript cohort.
    MessageGeneration {
        v: WireVersion,
        id: CanonicalUuid,
        epoch: CanonicalUuid,
        generation: TranscriptGeneration,
    },
    /// Current redacted provider quota state for a new subscription.
    Quota {
        v: WireVersion,
        id: CanonicalUuid,
        epoch: CanonicalUuid,
        generation: QuotaGeneration,
    },
    /// One later redacted provider quota generation.
    QuotaGeneration {
        v: WireVersion,
        id: CanonicalUuid,
        epoch: CanonicalUuid,
        generation: QuotaGeneration,
    },
    /// The resident read a [`StreamRequest::Ping`] and answered on that socket.
    ///
    /// The repository configuration revision is an invalidation token, not
    /// configuration content or mutation authority. An unchanged revision
    /// does not require a board reread or redraw.
    Pong {
        v: WireVersion,
        id: CanonicalUuid,
        epoch: CanonicalUuid,
        config_revision: CanonicalUuid,
    },
    /// The authenticated principal cannot admit the request as written.
    Refusal {
        v: WireVersion,
        id: CanonicalUuid,
        epoch: CanonicalUuid,
        refusal: Refusal,
    },
    /// The request or resident stream faulted.
    Fault {
        v: WireVersion,
        epoch: CanonicalUuid,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        id: Option<CanonicalUuid>,
        fault: Fault,
    },
    /// Bounded broadcast retention was exceeded; reconnect after this cursor.
    Overflow {
        v: WireVersion,
        id: CanonicalUuid,
        epoch: CanonicalUuid,
        after: StreamCursor,
        dropped: u64,
    },
    /// The resident generation is shutting down at this admitted cursor.
    Closing {
        v: WireVersion,
        id: CanonicalUuid,
        epoch: CanonicalUuid,
        after: StreamCursor,
    },
}
