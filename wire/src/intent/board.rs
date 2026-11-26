//! Actor-facing board intent, evidence, and presentation DTOs.

use serde::{Deserialize, Serialize};

use crate::{Actor, TaskHandle};

mod design;
mod detail;
mod relations;
mod timeline;

pub use design::{
    DesignRequest, DesignRequestAsk, DesignStanding, is_open_design, released_construction_root,
};
pub use detail::{
    TaskAcknowledgment, TaskDetail, TaskDetailAnswer, TaskDetailClosure, TaskDetailDeletion,
    TaskDetailDisposition, TaskDetailOrigin, TaskDetailPass, TaskDetailPost, TaskDetailReview,
    TaskDetailStep, TaskStep,
};
pub use relations::{
    ORIGIN_REQUEST_PREVIEW_LIMIT, OriginRequest, TASK_RELATION_DEPTH_LIMIT,
    TASK_RELATION_EDGE_LIMIT, TASK_RELATION_FRONTIER_LIMIT, TASK_RELATION_NODE_LIMIT,
    TASK_RELATION_ROOT_LIMIT, TaskLifecycleKind, TaskRelationEdge, TaskRelationFrontier,
    TaskRelationKind, TaskRelationNode, TaskRelationNodeRef, TaskRelationTruncation, TaskRelations,
    TaskRelationsAnswer,
};
pub use timeline::{
    TASK_TIMELINE_LANE_LIMIT, TASK_TIMELINE_PAGE_LIMIT_MAX, TaskTimeline, TaskTimelineAnswer,
    TaskTimelineEntry, TaskTimelineEvent,
};

/// Largest terminal-task page one board read accepts.
pub const TASK_HISTORY_PAGE_LIMIT_MAX: u16 = 100;
/// Largest UTF-8 search query one terminal-task history read accepts.
pub const TASK_HISTORY_QUERY_BYTES_MAX: usize = 256;

/// Stable keyset cursor for terminal tasks in newest-workflow-first order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskHistoryCursor {
    /// Durable workflow head ordering this terminal task.
    pub workflow: crate::Seq,
    /// Durable task identity breaking equal-head ties.
    pub task: crate::TaskStamp,
}

/// Metadata for one bounded terminal-task page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskHistoryPage {
    /// Cursor supplied by the request, absent for the first page.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub before: Option<TaskHistoryCursor>,
    /// Accepted maximum number of rows in this page.
    pub limit: u16,
    /// Total terminal rows at this answer's snapshot barrier.
    pub total: u64,
    /// Cursor for the next page, absent once this snapshot is exhausted.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub next: Option<TaskHistoryCursor>,
}

const fn no_rows(value: &u16) -> bool {
    *value == 0
}

/// Read an explicit projection of one authoritative board snapshot.
///
/// `All` remains complete; `Next`, `Open`, and `History` name their exclusions
/// explicitly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum BoardRead {
    /// The complete board, optionally restricted to a public handle family.
    /// Its size grows with retained history, so only an explicit export reads
    /// it.
    All {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        family: Option<String>,
    },
    /// The allocator's bounded live-frontier decision for one principal.
    Next,
    /// Every currently nonterminal task with the settled prerequisites,
    /// preparation parents, and controllers it is judged by, optionally
    /// enriched with one bounded newest-first window of terminal rows.
    Open {
        /// Maximum recently finished rows returned beside the open inventory.
        #[serde(default, skip_serializing_if = "no_rows")]
        recent_finished: u16,
        /// Public lane code naming the first row of a lane-list continuation.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        after: Option<crate::OccupantAddress>,
    },
    /// One bounded keyset page of terminal tasks.
    History {
        /// Last row returned by the preceding page.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        before: Option<TaskHistoryCursor>,
        /// Case-insensitive substring matched against handle, title, topic,
        /// and description. Empty or whitespace-only text is unfiltered.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        query: Option<String>,
        /// Include only tasks completed at or before this instant.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        completed_before: Option<crate::Instant>,
        /// Include only tasks the app team created, or whose most recent
        /// winning claim was held by a lane now in that team. Absent reads
        /// every team's history.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        team: Option<crate::CanonicalUuid>,
        /// Maximum rows returned by this page.
        limit: u16,
    },
    /// Named durable identities plus the context required by the caller.
    Tasks {
        tasks: Vec<crate::TaskStamp>,
        /// Include the transitive dependency closure of the selected tasks.
        dependencies: bool,
        /// Also select every active claim held by the authenticated principal.
        claims: bool,
        /// Also return each named task's acknowledgments, closure, and
        /// workflow steps; its conversation is its room.
        history: bool,
    },
    /// Trust rulings and residency authority, without task rows.
    Trust,
    /// Residency authority plus the lane one occupant code, current or
    /// historical, selects; without task rows.
    Lane {
        /// Code naming the lane through any occupant it has started.
        code: crate::OccupantAddress,
    },
}

