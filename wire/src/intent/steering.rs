//! Principal line-control command and answer DTOs.

use serde::{Deserialize, Serialize};

/// One principal's control over an exact admitted input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteerLineControl {
    /// Exact native steering input the principal received.
    pub input: crate::rooms::PrincipalInputRef,
    /// Header action applied to the input's conversation line.
    pub action: SteerLineAction,
    /// Optional authored body returned on that line.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub body: Option<String>,
}

/// Principal line-control header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SteerLineAction {
    /// Acknowledge the input, optionally continuing its line.
    #[serde(rename = "ACK")]
    Ack,
    /// Decline the input and close its line.
    #[serde(rename = "NAK")]
    Nak,
    /// Acknowledge the input and finish its line.
    #[serde(rename = "FIN")]
    Fin,
}

/// Result of one principal line-control command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteerLineAnswer {
    /// Input the command settled.
    pub input: crate::rooms::PrincipalInputRef,
    /// Recorded line transition and any body delivery judgment.
    pub effect: crate::outbox::LineEffect,
    /// Fate of body text outside a line transition, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<crate::outbox::BodyFate>,
}
