use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::*;
use wire::intent::{
    OriginRequest, TASK_RELATION_DEPTH_LIMIT, TASK_RELATION_EDGE_LIMIT,
    TASK_RELATION_FRONTIER_LIMIT, TASK_RELATION_NODE_LIMIT, TASK_RELATION_ROOT_LIMIT,
    TaskLifecycleKind, TaskRelationEdge, TaskRelationFrontier, TaskRelationKind, TaskRelationNode,
    TaskRelationNodeRef, TaskRelationTruncation, TaskRelations, TaskRelationsAnswer,
};

#[derive(Clone)]
struct Candidate {
    edge: TaskRelationEdge,
    target: TaskRelationNode,
}

#[derive(Clone)]
struct ReviewRecord {
    task: TaskStamp,
    position: Seq,
    /// The most recent completed phase before the review and its author.
    implemented: Option<(wire::Phase, Actor)>,
    reviewer: Actor,
    verdict: body::Verdict,
}

/// Children by origin parent and by controller, limited to the given rows.
#[derive(Default)]
struct ReverseRelations {
    origins: BTreeMap<TaskStamp, BTreeSet<TaskStamp>>,
    controllers: BTreeMap<TaskStamp, BTreeSet<TaskStamp>>,
}

impl ReverseRelations {
    fn new<V>(
        tasks: &BTreeMap<TaskStamp, V>,
        links: impl Fn(&V) -> (&wire::TaskProvenance, Option<&wire::TaskPreparation>),
    ) -> Self {
        let mut reverse = Self::default();
        for (task, row) in tasks {
            let (provenance, preparation) = links(row);
            let controller = preparation.map(|preparation| &preparation.controller);
            for (index, link) in [
                (&mut reverse.origins, provenance.parent()),
                (&mut reverse.controllers, controller),
            ] {
                if let Some(link) = link.filter(|link| tasks.contains_key(*link)) {
                    index.entry(link.clone()).or_default().insert(task.clone());
                }
            }
        }
        reverse
    }
}

#[derive(Default)]
struct Omitted {
    frontier: BTreeSet<TaskRelationFrontier>,
    beyond_frontier: u32,
}

impl Omitted {
    fn push(&mut self, depth: u8, edge: TaskRelationEdge) {
        let omitted = TaskRelationFrontier { depth, edge };
        if self.frontier.contains(&omitted) {
            return;
        }
        if self.frontier.len() < TASK_RELATION_FRONTIER_LIMIT {
            self.frontier.insert(omitted);
        } else {
            self.beyond_frontier = self.beyond_frontier.saturating_add(1);
        }
    }

    fn finish(self) -> Option<TaskRelationTruncation> {
        (!self.frontier.is_empty() || self.beyond_frontier > 0).then(|| TaskRelationTruncation {
            frontier: self.frontier.into_iter().collect(),
            omitted: self.beyond_frontier,
        })
    }
}

/// Read one bounded typed relationship projection from an immutable journal barrier.
pub(in crate::board_command) fn read_task_relations(
    bus: &Bus,
    repository: Option<&Repository>,
    request: &TaskRelations,
    principal: Actor,
) -> Outcome<CommandAnswer> {
    let (roots, relations) = match validate(request) {
        Ok(validated) => validated,
        Err(outcome) => return outcome,
    };
    let observed = match bus.task_snapshot() {
        Ok(observed) => observed,
        Err(error) => return store_fault("board::heads-unreadable", error.to_string()),
    };
    let selected = relationship_tasks(&observed, &roots, &relations, request.depth);
    let snapshot = match validated_snapshot_from_with_occupant(
        &observed,
        repository,
        principal,
        None,
        Some(&selected),
        false,
    ) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let tasks = snapshot
        .answer
        .tasks
        .iter()
        .map(|task| (task.statement.body.born.clone(), task))
        .collect::<BTreeMap<_, _>>();
    if let Some(unknown) = roots.iter().find(|root| !tasks.contains_key(*root)) {
        return refusal(
            "task::relations-unknown",
            Some(unknown.to_string()),
            "read the board and request current durable task identities visible to this principal",
        );
    }
    let review_records = match review_records(observed.facts().iter(), &tasks) {
        Ok(records) => records,
        Err(detail) => return store_fault("task::relations-review-invalid", &detail),
    };
    let lifecycle = lifecycle_records(observed.facts().iter(), &tasks);
    let dependencies = DependencyIndex::new(&tasks);
    let reverse = ReverseRelations::new(&tasks, |row| {
        let statement = &row.statement.body;
        (&statement.provenance, statement.preparation.as_ref())
    });
    let mut walk = Walk::seed(&roots, &tasks);
    while let Some((task, depth)) = walk.topic.pop_front() {
        if !walk.expanded.insert(task.clone()) {
            continue;
        }
        let candidates = candidates(
            &task,
            &relations,
            &tasks,
            &dependencies,
            &reverse,
            &review_records,
            &lifecycle,
        );
        walk.admit(depth, candidates, request);
    }
    if let Err(outcome) = attach_origin_requests(bus, &mut walk.nodes) {
        return outcome;
    }
    let answer = walk.answer(snapshot.cursor, roots);
    snapshot_reply(
        snapshot.cursor,
        CommandAnswer::TaskRelations(Box::new(answer)),
    )
}