/// Render one deterministic graph from the validated current board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardGraph {
    /// Closed board graph view name.
    pub view: String,
    /// Include settled tasks and their durable historical relations.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all: bool,
    /// Historical barrier requested by the caller, when present.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub at: Option<String>,
}

/// Compose one deterministic handout from the validated current board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardHandout {
    /// Declared output spelling selected by the caller.
    pub output: String,
    /// Historical barrier requested by the caller, when present.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub at: Option<String>,
}

/// Emit the canonical authored-document ledger for one task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocLedger {
    /// Task whose current statement becomes canonical Markdown.
    pub task: TaskHandle,
}

/// One authoritative head body together with the fact that established it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Positioned<T> {
    /// Fact position stored beside the current head.
    pub position: crate::Seq,
    /// Typed current head body.
    pub body: T,
}

/// Whether one exact task transcript coordinate can be rehydrated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum TaskTranscriptRecovery {
    /// The registered vendor session and exact source boundary are available.
    Transcript {
        /// Vendor session identity owning the source.
        session: String,
    },
    /// The journal preserves the boundary, but no executable transcript lookup exists.
    Unavailable {
        /// Evidence-specific reason the lookup cannot be rendered.
        reason: String,
    },
}

/// Compact provenance for the claim that opened the current task phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskClaimContext {
    /// ClaimWon fact opening the current attempt.
    pub claimed: crate::Seq,
    /// Lane bound when the claim was won.
    pub lane: crate::CanonicalUuid,
    /// Occupant bound when the claim was won.
    pub occupant: crate::CanonicalUuid,
    /// Resident generation that minted the claim.
    pub epoch: crate::CanonicalUuid,
    /// Complete accounted source boundary recorded on the claim.
    pub source: crate::body::SourceBoundary,
    /// Executable recovery coordinate, or an explicit reason none exists.
    pub recovery: TaskTranscriptRecovery,
}

/// Lifecycle boundary whose Git objects supply the current task's repository context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskGitContextKind {
    /// Repository state observed when the claim opened.
    Claim,
    /// Repository state published when an implementation phase completed.
    Completion,
    /// Repository state published when a review completed.
    Review,
}

impl TaskGitContextKind {
    /// Stable text spelling used by task detail output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::Completion => "completion",
            Self::Review => "review",
        }
    }
}

/// One compact, fact-attributed Git boundary selected for task recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskGitContext {
    /// Fact that recorded this boundary.
    pub fact: crate::Seq,
    /// Lifecycle role of that fact.
    pub kind: TaskGitContextKind,
    /// Exact repository object identities recorded by the lifecycle fact.
    pub boundary: crate::body::GitBoundary,
}

/// Current task statement evidence, with the title carried once beside it.
///
/// Environmental requirements remain on the authoritative [`crate::TaskHead`];
/// this actor view carries none rather than a partial requirements model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardStatement {
    /// Public task name derived from its topic and `born`.
    pub handle: crate::TaskHandle,
    /// Sole durable task identity.
    pub born: crate::TaskStamp,
    /// Fact that first placed this task in the topic.
    pub topic_origin: crate::Seq,
    /// One plain sentence saying who can do what once the task lands and why.
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
    /// Typed environmental facts required before allocation.
    #[serde(
        default,
        skip_serializing_if = "crate::EnvironmentRequirements::is_empty",
        deserialize_with = "crate::EnvironmentRequirements::present"
    )]
    pub requirements: crate::EnvironmentRequirements,
    /// Canonical resource-pool shard counts required before allocation.
    #[serde(default, skip_serializing_if = "crate::ResourceRequirements::is_empty")]
    pub resources: crate::ResourceRequirements,
    /// Topic node the task lives in, with its current path.
    pub topic: crate::TaskTopic,
    /// Declared scheduling priority.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub priority: Option<crate::TaskPriority>,
    /// Board-owned taste-review hold.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub taste: Option<crate::TasteHold>,
    /// Tasks that must complete first.
    pub dependencies: Vec<crate::TaskStamp>,
    /// Declared Plan or Design intent.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub intent: Option<crate::TaskIntent>,
    /// Explicit native preparation context, separate from scheduling.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub preparation: Option<crate::TaskPreparation>,
    /// Creation provenance, immutable across statement revisions.
    pub provenance: crate::TaskProvenance,
    /// Creation-time transcript authority, immutable across statement revisions.
    pub creation: crate::body::TaskCreationContext,
    /// Fact that established this statement revision.
    pub revision: crate::Seq,
}

