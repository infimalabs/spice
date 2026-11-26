//! Task and claim command contracts.
//!
//! Commands may name projected handles for lookup. Creation plans and durable
//! answers cross the admission boundary with birth stamps, so a later
//! topic move only changes presentation.

use serde::{Deserialize, Serialize};

use crate::envelope::{Actor, Origin, Seq};
use crate::ident::{CanonicalUuid, Digest, Domain, Instant};
use crate::witness::{Name, Phase, TaskHandle, TaskStamp};

/// Add one task as direct intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAdd {
    /// One-line statement of the work.
    pub title: String,
    /// One plain sentence saying who can do what once the task lands and why;
    /// the write refuses an absent or empty pitch.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pitch: String,
    /// Named topic, or absent to let the router place the task.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub topic: Option<crate::TopicInput>,
    /// Longer body context.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub description: Option<String>,
    /// Criteria that decide whether the work is done.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub term: Vec<String>,
    /// Typed environmental facts required before allocation.
    #[serde(
        default,
        skip_serializing_if = "crate::EnvironmentRequirements::is_empty",
        deserialize_with = "crate::EnvironmentRequirements::present"
    )]
    pub requirements: crate::EnvironmentRequirements,
    /// Caller-authored creation provenance, or server-resolved causal authority.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub origin: Option<Origin>,
    /// Non-default phase route, when the caller means one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub flow: Option<String>,
    /// Handles this task must follow.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends: Vec<TaskHandle>,
    /// Existing tasks this one is declared related to, without dependency,
    /// requirement, allocation hold, or completion credit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<TaskHandle>,
    /// Declared Plan or Design intent the task opens with.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub intent: Option<crate::TaskIntent>,
    /// Existing Plan or Design context this Plan prepares within.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub preparation: Option<TaskHandle>,
    /// Whether creation must atomically place the caller's lane in possession.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub claim: bool,
    /// Save prospective work in Draft without submitting or activating it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub draft: bool,
}

/// Add one acceptance-bearing line-item Plan under the current opportunistic
/// OOPS Design generation.
///
/// The route owns the flow, preparation, and construction contract. A caller
/// names only the public owner topic, or lets one active public claim supply
/// it, so this DTO cannot become generic task creation. `design` names either
/// the exact current generation or the intended absent generation atomically
/// opened with this report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskOopsAdd {
    /// Current or intended opportunistic OOPS Design generation.
    pub design: TaskStamp,
    /// Exact account of the unexpected behavior, retained as its Plan title.
    pub title: String,
    /// Explicit causal authority when no active claim owns the report.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub origin: Option<Origin>,
    /// Named topic; omission explicitly selects `/oops`, independently of routing.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub topic: Option<crate::TopicInput>,
}

/// File one previewed debt sweep: a Plan and one child task per file.
///
/// The route owns intent, flow, preparation, and dependency edges, so this
/// body cannot become generic batch creation. `preview` digests the filed
/// content, which the resident recomputes, so it files exactly what the
/// caller previewed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSweep {
    /// [`SweepPreview::digest`] of `content`.
    pub preview: Digest,
    /// Intended absent birth of the Plan.
    pub plan: TaskStamp,
    /// Intended absent birth of each child, in `content.children` order.
    pub children: Vec<TaskStamp>,
    /// The previewed texts and deferral.
    pub content: SweepPreview,
    /// Target-qualified per-test wall times attached to children whose slow
    /// Cargo test arm the apply selected and ran once per target.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub splits: Vec<SweepSplit>,
}

/// Target-qualified test reports for one slow Cargo test arm, attached to the
/// child that covers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepSplit {
    /// Index into `content.children`.
    pub child: usize,
    /// Each test the arm's targets ran, slowest first.
    pub tests: Vec<SweepTestTime>,
}

/// One test's identity and wall time from the harness report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepTestTime {
    /// Cargo target identity from the arm, such as `lib:spice`.
    pub target: String,
    pub name: String,
    pub millis: u64,
}

impl SweepSplit {
    /// The lines an applied sweep appends to its child's description.
    #[must_use]
    pub fn description(&self) -> String {
        use std::fmt::Write as _;

        let mut text = String::from("\n\nTests by wall time, from harness reports:");

        for test in &self.tests {
            let _ = write!(
                text,
                "\n- {}::{}: {}.{:03}s",
                test.target,
                test.name,
                test.millis / 1000,
                test.millis % 1000
            );
        }
        text
    }
}

