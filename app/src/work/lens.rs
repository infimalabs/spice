//! Typed relationship enrichment merged with the complete local root graph:
//! the overview's graph and relationship list.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;

use wire::intent::{
    TaskLifecycleKind, TaskRelationEdge, TaskRelationKind, TaskRelationNode, TaskRelationNodeRef,
    TaskRelationsAnswer,
};
use wire::{Actor, TaskStamp, body::Verdict};

use super::{
    TaskOverviewGraph, TaskOverviewNode, TaskOverviewNodeKind, TaskOverviewSelection,
    WorkProjection, WorkRelationshipDirection,
};

pub(crate) struct Presentation {
    pub(crate) nodes: Vec<Node>,
    pub(crate) edges: Vec<Edge>,
    pub(crate) source: String,
}

pub(crate) struct Node {
    pub(crate) id: String,
    pub(crate) kind: &'static str,
    pub(crate) label: String,
    pub(crate) detail: String,
    /// Commit time of a non-task fact, which the evidence list renders as a
    /// live relative time rather than as part of `detail`.
    #[cfg_attr(
        all(test, not(target_arch = "wasm32")),
        expect(
            dead_code,
            reason = "only the wasm32 evidence list renders the fact's relative time"
        )
    )]
    pub(crate) at: Option<wire::Instant>,
    pub(crate) task: Option<TaskStamp>,
    /// Workflow status the typed enrichment reports for a task the admitted
    /// board lacks, such as a completed prerequisite.
    pub(crate) status: Option<wire::WorkflowStatusKind>,
    diagram: String,
    pub(crate) selected: bool,
}

pub(crate) struct Edge {
    pub(crate) from: usize,
    pub(crate) to: usize,
    pub(crate) label: &'static str,
    dotted: bool,
}

/// The reader's view selectors over one selection: which relation the graph
/// draws, which dependency direction it follows, and whether settled tasks
/// are drawn at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LensView {
    pub(crate) relation: TaskRelationKind,
    pub(crate) direction: WorkRelationshipDirection,
    /// Draw preparation controller edges over the selected relation family.
    pub(crate) show_controllers: bool,
    /// Draw draft tasks, except that an individually addressed task stays drawn.
    pub(crate) show_drafts: bool,
    /// Keeps done and deleted tasks in the graph, including selected roots;
    /// otherwise they and every edge touching them are hidden, except the one
    /// task the selection addresses on its own, which is always drawn.
    pub(crate) show_settled: bool,
}

pub(crate) fn project(
    local: &TaskOverviewGraph,
    work: &WorkProjection<'_>,
    answer: Option<&TaskRelationsAnswer>,
    view: LensView,
    selection: &TaskOverviewSelection,
    local_time: &dyn Fn(&wire::Instant) -> String,
) -> Option<Presentation> {
    let LensView {
        relation,
        direction,
        show_controllers,
        show_drafts,
        show_settled,
    } = view;
    // An address naming one task never opens without that task.
    let addressed = selection.sole_task();
    let enriched = answer
        .into_iter()
        .flat_map(|answer| &answer.nodes)
        .filter_map(|node| match node {
            TaskRelationNode::Task { task, .. } => Some((task, node)),
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let mut identities = BTreeMap::new();
    let mut local_to_merged = BTreeMap::new();
    let mut nodes = Vec::new();
    for (local_index, node) in local.nodes().iter().enumerate() {
        if (relation != TaskRelationKind::Dependency || !direction.includes_prerequisites())
            && !node.selected()
        {
            continue;
        }
        let enrichment = enriched.get(node.task()).copied();
        let identity = TaskRelationNodeRef::Task {
            task: node.task().clone(),
        };
        if addressed != Some(node.task())
            && ((!show_settled && local_settled(node, work, enrichment))
                || (!show_drafts && local_draft(node, work, enrichment)))
        {
            // Local standing wins over stale enrichment.
            identities.insert(identity, None);
            continue;
        }
        let index = nodes.len();
        identities.insert(identity, Some(index));
        local_to_merged.insert(local_index, index);
        nodes.push(present_local_node(node, index, work, enrichment));
    }

    let mut edge_keys = BTreeSet::new();
    let mut edges = Vec::new();
    if relation == TaskRelationKind::Dependency {
        push_local_then_edges(
            local,
            &local_to_merged,
            direction,
            &mut edges,
            &mut edge_keys,
        );
    }

    if let Some(answer) = answer {
        let by_identity = answer
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.identity(), index))
            .collect::<BTreeMap<_, _>>();
        let admitted_edges = admitted_edges(answer, relation, direction, show_controllers);
        let included = included_nodes(answer, &admitted_edges, relation, direction);
        for (answer_index, node) in answer.nodes.iter().enumerate() {
            if !included.contains(&answer_index) {
                continue;
            }
            let identity = node.identity();
            if identities.contains_key(&identity) {
                continue;
            }
            if (!show_settled && remote_settled(node)) || (!show_drafts && remote_draft(node, work))
            {
                continue;
            }
            let index = nodes.len();
            identities.insert(identity, Some(index));
            nodes.push(present_remote_node(node, index, selection, local_time));
        }
        for edge in admitted_edges {
            let old_from = *by_identity.get(&edge.from)?;
            let old_to = *by_identity.get(&edge.to)?;
            if !included.contains(&old_from) || !included.contains(&old_to) {
                continue;
            }
            let (from, to) = if edge.relation == TaskRelationKind::Dependency {
                (identities.get(&edge.to), identities.get(&edge.from))
            } else {
                (identities.get(&edge.from), identities.get(&edge.to))
            };
            // A hidden task drops its incident edges too.
            let (Some(&Some(from)), Some(&Some(to))) = (from, to) else {
                continue;
            };
            push_edge(&mut edges, &mut edge_keys, from, to, edge.relation);
        }
    }
    drop_orphan_task_neighbors(&mut nodes, &mut edges);

    let source = mermaid_source(&nodes, &edges);
    Some(Presentation {
        nodes,
        edges,
        source,
    })
}