impl BoardStatement {
    /// Project a statement head at the phase its workflow currently occupies,
    /// which decides the rendered handle.
    #[must_use]
    pub fn from_head(statement: &crate::TaskHead, phase: crate::Phase) -> Self {
        Self {
            handle: statement.handle(phase),
            born: statement.born.clone(),
            topic_origin: statement.topic_origin,
            pitch: statement.pitch.clone(),
            description: statement.description.clone(),
            term: statement.term.clone(),
            requirements: statement.requirements.clone(),
            resources: statement.resources.clone(),
            topic: statement.topic.clone(),
            priority: statement.priority,
            taste: statement.taste.clone(),
            dependencies: statement.dependencies.clone(),
            intent: statement.intent,
            preparation: statement.preparation.clone(),
            provenance: statement.provenance.clone(),
            creation: statement.creation.clone(),
            revision: statement.revision,
        }
    }

    /// Reconstruct a client-side statement from this row and its title.
    ///
    /// This actor view predates environmental standing, so its client-side
    /// reconstruction is unconstrained. Authoritative allocation always reads
    /// the journal's [`crate::TaskHead`] rather than this presentation copy.
    #[must_use]
    pub fn with_title(&self, title: &str) -> crate::TaskHead {
        crate::TaskHead {
            born: self.born.clone(),
            topic_origin: self.topic_origin,
            title: title.to_owned(),
            pitch: self.pitch.clone(),
            description: self.description.clone(),
            term: self.term.clone(),
            requirements: self.requirements.clone(),
            resources: self.resources.clone(),
            topic: self.topic.clone(),
            priority: self.priority,
            taste: self.taste.clone(),
            dependencies: self.dependencies.clone(),
            intent: self.intent,
            preparation: self.preparation.clone(),
            provenance: self.provenance.clone(),
            creation: self.creation.clone(),
            revision: self.revision,
        }
    }
}

/// One complete current task evidence bundle in a board read answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardTask {
    /// Policy ownership at this read; absent on raw head projections.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub ownership: Option<super::TaskOwnership>,
    /// Server-resolved actor thread that created this task.
    pub origin_thread: String,
    /// Immutable creator team for lane-authored tasks.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub creator_team: Option<crate::CanonicalUuid>,
    /// Authored title, carried beside the statement rather than inside it.
    pub presentation_title: String,
    /// Current statement evidence; the title sits in `presentation_title`.
    pub statement: Positioned<BoardStatement>,
    pub workflow: Positioned<crate::WorkflowHead>,
    /// Phase completion targets and the current round's accepted arrivals.
    pub completion: Positioned<crate::CompletionHead>,
    /// Every retained claim: those active and those ended since the latest
    /// win, in identity order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<BoardClaim>,
    /// Commit instant of this task's first durable claim win.
    ///
    /// Later winners, renewals, carried claims, and phase transitions never
    /// replace it. Absent before the task has ever been claimed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub original_claimed_at: Option<crate::Instant>,
    /// Every occupied-lane invitation into this task, in publication order,
    /// on reads that name the task; empty on whole-board reads.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invitations: Vec<crate::TaskInvitation>,
    /// Every task joined to this one by a declared unlabeled relation, in
    /// identity order, on reads that name the task; empty on whole-board reads.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<RelatedTask>,
    /// Whether the current freed claim came from an explicit release command.
    #[serde(default, skip_serializing_if = "is_false")]
    pub explicit_release: bool,
    /// This Design's pass-cycle evidence, so a client allocates from the
    /// same projection the resident does; empty for any other task.
    #[serde(
        default,
        skip_serializing_if = "crate::rooms::DesignCycleFacts::is_empty"
    )]
    pub design_cycle: crate::rooms::DesignCycleFacts,
    /// Current-statement question coverage, independent of research readiness.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub design_coverage: Option<crate::rooms::DesignCoverage>,
    /// The reason recorded when the operator concluded this Design collapsed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub collapse_reason: Option<String>,
    /// Pending work this task's negative settlement would close atomically.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controlled: Vec<TaskHandle>,
    /// Claim-opening transcript coordinates, without embedded transcript content.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub claim_context: Option<TaskClaimContext>,
    /// Latest repository lifecycle boundary, without embedded Git object content.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub git_context: Option<TaskGitContext>,
    /// Commits saved by object ID after an explicit release.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub parked_commits: Option<crate::body::TaskParkedCommits>,
}

