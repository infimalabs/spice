//! Design pass cycle.
//!
//! An open Design alternates between passes and the input those passes need.
//! A pass is a claim won in the design phase; releasing an intake claim ends
//! none. A pass ends at its boundary: the round its final participant
//! completed with `task done`, or a free or lapse that abandoned it. Before
//! any boundary the Design is ready only when its maturity is `Steeped`. A
//! completed round awaits the operator's decision. After a boundary only
//! counted input recorded later refills it: a typed contribution, an
//! operator-authored room post, decision feedback that returned or rejected
//! work, or the final settlement of released work. While work the Design
//! released is still running, the Design waits on it whatever input arrives:
//! accepted work is worked whole before another pass opens, and the released
//! work is the Design's prerequisite set. Requests, notices, asks, leaves,
//! group membership, and unrelated facts are inert.
//!
//! [`DesignCycles`] folds those authorities in journal order, one fact at a
//! time, so replay is bounded by the facts it reads and never rescans a room.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::{
    DecisionOutcome, DesignItems, ExecutionAuthorized, OutputControlled, OutputVerdict,
    PreparationDecided, RoomContributed, RoomDestination, RoomPosted, RoomPublicationContent,
    RoomPublished,
};
use crate::body::{
    ClaimFreed, ClaimLapsed, ClaimWon, TaskAdded, TaskClosed, TaskCompletionStatus, TaskConcluded,
    TaskDeleted, TaskDone, TaskReviewed, TaskRoundCompleted,
};
use crate::{Actor, AnyFact, FactKind, Seq, SequenceCode, TaskHandle, TaskStamp};

/// What one open Design's pass cycle waits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DesignCycle {
    /// No counted input has arrived since the last pass boundary.
    GatheringInput,
    /// Counted input arrived; a lane may claim the Design for a research pass.
    ReadyForPass,
    /// A completed pass the operator has not decided.
    AwaitingDecision,
    /// Accepted work is still running; the Design waits on it as its
    /// prerequisites, whatever input arrives meanwhile.
    WaitingOnSelectedWork,
}

/// Who acts next on one Design's pass cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DesignCycleActor {
    /// Contributing lanes or the operator supply input.
    Contributors,
    /// Lanes claim the Design and complete its pass with `task done`.
    Lane,
    /// The operator decides the completed pass.
    Operator,
    /// The work the Design released finishes.
    SelectedWork,
}

impl DesignCycleActor {
    /// Operator-facing name of who acts.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Contributors => "contributors",
            Self::Lane => "lane",
            Self::Operator => "operator",
            Self::SelectedWork => "selected work",
        }
    }
}

impl DesignCycle {
    /// Every state, in cycle order.
    pub const ALL: [Self; 4] = [
        Self::GatheringInput,
        Self::ReadyForPass,
        Self::AwaitingDecision,
        Self::WaitingOnSelectedWork,
    ];

    /// Operator-facing name of this state.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::GatheringInput => "gathering input",
            Self::ReadyForPass => "ready for research pass",
            Self::AwaitingDecision => "awaiting decision",
            Self::WaitingOnSelectedWork => "waiting on selected work",
        }
    }

    /// Stable kebab-case key, the state's serialized name.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::GatheringInput => "gathering-input",
            Self::ReadyForPass => "ready-for-pass",
            Self::AwaitingDecision => "awaiting-decision",
            Self::WaitingOnSelectedWork => "waiting-on-selected-work",
        }
    }

    /// Who moves this Design forward.
    #[must_use]
    pub const fn next_actor(self) -> DesignCycleActor {
        match self {
            Self::GatheringInput => DesignCycleActor::Contributors,
            Self::ReadyForPass => DesignCycleActor::Lane,
            Self::AwaitingDecision => DesignCycleActor::Operator,
            Self::WaitingOnSelectedWork => DesignCycleActor::SelectedWork,
        }
    }

    /// The exact action that moves `design` forward, citing the pass or
    /// decision position `facts` holds.
    #[must_use]
    pub fn next_action(self, design: &TaskHandle, facts: DesignCycleFacts) -> String {
        match self {
            Self::GatheringInput => format!(
                "contribute with `spice room contribute {design}`, post in its room, or have \
                 a lane claim it now with `spice task capture {design}`"
            ),
            Self::ReadyForPass => format!(
                "a lane claims {design} through `spice task next` for a research pass, \
                 investigates every remaining question, then records `spice task done \
                 --validation \"...\"`"
            ),
            Self::AwaitingDecision => facts.pass.map_or_else(
                || format!("the operator decides the completed pass of {design}"),
                |pass| {
                    format!(
                        "the operator decides pass {} of {design} with `spice design decide \
                         DECISION_JSON`",
                        SequenceCode::new(pass.get())
                    )
                },
            ),
            Self::WaitingOnSelectedWork => facts.decision.map_or_else(
                || format!("finish the work {design} released"),
                |decision| {
                    format!(
                        "finish the work decision {} of {design} selected",
                        SequenceCode::new(decision.get())
                    )
                },
            ),
        }
    }
}

