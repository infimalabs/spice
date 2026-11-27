//! Command intent and its answer: the body a caller sends and the body it
//! gets back.
//!
//! WIRE fixes one rule here: the discriminant selects the body DTO, witness
//! schema, transition, and reply DTO together. The witness half already lives
//! in the registry; this module owns the other two, so every command kind
//! names exactly one intent and one answer.
//!
//! Mutation bodies carry caller intent only; the commit door assigns actor,
//! lane, epoch, journal position and resulting fact envelopes. Read bodies may
//! name bounds and previously observed pins without establishing that authority.
//!
//! Several kinds share one shape on purpose. Freeing and lapsing a claim both
//! name a task and nothing else, so they share
//! [`TaskRef`](crate::intent::TaskRef); renewal has its own shape because it
//! also names the requested lease duration.

// That link is crate-rooted, not bare, and has to stay that way. This module is
// documented from two places: `pub mod intent;` in `lib.rs` carries an outer
// comment as well, and rustdoc resolves the merged text in the scope that
// declaration sits in — the crate root, where a bare `TaskRef` is not in scope
// precisely because that outer comment's subject is that this module is
// deliberately not re-exported flat.

use serde::{Deserialize, Serialize};

use crate::Media;
use crate::body::{ConsentQueueCause, TranscriptLocus};
use crate::envelope::{Actor, Origin, Seq};
use crate::fact::FactKind;
use crate::head::{ResidentState, TapeLocus, TrustState};
use crate::ident::{CanonicalUuid, Digest, Instant};
use crate::witness::{Answer, Disposition, Name, TaskHandle};

mod board;
mod jobs;
pub use board::{
    BoardActorRoute, BoardAnswer, BoardClaim, BoardGraph, BoardHandout, BoardRead, BoardStatement,
    BoardTask, BoardedElsewhere, CompletionWait, DesignRequest, DesignRequestAsk, DesignStanding,
    DocLedger, NextWorkCounts, NextWorkState, NextWorkTask, ORIGIN_REQUEST_PREVIEW_LIMIT,
    OriginRequest, Positioned, RelatedTask, TASK_HISTORY_PAGE_LIMIT_MAX,
    TASK_HISTORY_QUERY_BYTES_MAX, TASK_RELATION_DEPTH_LIMIT, TASK_RELATION_EDGE_LIMIT,
    TASK_RELATION_FRONTIER_LIMIT, TASK_RELATION_NODE_LIMIT, TASK_RELATION_ROOT_LIMIT,
    TASK_TIMELINE_LANE_LIMIT, TASK_TIMELINE_PAGE_LIMIT_MAX, TaskAcknowledgment, TaskClaimContext,
    TaskDetail, TaskDetailAnswer, TaskDetailClosure, TaskDetailDeletion, TaskDetailDisposition,
    TaskDetailOrigin, TaskDetailPass, TaskDetailPost, TaskDetailReview, TaskDetailStep,
    TaskGitContext, TaskGitContextKind, TaskHistoryCursor, TaskHistoryPage, TaskLifecycleKind,
    TaskRelationEdge, TaskRelationFrontier, TaskRelationKind, TaskRelationNode,
    TaskRelationNodeRef, TaskRelationTruncation, TaskRelations, TaskRelationsAnswer, TaskStep,
    TaskTimeline, TaskTimelineAnswer, TaskTimelineEntry, TaskTimelineEvent, TaskTranscriptRecovery,
    TextAnswer, Unallocated, is_open_design, released_construction_root,
};
pub use jobs::{
    KeptJob, LaneJobAnswer, LaneJobCatalog, LaneJobPhase, LaneJobRef, LaneJobScope, LaneJobsAnswer,
    LaneJobsRead,
};
mod simple_answers;
pub use simple_answers::{RecognitionAnswer, TasteAnswer};
mod rack;
pub use rack::{
    RACK_DIRECTORY_ENTRIES_MAX, RACK_PATH_BYTES_MAX, RACK_TEXT_BYTES_MAX, RackAnswer, RackContent,
    RackEntry, RackEntryKind, RackIdentity, RackRead,
};
mod peer;
pub use peer::{LineAnswer, LineStanding, PeerAddressed, PeerLine, PeerLineInput, PeerSender};
mod rooms;
pub use rooms::{
    RoomConcernedDirection, RoomContribute, RoomContributeAnswer, RoomContributionRead,
    RoomContributionReadAnswer, RoomConversationContent, RoomConversationText,
    RoomConversationTextAnswer, RoomFactSummary, RoomFactTask, RoomLeave, RoomLeaveAnswer,
    RoomListeningDuty, RoomScore, RoomScoreAnswer, RoomScoreQualification, RoomScoreView,
};
mod steering;
pub use steering::{LineArm, LineArmAnswer, SteerLineAction, SteerLineAnswer, SteerLineControl};
mod trust;
pub use trust::{
    TrustBaseline, TrustDiffLine, TrustDocumentDiff, TrustLineEnding, TrustReportAnswer,
    TrustReportNext, TrustReportRead, TrustReportRuling, TrustShowAnswer,
};

