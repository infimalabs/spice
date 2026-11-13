//! Pure task projection shared by every app task control and overview.
//!
//! The admitted board snapshot and current lane topology are the sole inputs.
//! This module owns lifecycle precedence, stable selection scope identity,
//! full-topic grouping, relationship summaries, counts, board arrangements
//! over the resident's offer order, and owner naming so consumers cannot drift.

#[cfg(any(target_arch = "wasm32", test))]
pub(crate) mod lens;
mod operator;
mod overview;
mod relationship;
mod settlement;
mod vocabulary;
mod wall;
pub use operator::{OperatorWait, OperatorWaits};
pub use overview::{
    TaskOverviewBase, TaskOverviewContext, TaskOverviewEdge, TaskOverviewGraph, TaskOverviewNode,
    TaskOverviewNodeKind, TaskOverviewSelection,
};
pub use relationship::{
    WorkRelationshipDirection, WorkRelationshipEdge, WorkRelationshipError,
    WorkRelationshipExpansion, WorkRelationshipFrontier, WorkRelationshipGraph,
    WorkRelationshipKind, WorkRelationshipNode, WorkRelationshipOptions,
    WorkRelationshipOriginNode, WorkRelationshipTaskNode,
};
pub use settlement::{SettlementAct, SettlementRecord};
#[cfg(target_arch = "wasm32")]
pub(crate) use vocabulary::retained_track;
pub use vocabulary::{
    DesignPhase, HolderGroup, UNTIL_WOKEN, WorkGroup, WorkHolder, WorkList, WorkPhase,
};
pub use wall::DependencyWall;
#[cfg(test)]
mod team_scope_tests;

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use wire::head::ClaimDisposition;
use wire::intent::BoardTask;
use wire::{Actor, TaskStamp};

use crate::runtime::{BoardView, LaneMember, LaneView};
use crate::view::{WorkRoute, WorkRouteTopic};
use crate::watchband;

/// One mutually exclusive task lifecycle in the current board projection,
/// following task status's standing precedence for open work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum WorkAvailability {
    /// A current active claim possesses this open task.
    InFlight,
    /// Open, unclaimed, and immediately workable.
    Ready,
    /// Open and unclaimed in its review phase, awaiting a reviewer.
    Review,
    /// Open, with a lapsed claim awaiting takeover.
    Stale,
    /// Open and unclaimed, held at a dependency or controller gate only a
    /// human can repair.
    Blocked,
    /// Open and unclaimed, following normal Plan/Design control or a
    /// prerequisite chain that still reaches advancing work.
    Waiting,
    /// An open, unclaimed Design gathering input rather than waiting for
    /// selected work to advance.
    Passive,
    /// Open and unclaimed, past the gate, a Design whose remaining conclusion
    /// waits on the operator's selection of an output it constructed.
    Selection,
    /// Open and unclaimed, past the gate but needing an environment
    /// requirement only the operator can satisfy.
    Held,
    /// Open and unclaimed, with an explicit wakeable deferral.
    Deferred,
    /// Prospective work in the Draft phase, separate from execution inventory.
    /// Submitted uncontrolled Drafts may still be allocated for triage.
    Draft,
    /// No longer open, regardless of stale possession evidence.
    Terminal,
}

impl WorkAvailability {
    /// Stable presentation key shared by every task lifecycle surface.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::InFlight => "in-flight",
            Self::Ready => "ready",
            Self::Review => "review",
            Self::Stale => "stale",
            Self::Blocked => "blocked",
            Self::Waiting => "waiting",
            Self::Passive => "passive",
            Self::Selection => "selection",
            Self::Held => "held",
            Self::Deferred => "deferred",
            Self::Draft => "draft",
            Self::Terminal => "terminal",
        }
    }

    /// Whether an agent can take this open work up without the operator.
    #[must_use]
    pub const fn moving(self) -> bool {
        matches!(
            self,
            Self::InFlight | Self::Ready | Self::Review | Self::Stale
        )
    }
}

/// Current UI-placement home, separate from immutable creator provenance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkOwner {
    /// Current live team home.
    Team(wire::CanonicalUuid),
    /// The Design-planning namespace. A task can depend on more than one open
    /// Design, so no single Design stamp is an owner identity.
    Design,
}

/// Browser-local policy selecting a complete set for the unified overview.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum WorkScope {
    /// Every open execution task across every topic.
    #[default]
    AllOpen,
    /// Every open task: the execution and Design-planning scopes together.
    All,
    /// Open execution tasks under one exact topic.
    ///
    /// Work gated by an open Design uses [`Self::Planning`] even when it shares
    /// the same topic.
    Topic(String),
    /// Open execution tasks no first phase has placed in a topic yet.
    Unplaced,
    /// Open Designs and every task transitively gated by an open Design.
    Planning { topic: Option<String> },
    /// The operator's open Drafts.
    Drafts,
    /// Open execution tasks currently worked by one team's members.
    Team { team: wire::CanonicalUuid },
}
/// Resolution of one durable Work identity against hydrated mutable authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkRouteResolution {
    /// The board or team topology has not settled.
    Pending,
    /// Hydrated authority proves that this identity is absent.
    Missing,
    /// The current authority resolves the identity to this selection scope.
    Resolved(WorkScope),
}

