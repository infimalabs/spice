use super::*;
use crate::work::tests::{board, claim, design, retained, stamp, task};
use wire::EnvironmentRequirement;
use wire::head::ClaimDisposition;

#[test]
fn only_chains_agents_cannot_move_are_walls() {
    let mut active = task(1, "/wall/chain");
    active.claims = vec![retained(claim(&active, ClaimDisposition::Active))];
    let ready = task(2, "/wall/chain");
    let mut waiting = task(3, "/wall/chain");
    waiting.statement.body.dependencies = vec![stamp(1), stamp(2)];
    let mut transitive = task(4, "/wall/chain");
    transitive.statement.body.dependencies = vec![stamp(3)];
    let mut deferred = task(5, "/wall/chain");
    deferred.workflow.body.deferral = Some("2026-09-24T06:00:00.000Z".parse().expect("deferral"));
    let mut upgrade = task(6, "/wall/chain");
    upgrade.statement.body.requirements = wire::EnvironmentRequirements::one(
        EnvironmentRequirement::journal_schema(2).expect("requirement"),
    );
    let mut settled = task(7, "/wall/chain");
    settled.workflow.body.status = wire::WorkflowStatus::closed(
        "2026-09-23T06:00:00.000Z"
            .parse()
            .expect("fixture completion"),
    );
    let mut behind_deferral = task(8, "/wall/chain");
    behind_deferral.statement.body.dependencies = vec![stamp(5)];
    let mut behind_upgrade = task(9, "/wall/chain");
    behind_upgrade.statement.body.dependencies = vec![stamp(4), stamp(6)];
    let mut cycle_a = task(10, "/wall/chain");
    cycle_a.statement.body.dependencies = vec![stamp(11)];
    let mut cycle_b = task(11, "/wall/chain");
    cycle_b.statement.body.dependencies = vec![stamp(10)];
    let mut closed_or_settled = task(12, "/wall/chain");
    closed_or_settled.statement.body.dependencies = vec![stamp(7), stamp(99)];
    let mut stale = task(13, "/wall/stale");
    stale.claims = vec![retained(claim(&stale, ClaimDisposition::Lapsed))];
    let mut behind_stale = task(14, "/wall/stale");
    behind_stale.statement.body.dependencies = vec![stamp(13)];
    let mut stale_deferred = task(15, "/wall/stale");
    stale_deferred.claims = vec![retained(claim(&stale_deferred, ClaimDisposition::Lapsed))];
    stale_deferred.workflow.body.deferral =
        Some("2026-09-24T06:00:00.000Z".parse().expect("deferral"));
    let mut behind_stale_deferred = task(16, "/wall/stale");
    behind_stale_deferred.statement.body.dependencies = vec![stamp(15)];

    let mut board = board(vec![
        active,
        ready,
        waiting,
        transitive,
        deferred,
        upgrade,
        settled,
        behind_deferral,
        behind_upgrade,
        cycle_a,
        cycle_b,
        closed_or_settled,
        stale,
        behind_stale,
        stale_deferred,
        behind_stale_deferred,
    ]);
    // The resident's gate holds back every row waiting on an unsettled prerequisite.
    board.blocked.extend([3, 4, 8, 9, 10, 11, 12].map(stamp));
    board.workflow_waiting.insert(stamp(14));
    board.blocked.insert(stamp(16));
    let work = WorkProjection::from_board_and_lanes(&board, None);
    let mut walls = DependencyWalls::new(&work);
    let mut walls_of = |index: usize| {
        work.tasks()[index]
            .dependencies()
            .iter()
            .map(|dependency| walls.wall(index, dependency).map(|(_, wall)| wall))
            .collect::<Vec<_>>()
    };

    assert_eq!(
        walls_of(2),
        [None, None],
        "claimed and assignable work moves"
    );
    assert_eq!(walls_of(3), [None], "a chain of waits on moving work moves");
    assert_eq!(walls_of(7), [Some(DependencyWall::Deferred(&stamp(5)))]);
    assert_eq!(
        walls_of(8),
        [None, Some(DependencyWall::Environment(&stamp(6)))],
    );
    assert_eq!(walls_of(9), [Some(DependencyWall::Cycle(&stamp(10)))]);
    assert_eq!(
        walls_of(11),
        [None, None],
        "a settled prerequisite is no wall whether retained in context or omitted"
    );
    assert_eq!(
        walls_of(13),
        [None],
        "a claimable stale prerequisite is still moving work"
    );
    let mut walls = DependencyWalls::new(&work);
    assert_eq!(
        walls.wall(15, &work.tasks()[15].dependencies()[0]),
        Some((14, DependencyWall::Deferred(&stamp(15)))),
        "a stale prerequisite exposes its human-actionable wall"
    );
}

#[test]
fn design_standings_expose_the_true_wall_beneath_them() {
    let gathering = design(1, "/wall/design");
    let mut output = task(2, "/wall/design");
    output.statement.body.dependencies = vec![stamp(1)];
    let mut deferred = task(3, "/wall/design");
    deferred.workflow.body.deferral = Some("2026-09-24T06:00:00.000Z".parse().expect("deferral"));
    let mut blocked_design = design(4, "/wall/design");
    blocked_design.statement.body.dependencies = vec![stamp(3)];
    let mut behind_blocked = task(5, "/wall/design");
    behind_blocked.statement.body.dependencies = vec![stamp(4)];
    let mut selecting_walled_design = design(6, "/wall/design");
    selecting_walled_design.statement.body.dependencies = vec![stamp(3)];
    let mut behind_selection = task(7, "/wall/design");
    behind_selection.statement.body.dependencies = vec![stamp(6)];
    let mut board = board(vec![
        gathering,
        output,
        deferred,
        blocked_design,
        behind_blocked,
        selecting_walled_design,
        behind_selection,
    ]);
    board.workflow_waiting.insert(stamp(2));
    board.blocked.extend([stamp(4), stamp(5), stamp(7)]);
    board
        .design_cycles
        .insert(stamp(6), wire::rooms::DesignCycle::AwaitingDecision);
    let work = WorkProjection::from_board_and_lanes(&board, None);
    assert_eq!(work.tasks()[0].availability(), WorkAvailability::Ready);
    assert_eq!(work.tasks()[3].availability(), WorkAvailability::Blocked);
    assert_eq!(work.tasks()[5].availability(), WorkAvailability::Selection);
    let mut walls = DependencyWalls::new(&work);

    assert_eq!(
        walls.wall(1, &work.tasks()[1].dependencies()[0]),
        None,
        "an open Design gathering input needs no human act from its output"
    );
    for dependent in [4, 6] {
        assert_eq!(
            walls.wall(dependent, &work.tasks()[dependent].dependencies()[0]),
            Some((2, DependencyWall::Deferred(&stamp(3)))),
            "the wall beneath a Design is named, not the Design standing"
        );
    }
}
