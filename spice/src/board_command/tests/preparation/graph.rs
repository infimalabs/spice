use super::*;

/// A resolution accepting every one of `outputs`.
fn accept_all(outputs: &[&TaskHandle]) -> DecisionOutcome {
    DecisionOutcome::Resolved {
        outputs: outputs
            .iter()
            .map(|output| OutputDecision {
                output: output.born().expect("output stamp"),
                verdict: OutputVerdict::Accepted,
                comment: None,
            })
            .collect(),
        comment: None,
    }
}

#[test]
fn selected_draft_prerequisites_advance_together_but_execution_stays_blocked() {
    let (root, bus, epoch, seats) = fixture("preparation-draft-graph", 2);
    let design = add_controller(&bus, epoch, "Graph Design", wire::TaskIntent::Design, None);
    capture_design(&bus, epoch, &seats[0], &design);
    let first = add_output_with_flow(&bus, epoch, &design, "First task", "todo");
    let second = add_output_with_flow(&bus, epoch, &design, "Dependent task", "todo");
    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDepends,
        dependency_witnesses(&current, &second, &[&first], None, false),
        &TaskDepends {
            task: second.clone(),
            after: vec![first.clone()],
            remove: false,
        },
    ));
    assert_committed(end_pass(&bus, epoch, &seats[0], &design));
    let read = read_pass(&bus, epoch, &design);
    let mut correction = only(&first);
    correction.title = Some("Refined prerequisite".to_owned());
    assert_committed(super::super::edit::correct(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &correction,
    ));
    let refused = decide_read(
        &bus,
        epoch,
        &design,
        3,
        read,
        accept_all(&[&first, &second]),
    );
    assert!(
        matches!(&refused, Outcome::Refusal(refusal)
            if refusal.reason() == "preparation::output-revised"),
        "{refused:?}"
    );
    let pending = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&pending, &first).workflow.body.is_draft());
    assert!(at(&pending, &second).workflow.body.is_draft());
    assert_committed(select_outputs(&bus, epoch, &design, &[&first, &second], 3));
    let released = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    for output in [&first, &second] {
        assert!(at(&released, output).workflow.body.is_activated());
    }
    assert_eq!(
        at(&released, &second).statement.body.dependencies,
        vec![first.born().expect("prerequisite stamp")]
    );
    assert!(open_gated(&bus, epoch).contains(&second.born().expect("dependent stamp")));
    let worker = Actor::Lane(seats[1].lane);
    claim_and_complete_task(&bus, epoch, &seats[1], &worker, &first);
    assert!(!open_gated(&bus, epoch).contains(&second.born().expect("dependent stamp")));
    claim_task(&bus, epoch, &seats[1], &worker, &second);
    clean(&root, bus);
}

#[test]
fn changing_an_offered_flow_or_dependencies_refuses_a_stale_decision() {
    for change in ["flow", "dependencies"] {
        let (root, bus, epoch, seats) = fixture(change, 1);
        let design = add_controller(
            &bus,
            epoch,
            "Revision Design",
            wire::TaskIntent::Design,
            None,
        );
        capture_design(&bus, epoch, &seats[0], &design);
        let first = add_output(&bus, epoch, &design, "Revised output");
        let second = add_output(&bus, epoch, &design, "Other output");
        assert_committed(end_pass(&bus, epoch, &seats[0], &design));
        let read = read_pass(&bus, epoch, &design);
        let before = board(&bus, epoch, Actor::TEST_PRINCIPAL);
        if change == "dependencies" {
            assert_committed(issue(
                &bus,
                epoch,
                Actor::TEST_PRINCIPAL,
                CommandKind::TaskDepends,
                dependency_witnesses(&before, &first, &[&second], None, false),
                &TaskDepends {
                    task: first.clone(),
                    after: vec![second.clone()],
                    remove: false,
                },
            ));
        } else {
            assert_committed(super::super::edit::correct(
                &bus,
                epoch,
                Actor::TEST_PRINCIPAL,
                &TaskEdit {
                    flow: Some("todo".to_owned()),
                    ..only(&first)
                },
            ));
        }
        let refused = decide_read(
            &bus,
            epoch,
            &design,
            3,
            read,
            accept_all(&[&first, &second]),
        );
        assert!(
            matches!(&refused, Outcome::Refusal(refusal)
            if refusal.reason() == "preparation::output-revised"),
            "{refused:?}"
        );
        let unchanged = board(&bus, epoch, Actor::TEST_PRINCIPAL);
        assert!(at(&unchanged, &first).workflow.body.is_draft());
        assert!(at(&unchanged, &second).workflow.body.is_draft());
        assert_committed(select_outputs(&bus, epoch, &design, &[&first, &second], 3));
        assert!(
            at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &first)
                .workflow
                .body
                .is_activated()
        );
        clean(&root, bus);
    }
}

#[test]
fn ambient_construction_keeps_incomplete_work_in_draft_without_association_dependencies() {
    let (root, bus, epoch, seats) = fixture("preparation-ambient-draft", 1);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    let design = add_controller(
        &bus,
        epoch,
        "Ambient Design",
        wire::TaskIntent::Design,
        None,
    );
    capture_design(&bus, epoch, &seats[0], &design);
    for kind in ["ambient", "explicit", "independent"] {
        let current = board(&bus, epoch, Actor::Lane(seats[0].lane));
        let parent = at(&current, &design);
        let born = stamp(kind);
        let independent = kind == "independent";
        let add = TaskAdd {
            preparation: (kind == "explicit").then(|| design.clone()),
            topic: independent.then(|| "/board/test".parse().expect("topic")),
            term: if independent {
                vec!["The independent work executes.".to_owned()]
            } else {
                Vec::new()
            },
            origin: independent.then(fixture_origin),
            draft: kind == "explicit",
            ..adding(
                kind.to_owned(),
                "Prospective construction must not execute early.",
            )
        };
        let mut expected = vec![
            Witness::Absence { task: born.clone() },
            residency(&current, &seats[0]),
        ];
        crate::board_command::client::append_preparation_witnesses(&current, &add, &mut expected)
            .expect("the public creation route witnesses its preparation");
        assert_committed(issue(
            &bus,
            epoch,
            Actor::Lane(seats[0].lane),
            CommandKind::TaskAdd,
            expected,
            &add,
        ));
        let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
        let output = current
            .tasks
            .iter()
            .find(|row| row.statement.body.born == born)
            .expect("created output remains reachable");
        assert!(output.statement.body.dependencies.is_empty());
        if independent {
            assert!(output.workflow.body.is_activated());
            assert!(output.statement.body.preparation.is_none());
        } else {
            assert!(output.workflow.body.is_draft());
            assert!(output.statement.body.topic.placed.is_none());
            assert_eq!(
                output
                    .statement
                    .body
                    .preparation
                    .as_ref()
                    .map(|preparation| &preparation.parent),
                Some(&parent.statement.body.born)
            );
        }
    }
    clean(&root, bus);
}