/// What one debt sweep files, apart from the births each apply allocates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepPreview {
    /// Public topic the Plan and its children are filed in.
    pub topic: crate::Topic,
    /// Deferral of the Plan, when the sweep waits.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub until: Option<Instant>,
    /// The Plan every child waits on.
    pub plan: SweepText,
    /// One child per file.
    pub children: Vec<SweepText>,
}

impl SweepPreview {
    /// The digest an apply names to file exactly this content.
    #[must_use]
    pub fn digest(&self) -> Digest {
        let bytes = crate::encode(self).expect("a sweep preview encodes");
        Digest::compute(Domain::BoardSweepPreview, &bytes)
    }
}

/// The statement one sweep task is filed with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepText {
    /// One-line statement of the work.
    pub title: String,
    /// One plain sentence saying who can do what once the task lands and why.
    pub pitch: String,
    /// The debt and its provenance.
    pub description: String,
    /// Criteria that decide whether the work is done.
    pub term: Vec<String>,
}

/// Replace an unsettled task statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskEdit {
    /// Task whose statement is replaced.
    pub task: TaskHandle,
    /// Replacement title, when the title moves.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub title: Option<String>,
    /// Replacement pitch, when the pitch moves.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub pitch: Option<String>,
    /// Replacement description, when the description moves; empty clears it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub description: Option<String>,
    /// Complete replacement criteria, when they move. `Some([])` clears them;
    /// absence leaves them unchanged.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub term: Option<Vec<String>>,
    /// Dedicated replacement or clearing of environmental and resource requirements.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub requirements: Option<TaskRequirementsEdit>,
    /// Dedicated declaration or withdrawal of Plan or Design intent.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub intent: Option<crate::TaskIntentEdit>,
    /// Replacement priority, when the allocator's ranking of this task moves.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub priority: Option<String>,
    /// Replacement topic, when the task moves to a different audience.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub topic: Option<crate::TopicInput>,
    /// Complete desired route. Draft edits replace every prospective phase;
    /// activated edits preserve the route through the current phase and replace
    /// only its unstarted suffix.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub flow: Option<String>,
    /// Existing tasks this one is newly declared related to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<TaskHandle>,
}

/// One explicit task-requirement mutation.
///
/// The dedicated action keeps omission as "unchanged", a nonempty canonical
/// collection as "replace", and clearing as its own spelling. Resource entries
/// compile into the statement's canonical resource map during publication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
pub enum TaskRequirementsEdit {
    /// Replace the complete AND-composed requirement collection.
    Set {
        /// Nonempty canonical requirements in environmental-fact order.
        #[serde(deserialize_with = "crate::EnvironmentRequirements::present")]
        requirements: crate::EnvironmentRequirements,
    },
    /// Remove every environmental and resource requirement.
    Clear {},
}

impl TaskRequirementsEdit {
    /// Replace requirements with one necessarily canonical fact.
    #[must_use]
    pub fn one(requirement: crate::EnvironmentRequirement) -> Self {
        Self::Set {
            requirements: crate::EnvironmentRequirements::one(requirement),
        }
    }

    /// Complete replacement collection, or empty when clearing.
    #[must_use]
    pub fn requirements(&self) -> crate::EnvironmentRequirements {
        match self {
            Self::Set { requirements } => requirements.clone(),
            Self::Clear {} => crate::EnvironmentRequirements::default(),
        }
    }
}

/// Withdraw a task from the board with a recorded reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDelete {
    /// Task the operator withdraws.
    pub task: TaskHandle,
    /// Why the task leaves the board; required, because a withdrawal that
    /// records no reason is indistinguishable from one made in error.
    pub reason: String,
    /// Override a live claim rather than refusing the withdrawal.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force_claimed: bool,
}

/// Close a task as completed on the operator's word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskClose {
    /// Task the operator closes.
    pub task: TaskHandle,
    /// Evidence or reason the task is complete; recorded as its validation.
    pub validation: String,
    /// Override a live claim rather than refusing the closure.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force_claimed: bool,
}

/// Conclude an open, unheld Design on the operator's word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskConclude {
    /// Design the operator concludes.
    pub task: TaskHandle,
    /// Done, or collapsed with the reason its premise did not hold.
    pub conclusion: crate::body::DesignConclusion,
}

/// Append an observation without replacing the statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskNote {
    /// Task the observation attaches to.
    pub task: TaskHandle,
    /// The observation.
    pub note: String,
}

