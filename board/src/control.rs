//! Pending Draft settlement through construction lineage, never dependencies.

use std::collections::{BTreeMap, BTreeSet};

use wire::TaskStamp;

use crate::Entry;

/// Pending Draft descendants each construction parent controls.
///
/// Settlement policy decides which controller outcomes consume this closure.
#[must_use]
pub fn controlled_pending_tasks(entries: &[Entry]) -> BTreeMap<TaskStamp, Vec<TaskStamp>> {
    let gated = pending_by_gate(entries);
    let indexed = entries
        .iter()
        .map(|entry| (entry.born(), entry))
        .collect::<BTreeMap<_, _>>();
    entries
        .iter()
        .filter(|entry| gated.contains_key(entry.born()))
        .filter_map(|entry| {
            let mut tasks = gated_closure(&gated, entry.born());
            let protected = protected_branches(&gated, &indexed, entry.born(), &tasks);
            tasks.retain(|task| !protected.contains(task));
            (!tasks.is_empty()).then(|| (entry.born().clone(), tasks.into_iter().collect()))
        })
        .collect()
}

/// Pending Draft descendants of `controller`, excluding independently gated branches.
#[must_use]
pub fn controlled_pending<'a>(entries: &'a [Entry], controller: &TaskStamp) -> Vec<&'a Entry> {
    let Some(tasks) = controlled_pending_tasks(entries).remove(controller) else {
        return Vec::new();
    };
    let tasks = tasks.into_iter().collect::<BTreeSet<_>>();
    entries
        .iter()
        .filter(|entry| tasks.contains(entry.born()))
        .collect()
}

fn pending_by_gate(entries: &[Entry]) -> BTreeMap<&TaskStamp, Vec<&TaskStamp>> {
    let mut gated = BTreeMap::<&TaskStamp, Vec<&TaskStamp>>::new();
    for entry in entries
        .iter()
        .filter(|entry| entry.is_draft() && pending(entry))
    {
        if let Some(preparation) = &entry.statement().preparation {
            gated
                .entry(&preparation.parent)
                .or_default()
                .push(entry.born());
        }
    }
    gated
}

/// Every pending task `root` gates, directly or through other pending work.
fn gated_closure(
    gated: &BTreeMap<&TaskStamp, Vec<&TaskStamp>>,
    root: &TaskStamp,
) -> BTreeSet<TaskStamp> {
    let mut seen = BTreeSet::from([root]);
    let mut closure = BTreeSet::new();
    let mut frontier = vec![root];
    while let Some(gate) = frontier.pop() {
        for &task in gated.get(gate).into_iter().flatten() {
            if seen.insert(task) {
                closure.insert(task.clone());
                frontier.push(task);
            }
        }
    }
    closure
}

/// Branches with another open construction controller outside this closure.
fn protected_branches(
    gated: &BTreeMap<&TaskStamp, Vec<&TaskStamp>>,
    entries: &BTreeMap<&TaskStamp, &Entry>,
    root: &TaskStamp,
    closure: &BTreeSet<TaskStamp>,
) -> BTreeSet<TaskStamp> {
    let mut protected = BTreeSet::new();
    let inherited = entries
        .get(root)
        .and_then(|entry| entry.statement().preparation.as_ref())
        .map(|preparation| &preparation.controller);
    for task in closure {
        let Some(entry) = entries.get(task) else {
            continue;
        };
        let independently_gated =
            entry
                .statement()
                .preparation
                .as_ref()
                .is_some_and(|preparation| {
                    &preparation.controller != root
                        && inherited != Some(&preparation.controller)
                        && !closure.contains(&preparation.controller)
                        && entries
                            .get(&preparation.controller)
                            .is_some_and(|controller| pending(controller))
                });
        if independently_gated {
            protected.insert(task.clone());
            protected.extend(gated_closure(gated, task));
        }
    }
    protected
}

fn pending(entry: &Entry) -> bool {
    entry.workflow().status.is_pending()
}
