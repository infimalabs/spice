use super::*;

/// Defer one pending task. Another actor's deferral of an open Design a
/// lane holds asks that holder to park in the same write; the returned lane is
/// the one asked.
pub(crate) fn task_defer(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    defer: &TaskDefer,
    repository: Option<Host<'_>>,
    config: &steer::Config,
) -> (Outcome<CommandAnswer>, Option<wire::CanonicalUuid>) {
    let snapshot = match validated_working_snapshot(
        bus,
        repository.map(|host| host.repository),
        command.actor().clone(),
        std::slice::from_ref(&defer.task),
    ) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return (outcome, None),
    };
    change_deferral(
        bus,
        command,
        &snapshot,
        DeferralRequest {
            task: &defer.task,
            deferral: Some(defer.wait),
            claim_requested: false,
            stop: Some(config),
        },
        defer.clone(),
        repository,
    )
}

/// Wake one task whose deferral is currently set.
pub(in crate::board_command) fn task_wake(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    wake: &TaskWake,
    repository: Option<Host<'_>>,
) -> Outcome<CommandAnswer> {
    let snapshot = match validated_working_snapshot(
        bus,
        repository.map(|host| host.repository),
        command.actor().clone(),
        std::slice::from_ref(&wake.task),
    ) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    change_deferral(
        bus,
        command,
        &snapshot,
        DeferralRequest {
            task: &wake.task,
            deferral: None,
            claim_requested: wake.claim,
            stop: None,
        },
        wake.clone(),
        repository,
    )
    .0
}

fn append_deferral_fact(
    write: &mut JournalWrite,
    entry: &Entry,
    deferral: Option<Instant>,
) -> Result<PendingFact, Outcome<CommandAnswer>> {
    let fact = if let Some(until) = deferral {
        write.fact(
            body::TaskDeferred {
                task: entry.born().clone(),
                until,
            },
            FactCause::Root,
        )
    } else {
        write.fact(
            body::TaskWoken {
                task: entry.born().clone(),
            },
            FactCause::Root,
        )
    };
    fact.map_err(|error| store_fault("task::deferral-build", error.to_string()))
}

struct DeferralTransition {
    workflow: WorkflowHead,
    post: Option<Entry>,
    releases_claim: bool,
}

fn deferral_transition(
    entry: &Entry,
    command: &wire::AdmittedCommand,
    task: &TaskHandle,
    deferral: Option<Instant>,
) -> Result<DeferralTransition, Outcome<CommandAnswer>> {
    // Settled work never becomes workable again.
    if !entry.workflow().status.is_pending() {
        return Err(refusal(
            "task::settled",
            Some(task.to_string()),
            "defer or wake only pending work",
        ));
    }
    let releases_claim = deferral.is_some() && entry.claim_of(command.actor()).is_some();
    let mut workflow = entry.workflow().clone();
    workflow.deferral = deferral;
    let post = if deferral.is_none() {
        Some(
            Entry::new(
                entry.statement().clone(),
                workflow.clone(),
                entry.completion().clone(),
                entry.claim_bodies(),
            )
            .map_err(|error| {
                refusal(
                    "task::deferral-refused",
                    Some(error.to_string()),
                    "wake a task into a valid pending state",
                )
            })?
            .with_design(entry.design()),
        )
    } else {
        None
    };
    Ok(DeferralTransition {
        workflow,
        post,
        releases_claim,
    })
}

fn prepare_deferral_release<'a>(
    snapshot: &ValidatedSnapshot,
    command: &wire::AdmittedCommand,
    entry: &Entry,
    repository: Option<Host<'a>>,
    releases_claim: bool,
) -> Result<Option<spice::task_git::ParkedRelease<'a>>, Outcome<CommandAnswer>> {
    let Some(resident) = repository.filter(|_| releases_claim) else {
        return Ok(None);
    };
    let lane = task_repository(snapshot, command, &resident)?;
    let claimed = entry
        .claim_of(command.actor())
        .expect("release predicate requires a claim")
        .claimed();
    let opening = snapshot
        .facts
        .at(claimed)
        .and_then(|envelope| envelope.fact.read::<body::ClaimWon>())
        .and_then(|won| won.git);
    spice::task_git::prepare_free(&lane, resident.ambient, opening.as_ref())
        .map_err(task_git_outcome)
}

fn append_deferral_release(
    write: &mut JournalWrite,
    entry: &Entry,
    actor: &Actor,
    parked: Option<&spice::task_git::ParkedRelease>,
    releases_claim: bool,
) -> Result<(), Outcome<CommandAnswer>> {
    if !releases_claim {
        return Ok(());
    }
    let freed = write
        .fact(
            body::ClaimFreed {
                task: entry.born().clone(),
                actor: actor.clone(),
                parked: parked.map(|prepared| prepared.parked().clone()),
            },
            FactCause::Root,
        )
        .map_err(|error| store_fault("task::defer-release-build", error.to_string()))?;
    let mut released = entry
        .claim_of(actor)
        .expect("release predicate requires a claim")
        .claim()
        .clone();
    released.disposition = ClaimDisposition::Freed;
    write.claim_head(entry.born().clone(), freed, move |position| ClaimHead {
        fact: position,
        ..released
    });
    Ok(())
}