/// Set a wakeable deferral.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDefer {
    /// Task being deferred.
    pub task: TaskHandle,
    /// Instant the deferral wakes at.
    pub wait: Instant,
}

/// Clear the current deferral, optionally claiming the filed task atomically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskWake {
    /// Filed deferred task being woken.
    pub task: TaskHandle,
    /// Wake and atomically claim the task in its current lifecycle state. A
    /// Gathering Design remains unsteeped and gathering input.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub claim: bool,
}

/// Advance one uncontrolled draft out of its draft phase through the
/// phase-zero validator; an incomplete draft is instead marked submitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskFile {
    /// Draft task being advanced.
    pub task: TaskHandle,
    /// Public topic receiving the task; omission keeps its saved topic or
    /// lets the router decide.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub topic: Option<crate::TopicInput>,
    /// Advance and atomically claim the resulting ready task.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub claim: bool,
}

/// Preview or apply the router's placement of an existing task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPlace {
    /// Task whose saved topic may change.
    pub task: TaskHandle,
    /// Exact preview to apply; absence is a read-only preview.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub apply: Option<Digest>,
}

/// One router result, including an explicit refusal instead of a fallback topic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskRoute {
    /// The exact canonical filing destination, including any authored tail.
    Pick {
        /// Durable path proposed for filing.
        topic: crate::Topic,
        /// Prior tasks in the same forecast seat.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        companions: Vec<TaskStamp>,
        /// Present when the grouping evidence precedes the authorized source.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tree_lag: Option<crate::topic::TreeLag>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        unmatched: bool,
    },
    /// No destination is admitted by the routing authority.
    Unavailable {
        /// Stable routing diagnostic.
        reason: String,
        /// Why routing could not select a destination.
        detail: String,
    },
}

/// Placement and drive disposition captured by a task creation or filing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPlacement {
    /// Saved topic; absent when inference missed or a draft keeps only the proposal.
    pub topic: Option<crate::Topic>,
    /// The inferred proposal or its refusal. Explicit destinations bypass inference.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub router: Option<TaskRoute>,
    /// Stable policy denial that kept this work as a Draft.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub drive_refusal: Option<String>,
    /// Concrete repair for this task when drive denial kept it Draft.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub next: Option<String>,
}

/// Placement and ownership of a task created within another task's operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCreated {
    /// Owners at the child's final topic and phase.
    pub ownership: TaskOwnership,
    /// The child's placement and any drive denial that kept it Draft; absent
    /// when the child was created unplaced.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub placement: Option<TaskPlacement>,
}

/// Read-only placement preview or the corresponding committed topic move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPlaceAnswer {
    /// Task being placed.
    pub task: TaskHandle,
    /// Topic observed before applying placement; absent while unplaced.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub from: Option<crate::Topic>,
    /// Pick restricted to the creator team's drive roots.
    pub route: TaskRoute,
    /// Owners at the proposed destination; absent when there is no pick.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub ownership: Option<TaskOwnership>,
    /// Workflow phase after the proposed move; placement never advances a
    /// draft.
    pub phase: crate::Phase,
    /// Policy refusal that requires this placement to remain a Draft.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub drive_refusal: Option<String>,
    /// Digest admitting this exact preview; absent when no move is available.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub preview: Option<Digest>,
    /// Whether the topic move committed.
    pub applied: bool,
    /// Next explicit action, such as filing a placed Draft.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub next: Option<String>,
}

/// Replace one unfiled Draft's image artifacts with exactly `media`.
///
/// The set is admitted whole under [`crate::media::admit_media`] before any
/// artifact moves, so a refused set changes nothing. Images already stored
/// with the same name and bytes stay; the Draft's other images leave, and its
/// non-image artifacts are untouched. An empty set removes every image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskMedia {
    /// Draft whose images are replaced.
    pub task: TaskHandle,
    /// The Draft's complete image set, in presentation order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<crate::Media>,
}

/// Name one task and nothing else.
///
/// Claim moves that only identify their subject share this shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRef {
    /// The subject.
    pub task: TaskHandle,
}

/// Join an already-shared task directly or accept one typed invitation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskJoin {
    /// Consume one completion slot the current phase already exposes.
    Task {
        /// Exact task generation to join.
        task: TaskHandle,
    },
    /// Accept one live steering invitation by its canonical key.
    Invitation {
        /// Existing steering key whose control binds the invitation.
        key: String,
    },
}