impl WorkScope {
    /// The exact topic this scope narrows to, alone or within the
    /// Design-planning rail.
    #[must_use]
    pub fn topic(&self) -> Option<&str> {
        match self {
            Self::Topic(topic) | Self::Planning { topic: Some(topic) } => Some(topic),
            Self::AllOpen
            | Self::All
            | Self::Unplaced
            | Self::Planning { topic: None }
            | Self::Drafts
            | Self::Team { .. } => None,
        }
    }

    /// Every open Design-owned planning task.
    #[must_use]
    pub const fn planning() -> Self {
        Self::Planning { topic: None }
    }

    #[must_use]
    pub const fn team(&self) -> Option<wire::CanonicalUuid> {
        match self {
            Self::Team { team } => Some(*team),
            Self::AllOpen
            | Self::All
            | Self::Topic(_)
            | Self::Unplaced
            | Self::Planning { .. }
            | Self::Drafts => None,
        }
    }
}

impl WorkScope {
    /// Stable durable identity for this scope.
    #[must_use]
    pub(crate) fn route(&self) -> WorkRoute {
        match self {
            Self::AllOpen => WorkRoute::AllOpen,
            Self::All => WorkRoute::All,
            Self::Topic(topic) => WorkRoute::Topic(route_topic(topic)),
            Self::Unplaced => WorkRoute::Unplaced,
            Self::Planning { topic } => WorkRoute::Planning {
                topic: topic.as_deref().map(route_topic),
            },
            Self::Drafts => WorkRoute::Drafts,
            Self::Team { team } => WorkRoute::Team(*team),
        }
    }
}

fn route_topic(topic: &str) -> WorkRouteTopic {
    WorkRouteTopic::new(topic.to_owned()).expect("an admitted board topic is canonical")
}

/// Resolve a durable Work identity through the current board and topology.
/// All-open, all-planning, and unplaced exist even before any is populated.
#[must_use]
pub(crate) fn resolve_work_route(
    route: &WorkRoute,
    work: Option<&WorkProjection<'_>>,
) -> WorkRouteResolution {
    match route {
        WorkRoute::AllOpen => WorkRouteResolution::Resolved(WorkScope::AllOpen),
        WorkRoute::All => WorkRouteResolution::Resolved(WorkScope::All),
        WorkRoute::Planning { topic: None } => WorkRouteResolution::Resolved(WorkScope::planning()),
        WorkRoute::Unplaced => WorkRouteResolution::Resolved(WorkScope::Unplaced),
        WorkRoute::Drafts => WorkRouteResolution::Resolved(WorkScope::Drafts),
        WorkRoute::Topic(route_topic)
        | WorkRoute::Planning {
            topic: Some(route_topic),
        } => {
            let Some(work) = work else {
                return WorkRouteResolution::Pending;
            };
            let topic = route_topic.topic().to_owned();
            let scope = if matches!(route, WorkRoute::Topic(_)) {
                WorkScope::Topic(topic)
            } else {
                WorkScope::Planning { topic: Some(topic) }
            };
            if work.contains_scope(&scope) == Some(true) {
                WorkRouteResolution::Resolved(scope)
            } else {
                WorkRouteResolution::Missing
            }
        }
        WorkRoute::Team(team) => {
            let Some(work) = work else {
                return WorkRouteResolution::Pending;
            };
            let scope = WorkScope::Team { team: *team };
            match work.contains_scope(&scope) {
                Some(true) => WorkRouteResolution::Resolved(scope),
                Some(false) => WorkRouteResolution::Missing,
                None => WorkRouteResolution::Pending,
            }
        }
    }
}

/// The most actionable state one Work partition holds, which marks its tab.
/// Variants run from most to least actionable; the first one present wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkMark {
    /// Work only the operator can unblock: a dependency or controller wall
    /// with no advancing path, or an environment hold such as a trust grant.
    Blocked,
    /// A Design whose completed pass the operator has not decided.
    Decision,
    /// A research-ready Design that no lane holds.
    Pass,
    /// Work the operator set aside until a time; nothing offers it until woken.
    Deferred,
}

impl WorkMark {
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Decision => "decision",
            Self::Pass => "pass",
            Self::Deferred => "deferred",
        }
    }
}

/// Exclusive lifecycle totals for one scope, plus the unpossessed Designs
/// among them whose pass cycle is ready for a research pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkCounts {
    in_flight: usize,
    ready: usize,
    review: usize,
    stale: usize,
    blocked: usize,
    workflow_waiting: usize,
    passive: usize,
    selection: usize,
    held: usize,
    deferred: usize,
    terminal: usize,
    /// Overlaps `ready`; never part of [`Self::open`].
    pass_ready: usize,
}

impl WorkCounts {
    fn record(&mut self, availability: WorkAvailability, pass_ready: bool) {
        self.pass_ready += usize::from(pass_ready);
        match availability {
            WorkAvailability::InFlight => self.in_flight += 1,
            WorkAvailability::Ready => self.ready += 1,
            WorkAvailability::Review => self.review += 1,
            WorkAvailability::Stale => self.stale += 1,
            WorkAvailability::Blocked => self.blocked += 1,
            WorkAvailability::Waiting => self.workflow_waiting += 1,
            WorkAvailability::Passive => self.passive += 1,
            WorkAvailability::Selection => self.selection += 1,
            WorkAvailability::Held => self.held += 1,
            WorkAvailability::Deferred => self.deferred += 1,
            WorkAvailability::Terminal => self.terminal += 1,
            WorkAvailability::Draft => {}
        }
    }
    #[must_use]
    pub const fn in_flight(self) -> usize {
        self.in_flight
    }

