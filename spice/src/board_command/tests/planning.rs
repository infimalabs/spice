use super::*;

fn add_planning(
    bus: &Bus,
    epoch: CanonicalUuid,
    name: &str,
    intent: wire::TaskIntent,
    flow: Option<&str>,
) -> TaskHandle {
    add_planning_with(
        bus,
        epoch,
        name,
        intent,
        flow,
        &["The declared planning deliverable is supported"],
    )
}

fn add_planning_with(
    bus: &Bus,
    epoch: CanonicalUuid,
    name: &str,
    intent: wire::TaskIntent,
    flow: Option<&str>,
    term: &[&str],
) -> TaskHandle {
    let born = stamp(name);
    let task = TaskHandle::for_topic(
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
            topic: Some("/board/test".parse().expect("fixture topic")),
            term: term.iter().map(|item| (*item).to_owned()).collect(),
            origin: Some(fixture_origin()),
            flow: flow.map(str::to_owned),
            intent: Some(intent),
            ..adding(name.to_owned(), "This work keeps the fixture honest.")
        },
    ));
    task
}

fn done(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, task: &TaskHandle) -> Outcome<CommandAnswer> {
    claim_task(bus, epoch, seat, &Actor::Lane(seat.lane), task);
    complete(bus, epoch, seat, task)
}

/// Complete `task`'s current phase under the claim `seat` already holds.
fn complete(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    task: &TaskHandle,
) -> Outcome<CommandAnswer> {
    let lane = Actor::Lane(seat.lane);
    let held = match issue(
        bus,
        epoch,
        lane.clone(),
        CommandKind::BoardRead,
        Vec::new(),
        &BoardRead::Tasks {
            tasks: vec![task.born().expect("task identity")],
            dependencies: true,
            claims: false,
            history: false,
        },
    ) {
        Outcome::Reply(Reply {
            body: CommandAnswer::Board(answer),
            ..
        }) => *answer,
        outcome => panic!("completion read failed: {outcome:?}"),
    };
    let expected = progress_witnesses(&held, at(&held, task), seat.lane, seat.occupant, false)
        .expect("completion witnesses");
    issue(
        bus,
        epoch,
        lane,
        CommandKind::TaskDone,
        expected,
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(task, "Planning evidence retained")
        },
    )
}

/// Record a clean verdict on `task` under the review claim `seat` holds.
fn record_clean(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, task: &TaskHandle, note: &str) {
    let lane = Actor::Lane(seat.lane);
    let assigned = board(bus, epoch, lane.clone());
    let expected = progress_witnesses(
        &assigned,
        at(&assigned, task),
        seat.lane,
        seat.occupant,
        true,
    )
    .expect("re-review witnesses");
    assert_committed(issue(
        bus,
        epoch,
        lane,
        CommandKind::TaskReview,
        expected,
        &TaskReview {
            note: Some(note.to_owned()),
            ..verdict(task, "clean")
        },
    ));
}