/// One task a declared relation joins to another. The relation carries no
/// dependency, requirement, hold, or completion credit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelatedTask {
    pub task: TaskHandle,
    pub title: String,
}

/// Current heads needed to witness one `task next` decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NextWorkTask {
    /// Owners at the same policy generation as this allocator decision.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub ownership: Option<super::TaskOwnership>,
    pub statement: Positioned<crate::TaskHead>,
    pub workflow: Positioned<crate::WorkflowHead>,
    pub completion: Positioned<crate::CompletionHead>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<BoardClaim>,
}

/// One retained claim and the lease it holds while active.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardClaim {
    pub claim: Positioned<crate::ClaimHead>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub lease: Option<Positioned<crate::LeaseHead>>,
}

/// Exact pending execution-board classification when the allocator has no
/// candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NextWorkCounts {
    /// Current claims.
    pub active: usize,
    /// Lapsed claims.
    pub stale: usize,
    /// Closed execution gates with no advancing path.
    pub blocked: usize,
    /// Advancing prerequisite chains and Plan control.
    pub workflow_waiting: usize,
    /// Open Designs in each pass-cycle state.
    pub designs: crate::rooms::DesignCycleCounts,
    /// Explicitly deferred work.
    pub deferred: usize,
    /// Unheld rounds with accepted arrivals that still need distinct participants.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rounds_waiting: Vec<CompletionWait>,
    /// Pending work the principal's current session boarded that another
    /// actor now holds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boarded_elsewhere: Vec<BoardedElsewhere>,
    /// Topics with pending unheld work that no team owns, once per topic.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unowned_topics: Vec<crate::Topic>,
}

/// An unmet current completion round, projected from its durable head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionWait {
    pub task: crate::TaskHandle,
    pub phase: crate::Phase,
    pub arrived: usize,
    pub target: u32,
    /// The querying actor already contributed its one arrival to this round.
    pub actor_arrived: bool,
}

/// One task the principal boarded and another actor holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardedElsewhere {
    pub task: crate::TaskHandle,
    /// Actor holding the task's active claim.
    pub holder: crate::Actor,
}

/// One closed allocator decision captured at the board snapshot barrier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum NextWorkState {
    /// The principal already owns this sole active claim.
    Active { task: NextWorkTask },
    /// The globally highest-ranked eligible task and its claim witnesses.
    Candidate { task: NextWorkTask },
    /// An independent handoff waits for a currently eligible peer until the
    /// durable peer-first deadline.
    AwaitingPeer {
        task: NextWorkTask,
        until: crate::Instant,
    },
    /// A completed claim owes one room contribution or leave before allocation.
    DesignRequest { request: DesignRequest },
    /// No task is eligible under this team policy and board standing.
    Dry {
        board: NextWorkCounts,
        drive: bool,
        drain: bool,
    },
}

/// Why no team automatically allocates one open, unclaimed row: only the
/// operator can move it, by turning a drain on or changing team policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum Unallocated {
    /// These live teams' policy covers the row, but their drain is off.
    DrainOff { teams: Vec<crate::CanonicalUuid> },
    /// No live team's policy covers the row's topic and phase.
    Uncovered,
}

/// Server-resolved default read route for one board principal: activated
/// rows, plus rows created on the principal's own origin thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardActorRoute {
    /// Actor identity the route reads for.
    pub actor: String,
    /// Exact creation-origin thread selected by the route.
    pub origin_thread: String,
}

