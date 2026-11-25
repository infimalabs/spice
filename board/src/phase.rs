use std::fmt;

use wire::{Instant, Phase, TaskHandle, WorkflowHead};

use crate::claim::Active;
use crate::task::Entry;

/// Exclusive ownership opportunity after one phase advances.
///
/// This is the same short settling interval automatic lifecycle uses before it
/// seats newly ready work. Task birth keeps its existing scheduler treatment;
/// only transition ownership consults this bound.
pub const HANDOFF_SECONDS: i64 = 3;
/// End of the bounded peer-first opportunity after an independent handoff.
///
/// The prior author still waits only [`HANDOFF_SECONDS`] when no peer can take
/// the row. This longer bound applies only while a currently eligible,
/// unclaimed peer exists.
pub const PEER_HANDOFF_SECONDS: i64 = 30;

/// One guarded workflow change ready for the journal transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transition {
    task: TaskHandle,
    kind: TransitionKind,
    next: WorkflowHead,
}

impl Transition {
    /// Guard an advance to the immediately following declared phase.
    pub fn advance(
        entry: &Entry,
        active: &Active<'_>,
        target: Phase,
    ) -> Result<Self, PhaseRefusal> {
        bind_claim(entry, active)?;
        let workflow = entry.workflow();
        if !workflow.status.is_pending() {
            return Err(PhaseRefusal::Completed);
        }

        let from = workflow
            .flow
            .iter()
            .position(|phase| phase == &workflow.phase)
            .expect("Entry admits only a current phase inside its flow");
        let to = workflow
            .flow
            .iter()
            .position(|phase| phase == &target)
            .ok_or(PhaseRefusal::TargetOutsideFlow(target))?;
        if to != from + 1 {
            return Err(PhaseRefusal::Illegal {
                from: workflow.phase,
                to: target,
            });
        }

        let mut next = workflow.clone();
        let previous = next.phase;
        next.phase = target;
        Ok(Self {
            task: entry.handle().clone(),
            kind: TransitionKind::Advance {
                from: previous,
                to: target,
            },
            next,
        })
    }

    /// Guard completion from the final declared phase.
    ///
    /// A withdrawn task is settled by the same clause that settles a completed
    /// one: withdrawal stamps `completion`, so no separate status check is
    /// needed here and none is reachable anyway. Withdrawal also frees the
    /// claim, and possession is admitted before this runs.
    pub fn complete(entry: &Entry, active: &Active<'_>, at: Instant) -> Result<Self, PhaseRefusal> {
        bind_claim(entry, active)?;
        let workflow = entry.workflow();
        if !workflow.status.is_pending() {
            return Err(PhaseRefusal::Completed);
        }
        if workflow.flow.last() != Some(&workflow.phase) {
            return Err(PhaseRefusal::NotFinal(workflow.phase));
        }
        Ok(settle(entry, workflow, at))
    }

    /// Guard a Plan's no-construction disposition from its current phase.
    ///
    /// Nothing remains to execute, so the workflow passes every later phase
    /// before its first later review, which still verifies the disposition; a
    /// flow with no later review settles. No execution phase ever holds a Plan
    /// that constructs nothing.
    pub fn dispose(entry: &Entry, active: &Active<'_>, at: Instant) -> Result<Self, PhaseRefusal> {
        bind_claim(entry, active)?;
        let workflow = entry.workflow();
        if !workflow.status.is_pending() {
            return Err(PhaseRefusal::Completed);
        }
        let from = workflow
            .flow
            .iter()
            .position(|phase| phase == &workflow.phase)
            .expect("Entry admits only a current phase inside its flow");
        let Some(review) = workflow.flow[from + 1..]
            .iter()
            .find(|phase| phase.as_str() == "review")
        else {
            return Ok(settle(entry, workflow, at));
        };
        let mut next = workflow.clone();
        next.phase = *review;
        Ok(Self {
            task: entry.handle().clone(),
            kind: TransitionKind::Advance {
                from: workflow.phase,
                to: *review,
            },
            next,
        })
    }

