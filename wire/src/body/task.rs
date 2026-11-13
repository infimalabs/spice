//! Task and claim lifecycle fact bodies.
//!
//! Every task reference here is the durable birth stamp. Public handles
//! belong to folds and commands; no fact can make a rendered stem
//! authoritative again.

use super::*;

/// The sole transcript stream retained as executable task creation context.
pub const TASK_CREATION_CONTEXT_STREAM: &str = "session";

/// Why no executable transcript coordinate can be retained for a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskCreationUnavailable {
    /// A principal created the task outside an authenticated lane transcript.
    PrincipalActor,
    /// Spice created the task without a lane-authored command.
    SpiceActor,
}

impl TaskCreationUnavailable {
    /// Stable reason-code spelling used by human and machine task reads.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PrincipalActor => "principal-actor",
            Self::SpiceActor => "spice-actor",
        }
    }
}

/// Immutable transcript authority captured when a task is created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum TaskCreationContext {
    /// A lane-authored task was admitted after this exact accounted boundary.
    Transcript {
        /// Exact vendor session identity causally active for the creating lane.
        session: String,
        /// Source-aware position of the next complete record at task admission.
        boundary: TranscriptSourceHead,
        /// Creator team captured under the same residency guard as the boundary.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        creator_team: Option<CanonicalUuid>,
    },
    /// No authoritative transcript coordinate exists for this task.
    Unavailable {
        /// Closed reason no executable rehydration command can be offered.
        reason: TaskCreationUnavailable,
    },
}

impl TaskCreationContext {
    /// Build one explicit unavailable context.
    #[must_use]
    pub const fn unavailable(reason: TaskCreationUnavailable) -> Self {
        Self::Unavailable { reason }
    }

    /// Immutable creator team, absent for non-lane and migrated task creation.
    #[must_use]
    pub const fn creator_team(&self) -> Option<CanonicalUuid> {
        match self {
            Self::Transcript { creator_team, .. } => *creator_team,
            Self::Unavailable { .. } => None,
        }
    }
}

/// How a review closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// The reviewer found nothing to record.
    Clean,
    /// The reviewer recorded a finding.
    Finding,
}

/// What returned a held lane to seating eligibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Release {
    /// The operator woke the lane.
    Wake,
    /// The resumable vendor session or its pending operator direction changed.
    #[serde(rename = "input-changed")]
    InputChanged,
    /// A young-death hold reached its deadline.
    Expiry,
}

/// A task statement and initial workflow head were created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAdded {
    /// Sole task identity minted from the statement's birth instant.
    pub task: TaskStamp,
    /// Statement title.
    pub title: String,
    /// One plain sentence saying why the task matters.
    pub pitch: String,
    /// Longer authored context.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub description: Option<String>,
    /// Criteria that decide whether the work is done.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub term: Vec<String>,
    /// Typed facts the execution environment must satisfy together.
    #[serde(
        default,
        skip_serializing_if = "crate::EnvironmentRequirements::is_empty",
        deserialize_with = "crate::EnvironmentRequirements::present"
    )]
    pub requirements: crate::EnvironmentRequirements,
    /// Topic the task was created in, and how it was placed.
    pub topic: crate::TaskTopic,
    /// Whether the author handed this draft off for triage at creation: a
    /// creation without `--draft` whose construction could not yet advance.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub submitted: bool,
    /// Declared scheduling priority the statement opens with.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub priority: Option<crate::TaskPriority>,
    /// Taste-review hold the statement opens under.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub taste: Option<crate::TasteHold>,
    /// Tasks this one waits behind.
    pub depends: Vec<TaskStamp>,
    /// Declared Plan or Design intent the statement opens with.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub intent: Option<crate::TaskIntent>,
    /// Preparation association admitted atomically with this task.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub preparation: Option<crate::TaskPreparation>,
    /// Ordered phases the workflow head will walk.
    pub flow: Vec<Phase>,
    /// Phase the initial workflow head sits in.
    pub phase: Phase,
    /// Statement revision the head opens at.
    pub revision: u32,
    /// Creation-time transcript authority, immutable for the task's lifetime.
    pub creation: TaskCreationContext,
}

/// The workflow head moved to another phase: a claimed phase completed, or an
/// edit that cleared a taste hold withdrew the plan phase the hold inserted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPhased {
    /// Advanced task.
    pub task: TaskStamp,
    /// Phase the workflow head advanced into.
    pub phase: Phase,
}

