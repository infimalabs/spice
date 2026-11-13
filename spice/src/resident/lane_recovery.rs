//! Predecessor lane import and atomic authority transfer.

use std::sync::Arc;

use bus::Bus;
use journal::{FactCause, JournalGuard, JournalWrite, PendingFact};
use lane::capture::CaptureReporter;
use lane::occupant::Seats;
use wire::intent::{CommandAnswer, LaneChangeAnswer, LaneState};
use wire::{Actor, CanonicalUuid, Outcome, Producer};

#[cfg(test)]
use super::lane_surface::await_session;
use super::lane_surface::{
    OCCUPIED, OccupantLaunchRequest, PlannedSeat, ProvenOccupant, VACANT, admit_worktree,
    command_result, committed_lane_reply, lane_refusal, resume_session, session_fact, stop_lane,
};
use super::shutdown::{current_residency, guard_residency, rebase_lane};
use super::{
    CommandResult, RuntimePaths, commit, current_instant, new_uuid, store_fault, typed_outcome,
    write,
};

pub(super) struct Runtime {
    capture: Arc<dyn CaptureReporter>,
}

impl Runtime {
    pub(super) fn new(capture: Arc<dyn CaptureReporter>) -> Self {
        Self { capture }
    }
}

struct ImportContext {
    observed: journal::JournalHead<wire::ResidencyHead>,
    row: wire::LaneResidency,
    worktree: String,
    ancestor: CanonicalUuid,
    resume: vendor::Resume,
    carried: Option<CarriedClaim>,
}

#[derive(Clone)]
struct CarriedClaim {
    task: wire::TaskStamp,
    claim: journal::JournalHead<wire::ClaimHead>,
    lease: journal::JournalHead<wire::LeaseHead>,
    /// Accounted transcript the predecessor occupant stopped spending at.
    source: wire::body::SourceBoundary,
}

pub(super) struct ImportLaunch {
    launch: super::supervisor::PendingLaunch,
    pending: PendingImport,
}

impl ImportLaunch {
    pub(super) fn lane(&self) -> CanonicalUuid {
        self.pending.request.lane
    }

    pub(super) fn into_parts(self) -> (super::supervisor::PendingLaunch, PendingImport) {
        (self.launch, self.pending)
    }
}

pub(super) struct PendingImport {
    context: ImportContext,
    request: LaneState,
    occupant: CanonicalUuid,
}

pub(super) fn begin_import(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    seats: &Seats,
    runtime: &Runtime,
    request: &LaneState,
) -> Result<ImportLaunch, Outcome<CommandAnswer>> {
    if request.action != wire::intent::LaneAction::Wake {
        return Err(lane_refusal(
            "lane::state-shape",
            Some("wake action with successor worktree".into()),
            Some(format!("{:?}", request.action).to_lowercase()),
            "use lane import through its registered CLI contract",
        ));
    }
    let context = admit_import(bus, paths, epoch, seats, request).map_err(typed_outcome)?;
    let occupant = new_uuid();
    let fast_mode =
        super::settings_surface::lane_fast_mode(bus, context.observed.body(), &context.row)
            .map_err(typed_outcome)?;
    let launch = super::supervisor::begin_launch(
        paths,
        OccupantLaunchRequest {
            worktree: &context.worktree,
            vendor: &context.row.vendor,
            epoch,
            lane: context.row.lane,
            occupant,
            resume: context.resume.clone(),
            fast_mode,
            model: context.row.model.as_deref(),
            effort: context.row.effort.as_deref(),
        },
        Arc::clone(&runtime.capture),
    )
    .map_err(typed_outcome)?;
    Ok(ImportLaunch {
        launch,
        pending: PendingImport {
            context,
            request: request.clone(),
            occupant,
        },
    })
}

pub(super) fn finish_import(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    seats: &mut Seats,
    command: &wire::AdmittedCommand,
    mut pending: PendingImport,
    launched: CommandResult<super::supervisor::Launch>,
) -> Outcome<CommandAnswer> {
    let fresh_session = matches!(pending.context.resume, vendor::Resume::Fresh);
    let proven = match ProvenOccupant::seat(
        bus,
        paths,
        seats,
        PlannedSeat {
            lane: pending.context.row.lane,
            occupant: pending.occupant,
            fresh_session,
        },
        launched,
        |detail| store_fault("lane::import-launch-failed", detail),
    ) {
        Ok(proven) => proven,
        Err(outcome) => return typed_outcome(outcome),
    };
    pending.context.observed =
        match rebase_lane(bus, epoch, &pending.context.observed, pending.request.lane) {
            Ok(observed) => observed,
            Err(outcome) => {
                stop_lane(seats, pending.request.lane);
                return typed_outcome(outcome);
            }
        };
    let answer = LaneChangeAnswer {
        lane: pending.request.lane,
        topology_revision: pending.context.observed.body().topology_revision,
        from: pending.context.row.worktree.clone(),
        to: pending.context.worktree.clone(),
        git: Some(proven.git.clone().into_boxed_str()),
        rtk: proven.session.launch.rtk.clone(),
    };
    let proposal = propose_import(
        command,
        epoch,
        &pending.request,
        &pending.context,
        &proven,
        answer.clone(),
    );
    let proposal = match proposal {
        Ok(proposal) => proposal,
        Err(outcome) => {
            stop_lane(seats, pending.request.lane);
            return typed_outcome(outcome);
        }
    };
    match commit(bus, proposal) {
        Ok(committed) => committed_lane_reply(committed, answer),
        Err(outcome) => {
            stop_lane(seats, pending.request.lane);
            typed_outcome(outcome)
        }
    }
}

