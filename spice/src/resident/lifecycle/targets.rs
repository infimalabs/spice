use super::*;
#[cfg(test)]
mod tests;

pub(super) fn project_targets<'a>(
    snapshot: &bus::TaskSnapshot,
    projection: &'a Projection,
    now: Instant,
    holds: &Holds,
    allocation: &board::NextConfig,
    profiles: Option<&std::sync::Mutex<super::super::message_surface::MessageState>>,
    owners: &std::sync::Arc<board::OwnershipSnapshot>,
) -> CommandResult<Vec<Target<'a>>> {
    let Some(residency) = snapshot.residency() else {
        return Ok(Vec::new());
    };
    if !residency
        .body()
        .lanes
        .iter()
        .any(|row| row.occupant.is_none() && row.disposition != "occupied" && holds.seats(row.lane))
    {
        return Ok(Vec::new());
    }
    let selection = projection.selection();
    let satisfied = selection.resolved();
    let appliance = spice::version::running().map_err(Outcome::Fault)?;
    let environment = snapshot
        .environment()
        .with_appliance(appliance.product.clone());
    let projected_residency = residency.body();
    let view = BoardView {
        entries: projection.entries(),
        selection,
        ready_at: projection.ready_at(),
        allocation,
        environment: &environment,
        owners,
        now,
    };
    let matcher = RetainedMatch::open(profiles, projected_residency, projection.entries())
        .map_err(super::typed_outcome)?;
    let mut targets = Vec::new();
    for row in &projected_residency.lanes {
        if row.occupant.is_some() || row.disposition == "occupied" || !holds.seats(row.lane) {
            continue;
        }
        let inbox = projection.inbox_wake(snapshot, row.lane);
        let (held_claim, held_parked) = held_claim(
            view.entries,
            satisfied,
            view.ready_at,
            row,
            &environment,
            now,
        )?;
        let subscription =
            crate::resident::lane_surface::subscription_row(projected_residency, row.team)?;
        let first_drain_member = !projected_residency.lanes.iter().any(|member| {
            member.team == row.team && member.occupant.is_some() && member.disposition == "occupied"
        });
        let score = |entry: &Entry| {
            matcher
                .as_ref()
                .and_then(|matcher| matcher.score(row.lane, entry))
                .unwrap_or(0.0)
        };
        let match_score = matcher.as_ref().map(|_| &score as &dyn Fn(&Entry) -> f64);
        let work = if held_parked {
            Work::default()
        } else {
            work_availability(
                &view,
                projected_residency,
                row,
                subscription,
                first_drain_member,
                match_score,
            )?
        };
        targets.push(Target {
            lane: row.lane,
            last_claim: snapshot
                .facts()
                .latest_claim(&Actor::Lane(row.lane))
                .map(|(claim, _)| claim),
            pending_inbox: inbox.pending,
            inbox_reminder_only: inbox.reminder_only,
            held_claim,
            loose_worktree: false,
            drain_queue: work.drain_queue,
            last_resort: work.last_resort,
            task: work.task,
            matched: work.task.and_then(|task| {
                matcher
                    .as_ref()
                    .and_then(|matcher| matcher.score(row.lane, task))
            }),
            park: held_parked.then_some(wire::body::LanePark::Authority),
        });
    }
    Ok(targets)
}

/// Project the held-claim continuation for an occupied OMP process whose
/// durable idle state is still owned by the exact current claim. Task authority
/// admits the claim before native input is caught up; the lifecycle schedules
/// only this continuation, and the common job hold may delay it.
///
/// Deliberately level-triggered: every clean settlement under an active,
/// unparked claim yields one continuation, whether or not new input arrived.
/// A turn that ends on a reply to an interjection would otherwise strand the
/// claim. A lane that is woken repeatedly must finish, free, or defer its
/// claim; do not narrow this projection to input edges.
pub(super) fn project_retained_targets<'a>(
    snapshot: &bus::TaskSnapshot,
    projection: &'a Projection,
    paths: &RuntimePaths,
    now: Instant,
) -> CommandResult<Vec<Target<'a>>> {
    let Some(residency) = snapshot.residency() else {
        return Ok(Vec::new());
    };
    let appliance = spice::version::running().map_err(Outcome::Fault)?;
    let environment = snapshot
        .environment()
        .with_appliance(appliance.product.clone());
    let selection = projection.selection();
    let satisfied = selection.resolved();
    let mut targets = Vec::new();
    for row in &residency.body().lanes {
        let Some(occupant) = row.occupant else {
            continue;
        };
        let Some(owner) = super::super::supervisor::idle_claim(paths, row.lane, occupant)? else {
            continue;
        };
        let Some(heads) = snapshot.task_heads(owner.task()) else {
            super::super::supervisor::command::claim_activation(paths, row.lane, owner, false);
            continue;
        };
        if !owner.matches(heads) {
            super::super::supervisor::command::claim_activation(paths, row.lane, owner, false);
            continue;
        }
        let (held_claim, parked) = held_claim(
            projection.entries(),
            satisfied,
            projection.ready_at(),
            row,
            &environment,
            now,
        )?;
        let admitted = !parked && held_claim != Available::None;
        super::super::supervisor::command::claim_activation(paths, row.lane, owner, admitted);
        if held_claim == Available::None && !parked {
            continue;
        }
        targets.push(Target {
            lane: row.lane,
            last_claim: snapshot
                .facts()
                .latest_claim(&Actor::Lane(row.lane))
                .map(|(claim, _)| claim),
            pending_inbox: false,
            inbox_reminder_only: false,
            held_claim,
            loose_worktree: false,
            drain_queue: Available::None,
            last_resort: false,
            task: None,
            matched: None,
            park: parked.then_some(wire::body::LanePark::Authority),
        });
    }
    Ok(targets)
}