/// Exact repository history selected at one task boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitBoundary {
    /// Lane commit that entered the boundary.
    pub head: String,
    /// Commit whose tree and ancestry left the boundary.
    pub integrated: String,
    /// Remote-tracking baseline used by the boundary, when one exists.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub upstream: Option<String>,
    /// Exact baseline commit incorporated by the boundary, when one exists.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub upstream_head: Option<String>,
    /// Lane commits ahead of the observed baseline when the boundary opened.
    pub local_commits: u32,
}

/// Accounted transcript proven at one task lifecycle boundary.
///
/// A phase interval is bounded by two of these, and the usage spent inside it
/// is whatever the accounted transcript advanced between them. Every boundary
/// is proven: a lane whose transcript has not been accounted yet opens at
/// position zero, so no heads is a real boundary, and a producer that cannot
/// read the boundary it asked for fails rather than recording *unknown*.
///
/// The single variant is the durable spelling every stored boundary carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceBoundary {
    /// Every stream accounted for the lane when the boundary was fixed.
    #[serde(rename = "proven")]
    Proven {
        /// Accounted heads, ordered by composed stream key.
        heads: Vec<TranscriptSourceHead>,
    },
}

/// One claim arrived at a shared phase barrier without advancing the workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskArrived {
    /// Task whose current round received the arrival.
    pub task: TaskStamp,
    /// Replayable completion-operation identity for `task done` arrivals.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub operation: Option<CanonicalUuid>,
    /// Exact claim identity consumed by this arrival.
    pub claimed: Seq,
    /// Lane bound to the consumed claim.
    pub lane: CanonicalUuid,
    /// Occupant that held the consumed claim.
    pub occupant: CanonicalUuid,
    /// Phase completed by the claim.
    pub phase: Phase,
    /// Current-round identity the arrival belongs to.
    pub round: Seq,
    /// Evidence the arrival rests on.
    pub validation: String,
    /// Repository history published before the claim was released.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub git: Option<GitBoundary>,
    /// Accounted transcript the completed participation closes at.
    pub source: SourceBoundary,
    /// Why a Plan participant completed without constructing work.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub disposition: Option<PlanDisposition>,
}

/// One participant retained on a shared round's terminal completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoundParticipant {
    /// Exact claim identity consumed by the participant.
    pub claimed: Seq,
    /// Actor that held the claim.
    pub actor: Actor,
    /// Lane bound to the claim.
    pub lane: CanonicalUuid,
    /// Occupant that held the claim.
    pub occupant: CanonicalUuid,
    /// Evidence the participant supplied.
    pub validation: String,
    /// Repository history published by the participant.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub git: Option<GitBoundary>,
    /// Accounted transcript the participation closes at.
    pub source: SourceBoundary,
}

/// A task completed under a re-proven active claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDone {
    /// Completed task.
    pub task: TaskStamp,
    /// Evidence the completion rests on.
    pub validation: String,
    /// Replayable terminal response for the completion operation.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub terminal: Option<TaskCompletionTerminal>,
    /// Repository history published before the workflow advanced.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub git: Option<GitBoundary>,
    /// Accounted transcript the completed phase closes at.
    pub source: SourceBoundary,
    /// Why a Plan's plan phase completed without constructing work.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub disposition: Option<PlanDisposition>,
}

/// A shared phase round reached its barrier and advanced workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRoundCompleted {
    /// Task whose workflow advanced.
    pub task: TaskStamp,
    /// Completed phase.
    pub phase: Phase,
    /// Completed round identity.
    pub round: Seq,
    /// Every accepted arrival, in durable arrival order.
    pub participants: Vec<RoundParticipant>,
    /// Workflow standing after the barrier transition.
    pub status: TaskCompletionStatus,
    /// Next action rendered for the participant that closed the barrier.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub next: Option<Box<str>>,
    /// Why a Plan's plan phase completed without constructing work.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub disposition: Option<PlanDisposition>,
}

/// Why a Plan completed its plan phase without constructing work.
///
/// The Plan's own lane records it on the completion that closes the plan
/// phase; a clean review then settles the Plan with no outputs. `no-work`
/// stands alone, and every other kind names what already answers the need.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PlanDisposition {
    /// The stated need warrants no change.
    NoWork {},
    /// Another task or commit already states the same need.
    Duplicate {
        /// The task or commit it duplicates.
        cover: DispositionCover,
    },
    /// Existing work already delivers the need.
    Covered {
        /// The task or commit that delivers it.
        cover: DispositionCover,
    },
    /// Existing filed work delivers the need later.
    Deferred {
        /// The task or commit that will deliver it.
        cover: DispositionCover,
    },
}

