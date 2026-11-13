//! Full-battery admission from current task authority.
//!
//! The selected change gate and package-scoped checks are ordinary feedback
//! surfaces. The complete battery is different: it may start only when the
//! current worktree's exact occupied lane owns one coherent, unexpired task
//! claim at the typed `review` phase. This boundary reads the journal directly and
//! never delegates authority discovery to another command or prose renderer.

use std::fmt;
use std::path::Path;

use board::Entry;
use spice::ambient::Ambient;
use spice::repository::Repository;
use wire::{Actor, CanonicalUuid, Instant, ResidencyHead};

use super::Failure;

const EXPECTED: &str =
    "one occupied current-worktree lane holding one coherent unexpired task claim at phase review";
const REPAIR: &str = "claim a task whose current workflow phase is review in this worktree, then rerun the full gate";

pub(super) fn admit_current_worktree(ambient: &Ambient) -> Result<(), Failure> {
    let current = std::env::current_dir().map_err(|error| Failure::Fault {
        reason: "gate::working-directory-unavailable",
        detail: error.to_string(),
    })?;
    let repository = Repository::discover(&current).map_err(|error| Failure::Fault {
        reason: spice::repository::ResolveError::REASON,
        detail: error.to_string(),
    })?;
    let lane = ambient_lane(ambient)?;
    let state = repository
        .admit_state()
        .map_err(|error| unavailable(error.to_string()))?;
    let reader = journal::JournalReader::open(state.journal())
        .map_err(|error| unavailable(error.to_string()))?;
    let actor = Actor::Lane(lane);
    let (residency, heads) = reader
        .read(|read| Ok((read.residency_head()?, read.active_claim_for_actor(&actor)?)))
        .map_err(|error| unavailable(error.to_string()))?;
    let residency = residency
        .map(journal::JournalHead::into_body)
        .ok_or_else(|| refused(VerifyClaimRefusal::MissingResidency))?;
    let entry = heads
        .map(journal::TaskHeads::into_bodies)
        .map(|bodies| {
            Entry::new(
                bodies.statement,
                bodies.workflow,
                bodies.completion,
                bodies.claims,
            )
        })
        .transpose()
        .map_err(|error| unavailable(error.to_string()))?;
    let at =
        Instant::new(jiff::Timestamp::now()).map_err(|error| unavailable(error.to_string()))?;

    admit_snapshot(VerifyClaimSnapshot {
        worktree: repository.worktree_root(),
        lane,
        residency: &residency,
        entry: entry.as_ref(),
        at,
    })
    .map_err(refused)
}

pub(super) fn launch_after_admission<T, E>(
    admission: Result<(), E>,
    launch: impl FnOnce() -> T,
) -> Result<T, E> {
    admission?;
    Ok(launch())
}

fn ambient_lane(ambient: &Ambient) -> Result<CanonicalUuid, Failure> {
    let Some(value) = ambient.get(wire::SpiceVariable::Lane.as_str()) else {
        return Err(refused(VerifyClaimRefusal::MissingLane));
    };
    let value = value
        .to_str()
        .ok_or_else(|| refused(VerifyClaimRefusal::InvalidLane("non-UTF-8".to_owned())))?;
    value
        .parse()
        .map_err(|_| refused(VerifyClaimRefusal::InvalidLane(value.to_owned())))
}

fn unavailable(observed: String) -> Failure {
    Failure::Refusal {
        reason: "gate::verify-claim-unavailable",
        expected: EXPECTED.to_owned(),
        detail: format!("{observed}; {REPAIR}"),
        observed,
    }
}

fn refused(error: VerifyClaimRefusal) -> Failure {
    let observed = error.to_string();
    Failure::Refusal {
        reason: "gate::verify-claim-refused",
        expected: EXPECTED.to_owned(),
        detail: format!("{observed}; {REPAIR}"),
        observed,
    }
}

struct VerifyClaimSnapshot<'a> {
    worktree: &'a Path,
    lane: CanonicalUuid,
    residency: &'a ResidencyHead,
    entry: Option<&'a Entry>,
    at: Instant,
}

fn admit_snapshot(snapshot: VerifyClaimSnapshot<'_>) -> Result<(), VerifyClaimRefusal> {
    let seat = snapshot
        .residency
        .lanes
        .iter()
        .find(|seat| seat.lane == snapshot.lane)
        .ok_or(VerifyClaimRefusal::MissingSeat(snapshot.lane))?;
    if Path::new(&seat.worktree) != snapshot.worktree {
        return Err(VerifyClaimRefusal::ForeignWorktree {
            expected: snapshot.worktree.display().to_string(),
            observed: seat.worktree.clone(),
        });
    }
    if seat.disposition != "occupied" {
        return Err(VerifyClaimRefusal::InactiveSeat(seat.disposition.clone()));
    }
    let occupant = seat.occupant.ok_or(VerifyClaimRefusal::MissingOccupant)?;
    let entry = snapshot.entry.ok_or(VerifyClaimRefusal::MissingClaim)?;
    let actor = Actor::Lane(snapshot.lane);
    let active = board::Active::admit(entry, &actor, snapshot.at)
        .map_err(VerifyClaimRefusal::InactiveClaim)?;
    if active.lane() != snapshot.lane
        || active.occupant() != occupant
        || active.epoch() != snapshot.residency.epoch
    {
        return Err(VerifyClaimRefusal::ForeignClaim);
    }
    if !entry.workflow().status.is_pending() {
        return Err(VerifyClaimRefusal::SettledTask);
    }
    if entry.workflow().phase != wire::Phase::Review {
        return Err(VerifyClaimRefusal::Phase(
            entry.workflow().phase.to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug)]
enum VerifyClaimRefusal {
    MissingLane,
    InvalidLane(String),
    MissingResidency,
    MissingSeat(CanonicalUuid),
    ForeignWorktree { expected: String, observed: String },
    InactiveSeat(String),
    MissingOccupant,
    MissingClaim,
    InactiveClaim(board::ClaimRefusal),
    ForeignClaim,
    SettledTask,
    Phase(String),
}

impl fmt::Display for VerifyClaimRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingLane => formatter.write_str("current process has no lane identity"),
            Self::InvalidLane(value) => write!(formatter, "lane identity is invalid: {value:?}"),
            Self::MissingResidency => formatter.write_str("journal has no current residency head"),
            Self::MissingSeat(lane) => write!(formatter, "lane {lane} has no current seat"),
            Self::ForeignWorktree { expected, observed } => write!(
                formatter,
                "lane worktree {observed:?} does not match current worktree {expected:?}"
            ),
            Self::InactiveSeat(disposition) => {
                write!(formatter, "current-worktree lane is {disposition}, not occupied")
            }
            Self::MissingOccupant => {
                formatter.write_str("current-worktree lane has no current occupant")
            }
            Self::MissingClaim => {
                formatter.write_str("current-worktree lane has no active task claim")
            }
            Self::InactiveClaim(error) => write!(formatter, "task claim is not live: {error}"),
            Self::ForeignClaim => formatter.write_str(
                "active task claim does not match the current lane, occupant, and resident generation",
            ),
            Self::SettledTask => formatter.write_str("active claim names a settled task"),
            Self::Phase(phase) => write!(
                formatter,
                "active task phase is {phase}, not review"
            ),
        }
    }
}

#[cfg(test)]
mod tests;
