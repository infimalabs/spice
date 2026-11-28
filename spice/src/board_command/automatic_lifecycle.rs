//! Resident-owned claims for automatic lifecycle wakes.

use super::*;

/// Claim authority minted by the resident before it opens one vacant lane.
///
/// The occupant identity is chosen before the claim, so a crash after this
/// transaction and before process launch leaves an ordinary held claim for the
/// lifecycle authority to recover. No provisional claim vocabulary or second
/// writer exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AutomaticClaim {
    pub(crate) task: TaskStamp,
    pub(crate) lane: wire::CanonicalUuid,
    pub(crate) occupant: wire::CanonicalUuid,
}

/// The vacant seat one automatic claim is minted for.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AutomaticSeat {
    pub(crate) epoch: wire::CanonicalUuid,
    pub(crate) lane: wire::CanonicalUuid,
    pub(crate) occupant: wire::CanonicalUuid,
}

/// Select and claim once for a vacant resident-owned lane.
///
/// This is the automatic counterpart of `claim_win`: the same validated
/// snapshot, selector, allocator guard, and mint writer decide the result.
pub(crate) fn claim_automatic(
    bus: &Bus,
    observed: &bus::TaskSnapshot,
    repository: Host<'_>,
    seat: AutomaticSeat,
    allocation: &board::NextConfig,
) -> Result<AutomaticClaim, Outcome<CommandAnswer>> {
    let PreparedClaim {
        write,
        selected,
        entries,
        task,
        born,
        actor,
        git,
        ownership,
    } = prepare_claim(bus, observed, repository, seat, allocation)?;
    let answer = automatic_answer(
        &selected,
        &entries,
        task,
        actor,
        git.transplant(),
        ownership,
    );
    match committed_with_task_git(
        bus,
        write,
        CommandAnswer::Claim(Box::new(answer)),
        PreparedTaskGit::Claim(git),
    ) {
        Outcome::Reply(_) => Ok(AutomaticClaim {
            task: born,
            lane: seat.lane,
            occupant: seat.occupant,
        }),
        Outcome::Refusal(refusal) => Err(Outcome::Refusal(refusal)),
        Outcome::Fault(fault) => Err(Outcome::Fault(fault)),
    }
}

struct PreparedClaim<'a> {
    write: JournalWrite,
    selected: Entry,
    entries: Vec<Entry>,
    task: TaskHandle,
    born: TaskStamp,
    actor: Actor,
    git: spice::task_git::ClaimPreparation<'a>,
    ownership: wire::intent::TaskOwnership,
}

fn prepare_claim<'a>(
    bus: &Bus,
    observed: &bus::TaskSnapshot,
    repository: Host<'a>,
    seat: AutomaticSeat,
    allocation: &board::NextConfig,
) -> Result<PreparedClaim<'a>, Outcome<CommandAnswer>> {
    let AutomaticSeat { lane, occupant, .. } = seat;
    let actor = Actor::Lane(lane);
    let snapshot = validated_snapshot_for_automatic(
        observed,
        Some(repository.repository),
        actor.clone(),
        occupant,
    )?;
    let row = available_lane(&snapshot, lane)?;
    let team = available_subscription(&snapshot, row)?;
    let subscription = snapshot.owners.for_team(team.team);
    let selected = select_task(&snapshot, &actor, &subscription, allocation)?;
    let task = selected.handle().clone();
    let born = selected.born().clone();
    let (at, expiry, maximum) = claim_deadlines()?;
    let origin = task_origin(&born)?;
    let mut write = JournalWrite::new(actor.clone(), origin, at, Producer::CommandDoor);
    guard_allocation(
        &mut write,
        Vec::new(),
        AllocationObservation {
            cursor: snapshot.cursor,
            actor: actor.clone(),
            residency_position: snapshot.answer.residency.position,
        },
    );
    let git = stage_automatic(
        &mut write,
        bus,
        repository,
        &snapshot,
        (row, seat),
        &selected,
        (expiry, maximum),
        None,
    )?;
    Ok(PreparedClaim {
        ownership: snapshot.ownership(&selected),
        write,
        selected,
        entries: snapshot.entries,
        task,
        born,
        actor,
        git,
    })
}

