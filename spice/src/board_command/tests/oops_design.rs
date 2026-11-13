use super::*;

use super::preparation::{decide, end_pass, select_outputs};
use wire::rooms::DecisionOutcome;

fn task(micros: i64) -> TaskStamp {
    TaskStamp::for_micros(micros).expect("fixture task stamp")
}

fn add_line(
    bus: &Bus,
    epoch: CanonicalUuid,
    design: &TaskStamp,
    report: TaskStamp,
    text: &str,
) -> TaskHandle {
    let current = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let generation = current.oops_generation.as_ref();
    let mut expected = vec![
        Witness::Absence {
            task: report.clone(),
        },
        Witness::OopsGeneration {
            head: generation.map(|head| head.position),
            design: generation.and_then(|head| head.body.design.clone()),
        },
    ];
    if let Some(heads) = current
        .tasks
        .iter()
        .find(|heads| &heads.statement.body.born == design)
    {
        expected.push(statement_witness(heads));
        expected.push(workflow_witness(heads));
    } else {
        expected.push(Witness::Absence {
            task: design.clone(),
        });
    }
    let CommandAnswer::Task(answer) = assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskOopsAdd,
        expected,
        &TaskOopsAdd {
            design: design.clone(),
            title: text.to_owned(),
            origin: Some(fixture_origin()),
            topic: Some("/board/test".parse().expect("fixture topic")),
        },
    )) else {
        panic!("OOPS intake returned another answer shape");
    };
    let view = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let created = at(&view, &answer.task);
    assert_eq!(
        created.presentation_title(),
        text,
        "the report text is the title"
    );
    assert_eq!(
        created.statement.body.description, None,
        "the report text is not repeated as a description"
    );
    assert!(
        created.statement.body.term.is_empty(),
        "intake injects no acceptance"
    );
    answer.task
}

#[test]
fn concurrent_distinct_first_reports_converge_on_one_oops_design() {
    let (root, bus, epoch, _seats) = fixture("oops-design-first-race", 0);
    let first_design = task(1_760_000_000_100_000);
    let second_design = task(1_760_000_000_100_001);
    let first_report = task(1_760_000_000_100_002);
    let second_report = task(1_760_000_000_100_003);
    let expected = |design: TaskStamp, report: TaskStamp| {
        vec![
            Witness::Absence { task: report },
            Witness::Absence { task: design },
            Witness::OopsGeneration {
                head: None,
                design: None,
            },
        ]
    };
    let add = |design: TaskStamp, text: &str| TaskOopsAdd {
        design,
        title: text.to_owned(),
        origin: Some(fixture_origin()),
        topic: Some("/board/test".parse().expect("fixture topic")),
    };
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskOopsAdd,
        expected(first_design.clone(), first_report),
        &add(first_design.clone(), "First concurrent report"),
    ));
    let stale = issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskOopsAdd,
        expected(second_design.clone(), second_report.clone()),
        &add(second_design, "Second concurrent report"),
    );
    assert!(
        matches!(&stale, Outcome::Refusal(refusal) if refusal.reason() == "board::witness-moved"),
        "{stale:?}"
    );
    add_line(
        &bus,
        epoch,
        &first_design,
        second_report,
        "Second concurrent report",
    );
    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(
        current
            .tasks
            .iter()
            .filter(|heads| {
                heads.statement.body.intent == Some(wire::TaskIntent::OpportunisticDesign)
                    && heads.workflow.body.status.is_pending()
            })
            .count(),
        1
    );
    assert_eq!(
        current
            .tasks
            .iter()
            .filter(|heads| {
                heads.statement.body.intent == Some(wire::TaskIntent::Plan)
                    && heads
                        .statement
                        .body
                        .preparation
                        .as_ref()
                        .is_some_and(|preparation| preparation.controller == first_design)
            })
            .count(),
        2
    );
    clean(&root, bus);
}

#[test]
fn unrelated_board_churn_does_not_refuse_oops_intake() {
    let (root, bus, epoch, _seats) = fixture("oops-design-unrelated-churn", 0);
    let design = task(1_760_000_000_150_000);
    let report = task(1_760_000_000_150_001);
    let expected = vec![
        Witness::Absence {
            task: report.clone(),
        },
        Witness::Absence {
            task: design.clone(),
        },
        Witness::OopsGeneration {
            head: None,
            design: None,
        },
    ];
    add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "1B6QBLpz",
        "Unrelated board mutation",
        Some("todo"),
    );
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskOopsAdd,
        expected,
        &TaskOopsAdd {
            design,
            title: "Report after unrelated churn".to_owned(),
            origin: Some(fixture_origin()),
            topic: Some("/board/test".parse().expect("fixture topic")),
        },
    ));
    clean(&root, bus);
}

