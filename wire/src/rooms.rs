//! Native task-room identities and authority DTOs.
//!
//! A room is never assigned another identity: its anchor task's [`TaskStamp`]
//! is the room identity. The types here add only the authority that a task does
//! not already own: source links, decisions, and exact output authorization.

use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

mod contribution;
mod conversation;
mod cycle;
mod decision;
mod maturity;
mod notice;
mod stop;

pub use contribution::*;
pub use conversation::*;
pub use cycle::*;
pub use decision::*;
pub use maturity::*;
pub use notice::*;
pub use stop::*;

use crate::{
    CanonicalUuid, Digest, MessageLocus, Seq, SequenceCode, TaskHandle, TaskPreparation, TaskStamp,
    TranscriptSourceHead,
};

/// The identity of a room: exactly the anchor task's durable birth stamp.
pub type RoomAnchor = TaskStamp;

/// A current nine-digit native moment stamp for non-task room authority such
/// as publications. This wrapper reuses the native vowel-free base-52
/// microsecond codec while refusing a bare-decimal spelling; no UUID or
/// decimal protocol handle is introduced.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RoomAuthorityStamp(TaskStamp);

/// Text that does not spell a current room-authority stamp.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("room authority stamp must be nine native base-52 characters including a letter: {0:?}")]
pub struct RoomAuthorityStampError(String);

/// Epoch moment that cannot mint a non-decimal current room-authority stamp.
#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
pub enum RoomAuthorityStampMintError {
    /// Moment is outside the native nine-digit microsecond space.
    #[error(transparent)]
    Moment(#[from] crate::TaskStampMintError),
    /// Its canonical moment spelling would be an ambiguous bare decimal.
    #[error("room authority moment would mint a bare decimal handle: {0}")]
    BareDecimal(i64),
}

impl RoomAuthorityStamp {
    /// Mint one current room-authority identity from Unix microseconds.
    pub fn for_micros(epoch_micros: i64) -> Result<Self, RoomAuthorityStampMintError> {
        let stamp = TaskStamp::for_micros(epoch_micros)?;
        if stamp.as_str().bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(RoomAuthorityStampMintError::BareDecimal(epoch_micros));
        }
        Ok(Self(stamp))
    }

    /// Canonical stored spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Unix microsecond encoded by this identity.
    #[must_use]
    pub fn micros(&self) -> i64 {
        self.0.micros()
    }
}

impl FromStr for RoomAuthorityStamp {
    type Err = RoomAuthorityStampError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let stamp = text
            .parse::<TaskStamp>()
            .map_err(|_| RoomAuthorityStampError(text.to_owned()))?;
        if text.len() != TaskStamp::WIDTH || text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(RoomAuthorityStampError(text.to_owned()));
        }
        Ok(Self(stamp))
    }
}

impl fmt::Display for RoomAuthorityStamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for RoomAuthorityStamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for RoomAuthorityStamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

/// Stable identity of one operator decision under a controlling task.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRef {
    /// Plan or Design whose disposition was decided.
    pub controller: TaskStamp,
    /// Minimal-width native decision sequence under that controller.
    pub decision: SequenceCode,
}

/// Exact subject of one guarded proposal-authority mutation.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ProposalRef {
    /// One decision under a controlling task.
    Decision { decision: DecisionRef },
}

/// Half-open UTF-8 byte range within one decoded event's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextRange {
    /// First contributed byte.
    pub start: u64,
    /// One past the last contributed byte.
    pub end: u64,
}

/// Exact contributed bytes of one decoded transcript event.
///
/// The transcript owns the bytes; this names the accounted generation, the
/// durable vendor record and decoded event, and the range of that event's text
/// contributed, so a reader shares only those bytes and never the transcript.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptHunk {
    /// Accounted logical transcript source.
    pub head: TranscriptSourceHead,
    /// Exact durable vendor record and event.
    pub locus: MessageLocus,
    /// Contributed range of the event's decoded text.
    pub text: TextRange,
}

impl TranscriptHunk {
    /// Whether `other` names these exact bytes, wherever either head's
    /// accounted boundary stood when it was linked.
    #[must_use]
    pub fn same_bytes(&self, other: &Self) -> bool {
        self.head.key() == other.head.key()
            && self.head.source == other.head.source
            && self.locus == other.locus
            && self.text == other.text
    }
}