#[test]
fn declared_planning_defaults_give_each_purpose_its_flow() {
    let (root, bus, epoch, _) = fixture("planning-defaults", 1);
    let question = &["Which boundary owns the contract?"][..];
    for (name, intent, questions, flow) in [
        (
            "Plan contracts",
            wire::TaskIntent::Plan,
            question,
            &["draft", "plan", "review"][..],
        ),
        (
            "Design assembly",
            wire::TaskIntent::Design,
            question,
            &["draft", "design"][..],
        ),
        // A Design without an open question has no acceptance: it rests
        // submitted in draft on its direct route rather than gaining a plan.
        (
            "Design unframed",
            wire::TaskIntent::Design,
            &[][..],
            &["draft", "design"][..],
        ),
    ] {
        let task = add_planning_with(&bus, epoch, name, intent, None, questions);
        let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
        let task = at(&view, &task);
        assert_eq!(task.statement.body.intent, Some(intent));
        assert_eq!(
            task.workflow
                .body
                .flow
                .iter()
                .map(|phase| phase.as_str())
                .collect::<Vec<_>>(),
            flow
        );
    }
    for flow in ["design", "plan,design", "review,design"] {
        let task = add_planning(
            &bus,
            epoch,
            &format!("Admitted Design flow {flow}"),
            wire::TaskIntent::Design,
            Some(flow),
        );
        let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
        assert_eq!(
            at(&view, &task).workflow.body.phase.as_str(),
            flow.split(',').next().expect("flow has a first phase"),
        );
    }
    for flow in ["design,review", "review", "plan", "todo", "plan,review"] {
        let born = stamp(&format!("Design flow {flow}"));
        let result = issue(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            CommandKind::TaskAdd,
            vec![Witness::Absence { task: born.clone() }],
            &TaskAdd {
                topic: Some("/board/test".parse().expect("fixture topic")),
                term: vec!["The design is supported".to_owned()],
                origin: Some(fixture_origin()),
                flow: Some(flow.to_owned()),
                intent: Some(wire::TaskIntent::Design),
                ..adding(
                    format!("Design flow {flow}"),
                    "This work keeps the fixture honest.",
                )
            },
        );
        assert!(
            matches!(&result, Outcome::Refusal(refusal) if refusal.reason() == "task::design-flow"),
            "a Design with flow {flow} must refuse: {result:?}"
        );
    }
    clean(&root, bus);
}
#[test]
fn plan_intake_releases_its_claim_into_an_active_design() {
    let (root, bus, epoch, seats) = fixture("planning-design-plan-intake", 1);
    let design = add_planning(
        &bus,
        epoch,
        "Plan the Design statement",
        wire::TaskIntent::Design,
        Some("plan,design"),
    );
    let lane = Actor::Lane(seats[0].lane);
    assert_eq!(
        allocated(&bus, epoch, &lane).map(|(handle, _)| handle),
        Some(design.to_string()),
        "plan intake remains ordinary allocatable work",
    );
    let premature = stamp("Premature Design output");
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let parent = at(&view, &design);
    let refused = issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![
            Witness::Absence {
                task: premature.clone(),
            },
            statement_witness(parent),
            workflow_witness(parent),
        ],
        &TaskAdd {
            preparation: Some(design.clone()),
            topic: Some("/board/test".parse().expect("fixture topic")),
            term: vec!["The output is executable".to_owned()],
            origin: Some(fixture_origin()),
            flow: Some("todo".to_owned()),
            ..adding(
                "Premature Design output",
                "This work keeps the fixture honest.",
            )
        },
    );
    assert!(
        matches!(&refused, Outcome::Refusal(refusal)
            if refusal.reason() == "task::preparation-refused"),
        "a Design intake cannot control an output: {refused:?}",
    );
    assert!(
        board(&bus, epoch, Actor::TEST_PRINCIPAL)
            .tasks
            .iter()
            .all(|task| task.statement.body.born != premature),
        "refused pre-Design preparation wrote no task",
    );
    assert_committed(done(&bus, epoch, &seats[0], &design));

    assert_eq!(
        allocated(&bus, epoch, &lane).map(|(handle, _)| handle),
        None,
        "the transitioned Design gathers input outside allocation",
    );
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&view, &design);
    assert_eq!(heads.workflow.body.phase.as_str(), "design");
    assert!(heads.workflow.body.status.is_pending());
    assert_eq!(
        heads.latest_claim().map(|held| held.claim.body.disposition),
        Some(ClaimDisposition::Freed),
    );
    assert_eq!(
        heads.design_standing(),
        Some(wire::intent::DesignStanding::Open),
    );
    bus.verify()
        .expect("transitioned Design authority verifies before restart");
    bus.shutdown().expect("planning bus stops");
    let reopened =
        journal::Journal::open(root.join("authority.sqlite3")).expect("planning journal reopens");
    let reopened = Bus::start(reopened).expect("planning bus restarts");
    let restarted = board(&reopened, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(
        at(&restarted, &design).design_standing(),
        Some(wire::intent::DesignStanding::Open),
    );
    clean(&root, reopened);
}