/// One open Design's pass cycle as every surface presents it: the state,
/// who acts, the exact action, how the latest pass ended, and the pass and
/// decision positions that action cites.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignCyclePresentation {
    /// What the cycle waits on.
    pub cycle: DesignCycle,
    /// Who acts next.
    pub actor: DesignCycleActor,
    /// The exact action that moves the Design forward.
    pub action: String,
    /// Current research obligation, separate from allocation readiness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research: Option<String>,
    /// How the latest pass ended its claim, once any pass has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pass: Option<DesignPassEnd>,
    /// The completed pass awaiting the operator's decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass: Option<SequenceCode>,
    /// The operator's latest decision of one of its passes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<SequenceCode>,
}

/// Whether a lane may take, or already holds, one open Design's pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesignAvailability {
    /// Deferred without a holder; waking may atomically claim its pass.
    DeferredUnclaimed,
    /// Deferred while its holder checkpoints and frees its pass.
    DeferredClaimed,
    /// Undeferred and unclaimed.
    Unclaimed,
    /// A lane holds its current pass.
    Claimed,
}

impl DesignCyclePresentation {
    /// Present `design` in `cycle` with the evidence `facts` carries. A
    /// deferred, unheld Design may be woken and claimed atomically; a deferred
    /// held Design remains its holder's responsibility until that lane frees
    /// the pass. `target` is the current round's completion target, not its
    /// remaining holder count.
    #[must_use]
    pub fn new(
        cycle: DesignCycle,
        facts: DesignCycleFacts,
        design: &TaskHandle,
        availability: DesignAvailability,
        coverage: &super::DesignCoverage,
        questions: &[String],
        target: std::num::NonZeroU32,
    ) -> Self {
        let code = |seq: Seq| SequenceCode::new(seq.get());
        let (actor, mut action) = match (cycle, availability) {
            (
                DesignCycle::GatheringInput | DesignCycle::ReadyForPass,
                DesignAvailability::DeferredUnclaimed,
            ) => (
                DesignCycleActor::Lane,
                format!(
                    "a lane wakes and atomically claims {design} with `spice task wake {design} \
                     --claim`"
                ),
            ),
            (
                DesignCycle::GatheringInput | DesignCycle::ReadyForPass,
                DesignAvailability::DeferredClaimed,
            ) => (
                DesignCycleActor::Lane,
                format!(
                    "the holding lane checkpoints its current work and runs `spice task free \
                     {design}`; its claim stays authoritative until then"
                ),
            ),
            (
                DesignCycle::ReadyForPass | DesignCycle::GatheringInput,
                DesignAvailability::Claimed,
            ) if target != std::num::NonZeroU32::MIN => (
                DesignCycleActor::Lane,
                format!(
                    "the holding participant records its completed share of {design} with \
                     `spice task done --validation \"...\"`, then runs `spice task next`"
                ),
            ),
            (DesignCycle::ReadyForPass, DesignAvailability::Unclaimed)
                if target != std::num::NonZeroU32::MIN =>
            {
                (
                    DesignCycleActor::Lane,
                    format!(
                        "a lane claims {design} through `spice task next` for a research pass, \
                         then records its completed share with \
                         `spice task done --validation \"...\"`"
                    ),
                )
            }
            (
                DesignCycle::ReadyForPass | DesignCycle::GatheringInput,
                DesignAvailability::Claimed,
            ) => (
                DesignCycleActor::Lane,
                format!(
                    "the holding lane ends this pass of {design} with `spice task done \
                     --validation \"...\"`, which releases its claim"
                ),
            ),
            _ => (cycle.next_actor(), cycle.next_action(design, facts)),
        };
        let research = matches!(
            cycle,
            DesignCycle::GatheringInput | DesignCycle::ReadyForPass
        )
        .then(|| coverage.research_guidance(questions, design));
        if !coverage.is_full()
            && matches!(
                cycle,
                DesignCycle::GatheringInput | DesignCycle::ReadyForPass
            )
            && availability == DesignAvailability::Claimed
            && target == std::num::NonZeroU32::MIN
        {
            action = format!(
                "the holding lane investigates the remaining questions and records source-backed \
                 findings before `spice task done` ends this pass of {design}"
            );
        }
        Self {
            cycle,
            actor,
            action,
            research,
            last_pass: facts.end,
            pass: facts.pass.map(code),
            decision: facts.decision.map(code),
        }
    }