/// Invite one partner lane into the caller's held task phase.
///
/// Without a recipient the resident selects the best eligible nonparticipant,
/// preferring the holder's team. An address names the lane whose occupant it
/// addresses, so a relaunched lane still answers to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskInvite {
    /// Task the caller holds.
    pub task: TaskHandle,
    /// Exact occupant address naming the recipient lane.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub recipient: Option<crate::OccupantAddress>,
    /// The captured write of the invitation URL whose body is the requester's
    /// own words: read from the requester's tape at this locus, admitted with
    /// the invitation and delivered to the lane it reaches in the task room.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub message: Option<crate::outbox::Carrier>,
}

/// Where one committed invitation went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskInviteAnswer {
    /// Task the recipient was invited into.
    pub task: TaskHandle,
    /// Recipient lane.
    pub lane: CanonicalUuid,
    /// How the invitation reached that lane.
    pub delivery: TaskInviteDelivery,
    /// Outbox submission holding the requester's message, when it wrote one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub message: Option<CanonicalUuid>,
}

/// How one invitation reached its recipient lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskInviteDelivery {
    /// A vacant lane joined at once: it holds the claim its next launch seats.
    Joined {
        /// Occupant the claim names; the launch seats exactly this one.
        occupant: CanonicalUuid,
        /// Phase target the same write raised to open the slot.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        grown: Option<std::num::NonZeroU32>,
    },
    /// An occupied lane is invited: a live steering invitation whose ACK joins
    /// and whose NAK declines.
    Invited {
        /// Steering key the recipient answers.
        key: String,
        /// Stable steering identity the sender may cancel immediately.
        directive: CanonicalUuid,
        /// Instant the invitation lapses on its own; the inviter's claim,
        /// phase, generation or seat ending expires it earlier.
        expires: crate::Instant,
    },
}

/// Add or remove dependency edges between existing tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDepends {
    /// Task that follows.
    pub task: TaskHandle,
    /// Tasks it must follow.
    pub after: Vec<TaskHandle>,
    /// Remove the named edges instead of adding them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remove: bool,
}

/// Set how many distinct lanes complete one flow phase of a task.
///
/// Only the operator edits explicit targets. Invitation admission may grow
/// a full phase atomically with the partner claim that fills its new slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskTarget {
    /// Task whose staffing changes.
    pub task: TaskHandle,
    /// Flow phase the target governs.
    pub phase: Phase,
    /// Distinct lanes that phase needs; zero refuses at decode.
    pub target: std::num::NonZeroU32,
}

/// Complete the phase held by the caller's active claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDone {
    /// Task being completed.
    pub task: TaskHandle,
    /// How completion was verified.
    pub validation: String,
    /// Durable completion identity minted by the initiating client.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub operation: Option<CanonicalUuid>,
    /// Why a Plan's plan phase completes without constructing work.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub disposition: Option<crate::body::PlanDisposition>,
}

impl TaskDone {
    /// High bits naming an operation [`Self::operation_for_claim`] derived.
    const CLAIM_OPERATION_PREFIX: u128 = 0x7370_6963_652d_0000_0000_0000_0000_0000;

    /// Derive the one default completion operation owned by a claim.
    #[must_use]
    pub fn operation_for_claim(claim: Seq) -> CanonicalUuid {
        CanonicalUuid::new(uuid::Uuid::from_u128(
            Self::CLAIM_OPERATION_PREFIX | u128::from(claim.get()),
        ))
    }

    /// The claim whose default completion operation `operation` is, when
    /// [`Self::operation_for_claim`] derived it.
    #[must_use]
    pub fn claim_for_operation(operation: CanonicalUuid) -> Option<Seq> {
        let value = operation.get().as_u128();
        if value & !u128::from(u64::MAX) != Self::CLAIM_OPERATION_PREFIX {
            return None;
        }
        Seq::new(u64::try_from(value & u128::from(u64::MAX)).ok()?)
    }
}

/// Record an assigned review verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReview {
    /// Task being reviewed.
    pub task: TaskHandle,
    /// The verdict.
    pub finding: String,
    /// Supporting prose.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub note: Option<String>,
    /// Follow-up statements this review creates behind the reviewed task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub then: Vec<ReviewFollowup>,
    /// Existing tasks wired to follow the reviewed task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub followup: Vec<TaskHandle>,
}