/// A plan intake's claimant sharpens the Design statement through ordinary
/// `TaskEdit` authority before advancing; an edit witnessing the head it drew
/// from after a concurrent edit landed refuses and keeps that edit.
#[test]
fn a_plan_intake_claimant_revises_the_statement_and_a_stale_head_refuses() {
    let (root, bus, epoch, seats) = fixture("planning-design-intake-edit", 1);
    let design = add_planning(
        &bus,
        epoch,
        "Plan the Design statement",
        wire::TaskIntent::Design,
        Some("plan,design"),
    );
    let lane = Actor::Lane(seats[0].lane);
    claim_task(&bus, epoch, &seats[0], &lane, &design);
    let drawn = statement_witness(at(&board(&bus, epoch, lane.clone()), &design));
    assert_committed(super::edit::correct(
        &bus,
        epoch,
        lane.clone(),
        &TaskEdit {
            title: Some("Sharpened Design statement".to_owned()),
            description: Some("Who needs this decision, and why now.".to_owned()),
            ..only(&design)
        },
    ));
    let stale = issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::TaskEdit,
        vec![drawn],
        &TaskEdit {
            description: Some("A stale rewrite".to_owned()),
            ..only(&design)
        },
    );
    assert!(
        matches!(&stale, Outcome::Refusal(refusal) if refusal.reason() == "board::witness-moved"),
        "an edit drawn from a moved head refuses: {stale:?}",
    );
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&view, &design);
    assert_eq!(heads.presentation_title(), "Sharpened Design statement");
    assert_eq!(
        heads.statement.body.description.as_deref(),
        Some("Who needs this decision, and why now."),
    );
    assert_eq!(heads.workflow.body.phase.as_str(), "plan");
    assert_eq!(
        heads.latest_claim().map(|held| held.claim.body.disposition),
        Some(ClaimDisposition::Active),
        "refining the statement keeps the intake claim",
    );
    assert_committed(super::edit::correct(
        &bus,
        epoch,
        lane.clone(),
        &TaskEdit {
            description: Some(String::new()),
            ..only(&design)
        },
    ));
    assert_eq!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &design)
            .statement
            .body
            .description,
        None,
        "an empty replacement clears the description",
    );
    assert_committed(complete(&bus, epoch, &seats[0], &design));
    let heads = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &design).clone();
    assert_eq!(heads.presentation_title(), "Sharpened Design statement");
    assert_eq!(
        heads.design_standing(),
        Some(wire::intent::DesignStanding::Open),
    );
    clean(&root, bus);
}

#[test]
fn clean_review_intake_releases_its_claim_into_an_active_design() {
    let (root, bus, epoch, seats) = fixture("planning-design-review-intake", 1);
    let seat = &seats[0];
    let actor = Actor::Lane(seat.lane);
    let design = add_planning(
        &bus,
        epoch,
        "Review the Design statement",
        wire::TaskIntent::Design,
        Some("review,design"),
    );
    claim_task(&bus, epoch, seat, &actor, &design);
    let view = board(&bus, epoch, actor.clone());
    assert_committed(issue(
        &bus,
        epoch,
        actor,
        CommandKind::TaskReview,
        progress_witnesses(&view, at(&view, &design), seat.lane, seat.occupant, true)
            .expect("review intake witnesses"),
        &verdict(&design, "clean"),
    ));

    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(
        latest_review_status(&bus, &design),
        wire::body::TaskCompletionStatus::Pending,
        "a clean intake review advances the Design and settles nothing"
    );
    let design = at(&view, &design);
    assert_eq!(design.workflow.body.phase.as_str(), "design");
    assert!(matches!(
        design.workflow.body.status,
        wire::WorkflowStatus::Pending(wire::WorkflowReview::Clean)
    ));
    assert_eq!(
        design
            .latest_claim()
            .map(|held| held.claim.body.disposition),
        Some(ClaimDisposition::Freed),
    );
    assert_eq!(
        design.design_standing(),
        Some(wire::intent::DesignStanding::Open),
    );
    clean(&root, bus);
}

#[test]
fn finding_review_intake_enters_design_with_its_correction_recorded() {
    let (root, bus, epoch, seats) = fixture("planning-design-review-finding", 2);
    let seat = &seats[0];
    let actor = Actor::Lane(seat.lane);
    authorize_lane_creation(&bus, epoch, seat);
    let design = add_planning(
        &bus,
        epoch,
        "Review a Design that needs correction",
        wire::TaskIntent::Design,
        Some("review,design"),
    );
    claim_task(&bus, epoch, seat, &actor, &design);
    let view = board(&bus, epoch, actor.clone());
    let correction = stamp("Correct the Design statement");
    let mut expected =
        progress_witnesses(&view, at(&view, &design), seat.lane, seat.occupant, true)
            .expect("finding intake witnesses");
    expected.push(Witness::Absence {
        task: correction.clone(),
    });
    assert_committed(issue(
        &bus,
        epoch,
        actor.clone(),
        CommandKind::TaskReview,
        expected,
        &TaskReview {
            task: design.clone(),
            finding: "The failure boundary is underspecified".to_owned(),
            note: None,
            then: vec![wire::intent::ReviewFollowup {
                task: correction.clone(),
                add: TaskAdd {
                    topic: Some("/board/test".parse().expect("fixture topic")),
                    term: vec!["The failure boundary is explicit".to_owned()],
                    ..adding(
                        "Correct the Design statement",
                        "This work keeps the fixture honest.",
                    )
                },
            }],
            followup: Vec::new(),
        },
    ));

    let finding_view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let finding_design = at(&finding_view, &design);
    assert_eq!(finding_design.workflow.body.phase.as_str(), "review");
    assert!(matches!(
        finding_design.workflow.body.status,
        wire::WorkflowStatus::Pending(wire::WorkflowReview::Unassigned)
    ));
    assert_eq!(
        finding_design
            .latest_claim()
            .map(|held| held.claim.body.disposition),
        Some(ClaimDisposition::Freed),
    );
    assert!(
        finding_design
            .statement
            .body
            .dependencies
            .contains(&correction),
        "the finding remains an explicit input until its correction completes",
    );
    assert_eq!(finding_design.design_standing(), None);

    // The finder hands the corrected revision's review to a peer first.
    let correction = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &correction,
    );
    claim_and_complete_task(&bus, epoch, seat, &actor, &correction);
    let peer = &seats[1];
    claim_task(&bus, epoch, peer, &Actor::Lane(peer.lane), &design);
    record_clean(&bus, epoch, peer, &design, "the correction is ready");
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let design = at(&view, &design);
    assert_eq!(design.workflow.body.phase.as_str(), "design");
    assert!(matches!(
        design.workflow.body.status,
        wire::WorkflowStatus::Pending(wire::WorkflowReview::Clean)
    ));
    assert_eq!(
        design
            .latest_claim()
            .map(|held| held.claim.body.disposition),
        Some(ClaimDisposition::Freed),
    );
    assert_eq!(
        design.design_standing(),
        Some(wire::intent::DesignStanding::Open),
    );
    clean(&root, bus);
}