mod task;
pub use task::*;

/// Append one durable directive to a lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteerSend {
    /// One operator submit action shared only by its target-specific directives.
    pub fanout: CanonicalUuid,
    /// Explicit lane the directive lands on. When absent, the resident selects
    /// the best eligible lane from retained lane-profile evidence.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub lane: Option<CanonicalUuid>,
    /// The intent being sent.
    pub message: String,
    /// Admitted browsable images retained with the directive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Media>,
    /// Intended target-specific siblings of this direction, including itself.
    ///
    /// Empty on a send addressed to one lane, on per-lane drafts submitted
    /// together, and on resident-routed sends.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recipients: Vec<crate::body::SteeringRecipient>,
}

/// Read pending direction visible to the authenticated principal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteerInbox {
    /// Lane selected by an operator, or the authenticated lane when absent.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub lane: Option<CanonicalUuid>,
}

/// Resolve one quote target against the resident's durable authority.
///
/// The spelling crosses unparsed: a malformed target is a typed
/// [`crate::QuoteUnavailable::Malformed`] answer, not a refused command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuoteRead {
    /// Target spelling, normally `spice://quote/v1/...`.
    pub target: String,
}

/// Name one directive and nothing else.
///
/// Resending and cancelling both identify a directive without adding to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectiveRef {
    /// The subject directive.
    pub directive: CanonicalUuid,
}

impl DirectiveRef {
    /// The exact failed directive authority required to recover this reference.
    #[must_use]
    pub fn recovery_witness(&self) -> crate::Witness {
        crate::Witness::Recoverable {
            steer: self.directive,
        }
    }
}

/// Decide one pending consent request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentDecide {
    /// Request being decided.
    pub request: CanonicalUuid,
    /// The decision.
    pub answer: Answer,
}

/// Read the pending consent queue and its current renewal budgets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentRead {}

/// Read the tracked consent policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentPolicy {}

/// Read the durable global app settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalSettingsRead {}

/// Replace the durable global app settings.
///
/// Principal presentation is each principal's recorded profile, never a setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalSettingsSet {
    /// Whether supported agent vendors launch in fast mode.
    pub fast_mode: bool,
}

/// Authoritative global app settings at one journal revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalSettingsAnswer {
    /// Whether supported agent vendors launch in fast mode.
    pub fast_mode: bool,
    /// Every born principal's display identity when this answer was produced,
    /// naming the connected human and every human author.
    pub principals: crate::principal::PrincipalDirectory,
}

/// Read the vendor choices this resident generation can seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VendorCatalog {}

/// One vendor a seat can select, with the launch values it defaults to.
///
/// The resident derives every row from the extension registry it dispatches
/// with and the vendor configuration it launches under, so an app that
/// offers these choices cannot offer a vendor the resident would refuse.
/// Model spellings stay open: no vendor publishes a catalogue of them and
/// each resolves its own, so the configured default is named here while any
/// other spelling remains valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VendorChoice {
    /// Registry name a seat carries.
    pub name: Name,
    /// Operator-facing label for the same vendor.
    pub label: String,
    /// Model spelling launched when a seat names none.
    pub default_model: String,
    /// Effort spelling launched when a seat names none.
    pub default_effort: String,
}

/// The vendor choices one resident generation can seat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VendorCatalogAnswer {
    /// Seatable vendors in registry order.
    pub vendors: Vec<VendorChoice>,
    /// Every reasoning effort a seat may name, weakest first. One closed
    /// vocabulary governs every vendor, which is the same set the resident
    /// admits a seat's effort against.
    pub efforts: Vec<String>,
}

/// Name one executable digest and nothing else.
///
/// Pending, granting, and revoking trust each name the exact digest; the
/// standing they move it to is the command kind, not a body field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustRef {
    /// The exact executable digest.
    pub digest: Digest,
    /// Canonical worktree that produced a digest entering quarantine.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub worktree: Option<String>,
}

