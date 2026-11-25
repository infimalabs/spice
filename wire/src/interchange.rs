//! Portable authored task sets.
//!
//! This schema carries only creation intent that another repository can admit
//! under fresh authority. Journal positions, durable task identities, claims,
//! workflow progress, routing evidence, and residency never cross this boundary.

use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap};

use serde::{Deserialize, Serialize};

use crate::{EnvironmentRequirements, Phase, TaskIntent, TopicInput};

/// The sole current portable task-set schema.
pub const TASK_SET_VERSION: u16 = 1;
/// Most task definitions one portable set may carry.
pub const TASK_SET_MAX_TASKS: usize = 16_384;
/// Most encoded bytes one portable set may carry.
pub const TASK_SET_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// One complete portable task-set document.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSet {
    /// Exact schema version.
    pub version: u16,
    /// Definitions in canonical local-id order.
    pub tasks: Vec<TaskSetTask>,
}

/// Re-creatable authored intent for one task.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSetTask {
    /// One-based identity local to this document.
    pub id: u32,
    /// One-line statement of the work.
    pub title: String,
    /// One plain sentence saying who can do what once the task lands and why.
    pub pitch: String,
    /// Exact public destination, or an unresolved relative tail.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub topic: Option<TopicInput>,
    /// Longer authored context.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub description: Option<String>,
    /// Criteria that decide whether the work is done.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub term: Vec<String>,
    /// Typed execution-environment requirements.
    #[serde(
        default,
        skip_serializing_if = "EnvironmentRequirements::is_empty",
        deserialize_with = "EnvironmentRequirements::present"
    )]
    pub requirements: EnvironmentRequirements,
    /// Complete authored route, including draft at index zero.
    pub flow: Vec<Phase>,
    /// Local identities this task must follow.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends: Vec<u32>,
    /// Declared Plan or Design intent.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub intent: Option<TaskIntent>,
    /// Local planning task whose context this task prepares within.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub preparation: Option<u32>,
    /// Preserve an author-held draft rather than submitting it at creation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub draft: bool,
}

impl TaskSet {
    /// Validate the complete document and return task indexes in deterministic
    /// creation order. Every referenced task precedes its dependents.
    ///
    /// # Errors
    ///
    /// Refuses another schema version, an oversized set, non-canonical local
    /// identities, private topics, malformed flows, missing or duplicate
    /// references, self-reference, and cycles.
    pub fn creation_order(&self) -> Result<Vec<usize>, TaskSetError> {
        if self.version != TASK_SET_VERSION {
            return Err(TaskSetError::Version {
                found: self.version,
                current: TASK_SET_VERSION,
            });
        }
        if self.tasks.len() > TASK_SET_MAX_TASKS {
            return Err(TaskSetError::TooManyTasks {
                found: self.tasks.len(),
                maximum: TASK_SET_MAX_TASKS,
            });
        }

        let (children, mut incoming) = self.reference_graph()?;

        let mut ready = incoming
            .iter()
            .enumerate()
            .filter_map(|(index, count)| (*count == 0).then_some(Reverse(index)))
            .collect::<BinaryHeap<_>>();
        let mut order = Vec::with_capacity(self.tasks.len());
        while let Some(Reverse(index)) = ready.pop() {
            order.push(index);
            for child in children[index].iter().copied() {
                incoming[child] -= 1;
                if incoming[child] == 0 {
                    ready.push(Reverse(child));
                }
            }
        }
        if order.len() != self.tasks.len() {
            return Err(TaskSetError::Cycle);
        }
        let mut advances = vec![false; self.tasks.len()];
        for index in order.iter().copied() {
            let task = &self.tasks[index];
            advances[index] = !task.draft
                && !task.term.is_empty()
                && task.depends.iter().all(|parent| {
                    let parent =
                        usize::try_from(*parent - 1).expect("validated task-set ids fit usize");
                    advances[parent]
                });
            if let Some(parent) = task.preparation {
                let parent_index =
                    usize::try_from(parent - 1).expect("validated task-set ids fit usize");
                if !advances[parent_index] || !planning_context(&self.tasks[parent_index]) {
                    return Err(TaskSetError::InactivePreparation {
                        task: task.id,
                        parent,
                    });
                }
            }
        }
        Ok(order)
    }

    fn reference_graph(&self) -> Result<(Vec<Vec<usize>>, Vec<usize>), TaskSetError> {
        let mut children = vec![Vec::new(); self.tasks.len()];
        let mut incoming = vec![0_usize; self.tasks.len()];
        for (index, task) in self.tasks.iter().enumerate() {
            let expected = u32::try_from(index + 1).expect("task-set bound fits u32");
            if task.id != expected {
                return Err(TaskSetError::NonCanonicalId {
                    found: task.id,
                    expected,
                });
            }
            let phases = task.flow.iter().collect::<BTreeSet<_>>();
            if task.flow.len() < 2
                || task.flow.first() != Some(&Phase::Draft)
                || phases.len() != task.flow.len()
            {
                return Err(TaskSetError::InvalidFlow { task: task.id });
            }
            if matches!(task.topic, Some(TopicInput::Private(_))) {
                return Err(TaskSetError::PrivateTopic { task: task.id });
            }
            if task
                .topic
                .as_ref()
                .and_then(|topic| match topic {
                    TopicInput::Absolute(topic) => Some(topic),
                    TopicInput::Tail(_) | TopicInput::Private(_) => None,
                })
                .is_some_and(crate::is_oops_topic)
            {
                return Err(TaskSetError::ReservedTopic { task: task.id });
            }
            if task.intent == Some(TaskIntent::OpportunisticDesign) {
                return Err(TaskSetError::InternalIntent { task: task.id });
            }
            if task
                .intent
                .is_some_and(|intent| !intent.admits_flow(&task.flow))
            {
                return Err(TaskSetError::InvalidFlow { task: task.id });
            }

            let mut parents = BTreeSet::new();
            for parent in task.depends.iter().copied().chain(task.preparation) {
                if parent == task.id {
                    return Err(TaskSetError::SelfReference { task: task.id });
                }
                let Some(parent_index) = parent
                    .checked_sub(1)
                    .and_then(|id| usize::try_from(id).ok())
                else {
                    return Err(TaskSetError::MissingReference {
                        task: task.id,
                        reference: parent,
                    });
                };
                if parent_index >= self.tasks.len() {
                    return Err(TaskSetError::MissingReference {
                        task: task.id,
                        reference: parent,
                    });
                }
                if !parents.insert(parent_index) {
                    return Err(TaskSetError::DuplicateReference {
                        task: task.id,
                        reference: parent,
                    });
                }
            }
            incoming[index] = parents.len();
            for parent in parents {
                children[parent].push(index);
            }
        }
        Ok((children, incoming))
    }
}