/// A full Git object id: 40 lowercase hex digits for SHA-1 repositories, 64
/// for SHA-256 ones.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GitObjectId(String);

/// A spelling that is not a full lowercase Git object id.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid Git object id {0:?}; expected 40 or 64 lowercase hex digits")]
pub struct GitObjectIdError(String);

impl GitObjectId {
    /// The id as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The twelve-digit prefix a reader shows.
    #[must_use]
    pub fn short(&self) -> &str {
        &self.0[..12]
    }
}

impl FromStr for GitObjectId {
    type Err = GitObjectIdError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let hex = text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
        if hex && matches!(text.len(), 40 | 64) {
            Ok(Self(text.to_owned()))
        } else {
            Err(GitObjectIdError(text.to_owned()))
        }
    }
}

impl fmt::Display for GitObjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for GitObjectId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for GitObjectId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

/// Inclusive 1-based line range of one file; `first` is at least 1 and never
/// after `last`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "LineSpan")]
pub struct LineRange {
    first: u32,
    last: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LineSpan {
    first: u32,
    last: u32,
}

impl TryFrom<LineSpan> for LineRange {
    type Error = String;

    fn try_from(span: LineSpan) -> Result<Self, Self::Error> {
        Self::new(span.first, span.last)
            .ok_or_else(|| format!("invalid line range {}-{}", span.first, span.last))
    }
}

impl LineRange {
    /// The range `first..=last`, when `first` is at least 1 and not after
    /// `last`.
    #[must_use]
    pub const fn new(first: u32, last: u32) -> Option<Self> {
        if first >= 1 && first <= last {
            Some(Self { first, last })
        } else {
            None
        }
    }

    /// First line, 1-based.
    #[must_use]
    pub const fn first(self) -> u32 {
        self.first
    }

    /// Last line, inclusive.
    #[must_use]
    pub const fn last(self) -> u32 {
        self.last
    }
}

impl FromStr for LineRange {
    type Err = String;

    /// `L` or `L1-L2`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (first, last) = text.split_once('-').unwrap_or((text, text));
        first
            .parse()
            .ok()
            .zip(last.parse().ok())
            .and_then(|(first, last)| Self::new(first, last))
            .ok_or_else(|| format!("invalid line range {text:?}"))
    }
}

impl fmt::Display for LineRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.first == self.last {
            write!(formatter, "{}", self.first)
        } else {
            write!(formatter, "{}-{}", self.first, self.last)
        }
    }
}

/// One repository file, or a line range of it, at one commit. The Git object
/// store owns the bytes; `blob` is the object the commit's tree held at
/// `path` when the link was made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitSource {
    /// Commit whose tree names the file.
    pub commit: GitObjectId,
    /// Repository-relative path, `/`-separated.
    pub path: String,
    /// Blob at `path` in `commit`'s tree.
    pub blob: GitObjectId,
    /// Cited lines; absent cites the whole file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<LineRange>,
}

impl fmt::Display for GitSource {
    /// `PATH@SHORTCOMMIT[:LINES]`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}@{}", self.path, self.commit.short())?;
        match self.lines {
            Some(lines) => write!(formatter, ":{lines}"),
            None => Ok(()),
        }
    }
}

/// Exact immutable authority that owns room or candidate source bytes.
///
/// These values are links. Transcript bytes remain in tape, journal-authored
/// bytes remain at their fact, retained artifact bytes remain under their
/// digest, and repository source remains in the Git object store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RoomSource {
    /// Exact contributed bytes of one decoded event in an accounted transcript
    /// source generation.
    Transcript(TranscriptHunk),
    /// One immutable journal fact, rendered as native base-52 rather than a
    /// bare decimal sequence.
    Journal {
        /// Fact sequence owning the authored bytes.
        fact: SequenceCode,
    },
    /// One retained artifact owns the source bytes.
    Artifact {
        /// Content digest of the retained artifact.
        digest: Digest,
    },
    /// One lane contribution recorded in a Design room; the contribution's
    /// own fact owns its text and cited sources.
    Contribution {
        /// The Design anchor whose room holds the contribution.
        room: RoomAnchor,
        /// Native identity the contribution answered with.
        publication: RoomAuthorityStamp,
    },
    /// One repository file, or a line range of it, at one commit.
    Git(GitSource),
}

