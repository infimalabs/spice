//! The operator's decision on one completed Design pass.

use serde::{Deserialize, Serialize};

use super::{DecisionRef, OutputRevision, RoomSource};
use crate::{Seq, TaskStamp};

/// One output's verdict in a decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputVerdict {
    /// Release this output.
    Accepted,
    /// Close this output: the operator's veto settles it in the decision.
    Rejected,
}

/// The operator's exact verdict and optional comment for one offered output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputDecision {
    /// Offered output this verdict addresses.
    pub output: TaskStamp,
    /// Whether this output is released.
    pub verdict: OutputVerdict,
    /// Optional operator comment about this output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// What the operator read when deciding: the Design's statement and room
/// heads and the exact revision of every offered output. A move of any of
/// them after that read refuses the decision as stale.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionBasis {
    /// Design statement head.
    pub statement: Seq,
    /// Design room head; absent while the room holds nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<Seq>,
    /// Offered outputs at their exact revisions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<OutputRevision>,
}

/// Exact operator disposition of one completed Design pass.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum DecisionOutcome {
    /// Return the Design for another pass without releasing or concluding.
    Returned {
        /// Nonempty operator feedback for the next pass.
        reason: String,
    },
    /// Record one explicit verdict for every offered output.
    Resolved {
        /// Unique output verdicts.
        outputs: Vec<OutputDecision>,
        /// Optional comment applying to the whole decision.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        comment: Option<String>,
    },
    /// Conclude the Design: its purpose is delivered.
    Concluded {},
    /// Collapse the Design: its premise does not hold.
    Collapsed {
        /// Nonempty reason the premise failed.
        reason: String,
    },
}

impl DecisionOutcome {
    /// Work outputs this decision authorizes, in recorded order.
    pub fn accepted_outputs(&self) -> impl Iterator<Item = &TaskStamp> {
        self.outputs_with(OutputVerdict::Accepted)
    }

    /// Work outputs this decision closes, in recorded order.
    pub fn rejected_outputs(&self) -> impl Iterator<Item = &TaskStamp> {
        self.outputs_with(OutputVerdict::Rejected)
    }

    fn outputs_with(&self, verdict: OutputVerdict) -> impl Iterator<Item = &TaskStamp> {
        let outputs = match self {
            Self::Resolved { outputs, .. } => outputs.as_slice(),
            _ => &[],
        };
        outputs
            .iter()
            .filter(move |output| output.verdict == verdict)
            .map(|output| &output.output)
    }

    /// Whether this decision carries feedback for the Design's next holder.
    ///
    /// Accepted outputs with no comments release work without creating another
    /// input. A return, a rejection or either comment does.
    #[must_use]
    pub fn carries_feedback(&self) -> bool {
        match self {
            Self::Returned { .. } => true,
            Self::Resolved { outputs, comment } => {
                comment.is_some()
                    || outputs.iter().any(|output| {
                        output.verdict == OutputVerdict::Rejected || output.comment.is_some()
                    })
            }
            Self::Concluded {} | Self::Collapsed { .. } => false,
        }
    }
}

/// One source-backed operator decision under a controlling Design.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PreparationDecisionWire", into = "PreparationDecisionWire")]
pub struct PreparationDecision {
    /// Stable decision identity.
    pub decision: DecisionRef,
    /// The completed pass this decision disposes of.
    pub source: RoomSource,
    /// The heads the operator read.
    pub basis: DecisionBasis,
    /// Return, output resolution, conclusion or collapse.
    pub outcome: DecisionOutcome,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparationDecisionWire {
    decision: DecisionRef,
    source: RoomSource,
    basis: DecisionBasis,
    outcome: DecisionOutcome,
}

impl TryFrom<PreparationDecisionWire> for PreparationDecision {
    type Error = &'static str;

    fn try_from(value: PreparationDecisionWire) -> Result<Self, Self::Error> {
        if !unique(value.basis.outputs.iter().map(|revision| &revision.task)) {
            return Err("a decision basis names each offered output once");
        }
        match &value.outcome {
            DecisionOutcome::Resolved { outputs, comment } => {
                if outputs.is_empty() || !unique(outputs.iter().map(|output| &output.output)) {
                    return Err("an output resolution must name a nonempty unique output set");
                }
                if outputs
                    .iter()
                    .any(|output| invalid_optional_comment(output.comment.as_deref()))
                    || invalid_optional_comment(comment.as_deref())
                {
                    return Err("decision comments must be nonempty and trimmed when present");
                }
            }
            DecisionOutcome::Returned { reason } | DecisionOutcome::Collapsed { reason }
                if reason.is_empty() || reason.trim() != reason =>
            {
                return Err("a return or collapse requires a nonempty trimmed reason");
            }
            DecisionOutcome::Returned { .. }
            | DecisionOutcome::Concluded {}
            | DecisionOutcome::Collapsed { .. } => {}
        }
        Ok(Self {
            decision: value.decision,
            source: value.source,
            basis: value.basis,
            outcome: value.outcome,
        })
    }
}

fn unique<'a>(mut tasks: impl Iterator<Item = &'a TaskStamp>) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    tasks.all(|task| seen.insert(task))
}

fn invalid_optional_comment(comment: Option<&str>) -> bool {
    comment.is_some_and(|comment| comment.is_empty() || comment.trim() != comment)
}

impl From<PreparationDecision> for PreparationDecisionWire {
    fn from(value: PreparationDecision) -> Self {
        Self {
            decision: value.decision,
            source: value.source,
            basis: value.basis,
            outcome: value.outcome,
        }
    }
}

/// Command body recording one preparation decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparationDecide {
    /// Source-backed decision to record.
    pub decision: PreparationDecision,
}

/// Immutable fact body recording one preparation decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparationDecided {
    /// Source-backed decision that was recorded.
    pub decision: PreparationDecision,
}