#[test]
fn atomic_recovery_cannot_create_a_declared_planning_task() {
    let (root, bus, epoch, seats) = fixture("planning-recovery", 1);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    let lane = Actor::Lane(seats[0].lane);
    let born = stamp("Recovered implementation");
    let available = board(&bus, epoch, lane.clone());
    let recover: Name = "recover".parse().expect("recovery rule");
    let result = issue(
        &bus,
        epoch,
        lane,
        CommandKind::TaskAdd,
        vec![
            Witness::Absence { task: born },
            Witness::Eligibility { rule: recover },
            residency(&available, &seats[0]),
        ],
        &TaskAdd {
            topic: Some("/board/test".parse().expect("fixture topic")),
            term: vec!["The recovered behavior is implemented".to_owned()],
            origin: Some(fixture_origin()),
            intent: Some(wire::TaskIntent::Plan),
            claim: true,
            ..adding(
                "Recovered implementation",
                "This work keeps the fixture honest.",
            )
        },
    );
    assert!(
        matches!(
            &result,
            Outcome::Refusal(refusal) if refusal.reason() == "task::capture-planning"
        ),
        "planning recovery must refuse before repository recovery admission: {result:?}",
    );
    clean(&root, bus);
}

#[test]
fn claimed_wake_cannot_adopt_implementation_into_a_planning_deliverable() {
    let (root, bus, epoch, seats) = fixture("planning-wake-capture", 1);
    let seat = &seats[0];
    let lane = Actor::Lane(seat.lane);
    authorize_lane_creation(&bus, epoch, seat);
    let task = add_planning(&bus, epoch, "Woken plan", wire::TaskIntent::Plan, None);
    let before = board(&bus, epoch, lane.clone());
    let heads = at(&before, &task);
    let until = instant_after(current_instant().expect("clock"), 300).expect("future instant");
    assert_committed(issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::TaskDefer,
        vec![workflow_witness(heads), deferral_witness(heads)],
        &TaskDefer {
            task: task.clone(),
            wait: until,
        },
    ));
    let deferred = board(&bus, epoch, lane.clone());
    let heads = at(&deferred, &task);
    let wake = |rule: &str| {
        issue(
            &bus,
            epoch,
            lane.clone(),
            CommandKind::TaskWake,
            vec![
                workflow_witness(heads),
                deferral_witness(heads),
                Witness::Eligibility {
                    rule: rule.parse().expect("possession rule"),
                },
                residency(&deferred, seat),
            ],
            &TaskWake {
                task: task.clone(),
                claim: true,
            },
        )
    };
    let facts = || {
        bus.transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("facts read")
        .facts()
        .len()
    };
    let written = facts();
    for rule in [CAPTURE_WORK_RULE, "recover"] {
        let refused = wake(rule);
        assert!(
            matches!(
                &refused,
                Outcome::Refusal(refusal) if refusal.reason() == "task::capture-planning"
            ),
            "{rule} wake must not adopt implementation into a plan: {refused:?}",
        );
    }
    assert_eq!(facts(), written, "a refused wake writes nothing");

    let CommandAnswer::Task(woken) = assert_committed(wake("capture")) else {
        panic!("wake answers with a task status");
    };
    assert_eq!(
        woken.claim.and_then(|claim| claim.holder),
        Some(lane),
        "an ordinary claimed wake still holds the plan",
    );
    clean(&root, bus);
}