/// Read the current lane table without mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneRead {}

/// Seat a lane in an operator-created worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneSeat {
    /// Topology revision observed before this seat was requested.
    pub topology_revision: u64,
    /// Independently steerable member seat being created.
    pub lane: CanonicalUuid,
    /// App team whose lane receives the member.
    pub team: CanonicalUuid,
    /// Operator-configured agent display name, absent when unconfigured.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub name: Option<String>,
    /// Worktree path the operator created.
    pub worktree: String,
    /// Closed lane-life spelling selected for this seat.
    pub life: String,
    /// Vendor selected for this seat.
    pub vendor: Name,
    /// Model spelling selected for this seat, absent when the vendor's
    /// configured model governs.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub model: Option<String>,
    /// Effort spelling selected for this seat, absent when the vendor's
    /// configured effort governs.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub effort: Option<String>,
    /// Existing vendor session identity to resume, absent for a fresh launch.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub resume: Option<String>,
}

/// Change one team's runtime switches, configuration name, fast mode, or members.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamConfigure {
    /// Topology revision observed before this configuration was requested.
    pub topology_revision: u64,
    /// Stable app team identifying the group.
    pub team: CanonicalUuid,
    /// Found a fresh team for `member`, with `team` naming its observed source.
    /// The resident allocates the new identity; ordinary moves never create
    /// a team from an unknown target identity.
    #[serde(default, skip_serializing_if = "is_false")]
    pub new_team: bool,
    /// One seated member to move into this team, or absent to preserve membership.
    ///
    /// The member's own ID means return to its solo team. Other own-ID teams
    /// cannot be joined; create an independent formation first. A source left
    /// with one member ends and that member returns to its own team too.
    /// Every other member and subscription stays authoritative atomically.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub member: Option<CanonicalUuid>,
    /// Replacement creation switch, or absent to preserve it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub drive: Option<bool>,
    /// Replacement automatic allocation switch, or absent to preserve it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub drain: Option<bool>,
    /// Bind a configured team name; mutually exclusive with `clear_name`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub name: Option<String>,
    /// Release the current configuration name.
    #[serde(default, skip_serializing_if = "is_false")]
    pub clear_name: bool,
    /// Replacement team fast-mode flag, or absent to preserve it.
    ///
    /// The flag is additive: every member runs fast while it is on, and
    /// turning it off leaves members fast whose own flag or the global
    /// setting still enables it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub fast_mode: Option<bool>,
}

/// One app team and the seated lanes it holds, in operator order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamArrangement {
    /// Stable app team identifying the group.
    pub team: CanonicalUuid,
    /// Every lane this team holds after the arrangement commits.
    pub lanes: Vec<CanonicalUuid>,
}

/// Rearrange every app team: their order, and which lanes each holds.
///
/// The operator drags one team lane to reorder or join teams. Every gesture
/// is the same durable commitment -- a complete restatement of the topology --
/// so authority never reconciles concurrent partial moves. A team not yet
/// standing must hold exactly the one lane it is named for, and a team of two
/// or more keeps at least two. Stating the whole arrangement makes every other
/// malformed request atomic: naming a lane twice, dropping one, or inventing
/// a lane is refused rather than half-applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamArrange {
    /// Topology revision of the complete arrangement being restated.
    pub topology_revision: u64,
    /// Every surviving team, in the order the operator arranged them.
    pub teams: Vec<TeamArrangement>,
}
/// Change a seated lane's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LaneAction {
    /// Gracefully end the current occupant and leave the seat vacant.
    Drain,
    /// Launch a new occupant into a vacant seat.
    Wake,
    /// Remove a vacant, claim-free lane from the topology.
    Remove,
}

/// Change a seated lane's lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneState {
    /// Lane whose state moves.
    pub lane: CanonicalUuid,
    /// Lifecycle action the operator selected.
    pub action: LaneAction,
    /// Fresh linked worktree required only by import.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub worktree: Option<String>,
}

/// Request or clear agent succession for one exact app team member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneRenewal {
    /// Topology revision observed before this intent was requested.
    pub topology_revision: u64,
    /// App team whose member retains its original team slot.
    pub team: CanonicalUuid,
    /// Independently steerable member whose occupant is renewed.
    pub lane: CanonicalUuid,
    /// True requests succession; false clears only a pre-handoff request.
    pub requested: bool,
}

