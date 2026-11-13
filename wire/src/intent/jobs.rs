//! Operator job evidence and keeper-owned termination, independent of delivery.

use super::LaneJobKind;
use crate::{SequenceCode, TaskStamp};
use serde::{Deserialize, Serialize};

const JOB_KEY_WIDTH: usize = 9;

/// A validated topology slot, not a newly minted lane identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneJobScope {
    /// Existing topology revision in shortest native base52 spelling.
    pub topology: SequenceCode,
    /// Position in that exact resident lane table, also in base52.
    pub lane: SequenceCode,
}

impl LaneJobScope {
    /// Bind the catalog to the authority owning this slot.
    #[must_use]
    pub const fn new(topology: u64, lane: usize) -> Self {
        Self {
            topology: SequenceCode::new(topology),
            lane: SequenceCode::new(lane as u64),
        }
    }

    /// Changed topology or an absent slot resolves to no lane.
    #[must_use]
    pub fn slot(self, topology: u64, lanes: usize) -> Option<usize> {
        if self.topology.get() != topology {
            return None;
        }
        usize::try_from(self.lane.get())
            .ok()
            .filter(|slot| *slot < lanes)
    }
}

/// An immutable keeper key addressed within one validated topology slot.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneJobRef {
    /// Context supplied by the operator catalog.
    pub scope: LaneJobScope,
    /// Native microsecond job key, never a PID or filesystem path.
    #[serde(deserialize_with = "keeper_key")]
    pub id: String,
}

/// Read every retained job without claiming its agent delivery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneJobsRead {}

/// Keeper lifecycle, separate from whether its agent heard the result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LaneJobPhase {
    /// Request exists before keeper start is published.
    Starting,
    /// Keeper still runs the command.
    Running,
    /// Stop requested but ending not observed.
    Stopping,
    /// Ending published, or keeper lost without one.
    Ended,
}

/// Command metadata only; the catalog does not read each job's output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeptJob {
    /// Native base52 job key.
    #[serde(deserialize_with = "keeper_key")]
    pub id: String,
    /// Complete shell command declared by its agent.
    pub command: String,
    /// Ambient task held when the job was launched, if any.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub task: Option<TaskStamp>,
    /// Command, service, or event watch.
    pub kind: LaneJobKind,
    /// Keeper-observed lifecycle.
    pub phase: LaneJobPhase,
    /// Published start, absent while starting.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub started_millis: Option<u64>,
}

/// Retained jobs under one current owner, addressed without a UUID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneJobCatalog {
    /// Scope of every job in this owner group.
    pub scope: LaneJobScope,
    /// Operator's current display name, if declared.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub name: Option<String>,
    /// Admitted owner worktree; execution location, not an identity handle.
    pub worktree: String,
    /// Jobs whose endings have not been heard by their agent.
    pub jobs: Vec<KeptJob>,
}

/// One operator census; empty owners are omitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneJobsAnswer {
    /// Current owners with retained jobs.
    pub lanes: Vec<LaneJobCatalog>,
}

/// Bounded evidence; neither reading nor stopping claims agent delivery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneJobAnswer {
    /// Exact reference this evidence answers.
    pub target: LaneJobRef,
    /// Complete shell command declared by its agent.
    pub command: String,
    /// Actual working directory declared for execution.
    pub cwd: String,
    /// Ambient task held when the job was launched, if any.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub task: Option<TaskStamp>,
    /// Command, service, or event watch.
    pub kind: LaneJobKind,
    /// Current keeper lifecycle.
    pub phase: LaneJobPhase,
    /// Published keeper start.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub started_millis: Option<u64>,
    /// Deadline duration, absent for services.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub timeout_millis: Option<u64>,
    /// Ending instant, absent when not observed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub ended_millis: Option<u64>,
    /// Exit, signal, deadline, stop, failure, or lost-keeper evidence.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub ending: Option<String>,
    /// Most recent output, bounded by the keeper's tail policy.
    pub output: String,
    /// Earlier output exists outside this preview.
    pub output_truncated: bool,
}

fn keeper_key<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<String, D::Error> {
    let key = String::deserialize(decoder)?;
    if !key.is_empty() && key.len() <= JOB_KEY_WIDTH && crate::base52::decode(&key).is_ok() {
        Ok(key)
    } else {
        Err(serde::de::Error::custom(
            "job key must be native base52, not a UUID or path",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn topology_slots_cannot_retarget_after_reordering_or_removal() {
        let scope = LaneJobScope::new(52, 1);
        assert_eq!(scope.slot(52, 2), Some(1));
        assert_eq!(scope.slot(53, 2), None);
        assert_eq!(scope.slot(52, 1), None);
        assert_eq!(LaneJobScope::new(52, 0).slot(52, 0), None);
    }

    #[test]
    fn job_references_reject_uuid_and_path_handles() {
        for id in [
            "018f82d4-6f59-7d06-92e3-bce913116411",
            "../request.json",
            "",
            "not-a-key",
        ] {
            let body = serde_json::json!({"scope":{"topology":"0","lane":"0"},"id":id});
            assert!(crate::decode::<LaneJobRef>(&serde_json::to_vec(&body).unwrap()).is_err());
        }
    }
}