#[test]
fn a_plan_requires_constructed_output_not_only_its_own_acceptance() {
    let (root, bus, epoch, seats) = fixture("planning-construction", 3);
    let unlinked = add_planning(
        &bus,
        epoch,
        "Construct unlinked contracts",
        wire::TaskIntent::Plan,
        Some("plan,review"),
    );
    let result = done(&bus, epoch, &seats[0], &unlinked);
    let Outcome::Refusal(refusal) = result else {
        panic!("a Plan's own acceptance must not stand in for linked output: {result:?}");
    };
    assert_eq!(refusal.reason(), "task::plan-output-missing");
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let held = at(&view, &unlinked);
    assert_eq!(held.workflow.body.phase.as_str(), "plan");
    assert!(
        held.latest_claim()
            .map(|held| &held.claim)
            .is_some_and(|claim| {
                claim.body.disposition == ClaimDisposition::Active
                    && claim.body.actor == Actor::Lane(seats[0].lane)
            })
    );

    // Construction, rather than an execution prerequisite, supplies the Plan's output.
    let constructing = add_planning(
        &bus,
        epoch,
        "Construct later work",
        wire::TaskIntent::Plan,
        Some("plan"),
    );
    let planner = Actor::Lane(seats[2].lane);
    authorize_lane_creation(&bus, epoch, &seats[2]);
    claim_task(&bus, epoch, &seats[2], &planner, &constructing);
    add_associated(
        &bus,
        epoch,
        planner,
        "Implement the constructed contract",
        Some(&constructing),
        None,
    );
    assert_committed(complete(&bus, epoch, &seats[2], &constructing));
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&view, &constructing).workflow.body.status.is_completed());
    clean(&root, bus);
}

#[test]
fn a_plan_phase_of_ordinary_work_completes_on_its_own_acceptance() {
    let (root, bus, epoch, seats) = fixture("planning-bookend", 1);
    let task = add_task_with_term(
        &bus,
        epoch,
        "ORDINARY-planned",
        "Plan then build the contract",
        "/board/test",
        Some("plan,todo,review"),
        &["The planned contract holds"],
    );
    assert_committed(done(&bus, epoch, &seats[0], &task));
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&view, &task).workflow.body.phase.as_str(), "todo");
    clean(&root, bus);
}

#[test]
fn ordinary_work_completes_on_its_own_or_a_prerequisites_acceptance() {
    let (root, bus, epoch, seats) = fixture("ordinary-acceptance", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add_task_with_term(
        &bus,
        epoch,
        "ORDINARY-unspec",
        "Unspecified work",
        "/board/test",
        Some("todo,review"),
        &[],
    );
    let result = done(&bus, epoch, &seats[0], &task);
    let Outcome::Refusal(refusal) = result else {
        panic!("ordinary work without acceptance must refuse: {result:?}");
    };
    assert_eq!(refusal.reason(), "task::term-missing");
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let held = at(&view, &task);
    assert_eq!(held.workflow.body.phase.as_str(), "draft");
    assert!(
        held.latest_claim()
            .map(|held| &held.claim)
            .is_some_and(|claim| {
                claim.body.disposition == ClaimDisposition::Active && claim.body.actor == lane
            })
    );

    let prerequisite = add_task_with_term(
        &bus,
        epoch,
        "ORDINARY-prereq",
        "Accepted prerequisite",
        "/board/test",
        Some("todo"),
        &["the prerequisite behavior is verified"],
    );
    connect_child(&bus, epoch, &task, &prerequisite);
    // The prerequisite's acceptance lets the draft leave for todo, and the
    // same linked contract then completes that phase.
    assert_committed(complete(&bus, epoch, &seats[0], &task));
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&view, &task).workflow.body.phase.as_str(), "todo");
    assert_committed(complete(&bus, epoch, &seats[0], &task));
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&view, &task).workflow.body.phase.as_str(), "review");
    clean(&root, bus);
}

