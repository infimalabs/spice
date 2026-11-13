//! Steering fact bodies whose lifecycle is implemented outside the core body seam.
//!
//! A dead letter keeps its stable identity here so recovery never needs to
//! manufacture a replacement directive.

use serde::{Deserialize, Serialize};

use crate::CanonicalUuid;

/// A live directive was parked because its vacant lane could not start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteerDeadLettered {
    /// Durable directive identity retained for requeue.
    pub directive: CanonicalUuid,
    /// Parked directive key reported to the operator.
    pub key: String,
    /// Lane that could not be started for delivery.
    pub lane: CanonicalUuid,
    /// Closed lifecycle failure that prevented delivery.
    pub reason: String,
    /// Exact command that restores this directive's standing.
    pub requeue: String,
}
