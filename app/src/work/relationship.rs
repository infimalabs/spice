//! Bounded relationship neighborhoods over the admitted Work projection.
//!
//! This model keeps graph policy independent from both Mermaid source and the
//! browser surface. Durable task stamps are its identity boundary; indexes only
//! point back into the projection that produced the graph.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Display, Formatter};

use wire::{TaskProvenance, TaskStamp};

use super::{WorkAvailability, WorkProjection};

/// Which scheduling neighbors a relationship query follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkRelationshipDirection {
    None,
    Prerequisites,
    Dependents,
    Both,
}

impl WorkRelationshipDirection {
    pub(crate) const fn includes_prerequisites(self) -> bool {
        matches!(self, Self::Prerequisites | Self::Both)
    }

    pub(crate) const fn includes_dependents(self) -> bool {
        matches!(self, Self::Dependents | Self::Both)
    }

    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    pub(crate) const fn toggled(self, direction: Self) -> Self {
        match (
            self.includes_prerequisites() ^ direction.includes_prerequisites(),
            self.includes_dependents() ^ direction.includes_dependents(),
        ) {
            (false, false) => Self::None,
            (true, false) => Self::Prerequisites,
            (false, true) => Self::Dependents,
            (true, true) => Self::Both,
        }
    }
}

/// Validated bounds for one relationship neighborhood.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkRelationshipOptions {
    direction: WorkRelationshipDirection,
    depth: u8,
    max_nodes: usize,
}

impl WorkRelationshipOptions {
    pub const MAX_ROOTS: usize = 8;
    pub const MAX_DEPTH: u8 = 4;
    pub const MAX_NODES: usize = 128;

    pub fn new(
        direction: WorkRelationshipDirection,
        depth: u8,
        max_nodes: usize,
    ) -> Result<Self, WorkRelationshipError> {
        if depth > Self::MAX_DEPTH {
            return Err(WorkRelationshipError::Depth {
                provided: depth,
                maximum: Self::MAX_DEPTH,
            });
        }
        if !(1..=Self::MAX_NODES).contains(&max_nodes) {
            return Err(WorkRelationshipError::NodeLimit {
                provided: max_nodes,
                minimum: 1,
                maximum: Self::MAX_NODES,
            });
        }
        Ok(Self {
            direction,
            depth,
            max_nodes,
        })
    }

    #[must_use]
    pub const fn direction(self) -> WorkRelationshipDirection {
        self.direction
    }

    #[must_use]
    pub const fn depth(self) -> u8 {
        self.depth
    }

    #[must_use]
    pub const fn max_nodes(self) -> usize {
        self.max_nodes
    }
}

impl Default for WorkRelationshipOptions {
    fn default() -> Self {
        Self {
            direction: WorkRelationshipDirection::Both,
            depth: 1,
            max_nodes: 64,
        }
    }
}

/// A refused relationship query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkRelationshipError {
    NoRoots,
    Roots {
        provided: usize,
        maximum: usize,
    },
    Depth {
        provided: u8,
        maximum: u8,
    },
    NodeLimit {
        provided: usize,
        minimum: usize,
        maximum: usize,
    },
}

impl Display for WorkRelationshipError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRoots => formatter.write_str("a relationship query requires a selected task"),
            Self::Roots { provided, maximum } => write!(
                formatter,
                "relationship query selected {provided} roots; maximum is {maximum}"
            ),
            Self::Depth { provided, maximum } => write!(
                formatter,
                "relationship query depth {provided} exceeds maximum {maximum}"
            ),
            Self::NodeLimit {
                provided,
                minimum,
                maximum,
            } => write!(
                formatter,
                "relationship node limit {provided} is outside {minimum}..={maximum}"
            ),
        }
    }
}

impl std::error::Error for WorkRelationshipError {}

/// One typed node in a relationship neighborhood.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkRelationshipNode {
    Task(WorkRelationshipTaskNode),
    Origin(WorkRelationshipOriginNode),
}

/// A current or historical task identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkRelationshipTaskNode {
    born: TaskStamp,
    task: Option<usize>,
    selected: bool,
    depth: u8,
    /// The exact topic; `None` while the task is unplaced or outside the
    /// projection, never an empty path.
    topic: Option<String>,
    availability: Option<WorkAvailability>,
}

impl WorkRelationshipTaskNode {
    #[must_use]
    pub const fn born(&self) -> &TaskStamp {
        &self.born
    }