/// Stage one named automatic claim for a vacant `seat` on `selected`.
///
/// The lane's worktree inherits the task's parked work exactly as an
/// allocator claim does, so a release followed by a wake loses nothing. The
/// write's own instant starts the provisional lease, which is what marks the
/// claim automatic.
#[expect(
    clippy::too_many_arguments,
    reason = "one staged claim names its write, seat, task, lease and cause"
)]
pub(in crate::board_command) fn stage_automatic<'a>(
    write: &mut JournalWrite,
    bus: &Bus,
    repository: Host<'a>,
    snapshot: &ValidatedSnapshot,
    (row, seat): (&wire::LaneResidency, AutomaticSeat),
    selected: &Entry,
    (expiry, maximum): (Instant, Instant),
    cause: Option<PendingFact>,
) -> Result<spice::task_git::ClaimPreparation<'a>, Outcome<CommandAnswer>> {
    snapshot.guard_ownership(write);
    let actor = Actor::Lane(seat.lane);
    let source = claim_source_boundary(bus, &seat.lane)?;
    let lane_repository = lane_repository(repository, &row.worktree)?;
    let git = spice::task_git::prepare_task_claim(
        &lane_repository,
        repository.ambient,
        task_parked_commits(snapshot, selected.born()),
    )
    .map_err(task_git_outcome)?;
    let at = write.at();
    if let Err(outcome) = mint_claim(
        write,
        board::allocate::Mint {
            task: selected.born(),
            actor: &actor,
            workflow: selected.workflow(),
            topic: selected.topic(),
            lane: seat.lane,
            occupant: seat.occupant,
            epoch: seat.epoch,
            at,
            expiry,
            maximum,
            git: Some(git.boundary.clone()),
            recovery: None,
            source,
            cause,
        },
    ) {
        return Err(match git.rollback() {
            Ok(()) => outcome,
            Err(error) => task_git_outcome(error),
        });
    }
    Ok(git)
}

fn available_lane(
    snapshot: &ValidatedSnapshot,
    lane: wire::CanonicalUuid,
) -> Result<&wire::LaneResidency, Outcome<CommandAnswer>> {
    let row = snapshot
        .answer
        .residency
        .body
        .lanes
        .iter()
        .find(|row| row.lane == lane)
        .ok_or_else(|| {
            refusal(
                "lifecycle::available-work-lane-missing",
                Some(lane.to_string()),
                "reconcile against one current vacant lane",
            )
        })?;
    if row.occupant.is_some() || row.disposition == "occupied" {
        return Err(refusal(
            "lifecycle::available-work-capacity-moved",
            Some(row.disposition.clone()),
            "reconcile again after the current occupant is observed",
        ));
    }
    Ok(row)
}

/// Require a current drain-enabled team before automatic acquisition.
fn available_subscription(
    snapshot: &ValidatedSnapshot,
    row: &wire::LaneResidency,
) -> Result<wire::TeamSubscription, Outcome<CommandAnswer>> {
    let residency = &snapshot.answer.residency.body;
    let mut subscriptions = residency
        .subscriptions
        .iter()
        .filter(|subscription| subscription.team == row.team);
    let subscription = subscriptions.next().ok_or_else(|| {
        store_fault(
            "lifecycle::available-work-subscription-missing",
            "the vacant lane lineage has no durable task-allocation lifetime",
        )
    })?;
    if subscriptions.next().is_some() {
        return Err(store_fault(
            "lifecycle::available-work-subscription-ambiguous",
            "the vacant lane lineage has more than one durable task-allocation lifetime",
        ));
    }
    let subscription = subscription.clone();
    if !subscription.drain {
        return Err(refusal(
            "lifecycle::available-work-scope",
            Some("drain=off".to_owned()),
            "turn team drain on for capacity-following wake",
        ));
    }
    Ok(subscription)
}

fn select_task(
    snapshot: &ValidatedSnapshot,
    actor: &Actor,
    subscription: &board::Allocation,
    allocation: &board::NextConfig,
) -> Result<Entry, Outcome<CommandAnswer>> {
    let environment = snapshot.answer.environment();
    let now = current_instant()?;
    board::select_at(
        &snapshot.entries,
        actor,
        subscription,
        *allocation,
        &environment,
        now,
    )
    .map(|selection| selection.entry().clone())
    .ok_or_else(|| {
        refusal(
            "lifecycle::available-work-empty",
            None,
            "wait for a durable eligible task transition",
        )
    })
}

/// The lane's own repository, refused unless it is still the resident's worktree.
/// A refusal parks only this lane; a fault would end the resident for every lane.
fn lane_repository(
    repository: Host<'_>,
    worktree: &str,
) -> Result<Repository, Outcome<CommandAnswer>> {
    let lane_repository = Repository::discover(worktree)
        .map_err(|error| environment_fault("git::worktree-unresolved", error.to_string()))?;
    if lane_repository.common_dir() != repository.common_dir()
        || lane_repository.worktree_root() != std::path::Path::new(worktree)
    {
        return Err(refusal(
            "git::worktree-moved",
            Some(worktree.to_owned()),
            "the allocating lane no longer names one worktree in the resident repository",
        ));
    }
    Ok(lane_repository)
}

