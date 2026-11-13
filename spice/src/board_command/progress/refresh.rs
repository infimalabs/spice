//! Generation acknowledgement and same-lane residency recovery for one active claim.

use super::*;

pub(super) enum ClaimRefresh {
    Acknowledge,
    Rebound(Rebound),
}

/// The seat a same-lane recovery moves the claim onto.
pub(super) struct Rebound {
    lane: wire::CanonicalUuid,
    occupant: wire::CanonicalUuid,
    ancestor: wire::CanonicalUuid,
    epoch: wire::CanonicalUuid,
    source: body::SourceBoundary,
}

pub(super) fn refresh_entry<'a>(
    snapshot: &'a ValidatedSnapshot,
    refresh: &TaskRef,
) -> Result<&'a Entry, Outcome<CommandAnswer>> {
    task_entry(&snapshot.entries, &refresh.task).ok_or_else(|| {
        refusal(
            "task::unknown",
            Some(refresh.task.to_string()),
            "read the board and refresh one current task handle",
        )
    })
}

pub(super) fn refresh_claim(
    bus: &Bus,
    entry: &Entry,
    snapshot: &ValidatedSnapshot,
    command: &wire::AdmittedCommand,
    refresh: &TaskRef,
    at: Instant,
) -> Result<ClaimRefresh, Outcome<CommandAnswer>> {
    let held = active_claim(entry, command, at)?;
    let lease = held.lease().ok_or_else(|| {
        protocol_fault(
            "claim::lease-missing",
            "an active claim head has no durable lease head",
        )
    })?;
    refresh_residency(
        bus,
        snapshot,
        command,
        refresh,
        held.claim(),
        lease,
        entry.workflow(),
    )
}

fn active_claim<'a>(
    entry: &'a Entry,
    command: &wire::AdmittedCommand,
    at: Instant,
) -> Result<&'a board::TaskClaim, Outcome<CommandAnswer>> {
    if entry.claims().is_empty() {
        return Err(claim_authority_refusal(
            "claim::head-missing",
            Some(entry.handle().to_string()),
        ));
    }
    let Actor::Lane(_) = command.actor() else {
        return Err(refusal(
            "claim::lane-required",
            Some(command.actor().to_string()),
            "refresh through task next from the lane that owns the claim",
        ));
    };
    let Some(held) = entry.claim_of(command.actor()) else {
        return Err(claim_authority_refusal(
            "claim::ownership-refused",
            entry
                .latest_claim()
                .map(|latest| latest.claim().actor.to_string()),
        ));
    };
    board::Active::admit(entry, command.actor(), at).map_err(|error| {
        claim_authority_refusal("claim::active-refused", Some(error.to_string()))
    })?;
    Ok(held)
}

fn refresh_residency(
    bus: &Bus,
    snapshot: &ValidatedSnapshot,
    command: &wire::AdmittedCommand,
    refresh: &TaskRef,
    claim: &ClaimHead,
    lease: &LeaseHead,
    workflow: &WorkflowHead,
) -> Result<ClaimRefresh, Outcome<CommandAnswer>> {
    let Actor::Lane(lane) = command.actor() else {
        unreachable!("active claim admission requires a lane actor")
    };
    let residency = &snapshot.answer.residency.body;
    let seat = residency.lanes.iter().find(|seat| seat.lane == *lane);
    let occupant = seat
        .filter(|seat| seat.disposition == "occupied")
        .and_then(|seat| seat.occupant);
    if claim.lane != *lane || residency.epoch != command.epoch() || occupant.is_none() {
        return Err(moved_refusal(refresh));
    }
    let occupant = occupant.expect("occupied current seat has an occupant");
    if claim.epoch == command.epoch()
        && lease.epoch == command.epoch()
        && occupant == claim.occupant
    {
        if claim.is_stale(workflow) {
            return Ok(ClaimRefresh::Acknowledge);
        }
        return Err(protocol_fault(
            "claim::refresh-current",
            "task next requested a write for a current claim",
        ));
    }
    if claim.epoch != lease.epoch {
        return Err(protocol_fault(
            "claim::lease-epoch-mismatch",
            "active claim and lease generations disagree before recovery",
        ));
    }
    if !seat.is_some_and(|seat| seat.lineage.contains(&claim.occupant)) {
        return Err(moved_refusal(refresh));
    }
    let source = bus
        .source_boundary(lane)
        .map_err(|error| store_fault("claim::rebound-boundary-unreadable", error.to_string()))?;
    Ok(ClaimRefresh::Rebound(Rebound {
        lane: *lane,
        occupant,
        ancestor: claim.occupant,
        epoch: command.epoch(),
        source,
    }))
}

fn moved_refusal(refresh: &TaskRef) -> Outcome<CommandAnswer> {
    claim_authority_refusal("claim::residency-moved", Some(refresh.task.to_string()))
}

pub(super) fn append_refresh(
    write: &mut JournalWrite,
    entry: &Entry,
    actor: &Actor,
    state: ClaimRefresh,
) -> Result<(), Outcome<CommandAnswer>> {
    let held = entry
        .claim_of(actor)
        .expect("refresh admission requires a claim");
    match state {
        ClaimRefresh::Acknowledge => append_acknowledgement(write, entry, held),
        ClaimRefresh::Rebound(rebound) => append_rebound(write, entry, held, rebound),
    }
}

fn append_acknowledgement(
    write: &mut JournalWrite,
    entry: &Entry,
    held: &board::TaskClaim,
) -> Result<(), Outcome<CommandAnswer>> {
    let generation = entry.workflow().generation;
    let acknowledged = write
        .fact(
            body::ClaimAcknowledged {
                task: entry.born().clone(),
                generation,
            },
            FactCause::Root,
        )
        .map_err(|error| store_fault("claim::acknowledge-build", error.to_string()))?;
    let claim = held.claim().clone();
    write.claim_head(entry.born().clone(), acknowledged, move |fact| ClaimHead {
        fact,
        acknowledged: generation,
        ..claim
    });
    Ok(())
}

fn append_rebound(
    write: &mut JournalWrite,
    entry: &Entry,
    held: &board::TaskClaim,
    rebound: Rebound,
) -> Result<(), Outcome<CommandAnswer>> {
    let Rebound {
        lane,
        occupant,
        ancestor,
        epoch,
        source,
    } = rebound;
    let mut lease = held
        .lease()
        .expect("refresh admission requires a lease")
        .clone();
    let rebound = write
        .fact(
            body::ClaimRebound {
                task: entry.born().clone(),
                lane,
                occupant,
                ancestor,
                epoch,
                until: lease.expiry,
                maximum: lease.maximum,
                source,
            },
            FactCause::Root,
        )
        .map_err(|error| store_fault("claim::rebound-build", error.to_string()))?;
    let mut claim = held.claim().clone();
    claim.occupant = occupant;
    claim.epoch = epoch;
    claim.acknowledged = entry.workflow().generation;
    write.claim_head(entry.born().clone(), rebound, move |fact| ClaimHead {
        fact,
        ..claim
    });
    lease.epoch = epoch;
    write.lease_head(entry.born().clone(), rebound, move |_| lease);
    Ok(())
}