    /// Index into the [`WorkProjection`] that produced this graph.
    #[must_use]
    pub const fn task(&self) -> Option<usize> {
        self.task
    }

    #[must_use]
    pub const fn selected(&self) -> bool {
        self.selected
    }

    #[must_use]
    pub const fn depth(&self) -> u8 {
        self.depth
    }

    /// The exact topic; `None` while unplaced or outside the projection.
    #[must_use]
    pub fn topic(&self) -> Option<&str> {
        self.topic.as_deref()
    }

    #[must_use]
    pub const fn availability(&self) -> Option<WorkAvailability> {
        self.availability
    }
}

/// Immediate durable provenance attached to an included task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkRelationshipOriginNode {
    origin: TaskProvenance,
    depth: u8,
}

impl WorkRelationshipOriginNode {
    #[must_use]
    pub const fn origin(&self) -> &TaskProvenance {
        &self.origin
    }

    #[must_use]
    pub const fn depth(&self) -> u8 {
        self.depth
    }
}

/// Semantic edge type. Dependency edges point prerequisite to dependent;
/// provenance edges point origin to task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum WorkRelationshipKind {
    Dependency,
    Provenance,
}

/// One edge whose endpoints index [`WorkRelationshipGraph::nodes`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkRelationshipEdge {
    source: usize,
    target: usize,
    kind: WorkRelationshipKind,
    satisfied: Option<bool>,
}

impl WorkRelationshipEdge {
    #[must_use]
    pub const fn source(self) -> usize {
        self.source
    }

    #[must_use]
    pub const fn target(self) -> usize {
        self.target
    }

    #[must_use]
    pub const fn kind(self) -> WorkRelationshipKind {
        self.kind
    }

    /// Completion of the prerequisite for dependency edges.
    #[must_use]
    pub const fn satisfied(self) -> Option<bool> {
        self.satisfied
    }
}

/// Expansion direction omitted at a bounded node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum WorkRelationshipExpansion {
    Prerequisites,
    Dependents,
    Provenance,
}

/// Deterministic count of relationships omitted by depth or node bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkRelationshipFrontier {
    node: usize,
    expansion: WorkRelationshipExpansion,
    omitted: usize,
}

impl WorkRelationshipFrontier {
    #[must_use]
    pub const fn node(self) -> usize {
        self.node
    }

    #[must_use]
    pub const fn expansion(self) -> WorkRelationshipExpansion {
        self.expansion
    }

    #[must_use]
    pub const fn omitted(self) -> usize {
        self.omitted
    }
}

/// Stable bounded union of the selected roots' relationship neighborhoods.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkRelationshipGraph {
    nodes: Vec<WorkRelationshipNode>,
    edges: Vec<WorkRelationshipEdge>,
    frontier: Vec<WorkRelationshipFrontier>,
}

impl WorkRelationshipGraph {
    #[must_use]
    pub fn nodes(&self) -> &[WorkRelationshipNode] {
        &self.nodes
    }

    #[must_use]
    pub fn edges(&self) -> &[WorkRelationshipEdge] {
        &self.edges
    }

    #[must_use]
    pub fn frontier(&self) -> &[WorkRelationshipFrontier] {
        &self.frontier
    }

    #[must_use]
    pub fn truncated(&self) -> bool {
        !self.frontier.is_empty()
    }
}

impl WorkProjection<'_> {
    /// Build a bounded relationship union without consulting presentation text
    /// or renderer output. Unknown roots and dependencies remain task nodes so
    /// durable selection survives a snapshot that no longer carries their row.
    pub fn relationships(
        &self,
        roots: &[TaskStamp],
        options: WorkRelationshipOptions,
    ) -> Result<WorkRelationshipGraph, WorkRelationshipError> {
        let roots = validated_roots(roots, options)?;
        let by_born = self
            .tasks()
            .iter()
            .enumerate()
            .map(|(index, task)| (&task.row().statement.body.born, index))
            .collect::<BTreeMap<_, _>>();
        let mut graph = GraphDraft {
            nodes: initial_nodes(&roots, &by_born),
            ..GraphDraft::default()
        };
        let mut current = roots.iter().cloned().collect::<Vec<_>>();
        let mut depth = 0;

        while !current.is_empty() {
            let candidates =
                collect_candidates(self, &current, depth, options, &by_born, &mut graph);
            current = admit_candidates(candidates, depth, options, &by_born, &mut graph);
            depth = depth.saturating_add(1);
        }

        Ok(finish_graph(
            self,
            &roots,
            graph.nodes,
            graph.edges,
            graph.frontier,
        ))
    }
}