    #[must_use]
    pub const fn ready(self) -> usize {
        self.ready
    }

    #[must_use]
    pub const fn review(self) -> usize {
        self.review
    }

    #[must_use]
    pub const fn stale(self) -> usize {
        self.stale
    }

    #[must_use]
    pub const fn blocked(self) -> usize {
        self.blocked
    }
    #[must_use]
    pub const fn workflow_waiting(self) -> usize {
        self.workflow_waiting
    }

    #[must_use]
    pub const fn deferred(self) -> usize {
        self.deferred
    }

    #[must_use]
    pub const fn terminal(self) -> usize {
        self.terminal
    }

    /// Open work no agent is offered until its workflow or the operator moves:
    /// neutral Plan/Design sequencing, passive input gathering, environment
    /// holds, selection, or deferral.
    #[must_use]
    pub const fn waiting(self) -> usize {
        self.workflow_waiting + self.passive + self.held + self.selection + self.deferred
    }

    /// The partition's tab mark and how many tasks carry it: work only the
    /// operator can unblock, then a Design awaiting the operator's decision,
    /// then an unpossessed Design ready for a research pass, then work the operator
    /// deferred, which the mark keeps visible as an intentional set-aside.
    /// Neutral workflow waiting and parked work need nothing and mark nothing.
    #[must_use]
    pub const fn mark(self) -> Option<(WorkMark, usize)> {
        let blocked = self.blocked + self.held;
        if blocked > 0 {
            Some((WorkMark::Blocked, blocked))
        } else if self.selection > 0 {
            Some((WorkMark::Decision, self.selection))
        } else if self.pass_ready > 0 {
            Some((WorkMark::Pass, self.pass_ready))
        } else if self.deferred > 0 {
            Some((WorkMark::Deferred, self.deferred))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn open(self) -> usize {
        self.in_flight + self.ready + self.review + self.stale + self.blocked + self.waiting()
    }

    #[must_use]
    pub const fn has_open(self) -> bool {
        self.open() > 0
    }
}

impl<'p, 'a> FromIterator<&'p WorkTask<'a>> for WorkCounts {
    fn from_iter<I: IntoIterator<Item = &'p WorkTask<'a>>>(tasks: I) -> Self {
        let mut counts = Self::default();
        for task in tasks {
            counts.record(task.availability, task.pass_ready);
        }
        counts
    }
}

/// One dependency edge as resolved against this admitted board snapshot.
#[derive(Debug)]
pub struct WorkDependency<'a> {
    /// Stable dependency identity even when its row is unavailable.
    born: &'a TaskStamp,
    /// Resolved task index in [`WorkProjection::tasks`].
    task: Option<usize>,
    /// Whether the dependency is settled: its row has a completion head, or the
    /// open board omits it, which it does only for terminal prerequisites.
    satisfied: bool,
    /// Whether this is an output the task, a Design, released by decision
    /// rather than a prerequisite its statement names.
    released: bool,
}

impl<'a> WorkDependency<'a> {
    #[must_use]
    pub const fn born(&self) -> &'a TaskStamp {
        self.born
    }

    #[must_use]
    pub const fn task(&self) -> Option<usize> {
        self.task
    }

    #[must_use]
    pub const fn satisfied(&self) -> bool {
        self.satisfied
    }

    #[must_use]
    pub const fn released(&self) -> bool {
        self.released
    }
}

/// One reverse edge to a task that names this task as a dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkDependent {
    /// Dependent task index in [`WorkProjection::tasks`].
    task: usize,
    /// Whether the dependent is the Design that released this task.
    released: bool,
}

impl WorkDependent {
    #[must_use]
    pub const fn task(self) -> usize {
        self.task
    }

    #[must_use]
    pub const fn released(self) -> bool {
        self.released
    }
}

/// One task projected for Work consumers.
#[derive(Debug)]
pub struct WorkTask<'a> {
    row: &'a BoardTask,
    availability: WorkAvailability,
    /// Full-topic bucket in [`WorkProjection::topics`].
    topic: usize,
    /// An unsatisfied operator-actionable environment or missing explicitly
    /// declared resource pool is one blocker.
    environment_blocked: bool,
    /// A Design held for the operator's selection of an output it constructed,
    /// whether or not a lane still holds its claim.
    awaiting_selection: bool,
    /// An unpossessed, offered Design ready for a research pass.
    pass_ready: bool,
    /// Where it stands on its kind's track; `None` once withdrawn.
    phase: Option<WorkPhase<'a>>,
    /// Position in the resident's offer order, when the allocator offers it.
    offer_rank: Option<usize>,
    dependencies: Vec<WorkDependency<'a>>,
    dependents: Vec<WorkDependent>,
    /// Current accountable placement owner; immutable creator-team provenance
    /// remains on the board row.
    owner: Option<WorkOwner>,
}