impl BoardClaim {
    /// Whether this claim still holds possession.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.claim.body.disposition == crate::ClaimDisposition::Active
    }

    /// The one active claim `actor` holds among `claims`.
    #[must_use]
    pub fn held_by<'a>(claims: &'a [Self], actor: &Actor) -> Option<&'a Self> {
        claims
            .iter()
            .find(|claim| claim.is_active() && claim.claim.body.actor == *actor)
    }

    /// The claim-plane head: the newest head across `claims`, absent before
    /// the first win.
    #[must_use]
    pub fn plane(claims: &[Self]) -> Option<crate::Seq> {
        claims.iter().map(|claim| claim.claim.position).max()
    }

    /// The claim-plane witness over every claim of `task`.
    #[must_use]
    pub fn plane_witness(task: &crate::TaskStamp, claims: &[Self]) -> crate::Witness {
        crate::Witness::Claim {
            task: task.clone(),
            claimed: None,
            head: Self::plane(claims),
        }
    }

    /// The witness binding exactly this claim's head on `task`.
    #[must_use]
    pub fn witness(&self, task: &crate::TaskStamp) -> crate::Witness {
        crate::Witness::Claim {
            task: task.clone(),
            claimed: Some(self.claim.body.claimed),
            head: Some(self.claim.position),
        }
    }

    /// The witness binding this claim's lease head on `task`, when it has one.
    #[must_use]
    pub fn lease_witness(&self, task: &crate::TaskStamp) -> Option<crate::Witness> {
        self.lease.as_ref().map(|lease| crate::Witness::Lease {
            task: task.clone(),
            claimed: self.claim.body.claimed,
            head: lease.position,
        })
    }
}

impl BoardTask {
    /// Project authoritative heads into one task row.
    #[must_use]
    pub fn from_authoritative(
        origin_thread: String,
        creator_team: Option<crate::CanonicalUuid>,
        statement: Positioned<crate::TaskHead>,
        workflow: Positioned<crate::WorkflowHead>,
        completion: Positioned<crate::CompletionHead>,
        claims: Vec<BoardClaim>,
    ) -> Self {
        let presentation_title = statement.body.title.clone();
        let statement = Positioned {
            position: statement.position,
            body: BoardStatement::from_head(&statement.body, workflow.body.phase),
        };
        Self {
            ownership: None,
            origin_thread,
            creator_team,
            presentation_title,
            statement,
            workflow,
            completion,
            claims,
            original_claimed_at: None,
            invitations: Vec::new(),
            related: Vec::new(),
            explicit_release: false,
            design_cycle: crate::rooms::DesignCycleFacts::default(),
            design_coverage: None,
            collapse_reason: None,
            controlled: Vec::new(),
            claim_context: None,
            git_context: None,
            parked_commits: None,
        }
    }

    /// The one active claim `actor` holds on this task.
    #[must_use]
    pub fn held_by(&self, actor: &Actor) -> Option<&BoardClaim> {
        BoardClaim::held_by(&self.claims, actor)
    }

    /// Every active claim on this task, in identity order.
    pub fn active_claims(&self) -> impl Iterator<Item = &BoardClaim> {
        self.claims.iter().filter(|claim| claim.is_active())
    }

    /// The most recently won retained claim, whatever its disposition.
    #[must_use]
    pub fn latest_claim(&self) -> Option<&BoardClaim> {
        self.claims.last()
    }

    /// Attach whether the current claim head records an explicit release.
    #[must_use]
    pub const fn with_explicit_release(mut self, explicit_release: bool) -> Self {
        self.explicit_release = explicit_release;
        self
    }

    /// Attach the first durable claim instant projected from task history.
    #[must_use]
    pub const fn with_original_claimed_at(mut self, at: Option<crate::Instant>) -> Self {
        self.original_claimed_at = at;
        self
    }

    /// Attach this Design's pass-cycle evidence.
    #[must_use]
    pub const fn with_design_cycle(mut self, cycle: crate::rooms::DesignCycleFacts) -> Self {
        self.design_cycle = cycle;
        self
    }

    /// Attach coverage computed against this row's current questions.
    #[must_use]
    pub fn with_design_coverage(mut self, coverage: Option<crate::rooms::DesignCoverage>) -> Self {
        self.design_coverage = coverage;
        self
    }

