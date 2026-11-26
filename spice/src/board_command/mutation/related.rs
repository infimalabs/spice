use std::collections::BTreeSet;

use super::*;

/// The distinct live tasks `related` names that are not yet related to
/// `task`, in the order first named. A deleted task is as unknown as an
/// absent one.
pub(super) fn admitted_relations(
    bus: &Bus,
    task: &wire::TaskStamp,
    related: &[TaskHandle],
) -> Result<Vec<wire::TaskStamp>, Outcome<CommandAnswer>> {
    if related.is_empty() {
        return Ok(Vec::new());
    }
    let observed = bus
        .task_snapshot()
        .map_err(|error| store_fault("board::heads-unreadable", error.to_string()))?;
    let known = observed
        .facts()
        .related_tasks(task)
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut admitted = Vec::new();
    for handle in related {
        let born = handle
            .born()
            .expect("an admitted task handle carries a canonical task stamp");
        if born == *task {
            return Err(refusal(
                "task::related-self",
                Some(handle.to_string()),
                "name a different task with --related",
            ));
        }
        if observed
            .task_heads(&born)
            .is_none_or(|heads| heads.workflow().body().status.is_deleted())
        {
            return Err(refusal(
                "task::related-unknown",
                Some(handle.to_string()),
                "read board and name one existing task handle with --related",
            ));
        }
        if !known.contains(&born) && !admitted.contains(&born) {
            admitted.push(born);
        }
    }
    Ok(admitted)
}

/// Record `related` as declared relations of `task` in `write`.
pub(super) fn append_relations(
    write: &mut JournalWrite,
    task: &wire::TaskStamp,
    related: Vec<wire::TaskStamp>,
    cause: FactCause,
) -> Result<(), Outcome<CommandAnswer>> {
    if related.is_empty() {
        return Ok(());
    }
    write
        .fact(
            body::TaskRelated {
                task: task.clone(),
                related,
            },
            cause,
        )
        .map(|_| ())
        .map_err(|error| store_fault("task::related-build", error.to_string()))
}
