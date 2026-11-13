//! Retained-work warnings are owed atomically with a lane's team departure.

use board::team_policy::{Resolver, Task};
use journal::{PendingFact, TaskHeads};
use wire::{Phase, TaskHandle, TaskStamp, Topic};

use super::*;
use crate::board_command::drive::{PolicyCapture, teams};

mod delivery;
pub(super) use delivery::reconcile;

#[derive(Clone, Debug, Eq, PartialEq)]
struct Held {
    task: TaskStamp,
    topic: Option<Topic>,
    phase: Phase,
    creator: Option<CanonicalUuid>,
    statement: Seq,
    workflow: Seq,
    claim: Seq,
}

impl Held {
    fn of(heads: &TaskHeads, actor: &Actor) -> Option<Self> {
        let claim = heads.active_claim_of(actor)?;
        let statement = heads.statement().body();
        Some(Self {
            task: statement.born.clone(),
            topic: statement.topic.path.clone(),
            phase: heads.workflow().body().phase,
            creator: statement.creation.creator_team(),
            statement: heads.statement().position(),
            workflow: heads.workflow().position(),
            claim: claim.claim().position(),
        })
    }
}

/// Called before publishing the new residency head. An admission refusal leaves
/// both topology and notice unchanged; delivery needs no callback from this door.
pub(super) fn append(
    bus: &Bus,
    worktree: &Path,
    write: &mut JournalWrite,
    before: &ResidencyHead,
    after: &ResidencyHead,
    cause: PendingFact,
) -> CommandResult<()> {
    let mut departures = after
        .lanes
        .iter()
        .filter_map(|row| {
            before
                .lanes
                .iter()
                .find(|old| old.lane == row.lane && old.team != row.team)
                .map(|old| (old.team, row))
        })
        .peekable();
    if departures.peek().is_none() {
        return Ok(());
    }
    let snapshot = bus
        .task_snapshot()
        .map_err(|error| store_fault("team::departure-snapshot", &error.to_string()))?;
    let residency = snapshot.residency().ok_or_else(|| {
        store_fault(
            "team::departure-residency",
            "the membership observation has no residency head",
        )
    })?;
    if residency.body() != before {
        return Err(Outcome::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new("resident::head-moved", Some("team".into()), None, None)
                .with_detail("read current membership before changing teams"),
        )));
    }
    super::shutdown::guard_residency(write, residency);
    let evaluated = teams(after);
    let mut policy = None;
    for (from, row) in departures {
        let actor = Actor::Lane(row.lane);
        let held = snapshot
            .tasks()
            .iter()
            .find_map(|heads| Held::of(heads, &actor));
        if let Some(held) = &held {
            if policy.is_none() {
                let repository = Repository::discover(worktree)
                    .map_err(|error| store_fault("team::departure-worktree", &error.to_string()))?;
                policy = Some(PolicyCapture::capture(&repository).map_err(typed_outcome)?);
            }
            let resolver = Resolver::new(
                policy.as_ref().expect("held work captured policy").config(),
                &evaluated,
            );
            if !resolver.drive(
                row.team,
                Task {
                    topic: held.topic.as_ref(),
                    phase: &held.phase,
                    creator: held.creator,
                },
            ) {
                write
                    .fact(
                        wire::body::TeamDepartureNoticed {
                            lane: row.lane,
                            from,
                            team: row.team,
                            tasks: vec![TaskHandle::for_topic(held.topic.as_ref(), &held.task)],
                        },
                        FactCause::Pending(cause),
                    )
                    .map_err(|error| {
                        store_fault("team::departure-notice-build", &error.to_string())
                    })?;
            }
        }
        // This also witnesses absence: a claim acquired after our read cannot
        // silently miss the departure warning. Topic rendering is compared too,
        // because a subtree move need not change the task's statement revision.
        write.guard(JournalGuard::new(move |read| {
            let current = read.active_claim_for_actor(&actor)?;
            if current.as_ref().and_then(|heads| Held::of(heads, &actor)) != held {
                return Err(GuardError::refused("team::departure-held-work-moved"));
            }
            Ok(())
        }));
    }
    if let Some(policy) = policy {
        write.guard(JournalGuard::new(move |_| policy.require_current()));
    }
    Ok(())
}
