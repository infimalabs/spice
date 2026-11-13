//! Every task whose current standing needs operator intervention.
//!
//! Environment holds, dependency walls, and Designs awaiting the operator's
//! decision belong here. Normal Plan/Design sequencing, intentional deferral,
//! and claimed work remain reachable through their ordinary Work surfaces
//! without appearing in the operator waits.

use super::{DependencyWall, WorkAvailability, WorkProjection, wall::DependencyWalls};

/// One task only the operator can move, and why.
#[derive(Debug, PartialEq, Eq)]
pub struct OperatorWait<'a> {
    task: usize,
    environment: bool,
    wall: Option<DependencyWall<'a>>,
    holds: Vec<usize>,
}

impl<'a> OperatorWait<'a> {
    const fn new(task: usize) -> Self {
        Self {
            task,
            environment: false,
            wall: None,
            holds: Vec::new(),
        }
    }

    /// The waiting task's index in the projection.
    #[must_use]
    pub const fn task(&self) -> usize {
        self.task
    }

    /// Its environment needs an act only the operator can take.
    #[must_use]
    pub const fn environment(&self) -> bool {
        self.environment
    }

    /// The wall it presents to the tasks it holds when its environment does
    /// not explain it.
    #[must_use]
    pub const fn wall(&self) -> Option<DependencyWall<'a>> {
        self.wall
    }

    /// Projection indexes of the tasks the prerequisite gate holds behind
    /// this one, in board order.
    #[must_use]
    pub fn holds(&self) -> &[usize] {
        &self.holds
    }
}

/// The board's operator-waiting tasks in board order.
#[derive(Debug, PartialEq, Eq)]
pub struct OperatorWaits<'a> {
    waits: Vec<OperatorWait<'a>>,
}

impl<'a> OperatorWaits<'a> {
    #[must_use]
    pub fn new(work: &WorkProjection<'a>) -> Self {
        let tasks = work.tasks();
        let mut waits = (0..tasks.len()).map(|_| None).collect::<Vec<_>>();
        for (index, task) in tasks.iter().enumerate() {
            if task.environment_blocked() && !set_aside(task.availability()) {
                waits[index]
                    .get_or_insert_with(|| OperatorWait::new(index))
                    .environment = true;
            }
            if task.availability() == WorkAvailability::Selection {
                waits[index]
                    .get_or_insert_with(|| OperatorWait::new(index))
                    .wall = Some(DependencyWall::Selection(&task.row().statement.body.born));
            }
        }
        let mut walls = DependencyWalls::new(work);
        for (index, task) in tasks.iter().enumerate() {
            if task.availability() != WorkAvailability::Blocked {
                continue;
            }
            let mut explained = false;
            for dependency in task.dependencies() {
                let Some((at, wall)) = walls.wall(index, dependency) else {
                    continue;
                };
                explained = true;
                let wait = waits[at].get_or_insert_with(|| OperatorWait::new(at));
                match wall {
                    DependencyWall::Environment(_) => wait.environment = true,
                    _ => wait.wall = Some(wall),
                }
                if wait.holds.last() != Some(&index) {
                    wait.holds.push(index);
                }
            }
            if !explained && waits[index].is_none() {
                let mut wait = OperatorWait::new(index);
                wait.wall = Some(DependencyWall::Gate(&task.row().statement.body.born));
                waits[index] = Some(wait);
            }
        }
        Self {
            waits: waits.into_iter().flatten().collect(),
        }
    }

    #[must_use]
    pub fn waits(&self) -> &[OperatorWait<'a>] {
        &self.waits
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.waits.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.waits.is_empty()
    }
}

/// The operator already set this work aside, so it waits on nothing for its
/// own sake.
const fn set_aside(availability: WorkAvailability) -> bool {
    matches!(
        availability,
        WorkAvailability::Terminal | WorkAvailability::Draft | WorkAvailability::Deferred
    )
}

#[cfg(test)]
mod tests;