/// The held-claim trigger, and whether the held work is parked on authority no
/// lane holds: an unsatisfied trust grant, or a Design that awaits the
/// operator's decision or waits on work it released. A held Design still
/// gathering input or ready for a pass is its holder's pass and resumes like
/// any held work. Parked held work wakes nothing; the lane also takes no other
/// work while it holds that claim, so pending operator steering is its only
/// wake.
fn held_claim(
    entries: &[Entry],
    satisfied: &board::ResolvedTasks,
    ready_at: &BTreeMap<TaskStamp, Instant>,
    row: &wire::LaneResidency,
    environment: &wire::Environment,
    now: Instant,
) -> CommandResult<(Available, bool)> {
    let mut held = entries.iter().filter_map(|entry| {
        entry
            .active_claims()
            .find(|held| held.claim().lane == row.lane)
            .map(|held| (entry, held))
    });
    let Some((entry, claim)) = held.next() else {
        return Ok((Available::None, false));
    };
    if held.next().is_some() {
        return Err(store_fault(
            "lifecycle::held-claim-ambiguous",
            "one vacant lane carries more than one active claim",
        ));
    }
    let parked = entry
        .statement()
        .requirements
        .has_unsatisfied_trust_grant(environment)
        || board::design_hold(entry, satisfied).is_some();
    let live = claim
        .lease()
        .ok_or_else(|| {
            store_fault(
                "lifecycle::held-claim-lease-missing",
                "an active held claim has no durable lease head",
            )
        })?
        .expiry
        > now;
    let available = if live && !parked {
        Available::At(after(ready_instant(entry, ready_at)?, SETTLE_SECONDS)?)
    } else {
        Available::None
    };
    Ok((available, parked))
}