/// The bounded breadth-first state one relationship read expands from its
/// roots.
#[derive(Default)]
struct Walk {
    nodes: BTreeMap<TaskRelationNodeRef, TaskRelationNode>,
    topic: VecDeque<(TaskStamp, u8)>,
    expanded: BTreeSet<TaskStamp>,
    edges: BTreeSet<TaskRelationEdge>,
    omitted: Omitted,
}

impl Walk {
    fn seed(roots: &[TaskStamp], tasks: &BTreeMap<TaskStamp, &BoardTask>) -> Self {
        let mut walk = Self::default();
        for root in roots {
            let node = task_node(tasks[root]);
            walk.nodes.insert(node.identity(), node);
            walk.topic.push_back((root.clone(), 0));
        }
        walk
    }

    /// Admit one expanded task's distinct candidates in edge order. A
    /// candidate past the requested depth or node limit, or past the edge
    /// limit, joins the truncation frontier instead.
    fn admit(&mut self, depth: u8, mut candidates: Vec<Candidate>, request: &TaskRelations) {
        candidates.sort_by(|left, right| left.edge.cmp(&right.edge));
        candidates.dedup_by(|left, right| left.edge == right.edge);
        for candidate in candidates {
            let target = candidate.target.identity();
            let target_exists = self.nodes.contains_key(&target);
            if (!target_exists
                && (depth >= request.depth || self.nodes.len() >= usize::from(request.node_limit)))
                || (!self.edges.contains(&candidate.edge)
                    && self.edges.len() >= TASK_RELATION_EDGE_LIMIT)
            {
                self.omitted.push(depth.saturating_add(1), candidate.edge);
                continue;
            }
            if !target_exists {
                if let TaskRelationNodeRef::Task { task } = &target {
                    self.topic
                        .push_back((task.clone(), depth.saturating_add(1)));
                }
                self.nodes.insert(target, candidate.target);
            }
            self.edges.insert(candidate.edge);
        }
    }

    fn answer(self, snapshot: Seq, roots: Vec<TaskStamp>) -> TaskRelationsAnswer {
        TaskRelationsAnswer {
            snapshot,
            roots,
            nodes: self.nodes.into_values().collect(),
            edges: self.edges.into_iter().collect(),
            truncation: self.omitted.finish(),
        }
    }
}

