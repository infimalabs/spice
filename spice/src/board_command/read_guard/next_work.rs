use super::*;
use crate::resident::message_surface;
use occupant_match::running_occupant;
pub(crate) use occupant_match::{OccupantMatch, RetainedMatch};

mod occupant_match;

/// The resident's retained lane profiles, when message startup restored
/// them: what a running lane's `task next` ranks candidates by.
pub(crate) type OccupantProfiles<'a> = Option<&'a std::sync::Mutex<message_surface::MessageState>>;

/// Project and decide `task next` from the snapshot's live allocation frontier.
///
/// Terminal task heads stay behind the snapshot index. Only dependency stamps
/// named by the live graph consult their compact standing, so request work is
/// proportional to live vertices and edges rather than retained history.
#[cfg(test)]
pub(crate) fn read_next_work(
    bus: &Bus,
    observed: &bus::TaskSnapshot,
    principal: wire::Actor,
    allocation: &board::NextConfig,
    profiles: OccupantProfiles<'_>,
) -> Result<BoardAnswer, Outcome<CommandAnswer>> {
    let residency = observed.residency().expect("test snapshot has residency");
    let owners = std::sync::Arc::new(board::OwnershipSnapshot::from_residency(
        board::team_policy::Config::default(),
        residency.body(),
    ));
    read_next_work_with_resources(
        bus, observed, principal, allocation, profiles, None, &owners,
    )
}

pub(in crate::board_command) fn read_next_work_with_resources(
    bus: &Bus,
    observed: &bus::TaskSnapshot,
    principal: wire::Actor,
    allocation: &board::NextConfig,
    profiles: OccupantProfiles<'_>,
    resources: Option<&crate::board_command::resources::ResourceClaims>,
    owners: &std::sync::Arc<board::OwnershipSnapshot>,
) -> Result<BoardAnswer, Outcome<CommandAnswer>> {
    let residency = observed.residency().ok_or_else(|| {
        store_fault(
            "board::residency-missing",
            "a running resident has no current residency head",
        )
    })?;
    if let Some(fault) = observed.allocation_fault() {
        return Err(match fault {
            bus::AllocationFault::ActiveTerminal { task } => protocol_fault(
                "board::allocation-active-terminal",
                format!("terminal task {task} retains an active claim"),
            ),
        });
    }
    let (entries, satisfied, origin_threads, frontier_heads, _work) =
        project_frontier(observed, residency.body())?;

    let held = held_index(&entries, &principal)?;

    let team = actor_subscription(residency.body(), &principal)?;
    let subscription = owners.for_team(team.team);
    let mut environment = observed.environment();
    if let Some(resources) = resources {
        environment = resources.observe(environment, &entries)?;
    }
    let now = current_instant()?;
    let design_request = if held.is_none() {
        project_design_request(bus, observed, &entries, residency.body(), &principal)?
    } else {
        None
    };
    // Only a selection asks for scores: a held claim or an owed Design request
    // answers without fitting the frequency profile or taking the profile lock.
    let matcher = if held.is_none() && design_request.is_none() {
        OccupantMatch::open(profiles, residency.body(), &principal, &entries)?
    } else {
        None
    };
    let match_score = |entry: &Entry| matcher.as_ref().map_or(0.0, |matcher| matcher.score(entry));
    let state = if let Some(index) = held {
        NextWorkState::Active {
            task: next_task(frontier_heads[index], &principal, &entries[index], owners),
        }
    } else if let Some(request) = design_request {
        NextWorkState::DesignRequest { request }
    } else if let Some(selected) = board::select_with_satisfied_at(
        &entries,
        board::Claimant {
            actor: &principal,
            subscription: &subscription,
            environment: &environment,
            match_score: matcher
                .as_ref()
                .map(|_| &match_score as &dyn Fn(&Entry) -> f64),
        },
        *allocation,
        &satisfied,
        now,
    ) {
        let entry = selected.entry();
        let heads = frontier_heads.get(selected.entry_index()).ok_or_else(|| {
            protocol_fault(
                "board::allocation-index-invalid",
                "selected task position is absent from the live frontier",
            )
        })?;
        let task = next_task(heads, &principal, entry, owners);
        match independent_handoff_awaits_peer(
            &entries,
            &satisfied,
            entry,
            &principal,
            (residency.body(), owners),
            &environment,
            now,
        )
        .map_err(|fault| protocol_fault("board::peer-subscription-ambiguous", fault.detail()))?
        {
            Some(until) => NextWorkState::AwaitingPeer { task, until },
            None => NextWorkState::Candidate { task },
        }
    } else {
        NextWorkState::Dry {
            board: dry_counts(
                &entries,
                &satisfied,
                &origin_threads,
                &principal,
                &actor_route(&principal, residency.body())?,
                &environment,
                owners,
            ),
            drive: team.drive,
            drain: team.drain,
        }
    };

    next_answer(observed, residency, &principal, state, &environment)
}