/// Why a requested room source cannot currently be resolved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RoomSourceUnavailable {
    /// The referenced authority does not exist at the read barrier.
    Missing {},
    /// The authority exists but is outside the caller's visibility.
    Hidden {},
    /// The authority exists but no longer holds the linked bytes.
    Changed {},
    /// The authority exists but cannot be read or decoded now.
    Unreadable {},
}

/// Source link projected by a read without turning absence into empty success.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RoomSourceView {
    /// The owning authority is available.
    Available {
        /// Exact immutable source link.
        source: RoomSource,
    },
    /// The requested authority is explicit but unavailable.
    Unavailable {
        /// Exact link that could not be resolved.
        source: RoomSource,
        /// Typed reason it could not be projected.
        unavailable: RoomSourceUnavailable,
    },
}

/// Content authority of one room publication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RoomPublicationContent {
    /// An authored observation, the operator's or a lane's, whose text the
    /// admitted journal fact owns directly.
    Authored { text: String },
    /// Agent text remains owned by its transcript or retained artifact.
    Linked { link: RoomSource },
}

/// Immutable room publication fact body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomPublished {
    /// Anchor task; this is the room identity.
    pub room: RoomAnchor,
    /// Native moment identity of this publication.
    pub publication: RoomAuthorityStamp,
    /// Original bytes or exact source link, never a copied conversation.
    pub content: RoomPublicationContent,
}

/// An authored assistant message recorded its body in one anchor task's room.
///
/// The packet's transcript owns the bytes. One packet records at most one
/// publication per room, in the same write as the peer messages its directed
/// routes open, so a packet is recorded and sent together or not at all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomMessaged {
    /// Anchor task; this is the room identity.
    pub room: RoomAnchor,
    /// Native moment identity of this publication.
    pub publication: RoomAuthorityStamp,
    /// The packet's exact body bytes in its author's transcript.
    pub hunk: TranscriptHunk,
    /// Occupant whose captured transcript authored the packet.
    pub author: CanonicalUuid,
    /// Directives this packet's `A#room` routes opened in the same write, in
    /// route order; empty for a room-only packet.
    pub directed: Vec<CanonicalUuid>,
}

/// A lane stopped listening to one anchor task's room.
///
/// The leave declines the Design request it postdates, the lane's newest about
/// this Design: it retires that request's whole ask and only the question items
/// the request listed, leaving unlisted items askable; only a later request
/// restores listening. Authored messages and contributions remain participation
/// records and neither create nor restore listening. It never frees a claim,
/// settles a delivery, or answers a question; native duties keep the lane
/// listening regardless.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomLeft {
    /// Anchor task; this is the room identity.
    pub room: RoomAnchor,
    /// Native moment identity of this publication.
    pub publication: RoomAuthorityStamp,
    /// Logical lane that left; always the authenticated actor.
    pub lane: CanonicalUuid,
    /// The lane's reason for leaving.
    pub clause: String,
}

/// A page of unread room activity a lane read landed in its own transcript,
/// advancing that logical lane's delivery cursor.
///
/// The hunk's transcript head names the lane, so the cursor belongs to the
/// logical lane rather than any one occupant: a renewed occupant continues
/// from it. Each delivery strictly advances the cursor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomDelivered {
    /// Room publication position through which the lane has now been told.
    pub through: SequenceCode,
    /// The landed page's closing line in the lane's transcript.
    pub hunk: TranscriptHunk,
}

/// Native room-only publication command body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomPost {
    /// Current rendered handle resolved to the room's anchor birth stamp.
    pub room: TaskHandle,
    /// Complete authored observation bytes.
    pub text: String,
}
/// Successful operator room-publication reply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomPostAnswer {
    /// Current rendered anchor handle.
    pub room: TaskHandle,
    /// Stable native identity of the recorded publication.
    pub publication: RoomAuthorityStamp,
    /// Current listener result observed when the publication was admitted.
    pub outcome: RoomPostOutcome,
}

/// Listener result for one recorded room publication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RoomPostOutcome {
    /// One or more lanes currently have a reason to receive room catch-up.
    RecordedWithListeners { listeners: Vec<RoomPostListener> },
    /// No lane currently has a reason to receive room catch-up.
    RecordedNoCurrentListener {
        /// Explicit operator action that can establish directed attention.
        next: RoomPostNext,
    },
}

/// Explicit operator action offered when a recorded room has no listener.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RoomPostNext {
    /// Start work or direct a named lane; recording alone does neither.
    StartOrDirect,
}

