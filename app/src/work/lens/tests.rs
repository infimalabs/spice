use wire::intent::{TaskRelationEdge, TaskRelationKind, TaskRelationNode, TaskRelationNodeRef};

use super::*;
use crate::work::tests::{board, stamp, task};

fn remote_task(
    value: i64,
    title: &str,
    phase: &str,
    status: wire::WorkflowStatusKind,
) -> TaskRelationNode {
    TaskRelationNode::Task {
        task: stamp(value),
        handle: wire::TaskHandle::for_topic(
            &"/remote/fixture"
                .parse::<wire::Topic>()
                .expect("fixture topic"),
            &stamp(value),
        ),
        title: title.to_owned(),
        phase: phase.parse().expect("fixture phase"),
        status,
    }
}

fn answer(nodes: Vec<TaskRelationNode>) -> TaskRelationsAnswer {
    TaskRelationsAnswer {
        snapshot: wire::Seq::new(7).expect("fixture snapshot"),
        roots: vec![stamp(1), stamp(2)],
        nodes,
        edges: vec![TaskRelationEdge {
            relation: TaskRelationKind::Dependency,
            from: TaskRelationNodeRef::Task { task: stamp(1) },
            to: TaskRelationNodeRef::Task { task: stamp(2) },
        }],
        truncation: None,
    }
}

fn dependency_view(show_settled: bool) -> LensView {
    LensView {
        relation: TaskRelationKind::Dependency,
        direction: WorkRelationshipDirection::Both,
        show_controllers: true,
        show_drafts: true,
        show_settled,
    }
}

