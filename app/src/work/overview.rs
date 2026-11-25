//! Complete task-set selection and graph policy for the contextual overview.
//!
//! Selection follows the admitted projection's order. Graph construction is
//! intentionally unbounded: protocol relationship limits may bound optional
//! enrichment, but never the roots the operator selected from a board snapshot.

use std::collections::{BTreeMap, BTreeSet};

use wire::TaskStamp;

use super::{WorkProjection, WorkScope};

/// Why one complete task set was opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskOverviewContext {
    /// One task control opened its exact task.
    Task,
    /// A board scope opened every current task it contains.
    Scope(WorkScope),
    /// A relationship control opened a task neighborhood rooted here.
    Relationships(TaskStamp),
}

impl TaskOverviewContext {
    /// The heading for this context; `team` names a team scope.
    #[must_use]
    pub fn label(&self, team: impl Fn(wire::CanonicalUuid) -> String) -> String {
        match self {
            Self::Task => "Selected task".to_owned(),
            Self::Scope(WorkScope::AllOpen) => "All open execution work".to_owned(),
            Self::Scope(WorkScope::All) => "All open work".to_owned(),
            Self::Scope(WorkScope::Topic(topic)) => topic.clone(),
            Self::Scope(WorkScope::Unplaced) => "Unplaced".to_owned(),
            Self::Scope(WorkScope::Planning { topic: None }) => "All open Design work".to_owned(),
            Self::Scope(WorkScope::Planning { topic: Some(topic) }) => {
                format!("Design work · {topic}")
            }
            Self::Scope(WorkScope::Drafts) => "Drafts".to_owned(),
            Self::Scope(WorkScope::Team { team: id }) => format!("Team {}", team(*id)),
            Self::Relationships(task) => format!("Related to {task}"),
        }
    }
}

/// The base flavor an overview wears: the base of the namespace or console its
/// selection shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskOverviewBase {
    /// The page's own base.
    Page,
    /// The Design namespace's base, which the Design global rail wears.
    Design,
    /// The operator's neutral base, which the Drafts total wears.
    Drafts,
    /// One team console's base.
    Team(wire::CanonicalUuid),
}

/// One explicit overview selection. The vector is complete, stable, and
/// deduplicated; there is no distinguished current task or hidden sibling set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskOverviewSelection {
    tasks: Vec<TaskStamp>,
    context: TaskOverviewContext,
}

impl TaskOverviewSelection {
    #[must_use]
    pub fn new(tasks: impl IntoIterator<Item = TaskStamp>, context: TaskOverviewContext) -> Self {
        let mut seen = BTreeSet::new();
        let tasks = tasks
            .into_iter()
            .filter(|task| seen.insert(task.clone()))
            .collect();
        Self { tasks, context }
    }

    #[must_use]
    pub fn task(task: TaskStamp) -> Self {
        Self::new([task], TaskOverviewContext::Task)
    }

    /// Extend this exact selection with one relationship lead, preserving the
    /// admitted root order before the new task.
    #[must_use]
    pub fn with_related_task(&self, task: TaskStamp) -> Self {
        Self::new(
            self.tasks.iter().cloned().chain([task.clone()]),
            TaskOverviewContext::Relationships(task),
        )
    }

    #[must_use]
    pub fn tasks(&self) -> &[TaskStamp] {
        &self.tasks
    }

    #[must_use]
    pub const fn context(&self) -> &TaskOverviewContext {
        &self.context
    }

    /// The one task this selection addresses on its own, rather than a set a
    /// scope, a relationship lead, or several stamps opened.
    #[must_use]
    pub fn sole_task(&self) -> Option<&TaskStamp> {
        match self.tasks.as_slice() {
            [task] if self.context == TaskOverviewContext::Task => Some(task),
            _ => None,
        }
    }

    #[must_use]
    pub fn first(&self) -> Option<&TaskStamp> {
        self.tasks.first()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
}

/// How one task node relates to the complete selected set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskOverviewNodeKind {
    Selected,
    External,
    Missing,
}

/// One selected task or one direct dependency visible beside the selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskOverviewNode {
    task: TaskStamp,
    task_index: Option<usize>,
    selected: bool,
    kind: TaskOverviewNodeKind,
}

impl TaskOverviewNode {
    #[must_use]
    pub const fn task(&self) -> &TaskStamp {
        &self.task
    }

