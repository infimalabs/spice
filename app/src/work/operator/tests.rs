use super::*;
use crate::work::tests::{board, claim, retained, stamp, task};
use wire::EnvironmentRequirement;
use wire::head::ClaimDisposition;

fn upgrade() -> wire::EnvironmentRequirements {
    wire::EnvironmentRequirements::one(
        EnvironmentRequirement::journal_schema(2).expect("requirement"),
    )
}

fn waiting(waits: &OperatorWaits<'_>) -> Vec<usize> {
    waits.waits().iter().map(OperatorWait::task).collect()
}

#[test]
fn only_execution_walls_enter_operator_waits() {
    let mut claimed_design = task(1, "/topic/design");
    claimed_design.claims = vec![retained(claim(&claimed_design, ClaimDisposition::Active))];
    let mut deferred_design = task(2, "/topic/design");
    deferred_design.workflow.body.deferral =
        Some("2026-09-24T06:00:00.000Z".parse().expect("deferral"));
    let mut held = task(3, "/topic/held");
    held.statement.body.requirements = upgrade();
    let mut first_dependent = task(4, "/topic/held");
    first_dependent.statement.body.dependencies = vec![stamp(3)];
    let mut second_dependent = task(5, "/topic/held");
    second_dependent.statement.body.dependencies = vec![stamp(4), stamp(3)];
    let mut deferred_upgrade = task(6, "/topic/aside");
    deferred_upgrade.statement.body.requirements = upgrade();
    deferred_upgrade.workflow.body.deferral =
        Some("2026-09-24T06:00:00.000Z".parse().expect("deferral"));
    let mut behind_deferral = task(7, "/topic/aside");
    behind_deferral.statement.body.dependencies = vec![stamp(6)];
    let ready = task(8, "/topic/ready");
    let blocked_gate = task(9, "/topic/blocked");
    let mut stale_deferred = task(10, "/topic/stale");
    stale_deferred.claims = vec![retained(claim(&stale_deferred, ClaimDisposition::Lapsed))];
    stale_deferred.workflow.body.deferral =
        Some("2026-09-24T06:00:00.000Z".parse().expect("deferral"));
    let mut behind_stale_deferred = task(11, "/topic/stale");
    behind_stale_deferred.statement.body.dependencies = vec![stamp(10)];

    let mut board = board(vec![
        claimed_design,
        deferred_design,
        held,
        first_dependent,
        second_dependent,
        deferred_upgrade,
        behind_deferral,
        ready,
        blocked_gate,
        stale_deferred,
        behind_stale_deferred,
    ]);
    board.design_cycles.extend(
        [stamp(1), stamp(2)].map(|task| (task, wire::rooms::DesignCycle::AwaitingDecision)),
    );
    board
        .blocked
        .extend([stamp(4), stamp(5), stamp(7), stamp(9), stamp(11)]);
    let work = WorkProjection::from_board_and_lanes(&board, None);
    let operator_waits = OperatorWaits::new(&work);

    assert_eq!(waiting(&operator_waits), [2, 5, 8, 9]);
    let waits = operator_waits.waits();
    assert!(waits[0].environment());
    assert_eq!(
        waits[0].holds(),
        [3, 4],
        "a held task reached directly and through another wall is one wait"
    );
    assert!(
        !waits[1].environment(),
        "set-aside work waits on nothing for its own sake"
    );
    assert_eq!(waits[1].wall(), Some(DependencyWall::Deferred(&stamp(6))));
    assert_eq!(waits[1].holds(), [6]);
    assert_eq!(waits[2].wall(), Some(DependencyWall::Gate(&stamp(9))),);
    assert!(
        waits[2].holds().is_empty(),
        "a resident-classified blocked gate appears even without a projected dependency wall"
    );
    assert_eq!(
        waits[3].wall(),
        Some(DependencyWall::Deferred(&stamp(10))),
        "a lapsed prerequisite exposes its underlying wall"
    );
    assert_eq!(waits[3].holds(), [10]);
    assert!(
        !waiting(&operator_waits).contains(&0) && !waiting(&operator_waits).contains(&1),
        "selection stays visible through Work without becoming an operator wait"
    );
}

#[test]
fn a_claimed_task_the_operator_must_unblock_holds_its_dependents_in_one_wait() {
    let mut iced = task(1, "/topic/iced");
    iced.statement.body.requirements = upgrade();
    iced.claims = vec![retained(claim(&iced, ClaimDisposition::Active))];
    let mut dependent = task(2, "/topic/iced");
    dependent.statement.body.dependencies = vec![stamp(1)];
    let mut transitive = task(3, "/topic/iced");
    transitive.statement.body.dependencies = vec![stamp(2)];
    let mut board = board(vec![iced, dependent, transitive]);
    board.blocked.extend([stamp(2), stamp(3)]);
    let work = WorkProjection::from_board_and_lanes(&board, None);
    let operator_waits = OperatorWaits::new(&work);

    assert_eq!(waiting(&operator_waits), [0]);
    assert!(operator_waits.waits()[0].environment());
    assert_eq!(operator_waits.waits()[0].holds(), [1, 2]);
}
