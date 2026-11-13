use super::*;

/// Return the retained reply for a completion this actor already landed.
///
/// The terminal fact authenticates the operation and actor; every completion
/// in the current lineage has one retained command reply preserving the exact
/// ownership and placement decision.
pub(super) fn replay_task_completion(
    bus: &Bus,
    facts: &bus::TaskFacts,
    command: &wire::AdmittedCommand,
    done: &TaskDone,
    entry: &Entry,
    operation: wire::CanonicalUuid,
) -> Option<Outcome<CommandAnswer>> {
    let terminal = facts.completion_terminal(entry.born(), operation)?;
    let envelope = facts.at(terminal)?;
    let matches = if let Some(arrived) = envelope.fact.read::<body::TaskArrived>() {
        arrived.validation == done.validation
            && arrived.operation == Some(operation)
            && arrived.disposition == done.disposition
    } else {
        let completed = envelope.fact.read::<body::TaskDone>()?;
        completed.terminal.is_some()
            && completed.validation == done.validation
            && completed.disposition == done.disposition
    };
    if envelope.actor != *command.actor() || !matches {
        return None;
    }
    retained_operation_reply(bus, terminal, CommandKind::TaskDone, command.actor()).or_else(|| {
        Some(store_fault(
            "command::operation-reply-missing",
            "a current completion terminal has no retained command reply",
        ))
    })
}

pub(super) fn retained_operation_reply(
    bus: &Bus,
    terminal: wire::Seq,
    kind: CommandKind,
    actor: &Actor,
) -> Option<Outcome<CommandAnswer>> {
    let actor = actor.clone();
    match bus.read(move |read| read.command_reply_containing(terminal, kind, &actor)) {
        Ok(Some(reply)) => Some(Outcome::Reply(reply)),
        Ok(None) => None,
        Err(error) => Some(store_fault("command::operation-receipt", error.to_string())),
    }
}