impl<'a> WorkTask<'a> {
    #[must_use]
    pub const fn row(&self) -> &'a BoardTask {
        self.row
    }

    #[must_use]
    pub const fn availability(&self) -> WorkAvailability {
        self.availability
    }

    #[must_use]
    pub const fn topic(&self) -> usize {
        self.topic
    }

    #[must_use]
    pub const fn environment_blocked(&self) -> bool {
        self.environment_blocked
    }

    #[must_use]
    pub const fn awaiting_selection(&self) -> bool {
        self.awaiting_selection
    }

    /// An unpossessed, offered Design ready for a research pass: the state
    /// its tab marks.
    #[must_use]
    pub const fn pass_ready(&self) -> bool {
        self.pass_ready
    }

    /// This task's position in the resident's offer order: `Some(0)` is the
    /// task offered next, and `None` is work the allocator does not offer.
    #[must_use]
    pub const fn offer_rank(&self) -> Option<usize> {
        self.offer_rank
    }

    #[must_use]
    pub fn dependencies(&self) -> &[WorkDependency<'a>] {
        &self.dependencies
    }

    #[must_use]
    pub fn dependents(&self) -> &[WorkDependent] {
        &self.dependents
    }

    /// The actor currently working this open task: its earliest active
    /// participant.
    ///
    /// Activated work is classified [`WorkAvailability::InFlight`], while a
    /// claimed Draft stays a Draft so it remains in Draft inventory. Terminal
    /// rows can retain stale active-claim evidence, so lifecycle must agree
    /// before a claim is presented as live possession.
    #[must_use]
    pub fn working_claimant(&self) -> Option<&'a Actor> {
        self.participants().next()
    }

    /// Every actor working this open task, earliest claim first.
    ///
    /// Empty unless activated work is in flight or a Draft has a live claim.
    pub fn participants(&self) -> impl Iterator<Item = &'a Actor> + use<'a> {
        let row = self.row;
        matches!(
            self.availability,
            WorkAvailability::InFlight | WorkAvailability::Draft
        )
        .then(|| row.active_claims().map(|claim| &claim.claim.body.actor))
        .into_iter()
        .flatten()
    }

    /// The current round's accepted arrivals and its target, when the current
    /// phase takes more than one participant.
    #[must_use]
    pub fn round(&self) -> Option<(usize, u32)> {
        let completion = &self.row.completion.body;
        let target = completion.target(&self.row.workflow.body.phase).get();
        (target > 1).then_some((completion.arrivals.len(), target))
    }

    /// The independently steerable lanes bound to this task's active claims,
    /// earliest claim first.
    ///
    /// Activated work is in flight; claimed Drafts stay Drafts while their
    /// lanes still receive local task tabs. Terminal rows can retain stale
    /// claim evidence, so lifecycle must agree before a claim's lane is
    /// presented as current work.
    pub fn working_lanes(&self) -> impl Iterator<Item = wire::CanonicalUuid> + use<'a> {
        let row = self.row;
        matches!(
            self.availability,
            WorkAvailability::InFlight | WorkAvailability::Draft
        )
        .then(|| row.active_claims().map(|claim| claim.claim.body.lane))
        .into_iter()
        .flatten()
    }

    /// The independently steerable lane bound to this task's earliest active
    /// claim.
    #[must_use]
    pub fn working_lane(&self) -> Option<wire::CanonicalUuid> {
        self.working_lanes().next()
    }

    /// The canonical member accent for the lane bound to this task's current claim.
    ///
    /// Lanes outside the admitted roster have no member accent.
    /// [`Self::working_lane`] excludes stale and terminal claim evidence.
    #[must_use]
    pub fn working_claimant_accent(&self, lanes: Option<&LaneView>) -> Option<usize> {
        watchband::lane_accent(&lanes?.lanes, self.working_lane()?)
    }

    /// The actor that owns this task: its earliest active claimant.
    #[must_use]
    pub fn owner(&self) -> Option<&'a Actor> {
        self.row
            .active_claims()
            .next()
            .map(|claim| &claim.claim.body.actor)
    }

    #[must_use]
    pub fn belongs_to_team(&self, team: wire::CanonicalUuid) -> bool {
        self.owner == Some(WorkOwner::Team(team))
    }

    #[must_use]
    pub fn belongs_to_design(&self) -> bool {
        matches!(self.owner, Some(WorkOwner::Design))
    }
}

/// One exact topic in the lexically ordered topic grouping, or the unplaced
/// group, which precedes every topic.
#[derive(Debug)]
pub struct WorkTopic<'a> {
    topic: Option<&'a str>,
    counts: WorkCounts,
    /// Deterministically ranked indexes into [`WorkProjection::tasks`].
    tasks: Vec<usize>,
}

impl<'a> WorkTopic<'a> {
    /// The exact topic, or `None` for the unplaced group.
    #[must_use]
    pub const fn topic(&self) -> Option<&'a str> {
        self.topic
    }

    #[must_use]
    pub const fn counts(&self) -> WorkCounts {
        self.counts
    }

    #[must_use]
    pub fn tasks(&self) -> &[usize] {
        &self.tasks
    }
}

#[derive(Clone, Copy)]
struct BoardRows<'a> {
    open: &'a [BoardTask],
    finished: &'a [BoardTask],
}

impl<'a> From<&'a BoardView> for BoardRows<'a> {
    fn from(board: &'a BoardView) -> Self {
        Self {
            open: &board.tasks,
            finished: &board.finished,
        }
    }
}

impl<'a> BoardRows<'a> {
    fn len(self) -> usize {
        self.open.len() + self.finished.len()
    }

