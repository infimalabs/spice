use serde::{Deserialize, Serialize};

use super::is_zero;
use crate::ident::Instant;
use crate::witness::Phase;

/// Live review state carried by a pending workflow.
///
/// Reviewers are the active claims on a review phase, never workflow state, so
/// seating another reviewer moves no workflow and stales no seated one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowReview {
    /// No clean review precedes the current phase.
    Unassigned,
    /// Review completed cleanly before a later phase.
    Clean,
}

impl WorkflowReview {
    /// Stable wire and operator-facing review spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unassigned => "unassigned",
            Self::Clean => "clean",
        }
    }
}

/// Review state retained by a positive terminal workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowCompletionReview {
    /// Completion did not follow a clean review.
    Unassigned,
    /// Completion followed a clean review.
    Clean,
}

impl WorkflowCompletionReview {
    /// Stable wire and operator-facing review spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unassigned => "unassigned",
            Self::Clean => "clean",
        }
    }
}

/// Why a workflow settled without positively completing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowNegative {
    /// The operator closed the task or a Design collapsed.
    Closed,
    /// The task was withdrawn.
    Deleted,
}

/// A terminal workflow's positive or negative conclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowConclusion {
    /// The task completed positively.
    Positive {
        /// Review state retained by the canonical wire document.
        review: WorkflowCompletionReview,
    },
    /// The task settled without approving what it controls.
    Negative(WorkflowNegative),
}

/// One task's live or terminal workflow state.
///
/// The terminal state owns its instant and conclusion, so a completion instant
/// on pending work and a terminal workflow without one are not representable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowStatus {
    /// Work remains live.
    Pending(WorkflowReview),
    /// Work reached a terminal conclusion.
    Settled {
        /// Instant at which the workflow settled.
        at: Instant,
        /// Whether settlement positively completed the task.
        conclusion: WorkflowConclusion,
    },
}

impl WorkflowStatus {
    /// An unassigned live workflow.
    #[must_use]
    pub const fn pending() -> Self {
        Self::Pending(WorkflowReview::Unassigned)
    }

    /// A positive completion that did not follow clean review.
    #[must_use]
    pub const fn completed(at: Instant) -> Self {
        Self::Settled {
            at,
            conclusion: WorkflowConclusion::Positive {
                review: WorkflowCompletionReview::Unassigned,
            },
        }
    }

    /// A positive completion following clean review.
    #[must_use]
    pub const fn reviewed(at: Instant) -> Self {
        Self::Settled {
            at,
            conclusion: WorkflowConclusion::Positive {
                review: WorkflowCompletionReview::Clean,
            },
        }
    }

    /// A negative operator closure or collapsed Design.
    #[must_use]
    pub const fn closed(at: Instant) -> Self {
        Self::Settled {
            at,
            conclusion: WorkflowConclusion::Negative(WorkflowNegative::Closed),
        }
    }

    /// A withdrawn task.
    #[must_use]
    pub const fn deleted(at: Instant) -> Self {
        Self::Settled {
            at,
            conclusion: WorkflowConclusion::Negative(WorkflowNegative::Deleted),
        }
    }

