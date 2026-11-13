//! The steering control that asks a Design's holder to park it.

use std::fmt;
use std::str::FromStr;

use super::RoomAnchor;
use crate::SequenceCode;

/// Steering control carried by the one operator direction that asks the lane
/// holding a deferred Design to checkpoint and free its claim.
///
/// It names the Design and the exact claim win it applies to, so a repeated
/// deferral that leaves the same holder on the same win sends nothing new,
/// while a new win after a free or steal is asked afresh.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopControl {
    /// Design whose claim is asked to stop.
    pub room: RoomAnchor,
    /// Position of the claim win the request applies to.
    pub claim: SequenceCode,
}

impl StopControl {
    /// Text every stop control begins with; a room's stops share
    /// `PREFIX:ROOM:`.
    pub const PREFIX: &'static str = "room-stop";
}

const STOP_CONTROL: &str = "room-stop:";

/// Text that does not spell a stop control.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("not a stop control: {0:?}")]
pub struct StopControlError(String);

impl fmt::Display for StopControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{STOP_CONTROL}{}:{}", self.room, self.claim)
    }
}

impl FromStr for StopControl {
    type Err = StopControlError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || StopControlError(text.to_owned());
        let (room, claim) = text
            .strip_prefix(STOP_CONTROL)
            .and_then(|rest| rest.split_once(':'))
            .ok_or_else(invalid)?;
        Ok(Self {
            room: room.parse().map_err(|_| invalid())?,
            claim: claim.parse().map_err(|_| invalid())?,
        })
    }
}