impl PlanDisposition {
    /// Stable kind spelling shared by the CLI flag, reads, and the app.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::NoWork {} => "no-work",
            Self::Duplicate { .. } => "duplicate",
            Self::Covered { .. } => "covered",
            Self::Deferred { .. } => "deferred",
        }
    }

    /// What already answers the need; absent only for `no-work`.
    #[must_use]
    pub const fn cover(&self) -> Option<&DispositionCover> {
        match self {
            Self::NoWork {} => None,
            Self::Duplicate { cover } | Self::Covered { cover } | Self::Deferred { cover } => {
                Some(cover)
            }
        }
    }

    /// Build one disposition from its kind spelling and cover.
    ///
    /// # Errors
    ///
    /// Refuses an unknown kind, a cover on `no-work`, and a missing cover on
    /// every other kind.
    pub fn from_parts(kind: &str, cover: Option<DispositionCover>) -> Result<Self, &'static str> {
        match (kind, cover) {
            ("no-work", None) => Ok(Self::NoWork {}),
            ("no-work", Some(_)) => Err("no-work names no cover"),
            ("duplicate", Some(cover)) => Ok(Self::Duplicate { cover }),
            ("covered", Some(cover)) => Ok(Self::Covered { cover }),
            ("deferred", Some(cover)) => Ok(Self::Deferred { cover }),
            ("duplicate" | "covered" | "deferred", None) => {
                Err("duplicate, covered, and deferred name the task or commit that covers them")
            }
            _ => Err("name one of no-work, duplicate, covered, or deferred"),
        }
    }
}

/// The existing task or commit a disposition rests on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum DispositionCover {
    /// A task by its durable birth stamp.
    Task(TaskStamp),
    /// A commit by its full lowercase hexadecimal object id.
    Commit(String),
}

impl DispositionCover {
    /// Whether a commit cover spells one full SHA-1 or SHA-256 object id.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        match self {
            Self::Task(_) => true,
            Self::Commit(id) => {
                matches!(id.len(), 40 | 64)
                    && id
                        .bytes()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            }
        }
    }
}

/// Workflow status retained by a replayable task completion.
///
/// A claimant completion either advances to another pending phase or
/// positively completes the workflow; deletion and negative settlement are
/// operator actions and cannot occur through this fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskCompletionStatus {
    /// The completion advanced to another live phase.
    Pending,
    /// The completion positively settled the task.
    Completed,
}

impl TaskCompletionStatus {
    /// Stable response spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
        }
    }
}

impl std::fmt::Display for TaskCompletionStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Durable fields needed to reconstruct one terminal task response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCompletionTerminal {
    /// Identity returned by the initiating running acknowledgement.
    pub operation: CanonicalUuid,
    /// Workflow status returned by the completing transaction.
    pub status: TaskCompletionStatus,
    /// Exact next action projected by that transaction.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub next: Option<Box<str>>,
}

/// A review verdict landed with reviewer and assignment witnesses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskReviewed {
    /// Reviewed task.
    pub task: TaskStamp,
    /// Principal that reviewed.
    pub reviewer: Actor,
    /// How the review closed.
    pub verdict: Verdict,
    /// Workflow status the verdict left: `completed` exactly when this review
    /// positively settled the workflow, so a fact-only fold can treat a clean
    /// final review as settlement without knowing the task's flow; `pending`
    /// for a finding or a clean review that advanced to a later phase.
    pub status: TaskCompletionStatus,
    /// Recorded finding; absent on a clean verdict.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub finding: Option<String>,
    /// Reviewer's note on a clean verdict; a finding carries its note in `finding`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub note: Option<String>,
    /// Correction tasks the finding linked, in the order the reviewer named them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub followups: Vec<TaskStamp>,
    /// Shared review round a clean verdict arrived in without moving the
    /// workflow itself; such a verdict changes no task content, so it is not a
    /// task mutation. Absent on every verdict that alone decides its phase.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub round: Option<Seq>,
    /// Repository history published before the review completed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub git: Option<GitBoundary>,
    /// Accounted transcript the reviewed phase closes at.
    pub source: SourceBoundary,
}

