//! Occupant drain, compensation, and observed-exit handling.

use super::*;

pub(super) fn drain(
    bus: &Bus,
    epoch: CanonicalUuid,
    seats: &mut Seats,
    command: &wire::AdmittedCommand,
    request: &LaneState,
) -> Outcome<CommandAnswer> {
    let observed = match current_residency(bus, epoch) {
        Ok(head) => head,
        Err(outcome) => return typed_outcome(outcome),
    };
    let topology_revision = observed.body().topology_revision;
    let Some(row) = observed
        .body()
        .lanes
        .iter()
        .find(|row| row.lane == request.lane)
    else {
        return missing_lane(request.lane);
    };
    let Some(durable_occupant) = row.occupant else {
        return lane_refusal(
            "lane::already-vacant",
            Some(OCCUPIED.to_owned()),
            Some(row.disposition.clone()),
            "wake the lane before asking it to drain again",
        );
    };
    let Some(occupant) = seats.vacate(request.lane) else {
        return store_fault(
            "lane::occupant-unowned",
            "durable lane occupant has no process group supervised by this resident",
        );
    };
    if occupant.occupant() != durable_occupant {
        drop(occupant);
        return store_fault(
            "lane::occupant-moved",
            "supervised process group does not match durable residency",
        );
    }
    let evidence = match stop_occupant(occupant) {
        Ok(evidence) => evidence,
        Err(error) => return environment_fault("lane::drain-failed", &error),
    };
    let disposition = evidence.disposition();
    let answer = LaneChangeAnswer {
        lane: request.lane,
        topology_revision,
        from: OCCUPIED.to_owned(),
        to: disposition.to_owned(),
        git: None,
        rtk: None,
    };
    let proposal = match propose_drain(
        command,
        request,
        &observed,
        epoch,
        durable_occupant,
        evidence,
        answer.clone(),
    ) {
        Ok(proposal) => proposal,
        Err(outcome) => return typed_outcome(outcome),
    };
    match commit(bus, proposal) {
        Ok(committed) => committed_lane_reply(committed, answer),
        Err(outcome) => typed_outcome(outcome),
    }
}

/// End a lane's occupant without publishing how it ended.
///
/// The dropped [`StopEvidence`] is the point rather than an oversight. Every
/// caller here is a compensation path — `await_session`, `commit_restored`,
/// `propose_wake`, or the commit itself failed — so the seating this unwinds
/// was never committed. An ending fact names the residency it closes, and there
/// is no committed residency to close: publishing one would assert a lane life
/// the journal never recorded beginning, which is a worse record than silence.
///
/// The distinction this discards is only owed where an occupancy was durable,
/// which is [`stop_occupant`]'s other caller, and that one carries the evidence
/// through to a fact.
pub(in crate::resident) fn stop_lane(seats: &mut Seats, lane: CanonicalUuid) {
    if let Some(occupant) = seats.vacate(lane) {
        let _stopped = stop_occupant(occupant);
    }
}

/// How an occupant's residency ended, as the resident itself observed it.
///
/// Each arm names a distinct ending rather than one flattened exit status.
/// [`Self::Killed`] carries no code because a signal this resident sent left the
/// occupant no status of its own to report.
pub(super) enum StopEvidence {
    Drained,
    Exited(i32),
    TimedOut(i32),
    Cancelled(i32),
    Signalled(i32),
    /// A signal this resident sent ended it, at either signalling stage.
    ///
    /// Not only the kill: an occupant that never trapped the termination signal
    /// dies from that signal instead, and this resident is equally the reason
    /// for the ending in both cases.
    Killed,
    Degraded(String),
}

impl StopEvidence {
    pub(super) const fn disposition(&self) -> &'static str {
        match self {
            Self::Drained
            | Self::Exited(_)
            | Self::TimedOut(_)
            | Self::Cancelled(_)
            | Self::Signalled(_)
            | Self::Killed => VACANT,
            Self::Degraded(_) => DEGRADED,
        }
    }
}

pub(super) fn stop_occupant(occupant: Occupant) -> Result<StopEvidence, String> {
    let occupant = match occupant
        .wait_for(Duration::ZERO)
        .map_err(|error| error.to_string())?
    {
        OccupantWait::Exited { outcome, .. } => {
            return Ok(exit_evidence(&outcome));
        }
        OccupantWait::Running(occupant) => occupant,
    };
    occupant.terminate().map_err(|error| error.to_string())?;
    let grace = occupant.termination_grace();
    match occupant
        .wait_for(grace)
        .map_err(|error| error.to_string())?
    {
        OccupantWait::Exited { outcome, .. } => Ok(stop_evidence(&outcome)),
        OccupantWait::Running(occupant) => {
            occupant.kill().map_err(|error| error.to_string())?;
            let reap = occupant.reap_bound();
            match occupant.wait_for(reap).map_err(|error| error.to_string())? {
                // The wait outcome is deliberately not read for a code here.
                // This resident sent the signal that ended it, so whatever
                // status the platform reports is a description of that signal
                // rather than anything the occupant decided.
                OccupantWait::Exited { .. } => Ok(StopEvidence::Killed),
                OccupantWait::Running(_) => {
                    Err("process group survived its configured reap bound".to_owned())
                }
            }
        }
    }
}

