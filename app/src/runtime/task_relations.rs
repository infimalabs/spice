//! Capability-scoped, request-keyed task relationship enrichment.

use std::collections::{BTreeMap, BTreeSet};

use wire::intent::{
    TASK_RELATION_DEPTH_LIMIT, TASK_RELATION_NODE_LIMIT, TASK_RELATION_ROOT_LIMIT,
    TaskRelationKind, TaskRelationNodeRef, TaskRelations, TaskRelationsAnswer,
};
use wire::{FactKind, Response};

use super::{AppRuntime, ReadState};
use crate::socket::Admitted;

/// Exact identity of one bounded optional relationship enrichment read.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskRelationsKey {
    roots: Vec<wire::TaskStamp>,
    relation: TaskRelationKind,
    controllers: bool,
    depth: u8,
    node_limit: u16,
}

impl TaskRelationsKey {
    #[must_use]
    pub fn new(
        mut roots: Vec<wire::TaskStamp>,
        relation: TaskRelationKind,
        controllers: bool,
        depth: u8,
        node_limit: u16,
    ) -> Option<Self> {
        roots.sort();
        roots.dedup();
        (!roots.is_empty()
            && roots.len() <= TASK_RELATION_ROOT_LIMIT
            && depth <= TASK_RELATION_DEPTH_LIMIT
            && usize::from(node_limit) >= roots.len()
            && node_limit <= TASK_RELATION_NODE_LIMIT)
            .then_some(Self {
                roots,
                relation,
                controllers,
                depth,
                node_limit,
            })
    }

    #[must_use]
    pub fn request(&self) -> TaskRelations {
        let mut relations = vec![self.relation];
        if self.controllers && self.relation != TaskRelationKind::Controller {
            relations.push(TaskRelationKind::Controller);
        }
        TaskRelations {
            roots: self.roots.clone(),
            relations,
            depth: self.depth,
            node_limit: self.node_limit,
        }
    }

    #[must_use]
    pub fn roots(&self) -> &[wire::TaskStamp] {
        &self.roots
    }

    #[must_use]
    pub const fn relation(&self) -> TaskRelationKind {
        self.relation
    }

    fn includes(&self, relation: TaskRelationKind) -> bool {
        self.relation == relation
            || (self.controllers && matches!(relation, TaskRelationKind::Controller))
    }
}

/// Capability-scoped state for one request-identity-keyed enrichment read.
pub type TaskRelationsState = ReadState<Box<TaskRelationsAnswer>>;

#[derive(Default, PartialEq, Eq)]
pub(super) struct Cache {
    states: BTreeMap<TaskRelationsKey, TaskRelationsState>,
    revision: u64,
}

impl Cache {
    pub(super) fn reconnect(&mut self) {
        self.states.retain(|_, state| refresh_state(state, false));
        self.revision = self.revision.saturating_add(1);
    }

    #[cfg(any(target_arch = "wasm32", test, rust_analyzer))]
    fn invalidate(&mut self, changes: &[TaskRelationInvalidation]) {
        let mut invalidated = false;
        self.states.retain(|key, state| {
            if !changes.iter().any(|change| change.affects(key, state)) {
                return true;
            }
            invalidated = true;
            refresh_state(state, true)
        });
        if invalidated {
            self.revision = self.revision.saturating_add(1);
        }
    }
}

impl AppRuntime {
    #[must_use]
    pub fn task_relations(&self, key: &TaskRelationsKey) -> Option<&TaskRelationsState> {
        self.task_relations.states.get(key)
    }

    #[must_use]
    pub fn task_relations_needs_request(&self, key: &TaskRelationsKey) -> bool {
        matches!(
            self.task_relations.states.get(key),
            None | Some(TaskRelationsState::Refreshing(_))
        )
    }

    pub fn begin_task_relations(&mut self, key: TaskRelationsKey) {
        if !matches!(
            self.task_relations.states.get(&key),
            Some(TaskRelationsState::Refreshing(_))
        ) {
            self.task_relations
                .states
                .insert(key, TaskRelationsState::Loading);
        }
    }

    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    pub(crate) fn retry_task_relations(&mut self, key: &TaskRelationsKey) -> bool {
        if !matches!(
            self.task_relations.states.get(key),
            Some(TaskRelationsState::Unavailable { .. })
        ) {
            return false;
        }
        self.task_relations.states.remove(key);
        self.task_relations.revision = self.task_relations.revision.saturating_add(1);
        true
    }

    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    #[must_use]
    pub(crate) const fn task_relations_revision(&self) -> u64 {
        self.task_relations.revision
    }

    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    pub(crate) fn invalidate_task_relations(&mut self, changes: &[TaskRelationInvalidation]) {
        self.task_relations.invalidate(changes);
    }