/// The allocator seated a reviewer under the anti-self-review witness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAssigned {
    /// Assigned task.
    pub task: TaskStamp,
    /// Principal seated as reviewer.
    pub reviewer: Actor,
    /// Phase the assignment covers.
    pub phase: Phase,
}

/// A task received a wakeable deferral.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDeferred {
    /// Deferred task.
    pub task: TaskStamp,
    /// Instant the deferral becomes wakeable.
    pub until: Instant,
}

/// A task's deferral cleared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskWoken {
    /// Woken task.
    pub task: TaskStamp,
}

/// The author handed an uncontrolled draft off for triage: the draft phase's
/// one monotone sub-state, set once and never cleared, irrelevant once the
/// row leaves draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSubmitted {
    /// Submitted draft.
    pub task: TaskStamp,
}

/// One guarded replacement of a task's prospective workflow route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskFlowEdit {
    /// Complete route before the edit.
    pub previous: Vec<Phase>,
    /// Complete route admitted by the edit.
    pub current: Vec<Phase>,
}

/// An unsettled task statement changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEdited {
    /// Edited task.
    pub task: TaskStamp,
    /// Statement revision the edit produced.
    pub revision: u32,
    /// Complete dependency set when this edit changed it.
    ///
    /// Ordinary statement edits and facts written before relationship-aware
    /// invalidation omit this field.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub dependencies: Option<Vec<TaskStamp>>,
    /// Complete previous and current routes when this edit changed the workflow.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub flow: Option<TaskFlowEdit>,
    /// Explicit intent change this edit admitted.
    ///
    /// The fold of creation intent and these changes is the statement head's
    /// declared intent; edits that leave intent alone omit this field.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub intent: Option<crate::TaskIntentEdit>,
}

/// A task was withdrawn from the board with a recorded reason.
///
/// The journal is the authority and a fact is permanent, so a withdrawal is a
/// terminal workflow status rather than the removal of a row: the statement,
/// its history, and this reason all stay readable. `claimed` records the
/// possessing actor when the withdrawal overrode a live claim, so a forced
/// removal names whose work it took away rather than reporting a bare success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDeleted {
    /// Withdrawn task.
    pub task: TaskStamp,
    /// Operator's stated reason for the withdrawal.
    pub reason: String,
    /// Earliest-won actor whose live claim the withdrawal overrode, when it
    /// overrode one; every overridden claim is freed under this fact.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub claimed: Option<Actor>,
}

/// The operator closed a task as completed without holding its claim.
///
/// Completion ordinarily rests on the claimant's proof (`task::done`); this
/// is the operator's own lever, so the fact records the phase it cut short and
/// its stated validation instead. `claimed` names the earliest-won actor whose
/// live claim the closure overrode, when it overrode one; every overridden
/// claim is freed under this fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskClosed {
    /// Closed task.
    pub task: TaskStamp,
    /// Operator's stated evidence or reason the task is complete.
    pub validation: String,
    /// Phase the workflow held when the operator closed it.
    pub phase: Phase,
    /// Earliest-won actor whose live claim the closure overrode, when it
    /// overrode one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub claimed: Option<Actor>,
}

/// How the operator concludes an open Design.
///
/// `done` is the positive conclusion after its outputs; `collapsed` is the
/// negative one, when the premise did not hold, and always records why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "lowercase", deny_unknown_fields)]
pub enum DesignConclusion {
    /// The Design is done.
    Done {},
    /// The Design collapsed because its premise did not hold.
    Collapsed {
        /// Why the premise did not hold; never empty.
        reason: String,
    },
}

/// The operator concluded an open, unheld Design.
///
/// A Design stays open through any number of lane passes and selections; this
/// fact is its one terminal act. It never overrides a claim: a held Design
/// refuses conclusion until its holder checkpoints and frees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskConcluded {
    /// Concluded Design.
    pub task: TaskStamp,
    /// The operator's conclusion.
    pub conclusion: DesignConclusion,
}

/// The repository OOPS generation advanced between vacant and current state.
///
/// The singleton projection folds only these transitions. `from` must equal
/// the preceding head, `to` must alternate between vacancy and one Design, and
/// `revision` advances exactly once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OopsGenerationAdvanced {
    /// Revision assigned to this transition.
    pub revision: u64,
    /// Generation current before this transition.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub from: Option<TaskStamp>,
    /// Generation current after this transition.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub to: Option<TaskStamp>,
}

