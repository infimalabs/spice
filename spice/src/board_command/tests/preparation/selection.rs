//! Operator selection: accepted outputs release under retained preparation,
//! rejected outputs close, and a selection that would loop is refused.

use super::*;

#[test]
fn operator_selection_releases_outputs_and_leaves_the_design_unclaimed_and_open() {
    let (root, bus, epoch, seats) = fixture("preparation-held-selection", 2);
    let controller = add_controller(
        &bus,
        epoch,
        "Held preparation controller",
        wire::TaskIntent::Design,
        None,
    );
    capture_design(&bus, epoch, &seats[0], &controller);
    let output = add_plan_output(&bus, epoch, &controller, "Selected held output");
    let spare = add_plan_output(&bus, epoch, &controller, "Unselected held output");
    assert_committed(end_pass(&bus, epoch, &seats[0], &controller));
    assert_eq!(
        open_awaiting_selection(&bus, epoch),
        [controller.born().expect("Design stamp")].into(),
        "the completed pass waits on its exact operator decision"
    );

    authorize_test_output(&bus, epoch, &controller, &output, &spare);

    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(
        at(&current, &controller)
            .latest_claim()
            .map(|held| &held.claim)
            .is_none_or(|claim| claim.body.disposition != ClaimDisposition::Active),
        "the completed pass released the Design claim before operator selection"
    );
    let gated = open_gated(&bus, epoch);
    assert!(
        !gated.contains(&output.born().expect("output stamp")),
        "the selected output runs while its Design stays open"
    );
    assert!(
        !gated.contains(&spare.born().expect("spare stamp"))
            && !at(&current, &spare).workflow.body.status.is_pending(),
        "the rejected spare is closed by the decision rather than left gated"
    );
    assert!(
        open_awaiting_selection(&bus, epoch).is_empty(),
        "the Design waits on its released output rather than on a decision"
    );

    // Every read derives the Design's wait from the released output's
    // preparation, without an ordinary edge on either statement.
    let design_stamp = controller.born().expect("Design stamp");
    assert!(
        at(&current, &controller)
            .statement
            .body
            .dependencies
            .is_empty()
    );
    assert_reads_released_lifecycle_relation(
        &bus,
        epoch,
        &design_stamp,
        &output.born().expect("output stamp"),
    );
    clean(&root, bus);
}

/// The Design's own task read carries its released output, and the typed
/// dependency graph draws their virtual lifecycle relation.
fn assert_reads_released_lifecycle_relation(
    bus: &Bus,
    epoch: CanonicalUuid,
    design_stamp: &TaskStamp,
    output_stamp: &TaskStamp,
) {
    let Outcome::Reply(Reply {
        body: CommandAnswer::Board(shown),
        ..
    }) = issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::BoardRead,
        Vec::new(),
        &BoardRead::Tasks {
            tasks: vec![design_stamp.clone()],
            dependencies: true,
            claims: false,
            history: false,
        },
    )
    else {
        panic!("the Design's task read did not reply with a board")
    };
    assert!(
        shown
            .tasks
            .iter()
            .any(|row| row.statement.body.born == *output_stamp),
        "task show carries the released output beside the Design"
    );
    let (_, projected) = relations::answer(relations::read_relations(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &wire::intent::TaskRelations {
            roots: vec![design_stamp.clone()],
            relations: vec![wire::intent::TaskRelationKind::Dependency],
            depth: 1,
            node_limit: 16,
        },
    ));
    assert!(
        projected.edges.iter().any(|edge| {
            edge.relation == wire::intent::TaskRelationKind::Dependency
                && edge.from
                    == wire::intent::TaskRelationNodeRef::Task {
                        task: design_stamp.clone(),
                    }
                && edge.to
                    == wire::intent::TaskRelationNodeRef::Task {
                        task: output_stamp.clone(),
                    }
        }),
        "the dependency relation draws the Design waiting on its released output: {:?}",
        projected.edges
    );
}

#[test]
fn later_passes_release_additional_outputs() {
    let (root, bus, epoch, seats) = fixture("preparation-multi-pass-selection", 1);
    let design = add_controller(
        &bus,
        epoch,
        "Multi-pass preparation controller",
        wire::TaskIntent::Design,
        None,
    );
    capture_design(&bus, epoch, &seats[0], &design);
    let first = add_plan_output(&bus, epoch, &design, "First pass output");
    assert_committed(end_pass(&bus, epoch, &seats[0], &design));
    assert_committed(select_outputs(&bus, epoch, &design, &[&first], 1));
    assert!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &first)
            .workflow
            .body
            .is_activated()
    );
    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&current, &first);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskClose,
        transition_witnesses(heads),
        &TaskClose {
            task: first.clone(),
            validation: "the first selected Plan is settled".to_owned(),
            force_claimed: false,
        },
    ));

    capture_design(&bus, epoch, &seats[0], &design);
    let second = add_plan_output(&bus, epoch, &design, "Second pass output");
    assert_committed(end_pass(&bus, epoch, &seats[0], &design));
    assert_committed(select_outputs(&bus, epoch, &design, &[&second], 2));
    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&current, &first).workflow.body.status.is_completed());
    assert!(at(&current, &second).workflow.body.is_activated());
    assert!(
        !open_awaiting_selection(&bus, epoch).contains(&design.born().expect("Design stamp")),
        "every pass is decided"
    );
    assert!(
        at(&current, &design).workflow.body.status.is_pending(),
        "selections never conclude the Design"
    );
    clean(&root, bus);
}

#[test]
fn a_decision_refuses_an_accepted_output_that_waits_on_work_the_design_still_gates() {
    let (root, bus, epoch, seats) = fixture("preparation-selection-cycle", 2);
    let design = add_controller(&bus, epoch, "Cyclic design", wire::TaskIntent::Design, None);
    capture_design(&bus, epoch, &seats[0], &design);
    let selected = add_plan_output(&bus, epoch, &design, "Selected plan");
    let sibling = add_plan_output(&bus, epoch, &design, "Sibling plan");
    let prerequisite = add_plan_output(&bus, epoch, &design, "Prerequisite plan");
    let before = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDepends,
        dependencies::dependency_witnesses(&before, &selected, &[&prerequisite], None, false),
        &TaskDepends {
            task: selected.clone(),
            after: vec![prerequisite.clone()],
            remove: false,
        },
    ));
    assert_committed(end_pass(&bus, epoch, &seats[0], &design));
    let refused = select_outputs(&bus, epoch, &design, &[&selected], 3);
    let Outcome::Refusal(refusal) = &refused else {
        panic!("a selection closing a dependency loop committed: {refused:?}")
    };
    assert_eq!(refusal.reason(), "task::dependency-draft");
    let unchanged = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let design_stamp = design.born().expect("Design stamp");
    for task in [&selected, &sibling, &prerequisite] {
        let heads = at(&unchanged, task);
        assert!(heads.workflow.body.status.is_pending());
        assert!(heads.workflow.body.is_draft());
    }
    assert!(open_awaiting_selection(&bus, epoch).contains(&design_stamp));
    clean(&root, bus);
}
