//! Operator selection: accepted outputs release under retained preparation,
//! rejected outputs close, and a selection that would loop is refused.

use super::*;
use wire::rooms::{ROOM_PUBLICATION_BYTES_LIMIT, RoomPublicationContent, RoomPublished};

fn output_publications(bus: &Bus, output: &TaskHandle) -> journal::SelectedFactPage {
    let room = output.born().expect("fixture output carries a birth stamp");
    bus.read(move |read| {
        read.room_publications(
            &room,
            None,
            journal::FactReadBudget::new(8, journal::MAX_RETAINED_FACT_PAGE_BYTES)
                .expect("fixture room page budget"),
        )
    })
    .expect("output room publications read")
}

#[test]
fn accepted_output_comment_is_published_once_in_its_room() {
    let (root, bus, epoch, seats) = fixture("preparation-selection-comment", 1);
    let design = add_controller(
        &bus,
        epoch,
        "Commented selection",
        wire::TaskIntent::Design,
        None,
    );
    capture_design(&bus, epoch, &seats[0], &design);
    let accepted = add_plan_output(&bus, epoch, &design, "Accepted output");
    let rejected = add_plan_output(&bus, epoch, &design, "Rejected output");
    assert_committed(end_pass(&bus, epoch, &seats[0], &design));

    let read = read_pass(&bus, epoch, &design);
    let accepted_stamp = accepted.born().expect("accepted output stamp");
    let outputs = read
        .1
        .outputs
        .iter()
        .map(|revision| OutputDecision {
            output: revision.task.clone(),
            verdict: if revision.task == accepted_stamp {
                OutputVerdict::Accepted
            } else {
                OutputVerdict::Rejected
            },
            comment: Some(if revision.task == accepted_stamp {
                "Start with the retained operator constraint.".to_owned()
            } else {
                "This rejection remains only on the Design decision.".to_owned()
            }),
        })
        .collect();
    let outcome = DecisionOutcome::Resolved {
        outputs,
        comment: Some("This overall comment also remains on the Design.".to_owned()),
    };
    let mut oversized = outcome.clone();
    let DecisionOutcome::Resolved { outputs, .. } = &mut oversized else {
        unreachable!("fixture decision is resolved")
    };
    outputs
        .iter_mut()
        .find(|output| output.verdict == OutputVerdict::Accepted)
        .expect("fixture has an accepted output")
        .comment = Some("x".repeat(ROOM_PUBLICATION_BYTES_LIMIT + 1));
    let refused = decide_read(&bus, epoch, &design, 1, read.clone(), oversized);
    let Outcome::Refusal(refusal) = refused else {
        panic!("an oversized copied room comment committed: {refused:?}")
    };
    assert_eq!(refusal.reason(), "room::post-too-large");
    assert!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &accepted)
            .workflow
            .body
            .is_draft()
    );
    assert!(output_publications(&bus, &accepted).rows.is_empty());
    assert_committed(decide_read(
        &bus,
        epoch,
        &design,
        1,
        read.clone(),
        outcome.clone(),
    ));
    assert!(
        matches!(
            decide_read(&bus, epoch, &design, 1, read, outcome.clone()),
            Outcome::Refusal(_)
        ),
        "a stale retry refuses instead of recording another publication"
    );

    let page = output_publications(&bus, &accepted);
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].actor, Actor::TEST_PRINCIPAL);
    let published = page.rows[0]
        .fact
        .read::<RoomPublished>()
        .expect("accepted output publication has its typed body");
    assert_eq!(
        published.content,
        RoomPublicationContent::Authored {
            text: "Start with the retained operator constraint.".to_owned()
        }
    );
    let cause = page.rows[0]
        .cause
        .expect("copied output comment is caused by its decision");
    let cause_page = bus
        .read(move |read| {
            read.fact_page(
                journal::FactPageStart::At(cause),
                cause,
                journal::FactReadBudget::new(1, journal::MAX_RETAINED_FACT_PAGE_BYTES)
                    .expect("fixture cause page budget"),
            )
        })
        .expect("decision cause reads");
    let decided = cause_page.rows[0]
        .fact
        .read::<wire::rooms::PreparationDecided>()
        .expect("publication cause is the typed decision");
    assert_eq!(decided.decision.outcome, outcome);
    assert!(
        output_publications(&bus, &rejected).rows.is_empty(),
        "rejected-output and overall comments do not publish to output rooms"
    );
    clean(&root, bus);
}

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
    assert_reads_preparation_without_dependency_relation(
        &bus,
        epoch,
        &design_stamp,
        &output.born().expect("output stamp"),
    );
    clean(&root, bus);
}

/// The Design's task read may carry released lifecycle context, while the
/// typed relation graph exposes it only through the preparation controller.
fn assert_reads_preparation_without_dependency_relation(
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
            relations: vec![
                wire::intent::TaskRelationKind::Dependency,
                wire::intent::TaskRelationKind::Controller,
            ],
            depth: 1,
            node_limit: 16,
        },
    ));
    let joins_output = |edge: &wire::intent::TaskRelationEdge| {
        edge.from
            == (wire::intent::TaskRelationNodeRef::Task {
                task: design_stamp.clone(),
            })
            && edge.to
                == (wire::intent::TaskRelationNodeRef::Task {
                    task: output_stamp.clone(),
                })
    };
    assert!(
        !projected.edges.iter().any(|edge| {
            edge.relation == wire::intent::TaskRelationKind::Dependency && joins_output(edge)
        }),
        "preparation state must not masquerade as an execution dependency: {:?}",
        projected.edges
    );
    assert!(
        projected.edges.iter().any(|edge| {
            edge.relation == wire::intent::TaskRelationKind::Controller && joins_output(edge)
        }),
        "the controller relation retains the Design-to-output connection: {:?}",
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