/// State the model spelling one seated lane launches with.
///
/// The request restates the seat's model outright rather than describing a
/// change to it: a spelling pins the seat, and its absence returns the seat to
/// the vendor's configured model. A launch already in flight keeps the model
/// it started with, so this governs the lane's next occupant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneRetune {
    /// Topology revision observed before this retune was requested.
    pub topology_revision: u64,
    /// Seated member whose next launch changes.
    pub lane: CanonicalUuid,
    /// Model spelling this seat names, absent to follow the vendor's
    /// configured model.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub model: Option<String>,
}
/// State the reasoning effort one seated lane launches with.
///
/// A named effort pins the seat. Absence returns future launches to the
/// vendor's configured effort.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneEffortRetune {
    /// Topology revision observed before this retune was requested.
    pub topology_revision: u64,
    /// Seated member whose next launch changes.
    pub lane: CanonicalUuid,
    /// Effort spelling this seat names, absent to follow vendor configuration.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub effort: Option<String>,
}

/// Turn one seated lane's own fast-mode flag on or off.
///
/// The flag is additive with the lane's team flag and the global setting, so
/// turning it off leaves the lane fast while either of those is on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneFastRetune {
    /// Topology revision observed before this retune was requested.
    pub topology_revision: u64,
    /// Seated member whose own flag changes.
    pub lane: CanonicalUuid,
    /// Whether this lane itself enables fast mode.
    #[serde(default, skip_serializing_if = "is_false")]
    pub fast_mode: bool,
}

/// Select one lane's continuity account, or the operator-wide account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneRecall {
    /// Lane whose continuity is requested; absent for the operator-wide view.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub lane: Option<CanonicalUuid>,
}

/// Recognize one sealed proof bundle that arrived from outside the journal.
///
/// Recognition is idempotent on its observation key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recognize {
    /// Observation key that makes recognition idempotent.
    pub key: Digest,
}

/// Drive the resident to the requested lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpChange {
    /// State the resident is asked to reach.
    pub state: ResidentState,
}

/// The answer a steering command returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectiveAnswer {
    /// Directive the answer concerns.
    pub directive: CanonicalUuid,
    /// Actor the directive addresses: the lane a command selected or
    /// explicitly addressed, or the principal a route's input reaches.
    pub recipient: Actor,
    /// Human-sized key quoted by inbox and acknowledgment prose.
    pub key: String,
    /// Commit instant of the original operator publication.
    pub authored: Instant,
    /// Its disposition after the transaction.
    pub disposition: Disposition,
}
/// One pending directive returned by an inbox read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingDirective {
    /// Durable directive identity.
    pub directive: CanonicalUuid,
    /// Human-sized acknowledgment key.
    pub key: String,
    /// Operator submit action shared by target-specific fanout directives.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub fanout: Option<CanonicalUuid>,
    /// Direction text.
    pub text: String,
    /// Seated lane that told this directive as a peer message.
    ///
    /// Absent on operator and resident direction. Present only when the
    /// creating fact's actor is exactly this lane.
    #[serde(deserialize_with = "crate::optional::required")]
    pub from: Option<CanonicalUuid>,
    /// Exact occupant that sent this peer directive.
    ///
    /// Absent on direction that has no peer sender.
    #[serde(deserialize_with = "crate::optional::required")]
    pub sender: Option<PeerSender>,
    /// Immutable code of the principal that authored this direction: the
    /// agent-facing author slot a lane's occupant `sender` fills for peers.
    ///
    /// Absent on lane and resident direction.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub principal: Option<crate::principal::PrincipalCode>,
    /// Current rendered handle of the room anchor frozen by a peer route.
    ///
    /// Absent on non-message direction and direct peer routes. The durable
    /// steering authority stores only the anchor task's birth stamp.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub room: Option<TaskHandle>,
    /// The earlier occupant of this lane a peer message named, when the
    /// message reached this lane's later occupant instead.
    ///
    /// Absent on direction that named its seated recipient and on every other
    /// direction.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub addressed: Option<PeerAddressed>,
    /// Whether this directive can wake its lane.
    ///
    /// Only operator direction wakes a lane. Peer messages, peer replies, and
    /// resident reminders stay pending until the lane's next seat reads them.
    #[serde(default, skip_serializing_if = "is_false")]
    pub wakes: bool,
    /// First line authored outside quoted context, when the producer found one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub preview: Option<String>,
    /// Steering control attached to the direction, when present.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub control: Option<String>,
    /// Where a peer input stands on its conversation line.
    ///
    /// Absent on every direction that is not a peer line input.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub line: Option<LineStanding>,
    /// Exact attachments retained with the directive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Media>,
    /// Commit instant of the original publication.
    pub authored: Instant,
    /// Current presentation urgency on the stable directive.
    pub stage: crate::body::SteeringStage,
    /// Complete ordered presentation-transition audit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub escalations: Vec<crate::body::SteeringTransition>,
    /// Instant at which the directive lapses.
    pub expires: Instant,
    /// Codes of the other lanes the same broadcast reached, in its order.
    ///
    /// Each is the lane's newest occupant code, which addresses the lane
    /// whether or not that occupant still sits there. Empty when the
    /// direction reached this lane alone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cc: Vec<crate::OccupantAddress>,
}