fn next_answer(
    observed: &bus::TaskSnapshot,
    residency: &journal::JournalHead<wire::ResidencyHead>,
    principal: &wire::Actor,
    next: NextWorkState,
    environment: &wire::Environment,
) -> Result<BoardAnswer, Outcome<CommandAnswer>> {
    let cursor = observed.cursor().ok_or_else(|| {
        store_fault(
            "board::cursor-missing",
            "a running resident has no durable snapshot cursor",
        )
    })?;
    let mut residency = positioned(residency);
    for lane in &mut residency.body.lanes {
        lane.capabilities.clear();
    }
    let appliance = spice::version::running()
        .map_err(Outcome::Fault)?
        .product
        .clone();
    Ok(BoardAnswer {
        schema: journal::SCHEMA_VERSION,
        cursor,
        task_barrier: task_projection_barrier(observed.task_projection_cursor()),
        oops_generation: observed.oops_generation().map(positioned),
        principal: principal.clone(),
        actor_route: actor_route(principal, &residency.body)?,
        next: Some(next),
        tasks: Vec::new(),
        finished: Vec::new(),
        offer: Vec::new(),
        blocked: Vec::new(),
        unallocated: Default::default(),
        workflow_waiting: Vec::new(),
        design_cycles: Default::default(),
        origins: Default::default(),
        history: None,
        acknowledgments: Default::default(),
        closures: Default::default(),
        steps: Default::default(),
        trust: positioned_trust(observed),
        resource_pools: environment
            .resource_pools()
            .map(|(name, available)| (name.clone(), available))
            .collect(),
        appliance,
        residency,
        addresses: Default::default(),
        selected: None,
        codes: Default::default(),
    })
}
fn held_index(
    entries: &[Entry],
    principal: &wire::Actor,
) -> Result<Option<usize>, Outcome<CommandAnswer>> {
    let mut held = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.claim_of(principal).is_some());
    let first = held.next().map(|(index, _)| index);
    if held.next().is_some() {
        return Err(protocol_fault(
            "claim::active-ambiguous",
            "one principal holds more than one active claim",
        ));
    }
    Ok(first)
}

fn project_design_request(
    bus: &Bus,
    observed: &bus::TaskSnapshot,
    entries: &[Entry],
    residency: &wire::ResidencyHead,
    principal: &wire::Actor,
) -> Result<Option<wire::intent::DesignRequest>, Outcome<CommandAnswer>> {
    running_occupant(residency, principal)
        .map(|(lane, _occupant)| {
            crate::resident::design_awareness::project_request(bus, observed, entries, lane)
        })
        .transpose()
        .map(Option::flatten)
}

type FrontierProjection<'a> = (
    Vec<Entry>,
    board::ResolvedTasks,
    Vec<String>,
    Vec<&'a journal::TaskHeads>,
    bus::AllocationWork,
);

/// Every open Design's cycle among a scoped read's rows, judged against the
/// live frontier: the same resolution live allocation uses, since the scoped
/// rows need not include the work those Designs released.
pub(in crate::board_command) fn scoped_design_cycles(
    observed: &bus::TaskSnapshot,
    snapshot: &ValidatedSnapshot,
) -> Result<
    std::collections::BTreeMap<wire::TaskStamp, wire::rooms::DesignCycle>,
    Outcome<CommandAnswer>,
> {
    if !snapshot
        .entries
        .iter()
        .any(|entry| wire::intent::is_open_design(entry.statement().intent, entry.workflow()))
    {
        return Ok(std::collections::BTreeMap::new());
    }
    let (_, live, ..) = project_frontier(observed, &snapshot.answer.residency.body)?;
    Ok(board::design_cycles(&snapshot.entries, &live))
}