    /// Apply one correlated relationship answer without crossing request identity.
    /// Returns whether visible relationship content changed.
    pub fn apply_task_relations(
        &mut self,
        key: &TaskRelationsKey,
        admitted: &Admitted<Response<TaskRelationsAnswer>>,
    ) -> bool {
        if admitted.scope() != self.accepted.scope() {
            return false;
        }
        let state = TaskRelationsState::settle(
            &admitted.get().outcome,
            |answer| answer_matches(key, answer).then(|| Box::new(answer.clone())),
            (
                "task::relations-identity-mismatch",
                "the relationship answer did not match its roots, typed family, or node identities",
            ),
            "task::relations-refused",
        );
        let changed = !matches!(
            (&state, self.task_relations.states.get(key).and_then(TaskRelationsState::answer)),
            (TaskRelationsState::Available(next), Some(current)) if content_eq(current, next)
        );
        self.task_relations.states.insert(key.clone(), state);
        changed
    }
}

fn answer_matches(key: &TaskRelationsKey, answer: &TaskRelationsAnswer) -> bool {
    if answer.roots != key.roots {
        return false;
    }
    let identities = answer
        .nodes
        .iter()
        .map(wire::intent::TaskRelationNode::identity)
        .collect::<BTreeSet<_>>();
    if identities.len() != answer.nodes.len()
        || !key
            .roots
            .iter()
            .all(|task| identities.contains(&TaskRelationNodeRef::Task { task: task.clone() }))
    {
        return false;
    }
    answer.edges.iter().all(|edge| {
        key.includes(edge.relation)
            && identities.contains(&edge.from)
            && identities.contains(&edge.to)
    }) && answer.truncation.as_ref().is_none_or(|truncation| {
        truncation
            .frontier
            .iter()
            .all(|frontier| key.includes(frontier.edge.relation))
    })
}

fn content_eq(left: &TaskRelationsAnswer, right: &TaskRelationsAnswer) -> bool {
    left.roots == right.roots
        && left.nodes == right.nodes
        && left.edges == right.edges
        && left.truncation == right.truncation
}

/// Keep a stale answer on screen while it is reread; a read in flight stays
/// only while `in_flight_answers` says its answer can still arrive, and a
/// refusal is forgotten.
fn refresh_state(state: &mut TaskRelationsState, in_flight_answers: bool) -> bool {
    let keep = match state {
        TaskRelationsState::Available(_) | TaskRelationsState::Refreshing(_) => true,
        TaskRelationsState::Loading => in_flight_answers,
        TaskRelationsState::Unavailable { .. } => false,
    };
    state.refresh();
    keep
}

/// One stream change that can invalidate a cached relationship neighborhood.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskRelationInvalidation {
    kind: FactKind,
    task: wire::TaskStamp,
    dependency_targets: Vec<wire::TaskStamp>,
    origin_target: Option<wire::TaskStamp>,
    controller_target: Option<wire::TaskStamp>,
}