    fn iter(self) -> impl Iterator<Item = &'a BoardTask> + Clone {
        self.open.iter().chain(self.finished)
    }

    fn at(self, index: usize) -> &'a BoardTask {
        if index < self.open.len() {
            &self.open[index]
        } else {
            &self.finished[index - self.open.len()]
        }
    }
}

/// Complete client-side Work model for one admitted board snapshot.
#[derive(Debug)]
pub struct WorkProjection<'a> {
    counts: WorkCounts,
    tasks: Vec<WorkTask<'a>>,
    topics: Vec<WorkTopic<'a>>,
    /// Every task index in the resident's offer order, then every task the
    /// allocator does not offer in lifecycle and topic order.
    offered: Vec<usize>,
    /// Current roster team identities, absent until the lane topology loads.
    /// A team route settles unavailable only when a loaded roster lacks it.
    teams: Option<BTreeSet<wire::CanonicalUuid>>,
    /// Current independently steerable member to team placement.
    member_teams: BTreeMap<wire::CanonicalUuid, wire::CanonicalUuid>,
}

impl<'a> WorkProjection<'a> {
    #[must_use]
    pub fn tasks(&self) -> &[WorkTask<'a>] {
        &self.tasks
    }

    /// The one exact task selection not represented by `list`. Targeted task
    /// detail remains authoritative when the board projection cannot produce a
    /// row for that identity.
    #[must_use]
    pub fn retained_direct_task<'s>(
        &self,
        selection: &'s TaskOverviewSelection,
        list: &WorkList,
    ) -> Option<&'s TaskStamp> {
        let task = selection.sole_task()?;
        let listed = list
            .outside
            .iter()
            .chain(list.groups.iter().flat_map(|group| &group.tasks))
            .any(|&index| self.tasks[index].row().statement.body.born == *task);
        (!listed).then_some(task)
    }

    #[must_use]
    pub fn topics(&self) -> &[WorkTopic<'a>] {
        &self.topics
    }

    #[must_use]
    pub const fn counts(&self) -> WorkCounts {
        self.counts
    }

    /// The exact topic grouping named `topic`, if any task carries it.
    #[must_use]
    pub fn topic(&self, topic: &str) -> Option<&WorkTopic<'a>> {
        self.topics
            .binary_search_by(|candidate| candidate.topic.cmp(&Some(topic)))
            .ok()
            .map(|index| &self.topics[index])
    }

    /// Whether the projection holds `scope`; `None` while a team scope waits
    /// for the lane roster that decides it.
    #[must_use]
    pub fn contains_scope(&self, scope: &WorkScope) -> Option<bool> {
        match scope {
            WorkScope::AllOpen
            | WorkScope::All
            | WorkScope::Unplaced
            | WorkScope::Planning { topic: None }
            | WorkScope::Drafts => Some(true),
            WorkScope::Topic(topic) | WorkScope::Planning { topic: Some(topic) } => {
                Some(self.topic(topic).is_some())
            }
            WorkScope::Team { team } => self.teams.as_ref().map(|teams| teams.contains(team)),
        }
    }

    #[must_use]
    pub fn scope_counts(&self, scope: &WorkScope) -> WorkCounts {
        let mut counts = WorkCounts::default();
        let mut matched = false;
        for task in &self.tasks {
            if self.in_scope(task, scope) {
                matched = true;
                counts.record(task.availability, task.pass_ready);
            }
        }
        if matched {
            return counts;
        }
        match scope {
            WorkScope::Topic(_) => self.scope_counts(&WorkScope::AllOpen),
            WorkScope::Planning { topic: Some(_) } => self.scope_counts(&WorkScope::planning()),
            _ => WorkCounts::default(),
        }
    }

    /// Whether one task belongs to the selected overview scope.
    ///
    /// A team holds open work its member lanes actively claim and finished work
    /// accountable to that team. A member-claimed Design shows in both its team
    /// and Planning while staying out of execution and its topic.
    #[must_use]
    pub fn in_scope(&self, task: &WorkTask<'_>, scope: &WorkScope) -> bool {
        match scope {
            WorkScope::AllOpen => !task.belongs_to_design(),
            WorkScope::All => true,
            WorkScope::Drafts => task.availability == WorkAvailability::Draft,
            WorkScope::Topic(topic) => {
                !task.belongs_to_design() && self.task_topic(task) == Some(topic.as_str())
            }
            WorkScope::Unplaced => !task.belongs_to_design() && self.task_topic(task).is_none(),
            WorkScope::Planning { topic } => {
                task.belongs_to_design()
                    && topic
                        .as_deref()
                        .is_none_or(|topic| self.task_topic(task) == Some(topic))
            }
            WorkScope::Team { team } => {
                if task.availability == WorkAvailability::Terminal {
                    task.belongs_to_team(*team)
                } else {
                    task.working_lane()
                        .is_some_and(|lane| self.member_teams.get(&lane) == Some(team))
                }
            }
        }
    }

    /// The exact topic holding one task, or `None` while it is unplaced.
    #[must_use]
    pub fn task_topic(&self, task: &WorkTask<'_>) -> Option<&'a str> {
        self.topics[task.topic].topic
    }

    /// One scope's open tasks in strip order: in-flight, ready, review,
    /// stale, blocked, neutral workflow waiting, passive, selection, held,
    /// deferred, then Drafts, with each availability retaining resident offer
    /// order. A team scope admits a Draft only while a member lane claims it.
    #[must_use]
    pub fn scope_strip<'p>(&'p self, scope: &WorkScope) -> Vec<&'p WorkTask<'a>> {
        use WorkAvailability::{
            Blocked, Deferred, Draft, Held, InFlight, Passive, Ready, Review, Selection, Stale,
            Waiting,
        };

        let mut strip = Vec::new();
        for availability in [
            InFlight, Ready, Review, Stale, Blocked, Waiting, Passive, Selection, Held, Deferred,
            Draft,
        ] {
            strip.extend(
                self.offered
                    .iter()
                    .map(|&index| &self.tasks[index])
                    .filter(|task| task.availability == availability && self.in_scope(task, scope)),
            );
        }
        strip
    }

    #[must_use]
    pub fn from_board_and_lanes(board: &'a BoardView, lanes: Option<&LaneView>) -> Self {
        let Some(lanes) = lanes else {
            return Self::from_board_with_team_memberships(board, None, BTreeMap::new());
        };
        let mut teams = BTreeSet::new();
        let mut member_teams = BTreeMap::new();
        for team in &lanes.lanes {
            teams.insert(team.lane);
            for member in &team.members {
                member_teams.insert(member.lane, team.lane);
            }
        }
        Self::from_board_with_team_memberships(board, Some(teams), member_teams)
    }

    fn from_board_with_team_memberships(
        board: &'a BoardView,
        teams: Option<BTreeSet<wire::CanonicalUuid>>,
        member_teams: BTreeMap<wire::CanonicalUuid, wire::CanonicalUuid>,
    ) -> Self {
        let rows = BoardRows::from(board);
        let by_born = rows
            .iter()
            .enumerate()
            .map(|(index, task)| (&task.statement.body.born, index))
            .collect::<BTreeMap<_, _>>();
        let resource_configured = rows
            .iter()
            .map(|task| board.resources_configured(task))
            .collect::<Vec<_>>();
        let environment_blocked = rows
            .iter()
            .zip(&resource_configured)
            .map(|(task, configured)| !board.environment_satisfies(task) || !configured)
            .collect::<Vec<_>>();
        let resource_waiting = rows
            .iter()
            .zip(&resource_configured)
            .map(|(task, configured)| *configured && !board.resources_satisfy(task))
            .collect::<Vec<_>>();
        let availability = rows
            .iter()
            .enumerate()
            .map(|(index, task)| {
                classify(
                    board,
                    task,
                    environment_blocked[index],
                    resource_waiting[index],
                )
            })
            .collect::<Vec<_>>();

        let pass_ready = pass_ready(board, rows, &availability);
        let released = released_indexes(rows, &by_born);
        let dependent_indexes = dependent_indexes(rows, &by_born, &availability, &released);
        let design_owned = design_owned_indexes(rows, &by_born);
        let offer_ranks = offer_ranks(board, rows, &by_born);

        let mut counts = WorkCounts::default();
        let mut topic_builders = BTreeMap::<Option<&'a str>, TopicBuilder>::new();
        for (index, task) in rows.iter().enumerate() {
            counts.record(availability[index], pass_ready[index]);
            let topic = topic_builders.entry(row_topic(task)).or_default();
            topic.counts.record(availability[index], pass_ready[index]);
            topic.tasks.push(index);
        }

        let mut topics = Vec::with_capacity(topic_builders.len());
        let mut topic_by_task = vec![0; rows.len()];
        for (topic, mut builder) in topic_builders {
            builder
                .tasks
                .sort_by(|left, right| compare_tasks(rows, &availability, *left, *right));
            for &task in &builder.tasks {
                topic_by_task[task] = topics.len();
            }
            topics.push(WorkTopic {
                topic,
                counts: builder.counts,
                tasks: builder.tasks,
            });
        }

        let tasks = rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let dependencies = work_dependencies(row, rows, &by_born, &released[index]);
                let owner =
                    accountable_owner(row, design_owned[index], teams.as_ref(), &member_teams);
                let dependents = dependent_indexes[index]
                    .iter()
                    .map(|&task| WorkDependent {
                        task,
                        released: released[task].contains(&index),
                    })
                    .collect();
                WorkTask {
                    row,
                    availability: availability[index],
                    topic: topic_by_task[index],
                    environment_blocked: environment_blocked[index],
                    awaiting_selection: board.awaiting_selection(&row.statement.body.born),
                    pass_ready: pass_ready[index],
                    phase: vocabulary::phase(Some(board), row, &released[index]),
                    offer_rank: offer_ranks[index],
                    dependencies,
                    dependents,
                    owner,
                }
            })
            .collect();

        let offered = board_order(rows, &offer_ranks, &availability);

        Self {
            counts,
            tasks,
            topics,
            offered,
            teams,
            member_teams,
        }
    }
}