pub(in crate::board_command) fn claim_deadlines()
-> Result<(Instant, Instant, Instant), Outcome<CommandAnswer>> {
    let at = current_instant()?;
    let expiry = instant_after(at, AUTOMATIC_PROVISIONAL_SECONDS)?;
    let maximum = instant_after(at, AUTOMATIC_MAXIMUM_SECONDS)?;
    Ok((at, expiry, maximum))
}

pub(crate) fn is_automatic_claim(claim: &ClaimHead, facts: &bus::TaskFacts) -> bool {
    facts
        .at(claim.claimed)
        .is_some_and(|envelope| spice::claim_lease::is_automatic_claim_fact(claim, envelope))
}

fn automatic_answer(
    selected: &Entry,
    entries: &[Entry],
    task: TaskHandle,
    actor: Actor,
    transplant: Option<&wire::intent::ClaimTransplant>,
    ownership: wire::intent::TaskOwnership,
) -> wire::intent::ClaimAnswer {
    wire::intent::ClaimAnswer {
        ownership: Some(ownership),
        task,
        statement: None,
        holder: Some(actor),
        acknowledged: None,
        held: false,
        warnings: board::claim_overlaps(selected, entries)
            .into_iter()
            .map(|overlap| wire::intent::ClaimWarning {
                task: overlap.task,
                lane: overlap.lane,
                surface: overlap.surface,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        creation_context: None,
        origin_context: None,
        room_context: None,
        next: None,
        phase: None,
        nothing_stolen: false,
        transplant: transplant.cloned(),
        parked: None,
    }
}

/// Release exactly the automatic claim whose launch did not become durable.
///
/// The claim may already have replayed parked work into the lane, so the
/// release parks the lane exactly as an ordinary task free does and the next
/// claim replays it again. A slot the claim's own invitation grew leaves with
/// it in the same write, as any released participant's grown slot does.
pub(crate) fn release_automatic(
    bus: &Bus,
    repository: Host<'_>,
    automatic: &AutomaticClaim,
) -> Result<(), Outcome<CommandAnswer>> {
    let ActiveClaim {
        observed,
        actor,
        handle,
        worktree,
        opening,
        retraction,
        ownership,
        authority,
    } = active_claim(bus, repository.repository, automatic)?;
    let at = current_instant()?;
    let origin = task_origin(&automatic.task)?;
    let lane_repository = lane_repository(repository, &worktree)?;
    let parked =
        spice::task_git::prepare_free(&lane_repository, repository.ambient, opening.as_ref())
            .map_err(task_git_outcome)?;
    let mut write = JournalWrite::new(actor.clone(), origin, at, Producer::CommandDoor);
    authority.guard_ownership(&mut write);
    guard(
        &mut write,
        vec![Witness::Claim {
            task: automatic.task.clone(),
            claimed: Some(observed.body().claimed),
            head: Some(observed.position()),
        }],
    );
    let freed = match write.fact(
        body::ClaimFreed {
            task: automatic.task.clone(),
            actor: actor.clone(),
            parked: parked.as_ref().map(|release| release.parked().clone()),
        },
        FactCause::Root,
    ) {
        Ok(freed) => freed,
        Err(error) => {
            return Err(rollback_release(
                parked,
                store_fault("lifecycle::claim-release-build", error.to_string()),
            ));
        }
    };
    let mut next = observed.body().clone();
    next.disposition = ClaimDisposition::Freed;
    let task = automatic.task.clone();
    write.claim_head(task, freed, move |fact| ClaimHead { fact, ..next });
    if let Some(retraction) = retraction
        && let Err(error) = retraction.append(&mut write, FactCause::Pending(freed))
    {
        return Err(rollback_release(
            parked,
            store_fault("lifecycle::claim-release-target", error.to_string()),
        ));
    }
    let answer = wire::intent::ClaimAnswer {
        parked: parked_answer(&handle, parked.as_ref()),
        ownership: Some(ownership),
        task: handle,
        statement: None,
        holder: None,
        acknowledged: None,
        held: false,
        warnings: Box::default(),
        creation_context: None,
        origin_context: None,
        room_context: None,
        next: None,
        phase: None,
        nothing_stolen: false,
        transplant: None,
    };
    let body = CommandAnswer::Claim(Box::new(answer));
    let outcome = match parked {
        Some(parked) => committed_with_task_git(bus, write, body, PreparedTaskGit::Release(parked)),
        None => committed(bus, write, body),
    };
    match outcome {
        Outcome::Reply(_) => Ok(()),
        Outcome::Refusal(refusal) => Err(Outcome::Refusal(refusal)),
        Outcome::Fault(fault) => Err(Outcome::Fault(fault)),
    }
}

/// The still-active automatic claim, the lane boundary it opened, and the
/// grown slot its release withdraws.
struct ActiveClaim {
    observed: journal::JournalHead<ClaimHead>,
    actor: Actor,
    handle: TaskHandle,
    worktree: String,
    opening: Option<GitBoundary>,
    retraction: Option<board::round::Retraction>,
    ownership: wire::intent::TaskOwnership,
    authority: ValidatedSnapshot,
}

fn active_claim(
    bus: &Bus,
    repository: &Repository,
    automatic: &AutomaticClaim,
) -> Result<ActiveClaim, Outcome<CommandAnswer>> {
    let snapshot = bus
        .task_snapshot()
        .map_err(|error| store_fault("lifecycle::claim-release-read", error.to_string()))?;
    let heads = snapshot
        .tasks()
        .iter()
        .find(|heads| heads.statement().body().born == automatic.task)
        .ok_or_else(|| {
            store_fault(
                "lifecycle::claim-release-task-missing",
                "the automatic claim task disappeared before launch compensation",
            )
        })?;
    if heads.claims().is_empty() {
        return Err(store_fault(
            "lifecycle::claim-release-head-missing",
            "the automatic claim head disappeared before launch compensation",
        ));
    }
    let actor = Actor::Lane(automatic.lane);
    let authority = validated_snapshot_for_automatic(
        &snapshot,
        Some(repository),
        actor.clone(),
        automatic.occupant,
    )?;
    let creator_team = authority
        .entries
        .iter()
        .find(|entry| entry.born() == &automatic.task)
        .expect("automatic claim and ownership projection share task heads")
        .creator_team();
    let Some(observed) = heads
        .active_claim_of(&actor)
        .map(|held| held.claim().clone())
        .filter(|observed| observed.body().occupant == automatic.occupant)
    else {
        return Err(refusal(
            "lifecycle::claim-release-moved",
            Some(format!(
                "{} {} {}",
                automatic.task, automatic.lane, automatic.occupant
            )),
            "leave successor authority intact and reconcile current heads",
        ));
    };
    let claim = observed.body();
    let handle = heads
        .statement()
        .body()
        .handle(heads.workflow().body().phase);
    let worktree = snapshot
        .residency()
        .and_then(|residency| {
            residency
                .body()
                .lanes
                .iter()
                .find(|row| row.lane == automatic.lane)
        })
        .map(|row| row.worktree.clone())
        .ok_or_else(|| {
            store_fault(
                "lifecycle::claim-release-lane-missing",
                "the automatic claim lane disappeared before launch compensation",
            )
        })?;
    let opening = snapshot
        .facts()
        .at(claim.claimed)
        .and_then(|envelope| envelope.fact.read::<body::ClaimWon>())
        .and_then(|won| won.git);
    let mut bodies = heads.clone().into_bodies();
    let ending = bodies
        .claims
        .iter_mut()
        .find(|(current, _)| current.claimed == claim.claimed)
        .expect("the observed claim remains in its task heads");
    ending.0.disposition = ClaimDisposition::Freed;
    let post = Entry::new(
        bodies.statement,
        bodies.workflow,
        bodies.completion,
        bodies.claims,
    )
    .map_err(|error| {
        store_fault(
            "lifecycle::claim-release-task-invalid",
            format!("task {handle} is invalid: {error}"),
        )
    })?
    .with_creator_team(creator_team);
    let retraction = board::round::Retraction::owed(&post);
    Ok(ActiveClaim {
        ownership: authority.ownership(&post),
        authority,
        observed,
        actor,
        handle,
        worktree,
        opening,
        retraction,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_claim_starts_short_with_a_one_hour_maximum() {
        let (at, expiry, maximum) = claim_deadlines().expect("automatic deadlines build");

        assert_eq!(
            expiry,
            instant_after(at, AUTOMATIC_PROVISIONAL_SECONDS).expect("provisional deadline builds")
        );
        assert_eq!(
            maximum,
            instant_after(at, AUTOMATIC_MAXIMUM_SECONDS).expect("maximum deadline builds")
        );
        assert!(expiry < maximum);
    }
}
