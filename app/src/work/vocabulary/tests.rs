use super::*;
use crate::work::WorkScope;
use crate::work::tests::{board, claim, design, retained, stamp, task};
use wire::EnvironmentRequirement;
use wire::head::ClaimDisposition;
use wire::rooms::{DesignCoverage, StatementItem};

fn upgrade() -> wire::EnvironmentRequirements {
    wire::EnvironmentRequirements::one(
        EnvironmentRequirement::journal_schema(2).expect("requirement"),
    )
}

fn deferral() -> Option<Instant> {
    Some("2026-09-24T06:00:00.000Z".parse().expect("deferral"))
}

fn settled_at() -> Instant {
    "2026-09-23T06:00:00.000Z".parse().expect("settlement")
}

fn seq(value: u64) -> wire::Seq {
    wire::Seq::new(value).expect("positive")
}

#[test]
fn the_operator_queue_decides_yours_before_any_other_standing() {
    let mut done = task(1, "/vocab/finished");
    done.workflow.body.status = wire::WorkflowStatus::completed(settled_at());
    let mut deleted = task(2, "/vocab/finished");
    deleted.workflow.body.status = wire::WorkflowStatus::deleted(settled_at());
    let mut collapsed = design(3, "/vocab/finished");
    collapsed.workflow.body.status = wire::WorkflowStatus::closed(settled_at());
    collapsed.collapse_reason = Some("premise failed".to_owned());
    let mut draft = task(4, "/vocab/draft");
    draft.workflow.body.phase = wire::Phase::Draft;
    let mut working = task(5, "/vocab/open");
    working.claims = vec![retained(claim(&working, ClaimDisposition::Active))];
    let mut held = task(6, "/vocab/open");
    held.statement.body.requirements = upgrade();
    let human_gate = task(7, "/vocab/open");
    let mut waiting = task(8, "/vocab/open");
    waiting.statement.body.dependencies = vec![stamp(9)];
    let ready = task(9, "/vocab/open");
    let mut stale = task(10, "/vocab/open");
    stale.claims = vec![retained(claim(&stale, ClaimDisposition::Lapsed))];
    let mut deferred = task(11, "/vocab/open");
    deferred.workflow.body.deferral = deferral();
    let mut claimed_deciding = design(12, "/vocab/design");
    claimed_deciding.claims = vec![retained(claim(&claimed_deciding, ClaimDisposition::Active))];
    let mut deferred_deciding = design(13, "/vocab/design");
    deferred_deciding.workflow.body.deferral = deferral();
    let mut review = task(14, "/vocab/open");
    review.workflow.body.phase = "review".parse().expect("phase");
    let mut behind_held = task(15, "/vocab/open");
    behind_held.statement.body.dependencies = vec![stamp(6)];
    let mut claimed_upgrade = task(16, "/vocab/open");
    claimed_upgrade.statement.body.requirements = upgrade();
    claimed_upgrade.claims = vec![retained(claim(&claimed_upgrade, ClaimDisposition::Active))];

    let mut board = board(vec![
        done,
        deleted,
        collapsed,
        draft,
        working,
        held,
        human_gate,
        waiting,
        ready,
        stale,
        deferred,
        claimed_deciding,
        deferred_deciding,
        review,
        behind_held,
        claimed_upgrade,
    ]);
    board.blocked.extend([stamp(7), stamp(15)]);
    board.workflow_waiting.insert(stamp(8));
    board
        .design_cycles
        .extend([stamp(12), stamp(13)].map(|design| (design, DesignCycle::AwaitingDecision)));
    let work = WorkProjection::from_board_and_lanes(&board, None);

    let deferred_until = deferral().expect("deferral");
    let nine = stamp(9);
    let six = stamp(6);
    // Principal holder fixture.
    assert_eq!(
        work.holders(),
        [
            WorkHolder::Settled(wire::Settlement::Done),
            WorkHolder::Settled(wire::Settlement::Deleted),
            WorkHolder::Settled(wire::Settlement::Collapsed),
            WorkHolder::Draft,
            WorkHolder::Working(&Actor::TEST_PRINCIPAL),
            WorkHolder::Yours,
            WorkHolder::Yours,
            WorkHolder::WaitsOn(vec![&nine]),
            WorkHolder::Ready,
            WorkHolder::Ready,
            WorkHolder::Deferred(&deferred_until),
            WorkHolder::Yours,
            WorkHolder::Deferred(&deferred_until),
            WorkHolder::Ready,
            WorkHolder::WaitsOn(vec![&six]),
            WorkHolder::Yours,
        ],
        "a human-only gate, an environment hold, and a Design awaiting decision read yours even \
         when claimed; set-aside work stays deferred; a task behind a listed wall waits on it"
    );
}