    /// Whether work remains live.
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        matches!(self, Self::Pending(_))
    }

    /// Whether the canonical workflow status is `completed`.
    #[must_use]
    pub const fn is_completed(&self) -> bool {
        matches!(
            self,
            Self::Settled {
                conclusion: WorkflowConclusion::Positive { .. }
                    | WorkflowConclusion::Negative(WorkflowNegative::Closed),
                ..
            }
        )
    }

    /// Whether the task was withdrawn.
    #[must_use]
    pub const fn is_deleted(&self) -> bool {
        matches!(
            self,
            Self::Settled {
                conclusion: WorkflowConclusion::Negative(WorkflowNegative::Deleted),
                ..
            }
        )
    }

    /// Live review state, absent after settlement.
    #[must_use]
    pub const fn review(&self) -> Option<&WorkflowReview> {
        match self {
            Self::Pending(review) => Some(review),
            Self::Settled { .. } => None,
        }
    }

    /// Terminal instant, absent while live.
    #[must_use]
    pub const fn completion(&self) -> Option<Instant> {
        match self {
            Self::Pending(_) => None,
            Self::Settled { at, .. } => Some(*at),
        }
    }

    /// Terminal conclusion, absent while live.
    #[must_use]
    pub const fn conclusion(&self) -> Option<WorkflowConclusion> {
        match self {
            Self::Pending(_) => None,
            Self::Settled { conclusion, .. } => Some(*conclusion),
        }
    }

    /// Closed canonical spelling retained by presentation-only projections.
    #[must_use]
    pub const fn kind(&self) -> WorkflowStatusKind {
        match self {
            Self::Pending(_) => WorkflowStatusKind::Pending,
            Self::Settled {
                conclusion:
                    WorkflowConclusion::Positive { .. }
                    | WorkflowConclusion::Negative(WorkflowNegative::Closed),
                ..
            } => WorkflowStatusKind::Completed,
            Self::Settled {
                conclusion: WorkflowConclusion::Negative(WorkflowNegative::Deleted),
                ..
            } => WorkflowStatusKind::Deleted,
        }
    }

    /// The one word naming how settled work ended, from its stored
    /// conclusion and, for a collapsed Design, the reason that collapse
    /// recorded; absent while live. A collapse and an operator close land as
    /// the same negative conclusion, so only the collapse reason parts them.
    #[must_use]
    pub const fn settlement(&self, collapse_reason: Option<&str>) -> Option<Settlement> {
        match self {
            Self::Pending(_) => None,
            Self::Settled {
                conclusion: WorkflowConclusion::Positive { .. },
                ..
            } => Some(Settlement::Done),
            Self::Settled {
                conclusion: WorkflowConclusion::Negative(WorkflowNegative::Deleted),
                ..
            } => Some(Settlement::Deleted),
            Self::Settled {
                conclusion: WorkflowConclusion::Negative(WorkflowNegative::Closed),
                ..
            } => Some(if collapse_reason.is_some() {
                Settlement::Collapsed
            } else {
                Settlement::Closed
            }),
        }
    }

    /// Canonical wire and operator-facing status spelling.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        self.kind().as_str()
    }
}

/// How settled work ended, as every operator surface names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settlement {
    /// Completed positively: a lane's `task done`, clean review, or a Design
    /// concluded done.
    Done,
    /// The operator closed it without approving what it controls.
    Closed,
    /// The operator collapsed a Design whose premise failed.
    Collapsed,
    /// Withdrawn.
    Deleted,
}

impl Settlement {
    /// The settlement's word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Closed => "closed",
            Self::Collapsed => "collapsed",
            Self::Deleted => "deleted",
        }
    }
}

impl std::fmt::Display for Settlement {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::fmt::Display for WorkflowStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One current task workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowHead {
    /// Ordered phases the task walks.
    pub flow: Vec<Phase>,
    /// Current phase.
    pub phase: Phase,
    /// Current live or terminal workflow state.
    pub status: WorkflowStatus,
    /// Wakeable deferral deadline.
    pub deferral: Option<Instant>,
    /// Whether the author handed this draft off for triage.
    ///
    /// The draft phase's one sub-state: set once by the author's hand-off
    /// (`task file`, or creation without `--draft`) when construction cannot
    /// yet advance; never cleared; never set on a controlled draft; irrelevant
    /// once the workflow leaves draft. It is no activation authority: a triage
    /// holder leaves draft only through the phase-zero validator.
    pub submitted: bool,
    /// Durable task mutation generation.
    ///
    /// A task opens at zero, which is also the baseline every task written
    /// before this field existed carries, so zero is stored as absence. Every
    /// journal transaction that changes its statement, notes, deferral, or
    /// workflow advances this exactly once; claim, lease, and notification
    /// maintenance preserve it. The journal assigns it at commit, so a value
    /// a writer builds into a proposed head is not authority.
    pub generation: u64,
}

impl WorkflowHead {
    /// Whether the workflow still sits in its prospective draft phase.
    #[must_use]
    pub const fn is_draft(&self) -> bool {
        self.phase.is_draft()
    }

    /// Whether the workflow left draft: substantive work that asserts its
    /// statement, takes claims and satisfies prerequisites on completion.
    #[must_use]
    pub const fn is_activated(&self) -> bool {
        !self.phase.is_draft()
    }

    /// Whether the author handed this draft off: still at index zero and
    /// marked submitted. Controller standing is the statement's to add.
    #[must_use]
    pub const fn is_submitted_draft(&self) -> bool {
        self.is_draft() && self.submitted
    }

    /// Whether this workflow concluded positively.
    #[must_use]
    pub const fn concluded_positively(&self) -> bool {
        matches!(
            &self.status,
            WorkflowStatus::Settled {
                conclusion: WorkflowConclusion::Positive { .. },
                ..
            }
        )
    }

    /// Stable review spelling retained by the canonical workflow document.
    #[must_use]
    pub const fn review_str(&self) -> &'static str {
        match &self.status {
            WorkflowStatus::Pending(review) => review.as_str(),
            WorkflowStatus::Settled {
                conclusion: WorkflowConclusion::Positive { review },
                ..
            } => review.as_str(),
            WorkflowStatus::Settled {
                conclusion: WorkflowConclusion::Negative(_),
                ..
            } => "closed",
        }
    }
}

