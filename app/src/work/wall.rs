//! Which unsatisfied dependencies only the operator can move.
//!
//! A dependency on work an agent can claim or is already working resolves
//! organically, so it is an ordinary wait. A lapsed claim and a pending
//! selection are transparent: the task keeps its own stale or selection
//! standing, while its underlying gate decides the dependency path, so an open
//! Design gathering input never reads as a wall. A chain is a wall only when
//! every path forward needs the operator: a deferral, a deferred Design
//! included, an internal parked row, a cycle, a prerequisite that can never
//! complete, or an environment requirement only the operator can satisfy.

use wire::TaskStamp;

use super::{WorkAvailability, WorkDependency, WorkProjection};

/// The task where an unsatisfied dependency chain stops moving, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DependencyWall<'a> {
    /// The resident found a human-actionable dependency or controller gate
    /// whose unavailable source is not represented in the open projection.
    Gate(&'a TaskStamp),
    /// The task carries a deferral.
    Deferred(&'a TaskStamp),
    /// The task remains a Draft until it is filed.
    Parked(&'a TaskStamp),
    /// The task depends on itself through the chain.
    Cycle(&'a TaskStamp),
    /// The task was closed without completing.
    Unreachable(&'a TaskStamp),
    /// The task needs an environment requirement only the operator can satisfy.
    Environment(&'a TaskStamp),
    /// The task is a Design awaiting the operator's decision on its candidate.
    Selection(&'a TaskStamp),
}

/// Memoized wall search over one projection.
///
/// A task's verdict holds regardless of the path that reached it: a cycle
/// found below a task always runs back through that task.
pub(super) struct DependencyWalls<'p, 'a> {
    work: &'p WorkProjection<'a>,
    verdicts: Vec<Option<Option<(usize, DependencyWall<'a>)>>>,
    on_path: Vec<bool>,
}

impl<'p, 'a> DependencyWalls<'p, 'a> {
    #[must_use]
    pub(super) fn new(work: &'p WorkProjection<'a>) -> Self {
        let tasks = work.tasks().len();
        Self {
            work,
            verdicts: vec![None; tasks],
            on_path: vec![false; tasks],
        }
    }

    /// The wall behind one dependency of task `dependent` with the projection
    /// index of the task where it stands, or none when the dependency is
    /// satisfied or agent work will move it.
    pub(super) fn wall(
        &mut self,
        dependent: usize,
        dependency: &WorkDependency<'a>,
    ) -> Option<(usize, DependencyWall<'a>)> {
        self.on_path[dependent] = true;
        let wall = self.dependency(dependency);
        self.on_path[dependent] = false;
        wall
    }

    fn dependency(
        &mut self,
        dependency: &WorkDependency<'a>,
    ) -> Option<(usize, DependencyWall<'a>)> {
        if dependency.satisfied() {
            return None;
        }
        let task = dependency.task()?;
        if self.on_path[task] {
            return Some((task, DependencyWall::Cycle(dependency.born())));
        }
        if let Some(verdict) = self.verdicts[task] {
            return verdict;
        }
        let verdict = self.task(task);
        self.verdicts[task] = Some(verdict);
        verdict
    }

    fn task(&mut self, index: usize) -> Option<(usize, DependencyWall<'a>)> {
        let work = self.work;
        let task = &work.tasks()[index];
        let born = &task.row().statement.body.born;
        let wall = match task.availability() {
            // A claim on work whose environment only the operator can satisfy
            // does not move it.
            WorkAvailability::InFlight | WorkAvailability::Blocked
                if task.environment_blocked() =>
            {
                DependencyWall::Environment(born)
            }
            WorkAvailability::InFlight
            | WorkAvailability::Ready
            | WorkAvailability::Review
            | WorkAvailability::Passive
            | WorkAvailability::Waiting => return None,
            WorkAvailability::Stale | WorkAvailability::Selection => {
                return self.underlying(index);
            }
            WorkAvailability::Held => DependencyWall::Environment(born),
            WorkAvailability::Deferred => DependencyWall::Deferred(born),
            WorkAvailability::Draft => DependencyWall::Parked(born),
            WorkAvailability::Terminal => DependencyWall::Unreachable(born),
            WorkAvailability::Blocked => {
                self.on_path[index] = true;
                let wall = task
                    .dependencies()
                    .iter()
                    .find_map(|dependency| self.dependency(dependency));
                self.on_path[index] = false;
                return wall;
            }
        };
        Some((index, wall))
    }

    /// A lapsed claim or operator selection changes only the
    /// row's own standing. Follow the task's underlying gate so its dependents
    /// see the same wall they would if that standing were absent.
    fn underlying(&mut self, index: usize) -> Option<(usize, DependencyWall<'a>)> {
        let work = self.work;
        let task = &work.tasks()[index];
        let born = &task.row().statement.body.born;
        if task.row().workflow.body.deferral.is_some() {
            return Some((index, DependencyWall::Deferred(born)));
        }
        if task.environment_blocked() {
            return Some((index, DependencyWall::Environment(born)));
        }
        self.on_path[index] = true;
        let wall = task.dependencies().iter().find_map(|dependency| {
            if !dependency.satisfied() && dependency.task().is_none() {
                Some((index, DependencyWall::Gate(dependency.born())))
            } else {
                self.dependency(dependency)
            }
        });
        self.on_path[index] = false;
        wall
    }
}

#[cfg(test)]
mod tests;