fn validated_roots(
    roots: &[TaskStamp],
    options: WorkRelationshipOptions,
) -> Result<BTreeSet<TaskStamp>, WorkRelationshipError> {
    let roots = roots.iter().cloned().collect::<BTreeSet<_>>();
    if roots.is_empty() {
        return Err(WorkRelationshipError::NoRoots);
    }
    if roots.len() > WorkRelationshipOptions::MAX_ROOTS {
        return Err(WorkRelationshipError::Roots {
            provided: roots.len(),
            maximum: WorkRelationshipOptions::MAX_ROOTS,
        });
    }
    if options.max_nodes < roots.len() {
        return Err(WorkRelationshipError::NodeLimit {
            provided: options.max_nodes,
            minimum: roots.len(),
            maximum: WorkRelationshipOptions::MAX_NODES,
        });
    }
    Ok(roots)
}

fn initial_nodes(
    roots: &BTreeSet<TaskStamp>,
    by_born: &BTreeMap<&TaskStamp, usize>,
) -> BTreeMap<NodeKey, NodeDraft> {
    roots
        .iter()
        .map(|born| {
            (
                NodeKey::Task(born.clone()),
                NodeDraft::Task {
                    depth: 0,
                    task: by_born.get(born).copied(),
                },
            )
        })
        .collect()
}

fn collect_candidates(
    work: &WorkProjection<'_>,
    current: &[TaskStamp],
    depth: u8,
    options: WorkRelationshipOptions,
    by_born: &BTreeMap<&TaskStamp, usize>,
    graph: &mut GraphDraft,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for born in current {
        let Some(task_index) = by_born.get(born).copied() else {
            continue;
        };
        let task = &work.tasks()[task_index];
        include_origin(task, born, depth, options.max_nodes, graph);
        append_dependency_candidates(work, task, born, options.direction, &mut candidates);
    }
    candidates.sort();
    candidates
}

fn include_origin(
    task: &super::WorkTask<'_>,
    born: &TaskStamp,
    depth: u8,
    max_nodes: usize,
    graph: &mut GraphDraft,
) {
    let provenance = &task.row().statement.body.provenance;
    let task_key = NodeKey::Task(born.clone());
    let origin_key = NodeKey::Origin(provenance.clone());
    if graph.nodes.contains_key(&origin_key) || graph.nodes.len() < max_nodes {
        graph
            .nodes
            .entry(origin_key.clone())
            .or_insert(NodeDraft::Origin { depth });
        graph.edges.insert(EdgeDraft {
            source: origin_key,
            target: task_key,
            kind: WorkRelationshipKind::Provenance,
            satisfied: None,
        });
    } else {
        increment_frontier(
            &mut graph.frontier,
            task_key,
            WorkRelationshipExpansion::Provenance,
        );
    }
}

fn append_dependency_candidates(
    work: &WorkProjection<'_>,
    task: &super::WorkTask<'_>,
    born: &TaskStamp,
    direction: WorkRelationshipDirection,
    candidates: &mut Vec<Candidate>,
) {
    if direction.includes_prerequisites() {
        candidates.extend(task.dependencies().iter().map(|dependency| Candidate {
            neighbor: dependency.born().clone(),
            traversal: born.clone(),
            source: dependency.born().clone(),
            target: born.clone(),
            expansion: WorkRelationshipExpansion::Prerequisites,
            satisfied: dependency.satisfied(),
        }));
    }
    if direction.includes_dependents() {
        let satisfied = !task.row().workflow.body.status.is_pending();
        candidates.extend(task.dependents().iter().map(|dependent| {
            let dependent = &work.tasks()[dependent.task()].row().statement.body.born;
            Candidate {
                neighbor: dependent.clone(),
                traversal: born.clone(),
                source: born.clone(),
                target: dependent.clone(),
                expansion: WorkRelationshipExpansion::Dependents,
                satisfied,
            }
        }));
    }
}

