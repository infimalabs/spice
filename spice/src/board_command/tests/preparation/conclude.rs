//! Only the operator concludes an unheld open Design, done or collapsed.

use std::collections::BTreeSet;

use super::*;

pub(in crate::board_command::tests) fn done_conclusion() -> body::DesignConclusion {
    body::DesignConclusion::Done {}
}

fn collapse(reason: &str) -> body::DesignConclusion {
    body::DesignConclusion::Collapsed {
        reason: reason.to_owned(),
    }
}

pub(in crate::board_command::tests) fn conclusion_witnesses(
    bus: &Bus,
    epoch: CanonicalUuid,
    task: &TaskHandle,
) -> Vec<Witness> {
    transition_witnesses(at(&board(bus, epoch, Actor::TEST_PRINCIPAL), task))
}

fn conclude_witnessed(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    task: &TaskHandle,
    conclusion: body::DesignConclusion,
    expected: Vec<Witness>,
) -> Outcome<CommandAnswer> {
    issue(
        bus,
        epoch,
        actor,
        CommandKind::TaskConclude,
        expected,
        &TaskConclude {
            task: task.clone(),
            conclusion,
        },
    )
}

/// Drive one conclusion against the heads the board shows right now.
pub(in crate::board_command::tests) fn conclude(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    task: &TaskHandle,
    conclusion: body::DesignConclusion,
) -> Outcome<CommandAnswer> {
    let expected = conclusion_witnesses(bus, epoch, task);
    conclude_witnessed(bus, epoch, actor, task, conclusion, expected)
}

fn workflow(bus: &Bus, epoch: CanonicalUuid, task: &TaskHandle) -> wire::WorkflowHead {
    at(&board(bus, epoch, Actor::TEST_PRINCIPAL), task)
        .workflow
        .body
        .clone()
}

/// Admit a conclusion as `actor` without dispatching it.
fn fixture_command_admission(
    epoch: CanonicalUuid,
    actor: Actor,
    expected: Vec<Witness>,
    task: &TaskHandle,
) -> Result<wire::AdmittedCommand, wire::CommandRejection> {
    Command {
        v: WireVersion::CURRENT,
        id: uuid(9_999),
        epoch,
        kind: CommandKind::TaskConclude,
        expected,
        body: serde_json::to_value(TaskConclude {
            task: task.clone(),
            conclusion: done_conclusion(),
        })
        .expect("conclusion encodes"),
    }
    .admit(
        &Accepted {
            wire: WireVersion::CURRENT,
            actor,
            epoch,
            release: "1.2.3".parse().expect("fixture release"),
        },
        &wire::principal::PrincipalRoles::default(),
    )
}

#[test]
fn the_operator_alone_concludes_an_unheld_open_design() {
    let (root, bus, epoch, seats) = fixture("design-conclude-done", 1);
    let lane = Actor::Lane(seats[0].lane);
    let design = add_controller(
        &bus,
        epoch,
        "Concluded design",
        wire::TaskIntent::Design,
        None,
    );
    let by_lane = fixture_command_admission(
        epoch,
        lane.clone(),
        conclusion_witnesses(&bus, epoch, &design),
        &design,
    );
    assert!(
        matches!(&by_lane, Err(wire::CommandRejection::Refusal(refusal)) if refusal.reason() == "command::authority"),
        "a lane principal never concludes a Design: {by_lane:?}"
    );
    capture_design(&bus, epoch, &seats[0], &design);
    let held = conclude(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &design,
        done_conclusion(),
    );
    assert_eq!(refusal_reason(&held), "task::conclude-claimed");
    let Outcome::Refusal(refusal) = &held else {
        unreachable!()
    };
    assert_eq!(
        refusal
            .diagnostic()
            .and_then(wire::RefusalDiagnostic::observed),
        Some(lane.to_string().as_str()),
        "the refusal names the holder who must free it"
    );
    assert!(workflow(&bus, epoch, &design).status.is_pending());

    // A conclusion witnessed while the Design was held refuses once the holder
    // frees it, rather than concluding over heads it did not read.
    let stale = conclusion_witnesses(&bus, epoch, &design);
    free_active(&bus, epoch, &lane, &seats[0], &design);
    let raced = conclude_witnessed(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &design,
        done_conclusion(),
        stale,
    );
    assert!(matches!(raced, Outcome::Refusal(_)), "{raced:?}");
    assert!(workflow(&bus, epoch, &design).status.is_pending());

    let answer = assert_committed(conclude(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &design,
        done_conclusion(),
    ));
    let CommandAnswer::Task(answer) = answer else {
        panic!("conclusion did not return a task answer: {answer:?}");
    };
    assert_eq!(answer.status, "done");
    let concluded = workflow(&bus, epoch, &design);
    assert!(concluded.status.is_completed());
    assert!(concluded.status.completion().is_some());
    assert!(
        concluded.concluded_positively(),
        "done concludes positively"
    );
    for again in [done_conclusion(), collapse("premise failed")] {
        let repeated = conclude(&bus, epoch, Actor::TEST_PRINCIPAL, &design, again);
        assert_eq!(refusal_reason(&repeated), "task::settled");
    }
    assert_eq!(workflow(&bus, epoch, &design), concluded);
    clean(&root, bus);
}

