//! A Design without its own acceptance leaves draft, and later ends a pass,
//! only on acceptance read across a dependency edge in either direction; settled
//! work never lends its acceptance.

use super::*;

/// A Design that states no acceptance of its own: it rests submitted in
/// draft until linked work lends it one.
fn add_unaccepted_design(bus: &Bus, epoch: CanonicalUuid, name: &str) -> TaskHandle {
    let born = stamp(name);
    let design = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &born,
    );
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born }],
        &TaskAdd {
            intent: Some(wire::TaskIntent::Design),
            topic: Some("/board/test".parse().expect("fixture topic")),
            origin: Some(fixture_origin()),
            flow: Some("design".to_owned()),
            ..adding(name.to_owned(), "This work keeps the fixture honest.")
        },
    ));
    wake_design(bus, epoch, &design);
    design
}

/// Complete `task`'s held phase from `seat`: a draft's holder advances it,
/// an activated single phase settles.
fn done(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, task: &TaskHandle) -> Outcome<CommandAnswer> {
    let held = board(bus, epoch, Actor::Lane(seat.lane));
    issue(
        bus,
        epoch,
        Actor::Lane(seat.lane),
        CommandKind::TaskDone,
        progress_witnesses(&held, at(&held, task), seat.lane, seat.occupant, false)
            .expect("held work has progress witnesses"),
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(task, "the accepted behavior is verified")
        },
    )
}

fn finish(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, task: &TaskHandle) {
    assert_committed(done(bus, epoch, seat, task));
}

/// The draft advance refused for want of acceptance leaves the Design in
/// draft under its holder's live claim.
fn assert_stays_in_draft(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, design: &TaskHandle) {
    let refused = done(bus, epoch, seat, design);
    assert!(
        matches!(&refused, Outcome::Refusal(refusal) if refusal.reason() == "task::term-missing"),
        "{refused:?}"
    );
    let view = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&view, design);
    assert!(heads.workflow.body.is_draft());
    assert!(heads.design_cycle.pass.is_none());
    assert!(
        heads
            .latest_claim()
            .map(|held| &held.claim)
            .is_some_and(|claim| {
                claim.body.disposition == ClaimDisposition::Active
                    && claim.body.actor == Actor::Lane(seat.lane)
            })
    );
}

/// End `design`'s pass from `seat`, witnessing the linked work that lends
/// its acceptance only when `borrowed`.
fn end_linked_pass(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    design: &TaskHandle,
    borrowed: bool,
) -> Outcome<CommandAnswer> {
    let lane = Actor::Lane(seat.lane);
    let held = board(bus, epoch, lane.clone());
    let born = design.born().expect("design stamp");
    let mut expected =
        progress_witnesses(&held, at(&held, design), seat.lane, seat.occupant, false)
            .expect("held work has progress witnesses");
    if !borrowed {
        expected.retain(|witness| {
            !matches!(
                witness,
                Witness::Task { task, .. } | Witness::Workflow { task, .. } if *task != born
            )
        });
    }
    issue(
        bus,
        epoch,
        lane,
        CommandKind::TaskDone,
        expected,
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(design, "the linked work settles this Design")
        },
    )
}

#[test]
fn a_design_pass_ends_on_acceptance_from_pending_work_linked_either_way() {
    let (root, bus, epoch, seats) = fixture("design-linked-acceptance", 2);
    let forward = add_unaccepted_design(&bus, epoch, "Forward Design");
    let reverse = add_unaccepted_design(&bus, epoch, "Reverse Design");

    // The holder can acquire the Design before an operator adds its prerequisite.
    let prerequisite = add_task_with_term(
        &bus,
        epoch,
        "LINKED-prereq",
        "Accepted prerequisite",
        "/board/test",
        Some("todo"),
        &["the prerequisite behavior is verified"],
    );
    capture_for(&bus, epoch, &seats[0], &forward);
    connect_child(&bus, epoch, &forward, &prerequisite);
    finish(&bus, epoch, &seats[0], &forward);

    // Pending work depending on the Design lends acceptance too.
    let dependent = add_task_with_term(
        &bus,
        epoch,
        "LINKED-depend",
        "Accepted dependent",
        "/board/test",
        Some("todo"),
        &["the dependent behavior is verified"],
    );
    connect_child(&bus, epoch, &dependent, &reverse);
    capture_for(&bus, epoch, &seats[1], &reverse);
    finish(&bus, epoch, &seats[1], &reverse);
    let unwitnessed = end_linked_pass(&bus, epoch, &seats[0], &forward, false);
    assert!(
        matches!(&unwitnessed, Outcome::Refusal(refusal) if refusal.reason() == "task::term-witness-missing"),
        "{unwitnessed:?}"
    );
    assert_committed(end_linked_pass(&bus, epoch, &seats[0], &forward, true));
    assert_committed(end_linked_pass(&bus, epoch, &seats[1], &reverse, true));
    let ended = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    for design in [&forward, &reverse] {
        assert!(at(&ended, design).design_cycle.pass.is_some());
    }
    clean(&root, bus);
}

#[test]
fn settled_work_cannot_supply_a_designs_acceptance_in_either_direction() {
    for reverse in [false, true] {
        let (root, bus, epoch, seats) = fixture("settled-lender", 2);
        let lender = add_task_with_term(
            &bus,
            epoch,
            "LENDER-settled",
            "Accepted work",
            "/board/test",
            Some("todo"),
            &["the observable result meets its contract"],
        );
        claim_task(&bus, epoch, &seats[1], &Actor::Lane(seats[1].lane), &lender);
        let design = add_unaccepted_design(&bus, epoch, "Unspecified Design");
        if reverse {
            connect_child(&bus, epoch, &lender, &design);
        }
        finish(&bus, epoch, &seats[1], &lender);
        if !reverse {
            connect_child(&bus, epoch, &design, &lender);
        }
        capture_for(&bus, epoch, &seats[0], &design);
        assert_stays_in_draft(&bus, epoch, &seats[0], &design);
        clean(&root, bus);
    }
}

#[test]
fn a_design_without_any_acceptance_never_leaves_draft() {
    let (root, bus, epoch, seats) = fixture("design-missing-acceptance", 1);
    let design = add_unaccepted_design(&bus, epoch, "Unaccepted Design");
    capture_for(&bus, epoch, &seats[0], &design);
    assert_stays_in_draft(&bus, epoch, &seats[0], &design);
    clean(&root, bus);
}
