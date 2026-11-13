//! Intent proposals for seating, drain, wake, removal, and restart commitment.

use super::restart_claim::{append_restored_claim, guard_restored_claim, observe_restored_claim};
use super::*;

/// Propose the lane, capability, and first occupant one seat admits.
pub(super) fn propose_seat(
    command: &wire::AdmittedCommand,
    request: &LaneSeat,
    observed: &journal::JournalHead<wire::ResidencyHead>,
    epoch: CanonicalUuid,
    capability: CanonicalUuid,
    answer: LaneChangeAnswer,
) -> CommandResult<JournalWrite> {
    let mut next = observed.body().clone();
    next.topology_revision = answer.topology_revision;
    if subscription_row(observed.body(), request.team)?.is_none() {
        if observed
            .body()
            .lanes
            .iter()
            .any(|row| row.team == request.team)
        {
            return Err(store_fault(
                "team::subscription-missing",
                "an existing team has no durable runtime subscription",
            ));
        }
        next.subscriptions.push(wire::TeamSubscription {
            team: request.team,
            formed_at: None,
            drive: true,
            drain: true,
            name: None,
            fast_mode: false,
        });
        next.subscriptions
            .sort_by_key(|subscription| subscription.team);
    }
    next.lanes.push(wire::LaneResidency {
        lane: request.lane,
        team: request.team,
        name: request.name.clone(),
        worktree: request.worktree.clone(),
        life: request.life.clone(),
        vendor: request.vendor.clone(),
        model: request.model.clone(),
        effort: request.effort.clone(),
        fast_mode: false,
        occupant: None,
        lineage: Vec::new(),
        capabilities: vec![capability],
        session: None,
        disposition: VACANT.to_owned(),
    });
    let mut write = write(
        command.actor().clone(),
        epoch,
        Producer::ResidentRecognition,
    )?;
    team::reconcile_formations(observed.body(), &mut next, write.at());
    guard_residency(&mut write, observed);
    let seated = write
        .fact(
            wire::body::LaneSeated {
                lane: request.lane,
                team: request.team,
                capability,
                name: request.name.clone(),
                worktree: request.worktree.clone(),
                life: request.life.clone(),
                vendor: request.vendor.clone(),
                model: request.model.clone(),
                effort: request.effort.clone(),
            },
            FactCause::Existing(observed.position()),
        )
        .map_err(|error| store_fault("lane::fact-invalid", &error.to_string()))?;
    write.residency_head(seated, move |_| next);
    command_result(&mut write, command, request.clone(), epoch, answer)?;
    Ok(write)
}

/// Propose the vacancy and closure one drained lane records.
///
/// A drain closes no context: the resident may have terminated the occupant
/// mid-turn, so only the occupant's own accounted turn stops and compactions
/// write checkpoints.
pub(super) fn propose_drain(
    command: &wire::AdmittedCommand,
    request: &LaneState,
    observed: &journal::JournalHead<wire::ResidencyHead>,
    epoch: CanonicalUuid,
    occupant: CanonicalUuid,
    evidence: StopEvidence,
    answer: LaneChangeAnswer,
) -> CommandResult<JournalWrite> {
    let mut next = observed.body().clone();
    let row = next
        .lanes
        .iter_mut()
        .find(|row| row.lane == request.lane)
        .expect("observed lane remains in its cloned head");
    row.occupant = None;
    row.disposition = evidence.disposition().to_owned();
    let mut write = write(
        command.actor().clone(),
        epoch,
        Producer::ResidentRecognition,
    )?;
    guard_residency(&mut write, observed);
    let fact = ending_fact(
        &mut write,
        request.lane,
        occupant,
        &evidence,
        FactCause::Existing(observed.position()),
    )?;
    let fact = super::renewal::advance_exit(&mut write, &mut next, request.lane, occupant, fact)?;
    write.residency_head(fact, move |_| next);
    command_result(&mut write, command, request.clone(), epoch, answer)?;
    Ok(write)
}