    /// Guard a transition after a durable shared round has no active claim.
    ///
    /// Every participant was admitted under its exact claim before arriving;
    /// this constructor validates only the ladder transition because no
    /// arbitrary final holder remains to bind. A review round's arrivals are
    /// clean verdicts, so closing one is a clean review. An open Design's
    /// round ends its pass in place for the operator's decision.
    pub fn after_round(
        entry: &Entry,
        at: Instant,
        disposition: bool,
    ) -> Result<Self, PhaseRefusal> {
        let workflow = entry.workflow();
        if !workflow.status.is_pending() {
            return Err(PhaseRefusal::Completed);
        }
        if wire::intent::is_open_design(entry.statement().intent, workflow) {
            return Ok(Self {
                task: entry.handle().clone(),
                kind: TransitionKind::Pass {
                    phase: workflow.phase,
                },
                next: workflow.clone(),
            });
        }
        let from = workflow
            .flow
            .iter()
            .position(|phase| phase == &workflow.phase)
            .expect("Entry admits only a current phase inside its flow");
        let next_phase = if disposition {
            workflow.flow[from + 1..]
                .iter()
                .find(|phase| phase.as_str() == "review")
        } else {
            workflow.flow.get(from + 1)
        };
        let transition = match next_phase {
            Some(next_phase) => {
                let mut next = workflow.clone();
                next.phase = *next_phase;
                Self {
                    task: entry.handle().clone(),
                    kind: TransitionKind::Advance {
                        from: workflow.phase,
                        to: *next_phase,
                    },
                    next,
                }
            }
            None => settle(entry, workflow, at),
        };
        Ok(if workflow.phase.as_str() == "review" {
            transition.reviewed()
        } else {
            transition
        })
    }

    /// This transition as a clean review records it: the next head carries a
    /// clean review, live into a later phase or retained by its settlement.
    #[must_use]
    pub fn reviewed(mut self) -> Self {
        match &mut self.next.status {
            wire::WorkflowStatus::Pending(review) => *review = wire::WorkflowReview::Clean,
            wire::WorkflowStatus::Settled {
                conclusion: wire::WorkflowConclusion::Positive { review },
                ..
            } => *review = wire::WorkflowCompletionReview::Clean,
            wire::WorkflowStatus::Settled {
                conclusion: wire::WorkflowConclusion::Negative(_),
                ..
            } => unreachable!("a review transition never settles negatively"),
        }
        self
    }

    /// Task whose workflow head changes.
    #[must_use]
    pub const fn task(&self) -> &TaskHandle {
        &self.task
    }

    /// Kind of guarded change.
    #[must_use]
    pub const fn kind(&self) -> &TransitionKind {
        &self.kind
    }

    /// Replacement head to commit with the transition fact.
    #[must_use]
    pub const fn next(&self) -> &WorkflowHead {
        &self.next
    }
}

/// Closed workflow transitions board can propose.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransitionKind {
    Advance {
        from: Phase,
        to: Phase,
    },
    Complete {
        phase: Phase,
    },
    /// An open Design's completed pass: the workflow stays in place, awaiting
    /// the operator's decision.
    Pass {
        phase: Phase,
    },
}

/// Why no workflow transition was produced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PhaseRefusal {
    ClaimForAnotherTask,
    TargetOutsideFlow(Phase),
    Illegal { from: Phase, to: Phase },
    NotFinal(Phase),
    Completed,
}

/// The one deliverable a declared phase authorizes.
///
/// A flow is an open vocabulary, so a phase this repository does not spell
/// answers `None` rather than guessing at what it wants. The declared
/// stations differ in what they may touch, which is the whole reason a lane
/// must read its phase before it acts: design and plan move board state, todo
/// moves repository files, and review returns a verdict on work already
/// integrated.
#[must_use]
pub fn deliverable(phase: &Phase) -> &'static str {
    match *phase {
        Phase::Design => {
            "board state is the deliverable: investigate every question and weigh each with a \
             nonzero `spice room contribute HANDLE --charge=C --question HEX TEXT`. When the \
             evidence supports the Design, construct the outputs that would deliver it under \
             this held Design with `spice task add TITLE --pitch TEXT --term TEXT`, ordinary \
             tasks or a Plan, building only what the room's contributions or other cited \
             sources earn; those outputs are what the operator decides. A pass that constructs \
             none posts why with `spice room post HANDLE TEXT`: the Design's purpose is \
             delivered or its premise failed; ending an empty pass without that post refuses. \
             Then record your completed share with `spice task done --validation \"...\"` and \
             run `spice task next`; the final share ends the pass for the operator's decision, \
             and only the operator concludes a Design"
        }
        Phase::Draft => {
            "board state is the deliverable: make the acceptance explicit or decompose the \
             work into accepted children, settle its flow and placement, then advance it with \
             `spice task done --validation \"...\"`; do not edit or commit repository files"
        }
        Phase::Plan => {
            "board state is the deliverable: make the execution contract explicit or \
             decompose it into accepted children; do not edit or commit repository files"
        }
        Phase::Todo => {
            "repository work is the deliverable: implement the acceptance this task \
             carries, prove it, and commit while holding this claim"
        }
        Phase::Review => {
            "a verdict is the deliverable: inspect the integrated diff, check its \
             evidence, and record a clean or finding review"
        }
    }
}