/// Park held-claim continuation and repeat-only peer presentation while one
/// armed same-round peer line remains open. Fresh peer and operator input stays
/// pending and therefore keeps its ordinary higher-priority wake.
pub(super) fn await_armed_lines(
    bus: &Bus,
    snapshot: &bus::TaskSnapshot,
    targets: &mut [Target<'_>],
) -> CommandResult<Vec<(CanonicalUuid, Seq)>> {
    let invalid = targets
        .iter()
        .filter_map(|target| {
            let (position, armed) = snapshot.facts().latest_line_arm(target.lane)?;
            armed.is_none().then_some((target.lane, position))
        })
        .collect::<Vec<_>>();
    let candidates = targets
        .iter()
        .filter_map(|target| {
            let (_, armed) = snapshot.facts().latest_line_arm(target.lane)?;
            let armed = armed.filter(|armed| arm_matches_claim(snapshot, target.lane, armed))?;
            (!armed.roots.is_empty()).then_some((target.lane, armed.clone()))
        })
        .collect::<Vec<_>>();
    let open = bus
        .read(move |read| {
            candidates
                .iter()
                .map(|(lane, armed)| {
                    let mut partners = Vec::new();
                    for root in &armed.roots {
                        if let Some(partner) = armed_root_counterpart(read, *lane, *root)? {
                            partners.push(partner);
                        }
                    }
                    Ok((*lane, (armed.task.clone(), partners)))
                })
                .collect::<Result<BTreeMap<_, _>, journal::GuardError>>()
        })
        .map_err(|error| store_fault("lifecycle::line-park-unreadable", &error.to_string()))?;
    let standing = open
        .into_iter()
        .map(|(lane, (task, partners))| {
            let stands = partners
                .into_iter()
                .any(|partner| counterpart_holds_round(snapshot, &task, partner));
            (lane, stands)
        })
        .collect::<BTreeMap<_, _>>();
    for target in targets {
        if !standing.get(&target.lane).copied().unwrap_or(false) {
            continue;
        }
        let hold_reminder = target.pending_inbox && target.inbox_reminder_only;
        if target.held_claim == Available::None && !hold_reminder {
            continue;
        }
        if hold_reminder {
            target.pending_inbox = false;
        }
        target.held_claim = Available::None;
        target.drain_queue = Available::None;
        target.park = Some(wire::body::LanePark::Line);
    }
    Ok(invalid)
}

/// Whether `armed` binds `lane`'s one current active claim exactly.
fn arm_matches_claim(
    snapshot: &bus::TaskSnapshot,
    lane: CanonicalUuid,
    armed: &wire::body::LineArmed,
) -> bool {
    let mut claims = snapshot.tasks().iter().filter(|heads| {
        heads.claims().iter().any(|held| {
            let claim = held.claim().body();
            claim.lane == lane && claim.disposition == ClaimDisposition::Active
        })
    });
    let Some(heads) = claims.next() else {
        return false;
    };
    claims.next().is_none()
        && heads.statement().body().born == armed.task
        && heads.workflow().body().phase == armed.phase
        && heads.completion().body().round == armed.round
}

/// Whether `lane` still holds an active claim in `task`'s current round.
fn counterpart_holds_round(
    snapshot: &bus::TaskSnapshot,
    task: &TaskStamp,
    lane: CanonicalUuid,
) -> bool {
    snapshot.task_heads(task).is_some_and(|heads| {
        heads.claims().iter().any(|held| {
            let claim = held.claim().body();
            claim.lane == lane && claim.disposition == ClaimDisposition::Active
        })
    })
}

/// The other lane on one armed root while that line remains open. A missing,
/// terminal or malformed derived line head holds no park.
fn armed_root_counterpart(
    read: &journal::JournalRead<'_>,
    lane: CanonicalUuid,
    root: CanonicalUuid,
) -> Result<Option<CanonicalUuid>, journal::GuardError> {
    let Some(head) = read.peer_line_head(root)? else {
        return Ok(None);
    };
    if head.terminal().is_some()
        || head.participants().len() != 2
        || !head.participants().contains(&Actor::Lane(lane))
    {
        return Ok(None);
    }
    Ok(
        match head
            .participants()
            .iter()
            .find(|participant| **participant != Actor::Lane(lane))
        {
            Some(Actor::Lane(partner)) => Some(*partner),
            Some(Actor::Principal(_) | Actor::Spice) | None => None,
        },
    )
}

/// Leave a lane whose deadline-bounded jobs are unfinished to the job keeper:
/// its wake on completion, or the admitted expiry (`delivered`) when no keeper
/// speaks, causes the next turn. A held claim and repeat peer presentation wait
/// behind that boundary; fresh peer input and operator steering still outrank
/// it. Services and ended jobs bind nothing, so `delivered` is absent for them.
pub(super) fn await_regular_jobs(
    targets: &mut [Target],
    now: Instant,
    delivered: impl Fn(CanonicalUuid) -> Option<Instant>,
) {
    for target in targets {
        let hold_reminder = target.pending_inbox && target.inbox_reminder_only;
        if target.held_claim == Available::None && !hold_reminder {
            continue;
        }
        let Some(delivered) = delivered(target.lane).filter(|delivered| *delivered > now) else {
            continue;
        };
        if hold_reminder {
            target.pending_inbox = false;
        }
        if target.held_claim == Available::None {
            continue;
        }
        target.held_claim = match target.held_claim {
            Available::At(settled) if settled > delivered => Available::At(settled),
            _ => Available::At(delivered),
        };
        target.drain_queue = Available::None;
        target.park = Some(wire::body::LanePark::Job { until: delivered });
    }
}

/// The drain-queue availability, last-resort class and selected task for a lane.
struct Work<'a> {
    drain_queue: Available,
    last_resort: bool,
    task: Option<&'a Entry>,
}

impl Default for Work<'_> {
    fn default() -> Self {
        Self {
            drain_queue: Available::None,
            last_resort: false,
            task: None,
        }
    }
}

fn work_availability<'e>(
    view: &BoardView<'_, 'e>,
    residency: &wire::ResidencyHead,
    row: &wire::LaneResidency,
    subscription: Option<&wire::TeamSubscription>,
    first_drain_member: bool,
    match_score: Option<&dyn Fn(&Entry) -> f64>,
) -> CommandResult<Work<'e>> {
    let Some(subscription) = subscription.filter(|subscription| subscription.drain) else {
        return Ok(Work::default());
    };
    let actor = Actor::Lane(row.lane);
    let allocation = view.owners.for_team(subscription.team);
    let (mut selected, task) = selected_ready(view, &actor, &allocation, match_score)?;
    if let Some(task) = task {
        let peer_until = crate::board_command::independent_handoff_awaits_peer(
            view.entries,
            view.selection.resolved(),
            task,
            &actor,
            (residency, view.owners),
            view.environment,
            view.now,
        )
        .map_err(|fault| store_fault("lifecycle::subscription-ambiguous", &fault.detail()))?;
        if let (Some(selected), Some(peer_until)) = (&mut selected, peer_until) {
            selected.at = peer_until;
            selected.handoff_wait = true;
        }
    }
    let last_resort = selected.is_some_and(|selected| selected.last_resort);
    let drain_queue = drain_from_selected(selected, first_drain_member)?;
    Ok(Work {
        drain_queue,
        last_resort,
        task,
    })
}
