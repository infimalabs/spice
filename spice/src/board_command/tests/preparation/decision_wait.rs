use super::super::room::design_standing::{active_holder, win_named};
use super::*;

fn offered(bus: &Bus, epoch: CanonicalUuid, seat: &Seat) -> Option<TaskStamp> {
    match next_board(bus, epoch, Actor::Lane(seat.lane)).next {
        Some(NextWorkState::Candidate { task }) => Some(task.statement.body.born),
        _ => None,
    }
}

/// A lane may claim a Design still gathering input by name, by capture or by
/// steal, and end its pass with `task done`, while allocation waits for it to
/// be ready. A named claim on a Design awaiting decision or waiting on
/// released work, steal included, learns the cycle that holds it and the
/// action that moves it.
#[test]
fn a_named_claim_forces_a_gathering_design_but_not_a_held_one() {
    let (root, bus, epoch, seats) = fixture("preparation-design-held-capture", 3);
    let design = add_controller(
        &bus,
        epoch,
        "Steeping design",
        wire::TaskIntent::Design,
        None,
    );
    wake_design(&bus, epoch, &design);
    let held = |rule: &str, cycle: wire::rooms::DesignCycle| {
        let outcome = win_named(&bus, epoch, &seats[1], &design, rule);
        assert_eq!(
            refusal_reason(&outcome),
            "claim::design-held",
            "{rule}: {outcome:?}"
        );
        let Outcome::Refusal(refusal) = &outcome else {
            unreachable!()
        };
        let detail = refusal.explanation().to_owned();
        assert!(
            detail.contains(&design.to_string()) && detail.contains(cycle.label()),
            "{rule} names the Design and its cycle: {detail}"
        );
        detail
    };

    assert_eq!(offered(&bus, epoch, &seats[0]), None);
    let unheld = add_controller(
        &bus,
        epoch,
        "Unheld steeping design",
        wire::TaskIntent::Design,
        None,
    );
    wake_design(&bus, epoch, &unheld);
    let stolen = |outcome| match assert_committed(outcome) {
        CommandAnswer::Claim(answer) => answer.nothing_stolen,
        other => panic!("steal returned another answer shape: {other:?}"),
    };
    assert!(
        stolen(win_named(&bus, epoch, &seats[2], &unheld, "steal")),
        "steal claims a gathering Design nobody holds"
    );
    assert_committed(win_named(&bus, epoch, &seats[0], &design, "capture"));
    assert!(
        !stolen(win_named(&bus, epoch, &seats[1], &design, "steal")),
        "steal displaces a gathering Design's holder"
    );
    assert_eq!(
        active_holder(&bus, epoch, &design),
        Some(Actor::Lane(seats[1].lane))
    );

    // The holder's `task done` ends the pass and releases the claim; the
    // undecided pass then holds the Design for the operator.
    let output = add_plan_output(&bus, epoch, &design, "Selected output");
    assert_committed(end_pass(&bus, epoch, &seats[1], &design));
    assert_eq!(active_holder(&bus, epoch, &design), None);
    let awaiting = held("capture", wire::rooms::DesignCycle::AwaitingDecision);
    for rule in ["recover", "steal"] {
        assert_eq!(
            held(rule, wire::rooms::DesignCycle::AwaitingDecision),
            awaiting
        );
    }
    assert!(awaiting.contains("spice task decide"), "{awaiting}");

    // Selecting the output leaves the Design waiting through retained
    // preparation state, so no named claim opens another pass until the work
    // settles.
    assert_committed(select_outputs(&bus, epoch, &design, &[&output], 1));
    claim_task(&bus, epoch, &seats[0], &Actor::Lane(seats[0].lane), &output);
    let waiting = held("capture", wire::rooms::DesignCycle::WaitingOnSelectedWork);
    assert!(waiting.contains("finish the work decision"), "{waiting}");
    for rule in ["recover", "steal"] {
        held(rule, wire::rooms::DesignCycle::WaitingOnSelectedWork);
    }

    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&current, &output);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskClose,
        transition_witnesses(heads),
        &TaskClose {
            task: output,
            validation: "the released output settled".to_owned(),
            force_claimed: true,
        },
    ));
    assert_eq!(
        offered(&bus, epoch, &seats[1]),
        Some(design.born().expect("Design stamp")),
        "the last released output's settlement readies the Design for the input it gathered"
    );
    clean(&root, bus);
}

