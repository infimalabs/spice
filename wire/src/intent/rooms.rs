//! Bounded room command DTOs.

use serde::{Deserialize, Serialize};

use crate::rooms::{
    DesignFitGeneration, DesignNoticeEvidence, DesignReach, DesignSession, DesignSimilarity,
    RoomAnchor, RoomAuthorityStamp, RoomContributionKind, RoomSource, RoomSourceUnavailable,
    RoomSourceView,
};
use crate::{CanonicalUuid, OccupantAddress, SequenceCode, TaskHandle, TaskStamp};

/// One lane declines the Design request it owes in an anchor task's room.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomLeave {
    /// Current rendered handle of the anchor task.
    pub room: TaskHandle,
    /// Why the lane declines; recorded in the room.
    pub clause: String,
}

/// The recorded leave and each native duty that keeps the lane listening.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomLeaveAnswer {
    /// Current rendered anchor handle.
    pub room: TaskHandle,
    /// This command's leave publication.
    pub publication: RoomAuthorityStamp,
    /// Native duties leaving cannot end; empty when the lane stopped listening.
    pub kept: Vec<RoomListeningDuty>,
}

/// One native duty that keeps a lane listening to a room after it leaves.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "duty", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RoomListeningDuty {
    /// The lane holds an active claim on the anchor or related work.
    Work { task: TaskHandle },
    /// A live stop request addresses the lane's claim.
    Stop { key: String },
}

/// One lane contributes a typed record to an open Design room.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomContribute {
    /// Current rendered handle of the Design anchor.
    pub room: TaskHandle,
    /// What the contribution offers: a charge or a question.
    pub kind: RoomContributionKind,
    /// The question item a charge answers, resolved from the lane's handle
    /// against the witnessed statement head.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub question: Option<crate::rooms::StatementItem>,
    /// The lane's complete contributed text.
    pub text: String,
    /// Exact transcript, journal, or retained-artifact sources it cites.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<RoomSource>,
}

/// The recorded contribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    try_from = "RoomContributeAnswerWire",
    into = "RoomContributeAnswerWire"
)]
pub struct RoomContributeAnswer {
    /// Current rendered Design anchor handle.
    pub room: TaskHandle,
    /// Stable native identity of the recorded contribution.
    pub publication: RoomAuthorityStamp,
    /// What the contribution offers.
    pub kind: RoomContributionKind,
    /// The contribution as the exact [`RoomSource`] a candidate reference
    /// cites verbatim.
    pub source: RoomSource,
}

impl RoomContributeAnswer {
    /// The answer for `publication` in `room`, citing itself as a contribution.
    ///
    /// # Errors
    ///
    /// Refuses a handle that carries no birth stamp.
    pub fn new(
        room: TaskHandle,
        publication: RoomAuthorityStamp,
        kind: RoomContributionKind,
    ) -> Result<Self, crate::TaskStampError> {
        let source = RoomSource::Contribution {
            room: room.born()?,
            publication: publication.clone(),
        };
        Ok(Self {
            room,
            publication,
            kind,
            source,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoomContributeAnswerWire {
    room: TaskHandle,
    publication: RoomAuthorityStamp,
    kind: RoomContributionKind,
    source: RoomSource,
}

impl TryFrom<RoomContributeAnswerWire> for RoomContributeAnswer {
    type Error = String;

    fn try_from(value: RoomContributeAnswerWire) -> Result<Self, Self::Error> {
        let answer = Self::new(value.room, value.publication, value.kind)
            .map_err(|error| format!("contribution answer room: {error}"))?;
        if value.source != answer.source {
            return Err("contribution answer source names another contribution".to_owned());
        }
        Ok(answer)
    }
}

impl From<RoomContributeAnswer> for RoomContributeAnswerWire {
    fn from(value: RoomContributeAnswer) -> Self {
        Self {
            room: value.room,
            publication: value.publication,
            kind: value.kind,
            source: value.source,
        }
    }
}

/// Resolve one recorded contribution's cited sources at its own position,
/// and the text of the question item it names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomContributionRead {
    /// The Design room the contribution belongs to.
    pub room: RoomAnchor,
    /// The recorded contribution.
    pub publication: RoomAuthorityStamp,
}

/// Each cited source of one contribution, in the author's order, as a link
/// it can still follow or the typed reason it cannot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomContributionReadAnswer {
    /// The contribution read.
    pub publication: RoomAuthorityStamp,
    /// Its sources at the read barrier.
    pub sources: Vec<RoomSourceView>,
    /// What each available cited journal fact is, in citation order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<RoomFactSummary>,
    /// The text of the question item the contribution names, present only
    /// while the statement it witnessed is still the Design's current one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
}

/// What one cited journal fact is at the read barrier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomFactSummary {
    /// The cited fact.
    pub fact: SequenceCode,
    /// Its registered kind.
    pub kind: crate::FactKind,
    /// The task it concerns while that task exists: its task subject, else
    /// the Design room that holds a room record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<RoomFactTask>,
}

/// One existing task a cited fact concerns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomFactTask {
    /// Sole durable task identity.
    pub born: TaskStamp,
    /// Handle rendered from the task's current topic.
    pub handle: TaskHandle,
}