/// Pending direction for one lane at a resident snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneInbox {
    /// Lane this row describes.
    pub lane: CanonicalUuid,
    /// Occupant currently seated in the lane, when any.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub occupant: Option<CanonicalUuid>,
    /// Latest start, renewal, or compaction position for the current occupant.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub compaction: Option<u64>,
    /// Pending directives in journal order.
    pub pending: Vec<PendingDirective>,
    /// Open peer lines this lane takes part in, oldest effect first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<PeerLine>,
}

/// Snapshot-positioned steering inbox answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxAnswer {
    /// Resident generation that produced the answer.
    pub epoch: CanonicalUuid,
    /// Visible lanes in canonical lane order.
    pub lanes: Vec<LaneInbox>,
}

/// The answer a consent decision returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentAnswer {
    /// Request that was decided.
    pub request: CanonicalUuid,
    /// The recorded decision.
    pub answer: Answer,
}

/// One current bounded-rule budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentBudgetAnswer {
    /// Rule whose grants are metered.
    pub rule: Name,
    /// Grants admitted between renewals.
    pub bound: u32,
    /// Grants still available in the current renewal window.
    pub remaining: u32,
}

/// One request currently waiting on the operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentPendingAnswer {
    /// Pending request identity.
    pub request: CanonicalUuid,
    /// Lane that raised the request.
    pub lane: CanonicalUuid,
    /// Operation awaiting consent.
    pub operation: String,
    /// Rule or posture that queued the request.
    pub rule: Name,
    /// Request-bound reason the operation queued.
    pub cause: ConsentQueueCause,
    /// Current renewal state when this request exhausted a bounded rule.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub budget: Option<ConsentBudgetAnswer>,
}

impl ConsentPendingAnswer {
    /// The exact witnesses a terminal operator ruling on this request carries.
    ///
    /// Every decider (the CLI, the app, and the resident that re-derives
    /// them to admit a ruling) builds them here, so the resident refuses a
    /// ruling whose queue position has moved.
    ///
    /// # Errors
    ///
    /// Refuses any answer other than granted or denied.
    pub fn decision_witnesses(
        &self,
        answer: Answer,
    ) -> Result<Vec<crate::Witness>, crate::Refusal> {
        if !matches!(answer, Answer::Granted | Answer::Denied) {
            return Err(crate::Refusal::bare(
                crate::RefusalDiagnostic::new(
                    "consent::ruling-invalid",
                    Some(self.request.to_string()),
                    Some("granted or denied".to_owned()),
                    Some(format!("{answer:?}").to_lowercase()),
                )
                .with_detail("choose allow or deny for the queued request"),
            ));
        }
        let mut witnesses = vec![
            crate::Witness::Consent {
                request: self.request,
                answer: Answer::Queued,
            },
            crate::Witness::Rule {
                name: self.rule.clone(),
            },
        ];
        if answer == Answer::Granted
            && let Some(budget) = &self.budget
        {
            witnesses.push(crate::Witness::Budget {
                rule: budget.rule.clone(),
                remaining: budget.remaining,
            });
        }
        Ok(witnesses)
    }
}

/// The pending consent queue and all current bounded-rule budgets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentQueueAnswer {
    /// Requests waiting on the operator, ordered by request identity.
    pub pending: Vec<ConsentPendingAnswer>,
    /// Current budgets in tracked policy order.
    pub budgets: Vec<ConsentBudgetAnswer>,
}

/// One answer available to tracked consent policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsentPolicyDecision {
    /// Permit the operation.
    Allow,
    /// Refuse the operation.
    Deny,
    /// Ask the operator.
    Queue,
}