#[test]
fn recorded_rejections_close_outputs_while_the_design_waits_on_released_work() {
    let (root, bus, epoch, seats) = fixture("preparation-rejected-selection", 2);
    let design = add_controller(
        &bus,
        epoch,
        "Decision design",
        wire::TaskIntent::Design,
        None,
    );
    capture_design(&bus, epoch, &seats[0], &design);
    let accepted = add_plan_output(&bus, epoch, &design, "Accepted output");
    let rejected = add_plan_output(&bus, epoch, &design, "Rejected output");
    let undecided = add_plan_output(&bus, epoch, &design, "Undecided output");

    assert_committed(end_pass(&bus, epoch, &seats[0], &design));
    assert_committed(select_outputs(&bus, epoch, &design, &[&accepted], 1));
    let after_mixed = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(
        !at(&after_mixed, &rejected)
            .workflow
            .body
            .status
            .is_pending(),
        "a veto closes the output in the decision write"
    );
    assert!(at(&after_mixed, &accepted).workflow.body.is_activated());
    assert!(
        !at(&after_mixed, &undecided)
            .workflow
            .body
            .status
            .is_pending(),
        "every output the decision did not accept is closed"
    );
    assert!(open_awaiting_selection(&bus, epoch).is_empty());

    assert!(
        open_workflow_waiting(&bus, epoch).contains(&design.born().expect("Design stamp")),
        "retained preparation keeps the Design waiting until released work settles"
    );
    let CommandAnswer::Task(answer) = assert_committed(conclude(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &design,
        done_conclusion(),
    )) else {
        panic!("Design conclusion returned another answer")
    };
    assert!(
        answer.closed.is_empty(),
        "conclusion finds nothing still gated by the Design"
    );
    let settled = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&settled, &accepted).workflow.body.status.is_pending());
    clean(&root, bus);
}

#[test]
fn each_completed_pass_owes_one_decision() {
    let (root, bus, epoch, seats) = fixture("preparation-repeated-pass", 1);
    let design = add_controller(
        &bus,
        epoch,
        "Repeated design",
        wire::TaskIntent::Design,
        None,
    );
    capture_design(&bus, epoch, &seats[0], &design);
    let output = add_plan_output(&bus, epoch, &design, "Vetoed output");
    let design_stamp = design.born().expect("Design stamp");

    assert_committed(end_pass(&bus, epoch, &seats[0], &design));
    assert_eq!(
        open_awaiting_selection(&bus, epoch),
        [design_stamp.clone()].into(),
        "a completed pass owes the operator a decision"
    );
    assert_committed(select_outputs(&bus, epoch, &design, &[], 1));
    assert!(open_awaiting_selection(&bus, epoch).is_empty());
    assert!(
        !at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &output)
            .workflow
            .body
            .status
            .is_pending(),
        "a veto closes the output, so no later pass offers it"
    );
    assert!(
        !open_workflow_waiting(&bus, epoch).contains(&design_stamp),
        "a pass that released nothing leaves the Design ready for its next pass"
    );

    // The next pass offers new work built under the renewed claim.
    claim_task(&bus, epoch, &seats[0], &Actor::Lane(seats[0].lane), &design);
    let revised = add_plan_output(&bus, epoch, &design, "Revised output");
    assert_committed(end_pass(&bus, epoch, &seats[0], &design));
    assert_eq!(open_awaiting_selection(&bus, epoch), [design_stamp].into());
    let (_, basis) = read_pass(&bus, epoch, &design);
    assert_eq!(
        basis
            .outputs
            .iter()
            .map(|revision| &revision.task)
            .collect::<Vec<_>>(),
        [&revised.born().expect("revised stamp")],
        "only the open output is offered"
    );
    assert_committed(select_outputs(&bus, epoch, &design, &[], 2));
    assert!(open_awaiting_selection(&bus, epoch).is_empty());
    clean(&root, bus);
}
