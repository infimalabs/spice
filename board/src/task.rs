use std::collections::BTreeSet;
use std::fmt;

use wire::{
    Actor, ClaimDisposition, ClaimHead, CompletionHead, LeaseHead, Seq, TaskHandle, TaskHead,
    TaskStamp, WorkflowHead,
};

/// The uppercase family carried by a task handle.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Family(String);

impl Family {
    /// Derive the family from the already-validated wire handle.
    #[must_use]
    pub fn of(handle: &TaskHandle) -> Self {
        let family = handle
            .as_str()
            .split_once('-')
            .expect("TaskHandle always contains a separator")
            .0;
        Self(family.to_owned())
    }

    /// The family spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Family {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One dependency edge derived from an authoritative task statement head.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dependency {
    /// Task that cannot proceed yet.
    pub task: TaskStamp,
    /// Prerequisite task.
    pub after: TaskStamp,
}

/// Durable phase-transition evidence projected into allocator input.
///
/// `until` is the short exclusive handoff opportunity. `peer_until` bounds the
/// longer preference for an available peer before the prior author becomes the
/// allocator's last resort again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhaseHandoff {
    prior: BTreeSet<wire::Actor>,
    until: wire::Instant,
    peer_until: wire::Instant,
}

impl PhaseHandoff {
    /// Construct one handoff from one completed phase author.
    #[must_use]
    pub fn new(prior: wire::Actor, until: wire::Instant, peer_until: wire::Instant) -> Self {
        Self::from_authors([prior], until, peer_until)
    }

    /// Construct one handoff from every author of a shared completed round.
    #[must_use]
    pub fn from_authors(
        prior: impl IntoIterator<Item = wire::Actor>,
        until: wire::Instant,
        peer_until: wire::Instant,
    ) -> Self {
        Self {
            prior: prior.into_iter().collect(),
            until,
            peer_until,
        }
    }

    /// Whether `actor` authored the completed phase.
    #[must_use]
    pub fn contains(&self, actor: &wire::Actor) -> bool {
        self.prior.contains(actor)
    }

    /// End of the exclusive handoff opportunity.
    #[must_use]
    pub const fn until(&self) -> wire::Instant {
        self.until
    }

    /// End of the bounded peer-first opportunity.
    #[must_use]
    pub const fn peer_until(&self) -> wire::Instant {
        self.peer_until
    }
}

/// One retained claim and the lease it holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskClaim {
    claim: ClaimHead,
    lease: Option<LeaseHead>,
}

impl TaskClaim {
    /// The claim head.
    #[must_use]
    pub const fn claim(&self) -> &ClaimHead {
        &self.claim
    }

    /// The claim's lease head, when one was recorded.
    #[must_use]
    pub const fn lease(&self) -> Option<&LeaseHead> {
        self.lease.as_ref()
    }

    /// The claim's identity: the fact that won it.
    #[must_use]
    pub const fn claimed(&self) -> Seq {
        self.claim.claimed
    }

    /// Whether this claim still holds possession.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.claim.disposition == ClaimDisposition::Active
    }
}

/// One task's current journal-head bundle.
///
/// This is a checked view of wire heads, not a mutable board record. Replacing
/// any field requires committing the corresponding fact and head update in the
/// journal first and constructing a new `Entry` from that answer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    statement: TaskHead,
    handle: TaskHandle,
    workflow: WorkflowHead,
    completion: CompletionHead,
    claims: Vec<TaskClaim>,
    creator_team: Option<wire::CanonicalUuid>,
    explicit_release: bool,
    design: wire::rooms::DesignCycleFacts,
    phase_handoff: Option<PhaseHandoff>,
    topic_origin_at: Option<wire::Instant>,
    family: Family,
}

impl Entry {
    /// Validate one current task-head bundle.
    ///
    /// `claims` are the task's retained claims with their leases, in any
    /// order; each lease must belong to its claim, and no actor or lane may
    /// hold two active claims.
    pub fn new(
        statement: TaskHead,
        workflow: WorkflowHead,
        completion: CompletionHead,
        claims: Vec<(ClaimHead, Option<LeaseHead>)>,
    ) -> Result<Self, TaskError> {
        let handle = statement.handle(workflow.phase);
        let family = Family::of(&handle);
        let mut seen = BTreeSet::new();
        for dependency in &statement.dependencies {
            if dependency == &statement.born {
                return Err(TaskError::SelfDependency(statement.born.clone()));
            }
            if !seen.insert(dependency) {
                return Err(TaskError::DuplicateDependency {
                    task: statement.born.clone(),
                    after: dependency.clone(),
                });
            }
        }
        admit_workflow(&workflow)?;
        let claims = admit_claims(claims)?;

        Ok(Self {
            statement,
            handle,
            workflow,
            completion,
            claims,
            creator_team: None,
            explicit_release: false,
            design: wire::rooms::DesignCycleFacts::default(),
            phase_handoff: None,
            topic_origin_at: None,
            family,
        })
    }