/// One ordered tracked consent rule and its current budget state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentRuleAnswer {
    /// Stable rule identity.
    pub name: Name,
    /// Exact operation or glob pattern the rule matches.
    pub tool: String,
    /// Decision produced by the rule.
    pub answer: ConsentPolicyDecision,
    /// Configured grant bound, when present.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub budget: Option<u32>,
    /// Grants currently available, when the rule is bounded.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub remaining: Option<u32>,
}

/// The tracked consent posture and ordered rule table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentPolicyAnswer {
    /// Decision used when no rule matches.
    pub posture: ConsentPolicyDecision,
    /// Rules in first-match order.
    pub rules: Vec<ConsentRuleAnswer>,
}

/// The answer a trust command returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustAnswer {
    /// Digest whose standing moved.
    pub digest: Digest,
    /// Its standing after the transaction.
    pub standing: TrustState,
}

/// The answer a lane or resident state change returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateAnswer {
    /// State held before the transaction.
    pub from: ResidentState,
    /// State held after it.
    pub to: ResidentState,
}

impl std::fmt::Display for StateAnswer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(if self.to == ResidentState::Dark {
            "Spice resident stopped."
        } else {
            "Spice resident is stopping."
        })
    }
}

/// Where one command a lane keeps stands, seen from the lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LaneJobStanding {
    /// The command still runs under its keeper.
    Running,
    /// The command ended and nobody has heard how yet.
    Unheard,
}

impl LaneJobStanding {
    /// Exact operator-facing wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Unheard => "unheard",
        }
    }
}

/// How one command a lane keeps lives, as its call declared it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LaneJobKind {
    /// A deadline-bounded command the lane waits on or hears end later.
    Command,
    /// A long-lived service that runs until its claim ends or it is stopped.
    Service,
    /// A deadline-bounded command with an armed `until` watcher, whose first
    /// matching output line reaches the lane, waking it if nobody hears it.
    Watch,
    /// A service with an armed `until` watcher, whose first matching output
    /// line reaches only a seated occupant.
    WatchedService,
}

impl LaneJobKind {
    /// Exact operator-facing wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Service => "service",
            Self::Watch => "watch",
            Self::WatchedService => "watched-service",
        }
    }
}

/// One command a lane keeps beyond the call that started it, until the lane
/// hears how it ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneJob {
    /// The job's key: the system's microsecond key, so a listing is a timeline.
    pub id: String,
    /// The command as the model wrote it.
    pub command: String,
    /// Ambient task held when the job was launched, if any.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub task: Option<crate::TaskStamp>,
    /// How the command lives: waited on, served, or watched.
    pub kind: LaneJobKind,
    /// Whether the command still runs, or ended unheard.
    pub standing: LaneJobStanding,
}

/// Milliseconds of transcript quiet after which a supervised occupant is idle.
///
/// The resident liveness fold and every consumer that dates the corresponding
/// presence transition share this boundary.
pub const LANE_QUIET_HORIZON_MILLIS: u64 = 60_000;

/// Dynamic liveness of one lane at the read barrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LaneLiveness {
    /// The occupant is waiting inside one tool call beyond the resident bound.
    Blocked,
    /// The occupant emitted selected-record activity inside the quiet horizon,
    /// so what it is up to right now is known in detail.
    Working,
    /// The occupant is supervised in the seat; nothing more is claimed.
    Running,
    /// No occupant is supervised in this durable seat.
    Vacant,
}

impl LaneLiveness {
    /// Exact operator-facing wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Working => "working",
            Self::Running => "running",
            Self::Vacant => "vacant",
        }
    }
}
/// Current durable hold that pauses automatic seating for one lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneHold {
    /// Exact evidence carried by the journaled hold fact.
    pub evidence: String,
    /// Deadline carried by the journaled hold fact.
    pub until: Instant,
}

