//! Opportunistic storage maintenance at the supervisor's capture handoff.
//!
//! The resident reserves one launch per lane. Its off-loop establishment waits
//! for a terminal predecessor's supervisor to exit before spawning a successor.
//! Shutdown publishes the real completion before maintenance; its successor
//! therefore waits outside both launch horizons, not inside an agent deadline.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

use journal::JournalReader;
use sill::{Acquisition, Availability, LockFile, LockName, Possession};
use tape::Tape;
use wire::{Actor, CanonicalUuid};

use super::{
    Phase, Request, RuntimePaths, StateEvents, process_exists, process_exists_raw, read_state,
    supervisor_directory,
};

/// A minimum for archival work, never an active-capture rotation trigger.
pub(crate) const MINIMUM: u64 = 16 * 1024 * 1024;

const MAINTENANCE_LOCK: &str = ".maintenance.lock";

/// Why historical conversion may or may not touch one lane now.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum HistoricalAuthority {
    Eligible,
    Removed,
    Active,
    Claimed,
    JobHeld,
    Unknown(String),
}

/// Exclusive ownership of one lane's launch and archival boundary.
pub(crate) struct Maintenance {
    _possession: Possession,
}

/// Nonblocking acquisition of the lane's launch and archival boundary.
pub(crate) enum MaintenanceAttempt {
    Acquired(Maintenance),
    Contended,
}

/// Take the lane-wide boundary that excludes both a new launch and archival.
pub(crate) fn acquire_maintenance(
    tape_root: &Path,
    lane: CanonicalUuid,
) -> io::Result<MaintenanceAttempt> {
    let directory = spice::repository::lane_tape(tape_root, lane);
    fs::create_dir_all(&directory)?;
    let lock = maintenance_lock(&directory)?;
    match lock.try_acquire()? {
        Acquisition::Acquired(possession) => Ok(MaintenanceAttempt::Acquired(Maintenance {
            _possession: possession,
        })),
        Acquisition::Contended => Ok(MaintenanceAttempt::Contended),
    }
}

/// Observe the lane-wide boundary without creating lock or directory state.
pub(crate) fn maintenance_contended(tape_root: &Path, lane: CanonicalUuid) -> io::Result<bool> {
    Ok(
        maintenance_lock(&spice::repository::lane_tape(tape_root, lane))?.availability()?
            == Availability::Held,
    )
}

fn maintenance_lock(lane_root: &Path) -> io::Result<LockFile> {
    let name = LockName::new("tape-maintenance")
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    Ok(LockFile::new(name, lane_root.join(MAINTENANCE_LOCK)))
}

/// Revalidate that historical tape storage belongs to a fully quiescent lane.
pub(crate) fn historical_authority(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
) -> io::Result<HistoricalAuthority> {
    let reader = JournalReader::open(&paths.journal).map_err(io::Error::other)?;
    let authority = reader
        .read_with(|read| -> io::Result<_> {
            let Some(head) = read.residency_head().map_err(io::Error::other)? else {
                return Ok(Err("the journal has no residency head".to_owned()));
            };
            let row = head.body().lanes.iter().find(|row| row.lane == lane);
            let claimed = read
                .active_claim_for_actor(&Actor::Lane(lane))
                .map_err(io::Error::other)?
                .is_some();
            Ok(Ok((
                row.is_some(),
                row.and_then(|row| row.occupant),
                row.map_or_else(Vec::new, |row| row.lineage.clone()),
                claimed,
            )))
        })
        .map_err(io::Error::other)?;
    let (admitted, occupant, lineage, claimed) = match authority {
        Ok(authority) => authority,
        Err(detail) => return Ok(HistoricalAuthority::Unknown(detail)),
    };
    if occupant.is_some() {
        return Ok(HistoricalAuthority::Active);
    }
    if claimed {
        return Ok(HistoricalAuthority::Claimed);
    }
    if !super::terminal::Jobs::for_lane(paths, lane).quiescent()? {
        return Ok(HistoricalAuthority::JobHeld);
    }
    // Removing a lane is journal authority that its supervisor already
    // completed and its capture drained. An absent lane cannot launch again;
    // a later lane has a fresh identity. Retained supervisor documents are
    // evidence only, so their older schemas never become executable branches.
    if !admitted {
        return Ok(HistoricalAuthority::Removed);
    }
    let mut supervisors = lineage.into_iter().collect::<BTreeSet<_>>();
    match supervisor_occupants(paths, lane) {
        Ok(found) => supervisors.extend(found),
        Err(error) => {
            return Ok(HistoricalAuthority::Unknown(format!(
                "supervisor inventory is unavailable: {error}"
            )));
        }
    }
    for occupant in supervisors {
        let path = supervisor_directory(paths, lane, occupant).join("state.json");
        let state = match read_state(&path) {
            Ok(state) => state,
            Err(error) => {
                return Ok(HistoricalAuthority::Unknown(format!(
                    "supervisor state {} is unavailable: {error}",
                    path.display()
                )));
            }
        };
        if state.lane != lane || state.occupant != occupant {
            return Ok(HistoricalAuthority::Unknown(format!(
                "supervisor state {} names another occupant",
                path.display()
            )));
        }
        if process_exists_raw(state.supervisor)? {
            return Ok(HistoricalAuthority::Active);
        }
        if state.phase != Phase::Exited || !state.capture_drained {
            return Ok(HistoricalAuthority::Unknown(format!(
                "supervisor state {} does not prove drained completion",
                path.display()
            )));
        }
    }
    Ok(HistoricalAuthority::Eligible)
}