    /// Current statement head.
    #[must_use]
    pub const fn statement(&self) -> &TaskHead {
        &self.statement
    }

    /// This entry as it reads once `topic` places its statement, projecting a
    /// placement committed in the same transaction as the step that reads it.
    #[must_use]
    pub fn placed_in_topic(mut self, topic: wire::TaskTopic) -> Self {
        self.statement.topic = topic;
        self.handle = self.statement.handle(self.workflow.phase);
        self.family = Family::of(&self.handle);
        self
    }

    /// Whether this row is prospective work still in its draft phase.
    #[must_use]
    pub const fn is_draft(&self) -> bool {
        self.workflow.is_draft()
    }

    /// Whether this row left draft: activated work that asserts its statement.
    #[must_use]
    pub const fn is_activated(&self) -> bool {
        self.workflow.is_activated()
    }

    /// Whether a preparation controller gates this row's draft exit.
    #[must_use]
    pub const fn is_controlled(&self) -> bool {
        self.statement.preparation.is_some()
    }

    /// Whether this draft is triage work the allocator may hand out: the
    /// author submitted it and no controller gates it. Author-held and
    /// controlled drafts are never allocated or claimed.
    #[must_use]
    pub const fn is_submitted_draft(&self) -> bool {
        self.is_draft() && self.workflow.submitted && !self.is_controlled()
    }

    /// Whether the allocator and named claims may take this row at all:
    /// activated work, or a submitted uncontrolled draft.
    #[must_use]
    pub const fn is_acquirable_standing(&self) -> bool {
        self.is_activated() || self.is_submitted_draft()
    }

    /// Current public handle derived from topic and birth stamp.
    #[must_use]
    pub const fn handle(&self) -> &TaskHandle {
        &self.handle
    }

    /// Sole durable task identity.
    #[must_use]
    pub const fn born(&self) -> &TaskStamp {
        &self.statement.born
    }

    /// Current workflow head.
    #[must_use]
    pub const fn workflow(&self) -> &WorkflowHead {
        &self.workflow
    }

    /// Completion targets and the current round's arrivals.
    #[must_use]
    pub const fn completion(&self) -> &CompletionHead {
        &self.completion
    }

    /// Every retained claim, in identity order.
    #[must_use]
    pub fn claims(&self) -> &[TaskClaim] {
        &self.claims
    }

    /// Every retained claim's head and lease bodies, in identity order, for
    /// rebuilding the entry under other statement or workflow heads.
    #[must_use]
    pub fn claim_bodies(&self) -> Vec<(ClaimHead, Option<LeaseHead>)> {
        self.claims
            .iter()
            .map(|held| (held.claim.clone(), held.lease.clone()))
            .collect()
    }

    /// The retained claim with identity `claimed`.
    #[must_use]
    pub fn claim(&self, claimed: Seq) -> Option<&TaskClaim> {
        self.claims
            .binary_search_by_key(&claimed, TaskClaim::claimed)
            .ok()
            .map(|index| &self.claims[index])
    }

    /// The one active claim `actor` holds.
    #[must_use]
    pub fn claim_of(&self, actor: &Actor) -> Option<&TaskClaim> {
        self.active_claims()
            .find(|claim| claim.claim.actor == *actor)
    }

    /// Every active claim, in identity order.
    pub fn active_claims(&self) -> impl Iterator<Item = &TaskClaim> {
        self.claims.iter().filter(|claim| claim.is_active())
    }

    /// The most recently won retained claim.
    #[must_use]
    pub fn latest_claim(&self) -> Option<&TaskClaim> {
        self.claims.last()
    }

    /// Whether any claim currently possesses the task.
    #[must_use]
    pub fn is_held(&self) -> bool {
        self.active_claims().next().is_some()
    }

    /// Whether nobody holds the task and its latest claim ended by lapsing.
    #[must_use]
    pub fn is_lapsed(&self) -> bool {
        !self.is_held()
            && self
                .latest_claim()
                .is_some_and(|held| held.claim.disposition == ClaimDisposition::Lapsed)
    }

