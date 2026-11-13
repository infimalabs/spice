use super::*;

fn topic(bus: &Bus, epoch: CanonicalUuid, task: &TaskHandle) -> wire::Topic {
    at(&board(bus, epoch, Actor::TEST_PRINCIPAL), task)
        .statement
        .body
        .topic
        .path
        .clone()
        .expect("the task is placed")
}

/// OOPS intake records prospective work under its Design regardless of destination.
/// Drive policy is checked when the controlled Draft is released, not when authored.
#[test]
fn reporting_intake_keeps_default_and_outside_outputs_in_controlled_draft() {
    let (root, bus, epoch, seats) = fixture("oops-capability-intake", 1);
    let seat = &seats[0];
    authorize_lane_creation(&bus, epoch, seat);
    policy(&root, "[]");
    let actor = Actor::Lane(seat.lane);
    let design = stamp("oops-capability-design");
    let mut default_report = None;
    for (label, topic) in [
        ("oops-capability-report", None),
        ("oops-outside-report", Some("/outside/report")),
    ] {
        let before = board(&bus, epoch, actor.clone());
        let generation = before.oops_generation.as_ref();
        let mut expected = vec![
            Witness::Absence { task: stamp(label) },
            Witness::OopsGeneration {
                head: generation.map(|head| head.position),
                design: generation.and_then(|head| head.body.design.clone()),
            },
        ];
        if let Some(heads) = before
            .tasks
            .iter()
            .find(|heads| heads.statement.body.born == design)
        {
            expected.push(statement_witness(heads));
            expected.push(workflow_witness(heads));
        } else {
            expected.push(Witness::Absence {
                task: design.clone(),
            });
        }
        let CommandAnswer::Task(answer) = assert_committed(issue(
            &bus,
            epoch,
            actor.clone(),
            CommandKind::TaskOopsAdd,
            expected,
            &TaskOopsAdd {
                design: design.clone(),
                title: "Record friction under restrictive policy".into(),
                origin: Some(fixture_origin()),
                topic: topic.map(|path| path.parse().unwrap()),
            },
        )) else {
            panic!("OOPS returns a task receipt")
        };
        if topic.is_none() {
            default_report = Some(answer.task.clone());
        }
        let after = board(&bus, epoch, actor.clone());
        assert_eq!(
            after.oops_generation.as_ref().unwrap().body.design,
            Some(design.clone())
        );
        let report = at(&after, &answer.task);
        let filed = topic.unwrap_or("/oops").parse().unwrap();
        assert_eq!(report.statement.body.topic.path, Some(filed));
        assert!(report.workflow.body.is_draft());
        assert!(!report.workflow.body.submitted);
        let preparation = report
            .statement
            .body
            .preparation
            .as_ref()
            .expect("controlled output");
        assert_eq!(preparation.parent, design);
        assert_eq!(preparation.controller, design);
        assert!(report.statement.body.dependencies.is_empty());
        assert!(report.claims.is_empty());
        assert!(answer.claim.is_none());
        assert!(answer.ownership.as_ref().unwrap().teams.is_empty());
        let design_heads = after
            .tasks
            .iter()
            .find(|heads| heads.statement.body.born == design)
            .unwrap();
        let design_topic = "/oops/design".parse().unwrap();
        assert_eq!(design_heads.statement.body.topic.path, Some(design_topic));
        assert!(design_heads.workflow.body.is_activated());
    }
    let report = default_report.expect("default OOPS report");
    let current = board(&bus, epoch, actor.clone());
    let heads = at(&current, &report);
    let CommandAnswer::Task(moved_answer) = assert_committed(issue(
        &bus,
        epoch,
        actor,
        CommandKind::TaskEdit,
        vec![statement_witness(heads)],
        &TaskEdit {
            topic: Some("/outside/moved".parse().unwrap()),
            ..only(&report)
        },
    )) else {
        panic!("moving an existing OOPS report answers with its task")
    };
    assert_eq!(
        topic(&bus, epoch, &moved_answer.task).as_str(),
        "/outside/moved"
    );

    clean(&root, bus);
}

#[test]
fn ordinary_creation_edit_and_advance_cannot_enter_the_oops_topic() {
    let (root, bus, epoch, seats) = fixture("oops-reserved-placement", 1);
    let seat = &seats[0];
    authorize_lane_creation(&bus, epoch, seat);
    policy(&root, "true");
    let actor = Actor::Lane(seat.lane);

    for (label, topic, draft) in [
        ("oops-reserved-root", "/oops", false),
        ("oops-reserved-child", "/oops/report", true),
    ] {
        let mut add = authored_add(
            "Ordinary work cannot masquerade as a report",
            topic,
            Some("todo"),
            &["The ordinary task stays outside OOPS"],
        );
        add.draft = draft;
        let born = stamp(label);
        let before = bus.cursor().unwrap();
        let outcome = issue(
            &bus,
            epoch,
            actor.clone(),
            CommandKind::TaskAdd,
            vec![Witness::Absence { task: born.clone() }],
            &add,
        );
        assert!(
            matches!(
                outcome,
                Outcome::Refusal(ref refusal)
                    if refusal.reason() == "task::oops-topic-reserved"
            ),
            "{outcome:?}"
        );
        assert_eq!(bus.cursor().unwrap(), before);
        assert!(bus.task_snapshot().unwrap().task_heads(&born).is_none());
    }

    let mut draft_add = authored_add(
        "An ordinary saved Draft",
        "/board/test",
        Some("todo"),
        &["The Draft remains in its ordinary topic"],
    );
    draft_add.draft = true;
    let draft = issue_add(&bus, epoch, actor.clone(), "oops-reserved-draft", draft_add);
    let current = board(&bus, epoch, actor.clone());
    let heads = at(&current, &draft);
    let before = bus.cursor().unwrap();
    let filing = issue(
        &bus,
        epoch,
        actor.clone(),
        CommandKind::TaskFile,
        vec![
            statement_witness(heads),
            workflow_witness(heads),
            deferral_witness(heads),
        ],
        &TaskFile {
            task: draft.clone(),
            topic: Some("/oops/report".parse().unwrap()),
            claim: false,
        },
    );
    assert!(
        matches!(
            filing,
            Outcome::Refusal(ref refusal)
                if refusal.reason() == "task::oops-topic-reserved"
        ),
        "{filing:?}"
    );
    assert_eq!(bus.cursor().unwrap(), before);

    let editing = issue(
        &bus,
        epoch,
        actor,
        CommandKind::TaskEdit,
        vec![statement_witness(heads)],
        &TaskEdit {
            topic: Some("/oops".parse().unwrap()),
            ..only(&draft)
        },
    );
    assert!(
        matches!(
            editing,
            Outcome::Refusal(ref refusal)
                if refusal.reason() == "task::oops-topic-reserved"
        ),
        "{editing:?}"
    );
    assert_eq!(bus.cursor().unwrap(), before);
    let unchanged = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&unchanged, &draft);
    let saved = "/board/test".parse().unwrap();
    assert_eq!(heads.statement.body.topic.path, Some(saved));
    assert!(heads.workflow.body.is_draft());
    clean(&root, bus);
}
