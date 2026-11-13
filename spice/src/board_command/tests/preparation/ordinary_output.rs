use super::*;

/// Selection advances an ordinary output directly to execution and preserves
/// construction lineage without manufacturing a dependency on the Design.
#[test]
fn a_design_selects_an_ordinary_output_with_a_plain_typed_phase() {
    let (root, bus, epoch, seats) = fixture("preparation-design-ordinary-output", 2);
    let design = add_controller(
        &bus,
        epoch,
        "Ordinary design",
        wire::TaskIntent::Design,
        None,
    );
    capture_design(&bus, epoch, &seats[0], &design);
    let selected = add_output_with_flow(&bus, epoch, &design, "Selected ordinary task", "todo");
    let sibling = add_output(&bus, epoch, &design, "Sibling ordinary task");
    let born = |task: &TaskHandle| task.born().expect("task stamp");
    let before = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&before, &selected).workflow.body.is_draft());
    assert!(
        at(&before, &selected)
            .statement
            .body
            .dependencies
            .is_empty()
    );
    assert!(at(&before, &sibling).workflow.body.is_draft());

    assert_committed(end_pass(&bus, epoch, &seats[0], &design));
    authorize_test_output(&bus, epoch, &design, &selected, &sibling);
    assert_eq!(
        open_workflow_waiting(&bus, epoch),
        std::collections::BTreeSet::from([born(&design)]),
        "the veto closes the sibling and retained preparation keeps the Design waiting"
    );
    let after = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let selected_row = at(&after, &selected);
    assert_eq!(selected_row.workflow.body.phase, wire::Phase::Todo);
    assert_eq!(
        selected_row
            .statement
            .body
            .preparation
            .as_ref()
            .map(|preparation| &preparation.controller),
        Some(&born(&design))
    );
    capture_for(&bus, epoch, &seats[1], &selected);
    clean(&root, bus);
}