/// The exact topic a board row occupies, or `None` while it is unplaced.
#[must_use]
pub(crate) fn row_topic(task: &BoardTask) -> Option<&str> {
    task.statement
        .body
        .topic
        .path
        .as_ref()
        .map(wire::Topic::as_str)
}

fn accountable_owner(
    task: &BoardTask,
    design_owned: bool,
    teams: Option<&BTreeSet<wire::CanonicalUuid>>,
    member_teams: &BTreeMap<wire::CanonicalUuid, wire::CanonicalUuid>,
) -> Option<WorkOwner> {
    // Only prospective construction is Design-owned. An activated Plan and
    // the Drafts it subsequently constructs execute under that Plan's authority.
    if design_owned {
        return Some(WorkOwner::Design);
    }
    home_team(task, teams, member_teams).map(WorkOwner::Team)
}

fn home_team(
    task: &BoardTask,
    teams: Option<&BTreeSet<wire::CanonicalUuid>>,
    member_teams: &BTreeMap<wire::CanonicalUuid, wire::CanonicalUuid>,
) -> Option<wire::CanonicalUuid> {
    let claimant_team = |claim: &wire::head::ClaimHead| member_teams.get(&claim.lane).copied();
    if let Some(claim) = task.active_claims().next() {
        return claimant_team(&claim.claim.body);
    }
    task.creator_team
        .filter(|team| teams.is_some_and(|teams| teams.contains(team)))
        .or_else(|| {
            task.latest_claim()
                .and_then(|claim| claimant_team(&claim.claim.body))
        })
}