    /// Plain-text lines naming the state, actor, latest pass, and cited
    /// authorities; callers print [`Self::action`] as their next step.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("cycle {}", self.cycle.label()),
            format!("cycle-actor {}", self.actor.label()),
        ];
        lines.extend(self.research.iter().cloned());
        lines.extend(
            self.last_pass
                .map(|end| format!("last-pass {}", end.label())),
        );
        lines.extend(self.pass.map(|code| format!("pass {code}")));
        lines.extend(self.decision.map(|code| format!("decision {code}")));
        lines
    }
}

/// How many open Designs stand in each pass-cycle state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignCycleCounts {
    /// Designs gathering input.
    pub gathering_input: usize,
    /// Designs ready for a research pass.
    pub ready_for_pass: usize,
    /// Designs awaiting the operator's decision.
    pub awaiting_decision: usize,
    /// Designs waiting on the work they selected.
    pub waiting_on_selected_work: usize,
}

impl DesignCycleCounts {
    /// Count one Design in `cycle`.
    pub fn observe(&mut self, cycle: DesignCycle) {
        *self.slot(cycle) += 1;
    }

    /// Designs counted in `cycle`.
    #[must_use]
    pub fn get(mut self, cycle: DesignCycle) -> usize {
        *self.slot(cycle)
    }

    fn slot(&mut self, cycle: DesignCycle) -> &mut usize {
        match cycle {
            DesignCycle::GatheringInput => &mut self.gathering_input,
            DesignCycle::ReadyForPass => &mut self.ready_for_pass,
            DesignCycle::AwaitingDecision => &mut self.awaiting_decision,
            DesignCycle::WaitingOnSelectedWork => &mut self.waiting_on_selected_work,
        }
    }
}

/// How the latest pass boundary ended its claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DesignPassEnd {
    /// The pass's final participant completed its round.
    Completed,
    /// The claim was freed or lapsed before its round completed.
    Abandoned,
}

impl DesignPassEnd {
    /// Operator-facing name of how the pass ended.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Abandoned => "abandoned",
        }
    }
}

/// Compact cycle evidence one Design carries into allocation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignCycleFacts {
    /// The Design's maturity is `Steeped` at its current statement.
    #[serde(default)]
    pub steeped: bool,
    /// Latest pass boundary: a completed round or a claim release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub horizon: Option<Seq>,
    /// How the latest claim release ended its pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<DesignPassEnd>,
    /// Latest counted input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Seq>,
    /// Latest settlement of work this Design released.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled: Option<Seq>,
    /// Position of the completed pass still awaiting the operator's
    /// decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass: Option<Seq>,
    /// Position of the operator's latest decision of one of its passes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<Seq>,
}