/// One current listener and the delivery state implied by its availability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomPostListener {
    /// Logical lane identity.
    pub lane: CanonicalUuid,
    /// Current resident topology state.
    pub availability: RoomListenerAvailability,
    /// What recording means for this listener without waking it.
    pub delivery: RoomListenerDelivery,
}

/// Current availability of one room listener.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RoomListenerAvailability {
    /// A current occupant is seated.
    Occupied,
    /// The logical lane is between occupants.
    Vacant,
    /// The lane is known but cannot currently receive.
    Unavailable { disposition: String },
    /// Current topology does not contain the referenced lane.
    Unknown,
}

/// Non-waking delivery consequence for one current listener.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RoomListenerDelivery {
    /// Catch-up may be presented at the lane's next safe boundary.
    AtSafeBoundary,
    /// Delivery remains waiting until the vacant lane next starts.
    WaitsForNextStart,
    /// Delivery remains pending until the lane becomes available.
    PendingAvailability,
}

/// Exact output composition an operator read and an authorization releases.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputRevision {
    /// Stable output identity.
    pub task: TaskStamp,
    /// Current statement head.
    pub statement: Seq,
    /// Current workflow head.
    pub workflow: Seq,
    /// Ordinary execution prerequisites in authored order.
    pub dependencies: Vec<TaskStamp>,
}

/// Exact selected execution authority created by its operator decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    try_from = "ExecutionAuthorizationWire",
    into = "ExecutionAuthorizationWire"
)]
pub struct ExecutionAuthorization {
    /// Operator decision that selected this exact output subset.
    pub decision: DecisionRef,
    /// Exact nonempty output set made eligible; no later output inherits it.
    pub selected_outputs: Vec<TaskStamp>,
    /// Exact revisions the decision read for the selected outputs.
    pub output_revisions: Vec<OutputRevision>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionAuthorizationWire {
    decision: DecisionRef,
    selected_outputs: Vec<TaskStamp>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    output_revisions: Vec<OutputRevision>,
}

impl TryFrom<ExecutionAuthorizationWire> for ExecutionAuthorization {
    type Error = &'static str;

    fn try_from(value: ExecutionAuthorizationWire) -> Result<Self, Self::Error> {
        if value.selected_outputs.is_empty() || !all_unique(&value.selected_outputs) {
            return Err("authorization outputs must be an exact nonempty unique set");
        }
        Ok(Self {
            decision: value.decision,
            selected_outputs: value.selected_outputs,
            output_revisions: value.output_revisions,
        })
    }
}

impl From<ExecutionAuthorization> for ExecutionAuthorizationWire {
    fn from(value: ExecutionAuthorization) -> Self {
        Self {
            decision: value.decision,
            selected_outputs: value.selected_outputs,
            output_revisions: value.output_revisions,
        }
    }
}

/// Immutable fact body recording exact selected execution authorization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionAuthorized {
    /// Exact authority that was admitted.
    pub authorization: ExecutionAuthorization,
}

/// Command body explicitly adopting an existing task into preparation control.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputAdopt {
    /// Existing output task; running or completed work remains reference-only.
    pub output: TaskHandle,
    /// Existing planning context from which native [`TaskPreparation`] is
    /// derived under task-head witnesses.
    pub preparation: TaskHandle,
}

/// Immutable fact body applying the same native preparation representation to
/// an adopted or promoted output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputControlled {
    /// Controlled output task.
    pub output: TaskStamp,
    /// Stage Zero native preparation/controller/construction association.
    pub preparation: TaskPreparation,
}