fn collapse_and_open_next_generation(bus: &Bus, epoch: CanonicalUuid, design: TaskHandle) {
    let before = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let design_heads = at(&before, &design);
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskConclude,
        transition_witnesses(design_heads),
        &TaskConclude {
            task: design,
            conclusion: body::DesignConclusion::Collapsed {
                reason: "remaining line deferred to a later generation".to_owned(),
            },
        },
    ));

    let next_design = task(1_760_000_000_000_010);
    let next = add_line(
        bus,
        epoch,
        &next_design,
        task(1_760_000_000_000_011),
        "Next-generation friction",
    );
    let final_board = board(bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(
        final_board
            .tasks
            .iter()
            .filter(|heads| {
                heads.statement.body.intent == Some(wire::TaskIntent::OpportunisticDesign)
                    && heads.workflow.body.status.is_pending()
            })
            .count(),
        1
    );
    assert_eq!(
        at(&final_board, &next)
            .statement
            .body
            .preparation
            .as_ref()
            .map(|association| association.parent.clone()),
        Some(next_design)
    );
}

/// Reports gather under one Design: deciding it refuses until a holder's pass
/// ends, and that pass offers exactly the reports given acceptance.
#[test]
fn oops_reports_wait_for_their_holders_pass() {
    let (root, bus, epoch, seats) = fixture("oops-design-lines", 1);
    let lane = Actor::Lane(seats[0].lane);
    let design_stamp = task(1_760_000_000_000_000);
    let first_stamp = task(1_760_000_000_000_001);
    let second_stamp = task(1_760_000_000_000_002);
    let first = add_line(
        &bus,
        epoch,
        &design_stamp,
        first_stamp.clone(),
        "First friction",
    );
    let second = add_line(
        &bus,
        epoch,
        &design_stamp,
        second_stamp.clone(),
        "Second friction",
    );

    let preview = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let design = TaskHandle::for_topic(
        &wire::OOPS_DESIGN_TOPIC
            .parse::<wire::Topic>()
            .expect("OOPS topic"),
        &design_stamp,
    );
    let design_heads = at(&preview, &design);
    assert_eq!(
        design_heads.statement.body.intent,
        Some(wire::TaskIntent::OpportunisticDesign)
    );
    for line in [&first, &second] {
        let heads = at(&preview, line);
        assert_eq!(heads.statement.body.intent, Some(wire::TaskIntent::Plan));
        let association = heads
            .statement
            .body
            .preparation
            .as_ref()
            .expect("OOPS line is controlled");
        assert_eq!(association.parent, design_stamp);
        assert_eq!(association.controller, design_stamp);
        assert_eq!(association.construction, heads.statement.body.born);
    }

    // No pass has ended, so the operator cannot decide the pending reports.
    let facts = fact_count(&bus);
    let refused = decide(
        &bus,
        epoch,
        &design,
        1,
        DecisionOutcome::Returned {
            reason: "triage first".to_owned(),
        },
    );
    assert!(
        matches!(&refused, Outcome::Refusal(refusal) if refusal.reason() == "preparation::pass-open"),
        "{refused:?}"
    );
    assert_eq!(fact_count(&bus), facts);

    // Its statement text amends like any Design's; its intent does not.
    assert_committed(super::edit::correct(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &TaskEdit {
            description: Some("Triage these reports as one cluster.".to_owned()),
            ..only(&design)
        },
    ));
    let retyped = super::edit::correct(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &TaskEdit {
            intent: Some(wire::TaskIntentEdit::Clear {}),
            ..only(&design)
        },
    );
    assert!(
        matches!(&retyped, Outcome::Refusal(refusal) if refusal.reason() == "task::oops-design-internal"),
        "{retyped:?}"
    );

    // Moved to an active group, it is claimed and worked like any Design.
    capture_design(&bus, epoch, &seats[0], &design);
    let accept = |report: &TaskHandle| {
        assert_committed(super::edit::correct(
            &bus,
            epoch,
            lane.clone(),
            &TaskEdit {
                term: Some(vec![format!("{report} names its fix.")]),
                ..only(report)
            },
        ));
    };
    accept(&first);
    assert_committed(end_pass(&bus, epoch, &seats[0], &design));
    // A report without acceptance is not yet an output: the decision offers
    // and releases only the accepted one.
    assert_committed(select_outputs(&bus, epoch, &design, &[&first], 1));
    let decided = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&decided, &first).workflow.body.is_activated());
    let unaccepted = &at(&decided, &second).workflow.body;
    assert!(unaccepted.is_draft() && unaccepted.status.is_pending());

    collapse_and_open_next_generation(&bus, epoch, design);
    clean(&root, bus);
}
