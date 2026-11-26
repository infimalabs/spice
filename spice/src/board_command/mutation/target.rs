use super::*;

/// Set how many distinct lanes complete one flow phase of a pending task.
///
/// Only the operator sets explicit targets. Invitation transactions grow a
/// full phase only together with the partner claim that fills the new slot.
/// Target edits open no round or generation; a satisfied round closes as owed.
pub(in crate::board_command) fn task_target(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    target: &TaskTarget,
    repository: Option<Host<'_>>,
) -> Outcome<CommandAnswer> {
    let snapshot = match validated_working_snapshot(
        bus,
        repository.map(|host| host.repository),
        command.actor().clone(),
        std::slice::from_ref(&target.task),
    ) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let Some(entry) = task_entry(&snapshot.entries, &target.task) else {
        return refusal(
            "task::unknown",
            Some(target.task.to_string()),
            "read board and name one current task handle",
        );
    };
    if let Err(outcome) = admit_target(command, entry, target) {
        return outcome;
    }
    let next = next_action(&snapshot, entry);
    let at = match current_instant() {
        Ok(at) => at,
        Err(outcome) => return outcome,
    };
    let origin = match task_origin(entry.born()) {
        Ok(origin) => origin,
        Err(outcome) => return outcome,
    };
    let mut completion = entry.completion().clone();
    completion.ask(target.phase, target.target);
    let post = entry.clone().with_completion(completion.clone());
    let closure = match board::round::Closure::owed(&post, at, progress::snapshot_fact(&snapshot)) {
        Ok(closure) => closure,
        Err(error) => return progress::round_fault(error),
    };
    let ownership = snapshot.ownership_in_phase(
        &post,
        closure.as_ref().map_or(&post.workflow().phase, |closure| {
            &closure.transition().next().phase
        }),
    );
    let mut write = JournalWrite::new(command.actor().clone(), origin, at, Producer::CommandDoor);
    guard(&mut write, command.expected().to_vec());
    snapshot.guard_ownership(&mut write);
    let targeted = match write.fact(
        body::CompletionTargeted {
            task: entry.born().clone(),
            phase: target.phase,
            target: target.target,
        },
        FactCause::Root,
    ) {
        Ok(targeted) => targeted,
        Err(error) => return store_fault("completion::target-build", error.to_string()),
    };
    write.completion_head(entry.born().clone(), targeted, move |_| completion);
    if let Some(closure) = closure
        && let Err(error) = closure.append(&mut write, None, FactCause::Pending(targeted))
    {
        return store_fault("completion::round-completion-build", error.to_string());
    }
    let answer = wire::intent::TaskAnswer {
        diagnostic: None,
        duplicates: Vec::new(),
        duplicates_omitted: 0,
        ownership: Some(ownership),
        placement: None,
        created: Default::default(),
        taste: None,
        completion_contract: None,
        task: entry.handle().clone(),
        status: "targeted".to_owned(),
        completion: None,
        claim: None,
        closed: Vec::new(),
        next,
    };
    if let Err(error) = write.command_result(
        request(command, target.clone()),
        command.epoch(),
        answer.clone(),
    ) {
        return store_fault("completion::target-receipt", error.to_string());
    }
    committed(bus, write, CommandAnswer::Task(Box::new(answer)))
}

/// Refuse a target the task cannot take or the actor may not set.
fn admit_target(
    command: &wire::AdmittedCommand,
    entry: &Entry,
    target: &TaskTarget,
) -> Result<(), Outcome<CommandAnswer>> {
    let workflow = entry.workflow();
    if !workflow.status.is_pending() {
        return Err(refusal(
            "task::settled",
            Some(entry.handle().to_string()),
            "set a completion target only on pending work",
        ));
    }
    if !workflow.flow.contains(&target.phase) {
        return Err(refusal(
            "completion::phase-outside-flow",
            Some(format!("{} {}", entry.handle(), target.phase)),
            "name a phase of the task's flow",
        ));
    }
    let current = entry.completion().target(&target.phase);
    if target.target == current {
        return Err(refusal(
            "completion::target-unchanged",
            Some(format!("{} {}={current}", entry.handle(), target.phase)),
            "name a target that differs from the phase's current one",
        ));
    }
    if command.holds(wire::principal::PrincipalRole::Operator) {
        return Ok(());
    }
    Err(refusal(
        "completion::target-unauthorized",
        Some(command.actor().to_string()),
        "only the operator sets explicit completion targets; invite a partner for atomic staffing growth",
    ))
}