    #[must_use]
    pub const fn task_index(&self) -> Option<usize> {
        self.task_index
    }

    #[must_use]
    pub const fn selected(&self) -> bool {
        self.selected
    }

    #[must_use]
    pub const fn kind(&self) -> TaskOverviewNodeKind {
        self.kind
    }
}

/// A dependency edge from prerequisite to dependent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskOverviewEdge {
    prerequisite: usize,
    dependent: usize,
    satisfied: bool,
}

impl TaskOverviewEdge {
    #[must_use]
    pub const fn prerequisite(self) -> usize {
        self.prerequisite
    }

    #[must_use]
    pub const fn dependent(self) -> usize {
        self.dependent
    }

    #[must_use]
    pub const fn satisfied(self) -> bool {
        self.satisfied
    }
}

/// Complete selected roots plus every direct dependency, without protocol
/// root/node caps. Missing and out-of-selection neighbors remain explicit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskOverviewGraph {
    nodes: Vec<TaskOverviewNode>,
    edges: Vec<TaskOverviewEdge>,
}

impl TaskOverviewGraph {
    #[must_use]
    pub fn nodes(&self) -> &[TaskOverviewNode] {
        &self.nodes
    }

    #[must_use]
    pub fn edges(&self) -> &[TaskOverviewEdge] {
        &self.edges
    }
}

impl WorkProjection<'_> {
    /// Every open task in `scope`, in the projection's admitted offer order.
    #[cfg(test)]
    pub(crate) fn scope_tasks(&self, scope: &WorkScope) -> Vec<TaskStamp> {
        self.offered
            .iter()
            .map(|&index| &self.tasks[index])
            .filter(|task| {
                task.availability != super::WorkAvailability::Terminal && self.in_scope(task, scope)
            })
            .map(|task| task.row.statement.body.born.clone())
            .collect()
    }
    fn scope_overview_tasks(&self, scope: &WorkScope) -> Vec<TaskStamp> {
        self.offered
            .iter()
            .map(|&index| &self.tasks[index])
            .filter(|task| self.in_scope(task, scope))
            .map(|task| task.row.statement.body.born.clone())
            .collect()
    }
    /// Number of current and recently finished tasks the overview opens for
    /// this scope, including the settled rows it renders in its Done group.
    #[must_use]
    pub fn scope_overview_task_count(&self, scope: &WorkScope) -> usize {
        self.offered
            .iter()
            .map(|&index| &self.tasks[index])
            .filter(|task| self.in_scope(task, scope))
            .count()
    }

    #[must_use]
    pub fn overview_for_scope(&self, scope: WorkScope) -> TaskOverviewSelection {
        TaskOverviewSelection::new(
            self.scope_overview_tasks(&scope),
            TaskOverviewContext::Scope(scope),
        )
    }

    /// The base `selection` shows. A team scope shows its console; the Design
    /// scope or a nonempty selection of only Design-owned work shows the
    /// Design namespace; anything else shows the page.
    #[must_use]
    pub fn overview_base(&self, selection: &TaskOverviewSelection) -> TaskOverviewBase {
        match &selection.context {
            TaskOverviewContext::Scope(WorkScope::Team { team }) => TaskOverviewBase::Team(*team),
            TaskOverviewContext::Scope(WorkScope::Drafts) => TaskOverviewBase::Drafts,
            TaskOverviewContext::Scope(WorkScope::Planning { .. }) => TaskOverviewBase::Design,
            _ if !selection.is_empty() && self.only_design_owned(&selection.tasks) => {
                TaskOverviewBase::Design
            }
            _ => TaskOverviewBase::Page,
        }
    }

    /// Whether every task in the deduplicated `tasks` is current and
    /// Design-owned.
    fn only_design_owned(&self, tasks: &[TaskStamp]) -> bool {
        let tasks = tasks.iter().collect::<BTreeSet<_>>();
        self.tasks
            .iter()
            .filter(|task| {
                task.belongs_to_design() && tasks.contains(&task.row.statement.body.born)
            })
            .count()
            == tasks.len()
    }

    /// Build the graph that is available synchronously with the board snapshot.
    #[must_use]
    pub fn overview_graph(&self, selection: &TaskOverviewSelection) -> TaskOverviewGraph {
        let by_task = self
            .tasks
            .iter()
            .enumerate()
            .map(|(index, task)| (task.row.statement.body.born.clone(), index))
            .collect::<BTreeMap<_, _>>();
        let selected = selection.tasks.iter().cloned().collect::<BTreeSet<_>>();
        let mut node_by_task = BTreeMap::new();
        let mut nodes = Vec::with_capacity(selection.tasks.len());

        for task in &selection.tasks {
            let task_index = by_task.get(task).copied();
            let index = nodes.len();
            node_by_task.insert(task.clone(), index);
            nodes.push(TaskOverviewNode {
                task: task.clone(),
                task_index,
                selected: true,
                kind: if task_index.is_some() {
                    TaskOverviewNodeKind::Selected
                } else {
                    TaskOverviewNodeKind::Missing
                },
            });
        }

        let mut edges = Vec::new();
        for dependent in &selection.tasks {
            let Some(&dependent_node) = node_by_task.get(dependent) else {
                continue;
            };
            let Some(&task_index) = by_task.get(dependent) else {
                continue;
            };
            for dependency in self.tasks[task_index]
                .dependencies
                .iter()
                .filter(|dependency| !dependency.released())
            {
                let prerequisite = neighbor(
                    &mut nodes,
                    &mut node_by_task,
                    &by_task,
                    &selected,
                    dependency.born,
                );
                edges.push(TaskOverviewEdge {
                    prerequisite,
                    dependent: dependent_node,
                    satisfied: dependency.satisfied,
                });
            }
        }

        TaskOverviewGraph { nodes, edges }
    }
}

