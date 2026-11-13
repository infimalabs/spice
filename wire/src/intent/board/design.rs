//! Design standing and the Design invitation a lane owes.

use serde::{Deserialize, Serialize};

/// One Design invitation a lane owes, whichever occupant reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignRequest {
    /// Design whose room accepts the answer.
    pub design: crate::TaskHandle,
    /// Lane that owes the request.
    pub lane: crate::CanonicalUuid,
    /// Continuing session whose evidence selected the Design.
    pub session: crate::rooms::DesignSession,
    /// Event at which the session profile was evaluated.
    pub opportunity: crate::rooms::DesignNoticeOpportunity,
    /// Exact retained card that supported the match.
    pub evidence: crate::rooms::DesignNoticeEvidence,
    /// The supporting card's bounded authored excerpt.
    pub excerpt: String,
    /// Current Design title.
    pub title: String,
    /// Current Design statement revision.
    pub statement: crate::SequenceCode,
    /// One-line Design context.
    pub pitch: String,
    /// In-order prefix of whole open questions bounded for delivery.
    pub questions: Vec<String>,
    /// Distinctive shared terms that selected this Design.
    pub terms: Vec<String>,
    /// The match that admitted the request; an unsteeped Design admits on a
    /// wider cast.
    pub admission: crate::rooms::DesignNoticeMatch,
    /// Independently qualified questions in need, score and statement order;
    /// empty asks about the whole Design.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub asks: Vec<DesignRequestAsk>,
}

/// One question item a Design request asks about, at its statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignRequestAsk {
    /// The item's text identity.
    pub item: crate::rooms::StatementItem,
    /// The handle the lane names it by: a unique prefix of `item`.
    pub handle: String,
    /// The item verbatim.
    pub text: String,
    /// Where the item stood when asked; a contested item asks a lane already
    /// in its series for a strength-2 re-charge and any other lane for its
    /// charge.
    pub state: crate::rooms::DesignItemState,
    /// Charged posts the item holds.
    pub charges: u64,
    /// Distinct lanes those posts came from, as retained.
    pub lanes: usize,
    /// The exact card that qualified this question, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support: Option<crate::rooms::DesignQuestionSupport>,
}

/// Whether the current heads describe an open Design.
///
/// This is the canonical constant-time rule used by authority and presentation
/// surfaces. A Design-intent task in an intake phase is ordinary claimed work,
/// not an open Design.
#[must_use]
pub fn is_open_design(intent: Option<crate::TaskIntent>, workflow: &crate::WorkflowHead) -> bool {
    intent.is_some_and(crate::TaskIntent::is_design)
        && workflow.status.is_pending()
        && workflow.phase.as_str() == "design"
}

/// The parent of an activated construction root. Draft membership never
/// masquerades as released work, and execution dependencies carry no authority.
#[must_use]
pub fn released_construction_root<'a>(
    born: &crate::TaskStamp,
    workflow: &crate::WorkflowHead,
    preparation: Option<&'a crate::TaskPreparation>,
) -> Option<&'a crate::TaskStamp> {
    let preparation = preparation?;
    (preparation.construction == *born && preparation.parent != *born && workflow.is_activated())
        .then_some(&preparation.parent)
}

/// One Design's lifecycle standing. An open Design is deferred while its own
/// deferral stands and open otherwise; only the operator concludes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum DesignStanding {
    /// Open and deferred: it invites no lane and no path grants it a claim
    /// until it is woken.
    Deferred,
    /// Open and undeferred: lanes may be invited to it or claim it.
    Open,
    /// The operator concluded it done.
    Done,
    /// The operator concluded it collapsed. A retained record concluded
    /// before collapse recorded a reason carries none.
    Collapsed {
        /// Why its premise did not hold.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The operator withdrew it.
    Deleted,
}

impl DesignStanding {
    /// Derive the standing every board, task, group, room, and browser read
    /// shows from current heads, so terminal settlement outranks any retained
    /// group membership; `None` for a task without Design intent.
    pub fn of(
        intent: Option<crate::TaskIntent>,
        workflow: &crate::WorkflowHead,
        collapse_reason: Option<&str>,
    ) -> Option<Self> {
        if !intent.is_some_and(crate::TaskIntent::is_design) {
            return None;
        }
        Some(match workflow.status {
            crate::WorkflowStatus::Settled {
                conclusion: crate::WorkflowConclusion::Negative(crate::WorkflowNegative::Deleted),
                ..
            } => Self::Deleted,
            crate::WorkflowStatus::Settled {
                conclusion: crate::WorkflowConclusion::Positive { .. },
                ..
            } => Self::Done,
            crate::WorkflowStatus::Settled {
                conclusion: crate::WorkflowConclusion::Negative(crate::WorkflowNegative::Closed),
                ..
            } => Self::Collapsed {
                reason: collapse_reason.map(str::to_owned),
            },
            crate::WorkflowStatus::Pending(_) if !is_open_design(intent, workflow) => return None,
            crate::WorkflowStatus::Pending(_) if workflow.deferral.is_some() => Self::Deferred,
            crate::WorkflowStatus::Pending(_) => Self::Open,
        })
    }

    /// Whether the Design is still open.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        matches!(self, Self::Deferred | Self::Open)
    }

    /// Stable machine key.
    #[must_use]
    pub const fn key(&self) -> &'static str {
        match self {
            Self::Deferred => "deferred",
            Self::Open => "open",
            Self::Done => "done",
            Self::Collapsed { .. } => "collapsed",
            Self::Deleted => "deleted",
        }
    }
}

impl std::fmt::Display for DesignStanding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deferred => formatter.write_str("open deferred (wake it to resume)"),
            Self::Open => formatter.write_str("open"),
            Self::Done => formatter.write_str("concluded done"),
            Self::Collapsed {
                reason: Some(reason),
            } => write!(formatter, "concluded collapsed: {reason}"),
            Self::Collapsed { reason: None } => {
                formatter.write_str("concluded collapsed (no reason recorded)")
            }
            Self::Deleted => formatter.write_str("deleted"),
        }
    }
}
