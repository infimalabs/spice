//! Lane-attributed task-workflow transition pages.

use serde::{Deserialize, Serialize};

/// Most lanes one task-timeline read follows.
pub const TASK_TIMELINE_LANE_LIMIT: usize = 32;
/// Largest task-timeline page one read accepts.
pub const TASK_TIMELINE_PAGE_LIMIT_MAX: u16 = 100;

/// Page the task-workflow transitions a set of lanes caused, newest first.
///
/// A transition belongs to the lane that caused it or that it binds: a claim
/// to its lane, a review or reviewer seat to its reviewer, and any other
/// lifecycle fact to the lane that authored it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskTimeline {
    /// Distinct lanes whose transitions the page follows.
    pub lanes: Vec<crate::CanonicalUuid>,
    /// Oldest position the preceding page returned; absent for the newest page.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub before: Option<crate::Seq>,
    /// Maximum transitions returned by this page.
    pub limit: u16,
}

/// One closed task-workflow transition kind a timeline reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum TaskTimelineEvent {
    Added {},
    Edited {},
    /// The workflow head advanced into `phase`.
    Phased {
        phase: crate::Phase,
    },
    Completed {},
    /// The lane was seated as reviewer of `phase`.
    Assigned {
        phase: crate::Phase,
    },
    Reviewed {
        verdict: crate::body::Verdict,
    },
    Deferred {},
    Woken {},
    /// The author handed the draft off as triage work.
    Submitted {},
    Deleted {},
    /// The lane won possession.
    Claimed {},
    /// Possession passed to the lane's current occupant.
    Carried {},
    /// The lane recovered possession after residency moved ahead of it.
    Rebound {},
    /// The lane released possession.
    Released {},
    /// The lane's lease expired.
    Lapsed {},
}

/// One attributed task-workflow transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskTimelineEntry {
    /// Journal position of the fact that recorded the transition.
    pub position: crate::Seq,
    pub at: crate::Instant,
    pub lane: crate::CanonicalUuid,
    pub task: crate::TaskStamp,
    pub handle: crate::TaskHandle,
    pub title: String,
    pub event: TaskTimelineEvent,
}

/// One newest-first page of a lane set's task-workflow transitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskTimelineAnswer {
    pub snapshot: crate::Seq,
    /// The request's lanes in canonical order.
    pub lanes: Vec<crate::CanonicalUuid>,
    /// Transitions in descending journal position.
    pub entries: Vec<TaskTimelineEntry>,
    /// `before` for the next older page, absent once this snapshot is exhausted.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub next: Option<crate::Seq>,
}