/// A task's statement dependencies, then the construction roots a Design
/// released, which it waits on as its own prerequisites until they settle.
fn work_dependencies<'a>(
    task: &'a BoardTask,
    rows: BoardRows<'a>,
    by_born: &BTreeMap<&TaskStamp, usize>,
    released: &[usize],
) -> Vec<WorkDependency<'a>> {
    task.statement
        .body
        .dependencies
        .iter()
        .map(|born| {
            let task = by_born.get(born).copied();
            WorkDependency {
                born,
                task,
                satisfied: task.is_none_or(|dependency| {
                    !rows.at(dependency).workflow.body.status.is_pending()
                }),
                released: false,
            }
        })
        .chain(released.iter().map(|&index| {
            let output = rows.at(index);
            WorkDependency {
                born: &output.statement.body.born,
                task: Some(index),
                satisfied: !output.workflow.body.status.is_pending(),
                released: true,
            }
        }))
        .collect()
}

/// For every task index, the construction roots a Design at that index
/// released: activated outputs whose immediate parent is that Design.
fn released_indexes(rows: BoardRows<'_>, by_born: &BTreeMap<&TaskStamp, usize>) -> Vec<Vec<usize>> {
    let mut released = vec![Vec::new(); rows.len()];
    for (index, task) in rows.iter().enumerate() {
        let Some(&design) = wire::intent::released_construction_root(
            &task.statement.body.born,
            &task.workflow.body,
            task.statement.body.preparation.as_ref(),
        )
        .and_then(|parent| by_born.get(parent)) else {
            continue;
        };
        if rows.at(design).design_standing().is_some() {
            released[design].push(index);
        }
    }
    released
}

/// Each task's position in the resident's offer order, by task index. A stamp
/// the resident repeats keeps its first, leading position.
fn offer_ranks(
    board: &BoardView,
    rows: BoardRows<'_>,
    by_born: &BTreeMap<&TaskStamp, usize>,
) -> Vec<Option<usize>> {
    let mut ranks = vec![None; rows.len()];
    for (rank, born) in board.offer.iter().enumerate() {
        if let Some(&index) = by_born.get(born) {
            ranks[index].get_or_insert(rank);
        }
    }
    ranks
}

/// Whether each task, by index, is an unpossessed, offered Design ready for
/// a research pass.
fn pass_ready(
    board: &BoardView,
    rows: BoardRows<'_>,
    availability: &[WorkAvailability],
) -> Vec<bool> {
    rows.iter()
        .zip(availability)
        .map(|(task, availability)| {
            *availability == WorkAvailability::Ready
                && board.design_cycles.get(&task.statement.body.born)
                    == Some(&wire::rooms::DesignCycle::ReadyForPass)
        })
        .collect()
}

/// Every task index in the resident's offer order.
///
/// The resident ranks only work it would offer; that rank leads, and the rest
/// keeps lifecycle and topic order beneath it.
fn board_order(
    rows: BoardRows<'_>,
    offer_ranks: &[Option<usize>],
    availability: &[WorkAvailability],
) -> Vec<usize> {
    let rank = |index: &usize| offer_ranks[*index].unwrap_or(usize::MAX);
    let mut offered = (0..rows.len()).collect::<Vec<_>>();
    offered.sort_by(|left, right| {
        rank(left)
            .cmp(&rank(right))
            .then_with(|| compare_tasks(rows, availability, *left, *right))
    });
    offered
}

/// The name an operator reads for one task owner.
///
/// A seated lane reads as its agent's configured name or worktree, exactly as
/// its message cards sign themselves. A lane absent from the current topology
/// keeps a short identity prefix, since no name for it is known. A principal
/// reads by [`principal_label`]. The full actor spelling remains the caller's
/// hover text for every owner.
#[must_use]
pub fn owner_label(
    actor: &Actor,
    member: Option<&LaneMember>,
    principals: Option<&wire::principal::PrincipalDirectory>,
) -> String {
    match actor {
        Actor::Principal(_) => principal_label(actor, principals),
        Actor::Spice => "Spice".to_owned(),
        Actor::Lane(lane) => member.filter(|member| member.lane == *lane).map_or_else(
            || format!("lane {}", watchband::short(*lane)),
            |member| {
                watchband::agent_message_label(&member.worktree, member.name.as_deref()).to_owned()
            },
        ),
    }
}