    /// The research obligation for the holder of this Design's current pass.
    #[must_use]
    pub fn design_research(&self) -> Option<String> {
        self.design_coverage.as_ref().map(|coverage| {
            coverage.research_guidance(&self.statement.body.term, &self.statement.body.handle)
        })
    }

    /// Attach the reason the operator's collapse of this Design recorded.
    #[must_use]
    pub fn with_collapse_reason(mut self, reason: Option<String>) -> Self {
        self.collapse_reason = reason;
        self
    }

    /// Attach compact recovery references derived from the same authoritative snapshot.
    #[must_use]
    pub fn with_context(
        mut self,
        claim_context: Option<TaskClaimContext>,
        git_context: Option<TaskGitContext>,
        parked_commits: Option<crate::body::TaskParkedCommits>,
    ) -> Self {
        self.claim_context = claim_context;
        self.git_context = git_context;
        self.parked_commits = parked_commits;
        self
    }

    /// Client-side statement reconstructed from actor-facing evidence.
    #[must_use]
    pub fn client_statement(&self) -> crate::TaskHead {
        self.statement.body.with_title(&self.presentation_title)
    }

    /// Authored title of this task.
    #[must_use]
    pub fn presentation_title(&self) -> &str {
        &self.presentation_title
    }

    /// Whether this row belongs to pending activated work inventory.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.workflow.body.is_activated() && self.workflow.body.status.is_pending()
    }

    /// How this task settled, as every surface names it; `None` while live.
    #[must_use]
    pub fn settlement(&self) -> Option<crate::Settlement> {
        self.workflow
            .body
            .status
            .settlement(self.collapse_reason.as_deref())
    }

    /// The task's status word: its settlement once settled, else `pending`.
    #[must_use]
    pub fn status_word(&self) -> &'static str {
        self.settlement().map_or_else(
            || self.workflow.body.status.as_str(),
            crate::Settlement::as_str,
        )
    }

    /// This Design's lifecycle standing; `None` for a task without Design
    /// intent.
    #[must_use]
    pub fn design_standing(&self) -> Option<DesignStanding> {
        DesignStanding::of(
            self.statement.body.intent,
            &self.workflow.body,
            self.collapse_reason.as_deref(),
        )
    }

    /// Whether a lane may take, or already holds, this Design's pass.
    /// Deferral and possession remain independent: a non-holder deferral asks
    /// the live holder to checkpoint and free its pass.
    #[must_use]
    pub fn design_availability(&self) -> crate::rooms::DesignAvailability {
        match (
            self.workflow.body.deferral.is_some(),
            self.active_claims().next().is_some(),
        ) {
            (true, true) => crate::rooms::DesignAvailability::DeferredClaimed,
            (true, false) => crate::rooms::DesignAvailability::DeferredUnclaimed,
            (false, true) => crate::rooms::DesignAvailability::Claimed,
            (false, false) => crate::rooms::DesignAvailability::Unclaimed,
        }
    }
}

