use super::*;

fn progress_read(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    tasks: Vec<TaskStamp>,
    claims: bool,
) -> BoardAnswer {
    match issue(
        bus,
        epoch,
        actor,
        CommandKind::BoardRead,
        Vec::new(),
        &BoardRead::Tasks {
            tasks,
            dependencies: true,
            claims,
            history: false,
        },
    ) {
        Outcome::Reply(Reply {
            body: CommandAnswer::Board(answer),
            ..
        }) => *answer,
        outcome => panic!("progress board read failed: {outcome:?}"),
    }
}

#[test]
fn plan_claim_allows_board_team_but_classifies_repository_work_as_forbidden() {
    let (root, bus, epoch, seats) = fixture("plan-team-contract", 1);
    let lane = Actor::Lane(seats[0].lane);
    let plan = add_raw_task(
        &bus,
        epoch,
        "PLAN-1kRKw9Qjx",
        "Organize implementation work",
        &["implementation is decomposed"],
        Some("plan,todo"),
    );
    claim_task(&bus, epoch, &seats[0], &lane, &plan);

    let held = board(&bus, epoch, lane.clone());
    assert_eq!(
        claim_capture_from_board(&held),
        gate::ClaimCapture::Active {
            task: plan.clone(),
            phase: "plan".parse().expect("fixture plan phase"),
        },
    );
    let Err(Outcome::Refusal(refusal)) = require_no_planning_implementation(&held, "task capture")
    else {
        panic!("plan claim must refuse repository capture");
    };
    assert_eq!(refusal.reason(), "task::planning-implementation");

    assert_committed(issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::TaskEdit,
        vec![statement_witness(at(&held, &plan))],
        &TaskEdit {
            description: Some("Sequence the implementation children".to_owned()),
            term: Some(vec!["the fixture work is verified".to_owned()]),
            ..only(&plan)
        },
    ));
    let edited = board(&bus, epoch, lane.clone());
    assert_committed(issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::TaskNote,
        vec![Witness::Existence {
            task: at(&edited, &plan).statement.body.born.clone(),
        }],
        &TaskNote {
            task: plan.clone(),
            note: "The implementation boundary is explicit".to_owned(),
        },
    ));
    let child = add_raw_task(
        &bus,
        epoch,
        "PLAN-1kRKw9Qkx",
        "Implement the organized change",
        &["repository behavior is verified"],
        None,
    );
    let held = board(&bus, epoch, lane.clone());
    let child_heads = at(&held, &child);
    let mut expected = progress_witnesses(
        &held,
        at(&held, &plan),
        seats[0].lane,
        seats[0].occupant,
        false,
    )
    .expect("claimed plan has dependency-edit witnesses");
    expected.extend([
        statement_witness(child_heads),
        workflow_witness(child_heads),
        Witness::Edge {
            task: at(&held, &plan).statement.body.born.clone(),
            after: child_heads.statement.body.born.clone(),
        },
    ]);
    assert_committed(issue(
        &bus,
        epoch,
        lane,
        CommandKind::TaskDepends,
        expected,
        &TaskDepends {
            task: plan.clone(),
            after: vec![child.clone()],
            remove: false,
        },
    ));

    let organized = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(
        at(&organized, &plan).statement.body.description.as_deref(),
        Some("Sequence the implementation children"),
    );
    assert_eq!(
        at(&organized, &plan).statement.body.dependencies,
        vec![at(&organized, &child).statement.body.born.clone()],
    );
    clean(&root, bus);
}

#[test]
fn design_claim_refuses_repository_capture() {
    let (root, bus, epoch, seats) = fixture("design-capture-contract", 1);
    let lane = Actor::Lane(seats[0].lane);
    let design = add_raw_task(
        &bus,
        epoch,
        "DESIGN-1kRKw9Qkx",
        "Justify the selected assembly",
        &["the assembly decision is supported"],
        Some("design,review"),
    );
    claim_task(&bus, epoch, &seats[0], &lane, &design);

    let held = board(&bus, epoch, lane);
    let Err(Outcome::Refusal(refusal)) = require_no_planning_implementation(&held, "task capture")
    else {
        panic!("design claim must refuse repository capture");
    };
    assert_eq!(refusal.reason(), "task::planning-implementation");
    clean(&root, bus);
}