#[cfg(test)]
pub(super) fn import(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    seats: &mut Seats,
    runtime: &Runtime,
    command: &wire::AdmittedCommand,
    request: &LaneState,
) -> Outcome<CommandAnswer> {
    let launch = match begin_import(bus, paths, epoch, seats, runtime, request) {
        Ok(launch) => launch,
        Err(outcome) => return outcome,
    };
    let (launch, pending) = launch.into_parts();
    finish_import(
        bus,
        paths,
        epoch,
        seats,
        command,
        pending,
        launch.establish(),
    )
}

fn admit_import(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    seats: &Seats,
    request: &LaneState,
) -> CommandResult<ImportContext> {
    let worktree = request.worktree.clone().ok_or_else(|| {
        lane_refusal(
            "lane::import-worktree-missing",
            Some("one canonical linked worktree root".into()),
            None,
            "name the worktree receiving this vacant lineage seat",
        )
    })?;
    let observed = current_residency(bus, epoch)?;
    let row = observed
        .body()
        .lanes
        .iter()
        .find(|row| row.lane == request.lane)
        .cloned()
        .ok_or_else(|| {
            lane_refusal(
                "lane::missing",
                Some("one current lane".into()),
                Some(request.lane.to_string()),
                "read lane and retry with a current lane identity",
            )
        })?;
    admit_row(paths, seats, observed.body(), &row, &worktree)?;
    super::steering::lane_capability(&row)?;
    let ancestor = row.lineage.last().copied().ok_or_else(|| {
        store_fault(
            "lane::lineage-missing",
            "a durable lane import has no predecessor occupant lineage",
        )
    })?;
    let resume = resume_session(paths, &row)?;
    let at = current_instant()?;
    let carried = observe_carried_claim(bus, row.lane, ancestor, at)?;
    Ok(ImportContext {
        observed,
        row,
        worktree,
        ancestor,
        resume,
        carried,
    })
}

fn admit_row(
    paths: &RuntimePaths,
    seats: &Seats,
    residency: &wire::ResidencyHead,
    row: &wire::LaneResidency,
    worktree: &str,
) -> CommandResult<()> {
    if row.occupant.is_some() || seats.occupied(row.lane) {
        return Err(lane_refusal(
            "lane::import-occupied",
            Some(VACANT.into()),
            Some(row.disposition.clone()),
            "drain the predecessor before importing",
        ));
    }
    if worktree == row.worktree {
        return Err(lane_refusal(
            "lane::import-unmoved",
            Some("a different linked worktree".into()),
            Some(worktree.into()),
            "use lane wake in the prior worktree",
        ));
    }
    if residency
        .lanes
        .iter()
        .any(|other| other.lane != row.lane && other.worktree == worktree)
    {
        return Err(lane_refusal(
            "lane::worktree-occupied",
            Some("one lane per worktree".into()),
            Some(worktree.into()),
            "relocate the lane already bound there",
        ));
    }
    admit_worktree(paths, worktree).map(drop)
}

fn propose_import(
    command: &wire::AdmittedCommand,
    epoch: CanonicalUuid,
    request: &LaneState,
    context: &ImportContext,
    proven: &ProvenOccupant,
    answer: LaneChangeAnswer,
) -> CommandResult<JournalWrite> {
    let mut next = context.observed.body().clone();
    let row = next
        .lanes
        .iter_mut()
        .find(|row| row.lane == request.lane)
        .expect("observed imported lane remains in its cloned head");
    row.worktree.clone_from(&context.worktree);
    row.occupant = Some(proven.occupant);
    row.lineage.push(proven.occupant);
    super::lane_surface::seat_session(row, proven.session.clone());
    row.disposition = OCCUPIED.into();
    let mut proposal = write(
        command.actor().clone(),
        epoch,
        Producer::ResidentRecognition,
    )?;
    guard_residency(&mut proposal, &context.observed);
    if let Some(carried) = &context.carried {
        guard_carried_claim(&mut proposal, carried);
    }
    let started = import_facts(&mut proposal, epoch, context, proven)?;
    let head = match &context.carried {
        Some(carried) => append_claim_carry(
            &mut proposal,
            carried,
            context,
            proven.occupant,
            epoch,
            started,
        )?,
        None => started,
    };
    proposal.residency_head(head, move |_| next);
    command_result(&mut proposal, command, request.clone(), epoch, answer)?;
    Ok(proposal)
}