#[test]
fn conclusion_remains_design_only_while_ordinary_close_can_settle_a_design() {
    let (root, bus, epoch, _seats) = fixture("design-conclude-purpose", 1);
    let design = add_controller(&bus, epoch, "Closed design", wire::TaskIntent::Design, None);
    let plan = add_plan(&bus, epoch, "Concluded plan");
    let ordinary = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "Concluded ordinary",
        "Concluded ordinary",
        None,
    );
    for task in [&plan, &ordinary] {
        let refused = conclude(&bus, epoch, Actor::TEST_PRINCIPAL, task, done_conclusion());
        assert_eq!(refusal_reason(&refused), "task::conclude-not-design");
    }
    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&visible, &design);
    let closed = issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskClose,
        transition_witnesses(heads),
        &TaskClose {
            task: design.clone(),
            validation: "closed".to_owned(),
            force_claimed: true,
        },
    );
    assert_committed(closed);
    assert!(workflow(&bus, epoch, &design).status.is_completed());
    clean(&root, bus);
}

#[test]
fn collapse_settles_unselected_tree_without_retracting_selected_output() {
    let (root, bus, epoch, seats) = fixture("design-conclude-collapse", 2);
    let design = add_controller(
        &bus,
        epoch,
        "Collapsed design",
        wire::TaskIntent::Design,
        None,
    );
    capture_design(&bus, epoch, &seats[0], &design);
    let output = add_plan_output(&bus, epoch, &design, "Selected collapsed output");
    let spare = add_plan_output(&bus, epoch, &design, "Rejected collapsed Plan");
    let descendant = add_output(&bus, epoch, &spare, "Rejected Plan output");
    assert_committed(end_pass(&bus, epoch, &seats[0], &design));
    assert_committed(select_outputs(&bus, epoch, &design, &[&output], 1));
    let undecided = add_plan_output(&bus, epoch, &design, "Undecided collapsed Plan");
    let undecided_descendant = add_output(&bus, epoch, &undecided, "Undecided Plan output");
    let output_stamp = output.born().expect("output stamp");
    assert!(!open_blocked(&bus, epoch).contains(&output_stamp));
    for closed in [&spare, &descendant] {
        assert!(
            workflow(&bus, epoch, closed).status.is_completed(),
            "the veto settles the rejected Plan with its pending subtree"
        );
    }

    for blank in ["", "  \n"] {
        let refused = conclude(&bus, epoch, Actor::TEST_PRINCIPAL, &design, collapse(blank));
        assert_eq!(refusal_reason(&refused), "task::collapse-reason-empty");
    }
    let answer = assert_committed(conclude(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &design,
        collapse("the premise did not hold"),
    ));
    let CommandAnswer::Task(answer) = answer else {
        panic!("collapse did not return a task answer: {answer:?}");
    };
    assert_eq!(answer.status, "collapsed");
    assert_eq!(
        answer
            .closed
            .iter()
            .map(|task| task.born().expect("closed stamp"))
            .collect::<BTreeSet<_>>(),
        [&undecided, &undecided_descendant]
            .iter()
            .map(|task| task.born().expect("output stamp"))
            .collect(),
        "the answer names every pending task still gated by the controller"
    );
    let collapsed = workflow(&bus, epoch, &design);
    assert!(collapsed.status.is_completed());
    assert!(
        !collapsed.concluded_positively(),
        "collapse concludes negatively"
    );
    assert!(
        workflow(&bus, epoch, &output).status.is_pending(),
        "an authorized output remains live"
    );
    assert!(!open_blocked(&bus, epoch).contains(&output_stamp));
    for closed in [&undecided, &undecided_descendant] {
        assert!(
            workflow(&bus, epoch, closed).status.is_completed(),
            "every unselected branch member is settled"
        );
    }
    let closed_stamps = [&undecided, &undecided_descendant]
        .map(|task| task.born().expect("closed task stamp"))
        .into_iter()
        .collect::<BTreeSet<_>>();
    let validations = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("closure facts read")
        .facts()
        .iter()
        .filter_map(|fact| fact.fact.read::<body::TaskClosed>())
        .filter(|closed| closed_stamps.contains(&closed.task))
        .map(|closed| closed.validation)
        .collect::<Vec<_>>();
    assert_eq!(
        validations,
        vec![format!("Controller {design} was collapsed: the premise did not hold"); 2]
    );
    let design_stamp = design.born().expect("design stamp");
    let recorded = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("conclusion facts read")
        .facts()
        .iter()
        .filter_map(|fact| fact.fact.read::<body::TaskConcluded>())
        .filter(|concluded| concluded.task == design_stamp)
        .collect::<Vec<_>>();
    assert_eq!(
        recorded,
        vec![body::TaskConcluded {
            task: design_stamp,
            conclusion: collapse("the premise did not hold"),
        }],
        "the collapse keeps its stated reason durably"
    );
    clean(&root, bus);
}
