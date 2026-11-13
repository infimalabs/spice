//! Claim authority transferred while a resident generation restores its seats.

use super::*;

#[derive(Clone)]
pub(super) struct RestoredClaim {
    task: wire::TaskStamp,
    claim: journal::JournalHead<wire::ClaimHead>,
    lease: journal::JournalHead<wire::LeaseHead>,
    source: wire::body::SourceBoundary,
}

impl RestoredClaim {
    pub(super) fn is_bound_to(&self, occupant: CanonicalUuid, epoch: CanonicalUuid) -> bool {
        self.claim.body().occupant == occupant
            && self.claim.body().epoch == epoch
            && self.lease.body().epoch == epoch
    }

    pub(super) fn expired_at(&self, at: wire::Instant) -> bool {
        at >= self.lease.body().expiry
    }
}

/// Carry live lane claims in the same transaction that advances residency.
///
/// No resident renews a lease while none runs, so downtime is never held
/// against a holder: a claim still active when its lane is restored, even one
/// whose lease ran out while the resident was down, leaves startup with at
/// least one rolling window from now. A holder that does not return lapses
/// once that window passes unrenewed.
pub(in crate::resident) fn append_startup_claims(
    bus: &Bus,
    lanes: &[wire::LaneResidency],
    epoch: CanonicalUuid,
    write: &mut JournalWrite,
    mut cause: PendingFact,
) -> CommandResult<PendingFact> {
    let at = current_instant()?;
    for durable in lanes {
        let Some(occupant) = durable.occupant else {
            continue;
        };
        let Some(restored) = observe_restored_claim(bus, durable, at)? else {
            continue;
        };
        guard_restored_claim(write, &restored);
        cause = append_startup_claim(
            write,
            &restored,
            durable,
            occupant,
            epoch,
            at,
            FactCause::Pending(cause),
        )?;
    }
    Ok(cause)
}

/// The restored lease, rolling at least one window past `at`.
fn restarted_lease(
    observed: &wire::LeaseHead,
    at: wire::Instant,
) -> CommandResult<wire::LeaseHead> {
    let window = at
        .get()
        .checked_add(jiff::SignedDuration::from_secs(
            crate::board_command::AUTOMATIC_ROLLING_SECONDS,
        ))
        .map_err(|error| store_fault("lane::restart-lease-invalid", &error.to_string()))?;
    let window = wire::Instant::new(window)
        .map_err(|error| store_fault("lane::restart-lease-invalid", &error.to_string()))?;
    let mut lease = observed.clone();
    if window > lease.expiry {
        lease.renewal = std::cmp::max(lease.renewal, at);
        lease.expiry = window;
        lease.maximum = std::cmp::max(lease.maximum, window);
    }
    Ok(lease)
}

fn append_startup_claim(
    write: &mut JournalWrite,
    restored: &RestoredClaim,
    durable: &wire::LaneResidency,
    occupant: CanonicalUuid,
    epoch: CanonicalUuid,
    at: wire::Instant,
    cause: FactCause,
) -> CommandResult<PendingFact> {
    let ancestor = durable
        .occupant
        .expect("restored claims require a predecessor occupant");
    let mut lease = restarted_lease(restored.lease.body(), at)?;
    lease.epoch = epoch;
    let rebound = write
        .fact(
            wire::body::ClaimRebound {
                task: restored.task.clone(),
                lane: durable.lane,
                occupant,
                ancestor,
                epoch,
                until: lease.expiry,
                maximum: lease.maximum,
                source: restored.source.clone(),
            },
            cause,
        )
        .map_err(|error| store_fault("lane::restart-claim-rebound-invalid", &error.to_string()))?;
    let mut claim = restored.claim.body().clone();
    claim.occupant = occupant;
    claim.epoch = epoch;
    write.claim_head(restored.task.clone(), rebound, move |position| {
        claim.fact = position;
        claim
    });
    write.lease_head(restored.task.clone(), rebound, move |_| lease);
    Ok(rebound)
}

