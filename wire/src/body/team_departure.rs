use serde::{Deserialize, Serialize};

use crate::{CanonicalUuid, TaskHandle};

/// A team departure owes its lane one warning about retained work.
///
/// Recorded with the membership transition, before asynchronous delivery. The
/// task handles describe the placements tested against the new team's policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamDepartureNoticed {
    /// Lane retaining the claims.
    pub lane: CanonicalUuid,
    /// Team the lane left.
    pub from: CanonicalUuid,
    /// Team whose drive admission refused the retained work.
    pub team: CanonicalUuid,
    /// Held tasks the new team cannot drive; none is moved or released.
    pub tasks: Vec<TaskHandle>,
}
