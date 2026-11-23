use super::*;

/// Project the live board and only the settled rows reachable from it or the
/// command's named roots.
///
/// Mutations still decide against the complete working graph, but their cost
/// does not grow with unrelated terminal history.
pub(in crate::board_command) fn validated_working_snapshot(
    bus: &Bus,
    repository: Option<&Repository>,
    principal: wire::Actor,
    roots: &[wire::TaskHandle],
) -> Result<ValidatedSnapshot, Outcome<CommandAnswer>> {
    let roots = roots
        .iter()
        .map(|handle| {
            handle
                .born()
                .expect("an admitted task handle carries a canonical task stamp")
        })
        .collect::<Vec<_>>();
    validated_working_snapshot_for_tasks(bus, repository, principal, &roots)
}

pub(in crate::board_command) fn command_task_snapshot(
    bus: &Bus,
    repository: Option<&Repository>,
    command: &wire::AdmittedCommand,
    task: &wire::TaskHandle,
) -> Result<ValidatedSnapshot, Outcome<CommandAnswer>> {
    validated_working_snapshot(
        bus,
        repository,
        command.actor().clone(),
        std::slice::from_ref(task),
    )
}

/// Project only the causal graph for a creation by an intrinsic actor. Such an
/// actor cannot hold a claim, so unrelated live tasks cannot decide its origin
/// or next action. Lane creation retains the complete working graph.
pub(in crate::board_command) fn validated_task_creation_snapshot_for_tasks(
    bus: &Bus,
    repository: Option<&Repository>,
    principal: wire::Actor,
    roots: &[wire::TaskStamp],
) -> Result<ValidatedSnapshot, Outcome<CommandAnswer>> {
    let complete_working_graph = matches!(&principal, wire::Actor::Lane(_));
    validated_snapshot_for_tasks(bus, repository, principal, roots, complete_working_graph)
}

pub(in crate::board_command) fn validated_working_snapshot_for_tasks(
    bus: &Bus,
    repository: Option<&Repository>,
    principal: wire::Actor,
    roots: &[wire::TaskStamp],
) -> Result<ValidatedSnapshot, Outcome<CommandAnswer>> {
    validated_snapshot_for_tasks(bus, repository, principal, roots, true)
}

fn validated_snapshot_for_tasks(
    bus: &Bus,
    repository: Option<&Repository>,
    principal: wire::Actor,
    roots: &[wire::TaskStamp],
    complete_working_graph: bool,
) -> Result<ValidatedSnapshot, Outcome<CommandAnswer>> {
    let observed = bus
        .task_snapshot()
        .map_err(|error| store_fault("board::heads-unreadable", error.to_string()))?;
    let selected = if complete_working_graph {
        working_task_selection(&observed, roots)
    } else {
        expand_task_selection(&observed, roots.to_vec())
    };
    validated_snapshot_from_with_occupant(
        &observed,
        repository,
        principal,
        None,
        Some(&selected),
        true,
    )
}

fn working_task_selection(
    observed: &bus::TaskSnapshot,
    roots: &[wire::TaskStamp],
) -> std::collections::BTreeSet<wire::TaskStamp> {
    let pending = observed
        .allocation_frontier()
        .map(|heads| heads.statement().body().born.clone())
        .chain(roots.iter().cloned())
        .collect::<Vec<_>>();
    expand_task_selection(observed, pending)
}

pub(in crate::board_command) fn expand_task_selection(
    observed: &bus::TaskSnapshot,
    mut pending: Vec<wire::TaskStamp>,
) -> std::collections::BTreeSet<wire::TaskStamp> {
    let mut selected = std::collections::BTreeSet::new();
    while let Some(task) = pending.pop() {
        if !selected.insert(task.clone()) {
            continue;
        }
        if let Some(heads) = observed.task_heads(&task) {
            pending.extend(heads.statement().body().dependencies.iter().cloned());
            if let Some(preparation) = &heads.statement().body().preparation {
                pending.extend([
                    preparation.parent.clone(),
                    preparation.controller.clone(),
                    preparation.construction.clone(),
                ]);
            }
        }
    }
    selected
}

pub(in crate::board_command) fn validated_snapshot_for_automatic(
    observed: &bus::TaskSnapshot,
    repository: Option<&Repository>,
    principal: wire::Actor,
    occupant: wire::CanonicalUuid,
) -> Result<ValidatedSnapshot, Outcome<CommandAnswer>> {
    let selected = working_task_selection(observed, &[]);
    validated_snapshot_from_with_occupant(
        observed,
        repository,
        principal,
        Some(occupant),
        Some(&selected),
        true,
    )
}