pub(in crate::board_command) fn project_frontier<'a>(
    observed: &'a bus::TaskSnapshot,
    residency: &wire::ResidencyHead,
) -> Result<FrontierProjection<'a>, Outcome<CommandAnswer>> {
    let allocation = observed.allocation_read();
    let frontier = allocation.frontier();
    let mut entries = Vec::with_capacity(frontier.len());
    let mut satisfied = board::ResolvedTasks::new();
    let mut origin_threads = Vec::with_capacity(frontier.len());
    let mut frontier_heads = Vec::with_capacity(frontier.len());
    let mut prior = None;
    for heads in frontier {
        let statement = heads.statement().body();
        if !heads.workflow().body().status.is_pending() {
            return Err(protocol_fault(
                "board::allocation-index-invalid",
                "live frontier contains a terminal workflow",
            ));
        }
        if prior
            .as_ref()
            .is_some_and(|prior: &wire::TaskStamp| prior >= &statement.born)
        {
            return Err(protocol_fault(
                "board::allocation-index-invalid",
                "live frontier is not in unique task-stamp order",
            ));
        }
        prior = Some(statement.born.clone());
        let references = statement.dependencies.iter().chain(
            statement
                .preparation
                .iter()
                .flat_map(|preparation| [&preparation.parent, &preparation.controller]),
        );
        for dependency in references {
            if let Some(standing) = allocation.dependency_standing(dependency)
                && standing.is_settled()
            {
                satisfied.settle(
                    dependency.clone(),
                    standing == bus::DependencyStanding::Completed,
                );
            }
        }
        // Control reads the parent's intent and whether the controller still
        // stands, so both must be known even when off the frontier.
        for known in statement
            .preparation
            .iter()
            .flat_map(|preparation| [&preparation.parent, &preparation.controller])
        {
            if let Some(task) = observed.task_heads(known) {
                satisfied.know(known.clone(), task.statement().body().intent);
            }
        }
        let explicit_release = heads.claims().last().is_some_and(|held| {
            observed
                .facts()
                .is_explicit_release(&statement.born, held.claim().position())
        });
        let phase_handoff = phase_handoff(observed.facts(), heads)?;
        if observed.facts().has_invalid_creator_team(&statement.born) {
            return Err(protocol_fault(
                "board::task-creator-team-invalid",
                "a non-lane task creation names a creator team",
            ));
        }
        let handle = statement.handle(heads.workflow().body().phase);
        let entry = Entry::new(
            statement.clone(),
            heads.workflow().body().clone(),
            heads.completion().body().clone(),
            heads.claim_bodies(),
        )
        .map_err(|error| {
            store_fault(
                "board::head-invalid",
                format!("task {handle} current heads are invalid: {error}"),
            )
        })?
        .with_creator_team(statement.creation.creator_team())
        .with_explicit_release(explicit_release)
        .with_phase_handoff(phase_handoff)
        .with_design(observed.facts().design(statement))
        .with_topic_origin_at(topic_origin_at(observed.facts(), statement));
        origin_threads.push(task_origin_thread(
            observed,
            &statement.born,
            residency,
            &handle,
        )?);
        entries.push(entry);
        frontier_heads.push(heads);
    }
    satisfied.hold_released_work(&entries);
    let work = allocation.work();
    Ok((entries, satisfied, origin_threads, frontier_heads, work))
}

fn next_task(
    heads: &journal::TaskHeads,
    principal: &wire::Actor,
    entry: &Entry,
    owners: &board::OwnershipSnapshot,
) -> NextWorkTask {
    let mut statement = positioned(heads.statement());
    withhold_taste_findings(principal, &mut statement.body.taste);
    let ownership = owners.owners(entry);
    NextWorkTask {
        ownership: Some(wire::intent::TaskOwnership {
            teams: ownership.teams.clone(),
            reserved: ownership.reserved,
        }),
        statement,
        workflow: positioned(heads.workflow()),
        completion: positioned(heads.completion()),
        claims: board_claims(heads),
    }
}