/// Exclusive possession was minted from the current eligibility and claim heads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimWon {
    /// Possessed task.
    pub task: TaskStamp,
    /// Winning actor.
    pub actor: Actor,
    /// Lane the claim is bound to.
    pub lane: CanonicalUuid,
    /// Occupant holding the lane.
    pub occupant: CanonicalUuid,
    /// Resident epoch that minted the claim.
    pub epoch: CanonicalUuid,
    /// Topic and phase occupied when the claim was minted.
    pub cell: crate::ClaimCell,
    /// Lease expiry the possession runs to.
    pub until: Instant,
    /// Repository history fixed when possession was minted.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub git: Option<GitBoundary>,
    /// Dirty-handoff transaction whose replay this claim authorizes.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub recovery: Option<String>,
    /// Accounted transcript the possessed phase opens at.
    pub source: SourceBoundary,
}

/// Possession rebound to current coordination authority with lineage preserved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimCarried {
    /// Carried task.
    pub task: TaskStamp,
    /// Lane the possession stays bound to.
    pub lane: CanonicalUuid,
    /// Current occupant; it may be unchanged across resident replacement.
    pub occupant: CanonicalUuid,
    /// Occupant the possession came from; equal when only the epoch changed.
    pub ancestor: CanonicalUuid,
    /// Resident epoch that carried it.
    pub epoch: CanonicalUuid,
    /// Accounted transcript the current authority takes over at.
    ///
    /// Carrying the possession does not end the phase, so this is a boundary
    /// *within* one interval rather than a closing one: it names the exact
    /// source locus where the preceding authority's accounting stops and the
    /// current authority's begins.
    pub source: SourceBoundary,
}

/// Possession was rebound by its lane after residency moved ahead of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRebound {
    /// Rebound task.
    pub task: TaskStamp,
    /// Lane the possession remains bound to.
    pub lane: CanonicalUuid,
    /// Current occupant receiving possession.
    pub occupant: CanonicalUuid,
    /// Occupant named by the predecessor authority being recovered.
    pub ancestor: CanonicalUuid,
    /// Current resident epoch authorizing recovery.
    pub epoch: CanonicalUuid,
    /// Resulting lease expiry.
    pub until: Instant,
    /// Durable maximum the recovery stayed within.
    pub maximum: Instant,
    /// Accounted transcript boundary where current authority takes over.
    pub source: SourceBoundary,
}

/// A holder accepted the task generation currently governing its claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimAcknowledged {
    /// Task whose current statement and workflow were accepted.
    pub task: TaskStamp,
    /// Current task generation the holder accepted.
    pub generation: u64,
}

/// Commits and an optional dirty worktree snapshot detached on task release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskParkedCommits {
    /// Newest published commit the lane shared; release returned the lane here.
    pub baseline: String,
    /// Original saved tip, recoverable by object ID while Git retains it.
    pub head: String,
    /// Original commit identities in oldest-first ancestry order.
    pub commits: Vec<String>,
    /// Unreferenced worktree snapshot, restored without creating a lane commit.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub snapshot: Option<String>,
}

/// Possession was released and the task became eligible again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimFreed {
    /// Released task.
    pub task: TaskStamp,
    /// Actor that held the possession.
    pub actor: Actor,
    /// Commits retained for this task while no lane owns it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub parked: Option<TaskParkedCommits>,
}

/// The lease deadline passed and the resident expired possession.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimLapsed {
    /// Expired task.
    pub task: TaskStamp,
    /// Occupant that lost possession.
    pub occupant: CanonicalUuid,
    /// Lease deadline that passed.
    pub expiry: Instant,
}

/// The resident extended a live, epoch-bound lease within its maximum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRenewed {
    /// Renewed task.
    pub task: TaskStamp,
    /// Resident epoch permitted to advance the lease.
    pub epoch: CanonicalUuid,
    /// Extended expiry.
    pub until: Instant,
    /// Maximum the extension stayed within.
    pub maximum: Instant,
}

/// One flow phase's completion target changed.
///
/// The target is how many distinct lanes complete that phase. It is staffing,
/// not workflow: it advances no task generation and stales no holder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionTargeted {
    /// Task whose staffing changed.
    pub task: TaskStamp,
    /// Flow phase the target governs.
    pub phase: Phase,
    /// Distinct lanes that phase now needs.
    pub target: std::num::NonZeroU32,
}
