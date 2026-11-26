//! Seat sweep for endings whose supervisor notice never arrived.
//!
//! A detached supervisor reports its agent ending with one unbound datagram,
//! sent once and never confirmed. When that datagram is lost the durable row
//! stays occupied: every wake for the lane is refused as already occupied,
//! the app keeps showing a running occupant, and only a resident restart
//! would sweep the supervisor state and record the exit. This sweep reads
//! that same state for every seated lane on each reconciliation, so the
//! running resident accounts the ending itself and the wakes the lane was
//! owed fire from the reconciliation that follows. A notice that is merely
//! waiting at the door is not lost: the sweep asks the door first and leaves
//! that seat to the notice arm, which accounts the ending in arrival order.
//!
//! The closing transcript sweep lives here too: whichever path closes a seat,
//! what its occupant wrote last is presented from the same loop turn.

use std::collections::BTreeSet;

use super::last_word::LastWord;
use super::*;

type Vacancy = (CanonicalUuid, CanonicalUuid);
type BegunVacancies<B> = Option<(B, Vec<Vacancy>)>;

/// Account one occupant exit against residency, steering, and lifecycle holds.
///
/// The supervisor notice and the seat sweep both land here, so an exit is
/// accounted the same way whichever of them reports it first. The seat is
/// vacated and the exit journaled first, which also retires the direction
/// the occupant answered in its final transcript, and the death is judged
/// against what that left behind: an exit whose transcript ends on a vendor
/// failure holds the lane on that word alone; one that ends on the
/// occupant's own answer with no direction left pending is a mature death
/// however short its life; any other exit is one more death for the
/// young-death run. An exit whose journaling is refused counts nothing, so
/// the sweep that accounts it again counts it once.
pub(in crate::resident) fn account_exit(
    bus: &Bus,
    paths: &RuntimePaths,
    (config, epoch, joining): (
        &steer::Config,
        CanonicalUuid,
        crate::board_command::Joining<'_>,
    ),
    seats: &mut Seats,
    lifecycle: &mut lifecycle::Runtime,
    exit: lane::capture::CaptureExit,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    let renewal = lane_surface::renewal_requested_exit(bus, epoch, exit.lane, exit.occupant)?;
    let seated = seats
        .started(exit.lane)
        .filter(|activity| activity.occupant() == exit.occupant);
    let word = if seated.is_some() {
        super::last_word::exit_word(bus, paths, epoch, exit.lane, exit.occupant, diagnostic)?
    } else {
        LastWord::Silent
    };
    lane_surface::occupant_exited(
        bus,
        paths,
        config,
        (epoch, joining),
        seats,
        exit,
        diagnostic,
    )?;
    if renewal {
        return Ok(());
    }
    lifecycle.observe_exit(exit.lane);
    match word {
        LastWord::Failure(failure) => {
            lifecycle.observe_vendor_failure(bus, epoch, exit, &failure, diagnostic)
        }
        LastWord::Answered | LastWord::Silent => {
            let settled = word == LastWord::Answered && !direction_pending(bus, exit.lane)?;
            lifecycle.observe_death(bus, epoch, seated, exit.lane, settled, diagnostic)
        }
    }
}

/// Whether the lane still owes an answer to live direction.
///
/// An occupant that answered but left the direction it was woken for live
/// hands the same wake to its successor: the lane learns nothing between
/// the two, and only the young-death run stands between it and a wake for
/// every exit until the direction lapses.
fn direction_pending(bus: &Bus, lane: CanonicalUuid) -> CommandResult<bool> {
    bus.read(move |read| {
        read.pending_steering(
            Some(&wire::Actor::Lane(lane)),
            journal::FactPageStart::After(None),
        )
    })
    .map(|page| !page.rows().is_empty())
    .map_err(|error| store_fault("steer::index-unreadable", &error.to_string()))
}

