//! Live-allocation frontier and dependency standing for task snapshots.

use std::collections::HashMap;
use std::sync::Arc;

use journal::GuardError;

use super::{AllocationFault, DependencyStanding, TaskSnapshot};

pub(super) struct AllocationIndex {
    pub(super) frontier: Arc<[journal::TaskHeads]>,
    pub(super) references: Arc<HashMap<journal::crossing::TaskStamp, usize>>,
    pub(super) standing: Arc<HashMap<journal::crossing::TaskStamp, DependencyStanding>>,
    pub(super) faults: Arc<[AllocationFault]>,
}

pub(super) fn rebuild(tasks: &[journal::TaskHeads]) -> Result<AllocationIndex, GuardError> {
    let mut frontier = Vec::new();
    let mut references = HashMap::new();
    let mut faults = Vec::new();
    let mut prior = None;
    for heads in tasks {
        let task = heads.statement().body().born.clone();
        if prior.as_ref().is_some_and(|prior| prior >= &task) {
            return Err(GuardError::Invalid(
                "task snapshot heads are not in unique task-stamp order".to_owned(),
            ));
        }
        prior = Some(task.clone());
        match task_standing(heads) {
            DependencyStanding::Open => {
                add_dependencies(&mut references, allocation_references(heads));
                frontier.push(heads.clone());
            }
            DependencyStanding::Completed
            | DependencyStanding::Closed
            | DependencyStanding::Deleted => {
                if heads.actively_claimed() {
                    faults.push(AllocationFault::ActiveTerminal { task });
                }
            }
        }
    }
    let mut standing = HashMap::with_capacity(references.len());
    for heads in tasks {
        let task = &heads.statement().body().born;
        if references.contains_key(task) {
            standing.insert(task.clone(), task_standing(heads));
        }
    }
    Ok(AllocationIndex {
        frontier: Arc::from(frontier),
        references: Arc::new(references),
        standing: Arc::new(standing),
        faults: Arc::from(faults),
    })
}

pub(super) fn advance(
    read: &journal::JournalRead<'_>,
    prior: &TaskSnapshot,
    moved: &[journal::TaskHeads],
) -> Result<AllocationIndex, GuardError> {
    if moved.is_empty() {
        return Ok(AllocationIndex {
            frontier: Arc::clone(&prior.allocation_frontier),
            references: Arc::clone(&prior.dependency_references),
            standing: Arc::clone(&prior.dependency_standing),
            faults: Arc::clone(&prior.allocation_faults),
        });
    }
    let mut frontier = prior.allocation_frontier.to_vec();
    let mut references = (*prior.dependency_references).clone();
    let mut standing = (*prior.dependency_standing).clone();
    let mut faults = prior.allocation_faults.to_vec();
    let mut resolve = std::collections::HashSet::new();
    for heads in moved {
        let task = heads.statement().body().born.clone();
        faults.retain(|fault| fault.task() != &task);
        if let Ok(index) = frontier.binary_search_by(|kept| kept.statement().body().born.cmp(&task))
        {
            let old = frontier.remove(index);
            remove_dependencies(&mut references, &mut standing, allocation_references(&old));
        }
        match task_standing(heads) {
            DependencyStanding::Open => {
                for dependency in allocation_references(heads) {
                    if references.get(dependency).copied().unwrap_or(0) == 0 {
                        resolve.insert(dependency.clone());
                    }
                }
                add_dependencies(&mut references, allocation_references(heads));
                let index = frontier
                    .binary_search_by(|kept| kept.statement().body().born.cmp(&task))
                    .unwrap_or_else(|index| index);
                frontier.insert(index, heads.clone());
            }
            DependencyStanding::Completed
            | DependencyStanding::Closed
            | DependencyStanding::Deleted => {
                if heads.actively_claimed() {
                    faults.push(AllocationFault::ActiveTerminal { task: task.clone() });
                }
            }
        }
        if references.contains_key(&task) {
            resolve.insert(task);
        }
    }
    faults.sort_by(|left, right| left.task().cmp(right.task()));
    for task in resolve {
        if !references.contains_key(&task) {
            standing.remove(&task);
            continue;
        }
        let heads = match moved.binary_search_by(|heads| heads.statement().body().born.cmp(&task)) {
            Ok(index) => Some(moved[index].clone()),
            Err(_) => read.task_heads_for(&task)?,
        };
        match heads {
            Some(heads) => {
                standing.insert(task, task_standing(&heads));
            }
            None => {
                standing.remove(&task);
            }
        }
    }
    Ok(AllocationIndex {
        frontier: Arc::from(frontier),
        references: Arc::new(references),
        standing: Arc::new(standing),
        faults: Arc::from(faults),
    })
}

fn allocation_references(
    heads: &journal::TaskHeads,
) -> impl Iterator<Item = &journal::crossing::TaskStamp> {
    let statement = heads.statement().body();
    statement.dependencies.iter().chain(
        statement
            .preparation
            .iter()
            .flat_map(|preparation| [&preparation.parent, &preparation.controller]),
    )
}

fn add_dependencies<'a>(
    references: &mut HashMap<journal::crossing::TaskStamp, usize>,
    dependencies: impl Iterator<Item = &'a journal::crossing::TaskStamp>,
) {
    for dependency in dependencies {
        *references.entry(dependency.clone()).or_default() += 1;
    }
}

fn remove_dependencies<'a>(
    references: &mut HashMap<journal::crossing::TaskStamp, usize>,
    standing: &mut HashMap<journal::crossing::TaskStamp, DependencyStanding>,
    dependencies: impl Iterator<Item = &'a journal::crossing::TaskStamp>,
) {
    for dependency in dependencies {
        let Some(count) = references.get_mut(dependency) else {
            continue;
        };
        *count -= 1;
        if *count == 0 {
            references.remove(dependency);
            standing.remove(dependency);
        }
    }
}

fn task_standing(heads: &journal::TaskHeads) -> DependencyStanding {
    match heads.workflow().body().status.conclusion() {
        Some(journal::crossing::WorkflowConclusion::Positive { .. }) => {
            DependencyStanding::Completed
        }
        Some(journal::crossing::WorkflowConclusion::Negative(
            journal::crossing::WorkflowNegative::Closed,
        )) => DependencyStanding::Closed,
        Some(journal::crossing::WorkflowConclusion::Negative(
            journal::crossing::WorkflowNegative::Deleted,
        )) => DependencyStanding::Deleted,
        None => DependencyStanding::Open,
    }
}

impl AllocationFault {
    fn task(&self) -> &journal::crossing::TaskStamp {
        match self {
            Self::ActiveTerminal { task } => task,
        }
    }
}
