//! Consent requests a lane's occupant raises and the answers that reach it.
//!
//! An omp occupant that opens a confirm dialog holds its turn until someone
//! answers. Its supervisor states the question as one consent request, and
//! the resident records the ask together with what policy makes of it: a
//! grant or a denial answers the dialog at once, while a queue leaves the
//! request to the operator, whose ruling answers it later. Either answer
//! crosses the lane's kick door, and an occupant that has since left took
//! its dialog with it, exactly as it takes a missed inbox kick.

use std::io::Write;

use bus::Bus;
use journal::FactCause;
use wire::body::{ConsentAsked, ConsentLapsed};
use wire::{Actor, CanonicalUuid, Outcome, Producer, ResidencyHead};

use super::read_materializations::CurrentReads;
use super::supervisor::command;
use super::{CommandResult, RuntimePaths, commit, lane_origin, store_fault, write_at_origin};

/// One confirm dialog a seated occupant opened, as its supervisor stated it.
pub(super) struct LaneAsk {
    pub(super) lane: CanonicalUuid,
    pub(super) occupant: CanonicalUuid,
    pub(super) request: CanonicalUuid,
    pub(super) operation: String,
}

/// Record one lane's ask with its policy outcome and answer what is answered.
///
/// A refusal is written down rather than raised: the dialog then waits on
/// nothing durable, and the occupant's turn holds until it exits, which is
/// the same standing an unanswered prompt always had.
///
/// # Errors
///
/// Returns the fault of a journal that could not be read or written.
pub(super) fn ask(
    bus: &Bus,
    reads: &CurrentReads,
    paths: &RuntimePaths,
    policy: &consent::Policy,
    residency: &ResidencyHead,
    ask: LaneAsk,
    diagnostic: &mut dyn Write,
) -> CommandResult<()> {
    let seated = residency
        .lanes
        .iter()
        .any(|row| row.lane == ask.lane && row.occupant == Some(ask.occupant));
    if !seated {
        let _ = writeln!(
            diagnostic,
            "lane {} consent request {} from occupant {} after its seat closed",
            ask.lane, ask.request, ask.occupant
        );
        return Ok(());
    }
    let (lane, request) = (ask.lane, ask.request);
    match decide(bus, reads, policy, ask) {
        Ok(Some(confirmed)) => command::answer(paths, lane, request, confirmed),
        Ok(None) => {}
        Err(Outcome::Refusal(refusal)) => {
            let _ = writeln!(
                diagnostic,
                "lane {lane} consent request {request} refused: {}: {}",
                refusal.reason(),
                refusal.explanation()
            );
        }
        Err(other) => return Err(other),
    }
    Ok(())
}

/// Commit the ask and its outcome; the answer, when policy already gave one.
fn decide(
    bus: &Bus,
    reads: &CurrentReads,
    policy: &consent::Policy,
    ask: LaneAsk,
) -> CommandResult<Option<bool>> {
    let generation = reads
        .current()
        .map_err(|error| store_fault("consent::materialization-unavailable", &error))?;
    let asked = ConsentAsked {
        request: ask.request,
        lane: ask.lane,
        operation: ask.operation,
    };
    let outcome = generation
        .consent()
        .decide(policy, &asked)
        .map_err(Outcome::Refusal)?;
    let mut proposal = write_at_origin(
        Actor::Lane(ask.lane),
        lane_origin(ask.lane)?,
        Producer::CommandDoor,
    )?;
    proposal.guard(super::policy_surface::consent_guard(generation.cursor()));
    let asked = proposal
        .fact(asked, FactCause::Root)
        .map_err(|error| store_fault("consent::fact-invalid", &error.to_string()))?;
    let cause = FactCause::Pending(asked);
    let recorded = match &outcome {
        consent::Outcome::Granted(granted) => proposal.fact(granted.clone(), cause),
        consent::Outcome::Denied(denied) => proposal.fact(denied.clone(), cause),
        consent::Outcome::Queued(queued) => proposal.fact(queued.clone(), cause),
    };
    recorded.map_err(|error| store_fault("consent::fact-invalid", &error.to_string()))?;
    commit(bus, proposal)?;
    Ok(match outcome {
        consent::Outcome::Granted(_) => Some(true),
        consent::Outcome::Denied(_) => Some(false),
        consent::Outcome::Queued(_) => None,
    })
}

/// Lapse every request the lane still has pending, in one commit.
///
/// Only an explicit break lapses a request: a renewal seats a fresh
/// successor and a removal ends the seat, so nothing remains that could act
/// on a ruling. An exit is continuity, and the occupant that resumes the
/// session inherits the wait. The lapse is the resident's recognition, yet
/// it closes the request's own thread, so it is written where the request
/// was asked, guarded on the consent cursor the pending set was read at.
///
/// # Errors
///
/// Returns the fault of a journal that could not be read or written, or the
/// refusal of a consent state that moved while the lapse was proposed.
pub(super) fn commit_lapse(
    bus: &Bus,
    reads: &CurrentReads,
    lane: CanonicalUuid,
) -> CommandResult<()> {
    let generation = reads
        .current()
        .map_err(|error| store_fault("consent::materialization-unavailable", &error))?;
    let mut pending = generation.consent().pending_of(lane).peekable();
    if pending.peek().is_none() {
        return Ok(());
    }
    let mut proposal = write_at_origin(
        Actor::Spice,
        lane_origin(lane)?,
        Producer::ResidentRecognition,
    )?;
    proposal.guard(super::policy_surface::consent_guard(generation.cursor()));
    for request in pending {
        let lapsed = ConsentLapsed {
            request: request.request(),
            lane,
        };
        proposal
            .fact(lapsed, FactCause::Existing(request.fact()))
            .map_err(|error| store_fault("consent::fact-invalid", &error.to_string()))?;
    }
    commit(bus, proposal)?;
    Ok(())
}