/// Read one conversation publication's text from its owning source: a
/// principal's from its journal fact, a lane's from its transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomConversationText {
    /// The admitted message, question, or answer.
    pub publication: RoomAuthorityStamp,
}

/// One conversation publication's text, or why its source cannot supply it,
/// with the direction its principal route concerns and the exact principal
/// recipient.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomConversationTextAnswer {
    /// The publication read.
    pub publication: RoomAuthorityStamp,
    /// Its text at the read barrier.
    pub content: RoomConversationContent,
    /// The direction its principal route concerns.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub direction: Option<RoomConcernedDirection>,
    /// The principal recipient of this publication's actor-addressed input,
    /// absent when it opens none.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub recipient: Option<crate::Actor>,
}

/// One principal direction as its steering head reads at the read barrier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomConcernedDirection {
    /// Its steering key.
    pub key: String,
    /// The text the principal sent.
    pub text: String,
    /// Images the principal attached, which the text's `attachment:`
    /// references name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<crate::Media>,
}

/// What a conversation publication's owning source supplied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RoomConversationContent {
    /// The exact authored text.
    Text { text: String },
    /// A journal fact or retained artifact the publication cites rather than
    /// quotes.
    Linked { source: RoomSource },
    /// The source does not supply it, for a typed reason.
    Unavailable { unavailable: RoomSourceUnavailable },
}

/// Score one candidate document against the current session profile addressed
/// by an exact resident lane address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomScore {
    /// Canonical short address parsed by the same type as a message recipient.
    pub address: OccupantAddress,
    /// Complete candidate document scored as one term-count input.
    pub text: String,
}

/// Which canonical view of the winning retained card produced a score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RoomScoreView {
    /// Task title, description, and terms.
    Statement,
    /// Task statement plus bounded successful validation.
    Completion,
    /// Whole cleaned assistant message.
    Message,
    /// One retained paragraph or list-item view.
    Passage { index: u8 },
}

/// One diagnostic comparison against a Design-awareness qualification floor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomScoreQualification {
    /// Reach whose production floor is being compared.
    pub reach: DesignReach,
    /// Exact current production floor.
    pub floor: DesignSimilarity,
    /// Whether the measured score clears this floor.
    pub qualifies: bool,
}

/// Read-only measurement of one document against one current session profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomScoreAnswer {
    /// Plain diagnostic rendering. It labels floors as qualifications, not an
    /// admission prediction.
    pub text: String,
    /// Address supplied by the caller.
    pub address: OccupantAddress,
    /// Logical lane resolved from the address's current seat.
    pub lane: CanonicalUuid,
    /// Current declared vendor session on that seat.
    pub session: DesignSession,
    /// Strongest finite cosine across all retained card views.
    pub score: DesignSimilarity,
    /// Shared statement-and-conversation fit that weighted the score.
    pub generation: DesignFitGeneration,
    /// Exact retained card that supplied the winning view.
    pub evidence: DesignNoticeEvidence,
    /// Canonical view of the winning card.
    pub view: RoomScoreView,
    /// Strongest shared explanation terms in production order.
    pub terms: Vec<String>,
    /// Diagnostic comparisons against all current production floors.
    pub qualifications: Vec<RoomScoreQualification>,
}