/// Complete `plan`'s plan phase with `disposition`, witnessing `cover` when a
/// task answers the need.
fn dispose(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    plan: &TaskHandle,
    disposition: wire::body::PlanDisposition,
    cover: Option<&TaskHandle>,
) -> Outcome<CommandAnswer> {
    let lane = Actor::Lane(seat.lane);
    let held = board(bus, epoch, lane.clone());
    let mut expected = progress_witnesses(&held, at(&held, plan), seat.lane, seat.occupant, false)
        .expect("disposition witnesses");
    expected.extend(cover.map(|cover| statement_witness(at(&held, cover))));
    issue(
        bus,
        epoch,
        lane,
        CommandKind::TaskDone,
        expected,
        &TaskDone {
            task: plan.clone(),
            validation: "The landed contract already answers this need".to_owned(),
            disposition: Some(disposition),
            operation: Some(completion_operation()),
        },
    )
}

#[test]
fn an_ordinary_disposition_refusal_names_claimed_task_deletion() {
    let (root, bus, epoch, seats) = fixture("ordinary-disposition-refusal", 1);
    let seat = &seats[0];
    let lane = Actor::Lane(seat.lane);
    let task = add_task_with_term(
        &bus,
        epoch,
        "TASK-duplicate",
        "Remove duplicate work",
        "/board/test",
        Some("todo"),
        &["The duplicate is removed"],
    );
    claim_task(&bus, epoch, seat, &lane, &task);

    let result = dispose(
        &bus,
        epoch,
        seat,
        &task,
        wire::body::PlanDisposition::NoWork {},
        None,
    );
    let Outcome::Refusal(refusal) = result else {
        panic!("ordinary disposition must refuse: {result:?}");
    };
    assert_eq!(refusal.reason(), "task::disposition-refused");
    let detail = refusal
        .diagnostic()
        .and_then(wire::RefusalDiagnostic::detail)
        .expect("actionable disposition refusal");
    assert!(detail.contains("spice task delete HANDLE REASON --force-claimed"));
    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&current, &task).workflow.body.phase.as_str(), "todo");
    assert!(at(&current, &task).workflow.body.status.is_pending());
    clean(&root, bus);
}

#[test]
fn an_intentless_plan_flow_still_records_a_disposition() {
    let (root, bus, epoch, seats) = fixture("intentless-plan-disposition", 1);
    let seat = &seats[0];
    let lane = Actor::Lane(seat.lane);
    let plan = add_task_with_term(
        &bus,
        epoch,
        "PLAN-flow",
        "Plan by flow alone",
        "/board/test",
        Some("plan,review"),
        &["The need is planned"],
    );
    claim_task(&bus, epoch, seat, &lane, &plan);

    assert_committed(dispose(
        &bus,
        epoch,
        seat,
        &plan,
        wire::body::PlanDisposition::NoWork {},
        None,
    ));
    let disposed = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&disposed, &plan).workflow.body.phase.as_str(), "review");
    clean(&root, bus);
}

#[test]
fn a_disposed_plan_settles_on_clean_review_without_constructing_or_authorizing_work() {
    let (root, bus, epoch, seats) = fixture("planning-disposition", 1);
    let seat = &seats[0];
    let lane = Actor::Lane(seat.lane);
    let cover = add_task_with_term(
        &bus,
        epoch,
        "COVER-landed",
        "Answer the reported need",
        "/board/test",
        Some("todo"),
        &["The reported need is answered"],
    );
    claim_and_complete_task(&bus, epoch, seat, &lane, &cover);
    let covering = || wire::body::DispositionCover::Task(cover.born().expect("cover stamp"));
    let commit = || wire::body::DispositionCover::Commit("a".repeat(40));
    for (name, disposition, witness) in [
        (
            "Dispose no work",
            wire::body::PlanDisposition::NoWork {},
            None,
        ),
        (
            "Dispose duplicate",
            wire::body::PlanDisposition::Duplicate { cover: covering() },
            Some(&cover),
        ),
        (
            "Dispose covered",
            wire::body::PlanDisposition::Covered { cover: commit() },
            None,
        ),
        (
            "Dispose deferred",
            wire::body::PlanDisposition::Deferred { cover: covering() },
            Some(&cover),
        ),
    ] {
        let plan = add_planning(
            &bus,
            epoch,
            name,
            wire::TaskIntent::Plan,
            Some("plan,review"),
        );
        let before = board(&bus, epoch, Actor::TEST_PRINCIPAL);
        claim_task(&bus, epoch, seat, &lane, &plan);
        assert_committed(dispose(&bus, epoch, seat, &plan, disposition, witness));
        let disposed = board(&bus, epoch, Actor::TEST_PRINCIPAL);
        assert_eq!(at(&disposed, &plan).workflow.body.phase.as_str(), "review");
        assert!(at(&disposed, &plan).workflow.body.status.is_pending());

        claim_task(&bus, epoch, seat, &lane, &plan);
        assert_committed(super::preparation::review_clean(&bus, epoch, seat, &plan));
        let settled = board(&bus, epoch, Actor::TEST_PRINCIPAL);
        assert!(at(&settled, &plan).workflow.body.status.is_completed());
        // Nothing was constructed, released, or moved beside the Plan itself.
        assert_eq!(settled.tasks.len(), before.tasks.len(), "{name}");
        assert_eq!(
            at(&settled, &cover).workflow.position,
            at(&before, &cover).workflow.position,
            "{name}"
        );
    }
    clean(&root, bus);
}

