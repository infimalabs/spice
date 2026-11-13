//! A hand-taken claim's completion contract follows its linked work without
//! moving the borrower's generation, so the projection the resident tells the
//! holder from must move with it.

use super::*;

/// The contract the projection holds for `task`, if any. The projected handle
/// follows the row's phase, so the match is by birth stamp.
fn projected(bus: &Bus, task: &TaskHandle) -> Option<String> {
    let snapshot = bus.task_snapshot().expect("task snapshot reads");
    let born = task.born().expect("fixture handle carries a birth stamp");
    let mut held = claim_contracts(&snapshot)
        .into_iter()
        .filter(|held| held.task.born().ok() == Some(born.clone()));
    let found = held.next();
    assert!(held.next().is_none(), "one projection per claim");
    found.map(|held| held.contract)
}

/// The borrower's generation and its claim's acknowledgement.
fn standing(bus: &Bus, epoch: CanonicalUuid, task: &TaskHandle) -> (u64, u64) {
    let view = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&view, task);
    (
        heads.workflow.body.generation,
        heads
            .latest_claim()
            .map(|held| &held.claim)
            .expect("held")
            .body
            .acknowledged,
    )
}

/// Settle `task` negatively as the operator, the way an operator close does.
fn close_lender(bus: &Bus, epoch: CanonicalUuid, task: &TaskHandle) {
    let visible = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&visible, task);
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskClose,
        transition_witnesses(heads),
        &TaskClose {
            task: task.clone(),
            validation: "the fixture withdraws this lender".to_owned(),
            force_claimed: false,
        },
    ));
}

/// Make `task` depend on `after` as the operator, whoever holds `task`.
fn operator_link(bus: &Bus, epoch: CanonicalUuid, task: &TaskHandle, after: &TaskHandle) {
    let visible = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let parent = at(&visible, task);
    let child = at(&visible, after);
    let mut expected = vec![
        statement_witness(parent),
        workflow_witness(parent),
        claim_witness(parent),
        statement_witness(child),
        workflow_witness(child),
        Witness::Edge {
            task: parent.statement.body.born.clone(),
            after: child.statement.body.born.clone(),
        },
    ];
    append_dependency_reachability_witnesses(
        &visible,
        std::slice::from_ref(&child.statement.body.born),
        &mut expected,
    )
    .expect("fixture dependency closure is visible");
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDepends,
        expected,
        &TaskDepends {
            task: task.clone(),
            after: vec![after.clone()],
            remove: false,
        },
    ));
}

fn lender(bus: &Bus, epoch: CanonicalUuid, name: &str) -> TaskHandle {
    add_task_with_term(
        bus,
        epoch,
        name,
        "Accepted lender",
        "/board/test",
        Some("todo"),
        &["the lent behavior is verified"],
    )
}

#[test]
fn a_held_contract_follows_lenders_settling_and_arriving_in_either_direction() {
    let (root, bus, epoch, seats) = fixture("claim-contract-lenders", 1);
    let borrower = add_task_with_term(
        &bus,
        epoch,
        "CONTRACT-borrow",
        "Unaccepted borrower",
        "/board/test",
        Some("todo"),
        &[],
    );
    capture_for(&bus, epoch, &seats[0], &borrower);
    let unspecified =
        projected(&bus, &borrower).expect("an acknowledged hand-taken claim projects");

    // Linking a prerequisite edits the borrower's own statement, so the claim
    // trails its generation and the generation notice tells it; acknowledging
    // returns the claim to this projection, now linked.
    let prerequisite = lender(&bus, epoch, "CONTRACT-prereq");
    operator_link(&bus, epoch, &borrower, &prerequisite);
    assert_eq!(projected(&bus, &borrower), None);
    refresh_held(
        &bus,
        epoch,
        &Actor::Lane(seats[0].lane),
        &seats[0],
        &borrower,
    );
    let held = standing(&bus, epoch, &borrower);
    let linked = projected(&bus, &borrower).expect("acknowledged again");
    assert!(linked.contains(&prerequisite.to_string()), "{linked}");
    assert_ne!(linked, unspecified);

    // The sole prerequisite lender settling returns the borrower to exactly
    // the unspecified contract without moving its generation.
    close_lender(&bus, epoch, &prerequisite);
    assert_eq!(projected(&bus, &borrower).as_ref(), Some(&unspecified));
    assert_eq!(standing(&bus, epoch, &borrower), held);

    // Work arriving that depends on the borrower lends without editing it,
    // and settling that work leaves the borrower unspecified again.
    let dependent = lender(&bus, epoch, "CONTRACT-depend");
    connect_child(&bus, epoch, &dependent, &borrower);
    let restored = projected(&bus, &borrower).expect("still held and acknowledged");
    assert!(restored.contains(&dependent.to_string()), "{restored}");
    assert_eq!(standing(&bus, epoch, &borrower), held);
    close_lender(&bus, epoch, &dependent);
    assert_eq!(projected(&bus, &borrower), Some(unspecified));
    assert_eq!(standing(&bus, epoch, &borrower), held);
    clean(&root, bus);
}