fn finish_deferral<B: serde::Serialize>(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    intent: B,
    mut write: JournalWrite,
    answer: wire::intent::TaskAnswer,
    parked: Option<spice::task_git::ParkedRelease>,
    prepared_claim: Option<spice::task_git::ClaimPreparation>,
) -> Outcome<CommandAnswer> {
    if let Err(error) =
        write.command_result(request(command, intent), command.epoch(), answer.clone())
    {
        let outcome = store_fault("task::deferral-receipt", error.to_string());
        let outcome = rollback_release(parked, outcome);
        if let Some(prepared) = prepared_claim
            && let Err(rollback) = prepared.rollback()
        {
            return task_git_outcome(rollback);
        }
        return outcome;
    }
    let body = CommandAnswer::Task(Box::new(answer));
    if let Some(parked) = parked {
        committed_with_task_git(bus, write, body, PreparedTaskGit::Release(parked))
    } else if let Some(prepared) = prepared_claim {
        committed_with_task_git(bus, write, body, PreparedTaskGit::Claim(prepared))
    } else {
        committed(bus, write, body)
    }
}

/// What one defer or wake asks of its task: the new deferral, or none to
/// wake, whether the caller claims the task in the same write, and the
/// steering policy a deferral stops a held open Design's holder under.
struct DeferralRequest<'a> {
    task: &'a TaskHandle,
    deferral: Option<Instant>,
    claim_requested: bool,
    stop: Option<&'a steer::Config>,
}

/// Whether this deferral of `entry` stops a lane holding it: an open Design
/// deferred by anyone but its holder, whose own deferral releases it instead.
/// Ordinary work deferred by another actor stays held.
fn stops_holder(entry: &Entry, deferral: Option<Instant>, releases_claim: bool) -> bool {
    deferral.is_some()
        && !releases_claim
        && wire::intent::is_open_design(entry.statement().intent, entry.workflow())
}

fn change_deferral<B: serde::Serialize + Clone>(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    snapshot: &ValidatedSnapshot,
    request: DeferralRequest<'_>,
    intent: B,
    repository: Option<Host<'_>>,
) -> (Outcome<CommandAnswer>, Option<wire::CanonicalUuid>) {
    let DeferralRequest {
        task,
        deferral,
        claim_requested,
        stop,
    } = request;
    let Some(entry) = task_entry(&snapshot.entries, task) else {
        let unknown = refusal(
            "task::unknown",
            Some(task.to_string()),
            "read board and name one current task handle",
        );
        return (unknown, None);
    };
    let DeferralTransition {
        workflow,
        post,
        releases_claim,
    } = match deferral_transition(entry, command, task, deferral) {
        Ok(transition) => transition,
        Err(outcome) => return (outcome, None),
    };
    let selected = post.as_ref().unwrap_or(entry);
    let atomic_claim = match prepare_mutation_claim(
        bus,
        command,
        repository,
        snapshot,
        selected,
        claim_requested,
    ) {
        Ok(claim) => claim,
        Err(outcome) => return (outcome, None),
    };
    let claim_answer = atomic_claim.as_ref().map(|claim| claim.answer.clone());
    let next = claim_answer
        .as_ref()
        .and_then(|claim| claim.next.clone())
        .or_else(|| post.as_ref().and_then(|post| next_action(snapshot, post)));
    let mut write = match prepare_mutation_write(command, entry, snapshot, atomic_claim.as_ref()) {
        Ok(write) => write,
        Err(outcome) => return (outcome, None),
    };
    let parked =
        match prepare_deferral_release(snapshot, command, entry, repository, releases_claim) {
            Ok(parked) => parked,
            Err(outcome) => return (outcome, None),
        };
    let fact = match append_deferral_fact(&mut write, entry, deferral) {
        Ok(fact) => fact,
        Err(outcome) => return (rollback_release(parked, outcome), None),
    };
    write.workflow_head(entry.born().clone(), fact, move |_| workflow);
    let stopped = match stop.filter(|_| stops_holder(entry, deferral, releases_claim)) {
        Some(config) => match Deactivation::of(bus, snapshot, entry)
            .and_then(|deactivation| deactivation.stage(bus, command, config, &mut write))
        {
            Ok(stopped) => stopped,
            Err(outcome) => return (rollback_release(parked, outcome), None),
        },
        None => None,
    };
    if let Err(outcome) = append_deferral_release(
        &mut write,
        entry,
        command.actor(),
        parked.as_ref(),
        releases_claim,
    ) {
        return (rollback_release(parked, outcome), None);
    }
    let prepared_claim = match atomic_claim {
        Some(claim) => match claim.prepared.append(&mut write, command, selected) {
            Ok(prepared) => prepared,
            Err(outcome) => return (rollback_release(parked, outcome), None),
        },
        None => None,
    };
    let answer = wire::intent::TaskAnswer {
        diagnostic: None,
        duplicates: Vec::new(),
        duplicates_omitted: 0,
        ownership: Some(snapshot.ownership(selected)),
        placement: None,
        created: Default::default(),
        taste: None,
        completion_contract: None,
        task: entry.handle().clone(),
        status: if deferral.is_some() {
            "deferred".to_owned()
        } else {
            "pending".to_owned()
        },
        completion: None,
        claim: claim_answer,
        closed: Vec::new(),
        next,
    };
    let outcome = finish_deferral(bus, command, intent, write, answer, parked, prepared_claim);
    let committed = matches!(
        &outcome,
        Outcome::Reply(Reply {
            position: ReplyPosition::Committed(_),
            ..
        })
    );
    (outcome, stopped.filter(|_| committed))
}
