//! Replay-visible ordinary execution dependencies.

use std::collections::BTreeMap;

use wire::{AnyFact, TaskStamp};

use super::Authority;

/// Exact dependency lists from immutable births and edits.
#[derive(Default)]
pub(super) struct ReplayDependencies {
    edges: BTreeMap<String, Vec<TaskStamp>>,
}

impl ReplayDependencies {
    pub(super) fn get(&self, task: &TaskStamp) -> Option<Vec<TaskStamp>> {
        self.edges.get(task.as_str()).cloned()
    }

    pub(super) fn set(&mut self, task: &TaskStamp, dependencies: Vec<TaskStamp>) {
        self.edges.insert(task.to_string(), dependencies);
    }

    /// Retain only dependencies explicitly authored at birth.
    pub(super) fn birth(&mut self, added: &wire::body::TaskAdded) {
        self.set(&added.task, added.depends.clone());
    }

    /// Replace the exact authored dependency list.
    pub(super) fn edit(&mut self, task: &TaskStamp, dependencies: Vec<TaskStamp>) {
        self.set(task, dependencies);
    }
}

/// Fold one task birth or dependency edit into `authority`'s edges.
pub(super) fn track(authority: &mut impl Authority, fact: &AnyFact) {
    if let Some(added) = fact.read::<wire::body::TaskAdded>() {
        authority.set_task_birth_dependencies(&added);
        return;
    }
    if let Some(edited) = fact.read::<wire::body::TaskEdited>()
        && let Some(dependencies) = edited.dependencies
    {
        authority.set_task_edit_dependencies(&edited.task, dependencies);
    }
}
