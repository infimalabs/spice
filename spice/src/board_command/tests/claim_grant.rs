use super::*;
use wire::Fault;

// `ClaimGrant` documents its resolution order as a contract: each step can
// refuse, the caller sees the first refusal only, and a seat that cannot be read
// is reported before a boundary that cannot be captured. Only `claim_source_
// boundary` needs the lane, so swapping `claim_git_boundary` ahead of
// `claim_residency` compiles and changes which refusal a caller receives while
// every other test stays green. These two tests pin both sides of that order.
//
// The fixture refuses at both steps for unrelated reasons rather than one shared
// one: the witness names the seat beside the actor's own, which only the seat
// read compares, and that seat's worktree is `/fixture`, which only the boundary
// resolves. So the order under test is a choice between two live refusals.

/// The reason carried by a refusal or a fault, whichever the step raised.
///
/// The two steps here raise different `Outcome` variants -- a lane mismatch is a
/// refusal the caller could have avoided, an unresolvable worktree is an
/// environment fault -- so matching on one variant would decide the answer
/// before reading it.
fn grant_reason<T>(result: Result<T, Outcome<CommandAnswer>>) -> String {
    match result {
        Err(Outcome::Refusal(refusal)) => refusal.reason().to_owned(),
        Err(Outcome::Fault(Fault { reason, .. })) => reason,
        Err(Outcome::Reply(_)) => panic!("a refused claim step answered with a reply"),
        Ok(_) => panic!("expected the claim step to refuse, found it resolved"),
    }
}

/// A board, a repository, and a claim naming `seat` as its residency witness.
///
/// The repository is the resident's own, so `task_repository` gets past its
/// witness and lane checks and reaches the seated worktree before refusing.
fn grant_fixture(
    name: &str,
    seat: usize,
) -> (Bus, Repository, ValidatedSnapshot, wire::AdmittedCommand) {
    let (root, bus, epoch, seats) = fixture(name, 2);
    let actor = Actor::Lane(seats[0].lane);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    add(&bus, epoch, Actor::TEST_PRINCIPAL, "GUARD-1", "first", None);

    fs::create_dir_all(root.join(".git")).expect("claim grant fixture is a repository");
    let resident = Repository::discover(&root).expect("claim grant fixture repository resolves");

    let snapshot = validated_snapshot(&bus, Some(&resident), actor.clone())
        .expect("claim grant fixture snapshot validates");

    let winner = guard_winner(&snapshot.answer, &actor);
    let expected = claim_expected(&snapshot.answer, &winner, &seats[seat]);
    let command = fixture_command(epoch, actor, CommandKind::ClaimWin, expected, &next_rule());
    (bus, resident, snapshot, command)
}

/// The seat read is reported ahead of the boundary capture, both being live.
///
/// Severing the order -- moving `claim_git_boundary` ahead of `claim_residency`
/// in `claim_grant` -- reds this test alone, because the same inputs then reach
/// the boundary first and the caller receives `git::worktree-unresolved`.
#[test]
fn a_seat_that_cannot_be_read_is_reported_before_a_boundary_that_cannot_be_captured() {
    let (bus, resident, snapshot, command) = grant_fixture("claim-grant-order", 1);

    assert_eq!(
        grant_reason(claim_git_boundary(
            Some(host(&resident)),
            &snapshot,
            &command,
            &snapshot.entries[0],
            Possession::Next,
        )),
        "git::worktree-unresolved",
        "the boundary step must refuse on these inputs, or the order is a choice of one",
    );

    assert_eq!(
        grant_reason(claim_grant(
            &bus,
            &command,
            Some(host(&resident)),
            &snapshot,
            &snapshot.entries[0],
            Possession::Next,
        )),
        "claim::lane-mismatch",
        "the seat read is reported before the boundary that cannot be captured",
    );
}

/// The boundary refusal is reachable through `claim_grant`, not merely shadowed.
///
/// Without this, the test above would still pass if the boundary step were
/// unreachable from `claim_grant` for any reason: a refusal that never fires
/// cannot lose a race. Here the witness names the actor's own seat, so the seat
/// read resolves and the boundary refusal is the one that arrives.
#[test]
fn a_readable_seat_lets_the_boundary_refusal_reach_the_caller() {
    let (bus, resident, snapshot, command) = grant_fixture("claim-grant-boundary", 0);

    assert_eq!(
        grant_reason(claim_grant(
            &bus,
            &command,
            Some(host(&resident)),
            &snapshot,
            &snapshot.entries[0],
            Possession::Next,
        )),
        "git::worktree-unresolved",
        "a seat that reads clean leaves the boundary refusal as the first one",
    );
}

#[test]
fn intermediate_dependency_movement_refuses_before_closing_a_cycle() {
    let (root, bus, epoch, _) = fixture("dependency-intermediate-race", 1);
    let target = todo_task(&bus, epoch, "RACE-target01", "Target");
    let direct = todo_task(&bus, epoch, "RACE-direct01", "Direct");
    let intermediate = todo_task(&bus, epoch, "RACE-middle01", "Intermediate");
    connect_child(&bus, epoch, &direct, &intermediate);
    let stale = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let witnesses = dependency_witnesses(&stale, &target, &[&direct], None, false);
    let intent = TaskDepends {
        task: target.clone(),
        after: vec![direct],
        remove: false,
    };
    let command = fixture_command(
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDepends,
        witnesses,
        &intent,
    );
    let outcome = task_depends_with(&bus, &command, &intent, None, || {
        connect_child(&bus, epoch, &intermediate, &target)
    });
    assert!(
        matches!(outcome, Outcome::Refusal(ref refusal) if refusal.reason() == "board::witness-moved"),
        "intermediate graph movement must lose the commit race: {outcome:?}",
    );
    assert!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &target)
            .statement
            .body
            .dependencies
            .is_empty()
    );
    clean(&root, bus);
}

#[test]
fn a_lane_without_a_claim_can_edit_dependencies() {
    let (root, bus, epoch, seats) = fixture("dependency-absent-lane", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = todo_task(&bus, epoch, "AUTH-fresh001", "Target");
    let after = todo_task(&bus, epoch, "AUTH-fresh002", "After");
    let fresh = board(&bus, epoch, lane.clone());
    assert_committed(issue(
        &bus,
        epoch,
        lane,
        CommandKind::TaskDepends,
        dependency_witnesses(&fresh, &task, &[&after], None, false),
        &TaskDepends {
            task: task.clone(),
            after: vec![after.clone()],
            remove: false,
        },
    ));
    let edited = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(
        at(&edited, &task).statement.body.dependencies,
        [at(&edited, &after).statement.body.born.clone()],
    );
    clean(&root, bus);
}