fn admit_candidates(
    candidates: Vec<Candidate>,
    depth: u8,
    options: WorkRelationshipOptions,
    by_born: &BTreeMap<&TaskStamp, usize>,
    graph: &mut GraphDraft,
) -> Vec<TaskStamp> {
    let mut next = Vec::new();
    for candidate in candidates {
        let neighbor_key = NodeKey::Task(candidate.neighbor.clone());
        let admitted = if graph.nodes.contains_key(&neighbor_key) {
            true
        } else if depth < options.depth && graph.nodes.len() < options.max_nodes {
            graph.nodes.insert(
                neighbor_key,
                NodeDraft::Task {
                    depth: depth.saturating_add(1),
                    task: by_born.get(&candidate.neighbor).copied(),
                },
            );
            next.push(candidate.neighbor.clone());
            true
        } else {
            false
        };
        if admitted {
            graph.edges.insert(EdgeDraft {
                source: NodeKey::Task(candidate.source),
                target: NodeKey::Task(candidate.target),
                kind: WorkRelationshipKind::Dependency,
                satisfied: Some(candidate.satisfied),
            });
        } else {
            increment_frontier(
                &mut graph.frontier,
                NodeKey::Task(candidate.traversal),
                candidate.expansion,
            );
        }
    }
    next.sort();
    next.dedup();
    next
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum NodeKey {
    Task(TaskStamp),
    Origin(TaskProvenance),
}

#[derive(Clone, Copy, Debug)]
enum NodeDraft {
    Task { depth: u8, task: Option<usize> },
    Origin { depth: u8 },
}

#[derive(Default)]
struct GraphDraft {
    nodes: BTreeMap<NodeKey, NodeDraft>,
    edges: BTreeSet<EdgeDraft>,
    frontier: BTreeMap<(NodeKey, WorkRelationshipExpansion), usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct EdgeDraft {
    source: NodeKey,
    target: NodeKey,
    kind: WorkRelationshipKind,
    satisfied: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Candidate {
    neighbor: TaskStamp,
    traversal: TaskStamp,
    source: TaskStamp,
    target: TaskStamp,
    expansion: WorkRelationshipExpansion,
    satisfied: bool,
}

fn increment_frontier(
    frontier: &mut BTreeMap<(NodeKey, WorkRelationshipExpansion), usize>,
    node: NodeKey,
    expansion: WorkRelationshipExpansion,
) {
    *frontier.entry((node, expansion)).or_default() += 1;
}

fn finish_graph(
    work: &WorkProjection<'_>,
    roots: &BTreeSet<TaskStamp>,
    nodes: BTreeMap<NodeKey, NodeDraft>,
    edges: BTreeSet<EdgeDraft>,
    frontier: BTreeMap<(NodeKey, WorkRelationshipExpansion), usize>,
) -> WorkRelationshipGraph {
    let drafts = nodes.into_iter().collect::<Vec<_>>();
    let indexes = drafts
        .iter()
        .enumerate()
        .map(|(index, (key, _))| (key.clone(), index))
        .collect::<BTreeMap<_, _>>();
    let nodes = drafts
        .into_iter()
        .map(|(key, draft)| match (key, draft) {
            (NodeKey::Task(born), NodeDraft::Task { depth, task }) => {
                let projected = task.map(|index| &work.tasks()[index]);
                WorkRelationshipNode::Task(WorkRelationshipTaskNode {
                    selected: roots.contains(&born),
                    born,
                    task,
                    depth,
                    topic: projected
                        .and_then(|task| super::row_topic(task.row()).map(str::to_owned)),
                    availability: projected.map(|task| task.availability()),
                })
            }
            (NodeKey::Origin(origin), NodeDraft::Origin { depth }) => {
                WorkRelationshipNode::Origin(WorkRelationshipOriginNode { origin, depth })
            }
            _ => unreachable!("node key and draft variants are constructed together"),
        })
        .collect();
    let edges = edges
        .into_iter()
        .map(|edge| WorkRelationshipEdge {
            source: indexes[&edge.source],
            target: indexes[&edge.target],
            kind: edge.kind,
            satisfied: edge.satisfied,
        })
        .collect();
    let frontier = frontier
        .into_iter()
        .map(|((node, expansion), omitted)| WorkRelationshipFrontier {
            node: indexes[&node],
            expansion,
            omitted,
        })
        .collect();
    WorkRelationshipGraph {
        nodes,
        edges,
        frontier,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::BoardView;
    use wire::intent::{BoardStatement, BoardTask, Positioned};
    use wire::{Environment, EnvironmentRequirements};

    fn seq(value: u64) -> wire::Seq {
        wire::Seq::new(value).expect("fixture sequence is positive")
    }

    fn stamp(value: i64) -> TaskStamp {
        TaskStamp::for_micros(value).expect("fixture stamp is representable")
    }

    fn task(value: i64, dependencies: &[i64]) -> BoardTask {
        let born = stamp(value);
        BoardTask {
            ownership: None,
            origin_thread: "operator".to_owned(),
            creator_team: None,
            presentation_title: format!("task {value}"),
            statement: Positioned {
                position: seq(1),
                body: BoardStatement {
                    preparation: None,
                    intent: None,
                    handle: wire::TaskHandle::for_topic(
                        &"/canvas/work"
                            .parse::<wire::Topic>()
                            .expect("fixture topic"),
                        &born,
                    ),
                    born: born.clone(),
                    topic_origin: seq(value.unsigned_abs() + 1),
                    pitch: "This work keeps the fixture honest.".to_owned(),
                    description: None,
                    term: Vec::new(),
                    requirements: EnvironmentRequirements::default(),
                    resources: wire::ResourceRequirements::default(),
                    topic: "/canvas/work".parse().expect("fixture topic"),
                    priority: None,
                    taste: None,
                    dependencies: dependencies.iter().map(|value| stamp(*value)).collect(),
                    provenance: "ack:dTVQdQfLw".parse().expect("fixture origin"),
                    creation: wire::body::TaskCreationContext::unavailable(
                        wire::body::TaskCreationUnavailable::PrincipalActor,
                    ),
                    revision: seq(1),
                },
            },
            workflow: Positioned {
                position: seq(2),
                body: wire::WorkflowHead {
                    flow: vec![wire::Phase::Draft, "todo".parse().expect("fixture phase")],
                    phase: "todo".parse().expect("fixture phase"),
                    status: wire::WorkflowStatus::pending(),
                    deferral: None,
                    submitted: false,
                    generation: 0,
                },
            },
            completion: Positioned {
                position: seq(2),
                body: wire::CompletionHead::opened(seq(2)),
            },
            claims: Vec::new(),
            original_claimed_at: None,
            invitations: Vec::new(),
            related: Vec::new(),
            explicit_release: false,
            design_cycle: wire::rooms::DesignCycleFacts::default(),
            design_coverage: None,
            collapse_reason: None,
            controlled: Vec::new(),
            claim_context: None,
            git_context: None,
            parked_commits: None,
        }
    }

    /// Stands in for the resident's gate: a row waiting on an open
    /// prerequisite of this fixture is blocked.
    fn board(tasks: Vec<BoardTask>) -> BoardView {
        let blocked = tasks
            .iter()
            .filter(|task| {
                task.statement.body.dependencies.iter().any(|dependency| {
                    tasks.iter().any(|other| {
                        other.statement.body.born == *dependency
                            && other.workflow.body.status.is_pending()
                    })
                })
            })
            .map(|task| task.statement.body.born.clone())
            .collect();
        BoardView {
            schema: 1,
            cursor: seq(100),
            tasks,
            finished: Vec::new(),
            allocation_evidence: Vec::new(),
            offer: Vec::new(),
            blocked,
            unallocated: std::collections::BTreeMap::new(),
            workflow_waiting: std::collections::BTreeSet::new(),
            design_cycles: std::collections::BTreeMap::new(),
            quote_tasks: std::rc::Rc::default(),
            environment: Environment::journal_schema(1).expect("fixture schema"),
        }
    }

    fn task_nodes(graph: &WorkRelationshipGraph) -> Vec<&WorkRelationshipTaskNode> {
        graph
            .nodes()
            .iter()
            .filter_map(|node| match node {
                WorkRelationshipNode::Task(task) => Some(task),
                WorkRelationshipNode::Origin(_) => None,
            })
            .collect()
    }

    fn dependency_edges(graph: &WorkRelationshipGraph) -> usize {
        graph
            .edges()
            .iter()
            .filter(|edge| edge.kind() == WorkRelationshipKind::Dependency)
            .count()
    }

    #[test]
    fn multi_selection_unions_shared_dependencies_in_stable_order() {
        let board = board(vec![task(3, &[1]), task(1, &[]), task(2, &[1])]);
        let work = WorkProjection::from_board_and_lanes(&board, None);
        let options = WorkRelationshipOptions::new(WorkRelationshipDirection::Prerequisites, 1, 16)
            .expect("fixture options");
        let left = work
            .relationships(&[stamp(3), stamp(2)], options)
            .expect("relationship graph");
        let right = work
            .relationships(&[stamp(2), stamp(3)], options)
            .expect("relationship graph");

        assert_eq!(left, right);
        assert_eq!(
            task_nodes(&left)
                .iter()
                .map(|node| (node.born().clone(), node.selected(), node.depth()))
                .collect::<Vec<_>>(),
            [
                (stamp(1), false, 1),
                (stamp(2), true, 0),
                (stamp(3), true, 0),
            ]
        );
        assert_eq!(dependency_edges(&left), 2);
        assert!(!left.truncated());
    }

    #[test]
    fn repeated_paths_and_cycles_emit_each_node_and_edge_once() {
        let board = board(vec![task(1, &[2]), task(2, &[1])]);
        let work = WorkProjection::from_board_and_lanes(&board, None);
        let graph = work
            .relationships(
                &[stamp(1)],
                WorkRelationshipOptions::new(WorkRelationshipDirection::Both, 4, 16)
                    .expect("fixture options"),
            )
            .expect("cyclic snapshot remains projectable");

        assert_eq!(task_nodes(&graph).len(), 2);
        assert_eq!(dependency_edges(&graph), 2);
        assert!(!graph.truncated());
    }

    #[test]
    fn missing_historical_neighbors_keep_their_durable_identity() {
        let board = board(vec![task(1, &[99])]);
        let work = WorkProjection::from_board_and_lanes(&board, None);
        let graph = work
            .relationships(
                &[stamp(1)],
                WorkRelationshipOptions::new(WorkRelationshipDirection::Prerequisites, 1, 16)
                    .expect("fixture options"),
            )
            .expect("missing dependency remains representable");
        let missing = task_nodes(&graph)
            .into_iter()
            .find(|node| node.born() == &stamp(99))
            .expect("missing dependency node");

        assert_eq!(missing.task(), None);
        assert_eq!(missing.topic(), None);
        assert_eq!(missing.availability(), None);
        assert_eq!(dependency_edges(&graph), 1);
    }

    #[test]
    fn bounds_report_exact_frontiers_and_preserve_immediate_provenance() {
        let board = board(vec![
            task(4, &[1, 2, 3]),
            task(3, &[]),
            task(2, &[]),
            task(1, &[]),
        ]);
        let work = WorkProjection::from_board_and_lanes(&board, None);
        let bounded = work
            .relationships(
                &[stamp(4)],
                WorkRelationshipOptions::new(WorkRelationshipDirection::Prerequisites, 1, 3)
                    .expect("fixture options"),
            )
            .expect("bounded graph");

        assert_eq!(
            task_nodes(&bounded)
                .iter()
                .map(|node| node.born().clone())
                .collect::<Vec<_>>(),
            [stamp(1), stamp(4)]
        );
        assert_eq!(bounded.frontier().len(), 1);
        assert_eq!(
            bounded.frontier()[0].expansion(),
            WorkRelationshipExpansion::Prerequisites
        );
        assert_eq!(bounded.frontier()[0].omitted(), 2);

        let provenance = work
            .relationships(
                &[stamp(4)],
                WorkRelationshipOptions::new(WorkRelationshipDirection::Both, 0, 2)
                    .expect("fixture options"),
            )
            .expect("root provenance graph");
        assert!(matches!(
            &provenance.nodes()[1],
            WorkRelationshipNode::Origin(origin) if origin.origin().to_string() == "ack:dTVQdQfLw"
        ));
        assert_eq!(
            provenance.edges()[0].kind(),
            WorkRelationshipKind::Provenance
        );
        assert_eq!(task_nodes(&provenance)[0].topic(), Some("/canvas/work"));
        assert_eq!(
            task_nodes(&provenance)[0].availability(),
            Some(WorkAvailability::Blocked)
        );
    }

    #[test]
    fn selected_root_and_query_limits_are_explicit() {
        let board = board(vec![task(1, &[])]);
        let work = WorkProjection::from_board_and_lanes(&board, None);
        let options = WorkRelationshipOptions::default();
        assert_eq!(
            work.relationships(&[], options),
            Err(WorkRelationshipError::NoRoots)
        );
        let roots = (1..=9).map(stamp).collect::<Vec<_>>();
        assert_eq!(
            work.relationships(&roots, options),
            Err(WorkRelationshipError::Roots {
                provided: 9,
                maximum: WorkRelationshipOptions::MAX_ROOTS,
            })
        );
        assert_eq!(
            work.relationships(
                &[stamp(1), stamp(2)],
                WorkRelationshipOptions {
                    max_nodes: 1,
                    ..options
                }
            ),
            Err(WorkRelationshipError::NodeLimit {
                provided: 1,
                minimum: 2,
                maximum: WorkRelationshipOptions::MAX_NODES,
            })
        );
    }
}
