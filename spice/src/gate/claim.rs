//! The active claim of the lane this process acts for.

use wire::{Actor, CanonicalUuid, Phase, TaskHandle};

use crate::ambient::Ambient;
use crate::repository::StatePaths;

/// Why the ambient lane's claim could not be read.
#[derive(Debug)]
pub enum AmbientClaimError {
    /// The ambient lane identity is not a canonical lane UUID.
    Invalid(String),
    /// The journal could not be opened or read.
    Unavailable(String),
}

/// The task and phase the ambient `SPICE_LANE` lane holds, if any.
///
/// A process acting for no lane holds no claim.
///
/// # Errors
///
/// Returns [`AmbientClaimError::Invalid`] for a lane identity that is not one
/// canonical UUID, and [`AmbientClaimError::Unavailable`] when the journal
/// cannot answer.
pub fn ambient_claim(
    state: &StatePaths,
    ambient: &Ambient,
) -> Result<Option<(TaskHandle, Phase)>, AmbientClaimError> {
    let Some(lane) = ambient.get(wire::SpiceVariable::Lane.as_str()) else {
        return Ok(None);
    };
    let lane = lane
        .to_str()
        .ok_or_else(|| AmbientClaimError::Invalid("lane identity is not UTF-8".to_owned()))?;
    let lane: CanonicalUuid = lane
        .parse()
        .map_err(|_| AmbientClaimError::Invalid(format!("lane identity is invalid: {lane:?}")))?;
    let unavailable =
        |error: &dyn std::fmt::Display| AmbientClaimError::Unavailable(error.to_string());
    let reader =
        journal::JournalReader::open(state.journal()).map_err(|error| unavailable(&error))?;
    let heads = reader
        .read(|read| read.active_claim_for_actor(&Actor::Lane(lane)))
        .map_err(|error| unavailable(&error))?;
    Ok(heads.map(|heads| {
        let journal::TaskBodies {
            statement,
            workflow,
            ..
        } = heads.into_bodies();
        (statement.handle(workflow.phase), workflow.phase)
    }))
}