#[test]
fn a_plan_disposition_refuses_beside_a_live_output_or_an_unwitnessed_cover() {
    let (root, bus, epoch, seats) = fixture("planning-disposition-refusals", 1);
    let seat = &seats[0];
    let lane = Actor::Lane(seat.lane);
    let plan = add_planning(
        &bus,
        epoch,
        "Dispose refused",
        wire::TaskIntent::Plan,
        Some("plan,review"),
    );
    claim_task(&bus, epoch, seat, &lane, &plan);
    let cover = add_associated(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "Unwitnessed cover",
        None,
        None,
    );
    let unwitnessed = dispose(
        &bus,
        epoch,
        seat,
        &plan,
        wire::body::PlanDisposition::Duplicate {
            cover: wire::body::DispositionCover::Task(cover.born().expect("cover stamp")),
        },
        None,
    );
    assert!(
        matches!(&unwitnessed, Outcome::Refusal(refusal)
            if refusal.reason() == "task::disposition-cover-invalid"),
        "{unwitnessed:?}"
    );

    add_associated(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "Constructed live output",
        Some(&plan),
        None,
    );
    let beside = dispose(
        &bus,
        epoch,
        seat,
        &plan,
        wire::body::PlanDisposition::NoWork {},
        None,
    );
    assert!(
        matches!(&beside, Outcome::Refusal(refusal)
            if refusal.reason() == "task::disposition-beside-output"),
        "{beside:?}"
    );
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&view, &plan).workflow.body.phase.as_str(), "plan");
    clean(&root, bus);
}

#[test]
fn operator_closed_work_is_no_plan_output() {
    let (root, bus, epoch, seats) = fixture("planning-closed-construction", 1);
    let plan = add_planning_with(
        &bus,
        epoch,
        "Construct retained contracts",
        wire::TaskIntent::Plan,
        Some("plan,review"),
        &["Which contracts are retained?"],
    );
    let child = add_task_with_term(
        &bus,
        epoch,
        "CONTRACT-negative",
        "Investigate one possible contract",
        "/board/test",
        Some("todo"),
        &["The possible contract is justified"],
    );
    connect_child(&bus, epoch, &plan, &child);
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&view, &child);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskClose,
        transition_witnesses(heads),
        &TaskClose {
            task: child,
            validation: "Investigation rejected this contract".to_owned(),
            force_claimed: false,
        },
    ));

    let result = done(&bus, epoch, &seats[0], &plan);
    let Outcome::Refusal(refusal) = result else {
        panic!("operator-closed work must not count as Plan output: {result:?}");
    };
    assert_eq!(refusal.reason(), "task::plan-output-missing");
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&view, &plan).workflow.body.phase.as_str(), "plan");
    clean(&root, bus);
}

#[test]
fn ordinary_done_cannot_supply_a_required_review_verdict() {
    let (root, bus, epoch, seats) = fixture("planning-review-verdict", 1);
    let task = add_task_with_term(
        &bus,
        epoch,
        "REVIEW-verdict",
        "Review the declared deliverable",
        "/board/test",
        Some("review"),
        &["The review judges the deliverable"],
    );
    let result = done(&bus, epoch, &seats[0], &task);
    let Outcome::Refusal(refusal) = result else {
        panic!("ordinary completion cannot supply a verdict: {result:?}");
    };
    assert_eq!(refusal.reason(), "task::review-required");
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&view, &task).workflow.body.status.is_pending());
    clean(&root, bus);
}