#[test]
fn plan_advance_carries_its_claim_into_todo_over_competing_work() {
    let (root, bus, epoch, seats) = fixture("plan-advance-ownership", 1);
    let lane = Actor::Lane(seats[0].lane);
    let plan = add_raw_task(
        &bus,
        epoch,
        "PLAN-1kLGp016x",
        "Advance only the held plan phase",
        &["the held plan is decomposed"],
        Some("plan,todo,review"),
    );
    claim_task(&bus, epoch, &seats[0], &lane, &plan);
    prepare_plan_advance_competitor(&bus, epoch, &plan);
    // The operator edited the held plan, so the holder acknowledges it first.
    refresh_held(&bus, epoch, &lane, &seats[0], &plan);
    let (done, answer) = complete_plan_phase(&bus, epoch, &seats[0], &lane, &plan);
    let carried = answer
        .claim
        .as_ref()
        .expect("a plan advancing into todo carries its claim");
    assert_eq!(carried.task, plan);
    assert_eq!(carried.holder.as_ref(), Some(&lane));
    assert_eq!(
        carried.phase.as_ref().map(|phase| phase.name.as_str()),
        Some("todo")
    );
    let released =
        assert_plan_carry_and_replay(&bus, epoch, &seats[0], &lane, &plan, &done, &answer);
    let Some(NextWorkState::Active { task: active }) = next_board(&bus, epoch, lane).next else {
        panic!("the carrying lane's task next retains the carried claim")
    };
    assert_eq!(
        active.statement.body.born,
        at(&released, &plan).statement.body.born
    );
    clean(&root, bus);
}

#[test]
fn an_assignment_names_the_phase_it_landed_in() {
    let (root, bus, epoch, seats) = fixture("assignment-phase", 2);
    let plan = add_raw_task(
        &bus,
        epoch,
        "PLAN-1kRKw9Qlx",
        "Organize implementation work",
        &["implementation is decomposed"],
        Some("plan,todo"),
    );
    let ordinary = add_raw_task(
        &bus,
        epoch,
        "PLAN-1kRKw9Qmx",
        "Implement the organized change",
        &["repository behavior is verified"],
        None,
    );
    for (seat, task, phase) in [(0, &plan, "plan"), (1, &ordinary, "todo")] {
        let lane = Actor::Lane(seats[seat].lane);
        let available = board(&bus, epoch, lane.clone());
        let CommandAnswer::Claim(claimed) = assert_committed(issue(
            &bus,
            epoch,
            lane,
            CommandKind::ClaimWin,
            claim_expected(&available, task, &seats[seat]),
            &next_rule(),
        )) else {
            panic!("task next did not return a claim answer")
        };
        assert_eq!(&claimed.task, task);
        let assigned = claimed.phase.expect("an assignment names its phase");
        assert_eq!(assigned.name.as_str(), phase);
    }
    clean(&root, bus);
}

fn prepare_plan_advance_competitor(
    bus: &Bus,
    epoch: CanonicalUuid,
    plan: &TaskHandle,
) -> TaskHandle {
    let visible = board(bus, epoch, Actor::TEST_PRINCIPAL);
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        vec![statement_witness(at(&visible, plan))],
        &TaskEdit {
            term: Some(vec!["implementation has a checkable contract".to_owned()]),
            ..only(plan)
        },
    ));
    let sibling = add(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "PLAN-sibling01",
        "Competing eligible implementation",
        Some("todo,review"),
    );
    let visible = board(bus, epoch, Actor::TEST_PRINCIPAL);
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        vec![statement_witness(at(&visible, &sibling))],
        &TaskEdit {
            term: Some(vec!["the fixture work is verified".to_owned()]),
            priority: Some("critical".to_owned()),
            ..only(&sibling)
        },
    ));
    sibling
}

fn complete_plan_phase(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    lane: &Actor,
    plan: &TaskHandle,
) -> (TaskDone, wire::intent::TaskAnswer) {
    let held = board(bus, epoch, lane.clone());
    let done = TaskDone {
        operation: Some(completion_operation()),
        ..finished(plan, "plan acceptance is explicit")
    };
    let CommandAnswer::Task(answer) = assert_committed(issue(
        bus,
        epoch,
        lane.clone(),
        CommandKind::TaskDone,
        progress_witnesses(&held, at(&held, plan), seat.lane, seat.occupant, false)
            .expect("accepted plan has progress witnesses"),
        &done,
    )) else {
        panic!("plan completion did not return a task answer")
    };
    assert_eq!(answer.status, "pending");
    assert_eq!(
        answer.next,
        answer.claim.as_ref().and_then(|claim| claim.next.clone())
    );
    (done, *answer)
}

fn assert_plan_carry_and_replay(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    lane: &Actor,
    plan: &TaskHandle,
    done: &TaskDone,
    answer: &wire::intent::TaskAnswer,
) -> BoardAnswer {
    let released = board(bus, epoch, lane.clone());
    assert_workflow(
        &released,
        plan,
        "todo",
        &["draft", "plan", "todo", "review"],
    );
    let claim = &at(&released, plan)
        .latest_claim()
        .map(|held| &held.claim)
        .expect("advanced plan carries its claim head")
        .body;
    assert_eq!(claim.disposition, ClaimDisposition::Active);
    assert_eq!(&claim.actor, lane);
    let CommandAnswer::Task(replayed) = assert_committed(issue(
        bus,
        epoch,
        lane.clone(),
        CommandKind::TaskDone,
        progress_witnesses(
            &released,
            at(&released, plan),
            seat.lane,
            seat.occupant,
            false,
        )
        .expect("released plan retains replay witnesses"),
        done,
    )) else {
        panic!("plan completion replay did not return a task answer")
    };
    assert_eq!(&*replayed, answer);
    released
}