impl DesignCycleFacts {
    /// Whether no cycle authority has touched this task.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The cycle state, given whether work this Design released is still
    /// running, released or deferred. Released work holds the Design until
    /// it settles: accepted work is worked whole before input can open
    /// another pass.
    #[must_use]
    pub fn cycle(self, working: bool) -> DesignCycle {
        if self.pass.is_some() {
            DesignCycle::AwaitingDecision
        } else if working {
            DesignCycle::WaitingOnSelectedWork
        } else if self.counted() {
            DesignCycle::ReadyForPass
        } else {
            DesignCycle::GatheringInput
        }
    }

    fn counted(self) -> bool {
        let Some(horizon) = self.horizon else {
            return self.steeped;
        };
        self.input.is_some_and(|input| input > horizon)
            || self.settled.is_some_and(|settled| settled > horizon)
    }
}

/// One Design's fold state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct DesignCycleFold {
    /// Each charged question item's series; steeping is judged against the
    /// current statement when the facts are read.
    items: DesignItems,
    /// The current pass's round completed since the latest claim release.
    completed: bool,
    facts: DesignCycleFacts,
}

impl DesignCycleFold {
    fn input(&mut self, seq: Seq) {
        self.facts.input = Some(seq);
    }

    fn boundary(&mut self, seq: Seq) {
        self.facts.horizon = Some(self.facts.horizon.map_or(seq, |horizon| horizon.max(seq)));
    }

    fn contributed(&mut self, seq: Seq, contribution: &RoomContributed) {
        self.input(seq);
        self.items.observe(contribution);
    }

    fn completed(&mut self, seq: Seq) {
        self.boundary(seq);
        self.facts.pass.get_or_insert(seq);
        self.completed = true;
    }

    fn decided(&mut self, seq: Seq, outcome: &DecisionOutcome) {
        self.facts.pass = None;
        self.facts.decision = Some(seq);
        let feedback = match outcome {
            DecisionOutcome::Returned { .. } => true,
            DecisionOutcome::Resolved { outputs, .. } => outputs
                .iter()
                .any(|output| output.verdict == OutputVerdict::Rejected),
            DecisionOutcome::Concluded {} | DecisionOutcome::Collapsed { .. } => false,
        };
        if feedback {
            self.input(seq);
        }
    }

    fn released(&mut self, seq: Seq) {
        self.boundary(seq);
        self.facts.end = Some(if self.completed {
            DesignPassEnd::Completed
        } else {
            DesignPassEnd::Abandoned
        });
        self.completed = false;
    }
}

/// Pass-cycle evidence for every Design, folded from journal facts in order.
///
/// A pass is a claim held in the design phase: only a release of a claim won
/// there is a pass boundary, so releasing an intake claim, before or after the
/// phase change into design, leaves the first pass gated on maturity. A settled
/// task's fold is dropped. Released work maps each authorized
/// output or Plan-constructed task to its controlling Design so its settlement
/// reaches that Design.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DesignCycles {
    designs: HashMap<TaskStamp, DesignCycleFold>,
    released_work: HashMap<TaskStamp, TaskStamp>,
    constructed_work: HashMap<TaskStamp, TaskStamp>,
    drafts: HashSet<TaskStamp>,
    /// Tasks whose live claim was won in the design phase.
    passes: HashSet<TaskStamp>,
}

impl DesignCycles {
    /// Whether `kind` can change any Design's cycle evidence.
    #[must_use]
    pub const fn concerns(kind: FactKind) -> bool {
        matches!(
            kind,
            FactKind::RoomContributed
                | FactKind::RoomPublished
                | FactKind::RoomPosted
                | FactKind::TaskRoundCompleted
                | FactKind::PreparationDecided
                | FactKind::ExecutionAuthorized
                | FactKind::TaskAdded
                | FactKind::OutputControlled
                | FactKind::TaskPhased
                | FactKind::ClaimWon
                | FactKind::ClaimFreed
                | FactKind::ClaimLapsed
                | FactKind::TaskDone
                | FactKind::TaskReviewed
                | FactKind::TaskClosed
                | FactKind::TaskDeleted
                | FactKind::TaskConcluded
        )
    }