/// Deliverable override when an independent phase returns to its prior author.
///
/// Independent handoff is ranked last, never excluded, so a quiet board can
/// still progress without weakening the preference for another owner.
pub const SELF_HANDOFF_DELIVERABLE: &str = "the current phase deliverable remains authoritative: \
     the allocator assigned this independent phase back to its prior author as the last resort \
     because no peer work is eligible. This assignment authorizes and requires you to complete \
     the phase now; do not free or defer it merely because you authored the prior phase";

/// What a lane is told when it wins back a row it freed.
///
/// A freed row ranks last rather than being excluded, so it returns once the
/// lane has nothing else. Freeing it again would only repeat the loop; the
/// lasting answer is to state what the row waits on. An open Design never
/// waits that way: its pass cycle already says why it returned, so a
/// completed pass points at the decision and input since, and an abandoned
/// pass is finished with `task done`.
#[must_use]
pub fn released_reminder(entry: &Entry) -> String {
    let task = entry.handle();
    if wire::intent::is_open_design(entry.statement().intent, entry.workflow()) {
        return match entry.design().end {
            Some(wire::rooms::DesignPassEnd::Completed) => format!(
                "your earlier pass of {task} completed; this pass answers the operator's \
                 decision and the input recorded since, read through \
                 `spice task authority {task}`, and ends with `spice task done`"
            ),
            Some(wire::rooms::DesignPassEnd::Abandoned) | None => format!(
                "your earlier pass of {task} ended before its round completed; finish this pass \
                 with `spice task done`, constructing the outputs its evidence supports or \
                 posting with `spice room post {task}` whether the Design's purpose is \
                 delivered or its premise did not hold"
            ),
        };
    }
    format!(
        "you freed {task} earlier; if it still cannot proceed, record why instead of \
         freeing it again: \
         `spice task depends {task} AFTER...` for a prerequisite task, \
         `spice task require {task} --journal-schema SCHEMA | --trust-grant DIGEST | \
         --appliance-after VERSION` for the environment, or `spice task defer {task} UNTIL`"
    )
}

fn bind_claim(entry: &Entry, active: &Active<'_>) -> Result<(), PhaseRefusal> {
    if active.handle() == entry.handle() {
        Ok(())
    } else {
        Err(PhaseRefusal::ClaimForAnotherTask)
    }
}

/// Settle a pending workflow positively from its current phase, carrying its
/// live review state into the completion.
fn settle(entry: &Entry, workflow: &WorkflowHead, at: Instant) -> Transition {
    let review = match workflow.status.review() {
        Some(wire::WorkflowReview::Clean) => wire::WorkflowCompletionReview::Clean,
        Some(wire::WorkflowReview::Unassigned) => wire::WorkflowCompletionReview::Unassigned,
        None => unreachable!("pending workflow has a live review"),
    };
    let mut next = workflow.clone();
    next.status = wire::WorkflowStatus::Settled {
        at,
        conclusion: wire::WorkflowConclusion::Positive { review },
    };
    Transition {
        task: entry.handle().clone(),
        kind: TransitionKind::Complete {
            phase: workflow.phase,
        },
        next,
    }
}

impl fmt::Display for PhaseRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClaimForAnotherTask => {
                formatter.write_str("active claim belongs to another task")
            }
            Self::TargetOutsideFlow(phase) => {
                write!(formatter, "target phase {phase} is outside the flow")
            }
            Self::Illegal { from, to } => {
                write!(formatter, "workflow cannot transition from {from} to {to}")
            }
            Self::NotFinal(phase) => write!(formatter, "phase {phase} is not the final station"),
            Self::Completed => formatter.write_str("workflow is already completed"),
        }
    }
}

impl std::error::Error for PhaseRefusal {}