    /// Whether the current phase still wants another participant: active
    /// claims and accepted arrivals fall short of the target.
    #[must_use]
    pub fn has_capacity(&self) -> bool {
        let active = self.active_claims().count();
        let target = self.completion.target(&self.workflow.phase).get();
        let taken = active + self.completion.arrivals.len();
        u32::try_from(taken).is_ok_and(|taken| taken < target)
    }

    /// Whether the current round meets its barrier once `arriving`'s active
    /// claim ends. A lane whose durable arrival already exists does not count
    /// twice when it later closes the round.
    #[must_use]
    pub fn round_satisfied(&self, arriving: Option<&wire::Actor>) -> bool {
        let arrived = self.completion.arrivals.len()
            + usize::from(arriving.is_some_and(|actor| !self.has_arrived(actor)));
        self.active_claims()
            .all(|held| Some(&held.claim.actor) == arriving)
            && arrived >= self.completion.target(&self.workflow.phase).get() as usize
    }

    /// Whether `actor` has a durable arrival in the current round.
    #[must_use]
    pub fn has_arrived(&self, actor: &Actor) -> bool {
        self.completion
            .arrivals
            .iter()
            .any(|arrival| arrival.actor == *actor)
    }

    /// The active claim a steal ends: none while the phase has capacity, so the
    /// steal fills it like a capture; otherwise the claim won last, so the
    /// round's holder is displaced only as its sole claim and a counter-steal
    /// evicts the stealer.
    #[must_use]
    pub fn displaced_by_steal(&self) -> Option<&TaskClaim> {
        if self.has_capacity() {
            return None;
        }
        self.active_claims().max_by_key(|held| held.claimed())
    }

    /// Whether `actor` already holds or has arrived in the current round.
    #[must_use]
    pub fn participates(&self, actor: &Actor) -> bool {
        self.claim_of(actor).is_some() || self.has_arrived(actor)
    }

    /// Immutable creator team captured by the task-added fact.
    #[must_use]
    pub const fn creator_team(&self) -> Option<wire::CanonicalUuid> {
        self.creator_team
    }

    /// Attach creator-team projection derived from the immutable creation fact.
    #[must_use]
    pub fn with_creator_team(mut self, creator_team: Option<wire::CanonicalUuid>) -> Self {
        self.creator_team = creator_team;
        self
    }

    /// Attach whether the current freed claim came from an explicit release command.
    #[must_use]
    pub const fn with_explicit_release(mut self, explicit_release: bool) -> Self {
        self.explicit_release = explicit_release;
        self
    }

    /// This entry under `completion`, projecting a completion-head change.
    #[must_use]
    pub fn with_completion(mut self, completion: CompletionHead) -> Self {
        self.completion = completion;
        self
    }

    /// Attach phase-transition ownership evidence derived from journal facts.
    #[must_use]
    pub fn with_phase_handoff(mut self, phase_handoff: Option<PhaseHandoff>) -> Self {
        self.phase_handoff = phase_handoff;
        self
    }

    /// Current phase-transition ownership evidence, when this phase was reached
    /// from a completed claim.
    #[must_use]
    pub const fn phase_handoff(&self) -> Option<&PhaseHandoff> {
        self.phase_handoff.as_ref()
    }

    /// Attach the commit instant of the fact that established this task's topic origin.
    #[must_use]
    pub const fn with_topic_origin_at(mut self, topic_origin_at: Option<wire::Instant>) -> Self {
        self.topic_origin_at = topic_origin_at;
        self
    }

    /// The commit instant of the topic-origin fact, when the caller supplied
    /// it; a row without one is never overdue.
    #[must_use]
    pub const fn topic_origin_at(&self) -> Option<wire::Instant> {
        self.topic_origin_at
    }

    /// Whether the current freed claim came from an explicit release command.
    #[must_use]
    pub const fn explicit_release(&self) -> bool {
        self.explicit_release
    }

    /// Attach this Design's pass-cycle evidence.
    #[must_use]
    pub const fn with_design(mut self, design: wire::rooms::DesignCycleFacts) -> Self {
        self.design = design;
        self
    }

    /// This Design's pass-cycle evidence.
    #[must_use]
    pub const fn design(&self) -> wire::rooms::DesignCycleFacts {
        self.design
    }

    /// Topic the task lives in; `None` while it is unplaced.
    #[must_use]
    pub const fn topic(&self) -> Option<&wire::Topic> {
        self.statement.topic.path.as_ref()
    }

    /// Family derived from the handle.
    #[must_use]
    pub const fn family(&self) -> &Family {
        &self.family
    }