fn admitted_edges(
    answer: &TaskRelationsAnswer,
    relation: TaskRelationKind,
    direction: WorkRelationshipDirection,
    show_controllers: bool,
) -> Vec<&TaskRelationEdge> {
    answer
        .edges
        .iter()
        .filter(|edge| {
            (edge.relation == relation
                && (relation != TaskRelationKind::Dependency
                    || direction != WorkRelationshipDirection::None))
                || (show_controllers && edge.relation == TaskRelationKind::Controller)
        })
        .collect()
}

/// Drop relationship task nodes that lost their last visible edge when a
/// settled endpoint was hidden. Selected roots and non-task evidence remain
/// independently meaningful; an unselected task does not. Survivors are
/// renumbered, and each takes the id of its new position, which edges, row
/// links, and overlay targets name.
fn drop_orphan_task_neighbors(nodes: &mut Vec<Node>, edges: &mut [Edge]) {
    let mut connected = vec![false; nodes.len()];
    for edge in edges.iter() {
        connected[edge.from] = true;
        connected[edge.to] = true;
    }
    let mut remap = vec![None; nodes.len()];
    let mut old_index = 0;
    let mut next_index = 0;
    nodes.retain_mut(|node| {
        let keep = node.selected || node.task.is_none() || connected[old_index];
        if keep {
            remap[old_index] = Some(next_index);
            if old_index != next_index {
                // Compaction only shortens the numeric suffix; keep its allocation.
                node.id.truncate(1);
                write!(node.id, "{next_index}").expect("String writes are infallible");
            }
            next_index += 1;
        }
        old_index += 1;
        keep
    });
    for edge in edges {
        edge.from = remap[edge.from].expect("connected source remains");
        edge.to = remap[edge.to].expect("connected target remains");
    }
}