    /// Current evidence for `design` whose current statement items are
    /// `term`; empty when nothing has touched it.
    #[must_use]
    pub fn facts(&self, design: &TaskStamp, term: &[String]) -> DesignCycleFacts {
        self.designs
            .get(design)
            .map(|fold| DesignCycleFacts {
                steeped: fold.items.steeped(term),
                ..fold.facts
            })
            .unwrap_or_default()
    }

    /// Each charged question item's series for `design`, if anything touched
    /// it.
    #[must_use]
    pub fn items(&self, design: &TaskStamp) -> Option<&DesignItems> {
        self.designs.get(design).map(|fold| &fold.items)
    }

    /// Every Design whose cycle evidence `fact` may change and that the fact
    /// does not already name as its task subject or a mutated task.
    ///
    /// Settled released work reaches its controller through the settled
    /// task's own preparation, so a caller refreshing cached Designs also
    /// refreshes each changed task's controller.
    #[must_use]
    pub fn subjects(fact: &AnyFact) -> Vec<TaskStamp> {
        Self::borrowed_subjects(fact).into_iter().cloned().collect()
    }

    fn borrowed_subjects(fact: &AnyFact) -> Vec<&TaskStamp> {
        if let Some(contribution) = fact.as_body::<RoomContributed>() {
            vec![&contribution.room]
        } else if let Some(published) = fact.as_body::<RoomPublished>() {
            vec![&published.room]
        } else if let Some(posted) = fact.as_body::<RoomPosted>() {
            posted_rooms(posted)
        } else if let Some(decided) = fact.as_body::<PreparationDecided>() {
            vec![&decided.decision.decision.controller]
        } else if let Some(added) = fact.as_body::<TaskAdded>() {
            added
                .preparation
                .iter()
                .map(|preparation| &preparation.controller)
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Fold one fact at journal position `seq` authored by `actor`.
    pub fn observe(&mut self, seq: Seq, actor: &Actor, fact: &AnyFact) {
        match fact.kind() {
            FactKind::RoomContributed | FactKind::RoomPublished | FactKind::RoomPosted => {
                self.observe_input(seq, actor, fact)
            }
            FactKind::TaskRoundCompleted
            | FactKind::PreparationDecided
            | FactKind::ExecutionAuthorized => self.observe_pass(seq, fact),
            FactKind::TaskAdded | FactKind::OutputControlled | FactKind::TaskPhased => {
                self.observe_control(fact)
            }
            FactKind::ClaimWon
            | FactKind::ClaimFreed
            | FactKind::ClaimLapsed
            | FactKind::TaskDone
            | FactKind::TaskReviewed
            | FactKind::TaskClosed
            | FactKind::TaskDeleted
            | FactKind::TaskConcluded => self.observe_task(seq, fact),
            _ => {}
        }
    }

    fn design(&mut self, design: &TaskStamp) -> &mut DesignCycleFold {
        self.designs.entry(design.clone()).or_default()
    }

    fn observe_input(&mut self, seq: Seq, actor: &Actor, fact: &AnyFact) {
        if let Some(contribution) = fact.as_body::<RoomContributed>() {
            self.design(&contribution.room)
                .contributed(seq, contribution);
        } else if matches!(actor, Actor::Principal(_)) {
            self.observe_operator_post(seq, fact);
        }
    }

    /// An operator's authored room post or message counts as input.
    fn observe_operator_post(&mut self, seq: Seq, fact: &AnyFact) {
        if let Some(published) = fact.as_body::<RoomPublished>() {
            if matches!(published.content, RoomPublicationContent::Authored { .. }) {
                self.design(&published.room).input(seq);
            }
        } else if let Some(posted) = fact.as_body::<RoomPosted>() {
            for room in posted_rooms(posted) {
                self.design(room).input(seq);
            }
        }
    }

    fn observe_pass(&mut self, seq: Seq, fact: &AnyFact) {
        if let Some(completed) = fact.as_body::<TaskRoundCompleted>() {
            if completed.phase.as_str() == "design" {
                self.design(&completed.task).completed(seq);
            }
        } else if let Some(decided) = fact.as_body::<PreparationDecided>() {
            let decision = &decided.decision;
            self.design(&decision.decision.controller)
                .decided(seq, &decision.outcome);
        } else if let Some(authorized) = fact.as_body::<ExecutionAuthorized>() {
            let controller = &authorized.authorization.decision.controller;
            for output in &authorized.authorization.selected_outputs {
                if !self.drafts.contains(output) {
                    self.released_work
                        .insert(output.clone(), controller.clone());
                }
            }
        }
    }

    /// Retain construction lineage immediately; only phase-zero advancement
    /// makes Plan-constructed work part of the controller's released set.
    fn observe_control(&mut self, fact: &AnyFact) {
        if let Some(phased) = fact.as_body::<crate::body::TaskPhased>() {
            self.drafts.remove(&phased.task);
            if phased.phase.as_str() != "draft"
                && let Some(controller) = self.constructed_work.remove(&phased.task)
            {
                self.released_work.insert(phased.task.clone(), controller);
            }
            return;
        }
        let preparation = if let Some(added) = fact.as_body::<TaskAdded>() {
            if added.phase == crate::Phase::Draft {
                self.drafts.insert(added.task.clone());
            }
            added
                .preparation
                .as_ref()
                .map(|preparation| (&added.task, preparation))
        } else {
            fact.as_body::<OutputControlled>()
                .map(|controlled| (&controlled.output, &controlled.preparation))
        };
        if let Some((task, preparation)) = preparation {
            if self.drafts.contains(task) {
                self.constructed_work
                    .insert(task.clone(), preparation.controller.clone());
            } else if preparation.parent != preparation.controller {
                self.released_work
                    .insert(task.clone(), preparation.controller.clone());
            }
        }
    }

    /// A claim won in the design phase opens a pass its release ends; a
    /// completed `task::done` or `task::reviewed` positively settles its
    /// task; a phase advance or a finding leaves released work pending.
    fn observe_task(&mut self, seq: Seq, fact: &AnyFact) {
        let released = if let Some(won) = fact.as_body::<ClaimWon>() {
            if won.cell.phase.as_str() == "design" {
                self.passes.insert(won.task.clone());
            } else {
                self.passes.remove(&won.task);
            }
            return;
        } else if let Some(freed) = fact.as_body::<ClaimFreed>() {
            Some(&freed.task)
        } else {
            fact.as_body::<ClaimLapsed>().map(|lapsed| &lapsed.task)
        };
        if let Some(task) = released {
            if self.passes.remove(task) {
                self.design(task).released(seq);
            }
            return;
        }
        let settled = if let Some(done) = fact.as_body::<TaskDone>() {
            if !done
                .terminal
                .as_ref()
                .is_some_and(|terminal| terminal.status == TaskCompletionStatus::Completed)
            {
                return;
            }
            &done.task
        } else if let Some(reviewed) = fact.as_body::<TaskReviewed>() {
            if reviewed.status != TaskCompletionStatus::Completed {
                return;
            }
            &reviewed.task
        } else if let Some(closed) = fact.as_body::<TaskClosed>() {
            &closed.task
        } else if let Some(deleted) = fact.as_body::<TaskDeleted>() {
            &deleted.task
        } else if let Some(concluded) = fact.as_body::<TaskConcluded>() {
            &concluded.task
        } else {
            return;
        };
        self.designs.remove(settled);
        self.passes.remove(settled);
        self.drafts.remove(settled);
        self.constructed_work.remove(settled);
        if let Some(controller) = self.released_work.remove(settled)
            && let Some(design) = self.designs.get_mut(&controller)
        {
            design.facts.settled = Some(seq);
        }
    }
}

/// Each distinct room an operator post addresses: task routes and route
/// contexts.
fn posted_rooms(posted: &RoomPosted) -> Vec<&TaskStamp> {
    let mut rooms = posted
        .routes
        .iter()
        .flat_map(|route| {
            let destination = match &route.destination {
                RoomDestination::Task { task } => Some(task),
                RoomDestination::Principal { .. } | RoomDestination::Lane { .. } => None,
            };
            destination.into_iter().chain(route.context.as_ref())
        })
        .collect::<Vec<_>>();
    rooms.sort_unstable();
    rooms.dedup();
    rooms
}

#[cfg(test)]
mod tests;