#[test]
fn a_design_phase_follows_its_cycle_and_accepted_outputs_not_maturity() {
    let mut gathering = design(1, "/phase/design");
    gathering.design_cycle.steeped = true;
    gathering.design_coverage = Some(DesignCoverage {
        covered: vec![StatementItem::of("Who defines it?")],
        missing: vec![
            StatementItem::of("Who owns it?"),
            StatementItem::of("Who checks it?"),
        ],
    });
    let steeped = design(2, "/phase/design");
    let proposed = design(3, "/phase/design");
    let mut concluding = design(4, "/phase/design");
    concluding.design_cycle.settled = Some(seq(40));
    let deciding_again = design(5, "/phase/design");
    let mut released = task(6, "/phase/output");
    released.statement.body.preparation = Some(wire::TaskPreparation {
        parent: stamp(5),
        controller: stamp(5),
        construction: stamp(6),
    });
    let running = design(7, "/phase/design");
    let mut done = design(8, "/phase/design");
    done.workflow.body.status = wire::WorkflowStatus::completed(settled_at());
    let mut collapsed = design(9, "/phase/design");
    collapsed.workflow.body.status = wire::WorkflowStatus::closed(settled_at());
    let mut withdrawn = design(10, "/phase/design");
    withdrawn.workflow.body.status = wire::WorkflowStatus::deleted(settled_at());
    let mut finished = task(11, "/phase/execution");
    finished.workflow.body.status = wire::WorkflowStatus::completed(settled_at());

    let mut board = board(vec![
        gathering,
        steeped,
        proposed,
        concluding,
        deciding_again,
        released,
        running,
        done,
        collapsed,
        withdrawn,
        finished,
    ]);
    board.design_cycles.extend([
        (stamp(1), DesignCycle::GatheringInput),
        (stamp(2), DesignCycle::ReadyForPass),
        (stamp(3), DesignCycle::AwaitingDecision),
        (stamp(4), DesignCycle::AwaitingDecision),
        (stamp(5), DesignCycle::AwaitingDecision),
        (stamp(7), DesignCycle::WaitingOnSelectedWork),
    ]);
    let work = WorkProjection::from_board_and_lanes(&board, None);
    let design = |phase| Some(WorkPhase::Design(phase));

    assert_eq!(
        work.tasks().iter().map(WorkTask::phase).collect::<Vec<_>>(),
        [
            design(DesignPhase::Input {
                answered: 1,
                questions: 3,
            }),
            design(DesignPhase::Steeped),
            design(DesignPhase::Researched),
            design(DesignPhase::Decided),
            design(DesignPhase::Decided),
            Some(WorkPhase::Execution("todo")),
            design(DesignPhase::Decided),
            design(DesignPhase::Concluded),
            design(DesignPhase::Concluded),
            None,
            Some(WorkPhase::Execution("done")),
        ],
        "a steeped maturity flag never lifts a gathering Design; a pass awaiting decision after accepted work, \
         settled or still on the board, reads decided"
    );
}