fn push_edge(
    edges: &mut Vec<Edge>,
    keys: &mut BTreeSet<(usize, usize, &'static str)>,
    from: usize,
    to: usize,
    relation: TaskRelationKind,
) {
    let label = edge_label(relation);
    if keys.insert((from, to, label)) {
        edges.push(Edge {
            from,
            to,
            label,
            dotted: relation == TaskRelationKind::Controller,
        });
    }
}

/// Whether an admitted-board node names settled work: its own row decides,
/// and a row the board lacks takes the typed enrichment's status. Unknown
/// status is never treated as settled.
fn local_settled(
    node: &TaskOverviewNode,
    work: &WorkProjection<'_>,
    enrichment: Option<&TaskRelationNode>,
) -> bool {
    node.task_index().map_or_else(
        || {
            matches!(
                enrichment,
                Some(TaskRelationNode::Task { status, .. })
                    if *status != wire::WorkflowStatusKind::Pending
            )
        },
        |task_index| {
            !work.tasks()[task_index]
                .row()
                .workflow
                .body
                .status
                .is_pending()
        },
    )
}

/// Whether an enrichment node names settled work; non-task evidence is never
/// settled.
fn remote_settled(node: &TaskRelationNode) -> bool {
    matches!(
        node,
        TaskRelationNode::Task { status, .. }
            if *status != wire::WorkflowStatusKind::Pending
    )
}

fn local_draft(
    node: &TaskOverviewNode,
    work: &WorkProjection<'_>,
    enrichment: Option<&TaskRelationNode>,
) -> bool {
    node.task_index().map_or_else(
        || enrichment.is_some_and(|node| remote_draft(node, work)),
        |index| work.tasks()[index].row().workflow.body.phase == wire::Phase::Draft,
    )
}

fn remote_draft(node: &TaskRelationNode, work: &WorkProjection<'_>) -> bool {
    let TaskRelationNode::Task { task, phase, .. } = node else {
        return false;
    };
    let phase = work
        .tasks()
        .iter()
        .find(|candidate| candidate.row().statement.body.born == *task)
        .map_or(phase, |task| &task.row().workflow.body.phase);
    *phase == wire::Phase::Draft
}

/// The admitted board's own then arrows, from every prerequisite to the
/// dependent it blocks.
fn push_local_then_edges(
    local: &TaskOverviewGraph,
    local_to_merged: &BTreeMap<usize, usize>,
    direction: WorkRelationshipDirection,
    edges: &mut Vec<Edge>,
    keys: &mut BTreeSet<(usize, usize, &'static str)>,
) {
    if direction == WorkRelationshipDirection::None {
        return;
    }
    for edge in local.edges() {
        let (Some(&dependent), Some(&prerequisite)) = (
            local_to_merged.get(&edge.dependent()),
            local_to_merged.get(&edge.prerequisite()),
        ) else {
            continue;
        };
        if direction == WorkRelationshipDirection::Dependents
            && !(local.nodes()[edge.dependent()].selected()
                && local.nodes()[edge.prerequisite()].selected())
        {
            continue;
        }
        push_edge(
            edges,
            keys,
            prerequisite,
            dependent,
            TaskRelationKind::Dependency,
        );
    }
}

fn included_nodes(
    answer: &TaskRelationsAnswer,
    edges: &[&TaskRelationEdge],
    relation: TaskRelationKind,
    direction: WorkRelationshipDirection,
) -> BTreeSet<usize> {
    if relation != TaskRelationKind::Dependency || direction == WorkRelationshipDirection::Both {
        return (0..answer.nodes.len()).collect();
    }
    let by_identity = answer
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.identity(), index))
        .collect::<BTreeMap<_, _>>();
    let mut included = answer
        .roots
        .iter()
        .filter_map(|task| by_identity.get(&TaskRelationNodeRef::Task { task: task.clone() }))
        .copied()
        .collect::<BTreeSet<_>>();
    let mut queue = included.iter().copied().collect::<VecDeque<_>>();
    while let Some(current) = queue.pop_front() {
        let current_identity = answer.nodes[current].identity();
        for edge in edges {
            if edge.relation != TaskRelationKind::Dependency {
                continue;
            }
            let target = match direction {
                WorkRelationshipDirection::Prerequisites if edge.from == current_identity => {
                    by_identity.get(&edge.to).copied()
                }
                WorkRelationshipDirection::Dependents if edge.to == current_identity => {
                    by_identity.get(&edge.from).copied()
                }
                _ => None,
            };
            if let Some(target) = target
                && included.insert(target)
            {
                queue.push_back(target);
            }
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        for edge in edges
            .iter()
            .filter(|edge| edge.relation == TaskRelationKind::Controller)
        {
            let (Some(&from), Some(&to)) = (by_identity.get(&edge.from), by_identity.get(&edge.to))
            else {
                continue;
            };
            if included.contains(&from) || included.contains(&to) {
                changed |= included.insert(from);
                changed |= included.insert(to);
            }
        }
    }
    included
}

/// Present one node of the admitted-board graph. An admitted row is
/// authoritative; a node the board lacks, such as a completed task, takes the
/// same-stamp task from the typed enrichment answer when it has one.
fn present_local_node(
    node: &TaskOverviewNode,
    index: usize,
    work: &WorkProjection<'_>,
    enrichment: Option<&TaskRelationNode>,
) -> Node {
    let resolved = node
        .task_index()
        .map(|task_index| {
            let row = work.tasks()[task_index].row();
            (
                format!(
                    "{} · {}",
                    row.statement.body.handle,
                    compact_title(&row.presentation_title),
                ),
                format!("{} · {}", row.workflow.body.phase, row.workflow.body.status),
                None,
            )
        })
        .or_else(|| match enrichment {
            Some(TaskRelationNode::Task {
                handle,
                title,
                phase,
                status,
                ..
            }) => Some((
                format!("{handle} · {}", compact_title(title)),
                format!("{phase} · {status}"),
                Some(*status),
            )),
            _ => None,
        });
    let (kind, label, detail, status) = match (resolved, node.kind()) {
        (Some((label, detail, status)), _) if node.selected() => {
            ("Selected task", label, detail, status)
        }
        (Some((label, detail, status)), _) => ("External dependency", label, detail, status),
        (None, TaskOverviewNodeKind::Missing) => (
            "Missing task",
            node.task().to_string(),
            "Missing from the admitted board snapshot".to_owned(),
            None,
        ),
        (None, _) => (
            "Unavailable task",
            node.task().to_string(),
            "Unavailable in the admitted board snapshot".to_owned(),
            None,
        ),
    };
    Node {
        id: format!("n{index}"),
        kind,
        diagram: format!("{label}\n{detail}"),
        label,
        detail,
        at: None,
        task: Some(node.task().clone()),
        status,
        selected: node.selected(),
    }
}

fn present_remote_node(
    node: &TaskRelationNode,
    index: usize,
    selection: &TaskOverviewSelection,
    local_time: &dyn Fn(&wire::Instant) -> String,
) -> Node {
    let id = format!("n{index}");
    match node {
        TaskRelationNode::Task {
            task,
            handle,
            title,
            phase,
            status,
        } => {
            let title = compact_title(title);
            Node {
                id,
                kind: "Related task",
                label: format!("{handle} · {title}"),
                detail: format!("Phase {phase} · {status}"),
                at: None,
                diagram: format!("{handle}\n{title}\nPhase {phase} — {status}"),
                task: Some(task.clone()),
                status: Some(*status),
                selected: selection.tasks().contains(task),
            }
        }
        TaskRelationNode::Origin { origin, request } => {
            present_origin_node(id, origin, request.as_ref())
        }
        TaskRelationNode::Review {
            position,
            phase,
            author,
            reviewer,
            verdict,
            ..
        } => present_review_node(
            id,
            *position,
            phase.as_ref().zip(author.as_ref()),
            reviewer,
            *verdict,
        ),
        TaskRelationNode::Lifecycle {
            position,
            at,
            actor,
            event,
            ..
        } => {
            let position = wire::SequenceCode::new(position.get());
            let actor_label = compact_actor(actor);
            let event = lifecycle_label(*event);
            Node {
                id,
                kind: "Lifecycle transition",
                label: format!("{event} · {actor_label}"),
                detail: format!("{actor} · fact {position}"),
                at: Some(*at),
                diagram: format!(
                    "{event}\n{actor_label}\n{}\nfact {position}",
                    local_time(at)
                ),
                task: None,
                status: None,
                selected: false,
            }
        }
        TaskRelationNode::Claim {
            position,
            actor,
            lane,
            occupant,
            disposition,
            ..
        } => {
            let position = wire::SequenceCode::new(position.get());
            let actor_is_resident_lane =
                matches!(actor, Actor::Lane(actor_lane) if actor_lane == lane);
            let actor = compact_actor(actor);
            let disposition = disposition.as_str();
            let detail =
                format!("{disposition} · lane {lane} · occupant {occupant} · fact {position}");
            let lane = compact_identity(lane);
            let occupant = compact_identity(occupant);
            let owner = if actor_is_resident_lane {
                format!("lane {lane}")
            } else {
                format!("{actor}\nlane {lane}")
            };
            Node {
                id,
                kind: "Current possession",
                label: format!("Current claim · {actor}"),
                detail,
                at: None,
                diagram: format!(
                    "Current claim\n{owner}\n{disposition}\noccupant {occupant}\nfact {position}"
                ),
                task: None,
                status: None,
                selected: false,
            }
        }
    }
}

/// A steering origin reads as its request, its exact key kept beside it; any
/// other origin, or a key that resolved to no request, reads as the key.
fn present_origin_node(
    id: String,
    origin: &wire::TaskProvenance,
    request: Option<&wire::intent::OriginRequest>,
) -> Node {
    let (label, detail, diagram) = match request {
        Some(request) => {
            let summary = compact_title(&request.text);
            let mut text = request
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if request.truncated {
                text.push('…');
            }
            (
                format!("Request · {summary}"),
                format!("{origin} · {text}"),
                format!("{origin}\n{summary}"),
            )
        }
        None => (
            format!("Origin · {}", compact(&origin.to_string())),
            origin.to_string(),
            format!("Origin\n{origin}"),
        ),
    };
    Node {
        id,
        kind: "ACK / origin",
        label,
        detail,
        at: None,
        diagram,
        task: None,
        status: None,
        selected: false,
    }
}

/// A completed review, naming the phase it followed and that phase's author
/// only when the journal records one.
fn present_review_node(
    id: String,
    position: wire::Seq,
    implemented: Option<(&wire::Phase, &Actor)>,
    reviewer: &Actor,
    verdict: Verdict,
) -> Node {
    let position = wire::SequenceCode::new(position.get());
    let reviewer_label = compact_actor(reviewer);
    let verdict = verdict_label(verdict);
    let (label, detail, diagram) = match implemented {
        Some((phase, author)) => {
            let author_label = compact_actor(author);
            (
                format!("{phase} · {author_label} → {reviewer_label} · {verdict}"),
                format!(
                    "Phase {phase}: author {author} → reviewer {reviewer} → verdict {verdict} · fact {position}"
                ),
                format!(
                    "{phase} review\n{author_label} → {reviewer_label}\n{verdict}\nfact {position}"
                ),
            )
        }
        None => (
            format!("review · {reviewer_label} · {verdict}"),
            format!(
                "No completed phase before this review: reviewer {reviewer} → verdict {verdict} · fact {position}"
            ),
            format!("review\n{reviewer_label}\n{verdict}\nfact {position}"),
        ),
    };
    Node {
        id,
        kind: "Completed review",
        label,
        detail,
        at: None,
        diagram,
        task: None,
        status: None,
        selected: false,
    }
}

/// ELK's layered layout routes every edge orthogonally from its own attachment
/// point and spaces parallel runs apart, so each edge stays traceable from
/// source to target.
///
/// Mermaid roots that layout with included children, so ELK never separates
/// connected components and every unconnected root would share the first
/// layer: one column. Invisible links from [`row_links`] instead pack whole
/// components into rows, which ELK stacks in shared layers as a near-square
/// grid without moving any drawn edge.
///
/// Every task node is framed by its interactive overlay, which carries its
/// state and selection, so the diagram draws no stroke of its own around one.
fn mermaid_source(nodes: &[Node], edges: &[Edge]) -> String {
    let mut source = String::from("---\nconfig:\n  layout: elk\n---\nflowchart LR\n");
    for node in nodes {
        let label = crate::mermaid_label::multiline(&node.diagram);
        writeln!(source, "  {}[\"{label}\"]", node.id).expect("String writes are infallible");
    }
    for edge in edges {
        let arrow = if edge.dotted { "-.->" } else { "-->" };
        writeln!(
            source,
            "  n{} {arrow}|{}| n{}",
            edge.from, edge.label, edge.to
        )
        .expect("String writes are infallible");
    }
    for (from, to) in row_links(nodes.len(), edges) {
        writeln!(source, "  n{from} ~~~ n{to}").expect("String writes are infallible");
    }
    let tasks = nodes
        .iter()
        .filter(|node| node.task.is_some())
        .map(|node| node.id.as_str())
        .collect::<Vec<_>>();
    if !tasks.is_empty() {
        writeln!(source, "  class {} task", tasks.join(",")).expect("String writes are infallible");
        source.push_str("  classDef task stroke:none\n");
    }
    source
}

/// One connected component of the drawn graph, in node order.
struct Component {
    /// Layers the component spans left to right.
    depth: usize,
    /// The first node in its first layer.
    root: usize,
    /// The first node in its last layer.
    sink: usize,
}

/// Invisible links that place connected components side by side in rows.
///
/// Components keep node order. Each row takes whole components left to right
/// while their layers fit a budget of `max(⌈√nodes⌉, deepest component)`, so
/// a component never splits and the rows approach a square grid. Each link
/// joins one component's sink to the next component's root, so links never
/// join two nodes of one component. A single component needs no links.
fn row_links(node_count: usize, edges: &[Edge]) -> Vec<(usize, usize)> {
    let components = components(node_count, edges);
    if components.len() < 2 {
        return Vec::new();
    }
    let side = node_count.isqrt();
    let side = if side * side < node_count {
        side + 1
    } else {
        side
    };
    let budget = components
        .iter()
        .map(|component| component.depth)
        .fold(side, usize::max);
    let mut links = Vec::new();
    let mut used = 0;
    let mut previous: Option<&Component> = None;
    for component in &components {
        match previous {
            Some(previous) if used + component.depth <= budget => {
                links.push((previous.sink, component.root));
            }
            _ => used = 0,
        }
        used += component.depth;
        previous = Some(component);
    }
    links
}

/// Connected components ordered by their first node, each with its
/// longest-path layering. Kahn's order assigns layers, so a cycle leaves its
/// nodes at the layer their acyclic predecessors reach and still terminates.
fn components(node_count: usize, edges: &[Edge]) -> Vec<Component> {
    let mut neighbors = vec![Vec::new(); node_count];
    let mut successors = vec![Vec::new(); node_count];
    let mut incoming = vec![0usize; node_count];
    for edge in edges {
        neighbors[edge.from].push(edge.to);
        neighbors[edge.to].push(edge.from);
        successors[edge.from].push(edge.to);
        incoming[edge.to] += 1;
    }

    let mut layer = vec![0usize; node_count];
    let mut ready = (0..node_count)
        .filter(|&node| incoming[node] == 0)
        .collect::<VecDeque<_>>();
    while let Some(node) = ready.pop_front() {
        for &next in &successors[node] {
            layer[next] = layer[next].max(layer[node] + 1);
            incoming[next] -= 1;
            if incoming[next] == 0 {
                ready.push_back(next);
            }
        }
    }

    let mut seen = vec![false; node_count];
    let mut components = Vec::new();
    for start in 0..node_count {
        if seen[start] {
            continue;
        }
        seen[start] = true;
        let mut members = vec![start];
        let mut cursor = 0;
        while let Some(&node) = members.get(cursor) {
            cursor += 1;
            for &next in &neighbors[node] {
                if !seen[next] {
                    seen[next] = true;
                    members.push(next);
                }
            }
        }
        members.sort_unstable();
        let last = members.iter().map(|&node| layer[node]).max().unwrap_or(0);
        let first = members.iter().map(|&node| layer[node]).min().unwrap_or(0);
        let at = |wanted| members.iter().copied().find(|&node| layer[node] == wanted);
        components.push(Component {
            depth: last - first + 1,
            root: at(first).unwrap_or(start),
            sink: at(last).unwrap_or(start),
        });
    }
    components
}

fn compact(value: &str) -> String {
    const LIMIT: usize = 48;
    let mut value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if value.chars().count() > LIMIT {
        value = value.chars().take(LIMIT - 1).collect();
        value.push('…');
    }
    value
}

/// A task title's plain-text projection, compacted for a node label.
fn compact_title(title: &str) -> String {
    compact(&crate::composer_markdown::inline::plain_text(title))
}

fn compact_actor(actor: &Actor) -> String {
    match actor {
        Actor::Lane(lane) => format!("lane {}", compact_identity(lane)),
        Actor::Principal(principal) => format!("principal {}", compact_identity(principal)),
        Actor::Spice => "spice".to_owned(),
    }
}

fn compact_identity(identity: impl ToString) -> String {
    let mut identity = identity.to_string();
    identity.truncate(8);
    identity
}

const fn edge_label(relation: TaskRelationKind) -> &'static str {
    match relation {
        TaskRelationKind::Origin => "origin",
        TaskRelationKind::Dependency => "then",
        TaskRelationKind::Controller => "controls",
        TaskRelationKind::Review => "review",
        TaskRelationKind::Lifecycle => "transition",
        TaskRelationKind::Claim => "held by",
    }
}

fn verdict_label(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Clean => "clean",
        Verdict::Finding => "finding",
    }
}

fn lifecycle_label(event: TaskLifecycleKind) -> &'static str {
    match event {
        TaskLifecycleKind::Added => "Added",
        TaskLifecycleKind::Edited => "Edited",
        TaskLifecycleKind::Phased => "Phase changed",
        TaskLifecycleKind::Completed => "Phase completed",
        TaskLifecycleKind::Assigned => "Reviewer assigned",
        TaskLifecycleKind::Reviewed => "Reviewed",
        TaskLifecycleKind::Deferred => "Deferred",
        TaskLifecycleKind::Woken => "Woken",
        TaskLifecycleKind::Submitted => "Submitted",
        TaskLifecycleKind::Deleted => "Deleted",
    }
}

#[cfg(test)]
mod tests;