/// The node for `task`, appending it as an outside neighbor when the graph
/// does not hold it yet.
fn neighbor(
    nodes: &mut Vec<TaskOverviewNode>,
    node_by_task: &mut BTreeMap<TaskStamp, usize>,
    by_task: &BTreeMap<TaskStamp, usize>,
    selected: &BTreeSet<TaskStamp>,
    task: &TaskStamp,
) -> usize {
    if let Some(&index) = node_by_task.get(task) {
        return index;
    }
    let task_index = by_task.get(task).copied();
    let index = nodes.len();
    node_by_task.insert(task.clone(), index);
    nodes.push(TaskOverviewNode {
        task: task.clone(),
        task_index,
        selected: selected.contains(task),
        kind: if task_index.is_some() {
            TaskOverviewNodeKind::External
        } else {
            TaskOverviewNodeKind::Missing
        },
    });
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work::tests::{board, design, stamp, task};

    #[test]
    fn selection_preserves_first_order_and_deduplicates() {
        let first = stamp(1);
        let second = stamp(2);
        let selection = TaskOverviewSelection::new(
            [second.clone(), first.clone(), second],
            TaskOverviewContext::Task,
        );
        assert_eq!(selection.tasks(), &[stamp(2), first]);
    }

    #[test]
    fn single_and_relationship_entries_keep_exact_root_identity() {
        let single = TaskOverviewSelection::task(stamp(1));
        assert_eq!(single.tasks(), &[stamp(1)]);
        assert_eq!(single.context(), &TaskOverviewContext::Task);

        let related = single.with_related_task(stamp(2));
        assert_eq!(related.tasks(), &[stamp(1), stamp(2)]);
        assert_eq!(
            related.context(),
            &TaskOverviewContext::Relationships(stamp(2)),
        );
    }

    #[test]
    fn scope_tabs_count_the_complete_selection_they_open() {
        let mut settled = task(4, "/alpha");
        settled.workflow.body.status = wire::WorkflowStatus::completed(
            "2026-09-23T06:00:00.000Z"
                .parse()
                .expect("fixture completion"),
        );
        let mut concluded = design(5, "/alpha");
        concluded.workflow.body.status = wire::WorkflowStatus::completed(
            "2026-09-23T06:01:00.000Z"
                .parse()
                .expect("fixture completion"),
        );
        let mut board = board(vec![
            task(1, "/alpha"),
            task(2, "/alpha/other"),
            task(3, "/alpha"),
        ]);
        board.finished = vec![settled, concluded];
        board.offer = vec![stamp(3), stamp(1), stamp(2)];
        let work = WorkProjection::from_board_and_lanes(&board, None);
        let scope = WorkScope::Topic("/alpha".to_owned());
        assert_eq!(work.scope_tasks(&scope), vec![stamp(3), stamp(1)]);
        assert_eq!(work.scope_overview_task_count(&scope), 3);
        assert_eq!(
            work.overview_for_scope(scope).tasks(),
            &[stamp(3), stamp(1), stamp(4)]
        );
        assert_eq!(work.scope_overview_task_count(&WorkScope::All), 5);
        assert_eq!(work.scope_overview_task_count(&WorkScope::AllOpen), 4);
        assert_eq!(work.scope_overview_task_count(&WorkScope::planning()), 1);
        assert_eq!(
            work.overview_for_scope(WorkScope::planning()).tasks(),
            &[stamp(5)]
        );
    }

    #[test]
    fn global_scopes_and_topic_select_the_complete_admitted_set() {
        let first = task(1, "/alpha/one");
        let parent = task(2, "/alpha");
        let other = task(3, "/beta/other");
        let last = task(4, "/alpha/one");
        let mut board = board(vec![first, parent, other, last]);
        board.offer = vec![stamp(3), stamp(2), stamp(4), stamp(1)];
        let work = WorkProjection::from_board_and_lanes(&board, None);

        assert_eq!(
            work.overview_for_scope(WorkScope::AllOpen).tasks(),
            &[stamp(3), stamp(2), stamp(4), stamp(1)],
        );
        assert_eq!(
            work.overview_for_scope(WorkScope::Topic("/alpha/one".to_owned()))
                .tasks(),
            &[stamp(4), stamp(1)],
        );
        assert_eq!(
            work.overview_for_scope(WorkScope::Topic("/alpha".to_owned()))
                .tasks(),
            &[stamp(2)],
        );
    }

    #[test]
    fn an_overview_base_follows_its_scope_before_design_ownership() {
        let board = board(vec![
            design(1, "/alpha/design"),
            design(2, "/alpha/design"),
            task(3, "/alpha/build"),
        ]);
        let work = WorkProjection::from_board_and_lanes(&board, None);
        let selection = |tasks: &[i64], context| {
            TaskOverviewSelection::new(tasks.iter().copied().map(stamp), context)
        };
        let design_scope = TaskOverviewContext::Scope(WorkScope::Planning { topic: None });

        assert_eq!(
            work.overview_base(&selection(&[1, 2], TaskOverviewContext::Task)),
            TaskOverviewBase::Design
        );
        assert_eq!(
            work.overview_base(&selection(&[1, 3], TaskOverviewContext::Task)),
            TaskOverviewBase::Page,
            "execution work beside a Design keeps the page base"
        );
        assert_eq!(
            work.overview_base(&selection(&[1, 99], TaskOverviewContext::Task)),
            TaskOverviewBase::Page,
            "a task the board does not carry is not known to be Design-owned"
        );
        assert_eq!(
            work.overview_base(&selection(&[], TaskOverviewContext::Task)),
            TaskOverviewBase::Page
        );
        assert_eq!(
            work.overview_base(&selection(&[], design_scope)),
            TaskOverviewBase::Design,
            "the Design scope shows its namespace even while empty"
        );
        let team: wire::CanonicalUuid = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
            .parse()
            .expect("fixture team");
        let team_scope = TaskOverviewContext::Scope(WorkScope::Team { team });
        assert_eq!(
            work.overview_base(&selection(&[1, 2], team_scope)),
            TaskOverviewBase::Team(team),
            "a team scope shows its console even over only Design work"
        );
        assert_eq!(
            work.overview_base(&selection(
                &[1, 2],
                TaskOverviewContext::Scope(WorkScope::Drafts)
            )),
            TaskOverviewBase::Drafts,
            "the Drafts scope stays neutral even over only Design-owned drafts"
        );
    }
    #[test]
    fn dependency_overview_omits_preparation_lifecycle_wait() {
        let design = design(1, "/design");
        let mut output = task(2, "/output");
        output.statement.body.preparation = Some(wire::TaskPreparation {
            parent: stamp(1),
            controller: stamp(1),
            construction: stamp(2),
        });
        let mut board = board(vec![design, output]);
        board
            .design_cycles
            .insert(stamp(1), wire::rooms::DesignCycle::WaitingOnSelectedWork);
        let work = WorkProjection::from_board_and_lanes(&board, None);
        assert!(
            work.tasks()[0]
                .dependencies()
                .iter()
                .any(super::super::WorkDependency::released),
            "the lifecycle wait remains available to task detail"
        );

        let selection = TaskOverviewSelection::new([stamp(1), stamp(2)], TaskOverviewContext::Task);
        assert!(
            work.overview_graph(&selection).edges().is_empty(),
            "preparation state is not an execution dependency"
        );
    }
}