fn add_associated(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    name: &str,
    parent: Option<&TaskHandle>,
    intent: Option<wire::TaskIntent>,
) -> TaskHandle {
    let born = stamp(name);
    let task = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &born,
    );
    let add = TaskAdd {
        preparation: parent.cloned(),
        intent,
        topic: Some("/board/test".parse().expect("fixture topic")),
        term: vec!["The executable contract supports the requested behavior".to_owned()],
        origin: if actor == Actor::TEST_PRINCIPAL {
            Some(fixture_origin())
        } else {
            None
        },
        ..adding(name.to_owned(), "This work keeps the fixture honest.")
    };
    let view = board(bus, epoch, actor.clone());
    let mut expected = vec![Witness::Absence { task: born }];
    append_preparation_witnesses(&view, &add, &mut expected).expect("preparation witnesses");
    assert_committed(issue(
        bus,
        epoch,
        actor,
        CommandKind::TaskAdd,
        expected,
        &add,
    ));
    task
}

#[test]
fn negative_controller_closure_closes_only_controlled_drafts() {
    let (root, bus, epoch, _seats) = fixture("negative-controller", 1);
    let plan = add_planning(
        &bus,
        epoch,
        "Construct selected contract",
        wire::TaskIntent::Plan,
        None,
    );
    let output = add_associated(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "Controlled implementation",
        Some(&plan),
        None,
    );
    let ordinary = add_task_with_term(
        &bus,
        epoch,
        "TASK-ordinary-edge",
        "Unrelated follow-on",
        "/board/test",
        Some("todo"),
        &["The ordinary follow-on is supported"],
    );
    connect_child(&bus, epoch, &ordinary, &plan);
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&view, &plan);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskClose,
        transition_witnesses(heads),
        &TaskClose {
            task: plan,
            validation: "No executable output is selected".to_owned(),
            force_claimed: false,
        },
    ));
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&view, &output).workflow.body.status.is_completed());
    assert!(
        at(&view, &ordinary).workflow.body.status.is_pending(),
        "an execution prerequisite is not construction ownership"
    );
    clean(&root, bus);
}

#[test]
fn a_plan_correction_completes_only_through_its_own_linked_output() {
    let (root, bus, epoch, seats) = fixture("planning-correction-acceptance", 1);
    let actor = Actor::Lane(seats[0].lane);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    let plan = add_planning(
        &bus,
        epoch,
        "Construct executable contract",
        wire::TaskIntent::Plan,
        Some("review,todo"),
    );
    claim_task(&bus, epoch, &seats[0], &actor, &plan);
    let view = board(&bus, epoch, actor.clone());
    let correction_born = stamp("Construct missing output");
    let correction = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &correction_born,
    );
    let mut expected = progress_witnesses(
        &view,
        at(&view, &plan),
        seats[0].lane,
        seats[0].occupant,
        true,
    )
    .expect("review witnesses");
    expected.push(Witness::Absence {
        task: correction_born.clone(),
    });
    assert_committed(issue(
        &bus,
        epoch,
        actor.clone(),
        CommandKind::TaskReview,
        expected,
        &TaskReview {
            task: plan.clone(),
            finding: "The executable contract is absent".to_owned(),
            note: None,
            followup: Vec::new(),
            then: vec![wire::intent::ReviewFollowup {
                task: correction_born,
                add: TaskAdd {
                    topic: Some("/board/test".parse().expect("fixture topic")),
                    term: vec!["the fixture work is verified".to_owned()],
                    ..adding(
                        "Construct missing output",
                        "This work keeps the fixture honest.",
                    )
                },
            }],
        },
    ));
    // The correction inherits the Plan's intent, so neither the Plan's
    // acceptance nor its own stands in for linked executable work.
    claim_task(&bus, epoch, &seats[0], &actor, &correction);
    let review = || {
        let view = board(&bus, epoch, actor.clone());
        issue(
            &bus,
            epoch,
            actor.clone(),
            CommandKind::TaskReview,
            progress_witnesses(
                &view,
                at(&view, &correction),
                seats[0].lane,
                seats[0].occupant,
                true,
            )
            .expect("correction witnesses"),
            &verdict(&correction, "clean"),
        )
    };
    let refused = review();
    assert!(
        matches!(&refused, Outcome::Refusal(refusal)
            if refusal.reason() == "task::plan-output-missing"),
        "{refused:?}"
    );
    add_associated(
        &bus,
        epoch,
        actor.clone(),
        "Implement the corrected contract",
        Some(&correction),
        None,
    );
    let reply = assert_committed(review());
    let CommandAnswer::Task(answer) = reply else {
        panic!("review returns task state");
    };
    assert_eq!(answer.status, "pending");
    let view = board(&bus, epoch, actor);
    assert_eq!(at(&view, &correction).workflow.body.phase.as_str(), "todo");
    assert!(at(&view, &plan).workflow.body.status.is_pending());
    clean(&root, bus);
}