/// One follow-up statement a review creates, paired with its birth stamp.
///
/// The caller mints the stamp and witnesses its absence, the way `task add`
/// does, so the whole batch is decided before the review writes anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewFollowup {
    /// Sole identity minted for the statement.
    pub task: TaskStamp,
    /// The statement to create.
    pub add: TaskAdd,
}

/// Policy owners at the receipt or read's admitted configuration and membership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskOwnership {
    /// Winning teams; empty means no team owns automatic allocation.
    pub teams: Vec<CanonicalUuid>,
    /// Exclusive named rules reserved this work, even if their drain is off.
    pub reserved: bool,
}

/// The answer a task command returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAnswer {
    /// Placement decision for creation or filing; absent on other operations.
    #[serde(deserialize_with = "crate::optional::required")]
    pub placement: Option<TaskPlacement>,
    /// Children created by this operation, including their individual placement.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub created: std::collections::BTreeMap<TaskHandle, TaskCreated>,
    /// Resolved post-transaction ownership.
    #[serde(deserialize_with = "crate::optional::required")]
    pub ownership: Option<TaskOwnership>,
    /// Taste hold the transaction left standing on the task's statement,
    /// first so its author reads every match before anything else.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub taste: Option<crate::TasteHold>,
    /// Current acceptance authority, independent of the statement's taste hold.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub completion_contract: Option<Box<str>>,
    /// Task the answer concerns.
    pub task: TaskHandle,
    /// Its status after the transaction.
    pub status: String,
    /// Durable completion operation still running before this task may move.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub completion: Option<Box<TaskCompletionAnswer>>,
    /// Active claim minted in the same transaction, when creation also boards the caller.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub claim: Option<Box<ClaimAnswer>>,
    /// Pending controlled tasks closed atomically with the named task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closed: Vec<TaskHandle>,
    /// Exact CLI command admitted as the next action, when one is available.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub next: Option<Box<str>>,
    /// The construction refusal that left this task a submitted draft instead
    /// of advancing it: the refusal kind, detail, and remedy, so the author
    /// repairs the field or option it names before the next `task file`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub diagnostic: Option<Box<str>>,
    /// Siblings whose statement matched the created one closely enough to
    /// warn without refusing, strongest first, at most the duplicate preview.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub duplicates: Vec<DuplicateWarning>,
    /// Further matching siblings the duplicate preview leaves out.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub duplicates_omitted: u32,
}

const fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// One durable long-running task completion handoff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCompletionAnswer {
    /// Identity minted by the first command and retained by every retry.
    pub operation: CanonicalUuid,
    /// Closed operation state returned before terminal task publication.
    pub state: TaskCompletionState,
}

/// Non-terminal states a task command may return for durable completion work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskCompletionState {
    /// Mandatory evidence is executing outside the resident command loop.
    Running,
}

/// The answer a claim command returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimAnswer {
    /// Resolved ownership when this receipt was sealed.
    #[serde(deserialize_with = "crate::optional::required")]
    pub ownership: Option<TaskOwnership>,
    /// Task whose possession moved.
    pub task: TaskHandle,
    /// The won task's statement on fresh assignment, whole, because its
    /// holder works from it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub statement: Option<Box<ClaimStatement>>,
    /// Actor holding it after the transaction, absent when possession was released.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub holder: Option<Actor>,
    /// Task generation accepted by `task next`, so the holder knows completion
    /// is admitted against the current statement.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub acknowledged: Option<u64>,
    /// The lease sits at the indefinite approval horizon and stayed there
    /// while `task next` accepted the generation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
    /// Non-blocking overlap warnings discovered from the claim-time board.
    #[serde(default, skip_serializing_if = "claim_warnings_empty")]
    pub warnings: Box<[ClaimWarning]>,
    /// A steal found no live holder: it claimed the task with nobody
    /// displaced, as a capture would have.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub nothing_stolen: bool,
    /// Executable creation-context command or stable unavailable reason on fresh assignment.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub creation_context: Option<Box<str>>,
    /// Originating steering directive rendered for fresh task hydration.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub origin_context: Option<Box<str>>,
    /// The task's room conversation, newest first under the interactive read
    /// bounds, with the `task show` command that pages the rest; absent when
    /// the room holds no publication.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub room_context: Option<Box<str>>,
    /// Exact CLI command admitted as the next action, when one is available.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub next: Option<Box<str>>,
    /// Phase the claim landed in, with the one deliverable it authorizes.
    ///
    /// A lane reads this where it wins the work, because the phase decides
    /// whether the next action touches board state or repository files.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub phase: Option<ClaimPhase>,
    /// What claiming did with commits a previous holder left parked on the task.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub transplant: Option<ClaimTransplant>,
}