#[test]
fn a_track_marks_exactly_its_current_step_and_lineage_reads_apart_from_prerequisites() {
    let mut gathering = design(1, "/track/design");
    gathering.design_coverage = Some(DesignCoverage {
        covered: vec![StatementItem::of("Who defines it?")],
        missing: vec![StatementItem::of("Who owns it?")],
    });
    let releasing = design(2, "/track/design");
    let mut output = task(3, "/track/output");
    output.statement.body.preparation = Some(wire::TaskPreparation {
        parent: stamp(2),
        controller: stamp(2),
        construction: stamp(3),
    });
    let mut planned = task(4, "/track/execution");
    planned.workflow.body.flow = ["plan", "todo", "review"]
        .map(|phase| phase.parse().expect("phase"))
        .to_vec();
    planned.statement.body.dependencies = vec![stamp(3)];
    let mut finished = task(5, "/track/execution");
    finished.workflow.body.status = wire::WorkflowStatus::completed(settled_at());
    let mut withdrawn = task(6, "/track/execution");
    withdrawn.workflow.body.status = wire::WorkflowStatus::deleted(settled_at());
    let mut board = board(vec![
        gathering, releasing, output, planned, finished, withdrawn,
    ]);
    board.design_cycles.extend([
        (stamp(1), DesignCycle::GatheringInput),
        (stamp(2), DesignCycle::WaitingOnSelectedWork),
    ]);
    let work = WorkProjection::from_board_and_lanes(&board, None);
    let track = |index: usize| {
        work.tasks()[index]
            .track()
            .into_iter()
            .map(|step| (step.label, step.current))
            .collect::<Vec<_>>()
    };
    let steps = |labels: &[&str], current: &str| {
        labels
            .iter()
            .map(|label| ((*label).to_owned(), *label == current))
            .collect::<Vec<_>>()
    };
    let design_track = ["input", "steeped", "researched", "decided", "concluded"];

    assert_eq!(
        track(0),
        steps(
            &["input 1/2", "steeped", "researched", "decided", "concluded"],
            "input 1/2"
        ),
        "only the current input step carries its answered count"
    );
    assert_eq!(track(1), steps(&design_track, "decided"));
    assert_eq!(track(3), steps(&["plan", "todo", "review", "done"], "todo"));
    assert_eq!(track(4), steps(&["todo", "done"], "done"));
    assert!(track(5).is_empty(), "withdrawn work stands on no track");

    let edges = |index: usize| {
        let task = &work.tasks()[index];
        (
            task.dependencies()
                .iter()
                .map(|edge| (edge.task(), edge.released()))
                .collect::<Vec<_>>(),
            task.dependents()
                .iter()
                .map(|edge| (edge.task(), edge.released()))
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(
        edges(1),
        (vec![(Some(2), true)], vec![]),
        "the Design released its output"
    );
    assert_eq!(
        edges(2).1,
        [(1, true), (3, false)],
        "the output was released by the Design and unblocks its dependent"
    );
    assert_eq!(
        edges(3).0,
        [(Some(2), false)],
        "a named prerequisite is not lineage"
    );
}

#[test]
fn a_list_groups_in_holder_order_keeps_offer_order_and_never_filters_out_yours() {
    let early_ready = task(1, "/group/order");
    let mut working = task(2, "/group/order");
    working.claims = vec![retained(claim(&working, ClaimDisposition::Active))];
    let mut held = task(3, "/group/order");
    held.statement.body.requirements = upgrade();
    let mut draft = task(4, "/group/order");
    draft.workflow.body.phase = wire::Phase::Draft;
    let mut deferred = task(5, "/group/order");
    deferred.workflow.body.deferral = deferral();
    let mut waiting = task(6, "/group/order");
    waiting.statement.body.dependencies = vec![stamp(1)];
    let mut offered_ready = task(7, "/group/order");
    offered_ready.presentation_title = "Porcelain status output".to_owned();

    let mut board = board(vec![
        early_ready,
        working,
        held,
        draft,
        deferred,
        waiting,
        offered_ready,
    ]);
    board.workflow_waiting.insert(stamp(6));
    board.offer = vec![stamp(7), stamp(1)];
    let work = WorkProjection::from_board_and_lanes(&board, None);
    let holders = work.holders();
    let tasks = work.scope_tasks(&WorkScope::All);
    let born = |indexes: &[usize]| {
        indexes
            .iter()
            .map(|&index| work.tasks()[index].row().statement.body.born.clone())
            .collect::<Vec<_>>()
    };
    let listed = |list: WorkList| {
        let groups = list
            .groups
            .iter()
            .map(|group| (group.group, born(&group.tasks)))
            .collect::<Vec<_>>();
        (groups, born(&list.outside))
    };

    assert_eq!(
        listed(work.list(&tasks, &holders, |_| true)),
        (
            vec![
                (HolderGroup::Yours, vec![stamp(3)]),
                (HolderGroup::Working, vec![stamp(2)]),
                (HolderGroup::Ready, vec![stamp(7), stamp(1)]),
                (HolderGroup::Waiting, vec![stamp(6)]),
                (HolderGroup::Deferred, vec![stamp(5)]),
                (HolderGroup::Draft, vec![stamp(4)]),
            ],
            vec![],
        ),
        "the resident's offer order leads inside a group"
    );
    assert_eq!(
        listed(work.list(&tasks, &holders, |task| {
            task.matches_filter("  PORCELAIN ", None)
        })),
        (vec![(HolderGroup::Ready, vec![stamp(7)])], vec![stamp(3)]),
        "a title filter ignores case and keeps the task waiting on the operator apart"
    );
    let handle = work.tasks()[4].row().statement.body.handle.to_string();
    assert_eq!(
        listed(work.list(&tasks, &holders, |task| {
            task.matches_filter(&handle, None)
                || task.matches_filter("ava", task.working_claimant().map(|_| "Ava"))
        })),
        (
            vec![
                (HolderGroup::Working, vec![stamp(2)]),
                (HolderGroup::Deferred, vec![stamp(5)]),
            ],
            vec![stamp(3)],
        ),
        "an exact handle or a holder lane's label matches"
    );
    assert_eq!(
        listed(work.list(&tasks, &holders, |task| task.matches_filter("zzz", None))),
        (vec![], vec![stamp(3)]),
        "nothing else matching leaves only the operator's work, apart"
    );
}

#[test]
fn all_work_is_exactly_planning_and_execution_together() {
    let open_design = design(1, "/union/design");
    let mut gated = task(2, "/union/execution");
    gated.statement.body.dependencies = vec![stamp(1)];
    let execution = task(3, "/union/execution");
    let mut claimed = task(4, "/union/execution");
    claimed.claims = vec![retained(claim(&claimed, ClaimDisposition::Active))];
    let board = board(vec![open_design, gated, execution, claimed]);
    let work = WorkProjection::from_board_and_lanes(&board, None);
    let tasks = |scope: &WorkScope| {
        work.scope_tasks(scope)
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
    };

    let planning = tasks(&WorkScope::planning());
    let execution = tasks(&WorkScope::AllOpen);
    assert!(planning.is_disjoint(&execution));
    assert_eq!(tasks(&WorkScope::All), &planning | &execution);
    assert_eq!(
        work.scope_counts(&WorkScope::All).open(),
        work.scope_counts(&WorkScope::planning()).open()
            + work.scope_counts(&WorkScope::AllOpen).open(),
        "no task counts in both planning and execution"
    );
}

#[test]
fn retained_tracks_use_admitted_lifecycle_without_a_board_row() {
    let current = |row: &BoardTask| {
        retained_track(row)
            .into_iter()
            .filter(|step| step.current)
            .map(|step| step.label)
            .collect::<Vec<_>>()
    };
    let mut execution = task(21, "/history/execution");
    execution.workflow.body.flow = ["plan", "todo", "review"]
        .map(|phase| phase.parse().expect("phase"))
        .to_vec();
    execution.workflow.body.phase = "review".parse().expect("phase");
    assert_eq!(current(&execution), ["review"]);
    execution.workflow.body.status = wire::WorkflowStatus::completed(settled_at());
    assert_eq!(current(&execution), ["done"]);
    execution.workflow.body.status = wire::WorkflowStatus::closed(settled_at());
    assert_eq!(
        retained_track(&execution)
            .into_iter()
            .map(|step| (step.label, step.current))
            .collect::<Vec<_>>(),
        [
            ("plan", false),
            ("todo", false),
            ("review", false),
            ("closed", true)
        ]
        .map(|(label, current)| (label.to_owned(), current)),
        "an operator-closed task ends its track closed, never done"
    );

    let mut concluded = design(22, "/history/design");
    concluded.workflow.body.status = wire::WorkflowStatus::completed(settled_at());
    assert_eq!(current(&concluded), ["concluded"]);
    concluded.workflow.body.status = wire::WorkflowStatus::deleted(settled_at());
    assert!(retained_track(&concluded).is_empty());
}

#[test]
fn retained_prerequisite_standing_distinguishes_terminal_pending_and_unknown() {
    let mut finished = task(21, "/history/execution");
    finished.workflow.body.status = wire::WorkflowStatus::completed(settled_at());
    let pending = task(22, "/history/execution");
    let board = board(vec![finished, pending]);
    let work = WorkProjection::from_board_and_lanes(&board, None);
    assert_eq!(work.prerequisite_satisfied(&stamp(21)), Some(true));
    assert_eq!(work.prerequisite_satisfied(&stamp(22)), Some(false));
    assert_eq!(work.prerequisite_satisfied(&stamp(23)), None);
}