/// One capability-free lane row returned by [`LaneRead`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneAnswerRow {
    /// Stable independently steerable member identity.
    pub lane: CanonicalUuid,
    /// App team whose lane contains this member.
    pub team: CanonicalUuid,
    /// Operator-configured agent display name, absent when unconfigured.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub name: Option<String>,
    /// Canonical worktree bound to the lane.
    pub worktree: String,
    /// Effective browser-safe speech contract resolved from this member's worktree.
    pub speech: crate::SpeechPlayback,
    /// Closed lane-life spelling governing re-seating.
    pub life: String,
    /// Vendor selected for occupants of this lane.
    pub vendor: Name,
    /// Concrete model currently selected in the declared vendor session.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub model: Option<String>,
    /// Model spelling this seat pins for the launches it governs, absent when
    /// the seat follows the vendor's configured model.
    ///
    /// `model` reports the process that is running; this reports the seat that
    /// will start the next one. A retune states the seat alone, so the two
    /// disagree until the lane relaunches.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub seat_model: Option<String>,
    /// Reasoning effort currently selected in the declared vendor session.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub effort: Option<String>,
    /// Effort spelling this seat pins for the launches it governs, absent when
    /// the seat follows the vendor's configured effort.
    ///
    /// `effort` reports the running process; this reports the next launch.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub seat_effort: Option<String>,
    /// Whether the declared occupant runs in the vendor's fast mode.
    #[serde(default, skip_serializing_if = "is_false")]
    pub fast_mode: bool,
    /// The vendor's words refusing fast mode when that refusal turned it off
    /// on the declared occupant, which continues without it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub fast_refusal: Option<String>,
    /// Whether this lane's own flag enables fast mode.
    ///
    /// `fast_mode` reports the running process; the next launch runs fast
    /// when this flag, the team's flag, or the global setting is on.
    #[serde(default, skip_serializing_if = "is_false")]
    pub seat_fast_mode: bool,
    /// Observable GIT-04 outcome for the declared occupant launch.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub git: Option<String>,
    /// RTK health observed for the declared occupant launch.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub rtk: Option<crate::body::RtkHealth>,
    /// Current occupant, absent between seats.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub occupant: Option<CanonicalUuid>,
    /// The occupant code the lane shows: its newest occupant's birth address,
    /// kept while the lane is vacant and absent before any occupant started.
    /// Every code the lane has shown selects it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub code: Option<crate::OccupantAddress>,
    /// Occupant ancestry, oldest first.
    pub lineage: Vec<CanonicalUuid>,
    /// Exact resumable vendor session identity, absent before declaration.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub session: Option<String>,
    /// Whether this seat has resumable session authority.
    pub bound: bool,
    /// Durable occupancy disposition.
    pub disposition: String,
    /// Dynamic supervised live/quiet fold at this read barrier.
    pub liveness: LaneLiveness,
    /// Exact command holding a lane reported as blocked.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub blocked_on: Option<String>,
    /// Current durable seating hold, absent after its resume transition.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub hold: Option<LaneHold>,
    /// Why the lane's automatic lifecycle work stands parked at this read
    /// barrier; absent when nothing parks it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub parked: Option<crate::body::LanePark>,
    /// Commands the lane keeps beyond the calls that started them, oldest
    /// first: what a vacant lane is still waiting on, and will wake for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<LaneJob>,
}

/// Current lane state read at one resident barrier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneAnswer {
    /// Resident generation that owns the table.
    pub epoch: CanonicalUuid,
    /// Resident state at the read barrier.
    pub state: ResidentState,
    /// Monotonic authority for this exact topology and policy snapshot.
    pub topology_revision: u64,
    /// Current lane rows in the residency head's durable placement order: a
    /// seated lane appends, and team moves keep each team's member order.
    pub lanes: Vec<LaneAnswerRow>,
    /// One authoritative runtime policy subscription per returned team.
    pub subscriptions: Vec<crate::TeamSubscription>,
    /// Current capability-free agent renewal state in team/member order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub renewals: Vec<crate::LaneRenewalRecord>,
    /// Milliseconds until the next supervised occupant becomes quiet.
    ///
    /// Re-read once at this deadline; absence means no time-driven transition
    /// remains in this snapshot. This is not a periodic refresh interval.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub refresh_after_millis: Option<u32>,
}

/// The answer returned after setting one lane member's renewal intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "LaneRenewalAnswerWire", into = "LaneRenewalAnswerWire")]
pub struct LaneRenewalAnswer {
    /// App team whose member was addressed.
    pub team: CanonicalUuid,
    /// Independently steerable member whose intent was set.
    pub lane: CanonicalUuid,
    /// Topology authority after the transaction committed.
    pub topology_revision: u64,
    /// Current state, absent after a pre-handoff request is cleared.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub renewal: Option<crate::LaneRenewalRecord>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaneRenewalAnswerWire {
    team: CanonicalUuid,
    lane: CanonicalUuid,
    topology_revision: u64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    renewal: Option<crate::LaneRenewalRecord>,
}

impl TryFrom<LaneRenewalAnswerWire> for LaneRenewalAnswer {
    type Error = &'static str;