fn dry_counts(
    entries: &[Entry],
    satisfied: &board::ResolvedTasks,
    origin_threads: &[String],
    principal: &wire::Actor,
    route: &wire::intent::BoardActorRoute,
    environment: &wire::Environment,
    owners: &board::OwnershipSnapshot,
) -> NextWorkCounts {
    let mut counts = NextWorkCounts {
        active: 0,
        stale: 0,
        blocked: 0,
        workflow_waiting: 0,
        designs: wire::rooms::DesignCycleCounts::default(),
        deferred: 0,
        rounds_waiting: Vec::new(),
        boarded_elsewhere: Vec::new(),
        unowned_topics: Vec::new(),
    };
    let waits = board::work_waits_resolved(entries, satisfied, environment);
    let mut unowned_topics = std::collections::BTreeSet::new();
    let pending = entries.iter().zip(origin_threads).filter(|(entry, _)| {
        entry.is_acquirable_standing() && entry.workflow().status.is_pending()
    });
    for (entry, origin) in pending {
        let ownerless = !entry.is_held() && owners.unallocated(entry).is_some();
        if ownerless {
            counts.blocked += 1;
            unowned_topics.extend(entry.topic());
        } else if let Some(cycle) = board::design_cycle(entry, satisfied) {
            counts.designs.observe(cycle);
        } else {
            match waits.get(entry.born()) {
                Some(board::WorkWait::Blocked) => counts.blocked += 1,
                Some(board::WorkWait::Workflow) => counts.workflow_waiting += 1,
                Some(board::WorkWait::Selection) | None => {}
            }
        }
        if satisfied.speculative_design_work(entry.statement(), entry.workflow()) {
            continue;
        }
        let completion = entry.completion();
        let phase = &entry.workflow().phase;
        let target = completion.target(phase).get();
        if !entry.is_held()
            && !ownerless
            && !entry.is_lapsed()
            && entry.workflow().deferral.is_none()
            && !completion.arrivals.is_empty()
            && completion.arrivals.len() < target as usize
            && board::design_cycle(entry, satisfied).is_none()
            && board::dependencies_satisfied(entry, satisfied)
            && board::environment_satisfied(entry, environment)
        {
            counts.rounds_waiting.push(wire::intent::CompletionWait {
                task: entry.handle().clone(),
                phase: *phase,
                arrived: completion.arrivals.len(),
                target,
                actor_arrived: entry.participates(principal),
            });
        }
        if entry.is_held() {
            counts.active += 1;
            if *origin == route.origin_thread {
                counts.boarded_elsewhere.extend(
                    entry
                        .active_claims()
                        .filter(|held| held.claim().actor != *principal)
                        .map(|held| wire::intent::BoardedElsewhere {
                            task: entry.handle().clone(),
                            holder: held.claim().actor.clone(),
                        }),
                );
            }
        } else if entry.is_lapsed() {
            counts.stale += 1;
        }
        if entry.workflow().deferral.is_some() {
            counts.deferred += 1;
        }
    }
    counts.unowned_topics = unowned_topics.into_iter().cloned().collect();
    counts
}

fn task_origin_thread(
    observed: &bus::TaskSnapshot,
    task: &wire::TaskStamp,
    residency: &wire::ResidencyHead,
    handle: &TaskHandle,
) -> Result<String, Outcome<CommandAnswer>> {
    if observed.facts().has_duplicate_task_origin(task) {
        return Err(protocol_fault(
            "board::task-origin-duplicate",
            format!("task {handle} has more than one creation fact"),
        ));
    }
    match observed.facts().task_origin(task) {
        Some(bus::TaskOrigin::Identity(identity)) => {
            route_actor_identity(identity).ok_or_else(|| {
                protocol_fault(
                    "board::task-origin-session-invalid",
                    "the creating vendor session identity cannot name an actor task route",
                )
            })
        }
        Some(bus::TaskOrigin::ResidencyIdentity(lane)) => {
            let occupant = residency
                .lanes
                .iter()
                .find(|row| row.lane == *lane && row.session.is_none())
                .and_then(|row| row.occupant)
                .ok_or_else(|| {
                    protocol_fault(
                        "board::task-origin-occupant-missing",
                        "a lane task has no authoritative creating occupant",
                    )
                })?;
            let identity = observed
                .facts()
                .lane_session_identity(*lane, occupant)
                .map_or_else(|| occupant.to_string(), str::to_owned);
            route_actor_identity(&identity).ok_or_else(|| {
                protocol_fault(
                    "board::task-origin-session-invalid",
                    "the creating vendor session identity cannot name an actor task route",
                )
            })
        }
        None => Err(protocol_fault(
            "board::task-origin-missing",
            format!("task {handle} has no authoritative creation fact"),
        )),
    }
}