fn claim_warnings_empty(warnings: &[ClaimWarning]) -> bool {
    warnings.is_empty()
}

/// A won task's statement as its holder works from it. Description text stays
/// whole; each term row is prefixed by its current statement-item handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimStatement {
    /// Authored title.
    pub title: String,
    /// One plain sentence saying who can do what once the task lands and why.
    pub pitch: String,
    /// Longer authored context.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub description: Option<String>,
    /// Criteria that decide whether the work is done, each prefixed by its
    /// current statement-item handle.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub term: Vec<String>,
    /// Current own, linked, or unspecified acceptance contract.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub completion_contract: Option<Box<str>>,
    /// Current Design research obligation, independent of the completion contract.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub design_research: Option<Box<str>>,
}

/// The phase one claim answer was assigned in, and what it authorizes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimPhase {
    /// Phase as the workflow spells it.
    pub name: Phase,
    /// The one deliverable this phase authorizes, when it names one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub deliverable: Option<Box<str>>,
    /// Number of distinct arrivals required in this phase.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub target: u32,
    /// Participants whose claims are active before this assignment lands.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partners: Vec<ClaimParticipant>,
    /// Participants that already arrived in the current round.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arrivals: Vec<ClaimParticipant>,
}

fn one() -> u32 {
    1
}

fn is_one(value: &u32) -> bool {
    *value == 1
}

/// One active or arrived participant presented with a shared assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimParticipant {
    /// Participant actor.
    pub actor: Actor,
    /// Lane carrying the participation.
    pub lane: CanonicalUuid,
    /// Current proved peer address, when the lane is seated.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub address: Option<crate::OccupantAddress>,
}

/// Parked commits replayed onto the lane HEAD a claim fast-forwarded to.
///
/// Every parked commit lands in exactly one list, except the one the replay
/// stopped on, which `conflict` names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimTransplant {
    /// Lane HEAD the parked commits were replayed onto.
    pub onto: String,
    /// Commits replayed cleanly, oldest first, with their new identities.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied: Vec<TransplantedCommit>,
    /// Commits whose changes the lane already held, so nothing was replayed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub present: Vec<String>,
    /// Commits Git no longer holds, which could not be replayed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    /// The replay Git stopped on, left in the worktree for the claimant.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub conflict: Option<TransplantConflict>,
    /// Worktree snapshot restored uncommitted, or waiting behind a paused replay.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub snapshot: Option<String>,
}

/// One parked commit and the commit its replay created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransplantedCommit {
    /// Parked commit identity.
    pub parked: String,
    /// Commit created on the claimant's lane.
    pub commit: String,
}

/// A replay paused in the worktree as a Git cherry-pick with conflicts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransplantConflict {
    /// Parked commit whose cherry-pick stopped.
    pub commit: String,
    /// Paths Git left conflicted.
    pub paths: Vec<String>,
    /// Later parked commits queued behind it in Git's cherry-pick sequence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remaining: Vec<String>,
    /// How to finish the replay.
    pub remedy: Box<str>,
}

/// One active claim whose acceptance names the same implementation surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimWarning {
    /// Other in-flight task.
    pub task: TaskHandle,
    /// Lane holding the other claim.
    pub lane: CanonicalUuid,
    /// Rare exact identifiers both statements name, then their strongest
    /// shared weighted terms.
    pub surface: Vec<String>,
}

/// One independent sibling whose statement repeats a task being created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DuplicateWarning {
    /// The sibling task.
    pub task: TaskHandle,
    /// The sibling's title.
    pub title: String,
    /// Actor that created the sibling, when recorded.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub creator: Option<Actor>,
    /// Cosine between the two statements, to two decimals.
    pub cosine: Box<str>,
    /// The strongest weighted terms both statements share.
    pub surface: Vec<String>,
}

/// Run the named eligibility rule and atomically win its claim.
///
/// Ordinary allocation rules select from board state. Explicit recovery binds
/// the exact task through its sealed transaction and matching witnesses, so the
/// recovery identity is the sole additional selector carried by this intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimWin {
    /// Named eligibility rule the query runs.
    pub rule: Name,
    /// Exact dirty-handoff transaction, present only for explicit recovery.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub recovery: Option<String>,
}