    /// Dependency edges in statement order.
    pub fn dependencies(&self) -> impl Iterator<Item = Dependency> + '_ {
        self.statement
            .dependencies
            .iter()
            .cloned()
            .map(|after| Dependency {
                task: self.statement.born.clone(),
                after,
            })
    }
}

/// A task-head bundle that cannot enter board policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskError {
    /// A task named itself as a prerequisite.
    SelfDependency(TaskStamp),
    /// One prerequisite appeared more than once.
    DuplicateDependency { task: TaskStamp, after: TaskStamp },
    /// No station was declared.
    EmptyFlow,
    /// The flow does not open with the draft phase.
    DraftNotFirst(wire::Phase),
    /// The flow names no phase after draft.
    NoSubstantivePhase,
    /// One station appeared more than once.
    DuplicatePhase(wire::Phase),
    /// Current station is not a member of its declared flow.
    PhaseOutsideFlow(wire::Phase),
    /// One claim identity appeared more than once.
    DuplicateClaim(Seq),
    /// A lease named a claim other than the one it was paired with.
    LeaseMismatch { claimed: Seq, lease: Seq },
    /// One actor held two active claims.
    DoubleHolder(Actor),
    /// One lane held two active claims.
    DoubleLane(wire::CanonicalUuid),
}

fn admit_claims(claims: Vec<(ClaimHead, Option<LeaseHead>)>) -> Result<Vec<TaskClaim>, TaskError> {
    let mut claims = claims
        .into_iter()
        .map(|(claim, lease)| TaskClaim { claim, lease })
        .collect::<Vec<_>>();
    claims.sort_unstable_by_key(TaskClaim::claimed);
    let mut actors = BTreeSet::new();
    let mut lanes = BTreeSet::new();
    for (index, claim) in claims.iter().enumerate() {
        if index > 0 && claims[index - 1].claimed() == claim.claimed() {
            return Err(TaskError::DuplicateClaim(claim.claimed()));
        }
        if let Some(lease) = &claim.lease
            && lease.claimed != claim.claimed()
        {
            return Err(TaskError::LeaseMismatch {
                claimed: claim.claimed(),
                lease: lease.claimed,
            });
        }
        if claim.is_active() {
            if !actors.insert(&claim.claim.actor) {
                return Err(TaskError::DoubleHolder(claim.claim.actor.clone()));
            }
            if !lanes.insert(claim.claim.lane) {
                return Err(TaskError::DoubleLane(claim.claim.lane));
            }
        }
    }
    Ok(claims)
}

fn admit_workflow(workflow: &WorkflowHead) -> Result<(), TaskError> {
    let Some((first, rest)) = workflow.flow.split_first() else {
        return Err(TaskError::EmptyFlow);
    };
    if !first.is_draft() {
        return Err(TaskError::DraftNotFirst(*first));
    }
    if rest.is_empty() {
        return Err(TaskError::NoSubstantivePhase);
    }
    let mut seen = BTreeSet::new();
    for phase in &workflow.flow {
        if !seen.insert(phase) {
            return Err(TaskError::DuplicatePhase(*phase));
        }
    }
    if !seen.contains(&workflow.phase) {
        return Err(TaskError::PhaseOutsideFlow(workflow.phase));
    }
    Ok(())
}

impl fmt::Display for TaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SelfDependency(task) => write!(formatter, "task {task} depends on itself"),
            Self::DuplicateDependency { task, after } => {
                write!(formatter, "task {task} repeats dependency {after}")
            }
            Self::EmptyFlow => formatter.write_str("workflow flow is empty"),
            Self::DraftNotFirst(phase) => {
                write!(formatter, "workflow opens with {phase}, not draft")
            }
            Self::NoSubstantivePhase => formatter.write_str("workflow names no phase after draft"),
            Self::DuplicatePhase(phase) => write!(formatter, "workflow repeats phase {phase}"),
            Self::PhaseOutsideFlow(phase) => {
                write!(
                    formatter,
                    "current phase {phase} is outside the workflow flow"
                )
            }
            Self::DuplicateClaim(claimed) => write!(formatter, "claim {claimed} appears twice"),
            Self::LeaseMismatch { claimed, lease } => {
                write!(
                    formatter,
                    "claim {claimed} carries the lease of claim {lease}"
                )
            }
            Self::DoubleHolder(actor) => write!(formatter, "{actor} holds two active claims"),
            Self::DoubleLane(lane) => write!(formatter, "lane {lane} holds two active claims"),
        }
    }
}

impl std::error::Error for TaskError {}