fn supervisor_occupants(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
) -> io::Result<Vec<CanonicalUuid>> {
    const MAX: usize = 1_024;
    let root = paths.lane_occupants(lane);
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut occupants = Vec::new();
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "supervisor directory name is not UTF-8",
            ));
        };
        let occupant = name.parse::<CanonicalUuid>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("supervisor directory {name:?} is not a canonical occupant"),
            )
        })?;
        if !kind.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("supervisor entry {name:?} is not a real directory"),
            ));
        }
        if occupants.len() == MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("supervisor inventory exceeds {MAX} occupants"),
            ));
        }
        occupants.push(occupant);
    }
    occupants.sort();
    Ok(occupants)
}

#[derive(Clone, Copy)]
enum Boundary {
    Startup,
    Shutdown,
}

struct Authority {
    previous: Option<CanonicalUuid>,
    eligible: bool,
}

/// Before any successor process or launch clock exists, finish its handoff.
/// Unreadable predecessor state cannot authorize optional maintenance.
pub(super) fn startup(paths: &RuntimePaths, request: &Request) -> io::Result<()> {
    let authority = match authority(paths, request, Boundary::Startup) {
        Ok(Some(authority)) => authority,
        Ok(None) => return Ok(()),
        Err(error) => {
            diagnostic(&error);
            return Ok(());
        }
    };
    let Some(previous) = authority.previous else {
        return Ok(());
    };
    let path = supervisor_directory(paths, request.lane, previous).join("state.json");
    let state = match read_state(&path) {
        Ok(state) => state,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            diagnostic(&error);
            return Ok(());
        }
    };
    if state.lane != request.lane || state.occupant != previous || state.vendor != request.vendor {
        return Ok(());
    }
    if state.phase != Phase::Exited {
        return Ok(());
    }
    if process_exists(state.supervisor).map_err(super::outcome_error)? {
        StateEvents::arm(&path, state.supervisor)?.wait_for_exit()?;
    }
    if !state.capture_drained {
        return Ok(());
    }
    // Waiting may have admitted a claim or changed the lane's ownership.
    match ready(paths, request, Boundary::Startup) {
        Ok(Some(tape)) => compress(&tape),
        Ok(None) => {}
        Err(error) => diagnostic(&error),
    }
    Ok(())
}

/// Called only after local child reaping, complete capture, and driver closure.
pub(super) fn shutdown(paths: &RuntimePaths, request: &Request) -> Option<Tape> {
    match ready(paths, request, Boundary::Shutdown) {
        Ok(tape) => tape,
        Err(error) => {
            diagnostic(&error);
            None
        }
    }
}

fn authority(
    paths: &RuntimePaths,
    request: &Request,
    boundary: Boundary,
) -> io::Result<Option<Authority>> {
    let reader = JournalReader::open(&paths.journal).map_err(io::Error::other)?;
    reader
        .read_with(|read| -> io::Result<Option<Authority>> {
            let Some(head) = read.residency_head().map_err(io::Error::other)? else {
                return Ok(None);
            };
            let head = head.body();
            let Some(row) = head.lanes.iter().find(|row| {
                row.lane == request.lane
                    && row.vendor.as_str() == request.vendor
                    && row.worktree == request.worktree
            }) else {
                return Ok(None);
            };
            let owned = match boundary {
                Boundary::Startup => row.occupant.is_none(),
                Boundary::Shutdown => row.occupant == Some(request.occupant),
            };
            let claimed = read
                .active_claim_for_actor(&Actor::Lane(request.lane))
                .map_err(io::Error::other)?
                .is_some();
            Ok(Some(Authority {
                previous: row.lineage.last().copied(),
                eligible: owned && head.epoch == request.epoch && !claimed,
            }))
        })
        .map_err(io::Error::other)
}

fn ready(paths: &RuntimePaths, request: &Request, boundary: Boundary) -> io::Result<Option<Tape>> {
    if !authority(paths, request, boundary)?.is_some_and(|authority| authority.eligible)
        || !super::terminal::Jobs::for_lane(paths, request.lane).quiescent()?
    {
        return Ok(None);
    }
    let root = paths.lane_tape(request.lane).join(&request.vendor);
    let tape = match Tape::open_existing(root) {
        Ok(tape) => tape,
        Err(tape::Error::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(io::Error::other(error)),
    };
    let eligible = tape.plaintext_bytes().map_err(io::Error::other)? >= MINIMUM;
    Ok(eligible.then_some(tape))
}

/// Storage owns verification, publication, recovery, and exclusive writer access.
/// A refusal changes neither the already-published agent result nor its bytes.
pub(super) fn compress(tape: &Tape) {
    match tape.seal() {
        Ok(tape::Seal::Sealed {
            member,
            plaintext,
            compressed,
        }) => {
            eprintln!(
                "spice supervisor: archived tape member {member}: {plaintext} -> {compressed} bytes"
            );
        }
        Ok(tape::Seal::Empty) => {}
        Err(error) => diagnostic(&error),
    }
}

fn diagnostic(error: &impl std::fmt::Display) {
    eprintln!("spice supervisor: tape archival skipped: {error}");
}

#[cfg(test)]
mod tests;