fn planning_context(task: &TaskSetTask) -> bool {
    task.intent.is_some()
        || (task.intent.is_none()
            && task.flow.contains(&Phase::Plan)
            && task
                .flow
                .iter()
                .all(|phase| matches!(phase, Phase::Draft | Phase::Plan | Phase::Review)))
        || task
            .flow
            .get(1)
            .is_some_and(|phase| matches!(phase, Phase::Plan | Phase::Design))
}

/// Why a portable task set cannot be imported.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TaskSetError {
    /// The document names another schema.
    #[error("task-set schema {found} is unsupported; current schema is {current}")]
    Version { found: u16, current: u16 },
    /// The document exceeds its task-count bound.
    #[error("task set carries {found} tasks; maximum is {maximum}")]
    TooManyTasks { found: usize, maximum: usize },
    /// IDs must be exactly one through the task count.
    #[error("task-set id {found} is non-canonical; expected {expected}")]
    NonCanonicalId { found: u32, expected: u32 },
    /// Private ownership cannot cross repositories.
    #[error("task {task} names a private topic, which is not portable")]
    PrivateTopic { task: u32 },
    /// Internal recovery topics are generated locally, never imported.
    #[error("task {task} names the reserved OOPS topic")]
    ReservedTopic { task: u32 },
    /// Opportunistic-design intent is generated locally, never imported.
    #[error("task {task} names internal opportunistic-design intent")]
    InternalIntent { task: u32 },
    /// A creation route must start with draft and contain another phase.
    #[error("task {task} has an invalid creation flow")]
    InvalidFlow { task: u32 },
    /// A local reference does not name this document.
    #[error("task {task} references missing local task {reference}")]
    MissingReference { task: u32, reference: u32 },
    /// One edge was named twice, including across dependency and preparation.
    #[error("task {task} repeats local reference {reference}")]
    DuplicateReference { task: u32, reference: u32 },
    /// A task cannot wait for or prepare within itself.
    #[error("task {task} references itself")]
    SelfReference { task: u32 },
    /// A preparation cannot bind to a planning parent that will remain draft.
    #[error("task {task} preparation parent {parent} cannot activate during import")]
    InactivePreparation { task: u32, parent: u32 },
    /// The local dependency/preparation graph cannot be created.
    #[error("task-set dependency and preparation graph contains a cycle")]
    Cycle,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: u32, depends: &[u32]) -> TaskSetTask {
        TaskSetTask {
            id,
            title: format!("task {id}"),
            pitch: format!("task {id} matters"),
            topic: Some("/portable".parse().unwrap()),
            description: None,
            term: Vec::new(),
            requirements: EnvironmentRequirements::default(),
            flow: vec![Phase::Draft, Phase::Todo],
            depends: depends.to_vec(),
            intent: None,
            preparation: None,
            draft: false,
        }
    }

    #[test]
    fn creation_order_places_references_before_dependents() {
        let set = TaskSet {
            version: TASK_SET_VERSION,
            tasks: vec![task(1, &[3]), task(2, &[1]), task(3, &[])],
        };
        assert_eq!(set.creation_order().unwrap(), vec![2, 0, 1]);
    }

    #[test]
    fn cycles_are_refused_before_creation() {
        let set = TaskSet {
            version: TASK_SET_VERSION,
            tasks: vec![task(1, &[2]), task(2, &[1])],
        };
        assert_eq!(set.creation_order(), Err(TaskSetError::Cycle));
    }

    #[test]
    fn intent_and_flow_are_validated_before_creation() {
        let mut invalid = task(2, &[]);
        invalid.intent = Some(TaskIntent::Design);
        invalid.flow = vec![Phase::Draft, Phase::Todo];
        let set = TaskSet {
            version: TASK_SET_VERSION,
            tasks: vec![task(1, &[]), invalid],
        };
        assert_eq!(
            set.creation_order(),
            Err(TaskSetError::InvalidFlow { task: 2 })
        );
    }

    #[test]
    fn preparation_requires_a_parent_that_can_activate() {
        let mut parent = task(1, &[]);
        parent.intent = Some(TaskIntent::Plan);
        parent.flow = vec![Phase::Draft, Phase::Plan, Phase::Review];
        let mut child = task(2, &[]);
        child.preparation = Some(1);
        let mut set = TaskSet {
            version: TASK_SET_VERSION,
            tasks: vec![parent, child],
        };
        assert_eq!(
            set.creation_order(),
            Err(TaskSetError::InactivePreparation { task: 2, parent: 1 })
        );

        set.tasks[0]
            .term
            .push("The plan has acceptance.".to_owned());
        assert_eq!(set.creation_order().unwrap(), vec![0, 1]);
    }
}