/// The app's short presentation of a principal actor: recorded profile
/// initials when available, otherwise its immutable code. An unresolved
/// principal keeps its full actor spelling.
#[must_use]
pub fn principal_label(
    actor: &Actor,
    principals: Option<&wire::principal::PrincipalDirectory>,
) -> String {
    principals
        .and_then(|principals| principals.principal(actor))
        .map_or_else(
            || actor.to_string(),
            |display| {
                display
                    .initials
                    .clone()
                    .unwrap_or_else(|| display.code.to_string())
            },
        )
}

#[derive(Default)]
struct TopicBuilder {
    counts: WorkCounts,
    tasks: Vec<usize>,
}

fn dependent_indexes(
    rows: BoardRows<'_>,
    by_born: &BTreeMap<&TaskStamp, usize>,
    availability: &[WorkAvailability],
    released: &[Vec<usize>],
) -> Vec<Vec<usize>> {
    let mut dependents = vec![Vec::new(); rows.len()];
    for (dependent, task) in rows.iter().enumerate() {
        for dependency in &task.statement.body.dependencies {
            if let Some(&index) = by_born.get(dependency) {
                dependents[index].push(dependent);
            }
        }
        for &output in &released[dependent] {
            dependents[output].push(dependent);
        }
    }
    for indexes in &mut dependents {
        indexes.sort_by(|left, right| compare_tasks(rows, availability, *left, *right));
    }
    dependents
}

/// Follow only prospective construction parents to an open Design. An active
/// parent cuts the gate even though its controller lineage remains recorded.
/// Memoization also bounds malformed cyclic fixture graphs.
fn design_owned_indexes(rows: BoardRows<'_>, by_born: &BTreeMap<&TaskStamp, usize>) -> Vec<bool> {
    let mut owned = vec![false; rows.len()];
    let mut evaluated = vec![false; rows.len()];
    let mut path = Vec::new();
    for (index, task) in rows.iter().enumerate() {
        if task.design_standing().is_some() {
            owned[index] = true;
            evaluated[index] = true;
            continue;
        }
        if evaluated[index] {
            continue;
        }
        let mut current = index;
        let design_owned = loop {
            let task = rows.at(current);
            if let Some(standing) = task.design_standing() {
                break standing.is_open();
            }
            if evaluated[current] {
                break owned[current];
            }
            evaluated[current] = true;
            path.push(current);
            if !task.workflow.body.is_draft() || !task.workflow.body.status.is_pending() {
                break false;
            }
            let Some(preparation) = &task.statement.body.preparation else {
                break false;
            };
            let Some(&parent) = by_born.get(&preparation.parent) else {
                break false;
            };
            current = parent;
        };
        for current in path.drain(..) {
            owned[current] = design_owned;
        }
    }
    owned
}

/// Task status's standing precedence: possession and distinct set-aside
/// states, then the resident-owned selection/workflow/blocker partition, then
/// review and environment readiness. The resident judges closed gates over
/// terminal evidence the open board does not carry; this surface only consumes
/// that answer.
fn classify(
    board: &BoardView,
    task: &BoardTask,
    environment_blocked: bool,
    resource_waiting: bool,
) -> WorkAvailability {
    let workflow = &task.workflow.body;
    if !workflow.status.is_pending() {
        return WorkAvailability::Terminal;
    }
    if workflow.is_draft() {
        return WorkAvailability::Draft;
    }
    if task.active_claims().next().is_some() {
        return WorkAvailability::InFlight;
    }
    if task
        .latest_claim()
        .is_some_and(|claim| claim.claim.body.disposition == ClaimDisposition::Lapsed)
    {
        return WorkAvailability::Stale;
    }
    if workflow.deferral.is_some() {
        WorkAvailability::Deferred
    } else if board.awaiting_selection(&task.statement.body.born) {
        WorkAvailability::Selection
    } else if board.workflow_waiting.contains(&task.statement.body.born) {
        match board.design_cycles.get(&task.statement.body.born) {
            Some(wire::rooms::DesignCycle::GatheringInput) => WorkAvailability::Passive,
            _ => WorkAvailability::Waiting,
        }
    } else if board.blocked.contains(&task.statement.body.born) {
        WorkAvailability::Blocked
    } else if workflow.phase.as_str() == "review" {
        WorkAvailability::Review
    } else if environment_blocked {
        WorkAvailability::Held
    } else if resource_waiting {
        WorkAvailability::Waiting
    } else {
        WorkAvailability::Ready
    }
}

fn compare_tasks(
    rows: BoardRows<'_>,
    availability: &[WorkAvailability],
    left: usize,
    right: usize,
) -> Ordering {
    availability[left].cmp(&availability[right]).then_with(|| {
        if availability[left] == WorkAvailability::Terminal {
            return left.cmp(&right);
        }
        rows.at(left)
            .statement
            .body
            .topic_origin
            .cmp(&rows.at(right).statement.body.topic_origin)
            .then_with(|| {
                rows.at(left)
                    .statement
                    .body
                    .born
                    .cmp(&rows.at(right).statement.body.born)
            })
    })
}

#[cfg(test)]
pub(crate) mod tests;
