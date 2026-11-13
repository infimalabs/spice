//! Consent fact bodies extracted from the core body registry.
//!
//! These types retain their public `wire::body::*` paths through the parent
//! module while keeping the core body registry below its structural ceiling.

use serde::{Deserialize, Serialize};

use crate::CanonicalUuid;

/// An ACP permission request arrived, bound to its cause.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentAsked {
    /// Identity of the permission request.
    pub request: CanonicalUuid,
    /// Lane the request came from.
    pub lane: CanonicalUuid,
    /// Operation permission is asked for.
    pub operation: String,
}

/// A named rule or operator allowed the request; any budget effect is carried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentGranted {
    /// Allowed request.
    pub request: CanonicalUuid,
    /// Lane the request came from.
    pub lane: CanonicalUuid,
    /// Rule or operator that allowed it.
    pub rule: String,
    /// Budget remaining after the grant; absent when the rule carries no budget.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub budget: Option<u32>,
}

/// A named rule or operator denied the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentDenied {
    /// Denied request.
    pub request: CanonicalUuid,
    /// Lane the request came from.
    pub lane: CanonicalUuid,
    /// Rule or operator that denied it.
    pub rule: String,
}

/// Why a consent request pended to the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ConsentQueueCause {
    /// The deciding rule or posture explicitly requires an operator answer.
    Policy {},
    /// The deciding rule spent its last admitted grant and requires renewal.
    BudgetExhausted {
        /// Grants the named rule admits between renewals.
        bound: u32,
    },
}

/// The request pended to the operator while reversible work could continue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentQueued {
    /// Pending request.
    pub request: CanonicalUuid,
    /// Lane the request came from.
    pub lane: CanonicalUuid,
    /// Rule or posture that queued it.
    pub rule: String,
    /// Whether policy asked directly or a named budget requires renewal.
    pub cause: ConsentQueueCause,
}

/// The lane reached an explicit break before anyone ruled on the request.
///
/// A renewal seats a fresh successor and a removal ends the seat, so no
/// occupant remains that could act on a ruling. An exit is continuity, not a
/// break: the occupant that resumes the session inherits the wait.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentLapsed {
    /// Lapsed request.
    pub request: CanonicalUuid,
    /// Lane the request came from.
    pub lane: CanonicalUuid,
}