/// Validated current board heads returned from one snapshot barrier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardAnswer {
    /// Journal schema that owns the returned heads.
    pub schema: u32,
    /// Greatest durable fact position covered by every returned head.
    pub cursor: crate::Seq,
    /// Authoritative statement, workflow and topic projection barrier, before row filtering.
    pub task_barrier: crate::Digest,
    /// Current repository OOPS generation, absent before its first transition.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub oops_generation: Option<Positioned<crate::OopsGenerationHead>>,
    /// Principal derived from the capability that requested this snapshot.
    pub principal: Actor,
    /// Complete actor-specific route captured by the snapshot authority.
    pub actor_route: BoardActorRoute,
    /// Typed allocator decision, present only for [`BoardRead::Next`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub next: Option<NextWorkState>,
    /// Selected current task evidence; history pages are newest-workflow-first,
    /// while every other scope retains durable task-stamp order.
    pub tasks: Vec<BoardTask>,
    /// Open-read terminal enrichment in newest-workflow-first order. Every
    /// stamp names one row in `tasks`; empty for every other read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub finished: Vec<crate::TaskStamp>,
    /// Open-read allocation order: the successive choices of the resident
    /// allocator's own selection step for a draining lane with no session
    /// match, so the first stamp is the task offered next. Standing is held
    /// fixed across steps. Empty for every other read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offer: Vec<crate::TaskStamp>,
    /// Open-read rows whose prerequisite or controller gate has no path
    /// through current advancing work. This is execution trouble, in durable
    /// identity order. Empty for every other read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked: Vec<crate::TaskStamp>,
    /// Why each open-read row no team automatically allocates stands still.
    /// Every key also names a row in `blocked`. Empty for every other read.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub unallocated: std::collections::BTreeMap<crate::TaskStamp, Unallocated>,
    /// Open-read rows held by normal Plan/Design control or an unresolved
    /// prerequisite chain that still reaches advancing work. These remain
    /// allocator-ineligible but are not execution trouble. Durable identity
    /// order; empty for every other read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workflow_waiting: Vec<crate::TaskStamp>,
    /// Every open Design's pass-cycle state among this read's rows, judged
    /// once from the whole live board so each surface renders the same
    /// value; a Design awaiting decision is the operator's selection wait.
    /// Empty for [`BoardRead::Next`].
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub design_cycles: std::collections::BTreeMap<crate::TaskStamp, crate::rooms::DesignCycle>,
    /// Every steering key whose direction caused non-deleted work, mapped to
    /// that work's handles in stable order. Open reads carry the whole index,
    /// so a quoted direction keeps its Work after that work settles.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub origins: std::collections::BTreeMap<String, Vec<crate::TaskHandle>>,
    /// Terminal-page metadata, present only for [`BoardRead::History`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub history: Option<TaskHistoryPage>,
    /// Every acknowledged steering directive joined to each named task through
    /// an `ack:` task/claim origin or the holder's operator `steer::acked` fact.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub acknowledgments: std::collections::BTreeMap<crate::TaskStamp, Vec<TaskAcknowledgment>>,
    /// The operator's closure of each named task that was closed.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub closures: std::collections::BTreeMap<crate::TaskStamp, TaskDetailClosure>,
    /// Workflow steps on the named tasks of a [`BoardRead::Tasks`] read that
    /// asked for history, each task's steps in ascending journal position.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub steps: std::collections::BTreeMap<crate::TaskStamp, Vec<TaskDetailStep>>,
    /// Current executable-configuration trust heads in ascending digest order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trust: Vec<Positioned<crate::TrustHead>>,
    /// Available shard count observed for each declared resource pool.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub resource_pools: std::collections::BTreeMap<crate::ResourcePoolName, u32>,
    /// Product version of the resident appliance that captured this snapshot.
    pub appliance: crate::manifest::ProductVersion,
    /// Current resident and lane head used by possession witnesses.
    pub residency: Positioned<crate::ResidencyHead>,
    /// The peer address every residency lane offers at this barrier.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub addresses: std::collections::BTreeMap<crate::CanonicalUuid, crate::SeatAddress>,
    /// The lane a [`BoardRead::Lane`] read's code selects at this barrier.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub selected: Option<crate::LaneSelection>,
    /// Most recent proved occupant birth code on each current residency lane.
    ///
    /// Unlike `addresses`, a code remains selectable after its occupant leaves
    /// and therefore exists for a vacant lane with authoritative history.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub codes: std::collections::BTreeMap<crate::CanonicalUuid, crate::OccupantAddress>,
}

impl BoardAnswer {
    /// Activated pending rows only; Draft triage is counted separately.
    pub fn active_tasks(&self) -> impl Iterator<Item = &BoardTask> {
        self.tasks.iter().filter(|task| task.is_active())
    }

    /// Whether `task` is a Design the operator's decision holds.
    #[must_use]
    pub fn awaiting_selection(&self, task: &crate::TaskStamp) -> bool {
        self.design_cycles.get(task) == Some(&crate::rooms::DesignCycle::AwaitingDecision)
    }

    /// Compile the environmental observations carried by this snapshot.
    #[must_use]
    pub fn environment(&self) -> crate::Environment {
        let mut environment = crate::Environment::journal_schema(self.schema)
            .expect("an admitted board schema is positive")
            .with_trust_heads(self.trust.iter().map(|head| head.body.clone()))
            .with_appliance(self.appliance.clone());
        for (name, available) in &self.resource_pools {
            environment = environment.with_resource_pool(name.clone(), *available);
        }
        environment
    }
}

/// One deterministic resident-rendered document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextAnswer {
    pub text: String,
}

const fn is_false(value: &bool) -> bool {
    !*value
}