/// The active claim `durable`'s lane holds, as long as its lease has started
/// by `at`; an expired lease is still returned for startup to restore.
pub(super) fn observe_restored_claim(
    bus: &Bus,
    durable: &wire::LaneResidency,
    at: wire::Instant,
) -> CommandResult<Option<RestoredClaim>> {
    let Some(ancestor) = durable.occupant else {
        return Ok(None);
    };
    let snapshot = bus
        .task_snapshot()
        .map_err(|error| store_fault("lane::restart-claim-heads-unreadable", &error.to_string()))?;
    let mut restored = None;
    let mut found = false;
    for (heads, held) in snapshot
        .tasks()
        .iter()
        .flat_map(|heads| heads.claims().iter().map(move |held| (heads, held)))
    {
        let claim = held.claim();
        if claim.body().disposition != wire::ClaimDisposition::Active {
            continue;
        }
        if claim.body().lane != durable.lane && claim.body().actor != Actor::Lane(durable.lane) {
            continue;
        }
        if found {
            return Err(store_fault(
                "lane::restart-claim-ambiguous",
                "one restored lane owns more than one active claim",
            ));
        }
        found = true;
        let actor_bound = matches!(claim.body().actor, Actor::Principal(_))
            || claim.body().actor == Actor::Lane(durable.lane);
        if !actor_bound || claim.body().lane != durable.lane || claim.body().occupant != ancestor {
            return Err(store_fault(
                "lane::restart-claim-lineage-moved",
                "active claim ownership differs from the restored predecessor lineage",
            ));
        }
        let lease = held.lease().ok_or_else(|| {
            store_fault(
                "lane::restart-claim-lease-missing",
                "an active restored claim has no durable lease head",
            )
        })?;
        if lease.body().epoch != claim.body().epoch {
            return Err(store_fault(
                "lane::restart-claim-epoch-mismatch",
                "restored claim and lease generations disagree",
            ));
        }
        if at < lease.body().start {
            continue;
        }
        restored = Some(RestoredClaim {
            task: heads.statement().body().born.clone(),
            claim: claim.clone(),
            lease: lease.clone(),
            source: bus.source_boundary(&durable.lane).map_err(|error| {
                store_fault(
                    "lane::restart-claim-boundary-unreadable",
                    &error.to_string(),
                )
            })?,
        });
    }
    Ok(restored)
}

pub(super) fn guard_restored_claim(write: &mut JournalWrite, restored: &RestoredClaim) {
    let task = restored.task.clone();
    let claim = restored.claim.clone();
    let lease = restored.lease.clone();
    write.guard(JournalGuard::new(move |read| {
        let claimed = claim.body().claimed;
        if read.claim_head(&task, claimed)? != Some(claim.clone()) {
            return Err(journal::GuardError::refused(
                "lane::restart-claim-head-moved",
            ));
        }
        if read.lease_head(&task, claimed)? != Some(lease.clone()) {
            return Err(journal::GuardError::refused(
                "lane::restart-lease-head-moved",
            ));
        }
        Ok(())
    }));
}

pub(super) fn append_restored_claim(
    write: &mut JournalWrite,
    restored: &RestoredClaim,
    durable: &wire::LaneResidency,
    occupant: CanonicalUuid,
    epoch: CanonicalUuid,
    cause: FactCause,
) -> CommandResult<PendingFact> {
    let ancestor = durable
        .occupant
        .expect("restored claims require a predecessor occupant");
    let carried = write
        .fact(
            wire::body::ClaimCarried {
                task: restored.task.clone(),
                lane: durable.lane,
                occupant,
                ancestor,
                epoch,
                source: restored.source.clone(),
            },
            cause,
        )
        .map_err(|error| store_fault("lane::restart-claim-carry-invalid", &error.to_string()))?;
    let mut claim = restored.claim.body().clone();
    claim.occupant = occupant;
    claim.epoch = epoch;
    write.claim_head(restored.task.clone(), carried, move |position| {
        claim.fact = position;
        claim
    });
    let mut lease = restored.lease.body().clone();
    lease.epoch = epoch;
    write.lease_head(restored.task.clone(), carried, move |_| lease);
    Ok(carried)
}