/// Classify an ending this resident's termination signal is responsible for.
///
/// The only caller is the wait that follows [`Occupant::terminate`], so every
/// outcome reaching here was already signalled by this resident. A code means
/// the occupant trapped the signal and chose what to report; its absence means
/// the signal is what ended it, which is the same ending the kill stage records.
/// Reporting that as an exit would publish a status the occupant never chose.
fn stop_evidence(outcome: &WaitOutcome) -> StopEvidence {
    if let Some(failure) = outcome.observation_failure() {
        return StopEvidence::Degraded(format!("capture failed: {failure}"));
    }
    let status = outcome.completion().status();
    match outcome.completion() {
        sill::Completion::TimedOut(_) => return StopEvidence::TimedOut(exit_code(outcome)),
        sill::Completion::Cancelled(_) => return StopEvidence::Cancelled(exit_code(outcome)),
        sill::Completion::Exited(_) => {}
    }
    if status.success() {
        return StopEvidence::Drained;
    }
    match status.code() {
        Some(code) => StopEvidence::Exited(code),
        None => StopEvidence::Killed,
    }
}

pub(super) fn exit_evidence(outcome: &WaitOutcome) -> StopEvidence {
    if let Some(failure) = outcome.observation_failure() {
        return StopEvidence::Degraded(format!("capture failed: {failure}"));
    }
    match outcome.completion() {
        sill::Completion::Exited(status) => match status.code() {
            Some(code) => StopEvidence::Exited(code),
            None => StopEvidence::Signalled(signal_number(outcome)),
        },
        sill::Completion::TimedOut(_) => StopEvidence::TimedOut(exit_code(outcome)),
        sill::Completion::Cancelled(_) => StopEvidence::Cancelled(exit_code(outcome)),
    }
}

pub(super) fn ending_fact(
    write: &mut JournalWrite,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    evidence: &StopEvidence,
    cause: FactCause,
) -> CommandResult<PendingFact> {
    let fact = match evidence {
        StopEvidence::Drained => write.fact(wire::body::LaneDrained { lane, occupant }, cause),
        StopEvidence::Exited(code) => write.fact(
            wire::body::LaneExited {
                lane,
                occupant,
                code: *code,
            },
            cause,
        ),
        StopEvidence::TimedOut(code) => write.fact(
            wire::body::LaneTimedOut {
                lane,
                occupant,
                code: *code,
            },
            cause,
        ),
        StopEvidence::Cancelled(code) => write.fact(
            wire::body::LaneCancelled {
                lane,
                occupant,
                code: *code,
            },
            cause,
        ),
        StopEvidence::Signalled(signal) => write.fact(
            wire::body::LaneSignalled {
                lane,
                occupant,
                signal: *signal,
            },
            cause,
        ),
        StopEvidence::Killed => write.fact(wire::body::LaneKilled { lane, occupant }, cause),
        StopEvidence::Degraded(marker) => write.fact(
            wire::body::LaneDegraded {
                lane,
                marker: format!("occupant {occupant} {marker}"),
            },
            cause,
        ),
    }
    .map_err(|error| store_fault("lane::fact-invalid", &error.to_string()))?;
    Ok(fact)
}

/// Write down an exit notice the resident did not account, and why.
///
/// A notice is refused when the durable row and the seat do not both name
/// the exiting occupant: the row moved on, the seat holds a successor, or the
/// lane is not in residency at all. Those are the only values that decided
/// the refusal, so the line carries all of them; without it a notice that
/// missed leaves an occupied row with nothing in the log to explain why.
pub(super) fn report_unaccounted_exit(
    diagnostic: &mut dyn io::Write,
    exit: CaptureExit,
    row: Option<&wire::LaneResidency>,
    seated: Option<CanonicalUuid>,
) {
    let durable = row.map_or_else(
        || "no durable row".to_owned(),
        |row| format!("durable row holds {}", named(row.occupant)),
    );
    let _ = writeln!(
        diagnostic,
        "lane {} exit of occupant {} not accounted: {durable}, seat holds {}",
        exit.lane,
        exit.occupant,
        named(seated),
    );
}

fn named(occupant: Option<CanonicalUuid>) -> String {
    occupant.map_or_else(|| "none".to_owned(), |occupant| occupant.to_string())
}

/// The status of an occupant that was already gone before this resident acted.
///
/// This is the one place a missing code still becomes a number, and it is the
/// one place that number says something true: nothing here had been signalled by
/// this resident, so a signal death is one it did not cause. Since a Unix status
/// code is 0 to 255 and never negative, the substitute cannot collide with a
/// status an occupant chose.
fn exit_code(outcome: &WaitOutcome) -> i32 {
    outcome.completion().status().code().unwrap_or(-1)
}

#[cfg(unix)]
fn signal_number(outcome: &WaitOutcome) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;

    outcome.completion().status().signal().unwrap_or(-1)
}

#[cfg(not(unix))]
fn signal_number(_outcome: &WaitOutcome) -> i32 {
    -1
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::ExitStatus;

    use super::*;

    #[test]
    fn detached_terminal_causes_remain_distinct_from_natural_exit() {
        let timed_out = WaitOutcome::externally_reaped_completion(sill::Completion::TimedOut(
            ExitStatus::from_raw(0),
        ));
        let cancelled = WaitOutcome::externally_reaped_completion(sill::Completion::Cancelled(
            ExitStatus::from_raw(7 << 8),
        ));
        let signalled = WaitOutcome::externally_reaped_completion(sill::Completion::Exited(
            ExitStatus::from_raw(libc::SIGTERM),
        ));

        assert!(matches!(
            exit_evidence(&timed_out),
            StopEvidence::TimedOut(0)
        ));
        assert!(matches!(
            exit_evidence(&cancelled),
            StopEvidence::Cancelled(7)
        ));
        assert!(matches!(
            exit_evidence(&signalled),
            StopEvidence::Signalled(libc::SIGTERM)
        ));
    }
}