#[test]
fn selected_plan_reads_gate_completion_with_current_reverse_child_heads() {
    let (root, bus, epoch, seats) = fixture("gated-plan-child", 1);
    let lane = Actor::Lane(seats[0].lane);
    let plan = add_raw_task(
        &bus,
        epoch,
        "PLAN-1kRKw9Qnx",
        "Plan-only parent",
        &[],
        Some("plan"),
    );
    let child = add_raw_task(
        &bus,
        epoch,
        "PLAN-1kRKw9Qpx",
        "Accepted implementation",
        &["the implementation behavior is verified"],
        Some("todo"),
    );
    connect_child(&bus, epoch, &child, &plan);
    // The child's acceptance lets the holder advance the plan out of draft;
    // the claim carries into the plan phase.
    super::super::capture_for(&bus, epoch, &seats[0], &plan);
    complete_plan_phase(&bus, epoch, &seats[0], &lane, &plan);
    let unrelated = todo_task(&bus, epoch, "unrelated", "Unrelated work");

    let named = progress_read(
        &bus,
        epoch,
        lane.clone(),
        vec![plan.born().expect("plan task stamp")],
        false,
    );
    assert_eq!(named.tasks.len(), 2);
    assert_eq!(at(&named, &plan).workflow.body.phase.as_str(), "plan");
    assert!(at(&named, &child).workflow.body.status.is_pending());
    assert!(
        named
            .tasks
            .iter()
            .all(|heads| heads.statement.body.handle != unrelated),
        "a named plan read stays bounded to the plan and its accepted child"
    );

    let held = progress_read(&bus, epoch, lane.clone(), Vec::new(), true);
    assert_eq!(held.tasks.len(), 2);
    let child_heads = at(&held, &child);
    let stale_expected = progress_witnesses(
        &held,
        at(&held, &plan),
        seats[0].lane,
        seats[0].occupant,
        false,
    )
    .expect("ambient plan completion carries reverse child witnesses");
    assert!(stale_expected.contains(&statement_witness(child_heads)));
    assert!(stale_expected.contains(&workflow_witness(child_heads)));

    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        vec![statement_witness(at(&visible, &child))],
        &TaskEdit {
            term: Some(vec![
                "the changed implementation behavior is verified".to_owned(),
            ]),
            ..only(&child)
        },
    ));
    let done = TaskDone {
        operation: Some(completion_operation()),
        ..finished(&plan, "the accepted child is gated behind the plan")
    };
    let refused = issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::TaskDone,
        stale_expected,
        &done,
    );
    assert_refusal(refused, "task::term-witness-missing");

    let refreshed = progress_read(&bus, epoch, lane.clone(), Vec::new(), true);
    let CommandAnswer::Task(answer) = assert_committed(issue(
        &bus,
        epoch,
        lane,
        CommandKind::TaskDone,
        progress_witnesses(
            &refreshed,
            at(&refreshed, &plan),
            seats[0].lane,
            seats[0].occupant,
            false,
        )
        .expect("refreshed child witnesses admit plan completion"),
        &done,
    )) else {
        panic!("plan-only completion returned another answer")
    };
    assert_eq!(answer.status, "completed");
    clean(&root, bus);
}

#[test]
fn plan_only_completion_rejects_reverse_children_without_pending_acceptance() {
    let (root, bus, epoch, seats) = fixture("ineligible-gated-plan-children", 1);
    let lane = Actor::Lane(seats[0].lane);
    let plan = add_raw_task(
        &bus,
        epoch,
        "PLAN-1kRKw9Qrx",
        "Plan-only parent",
        &[],
        Some("plan"),
    );
    claim_task(&bus, epoch, &seats[0], &lane, &plan);
    let unaccepted = add_raw_task(
        &bus,
        epoch,
        "PLAN-1kRKw9Qsx",
        "Unaccepted implementation",
        &[],
        Some("todo"),
    );
    let completed = add_raw_task(
        &bus,
        epoch,
        "PLAN-1kRKw9Qtx",
        "Completed accepted implementation",
        &["the implementation behavior is verified"],
        Some("todo"),
    );
    connect_child(&bus, epoch, &unaccepted, &plan);
    connect_child(&bus, epoch, &completed, &plan);
    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let completed_heads = at(&visible, &completed);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskClose,
        transition_witnesses(completed_heads),
        &TaskClose {
            task: completed,
            validation: "implementation is complete".to_owned(),
            force_claimed: false,
        },
    ));

    let held = board(&bus, epoch, lane.clone());
    let refused = issue(
        &bus,
        epoch,
        lane,
        CommandKind::TaskDone,
        progress_witnesses(
            &held,
            at(&held, &plan),
            seats[0].lane,
            seats[0].occupant,
            false,
        )
        .expect("plan completion carries its own witnesses"),
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(&plan, "ineligible children do not satisfy planning")
        },
    );
    assert_refusal(refused, "task::term-missing");
    clean(&root, bus);
}