/// Propose the occupant, carried claim, and residency one woken lane records.
///
/// A start is also where a pending replacement and any predecessor claim are
/// taken: the successor records land in the same commit that seats the
/// occupant, so no lane is ever durably renewed without one running and no
/// seated successor observes predecessor-bound task authority.
pub(super) fn propose_wake(
    command: &wire::AdmittedCommand,
    request: &LaneState,
    observed: &journal::JournalHead<wire::ResidencyHead>,
    proven: &ProvenOccupant,
    claim: Option<&super::automatic::HeldClaimAuthority>,
    renewal: Option<&wire::LaneRenewalRecord>,
    answer: LaneChangeAnswer,
) -> CommandResult<JournalWrite> {
    let epoch = observed.body().epoch;
    let occupant = proven.occupant;
    let session = &proven.session;
    let mut next = observed.body().clone();
    let row = next
        .lanes
        .iter_mut()
        .find(|row| row.lane == request.lane)
        .expect("observed lane remains in its cloned head");
    row.occupant = Some(occupant);
    row.lineage.push(occupant);
    super::retune::seat_session(row, session.clone());
    row.disposition = OCCUPIED.to_owned();
    let mut write = write(
        command.actor().clone(),
        epoch,
        Producer::ResidentRecognition,
    )?;
    guard_residency(&mut write, observed);
    if let Some(claim) = claim {
        super::automatic::guard_held_claim(&mut write, claim);
    }
    let declared = write
        .fact(
            session_fact(session, proven.source),
            FactCause::Existing(observed.position()),
        )
        .map_err(|error| store_fault("lane::fact-invalid", &error.to_string()))?;
    let started = proven.started(
        &mut write,
        request.lane,
        epoch,
        declared,
        "lane::fact-invalid",
    )?;
    let started = match claim {
        Some(claim) => {
            let at = current_instant()?;
            let carried = super::automatic::carry_held_claim(
                &mut write,
                claim,
                request.lane,
                occupant,
                epoch,
                started,
            )?;
            super::automatic::promote_woken_lease(&mut write, claim, epoch, at, carried)?
        }
        None => started,
    };
    let started = match renewal {
        Some(renewal) => {
            super::renewal::append_successor(&mut write, &mut next, renewal, session, started)?
        }
        None => started,
    };
    write.residency_head(started, move |_| next);
    command_result(&mut write, command, request.clone(), epoch, answer)?;
    Ok(write)
}

/// Propose removing one vacant lane from the resident topology.
pub(super) fn propose_remove(
    bus: &Bus,
    paths: &RuntimePaths,
    command: &wire::AdmittedCommand,
    request: &LaneState,
    observed: &journal::JournalHead<wire::ResidencyHead>,
    row: &wire::LaneResidency,
    answer: LaneChangeAnswer,
) -> CommandResult<JournalWrite> {
    let mut next = observed.body().clone();
    next.topology_revision = answer.topology_revision;
    team::remove_member(&mut next, request.lane)?;
    let mut write = write(
        command.actor().clone(),
        observed.body().epoch,
        Producer::ResidentRecognition,
    )?;
    team::reconcile_formations(observed.body(), &mut next, write.at());
    guard_residency(&mut write, observed);
    let fact = write
        .fact(
            wire::body::LaneRemoved {
                lane: row.lane,
                team: row.team,
                worktree: row.worktree.clone(),
            },
            FactCause::Existing(observed.position()),
        )
        .map_err(|error| store_fault("lane::remove-fact-invalid", &error.to_string()))?;
    crate::resident::team_departure::append(
        bus,
        &paths.worktree,
        &mut write,
        observed.body(),
        &next,
        fact,
    )?;
    write.residency_head(fact, move |_| next);
    command_result(
        &mut write,
        command,
        request.clone(),
        observed.body().epoch,
        answer,
    )?;
    Ok(write)
}

/// Carry only epoch-bound claim authority when the same detached supervisor
/// and occupant remain live across resident replacement.
///
/// The startup transaction already retained the lane row while advancing the
/// residency epoch. No lane-start or session fact is owed here: neither the
/// process nor its vendor session restarted. An active claim is the sole piece
/// of authority that is resident-generation-bound, so it alone is rewritten.
pub(super) fn commit_adopted(
    bus: &Bus,
    epoch: CanonicalUuid,
    durable: &wire::LaneResidency,
) -> CommandResult<()> {
    let Some(occupant) = durable.occupant else {
        return Err(store_fault(
            "lane::restart-adoption-vacant",
            "a detached supervisor cannot be adopted into a vacant lane",
        ));
    };
    let at = current_instant()?;
    let Some(carried) = observe_restored_claim(bus, durable, at)? else {
        return Ok(());
    };
    if carried.is_bound_to(occupant, epoch) || carried.expired_at(at) {
        return Ok(());
    }
    let observed = current_residency(bus, epoch)?;
    let mut write = write(Actor::Spice, epoch, Producer::ResidentRecognition)?;
    guard_residency(&mut write, &observed);
    guard_restored_claim(&mut write, &carried);
    append_restored_claim(
        &mut write,
        &carried,
        durable,
        occupant,
        epoch,
        FactCause::Existing(observed.position()),
    )?;
    commit(bus, write).map(|_| ())
}

#[cfg(test)]
#[path = "propose_tests.rs"]
pub(in crate::resident::lane_surface) mod tests;