/// Attach to each steering origin node the request its key names, from one
/// journal read; a key no directive carries keeps none.
fn attach_origin_requests(
    bus: &Bus,
    nodes: &mut BTreeMap<TaskRelationNodeRef, TaskRelationNode>,
) -> Result<(), Outcome<CommandAnswer>> {
    let keys = nodes
        .keys()
        .filter_map(|node| match node {
            TaskRelationNodeRef::Origin {
                origin: wire::TaskProvenance::Steering(key),
            } => Some(key.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if keys.is_empty() {
        return Ok(());
    }
    let requests = steering_requests(bus, keys)?;
    for (key, resolved) in requests {
        let origin = TaskRelationNodeRef::Origin {
            origin: wire::TaskProvenance::Steering(key),
        };
        if let Some(TaskRelationNode::Origin { request, .. }) = nodes.get_mut(&origin) {
            *request = resolved;
        }
    }
    Ok(())
}

/// The request each steering key names, from one journal read; a key no
/// directive carries resolves to none.
fn steering_requests(
    bus: &Bus,
    keys: Vec<String>,
) -> Result<Vec<(String, Option<OriginRequest>)>, Outcome<CommandAnswer>> {
    bus.read(move |read| {
        keys.into_iter()
            .map(|key| {
                let request = read
                    .steering_key(&key)?
                    .map(|head| OriginRequest::preview(&head.body().text));
                Ok((key, request))
            })
            .collect::<Result<Vec<_>, journal::GuardError>>()
    })
    .map_err(|error| store_fault("task::steering-request-unreadable", error.to_string()))
}

fn relationship_tasks(
    observed: &bus::TaskSnapshot,
    roots: &[TaskStamp],
    relations: &BTreeSet<TaskRelationKind>,
    depth: u8,
) -> BTreeSet<TaskStamp> {
    let tasks = observed
        .tasks()
        .iter()
        .map(|heads| (heads.statement().body().born.clone(), heads))
        .collect::<BTreeMap<_, _>>();
    let mut reverse_dependencies = BTreeMap::<TaskStamp, BTreeSet<TaskStamp>>::new();
    let reverse = ReverseRelations::new(&tasks, |heads| {
        let statement = heads.statement().body();
        (&statement.provenance, statement.preparation.as_ref())
    });
    for (task, heads) in &tasks {
        let statement = heads.statement().body();
        for dependency in &statement.dependencies {
            if tasks.contains_key(dependency) {
                reverse_dependencies
                    .entry(dependency.clone())
                    .or_default()
                    .insert(task.clone());
            }
        }
    }
    let mut selected = roots.iter().cloned().collect::<BTreeSet<_>>();
    let mut topic = roots
        .iter()
        .cloned()
        .map(|task| (task, 0_u8))
        .collect::<VecDeque<_>>();
    let mut expanded = BTreeSet::new();
    while let Some((task, current_depth)) = topic.pop_front() {
        if !expanded.insert(task.clone()) {
            continue;
        }
        let Some(heads) = tasks.get(&task) else {
            continue;
        };
        let statement = heads.statement().body();
        let mut neighbors = BTreeSet::new();
        if relations.contains(&TaskRelationKind::Dependency) {
            neighbors.extend(
                statement
                    .dependencies
                    .iter()
                    .filter(|dependency| tasks.contains_key(*dependency))
                    .cloned(),
            );
            neighbors.extend(
                reverse_dependencies
                    .get(&task)
                    .into_iter()
                    .flatten()
                    .cloned(),
            );
        }
        if relations.contains(&TaskRelationKind::Origin) {
            neighbors.extend(
                statement
                    .provenance
                    .parent()
                    .filter(|parent| tasks.contains_key(*parent))
                    .cloned(),
            );
            neighbors.extend(reverse.origins.get(&task).into_iter().flatten().cloned());
        }
        if relations.contains(&TaskRelationKind::Controller) {
            add_controller_neighbors(&mut neighbors, statement, &tasks, &reverse.controllers);
        }
        for neighbor in neighbors {
            selected.insert(neighbor.clone());
            if current_depth < depth {
                topic.push_back((neighbor, current_depth.saturating_add(1)));
            }
        }
    }
    selected
}

fn add_controller_neighbors<V>(
    neighbors: &mut BTreeSet<TaskStamp>,
    statement: &wire::TaskHead,
    tasks: &BTreeMap<TaskStamp, V>,
    reverse: &BTreeMap<TaskStamp, BTreeSet<TaskStamp>>,
) {
    neighbors.extend(
        statement
            .preparation
            .as_ref()
            .map(|preparation| &preparation.controller)
            .filter(|controller| tasks.contains_key(*controller))
            .cloned(),
    );
    neighbors.extend(reverse.get(&statement.born).into_iter().flatten().cloned());
}

fn validate(
    request: &TaskRelations,
) -> Result<(Vec<TaskStamp>, BTreeSet<TaskRelationKind>), Outcome<CommandAnswer>> {
    if request.roots.is_empty() || request.roots.len() > TASK_RELATION_ROOT_LIMIT {
        return Err(refusal(
            "task::relations-roots-invalid",
            Some(request.roots.len().to_string()),
            "request between one and eight durable task roots",
        ));
    }
    let roots = request.roots.iter().cloned().collect::<BTreeSet<_>>();
    if roots.len() != request.roots.len() {
        return Err(refusal(
            "task::relations-roots-duplicate",
            None,
            "request each durable task root once",
        ));
    }
    let relations = request.relations.iter().copied().collect::<BTreeSet<_>>();
    if relations.is_empty() || relations.len() != request.relations.len() {
        return Err(refusal(
            "task::relations-kinds-invalid",
            Some(request.relations.len().to_string()),
            "request one or more distinct typed relation kinds",
        ));
    }
    if request.depth > TASK_RELATION_DEPTH_LIMIT {
        return Err(refusal(
            "task::relations-depth-excessive",
            Some(request.depth.to_string()),
            "request a relationship depth no greater than four",
        ));
    }
    if request.node_limit > TASK_RELATION_NODE_LIMIT
        || usize::from(request.node_limit) < roots.len()
    {
        return Err(refusal(
            "task::relations-node-limit-invalid",
            Some(request.node_limit.to_string()),
            "request a node limit that contains every root and does not exceed 128",
        ));
    }
    Ok((roots.into_iter().collect(), relations))
}

fn task_node(task: &BoardTask) -> TaskRelationNode {
    TaskRelationNode::Task {
        task: task.statement.body.born.clone(),
        handle: task.statement.body.handle.clone(),
        title: task.presentation_title.clone(),
        pitch: task.statement.body.pitch.clone(),
        phase: task.workflow.body.phase,
        status: task.workflow.body.status.kind(),
    }
}

fn candidates(
    task: &TaskStamp,
    relations: &BTreeSet<TaskRelationKind>,
    tasks: &BTreeMap<TaskStamp, &BoardTask>,
    dependencies: &DependencyIndex,
    reverse: &ReverseRelations,
    reviews: &BTreeMap<TaskStamp, Vec<ReviewRecord>>,
    lifecycle: &BTreeMap<TaskStamp, Vec<TaskRelationNode>>,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    let source = TaskRelationNodeRef::Task { task: task.clone() };
    let row = tasks[task];
    if relations.contains(&TaskRelationKind::Origin) {
        let origin = &row.statement.body.provenance;
        let target = match origin.parent() {
            Some(parent) if tasks.contains_key(parent) => task_node(tasks[parent]),
            _ => TaskRelationNode::Origin {
                origin: origin.clone(),
                request: None,
            },
        };
        candidates.push(candidate(TaskRelationKind::Origin, source.clone(), target));
        if let Some(children) = reverse.origins.get(task) {
            for child in children {
                let target = task_node(tasks[child]);
                candidates.push(Candidate {
                    edge: TaskRelationEdge {
                        relation: TaskRelationKind::Origin,
                        from: target.identity(),
                        to: source.clone(),
                    },
                    target,
                });
            }
        }
    }
    if relations.contains(&TaskRelationKind::Dependency) {
        let prerequisites = &row.statement.body.dependencies;
        for dependency in prerequisites {
            if let Some(dependency) = tasks.get(dependency) {
                candidates.push(candidate(
                    TaskRelationKind::Dependency,
                    source.clone(),
                    task_node(dependency),
                ));
            }
        }
        if let Some(dependents) = dependencies.reverse.get(task) {
            for dependent in dependents {
                let target = task_node(tasks[dependent]);
                candidates.push(Candidate {
                    edge: TaskRelationEdge {
                        relation: TaskRelationKind::Dependency,
                        from: target.identity(),
                        to: source.clone(),
                    },
                    target,
                });
            }
        }
    }
    if relations.contains(&TaskRelationKind::Controller) {
        add_controller_candidates(
            &mut candidates,
            task,
            row,
            &source,
            tasks,
            &reverse.controllers,
        );
    }
    if relations.contains(&TaskRelationKind::Review) {
        for review in reviews.get(task).into_iter().flatten() {
            let (phase, author) = review.implemented.clone().unzip();
            let target = TaskRelationNode::Review {
                task: review.task.clone(),
                position: review.position,
                phase,
                author,
                reviewer: review.reviewer.clone(),
                verdict: review.verdict,
            };
            candidates.push(candidate(TaskRelationKind::Review, source.clone(), target));
        }
    }
    if relations.contains(&TaskRelationKind::Lifecycle) {
        for target in lifecycle.get(task).into_iter().flatten().cloned() {
            candidates.push(candidate(
                TaskRelationKind::Lifecycle,
                source.clone(),
                target,
            ));
        }
    }
    if relations.contains(&TaskRelationKind::Claim) {
        for claim in row.active_claims().map(|held| &held.claim) {
            let target = TaskRelationNode::Claim {
                task: task.clone(),
                position: claim.position,
                actor: claim.body.actor.clone(),
                lane: claim.body.lane,
                occupant: claim.body.occupant,
                disposition: claim.body.disposition,
            };
            candidates.push(candidate(TaskRelationKind::Claim, source.clone(), target));
        }
    }
    candidates
}

fn add_controller_candidates(
    candidates: &mut Vec<Candidate>,
    task: &TaskStamp,
    row: &BoardTask,
    source: &TaskRelationNodeRef,
    tasks: &BTreeMap<TaskStamp, &BoardTask>,
    reverse: &BTreeMap<TaskStamp, BTreeSet<TaskStamp>>,
) {
    if let Some(controller) = row
        .statement
        .body
        .preparation
        .as_ref()
        .map(|preparation| &preparation.controller)
        .filter(|controller| tasks.contains_key(*controller))
    {
        let target = task_node(tasks[controller]);
        candidates.push(Candidate {
            edge: TaskRelationEdge {
                relation: TaskRelationKind::Controller,
                from: target.identity(),
                to: source.clone(),
            },
            target,
        });
    }
    if let Some(controlled) = reverse.get(task) {
        for controlled in controlled {
            candidates.push(candidate(
                TaskRelationKind::Controller,
                source.clone(),
                task_node(tasks[controlled]),
            ));
        }
    }
}

fn candidate(
    relation: TaskRelationKind,
    from: TaskRelationNodeRef,
    target: TaskRelationNode,
) -> Candidate {
    Candidate {
        edge: TaskRelationEdge {
            relation,
            from,
            to: target.identity(),
        },
        target,
    }
}

/// Reverse ordinary dependency edges for each task.
struct DependencyIndex {
    reverse: BTreeMap<TaskStamp, BTreeSet<TaskStamp>>,
}

impl DependencyIndex {
    fn new(tasks: &BTreeMap<TaskStamp, &BoardTask>) -> Self {
        let mut reverse = BTreeMap::<TaskStamp, BTreeSet<TaskStamp>>::new();
        for (task, row) in tasks {
            for dependency in &row.statement.body.dependencies {
                if tasks.contains_key(dependency) {
                    reverse
                        .entry(dependency.clone())
                        .or_default()
                        .insert(task.clone());
                }
            }
        }
        Self { reverse }
    }
}

fn lifecycle_records<'a>(
    facts: impl IntoIterator<Item = &'a wire::Envelope<wire::AnyFact>>,
    tasks: &BTreeMap<TaskStamp, &BoardTask>,
) -> BTreeMap<TaskStamp, Vec<TaskRelationNode>> {
    let mut records = BTreeMap::<TaskStamp, Vec<TaskRelationNode>>::new();
    let mut ordered = facts.into_iter().collect::<Vec<_>>();
    ordered.sort_by_key(|envelope| envelope.seq);
    for envelope in ordered {
        let Some((task, event)) = lifecycle_event(envelope) else {
            continue;
        };
        if !tasks.contains_key(&task) {
            continue;
        }
        records
            .entry(task.clone())
            .or_default()
            .push(TaskRelationNode::Lifecycle {
                task,
                position: envelope.seq,
                at: envelope.at,
                actor: envelope.actor.clone(),
                event,
            });
    }
    records
}

fn lifecycle_event(
    envelope: &wire::Envelope<wire::AnyFact>,
) -> Option<(TaskStamp, TaskLifecycleKind)> {
    let (task, event) = match envelope.fact.kind() {
        wire::FactKind::TaskAdded => (
            envelope.fact.read::<body::TaskAdded>()?.task,
            TaskLifecycleKind::Added,
        ),
        wire::FactKind::TaskEdited => (
            envelope.fact.read::<body::TaskEdited>()?.task,
            TaskLifecycleKind::Edited,
        ),
        wire::FactKind::TaskPhased => (
            envelope.fact.read::<body::TaskPhased>()?.task,
            TaskLifecycleKind::Phased,
        ),
        wire::FactKind::TaskDone => (
            envelope.fact.read::<body::TaskDone>()?.task,
            TaskLifecycleKind::Completed,
        ),
        wire::FactKind::TaskClosed => (
            envelope.fact.read::<body::TaskClosed>()?.task,
            TaskLifecycleKind::Completed,
        ),
        wire::FactKind::TaskConcluded => (
            envelope.fact.read::<body::TaskConcluded>()?.task,
            TaskLifecycleKind::Completed,
        ),
        wire::FactKind::TaskAssigned => (
            envelope.fact.read::<body::TaskAssigned>()?.task,
            TaskLifecycleKind::Assigned,
        ),
        wire::FactKind::TaskReviewed => (
            envelope.fact.read::<body::TaskReviewed>()?.task,
            TaskLifecycleKind::Reviewed,
        ),
        wire::FactKind::TaskDeferred => (
            envelope.fact.read::<body::TaskDeferred>()?.task,
            TaskLifecycleKind::Deferred,
        ),
        wire::FactKind::TaskWoken => (
            envelope.fact.read::<body::TaskWoken>()?.task,
            TaskLifecycleKind::Woken,
        ),
        wire::FactKind::TaskSubmitted => (
            envelope.fact.read::<body::TaskSubmitted>()?.task,
            TaskLifecycleKind::Submitted,
        ),
        wire::FactKind::TaskDeleted => (
            envelope.fact.read::<body::TaskDeleted>()?.task,
            TaskLifecycleKind::Deleted,
        ),
        _ => return None,
    };
    Some((task, event))
}

/// Each named task's reviews, after proving the recorded reviewer is the fact
/// actor.
fn review_records<'a>(
    facts: impl IntoIterator<Item = &'a wire::Envelope<wire::AnyFact>>,
    tasks: &BTreeMap<TaskStamp, &BoardTask>,
) -> Result<BTreeMap<TaskStamp, Vec<ReviewRecord>>, String> {
    let mut ordered = facts.into_iter().collect::<Vec<_>>();
    ordered.sort_by_key(|envelope| envelope.seq);
    let mut phases = BTreeMap::<TaskStamp, wire::Phase>::new();
    let mut authors = BTreeMap::<TaskStamp, (wire::Phase, Actor)>::new();
    let mut records = BTreeMap::<TaskStamp, Vec<ReviewRecord>>::new();
    for envelope in ordered {
        match envelope.fact.kind() {
            wire::FactKind::TaskAdded => {
                let added = envelope
                    .fact
                    .read::<body::TaskAdded>()
                    .expect("registered task-added fact decodes");
                if tasks.contains_key(&added.task) {
                    phases.insert(added.task, added.phase);
                }
            }
            wire::FactKind::TaskDone => {
                let done = envelope
                    .fact
                    .read::<body::TaskDone>()
                    .expect("registered task-done fact decodes");
                if let Some(phase) = phases.get(&done.task).cloned() {
                    authors.insert(done.task, (phase, envelope.actor.clone()));
                }
            }
            wire::FactKind::TaskPhased => {
                let phased = envelope
                    .fact
                    .read::<body::TaskPhased>()
                    .expect("registered task-phased fact decodes");
                if tasks.contains_key(&phased.task) {
                    phases.insert(phased.task, phased.phase);
                }
            }
            wire::FactKind::TaskReviewed => {
                let reviewed = envelope
                    .fact
                    .read::<body::TaskReviewed>()
                    .expect("registered task-reviewed fact decodes");
                if !tasks.contains_key(&reviewed.task) {
                    continue;
                }
                if reviewed.reviewer != envelope.actor {
                    return Err(format!(
                        "task {} review at {} names reviewer {} under fact actor {}",
                        reviewed.task, envelope.seq, reviewed.reviewer, envelope.actor
                    ));
                }
                let implemented = authors.get(&reviewed.task).cloned();
                records
                    .entry(reviewed.task.clone())
                    .or_default()
                    .push(ReviewRecord {
                        task: reviewed.task,
                        position: envelope.seq,
                        implemented,
                        reviewer: reviewed.reviewer,
                        verdict: reviewed.verdict,
                    });
            }
            _ => {}
        }
    }
    Ok(records)
}
