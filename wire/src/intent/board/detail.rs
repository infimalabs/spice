//! One task's current evidence and its durable workflow history.

use serde::{Deserialize, Serialize};

use super::BoardTask;
use crate::Actor;

/// Read one task's current evidence and durable note history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDetail {
    /// Sole durable task identity.
    pub task: crate::TaskStamp,
}

/// The operator's closure of a task, read back from its `task::closed` fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDetailClosure {
    /// Position of the closure fact.
    pub position: crate::Seq,
    /// Commit time recorded by the journal.
    pub at: crate::Instant,
    /// Principal that closed the task.
    pub author: Actor,
    /// Stated evidence or reason the task is complete.
    pub validation: String,
    /// Phase the workflow held when the task was closed.
    pub phase: crate::Phase,
    /// Actor whose live claim the closure overrode, when it overrode one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub claimed: Option<Actor>,
}

/// The operator's withdrawal of a task, read back from its `task::deleted`
/// fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDetailDeletion {
    /// Position of the withdrawal fact.
    pub position: crate::Seq,
    /// Commit time recorded by the journal.
    pub at: crate::Instant,
    /// Principal that withdrew the task.
    pub author: Actor,
    /// Stated reason for the withdrawal.
    pub reason: String,
    /// Actor whose live claim the withdrawal overrode, when it overrode one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub claimed: Option<Actor>,
}

/// The disposition a Plan's lane recorded instead of constructing work, read
/// back from the Plan's latest completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDetailDisposition {
    /// Position of the completion fact.
    pub position: crate::Seq,
    /// Commit time recorded by the journal.
    pub at: crate::Instant,
    /// Principal that recorded it.
    pub author: Actor,
    /// What answers the reported need.
    pub disposition: crate::body::PlanDisposition,
    /// Evidence the disposition rests on.
    pub validation: String,
}

/// One workflow step on a task's history, read back from its lifecycle fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDetailStep {
    /// Position of the step's fact.
    pub position: crate::Seq,
    /// Commit time recorded by the journal.
    pub at: crate::Instant,
    /// Principal that took the step.
    pub author: Actor,
    /// What the step recorded.
    pub step: TaskStep,
}

/// What one workflow step recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskStep {
    /// A claimed phase completed on its stated validation.
    Done {
        /// Phase the completion closed.
        phase: crate::Phase,
        /// Evidence the completion rests on.
        validation: String,
        /// Why a Plan's plan phase completed without constructing work.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        disposition: Option<crate::body::PlanDisposition>,
    },
    /// The workflow advanced into a phase.
    Phased {
        /// Phase the workflow entered.
        phase: crate::Phase,
    },
    /// A review verdict landed.
    Reviewed {
        /// How the review closed.
        verdict: crate::body::Verdict,
        /// Whether this verdict completed the task or left later work pending.
        status: crate::body::TaskCompletionStatus,
        /// A finding with its note, or a clean verdict's note.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        text: Option<String>,
        /// Correction tasks the review linked.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        followups: Vec<crate::TaskStamp>,
    },
    /// The operator concluded an open Design.
    Concluded {
        /// Done, or collapsed with the stated reason its premise failed.
        conclusion: crate::body::DesignConclusion,
    },
}

/// One acknowledged steering directive on a task's history: a task or claim
/// fact whose `ack:` origin names it, or a `steer::acked` fact by which the
/// task's holder acknowledged operator steering while holding the task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAcknowledgment {
    /// Position of the carrying fact.
    pub position: crate::Seq,
    /// Kind of that fact.
    pub kind: crate::FactKind,
    /// Steering key the fact names.
    pub key: String,
}

/// The latest recorded review of a task, independent of repository context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDetailReview {
    /// Position of the authoritative `task::reviewed` fact.
    pub position: crate::Seq,
    /// Commit time recorded by the journal.
    pub at: crate::Instant,
    /// Recorded reviewer, resolved through the principal directory.
    pub reviewer: Actor,
    /// The review's verdict.
    pub verdict: crate::body::Verdict,
    /// Whether this review completed the task.
    pub status: crate::body::TaskCompletionStatus,
    /// The finding or clean review's note, when supplied.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub text: Option<String>,
}

/// Current evidence for one selected task; its conversation is its room.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDetailAnswer {
    /// Current task heads projected at the reply's snapshot barrier.
    pub task: BoardTask,
    /// Bounded browser-renderable artifacts in task citation order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<crate::Media>,
    /// The operator's closure, when the task was closed rather than done.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub closure: Option<TaskDetailClosure>,
    /// The operator's withdrawal, when the task was deleted.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub deletion: Option<TaskDetailDeletion>,
    /// The no-construction disposition the Plan's latest completion recorded.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub disposition: Option<TaskDetailDisposition>,
    /// Latest review authority, including reviews without a Git boundary.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub review: Option<TaskDetailReview>,
    /// The steering directive a steering origin names, when its key resolves.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub origin: Option<TaskDetailOrigin>,
    /// Resident presentation of each quote link in the task description, in
    /// description order, as an agent message carries its own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub description_quotes: Vec<crate::QuotePresentation>,
}

/// The steering directive behind a task's origin, which its room shows as its
/// first card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDetailOrigin {
    /// Instant the directive was published.
    pub published: crate::Instant,
    /// The resident's presentation of the directive itself: its whole text,
    /// recorded author and disposition, and every quote it carries, or why it
    /// no longer resolves.
    pub directive: crate::QuotePresentation,
}