impl TaskRelationInvalidation {
    pub(crate) fn from_fact(fact: &wire::StreamFact) -> Option<Self> {
        let kind = fact.kind();
        if kind.task_relation_kinds().is_empty() {
            return None;
        }
        if let Some(authorized) = fact.read::<wire::rooms::ExecutionAuthorized>() {
            return Some(Self {
                kind,
                task: authorized.authorization.decision.controller,
                dependency_targets: authorized.authorization.selected_outputs,
                origin_target: None,
                controller_target: None,
            });
        }
        if let Some(controlled) = fact.read::<wire::rooms::OutputControlled>() {
            return Some(Self {
                kind,
                task: controlled.output,
                dependency_targets: Vec::new(),
                origin_target: None,
                controller_target: Some(controlled.preparation.controller),
            });
        }
        let task = fact.task()?;
        let dependency_targets = match kind {
            FactKind::TaskAdded => fact
                .read::<wire::body::TaskAdded>()
                .map_or_else(Vec::new, |added| added.depends.clone()),
            FactKind::TaskEdited => fact
                .read::<wire::body::TaskEdited>()
                .and_then(|edited| edited.dependencies.clone())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let origin_target = (kind == FactKind::TaskAdded)
            .then(|| fact.origin().as_str().parse::<wire::TaskProvenance>().ok())
            .flatten()
            .and_then(|origin| origin.parent().cloned());
        let controller_target = (kind == FactKind::TaskAdded)
            .then(|| fact.read::<wire::body::TaskAdded>())
            .flatten()
            .and_then(|added| added.preparation.map(|preparation| preparation.controller));
        Some(Self {
            kind,
            task,
            dependency_targets,
            origin_target,
            controller_target,
        })
    }

    #[cfg(any(target_arch = "wasm32", test, rust_analyzer))]
    fn affects(&self, key: &TaskRelationsKey, state: &TaskRelationsState) -> bool {
        if !self
            .kind
            .task_relation_kinds()
            .iter()
            .any(|relation| key.includes(*relation))
        {
            return false;
        }
        let contains = |task: &wire::TaskStamp| {
            state.answer().map_or_else(
                || key.roots.contains(task),
                |answer| {
                    answer.nodes.iter().any(|node| {
                        matches!(
                            node,
                            wire::intent::TaskRelationNode::Task { task: node_task, .. }
                                if node_task == task
                        )
                    })
                },
            )
        };
        contains(&self.task)
            || (key.includes(TaskRelationKind::Dependency)
                && self.dependency_targets.iter().any(contains))
            || (key.includes(TaskRelationKind::Origin)
                && self.origin_target.as_ref().is_some_and(contains))
            || (key.includes(TaskRelationKind::Controller)
                && self.controller_target.as_ref().is_some_and(contains))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(value: i64) -> wire::TaskStamp {
        wire::TaskStamp::for_micros(value).expect("fixture task stamp")
    }

    fn sequence(value: u64) -> wire::Seq {
        wire::Seq::new(value).expect("fixture sequence")
    }

    fn key(task: &wire::TaskStamp, relation: TaskRelationKind) -> TaskRelationsKey {
        TaskRelationsKey::new(vec![task.clone()], relation, true, 2, 48)
            .expect("bounded fixture key")
    }

    fn answer(task: &wire::TaskStamp) -> Box<TaskRelationsAnswer> {
        Box::new(TaskRelationsAnswer {
            snapshot: sequence(1),
            roots: vec![task.clone()],
            nodes: vec![wire::intent::TaskRelationNode::Task {
                task: task.clone(),
                handle: wire::TaskHandle::for_topic(
                    &"/app".parse::<wire::Topic>().expect("fixture topic"),
                    task,
                ),
                title: "Shown task".to_owned(),
                pitch: String::new(),
                phase: "todo".parse().expect("fixture phase"),
                status: wire::WorkflowStatusKind::Pending,
            }],
            edges: Vec::new(),
            truncation: None,
        })
    }

    fn cache(task: &wire::TaskStamp) -> Cache {
        Cache {
            states: [
                TaskRelationKind::Origin,
                TaskRelationKind::Dependency,
                TaskRelationKind::Controller,
                TaskRelationKind::Review,
                TaskRelationKind::Lifecycle,
            ]
            .into_iter()
            .map(|relation| {
                (
                    key(task, relation),
                    TaskRelationsState::Available(answer(task)),
                )
            })
            .collect(),
            revision: 0,
        }
    }

    #[test]
    fn new_controller_endpoint_refreshes_every_default_overlay_graph() {
        let shown = stamp(1);
        let mut cache = cache(&shown);
        cache.invalidate(&[TaskRelationInvalidation {
            kind: FactKind::TaskAdded,
            task: stamp(2),
            dependency_targets: vec![shown.clone()],
            origin_target: Some(shown.clone()),
            controller_target: Some(shown.clone()),
        }]);

        assert_eq!(cache.revision, 1);
        assert!(
            cache
                .states
                .values()
                .all(|state| matches!(state, TaskRelationsState::Refreshing(_)))
        );
    }
    #[test]
    fn output_control_refreshes_controller_graphs_at_both_edge_endpoints() {
        let controller = stamp(1);
        let controlled = stamp(2);
        let unrelated = stamp(3);
        let fact: wire::StreamFact = serde_json::from_value(serde_json::json!({
            "v": wire::WireVersion::CURRENT,
            "seq": 7,
            "at": "2026-09-23T06:00:00.000Z",
            "actor": "principal:00000000-0000-0000-0000-000000000000",
            "origin": format!("task:{controller}"),
            "cause": 6,
            "fact": {
                "kind": "task::controlled",
                "output": controlled,
                "preparation": {
                    "parent": controller,
                    "controller": controller,
                    "construction": controlled,
                }
            }
        }))
        .expect("admitted output control fact");
        let mut cache = Cache {
            states: [&controller, &controlled, &unrelated]
                .into_iter()
                .flat_map(|task| cache(task).states)
                .collect(),
            revision: 0,
        };

        cache.invalidate(&[TaskRelationInvalidation::from_fact(&fact)
            .expect("output control changes controller relations")]);

        assert!(cache.states.iter().all(|(key, state)| {
            let affected = key.roots.contains(&controlled) || key.roots.contains(&controller);
            matches!(state, TaskRelationsState::Refreshing(_)) == affected
        }));
    }

    #[test]
    fn unrelated_task_change_leaves_retained_graphs_and_revision_unchanged() {
        let shown = stamp(1);
        let mut cache = cache(&shown);
        cache.invalidate(&[TaskRelationInvalidation {
            kind: FactKind::TaskEdited,
            task: stamp(2),
            dependency_targets: Vec::new(),
            origin_target: None,
            controller_target: None,
        }]);

        assert_eq!(cache.revision, 0);
        assert!(
            cache
                .states
                .values()
                .all(|state| matches!(state, TaskRelationsState::Available(_)))
        );
    }

    #[test]
    fn relationship_answer_must_match_roots_nodes_and_requested_family() {
        let root = stamp(1);
        let key = key(&root, TaskRelationKind::Dependency);
        assert_eq!(
            key.request().relations,
            [TaskRelationKind::Dependency, TaskRelationKind::Controller]
        );
        let hidden = TaskRelationsKey::new(
            vec![root.clone()],
            TaskRelationKind::Dependency,
            false,
            2,
            48,
        )
        .expect("bounded controller-hidden key");
        assert_eq!(hidden.request().relations, [TaskRelationKind::Dependency]);
        let mut answer = *answer(&root);
        assert!(answer_matches(&key, &answer));

        answer.roots = vec![stamp(2)];
        assert!(!answer_matches(&key, &answer));
        answer.roots = vec![root.clone()];
        answer.nodes.push(answer.nodes[0].clone());
        assert!(!answer_matches(&key, &answer));
    }

    #[test]
    fn authorization_refreshes_dependency_graphs_for_its_controller_and_selected_outputs() {
        let controller = stamp(1);
        let selected = stamp(2);
        let governed = stamp(3);
        let unrelated = stamp(4);
        let fact: wire::StreamFact = serde_json::from_value(serde_json::json!({
            "v": wire::WireVersion::CURRENT,
            "seq": 7,
            "at": "2026-09-23T06:00:00.000Z",
            "actor": "principal:00000000-0000-0000-0000-000000000000",
            "origin": format!("task:{controller}"),
            "cause": 6,
            "fact": {
                "kind": "proposal::authorized",
                "authorization": {
                    "decision": {"controller": controller, "decision": "1"},
                    "selected_outputs": [selected]
                }
            }
        }))
        .expect("admitted authorization fact");
        let mut cache = Cache {
            states: [&controller, &selected, &governed, &unrelated]
                .into_iter()
                .flat_map(|task| cache(task).states)
                .collect(),
            revision: 0,
        };

        cache
            .invalidate(&[TaskRelationInvalidation::from_fact(&fact)
                .expect("authorization changes dependencies")]);

        assert_eq!(cache.revision, 1);
        for (key, state) in &cache.states {
            let affected = key.relation == TaskRelationKind::Dependency
                && key
                    .roots
                    .iter()
                    .any(|task| task == &controller || task == &selected);
            assert_eq!(
                matches!(state, TaskRelationsState::Refreshing(_)),
                affected,
                "only graphs holding a released edge endpoint need a new read: {key:?}"
            );
        }
    }

    #[test]
    fn shown_task_lifecycle_change_refreshes_each_projection_once() {
        let shown = stamp(1);
        let mut cache = cache(&shown);

        cache.invalidate(&[TaskRelationInvalidation {
            kind: FactKind::TaskDone,
            task: shown,
            dependency_targets: Vec::new(),
            origin_target: None,
            controller_target: None,
        }]);

        assert_eq!(cache.revision, 1);
        assert!(
            cache
                .states
                .values()
                .all(|state| matches!(state, TaskRelationsState::Refreshing(_)))
        );
    }
}