fn import_facts(
    write: &mut JournalWrite,
    epoch: CanonicalUuid,
    context: &ImportContext,
    proven: &ProvenOccupant,
) -> CommandResult<PendingFact> {
    let imported = write
        .fact(
            wire::body::LaneImported {
                lane: context.row.lane,
                ancestor: context.ancestor,
                from_worktree: context.row.worktree.clone(),
                worktree: context.worktree.clone(),
                vendor: context.row.vendor.clone(),
                identity: proven.session.identity.clone(),
                epoch,
            },
            FactCause::Existing(context.observed.position()),
        )
        .map_err(|error| store_fault("lane::import-fact-invalid", &error.to_string()))?;
    let declared = write
        .fact(
            session_fact(&proven.session, proven.source),
            FactCause::Pending(imported),
        )
        .map_err(|error| store_fault("lane::import-fact-invalid", &error.to_string()))?;
    proven.started(
        write,
        context.row.lane,
        epoch,
        declared,
        "lane::import-fact-invalid",
    )
}

fn observe_carried_claim(
    bus: &Bus,
    lane: CanonicalUuid,
    ancestor: CanonicalUuid,
    at: wire::Instant,
) -> CommandResult<Option<CarriedClaim>> {
    let snapshot = bus
        .task_snapshot()
        .map_err(|error| store_fault("lane::claim-heads-unreadable", &error.to_string()))?;
    let mut carried = None;
    for (heads, held) in snapshot
        .tasks()
        .iter()
        .flat_map(|heads| heads.claims().iter().map(move |held| (heads, held)))
    {
        let claim = held.claim();
        if claim.body().disposition != wire::ClaimDisposition::Active {
            continue;
        }
        if claim.body().lane != lane && claim.body().actor != Actor::Lane(lane) {
            continue;
        }
        if carried.is_some() {
            return Err(store_fault(
                "lane::claim-ambiguous",
                "one lane owns more than one active claim",
            ));
        }
        let actor_bound = matches!(claim.body().actor, Actor::Principal(_))
            || claim.body().actor == Actor::Lane(lane);
        if !actor_bound || claim.body().lane != lane || claim.body().occupant != ancestor {
            return Err(store_fault(
                "lane::claim-lineage-moved",
                "active claim ownership differs from predecessor lineage",
            ));
        }
        let lease = held.lease().ok_or_else(|| {
            store_fault(
                "lane::claim-lease-missing",
                "an imported active claim has no durable lease head",
            )
        })?;
        if lease.body().epoch != claim.body().epoch {
            return Err(store_fault(
                "lane::claim-epoch-mismatch",
                "claim and lease generations disagree",
            ));
        }
        if at < lease.body().start || at >= lease.body().expiry {
            return Err(lane_refusal(
                "lane::claim-lease-stale",
                Some(format!(
                    "{} <= import instant < {}",
                    lease.body().start,
                    lease.body().expiry
                )),
                Some(at.to_string()),
                "reallocate an expired claim before importing its lane",
            ));
        }
        carried = Some((
            heads.statement().body().born.clone(),
            claim.clone(),
            lease.clone(),
        ));
    }
    // Read once the lane is known to carry a claim, and at the same barrier the
    // claim and lease heads were observed at, so the successor takes over at the
    // position its predecessor is guarded against having moved past.
    carried
        .map(|(task, claim, lease)| {
            Ok(CarriedClaim {
                task,
                claim,
                lease,
                source: bus.source_boundary(&lane).map_err(|error| {
                    store_fault("lane::claim-boundary-unreadable", &error.to_string())
                })?,
            })
        })
        .transpose()
}

fn guard_carried_claim(write: &mut JournalWrite, carried: &CarriedClaim) {
    let task = carried.task.clone();
    let claim = carried.claim.clone();
    let lease = carried.lease.clone();
    write.guard(JournalGuard::new(move |read| {
        let claimed = claim.body().claimed;
        if read.claim_head(&task, claimed)? != Some(claim.clone()) {
            return Err(journal::GuardError::refused("lane::claim-head-moved"));
        }
        if read.lease_head(&task, claimed)? != Some(lease.clone()) {
            return Err(journal::GuardError::refused("lane::lease-head-moved"));
        }
        Ok(())
    }));
}

fn append_claim_carry(
    write: &mut JournalWrite,
    carried: &CarriedClaim,
    context: &ImportContext,
    occupant: CanonicalUuid,
    epoch: CanonicalUuid,
    cause: PendingFact,
) -> CommandResult<PendingFact> {
    let fact = write
        .fact(
            wire::body::ClaimCarried {
                task: carried.task.clone(),
                lane: context.row.lane,
                occupant,
                ancestor: context.ancestor,
                epoch,
                source: carried.source.clone(),
            },
            FactCause::Pending(cause),
        )
        .map_err(|error| store_fault("lane::claim-carry-invalid", &error.to_string()))?;
    let mut claim = carried.claim.body().clone();
    claim.occupant = occupant;
    claim.epoch = epoch;
    write.claim_head(carried.task.clone(), fact, move |position| {
        claim.fact = position;
        claim
    });
    let mut lease = carried.lease.body().clone();
    lease.epoch = epoch;
    write.lease_head(carried.task.clone(), fact, move |_| lease);
    Ok(fact)
}

#[cfg(test)]
#[path = "lane_recovery_tests.rs"]
mod tests;