/// Closed canonical status spelling used at summary wire boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowStatusKind {
    /// Work remains live.
    Pending,
    /// Work settled either positively or by operator closure.
    Completed,
    /// Work was withdrawn.
    Deleted,
}

impl WorkflowStatusKind {
    /// Stable wire and operator-facing spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Deleted => "deleted",
        }
    }
}

impl std::fmt::Display for WorkflowStatusKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum WorkflowReviewWire {
    Unassigned,
    Clean,
    Closed,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowHeadWire {
    flow: Vec<Phase>,
    phase: Phase,
    status: WorkflowStatusKind,
    review: WorkflowReviewWire,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    deferral: Option<Instant>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    submitted: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    completion: Option<Instant>,
    #[serde(default, skip_serializing_if = "is_zero")]
    generation: u64,
}

impl Serialize for WorkflowHead {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let (status, review, completion) = match &self.status {
            WorkflowStatus::Pending(WorkflowReview::Unassigned) => (
                WorkflowStatusKind::Pending,
                WorkflowReviewWire::Unassigned,
                None,
            ),
            WorkflowStatus::Pending(WorkflowReview::Clean) => {
                (WorkflowStatusKind::Pending, WorkflowReviewWire::Clean, None)
            }
            WorkflowStatus::Settled {
                at,
                conclusion:
                    WorkflowConclusion::Positive {
                        review: WorkflowCompletionReview::Unassigned,
                    },
            } => (
                WorkflowStatusKind::Completed,
                WorkflowReviewWire::Unassigned,
                Some(*at),
            ),
            WorkflowStatus::Settled {
                at,
                conclusion:
                    WorkflowConclusion::Positive {
                        review: WorkflowCompletionReview::Clean,
                    },
            } => (
                WorkflowStatusKind::Completed,
                WorkflowReviewWire::Clean,
                Some(*at),
            ),
            WorkflowStatus::Settled {
                at,
                conclusion: WorkflowConclusion::Negative(WorkflowNegative::Closed),
            } => (
                WorkflowStatusKind::Completed,
                WorkflowReviewWire::Closed,
                Some(*at),
            ),
            WorkflowStatus::Settled {
                at,
                conclusion: WorkflowConclusion::Negative(WorkflowNegative::Deleted),
            } => (
                WorkflowStatusKind::Deleted,
                WorkflowReviewWire::Closed,
                Some(*at),
            ),
        };
        WorkflowHeadWire {
            flow: self.flow.clone(),
            phase: self.phase,
            status,
            review,
            deferral: self.deferral,
            submitted: self.submitted,
            completion,
            generation: self.generation,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for WorkflowHead {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;

        let wire = WorkflowHeadWire::deserialize(deserializer)?;
        let status = match (wire.status, wire.review, wire.completion) {
            (WorkflowStatusKind::Pending, WorkflowReviewWire::Unassigned, None) => {
                WorkflowStatus::pending()
            }
            (WorkflowStatusKind::Pending, WorkflowReviewWire::Clean, None) => {
                WorkflowStatus::Pending(WorkflowReview::Clean)
            }
            (WorkflowStatusKind::Completed, WorkflowReviewWire::Unassigned, Some(at)) => {
                WorkflowStatus::Settled {
                    at,
                    conclusion: WorkflowConclusion::Positive {
                        review: WorkflowCompletionReview::Unassigned,
                    },
                }
            }
            (WorkflowStatusKind::Completed, WorkflowReviewWire::Clean, Some(at)) => {
                WorkflowStatus::Settled {
                    at,
                    conclusion: WorkflowConclusion::Positive {
                        review: WorkflowCompletionReview::Clean,
                    },
                }
            }
            (WorkflowStatusKind::Completed, WorkflowReviewWire::Closed, Some(at)) => {
                WorkflowStatus::Settled {
                    at,
                    conclusion: WorkflowConclusion::Negative(WorkflowNegative::Closed),
                }
            }
            (WorkflowStatusKind::Deleted, WorkflowReviewWire::Closed, Some(at)) => {
                WorkflowStatus::Settled {
                    at,
                    conclusion: WorkflowConclusion::Negative(WorkflowNegative::Deleted),
                }
            }
            _ => {
                return Err(D::Error::custom(
                    "workflow status, review, and completion disagree",
                ));
            }
        };
        Ok(Self {
            flow: wire.flow,
            phase: wire.phase,
            status,
            deferral: wire.deferral,
            submitted: wire.submitted,
            generation: wire.generation,
        })
    }
}

#[cfg(test)]
mod tests;