#[test]
fn dependency_directions_compose_without_hiding_controller_relationships() {
    let mut root = task(1, "/alpha");
    root.statement.body.dependencies = vec![stamp(2)];
    let board = board(vec![root, task(2, "/alpha")]);
    let work = WorkProjection::from_board_and_lanes(&board, None);
    let selection = TaskOverviewSelection::new(
        [stamp(1), stamp(2)],
        super::super::TaskOverviewContext::Task,
    );
    let mut enriched = answer(
        (1..=5)
            .map(|id| {
                remote_task(
                    id,
                    "Related task",
                    "todo",
                    wire::WorkflowStatusKind::Pending,
                )
            })
            .collect(),
    );
    enriched.edges.extend([
        TaskRelationEdge {
            relation: TaskRelationKind::Dependency,
            from: TaskRelationNodeRef::Task { task: stamp(2) },
            to: TaskRelationNodeRef::Task { task: stamp(3) },
        },
        TaskRelationEdge {
            relation: TaskRelationKind::Dependency,
            from: TaskRelationNodeRef::Task { task: stamp(4) },
            to: TaskRelationNodeRef::Task { task: stamp(1) },
        },
        TaskRelationEdge {
            relation: TaskRelationKind::Controller,
            from: TaskRelationNodeRef::Task { task: stamp(5) },
            to: TaskRelationNodeRef::Task { task: stamp(1) },
        },
    ]);
    for (direction, expected) in [
        (WorkRelationshipDirection::None, vec![(5, 1)]),
        (
            WorkRelationshipDirection::Prerequisites,
            vec![(2, 1), (3, 2), (5, 1)],
        ),
        (
            WorkRelationshipDirection::Dependents,
            vec![(1, 4), (2, 1), (5, 1)],
        ),
        (
            WorkRelationshipDirection::Both,
            vec![(1, 4), (2, 1), (3, 2), (5, 1)],
        ),
    ] {
        let presentation = super::project(
            &work.overview_graph(&selection),
            &work,
            Some(&enriched),
            LensView {
                direction,
                ..dependency_view(true)
            },
            &selection,
            &|at| at.to_string(),
        )
        .expect("direction presentation");
        let edges = presentation
            .edges
            .iter()
            .map(|edge| {
                (
                    presentation.nodes[edge.from]
                        .task
                        .clone()
                        .expect("task source"),
                    presentation.nodes[edge.to]
                        .task
                        .clone()
                        .expect("task target"),
                )
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            edges,
            expected
                .into_iter()
                .map(|(from, to)| (stamp(from), stamp(to)))
                .collect(),
            "{direction:?}",
        );
        assert!(
            presentation
                .nodes
                .iter()
                .any(|node| node.task == Some(stamp(1)))
        );
        assert!(
            presentation
                .nodes
                .iter()
                .any(|node| node.task == Some(stamp(2)))
        );
    }
}

#[test]
fn draft_visibility_uses_local_phase_and_preserves_the_addressed_task() {
    let mut draft = task(1, "/alpha");
    draft.workflow.body.phase = wire::Phase::Draft;
    let mut related = task(4, "/alpha");
    related.workflow.body.phase = wire::Phase::Draft;
    let board = board(vec![draft, task(2, "/alpha"), related]);
    let work = WorkProjection::from_board_and_lanes(&board, None);
    let selection = TaskOverviewSelection::new(
        [stamp(1), stamp(2), stamp(3)],
        super::super::TaskOverviewContext::Task,
    );
    let mut enriched = answer(vec![
        remote_task(1, "Stale live", "todo", wire::WorkflowStatusKind::Pending),
        remote_task(2, "Stale draft", "draft", wire::WorkflowStatusKind::Pending),
        remote_task(
            3,
            "Remote draft",
            "draft",
            wire::WorkflowStatusKind::Pending,
        ),
        remote_task(
            4,
            "Stale related phase",
            "todo",
            wire::WorkflowStatusKind::Pending,
        ),
    ]);
    enriched.edges.push(TaskRelationEdge {
        relation: TaskRelationKind::Controller,
        from: TaskRelationNodeRef::Task { task: stamp(4) },
        to: TaskRelationNodeRef::Task { task: stamp(2) },
    });
    let drawn = |selection: &TaskOverviewSelection, show_drafts| {
        super::project(
            &work.overview_graph(selection),
            &work,
            Some(&enriched),
            LensView {
                show_drafts,
                ..dependency_view(false)
            },
            selection,
            &|at| at.to_string(),
        )
        .expect("draft presentation")
    };
    let hidden = drawn(&selection, false);
    assert_eq!(
        hidden
            .nodes
            .iter()
            .map(|node| node.task.clone())
            .collect::<Vec<_>>(),
        [Some(stamp(2))],
    );
    assert!(hidden.edges.is_empty());
    let shown = drawn(&selection, true);
    assert_eq!(
        shown
            .nodes
            .iter()
            .map(|node| node.task.clone())
            .collect::<BTreeSet<_>>(),
        [
            Some(stamp(1)),
            Some(stamp(2)),
            Some(stamp(3)),
            Some(stamp(4))
        ]
        .into(),
    );
    for id in [1, 3] {
        let addressed = drawn(&TaskOverviewSelection::task(stamp(id)), false);
        assert!(
            addressed
                .nodes
                .iter()
                .any(|node| node.task == Some(stamp(id)))
        );
    }
}

fn settled_at() -> wire::Instant {
    "2026-09-23T06:00:00.000Z"
        .parse()
        .expect("fixture settlement instant")
}

/// A selected task the open board no longer admits, such as a completed
/// prerequisite, resolves from the same-stamp task in the typed answer while
/// admitted rows stay authoritative and no node or edge is duplicated.
#[test]
fn enrichment_resolves_a_missing_selected_task_in_place() {
    let mut open = task(1, "/alpha/open");
    open.statement.body.dependencies = vec![stamp(2)];
    let board = board(vec![open]);
    let work = WorkProjection::from_board_and_lanes(&board, None);
    let selection = TaskOverviewSelection::new(
        [stamp(1), stamp(2)],
        super::super::TaskOverviewContext::Task,
    );
    let local = work.overview_graph(&selection);
    let project = |answer: Option<&TaskRelationsAnswer>| {
        super::project(
            &local,
            &work,
            answer,
            dependency_view(true),
            &selection,
            &|at| at.to_string(),
        )
        .expect("overview presentation")
    };

    let unresolved = project(None);
    assert_eq!(unresolved.nodes[1].kind, "Missing task");

    let enriched = answer(vec![
        remote_task(
            1,
            "remote title",
            "review",
            wire::WorkflowStatusKind::Pending,
        ),
        remote_task(
            2,
            "Completed prerequisite",
            "review",
            wire::WorkflowStatusKind::Completed,
        ),
    ]);
    let resolved = project(Some(&enriched));
    let tasks = resolved
        .nodes
        .iter()
        .map(|node| node.task.clone())
        .collect::<Vec<_>>();
    assert_eq!(tasks, vec![Some(stamp(1)), Some(stamp(2))]);
    assert_eq!(
        resolved.nodes[0].label,
        format!("{} · task 1", board.tasks[0].statement.body.handle)
    );
    assert_eq!(resolved.nodes[0].detail, "todo · pending");
    assert_eq!(resolved.nodes[0].status, None);
    let completed = &resolved.nodes[1];
    assert_eq!(completed.kind, "Selected task");
    assert!(completed.selected);
    assert_eq!(
        completed.label,
        format!(
            "{} · Completed prerequisite",
            wire::TaskHandle::for_topic(
                &"/remote/fixture"
                    .parse::<wire::Topic>()
                    .expect("fixture topic"),
                &stamp(2)
            )
        )
    );
    assert_eq!(completed.detail, "review · completed");
    assert_eq!(completed.status, Some(wire::WorkflowStatusKind::Completed));
    assert_eq!(resolved.edges.len(), 1);
    assert_eq!((resolved.edges[0].from, resolved.edges[0].to), (1, 0));
    assert!(resolved.source.contains("Completed prerequisite"));
    assert!(!resolved.source.contains("Missing from the admitted board"));

    let mut elsewhere = answer(vec![remote_task(
        1,
        "remote title",
        "review",
        wire::WorkflowStatusKind::Pending,
    )]);
    elsewhere.edges.clear();
    assert_eq!(project(Some(&elsewhere)).nodes[1].kind, "Missing task");
}
#[test]
fn controller_edges_render_from_controller_to_controlled_task() {
    let board = board(vec![task(1, "/alpha/controlled")]);
    let work = WorkProjection::from_board_and_lanes(&board, None);
    let selection = TaskOverviewSelection::task(board.tasks[0].statement.body.born.clone());
    let local = work.overview_graph(&selection);
    let mut enriched = answer(vec![
        remote_task(
            1,
            "Controlled output",
            "draft",
            wire::WorkflowStatusKind::Pending,
        ),
        remote_task(
            2,
            "Controller plan",
            "plan",
            wire::WorkflowStatusKind::Pending,
        ),
    ]);
    enriched.roots = vec![stamp(1)];
    enriched.edges = vec![TaskRelationEdge {
        relation: TaskRelationKind::Controller,
        from: TaskRelationNodeRef::Task { task: stamp(2) },
        to: TaskRelationNodeRef::Task { task: stamp(1) },
    }];

    let presentation = super::project(
        &local,
        &work,
        Some(&enriched),
        dependency_view(true),
        &selection,
        &|at| at.to_string(),
    )
    .expect("controller presentation");

    assert_eq!(presentation.edges.len(), 1);
    assert_eq!(
        (
            presentation.nodes[presentation.edges[0].from].task.as_ref(),
            presentation.nodes[presentation.edges[0].to].task.as_ref(),
            presentation.edges[0].label,
        ),
        (Some(&stamp(2)), Some(&stamp(1)), "controls")
    );
    assert!(presentation.source.contains("-.->|controls|"));
    let hidden = super::project(
        &local,
        &work,
        Some(&enriched),
        LensView {
            show_controllers: false,
            ..dependency_view(true)
        },
        &selection,
        &|at| at.to_string(),
    )
    .expect("controller-hidden presentation");
    assert!(hidden.edges.is_empty());
    assert_eq!(hidden.nodes.len(), 1);
}

/// Settled roots and neighbors disappear with their edges, including a
/// missing-board neighbor that would otherwise become an unexplained orphan.
/// Pending work and non-task evidence remain. Showing settled restores the
/// complete graph, with the admitted board authoritative over stale enrichment.
#[test]
fn settled_tasks_are_hidden_even_when_selected_until_shown() {
    let mut live = task(1, "/alpha/live");
    live.statement.body.dependencies = vec![stamp(2), stamp(3), stamp(5)];
    let mut done_prerequisite = task(2, "/alpha/done");
    done_prerequisite.workflow.body.status = wire::WorkflowStatus::completed(settled_at());
    let mut deleted_prerequisite = task(3, "/alpha/deleted");
    deleted_prerequisite.workflow.body.status = wire::WorkflowStatus::deleted(settled_at());
    let mut done_root = task(4, "/alpha/root");
    done_root.workflow.body.status = wire::WorkflowStatus::completed(settled_at());
    done_root.statement.body.dependencies = vec![stamp(7)];
    let board = board(vec![
        live,
        done_prerequisite,
        deleted_prerequisite,
        done_root,
    ]);
    let work = WorkProjection::from_board_and_lanes(&board, None);
    let selection = TaskOverviewSelection::new(
        [stamp(1), stamp(3), stamp(4), stamp(6)],
        super::super::TaskOverviewContext::Task,
    );
    let local = work.overview_graph(&selection);
    let mut enriched = answer(vec![
        remote_task(
            1,
            "stale completed",
            "review",
            wire::WorkflowStatusKind::Completed,
        ),
        remote_task(
            4,
            "stale pending",
            "todo",
            wire::WorkflowStatusKind::Pending,
        ),
        remote_task(
            6,
            "Retained done root",
            "review",
            wire::WorkflowStatusKind::Completed,
        ),
        remote_task(
            5,
            "Elsewhere done",
            "review",
            wire::WorkflowStatusKind::Completed,
        ),
        TaskRelationNode::Origin {
            origin: "ack:dTVQdQfLw".parse().expect("fixture origin"),
            request: None,
        },
    ]);
    enriched.roots = vec![stamp(1), stamp(3), stamp(4), stamp(6)];
    enriched.edges = vec![TaskRelationEdge {
        relation: TaskRelationKind::Dependency,
        from: TaskRelationNodeRef::Task { task: stamp(1) },
        to: TaskRelationNodeRef::Task { task: stamp(5) },
    }];
    let project = |show_settled: bool| {
        super::project(
            &local,
            &work,
            Some(&enriched),
            dependency_view(show_settled),
            &selection,
            &|at| at.to_string(),
        )
        .expect("overview presentation")
    };
    let tasks = |presentation: &Presentation| {
        presentation
            .nodes
            .iter()
            .map(|node| node.task.clone())
            .collect::<Vec<_>>()
    };

    let pruned = project(false);
    assert_eq!(tasks(&pruned), vec![Some(stamp(1)), None]);
    assert!(pruned.nodes[1].task.is_none(), "non-task evidence remains");
    assert!(pruned.edges.is_empty(), "every edge touched a pruned task");

    let shown = project(true);
    assert_eq!(
        tasks(&shown).into_iter().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            Some(stamp(1)),
            Some(stamp(2)),
            Some(stamp(3)),
            Some(stamp(4)),
            Some(stamp(5)),
            Some(stamp(6)),
            Some(stamp(7)),
            None,
        ])
    );
    let arrows = shown
        .edges
        .iter()
        .map(|edge| {
            (
                shown.nodes[edge.from].task.clone(),
                shown.nodes[edge.to].task.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        arrows,
        BTreeSet::from([
            (Some(stamp(2)), Some(stamp(1))),
            (Some(stamp(3)), Some(stamp(1))),
            (Some(stamp(5)), Some(stamp(1))),
            (Some(stamp(7)), Some(stamp(4))),
        ])
    );
}

/// An address naming one task always draws that task, settled or not, on the
/// board or known only to the typed enrichment. Hiding settled work removes
/// only what surrounds it; a set or scope holding the same task hides it as
/// any settled root.
#[test]
fn an_addressed_task_stays_drawn_while_settled_work_is_hidden() {
    let mut done = task(1, "/alpha/done");
    done.workflow.body.status = wire::WorkflowStatus::completed(settled_at());
    done.statement.body.dependencies = vec![stamp(2), stamp(3)];
    let mut done_prerequisite = task(2, "/alpha/prior");
    done_prerequisite.workflow.body.status = wire::WorkflowStatus::completed(settled_at());
    let board = board(vec![done, done_prerequisite, task(3, "/alpha/open")]);
    let work = WorkProjection::from_board_and_lanes(&board, None);
    let drawn = |selection: &TaskOverviewSelection,
                 answer: Option<&TaskRelationsAnswer>,
                 show_settled: bool| {
        super::project(
            &work.overview_graph(selection),
            &work,
            answer,
            dependency_view(show_settled),
            selection,
            &|at| at.to_string(),
        )
        .expect("overview presentation")
        .nodes
        .into_iter()
        .map(|node| node.task)
        .collect::<BTreeSet<_>>()
    };
    let stamps = |values: &[i64]| {
        values
            .iter()
            .map(|value| Some(stamp(*value)))
            .collect::<BTreeSet<_>>()
    };

    let addressed = TaskOverviewSelection::task(stamp(1));
    assert_eq!(drawn(&addressed, None, false), stamps(&[1, 3]));
    assert_eq!(drawn(&addressed, None, true), stamps(&[1, 2, 3]));

    // Off the board, a typed settled status hides the addressed task no more.
    let history = TaskOverviewSelection::task(stamp(9));
    let mut enriched = answer(vec![remote_task(
        9,
        "Old work",
        "review",
        wire::WorkflowStatusKind::Completed,
    )]);
    enriched.roots = vec![stamp(9)];
    enriched.edges = Vec::new();
    assert_eq!(drawn(&history, Some(&enriched), false), stamps(&[9]));

    let scope = TaskOverviewSelection::new(
        [stamp(1)],
        super::super::TaskOverviewContext::Scope(super::super::WorkScope::All),
    );
    assert_eq!(drawn(&scope, None, false), stamps(&[]));
    let pair = TaskOverviewSelection::new(
        [stamp(1), stamp(2)],
        super::super::TaskOverviewContext::Task,
    );
    assert_eq!(drawn(&pair, None, false), stamps(&[]));
}

/// Hiding settled work can orphan an unselected neighbor that sits before a
/// kept edge. Dropping it renumbers the graph, and the Mermaid source must
/// still declare every node its edges and row links name; an undeclared one
/// is drawn as a bare placeholder box.
#[test]
fn dropping_an_orphaned_neighbor_keeps_every_drawn_link_on_a_declared_node() {
    let mut done_root = task(1, "/alpha/done");
    done_root.workflow.body.status = wire::WorkflowStatus::completed(settled_at());
    done_root.statement.body.dependencies = vec![stamp(2)];
    let mut live_root = task(3, "/alpha/live");
    live_root.statement.body.dependencies = vec![stamp(4)];
    let unrelated_root = task(5, "/alpha/alone");
    let board = board(vec![
        done_root,
        task(2, "/alpha/orphan"),
        live_root,
        task(4, "/alpha/prerequisite"),
        unrelated_root,
    ]);
    let work = WorkProjection::from_board_and_lanes(&board, None);
    let selection = TaskOverviewSelection::new(
        [stamp(1), stamp(3), stamp(5)],
        super::super::TaskOverviewContext::Task,
    );
    let local = work.overview_graph(&selection);

    let pruned = super::project(
        &local,
        &work,
        None,
        dependency_view(false),
        &selection,
        &|at| at.to_string(),
    )
    .expect("overview presentation");

    assert!(
        pruned
            .nodes
            .iter()
            .all(|node| node.task != Some(stamp(2)) && node.task != Some(stamp(1))),
        "the settled root and its orphaned prerequisite are hidden"
    );
    assert_eq!(pruned.edges.len(), 1);
    let declared = pruned
        .source
        .lines()
        .filter_map(|line| line.trim().split_once("[\"").map(|(id, _)| id))
        .collect::<BTreeSet<_>>();
    let linked = pruned
        .source
        .lines()
        .filter(|line| line.contains("-->") || line.contains("~~~"))
        .flat_map(|line| {
            let mut words = line.split_whitespace();
            [words.next(), words.last()]
        })
        .flatten()
        .collect::<BTreeSet<_>>();
    assert!(
        linked.is_subset(&declared),
        "links name undeclared nodes {:?}:\n{}",
        linked.difference(&declared).collect::<Vec<_>>(),
        pruned.source
    );
}

/// A Design's overview draws its Plan outputs downstream of it, a Plan's own
/// overview names its gating Design upstream, and a Plan-to-executable edge
/// remains the only path to nested work.
#[test]
fn design_plan_dependency_edges_do_not_shortcut_nested_work() {
    let mut design = task(1, "/rooms/design");
    design.statement.body.intent = Some(wire::TaskIntent::Design);
    let plan = |value| {
        let mut plan = task(value, "/rooms/plan");
        plan.statement.body.intent = Some(wire::TaskIntent::Plan);
        plan.statement.body.dependencies = vec![stamp(1)];
        plan
    };
    let mut execution = task(4, "/rooms/work");
    execution.statement.body.dependencies = vec![stamp(2)];
    let board = board(vec![design, plan(2), plan(3), execution]);
    let work = WorkProjection::from_board_and_lanes(&board, None);
    let arrows = |selected: Vec<wire::TaskStamp>| {
        let selection =
            TaskOverviewSelection::new(selected, super::super::TaskOverviewContext::Task);
        let presentation = super::project(
            &work.overview_graph(&selection),
            &work,
            None,
            dependency_view(false),
            &selection,
            &|at| at.to_string(),
        )
        .expect("overview presentation");
        presentation
            .edges
            .iter()
            .map(|edge| {
                (
                    presentation.nodes[edge.from].task.clone(),
                    presentation.nodes[edge.to].task.clone(),
                    edge.label,
                )
            })
            .collect::<BTreeSet<_>>()
    };

    assert_eq!(
        arrows(vec![stamp(1), stamp(2), stamp(3)]),
        BTreeSet::from([
            (Some(stamp(1)), Some(stamp(2)), "then"),
            (Some(stamp(1)), Some(stamp(3)), "then"),
        ]),
    );
    assert_eq!(
        arrows(vec![stamp(2)]),
        BTreeSet::from([(Some(stamp(1)), Some(stamp(2)), "then")]),
        "a Plan alone still shows its Design prerequisite upstream",
    );
    assert_eq!(
        arrows(vec![stamp(1), stamp(2), stamp(4)]),
        BTreeSet::from([
            (Some(stamp(1)), Some(stamp(2)), "then"),
            (Some(stamp(2)), Some(stamp(4)), "then"),
        ]),
        "the Design edge does not shortcut the Plan-to-task dependency",
    );
}

fn then(from: usize, to: usize) -> Edge {
    Edge {
        from,
        to,
        label: "then",
        dotted: false,
    }
}

#[test]
fn disconnected_components_pack_into_rows_of_whole_components() {
    assert_eq!(row_links(6, &[]), [(0, 1), (1, 2), (3, 4), (4, 5)]);
    assert!(row_links(3, &[then(0, 1), then(1, 2)]).is_empty());

    // Ten nodes budget four layers: the three-layer chain fills its row, the
    // two-layer fan opens the next from its first last-layer node, and the
    // unconnected tasks fill rows behind it.
    let mixed = [then(0, 1), then(1, 2), then(3, 4), then(3, 5)];
    assert_eq!(row_links(10, &mixed), [(4, 6), (6, 7), (8, 9)]);

    // A chain deeper than the square side widens the budget and keeps a row.
    let deep = [then(0, 1), then(1, 2), then(2, 3), then(3, 4)];
    assert_eq!(row_links(8, &deep), [(5, 6), (6, 7)]);
}

#[test]
fn a_cycle_still_packs_deterministically() {
    let cycle = [then(0, 1), then(1, 2), then(2, 0)];
    assert_eq!(row_links(4, &cycle), [(0, 3)]);
}
