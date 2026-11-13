use super::*;
struct ReleaseTransition {
    post: Entry,
    claimed: Seq,
    retraction: Option<board::round::Retraction>,
}

fn release_transition(
    entry: &Entry,
    actor: &Actor,
) -> Result<ReleaseTransition, Outcome<CommandAnswer>> {
    if entry.claims().is_empty() {
        return Err(refusal(
            "claim::head-missing",
            Some(entry.handle().to_string()),
            "free only a task carrying an active claim",
        ));
    }
    let Some(held) = entry.claim_of(actor) else {
        return Err(refusal(
            "claim::ownership-refused",
            entry
                .latest_claim()
                .map(|latest| latest.claim().actor.to_string()),
            "free the task through the capability that currently owns it",
        ));
    };
    let post = freed_projection(
        entry,
        actor,
        entry.statement().clone(),
        entry.workflow().clone(),
    )
    .with_explicit_release(true);
    let retraction = board::round::Retraction::owed(&post);
    let post = match &retraction {
        Some(retraction) => retraction.apply(post),
        None => post,
    };
    Ok(ReleaseTransition {
        post,
        claimed: held.claimed(),
        retraction,
    })
}

pub(super) fn prepare_release<'a>(
    snapshot: &ValidatedSnapshot,
    command: &wire::AdmittedCommand,
    claimed: Seq,
    repository: Option<Host<'a>>,
    planning_phase: Option<&str>,
) -> Result<Option<spice::task_git::ParkedRelease<'a>>, Outcome<CommandAnswer>> {
    let Some(resident) = repository else {
        return Ok(None);
    };
    let lane = task_repository(snapshot, command, &resident)?;
    let opening = snapshot
        .facts
        .at(claimed)
        .and_then(|envelope| envelope.fact.read::<body::ClaimWon>())
        .and_then(|won| won.git);
    if let Some(phase) = planning_phase {
        spice::task_git::prepare_planning_free(&lane, resident.ambient, phase, opening.as_ref())
            .map_err(task_git_outcome)
    } else {
        spice::task_git::prepare_free(&lane, resident.ambient, opening.as_ref())
            .map_err(task_git_outcome)
    }
}

pub(super) fn for_completion<'a>(
    snapshot: &ValidatedSnapshot,
    command: &wire::AdmittedCommand,
    entry: &Entry,
    repository: Option<Host<'a>>,
) -> Result<Option<spice::task_git::ParkedRelease<'a>>, Outcome<CommandAnswer>> {
    let phase = entry.workflow().phase.as_str();
    if !gate::lacks_commit_authority(phase) {
        return Ok(None);
    }
    let claimed = entry
        .claim_of(command.actor())
        .expect("completion requires a claim")
        .claimed();
    prepare_release(snapshot, command, claimed, repository, Some(phase))
}

fn append_release(
    write: &mut JournalWrite,
    entry: &Entry,
    transition: &ReleaseTransition,
    actor: &Actor,
    parked: Option<&spice::task_git::ParkedRelease>,
) -> Result<PendingFact, Outcome<CommandAnswer>> {
    let freed = write
        .fact(
            body::ClaimFreed {
                task: entry.born().clone(),
                actor: actor.clone(),
                parked: parked.map(|prepared| prepared.parked().clone()),
            },
            FactCause::Root,
        )
        .map_err(|error| store_fault("claim::free-build", error.to_string()))?;
    let next_claim = transition
        .post
        .claim(transition.claimed)
        .expect("released task retains its claim")
        .claim()
        .clone();
    write.claim_head(entry.born().clone(), freed, move |fact| ClaimHead {
        fact,
        ..next_claim
    });
    Ok(freed)
}

fn finish_release(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    task: &TaskRef,
    mut write: JournalWrite,
    answer: wire::intent::ClaimAnswer,
    parked: Option<spice::task_git::ParkedRelease>,
) -> Outcome<CommandAnswer> {
    if let Err(error) = write.command_result(
        request(command, task.clone()),
        command.epoch(),
        answer.clone(),
    ) {
        return rollback_release(
            parked,
            store_fault("claim::free-receipt", error.to_string()),
        );
    }
    let body = CommandAnswer::Claim(Box::new(answer));
    match parked {
        Some(parked) => committed_with_task_git(bus, write, body, PreparedTaskGit::Release(parked)),
        None => committed(bus, write, body),
    }
}

pub(in crate::board_command) fn claim_free(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    task: &TaskRef,
    repository: Option<Host<'_>>,
) -> Outcome<CommandAnswer> {
    let snapshot = match validated_working_snapshot(
        bus,
        repository.map(|host| host.repository),
        command.actor().clone(),
        std::slice::from_ref(&task.task),
    ) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let Some(entry) = task_entry(&snapshot.entries, &task.task) else {
        return refusal(
            "task::unknown",
            Some(task.task.to_string()),
            "read board and name one current task handle",
        );
    };
    let transition = match release_transition(entry, command.actor()) {
        Ok(transition) => transition,
        Err(outcome) => return outcome,
    };
    let at = match current_instant() {
        Ok(at) => at,
        Err(outcome) => return outcome,
    };
    let origin = match task_origin(entry.born()) {
        Ok(origin) => origin,
        Err(outcome) => return outcome,
    };
    let next = next_action(&snapshot, &transition.post);
    let closure = match board::round::Closure::owed(&transition.post, at, snapshot_fact(&snapshot))
    {
        Ok(closure) => closure,
        Err(error) => return round_fault(error),
    };
    let ownership = if let Some(closure) = &closure {
        let post = freed_projection(
            &transition.post,
            command.actor(),
            transition.post.statement().clone(),
            closure.transition().next().clone(),
        );
        snapshot.ownership(&post)
    } else {
        snapshot.ownership(&transition.post)
    };
    let parked = match prepare_release(&snapshot, command, transition.claimed, repository, None) {
        Ok(parked) => parked,
        Err(outcome) => return outcome,
    };
    let mut write = JournalWrite::new(command.actor().clone(), origin, at, Producer::CommandDoor);
    guard(&mut write, command.expected().to_vec());
    snapshot.guard_ownership(&mut write);
    let freed = match append_release(
        &mut write,
        entry,
        &transition,
        command.actor(),
        parked.as_ref(),
    ) {
        Ok(freed) => freed,
        Err(outcome) => return rollback_release(parked, outcome),
    };
    if let Some(retraction) = transition.retraction
        && let Err(error) = retraction.append(&mut write, FactCause::Pending(freed))
    {
        return rollback_release(
            parked,
            store_fault("completion::target-retract-build", error.to_string()),
        );
    }
    if let Some(closure) = closure
        && let Err(error) = closure.append(&mut write, None, FactCause::Pending(freed))
    {
        return rollback_release(
            parked,
            store_fault("completion::round-completion-build", error.to_string()),
        );
    }
    let answer = wire::intent::ClaimAnswer {
        task: entry.handle().clone(),
        ownership: Some(ownership),
        statement: None,
        holder: None,
        acknowledged: None,
        held: false,
        warnings: Box::default(),
        creation_context: None,
        origin_context: None,
        room_context: None,
        next,
        phase: None,
        nothing_stolen: false,
        transplant: None,
    };
    finish_release(bus, command, task, write, answer, parked)
}