fn all_unique<T: PartialEq>(values: &[T]) -> bool {
    values
        .iter()
        .enumerate()
        .all(|(index, value)| !values[index + 1..].contains(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(text: &str) -> TaskStamp {
        text.parse().expect("task stamp")
    }

    fn authority(text: &str) -> RoomAuthorityStamp {
        text.parse().expect("room authority stamp")
    }

    #[test]
    fn room_and_proposal_identities_use_only_native_base52_authority() {
        let anchor = task("1kKRRsDR0");
        let room: RoomAnchor = anchor.clone();
        assert_eq!(room, anchor, "room identity is exactly its anchor task");

        let id = authority("1kKRRsDR1");
        assert_eq!(
            crate::decode::<RoomAuthorityStamp>(&crate::encode(&id).unwrap()).unwrap(),
            id
        );
        let minted = RoomAuthorityStamp::for_micros(1_700_000_000_000_000)
            .expect("current native authority stamp");
        assert_eq!(minted.as_str().len(), TaskStamp::WIDTH);
        assert_eq!(minted.as_str().parse(), Ok(minted));
        assert_eq!(
            RoomAuthorityStamp::for_micros(1),
            Err(RoomAuthorityStampMintError::BareDecimal(1))
        );
        for invalid in [
            "1kKRRsDR",
            "018f82d4-6f59-7d06-92e3-bce913116301",
            "123456789",
            "1kKRRsDRA",
        ] {
            assert!(invalid.parse::<RoomAuthorityStamp>().is_err(), "{invalid}");
        }
    }

    #[test]
    fn decisions_and_authorizations_retain_exact_referential_sets() {
        let decision_value = serde_json::json!({
            "decision": {"controller": "1kKRRsDR0", "decision": "4"},
            "source": {"kind": "journal", "fact": "3"},
            "basis": {
                "statement": 10,
                "room": 12,
                "outputs": [
                    {"task": "1kKRRsDR3", "statement": 8, "workflow": 9, "dependencies": []},
                    {"task": "1kKRRsDR4", "statement": 8, "workflow": 9, "dependencies": []}
                ]
            },
            "outcome": {
                "kind": "resolved",
                "outputs": [
                    {"output": "1kKRRsDR3", "verdict": "accepted", "comment": "meets the contract"},
                    {"output": "1kKRRsDR4", "verdict": "rejected"}
                ],
                "comment": "move the first plan forward"
            }
        });
        let decision: PreparationDecision = serde_json::from_value(decision_value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&decision).unwrap(), decision_value);
        let accepted = task("1kKRRsDR3");
        assert_eq!(
            decision.outcome.accepted_outputs().collect::<Vec<_>>(),
            vec![&accepted]
        );
        assert!(decision.outcome.carries_feedback());

        for outcome in [
            serde_json::json!({"kind": "returned", "reason": "revise the premise"}),
            serde_json::json!({"kind": "concluded"}),
            serde_json::json!({"kind": "collapsed", "reason": "the premise failed"}),
        ] {
            let mut value = decision_value.clone();
            value["outcome"] = outcome;
            let decision: PreparationDecision = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(decision).unwrap(), value);
        }

        for (path, invalid) in [
            (
                "outcome",
                serde_json::json!({"kind": "resolved", "outputs": []}),
            ),
            (
                "outcome",
                serde_json::json!({"kind": "returned", "reason": " "}),
            ),
            (
                "outcome",
                serde_json::json!({"kind": "collapsed", "reason": ""}),
            ),
            (
                "outcome",
                serde_json::json!({"kind": "resolved", "outputs": [
                    {"output": "1kKRRsDR3", "verdict": "accepted"},
                    {"output": "1kKRRsDR3", "verdict": "rejected"}
                ]}),
            ),
            (
                "outcome",
                serde_json::json!({"kind": "resolved", "outputs": [
                    {"output": "1kKRRsDR3", "verdict": "accepted", "comment": " "}
                ]}),
            ),
            (
                "basis",
                serde_json::json!({"statement": 10, "outputs": [
                    {"task": "1kKRRsDR3", "statement": 8, "workflow": 9, "dependencies": []},
                    {"task": "1kKRRsDR3", "statement": 8, "workflow": 9, "dependencies": []}
                ]}),
            ),
        ] {
            let mut value = decision_value.clone();
            value[path] = invalid;
            assert!(
                serde_json::from_value::<PreparationDecision>(value).is_err(),
                "invalid decision was admitted"
            );
        }

        let authorization_value = serde_json::json!({
            "decision": {"controller": "1kKRRsDR0", "decision": "4"},
            "selected_outputs": ["1kKRRsDR3"],
            "output_revisions": [
                {"task": "1kKRRsDR3", "statement": 8, "workflow": 9, "dependencies": []}
            ]
        });
        let authorization: ExecutionAuthorization =
            serde_json::from_value(authorization_value.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(authorization).unwrap(),
            authorization_value
        );
        let mut empty = authorization_value;
        empty["selected_outputs"] = serde_json::json!([]);
        assert!(serde_json::from_value::<ExecutionAuthorization>(empty).is_err());
    }
}
