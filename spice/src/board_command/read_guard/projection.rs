//! Task-row projection from one immutable journal snapshot.

use super::*;

/// Project every selected task row from one snapshot.
///
/// The fact log is read once here, so each row is a lookup rather than
/// another pass.
pub(super) fn project_task_rows(
    observed: &bus::TaskSnapshot,
    residency: &wire::ResidencyHead,
    selected: Option<&std::collections::BTreeSet<wire::TaskStamp>>,
) -> Result<(Vec<Entry>, Vec<BoardTask>), Outcome<CommandAnswer>> {
    let (origin_threads, creator_teams, (claim_contexts, git_contexts)) = match selected {
        Some(selected) if selected.is_empty() => {
            (Default::default(), Default::default(), Default::default())
        }
        Some(selected) => (
            selected_task_origin_threads(observed, residency, selected)?,
            selected_task_creator_teams(observed, selected)?,
            selected_task_contexts(observed, selected),
        ),
        None => (
            task_origin_threads(observed.facts().iter(), residency, None)?,
            task_creator_teams(observed.facts().iter(), None)?,
            task_contexts(observed.facts(), None),
        ),
    };
    let projection = TaskProjection {
        facts: observed.facts(),
        origin_threads: &origin_threads,
        creator_teams: &creator_teams,
        claim_contexts: &claim_contexts,
        git_contexts: &git_contexts,
    };
    let capacity = selected.map_or(observed.tasks().len(), std::collections::BTreeSet::len);
    let mut tasks = Vec::with_capacity(capacity);
    let mut entries = Vec::with_capacity(capacity);
    let mut project = |heads: &journal::TaskHeads| -> Result<(), Outcome<CommandAnswer>> {
        let (entry, task) = projection.project(heads)?;
        entries.push(entry);
        tasks.push(task);
        Ok(())
    };
    if let Some(selected) = selected {
        for task in selected {
            if let Some(heads) = observed.task_heads(task) {
                project(heads)?;
            }
        }
    } else {
        for heads in observed.tasks() {
            project(heads)?;
        }
    }
    let handles = entries
        .iter()
        .map(|entry| (entry.born().clone(), entry.handle().clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let controlled = board::controlled_pending_tasks(&entries);
    for (entry, task) in entries.iter().zip(&mut tasks) {
        if let Some(children) = controlled.get(entry.born()) {
            task.controlled = children
                .iter()
                .map(|child| {
                    handles
                        .get(child)
                        .expect("a controlled task is in its controller's working projection")
                        .clone()
                })
                .collect();
        }
    }
    Ok((entries, tasks))
}