    fn try_from(wire: LaneRenewalAnswerWire) -> Result<Self, Self::Error> {
        if wire.renewal.as_ref().is_some_and(|renewal| {
            renewal.team != wire.team
                || renewal.lane != wire.lane
                || renewal.revision != wire.topology_revision
        }) {
            return Err("renewal state does not match the addressed topology revision");
        }
        Ok(Self {
            team: wire.team,
            lane: wire.lane,
            topology_revision: wire.topology_revision,
            renewal: wire.renewal,
        })
    }
}

impl From<LaneRenewalAnswer> for LaneRenewalAnswerWire {
    fn from(answer: LaneRenewalAnswer) -> Self {
        Self {
            team: answer.team,
            lane: answer.lane,
            topology_revision: answer.topology_revision,
            renewal: answer.renewal,
        }
    }
}

/// The answer a lane seating or lifecycle command returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneChangeAnswer {
    /// Lane whose disposition changed.
    pub lane: CanonicalUuid,
    /// Topology authority after the transaction committed.
    pub topology_revision: u64,
    /// Disposition held before the transaction.
    pub from: String,
    /// Disposition held after the transaction.
    pub to: String,
    /// GIT-04 outcome for a launch transition; absent for non-launch changes.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub git: Option<Box<str>>,
    /// RTK health for a launch transition; absent for non-launch changes.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub rtk: Option<crate::body::RtkHealth>,
}

/// One bounded continuity event retained in a recall account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallEntry {
    /// Journal position of the event.
    pub seq: Seq,
    /// Closed fact kind observed there.
    pub kind: FactKind,
    /// Principal that authored the event.
    pub actor: Actor,
    /// Durable origin carried by the event.
    pub origin: Origin,
    /// Lane named by the event, absent for repository-wide facts.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub lane: Option<CanonicalUuid>,
    /// Safe continuity evidence projected from selected fact bodies.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub provenance: Option<RecallProvenance>,
}

/// Closed, redacted evidence carried by a continuity chronology entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RecallProvenance {
    /// Exact durable vendor session authority.
    Session {
        /// Occupant that declared the session.
        occupant: CanonicalUuid,
        /// Worktree bound to the session launch.
        worktree: String,
        /// Vendor that admitted the declaration.
        vendor: Name,
        /// Resident generation that observed the declaration.
        epoch: CanonicalUuid,
        /// Selected child stream carrying the declaration.
        stream: String,
        /// Exact resumable vendor identity.
        identity: String,
        /// Durable bytes carrying the declaration.
        locus: TapeLocus,
    },
    /// One typed compaction boundary recognized from selected-stream bytes.
    Compaction {
        /// Occupant that crossed the boundary.
        occupant: CanonicalUuid,
        /// Selected child stream carrying the boundary.
        stream: String,
        /// Durable bytes carrying the boundary.
        locus: TapeLocus,
    },
    /// Redacted steering lineage or disposition evidence.
    Steering {
        /// Stable directive identity.
        directive: CanonicalUuid,
        /// Stable directive key.
        key: String,
        /// Current disposition established by the fact.
        disposition: Disposition,
        /// Event-local stage, or the preserved current stage on terminal facts.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        stage: Option<crate::body::SteeringStage>,
        /// Complete ordered presentation-transition audit retained by the stable directive.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        escalations: Vec<crate::body::SteeringTransition>,
        /// Resident generation authenticating transcript evidence, when applicable.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        epoch: Option<CanonicalUuid>,
        /// Exact assistant-prose evidence, absent for send, resend, or cancellation.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        locus: Option<TranscriptLocus>,
    },
    /// Successor ancestry without exposing the operating-system process group.
    Renewal {
        /// Successor occupant.
        occupant: CanonicalUuid,
        /// Occupant the successor descends from.
        ancestor: CanonicalUuid,
        /// Resident generation that admitted the successor.
        epoch: CanonicalUuid,
    },
}

/// Ranked, bounded continuity account served at one journal barrier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallAnswer {
    /// Composite journal barrier every field was derived from.
    pub barrier: Seq,
    /// Canonical repository worktree serving the account.
    pub repository: String,
    /// Requested lane, absent for an operator-wide account.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub lane: Option<CanonicalUuid>,
    /// Current resident and lane state.
    pub residency: LaneAnswer,
    /// Most recent relevant events, newest first and explicitly bounded.
    pub recent: Vec<RecallEntry>,
}

mod binding;
pub use binding::{AnswerShape, Binding, BodyShape, CommandAnswer, Intent};

const fn is_false(value: &bool) -> bool {
    !*value
}