/// Account one occupant exit its supervisor's `failure` caused, the way the
/// failure's own notice would have.
pub(super) fn account_failure(
    bus: &Bus,
    epoch: CanonicalUuid,
    seats: &mut Seats,
    lifecycle: &mut lifecycle::Runtime,
    exit: lane::capture::CaptureExit,
    failure: &str,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    let lane::capture::CaptureExit { lane, occupant } = exit;
    let _ = writeln!(
        diagnostic,
        "lane {lane} occupant {occupant} supervisor failed: {failure}"
    );
    if !lane_surface::renewal_requested_exit(bus, epoch, lane, occupant)? {
        let seated = seats
            .started(lane)
            .filter(|activity| activity.occupant() == occupant);
        lifecycle.observe_death(bus, epoch, seated, lane, false, diagnostic)?;
    }
    if seats
        .occupant(lane)
        .is_some_and(|sitting| sitting.occupant() == occupant)
    {
        let _detached = seats.vacate(lane);
    }
    lane_surface::supervisor_failed(bus, epoch, lane, occupant, failure)
}

/// Account every seated occupant whose supervisor published an ending and
/// whose notice of it is not waiting at the door.
pub(super) fn reap_ended_seats(
    command_surface: &command_surface::Surface<'_>,
    seats: &mut Seats,
    lifecycle: &mut lifecycle::Runtime,
    door: &mut capture::SupervisorReports,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    let noticed = door.noticed_endings();
    sweep_seats(
        command_surface.bus(),
        (
            &command_surface.config.steering,
            command_surface.epoch(),
            command_surface.joining(),
        ),
        command_surface.paths,
        seats,
        lifecycle,
        &noticed,
        diagnostic,
    )
}

/// Judge every seat against its supervisor state and account the ended ones.
///
/// A seat whose ending is among the `noticed` ones is left alone: its notice
/// is at the door, and the notice arm accounts it after the records before
/// it. Each other seat is judged from its own durable state, so one
/// unreadable record leaves the other seats accounted and is reported for
/// the next sweep to read again. A refused commit defers that seat the way a
/// late notice is deferred: the state on disk does not change, so the next
/// reconciliation reaches the same judgement.
pub(in crate::resident) fn sweep_seats(
    bus: &Bus,
    (config, epoch, joining): (
        &steer::Config,
        CanonicalUuid,
        crate::board_command::Joining<'_>,
    ),
    paths: &RuntimePaths,
    seats: &mut Seats,
    lifecycle: &mut lifecycle::Runtime,
    noticed: &BTreeSet<(CanonicalUuid, CanonicalUuid)>,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    for lane in seats.lanes() {
        let Some(occupant) = seats.occupant(lane).map(lane::occupant::Occupant::occupant) else {
            continue;
        };
        if noticed.contains(&(lane, occupant)) {
            continue;
        }
        let ending = match supervisor::ending::published_ending(paths, lane, occupant) {
            Ok(Some(ending)) => ending,
            Ok(None) => continue,
            Err(outcome) => {
                let _ = writeln!(
                    diagnostic,
                    "lane {lane} occupant {occupant} supervisor state unreadable; seat kept: {outcome:?}"
                );
                continue;
            }
        };
        let _ = writeln!(
            diagnostic,
            "lane {lane} occupant {occupant} ended without a supervisor notice; accounting from its published state"
        );
        let accounted = match ending {
            supervisor::ending::PublishedEnding::Exited => account_exit(
                bus,
                paths,
                (config, epoch, joining),
                seats,
                lifecycle,
                lane::capture::CaptureExit { lane, occupant },
                diagnostic,
            ),
            supervisor::ending::PublishedEnding::Failed(failure) => account_failure(
                bus,
                epoch,
                seats,
                lifecycle,
                lane::capture::CaptureExit { lane, occupant },
                &failure,
                diagnostic,
            ),
        };
        super::deferral::defer_store_fault(accounted, diagnostic, "reaped seat accounting")?;
    }
    Ok(())
}

/// Begin one closing batch without taking ownership of vacancy work early.
///
/// Acquiring the message batch can fail before any tape is presentable. The
/// seat ledger remains the retry authority until that acquisition succeeds.
pub(in crate::resident) fn begin_vacated_batch<B>(
    seats: &mut Seats,
    begin: impl FnOnce() -> CommandResult<B>,
) -> CommandResult<BegunVacancies<B>> {
    if !seats.has_vacated() {
        return Ok(None);
    }
    let batch = begin()?;
    let mut vacated = seats.take_vacated();
    vacated.sort_unstable();
    Ok(Some((batch, vacated)))
}

/// Attempt every drained vacancy and return the first terminal outcome.
///
/// Store faults remain deferred housekeeping diagnostics. Protocol,
/// environment, and refusal outcomes cannot strand later occupant bindings:
/// each attempt owns its binding cleanup and all attempts run before the first
/// terminal outcome is returned.
pub(in crate::resident) fn attempt_vacated(
    vacated: Vec<Vacancy>,
    diagnostic: &mut dyn io::Write,
    mut attempt: impl FnMut(CanonicalUuid, CanonicalUuid, &mut dyn io::Write) -> CommandResult<()>,
) -> CommandResult<()> {
    let mut first_terminal = None;
    for (lane, occupant) in vacated {
        let result = attempt(lane, occupant, diagnostic);
        if let Err(outcome) =
            super::deferral::defer_store_fault(result, diagnostic, "closing transcript sweep")
            && first_terminal.is_none()
        {
            first_terminal = Some(outcome);
        }
    }
    first_terminal.map_or(Ok(()), Err)
}

/// Present what every seat vacated this turn left on its tape.
///
/// Every path that closes a seat, a supervisor exit or failure notice, the
/// seat sweep for a lost ending, an operator stop, records the occupant in the
/// seat ledger. The closing sweep runs here, after the turn, where the taste,
/// steering, and app surfaces all exist. Batch acquisition precedes the
/// deterministic take from the seat ledger; after that take every entry is
/// attempted before the first terminal outcome is returned.
pub(in crate::resident) fn present_closed_transcripts(
    seats: &mut Seats,
    command_surface: &command_surface::Surface<'_>,
    message_publisher: &serve::stream::MessagePublisher,
    mut taste_judge: Option<&mut taste_surface::AdjudicationSubmitter>,
    message_state: &std::sync::Mutex<message_surface::MessageState>,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    let (bus, epoch) = (command_surface.bus(), command_surface.epoch());
    let begun = begin_vacated_batch(seats, || {
        message_publisher
            .batch()
            .map_err(|error| store_fault("serve::message-publish-failed", &error.to_string()))
    });
    let Some((generation, vacated)) = (match begun {
        Ok(begun) => begun,
        Err(outcome) => {
            return super::deferral::defer_store_fault(
                Err(outcome),
                diagnostic,
                "closing transcript batch",
            );
        }
    }) else {
        return Ok(());
    };
    let presented = attempt_vacated(vacated, diagnostic, |lane, occupant, diagnostic| {
        let surfaces = CaptureSurfaces::new(
            command_surface.paths,
            &command_surface.config.taste,
            &command_surface.config.steering,
            &generation,
            command_surface.joining(),
        )
        .without_narration();
        let recognition = taste_judge.as_deref_mut().map_or(
            message_surface::TasteRecognition::Direct,
            message_surface::TasteRecognition::Deferred,
        );
        message_surface::present_closed_transcript(
            bus,
            epoch,
            &surfaces,
            recognition,
            lane,
            occupant,
            message_surface::CaptureViews::new(message_state, diagnostic),
        )
    });
    let committed = generation
        .commit()
        .map(drop)
        .map_err(|error| store_fault("serve::message-publish-failed", &error.to_string()));
    let committed =
        super::deferral::defer_store_fault(committed, diagnostic, "closing transcript batch");
    match (presented, committed) {
        (Err(first), Err(later)) => {
            let _ = writeln!(
                diagnostic,
                "closing transcript batch failed after an earlier terminal outcome: {later:?}"
            );
            Err(first)
        }
        (Err(first), Ok(())) => Err(first),
        (Ok(()), committed) => committed,
    }
}
