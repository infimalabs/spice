use super::*;

fn dependent_add(dependencies: Vec<TaskHandle>, flow: Option<&str>) -> TaskAdd {
    TaskAdd {
        topic: Some("/task/workflow".parse().expect("fixture topic")),
        term: vec!["the fixture work is verified".to_owned()],
        origin: Some(fixture_origin()),
        flow: flow.map(str::to_owned),
        depends: dependencies,
        ..adding("Dependent work", "This work keeps the fixture honest.")
    }
}

fn workflow_handle(born: &TaskStamp) -> TaskHandle {
    TaskHandle::for_topic(
        &"/task/workflow"
            .parse::<wire::Topic>()
            .expect("fixture topic"),
        born,
    )
}

/// An add the issuing lane holds at once, under fixture steering.
fn claimed_add(title: &str, depends: Vec<TaskHandle>) -> TaskAdd {
    TaskAdd {
        topic: Some("/task/workflow".parse().expect("fixture topic")),
        term: vec!["the fixture work is verified".to_owned()],
        origin: Some("ack:1kLGp2NJx".parse().expect("fixture steering origin")),
        flow: Some("todo,review".to_owned()),
        depends,
        claim: true,
        ..adding(title.to_owned(), "This work keeps the fixture honest.")
    }
}

/// Issue a create-and-claim of `add` born as `born`, witnessed against the
/// current board.
fn issue_claimed_add(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    born: TaskStamp,
    add: &TaskAdd,
) -> Outcome<CommandAnswer> {
    let actor = Actor::Lane(seat.lane);
    let current = board(bus, epoch, actor.clone());
    issue(
        bus,
        epoch,
        actor,
        CommandKind::TaskAdd,
        vec![
            Witness::Absence { task: born },
            Witness::Eligibility {
                rule: "capture".parse().expect("fixture capture rule"),
            },
            residency(&current, seat),
        ],
        add,
    )
}

fn add_with_dependencies(
    bus: &Bus,
    epoch: CanonicalUuid,
    task: &str,
    dependencies: Vec<TaskHandle>,
    flow: Option<&str>,
) -> TaskHandle {
    let born = stamp(task);
    let handle = workflow_handle(&born);
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born.clone() }],
        &dependent_add(dependencies, flow),
    ));
    handle
}

fn flow<'a>(answer: &'a BoardAnswer, task: &TaskHandle) -> Vec<&'a str> {
    at(answer, task)
        .workflow
        .body
        .flow
        .iter()
        .map(|phase| phase.as_str())
        .collect()
}

fn complete_review(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, task: &TaskHandle) {
    let lane = Actor::Lane(seat.lane);
    claim_task(bus, epoch, seat, &lane, task);
    let held = board(bus, epoch, lane.clone());
    let expected = progress_witnesses(&held, at(&held, task), seat.lane, seat.occupant, true)
        .expect("held review has progress witnesses");
    assert_committed(issue(
        bus,
        epoch,
        lane,
        CommandKind::TaskReview,
        expected,
        &verdict(task, "clean"),
    ));
}

#[test]
fn task_plan_and_design_creation_admit_a_public_topic_at_any_depth() {
    let (root, bus, epoch, _) = fixture("creation-authored-root", 1);
    for (name, intent, flow) in [
        ("ordinary", None, "todo,review"),
        ("plan", Some(wire::TaskIntent::Plan), "plan,review"),
        ("design", Some(wire::TaskIntent::Design), "design"),
    ] {
        let create = |label: &str, topic: &str| {
            let born = stamp(&format!("{name}-{label}"));
            let mut add = dependent_add(Vec::new(), Some(flow));
            add.title = format!("Authored {name} topic");
            add.topic = Some(topic.parse().expect("fixture topic"));
            add.intent = intent;
            add.term = vec!["Reach the outcome".to_owned()];
            let command = fixture_command(
                epoch,
                Actor::TEST_PRINCIPAL,
                CommandKind::TaskAdd,
                vec![Witness::Absence { task: born }],
                &add,
            );
            task_add(&bus, &command, &add, None)
        };
        for (label, topic) in [
            ("single", "/foreign"),
            ("nested", "/foreign/work"),
            ("deep", "/foreign/one/two/three/four"),
        ] {
            assert_committed(create(label, topic));
            assert!(
                board(&bus, epoch, Actor::TEST_PRINCIPAL)
                    .tasks
                    .iter()
                    .any(|task| task
                        .statement
                        .body
                        .topic
                        .path
                        .as_ref()
                        .map(wire::Topic::as_str)
                        == Some(topic)
                        && task.statement.body.intent == intent),
                "{name} {topic} was not stored",
            );
        }
    }
    clean(&root, bus);
}

#[test]
fn create_and_claim_is_one_transaction_and_active_claim_refusal_is_zero_write() {
    let (root, bus, epoch, seats) = fixture("create-and-claim", 1);
    let seat = &seats[0];
    let actor = Actor::Lane(seat.lane);
    authorize_lane_creation(&bus, epoch, seat);
    let born = stamp("CLAIM-add00001");
    let handle = workflow_handle(&born);
    let add = claimed_add("Board and hold this work", Vec::new());
    let Outcome::Reply(Reply {
        position: ReplyPosition::Committed(range),
        body: CommandAnswer::Task(answer),
    }) = issue_claimed_add(&bus, epoch, seat, born, &add)
    else {
        panic!("create-and-claim did not commit")
    };
    let held = answer
        .claim
        .expect("task add --claim returns its held claim");
    assert_eq!(answer.task, handle);
    assert_eq!(held.task, handle);
    assert_eq!(held.holder, Some(actor.clone()));
    assert_eq!(
        held.phase.as_ref().map(|phase| phase.name.as_str()),
        Some("todo")
    );
    let committed = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("fixture facts remain readable")
        .facts()
        .iter()
        .filter(|fact| fact.seq >= range.first && fact.seq <= range.last)
        .map(|fact| fact.fact.kind())
        .collect::<Vec<_>>();
    assert!(committed.contains(&wire::FactKind::TaskAdded));
    assert!(committed.contains(&wire::FactKind::ClaimWon));
    let current = board(&bus, epoch, actor.clone());
    let claim = at(&current, &handle)
        .latest_claim()
        .map(|held| &held.claim)
        .expect("created task is held");
    assert_eq!(claim.body.actor, actor);
    assert_eq!(claim.body.disposition, ClaimDisposition::Active);

    let refused_born = stamp("CLAIM-add00002");
    let refused_handle = workflow_handle(&refused_born);
    let facts_before = fact_count(&bus);
    let outcome = issue_claimed_add(
        &bus,
        epoch,
        seat,
        refused_born,
        &TaskAdd {
            title: "A second held task must refuse".to_owned(),
            ..add
        },
    );
    assert!(
        matches!(outcome, Outcome::Refusal(ref refusal)
            if refusal.reason() == "claim::active-exists"),
        "active claim must refuse create-and-claim: {outcome:?}",
    );
    let after = board(&bus, epoch, actor);
    assert_eq!(
        fact_count(&bus),
        facts_before,
        "refusal must not write any facts",
    );
    assert!(
        after
            .tasks
            .iter()
            .all(|task| task.statement.body.handle != refused_handle),
        "refused task must not exist",
    );
    clean(&root, bus);
}

/// An add whose new task could not be claimed refuses whole, and its remedy
/// routes through adding without a claim rather than naming the handle the
/// refused write never created.
#[test]
fn create_and_claim_of_blocked_work_refuses_without_naming_the_uncreated_task() {
    let (root, bus, epoch, seats) = fixture("create-and-claim-blocked", 1);
    let seat = &seats[0];
    let actor = Actor::Lane(seat.lane);
    authorize_lane_creation(&bus, epoch, seat);
    let prerequisite = todo_task(&bus, epoch, "CLAIM-first001", "Open prerequisite");
    let born = stamp("CLAIM-block001");
    let handle = workflow_handle(&born);
    let facts_before = fact_count(&bus);
    let outcome = issue_claimed_add(
        &bus,
        epoch,
        seat,
        born,
        &claimed_add("Blocked work this lane wants to hold", vec![prerequisite]),
    );
    let Outcome::Refusal(refusal) = &outcome else {
        panic!("blocked create-and-claim must refuse: {outcome:?}");
    };
    assert_eq!(refusal.reason(), "claim::capture-ineligible");
    let remedy = refusal.explanation();
    assert!(
        remedy.contains("without --claim") && !remedy.contains(&handle.to_string()),
        "{remedy}"
    );
    assert_eq!(
        fact_count(&bus),
        facts_before,
        "refusal must not write any facts",
    );
    assert!(
        board(&bus, epoch, actor)
            .tasks
            .iter()
            .all(|task| task.statement.body.handle != handle),
        "refused task must not exist",
    );
    clean(&root, bus);
}

/// A claiming add whose contract cannot leave draft refuses whole: its author
/// hears what the contract lacks, and no unclaimed draft is left for triage
/// to hand to another lane.
#[test]
fn create_and_claim_of_an_incomplete_contract_refuses_without_a_draft() {
    let (root, bus, epoch, seats) = fixture("create-and-claim-incomplete", 1);
    let seat = &seats[0];
    authorize_lane_creation(&bus, epoch, seat);
    let born = stamp("CLAIM-unacc001");
    let facts_before = fact_count(&bus);
    let outcome = issue_claimed_add(
        &bus,
        epoch,
        seat,
        born.clone(),
        &TaskAdd {
            term: Vec::new(),
            ..claimed_add("Work whose acceptance is not written yet", Vec::new())
        },
    );
    let Outcome::Refusal(refusal) = &outcome else {
        panic!("an incomplete create-and-claim must refuse: {outcome:?}");
    };
    assert_eq!(refusal.reason(), "task::claim-unadvanced");
    let observed = refusal
        .diagnostic()
        .and_then(wire::RefusalDiagnostic::observed)
        .unwrap_or_default();
    assert!(observed.contains("task::term-missing"), "{observed}");
    assert_eq!(
        fact_count(&bus),
        facts_before,
        "refusal must not write any facts"
    );
    assert!(
        board(&bus, epoch, Actor::Lane(seat.lane))
            .tasks
            .iter()
            .all(|task| task.statement.body.born != born),
        "refused task must not exist",
    );
    clean(&root, bus);
}

#[test]
fn a_defaulted_flow_omits_review_only_when_every_dependency_is_in_review() {
    let (root, bus, epoch, seats) = fixture("creation-dependency-flow", 1);
    let lane = Actor::Lane(seats[0].lane);
    let completed = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "FLOW-complete1",
        "Completed review dependency",
        Some("review"),
    );
    complete_review(&bus, epoch, &seats[0], &completed);
    let reviewed = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "FLOW-review001",
        "Review dependency",
        Some("todo,review"),
    );
    claim_and_complete_task(&bus, epoch, &seats[0], &lane, &reviewed);
    let todo = todo_task(&bus, epoch, "FLOW-todo0001", "Todo dependency");

    let all_review =
        add_with_dependencies(&bus, epoch, "FLOW-allreview", vec![reviewed.clone()], None);
    let explicit = add_with_dependencies(
        &bus,
        epoch,
        "FLOW-explicit1",
        vec![reviewed.clone()],
        Some("plan,todo,review"),
    );
    let mixed = add_with_dependencies(&bus, epoch, "FLOW-mixed001", vec![reviewed, todo], None);
    let settled = add_with_dependencies(&bus, epoch, "FLOW-settled01", vec![completed], None);
    let none = add_with_dependencies(&bus, epoch, "FLOW-none0001", Vec::new(), None);
    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);

    assert_eq!(flow(&current, &all_review), ["draft", "todo"]);
    assert_eq!(
        flow(&current, &explicit),
        ["draft", "plan", "todo", "review"]
    );
    assert_eq!(flow(&current, &mixed), ["draft", "todo", "review"]);
    assert_eq!(flow(&current, &settled), ["draft", "todo", "review"]);
    assert_eq!(flow(&current, &none), ["draft", "todo", "review"]);
    clean(&root, bus);
}

#[test]
fn dependency_phase_movement_refuses_creation_with_the_stale_flow() {
    let (root, bus, epoch, seats) = fixture("creation-dependency-race", 1);
    let dependency = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "FLOW-moving01",
        "Moving dependency",
        Some("review"),
    );
    let born = stamp("FLOW-stale0001");
    let handle = workflow_handle(&born);
    let add = dependent_add(vec![dependency.clone()], None);
    let command = fixture_command(
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born }],
        &add,
    );

    let outcome = task_add_with(&bus, &command, &add, None, || {
        complete_review(&bus, epoch, &seats[0], &dependency)
    });
    assert!(
        matches!(outcome, Outcome::Refusal(ref refusal)
            if refusal.reason() == "board::witness-moved"),
        "dependency movement must refuse the stale creation: {outcome:?}",
    );
    assert!(
        board(&bus, epoch, Actor::TEST_PRINCIPAL)
            .tasks
            .iter()
            .all(|task| task.statement.body.handle != handle),
        "the refused task add remains zero-write",
    );
    clean(&root, bus);
}

/// Every value option the capture contract declares describes new work, so
/// each must be in the creation-only list that refuses it beside an existing
/// HANDLE; an option the list omits would be accepted and silently dropped.
#[test]
fn every_capture_value_option_is_a_creation_only_field() {
    use std::collections::BTreeSet;
    let parser = crate::cli::command();
    let capture = ["task", "capture"].iter().fold(&parser, |command, word| {
        command
            .get_subcommands()
            .find(|child| child.get_name() == *word)
            .expect("task capture is registered")
    });
    let value_options = capture
        .get_arguments()
        .filter(|argument| !argument.is_positional() && argument.get_action().takes_values())
        .map(|argument| argument.get_id().as_str())
        .collect::<BTreeSet<_>>();
    let creation = crate::board_command::CAPTURE_CREATION_FIELDS
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    assert_eq!(value_options, creation);
}

fn public_task_add_expected(
    answer: &BoardAnswer,
    add: &TaskAdd,
    child: wire::TaskStamp,
) -> Vec<Witness> {
    let mut expected = vec![Witness::Absence { task: child }];
    append_preparation_witnesses(answer, add, &mut expected)
        .expect("public task add maps current board authority");
    expected
}

fn accepted_child() -> TaskAdd {
    let mut add = dependent_add(Vec::new(), Some("todo,review"));
    add.origin = None;
    add.term = vec!["the implementation has a checkable contract".to_owned()];
    add
}

#[test]
fn an_ambient_plan_releases_its_output_on_positive_completion_without_selection() {
    let (root, bus, epoch, seats) = fixture("ambient-plan-child", 2);
    let seat = &seats[0];
    let lane = Actor::Lane(seat.lane);
    authorize_lane_creation(&bus, epoch, seat);
    let plan = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "PLAN-ambient01",
        "Plan-only parent",
        Some("plan"),
    );
    claim_task(&bus, epoch, seat, &lane, &plan);
    let held = board(&bus, epoch, lane.clone());
    let child_born = stamp("PLAN-child0001");
    let child = workflow_handle(&child_born);
    let add = accepted_child();

    assert_committed(issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::TaskAdd,
        public_task_add_expected(&held, &add, child_born.clone()),
        &add,
    ));
    let linked = board(&bus, epoch, lane.clone());
    assert_eq!(at(&linked, &plan).statement, at(&held, &plan).statement);
    assert!(at(&linked, &child).statement.body.dependencies.is_empty());
    assert!(at(&linked, &child).workflow.body.is_draft());
    let worker = Actor::Lane(seats[1].lane);
    let offered = |eligible: bool, reason: &str| {
        let available = board(&bus, epoch, worker.clone());
        let entries = actor_entries(&available).expect("board entries");
        let subscription = allocation_for(&available, &worker);
        let environment = available.environment();
        assert_eq!(
            board::select(&entries, &worker, &subscription, &environment,)
                .map(|selection| selection.entry().born().clone()),
            eligible.then(|| child_born.clone()),
            "{reason}"
        );
        let satisfied = board::resolved_tasks(&entries);
        let output = entries
            .iter()
            .find(|entry| entry.born() == &child_born)
            .expect("the output is on the board");
        assert_eq!(
            (
                board::eligible_entry(&satisfied, output, &worker, &environment),
                board::stealable_entry(&satisfied, output, &worker, &environment),
            ),
            (eligible, eligible),
            "{reason}: capture and steal share the gate"
        );
    };
    offered(false, "the held plan must keep its child out of allocation");

    let declare = TaskEdit {
        term: Some(vec!["the fixture work is verified".to_owned()]),
        intent: Some(wire::TaskIntentEdit::Set {
            intent: wire::TaskIntent::Plan,
        }),
        flow: Some("plan,review".to_owned()),
        ..only(&child)
    };
    assert_committed(correct(&bus, epoch, Actor::TEST_PRINCIPAL, &declare));
    offered(false, "an output cannot declare its way past its Plan");

    let linked = board(&bus, epoch, lane.clone());
    let expected = progress_witnesses(&linked, at(&linked, &plan), seat.lane, seat.occupant, false)
        .expect("the linked accepted child witnesses the attempted conclusion");
    assert_committed(issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::TaskDone,
        expected,
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(&plan, "accepted implementation child is connected")
        },
    ));
    let concluded = board(&bus, epoch, worker.clone());
    assert!(at(&concluded, &plan).workflow.body.status.is_completed());
    assert!(at(&concluded, &child).workflow.body.is_activated());
    assert!(
        at(&concluded, &child)
            .statement
            .body
            .dependencies
            .is_empty()
    );
    assert_eq!(
        at(&concluded, &child)
            .statement
            .body
            .preparation
            .as_ref()
            .expect("retained lineage")
            .parent,
        plan.born().expect("Plan stamp")
    );
    offered(
        true,
        "the concluded plan releases its output without selection",
    );
    clean(&root, bus);
}

#[test]
fn explicit_dependencies_suppress_the_ambient_link() {
    let (root, bus, epoch, seats) = fixture("ambient-plan-override", 1);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    let plan = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "PLAN-override1",
        "Planning parent",
        Some("plan"),
    );
    let prerequisite = todo_task(&bus, epoch, "PLAN-prereq001", "Explicit prerequisite");
    let planner = Actor::Lane(seats[0].lane);
    claim_task(&bus, epoch, &seats[0], &planner, &plan);
    let explicit_born = stamp("PLAN-explicit1");
    let explicit = workflow_handle(&explicit_born);
    let held = board(&bus, epoch, planner.clone());
    let add = dependent_add(vec![prerequisite.clone()], Some("todo"));
    assert_committed(issue(
        &bus,
        epoch,
        planner,
        CommandKind::TaskAdd,
        public_task_add_expected(&held, &add, explicit_born.clone()),
        &add,
    ));
    let after_explicit = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(
        at(&after_explicit, &plan)
            .statement
            .body
            .dependencies
            .is_empty()
    );
    assert_eq!(
        at(&after_explicit, &explicit).statement.body.dependencies,
        [prerequisite
            .born()
            .expect("fixture prerequisite has a birth stamp")]
    );

    clean(&root, bus);
}
#[test]
fn every_ambient_planning_child_stays_draft_under_its_parent_plan() {
    let (root, bus, epoch, seats) = fixture("ambient-subplan", 1);
    let seat = &seats[0];
    authorize_lane_creation(&bus, epoch, seat);
    let parent = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "PLAN-subparent",
        "Parent Plan",
        Some("plan"),
    );
    let planner = Actor::Lane(seat.lane);
    claim_task(&bus, epoch, seat, &planner, &parent);
    let mut children = Vec::new();
    for (name, intent, flow) in [
        ("PLAN-subplan01", wire::TaskIntent::Plan, "plan,review"),
        ("DESIGN-child01", wire::TaskIntent::Design, "design"),
    ] {
        let born = stamp(name);
        let child = workflow_handle(&born);
        let held = board(&bus, epoch, planner.clone());
        let mut add = accepted_child();
        add.intent = Some(intent);
        add.flow = Some(flow.to_owned());
        assert_committed(issue(
            &bus,
            epoch,
            planner.clone(),
            CommandKind::TaskAdd,
            public_task_add_expected(&held, &add, born.clone()),
            &add,
        ));
        children.push((born, child));
    }

    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let parent = parent.born().expect("parent Plan stamp");
    for (born, child) in children {
        let heads = at(&current, &child);
        assert!(heads.workflow.body.is_draft());
        assert!(heads.statement.body.dependencies.is_empty());
        assert_eq!(
            heads
                .statement
                .body
                .preparation
                .as_ref()
                .expect("construction")
                .parent,
            parent
        );
        assert!(!current.offer.contains(&born));
    }
    clean(&root, bus);
}

#[test]
fn a_non_plan_claim_does_not_gain_the_created_task_as_a_dependency() {
    let (root, bus, epoch, seats) = fixture("ambient-non-plan", 1);
    let seat = &seats[0];
    authorize_lane_creation(&bus, epoch, seat);
    let todo = todo_task(&bus, epoch, "PLAN-todo0002", "Implementation parent");
    let implementer = Actor::Lane(seat.lane);
    claim_task(&bus, epoch, seat, &implementer, &todo);
    let ordinary_born = stamp("PLAN-ordinary1");
    let ordinary = workflow_handle(&ordinary_born);
    let held = board(&bus, epoch, implementer.clone());
    let add = accepted_child();
    assert_committed(issue(
        &bus,
        epoch,
        implementer,
        CommandKind::TaskAdd,
        public_task_add_expected(&held, &add, ordinary_born),
        &add,
    ));
    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&current, &todo).statement.body.dependencies.is_empty());
    assert!(
        at(&current, &ordinary)
            .statement
            .body
            .dependencies
            .is_empty()
    );
    clean(&root, bus);
}

#[test]
fn ambient_plan_authority_movement_refuses_child_creation() {
    for movement in ["statement", "workflow", "claim"] {
        let (root, bus, epoch, seats) = fixture("ambient-plan-race", 1);
        let seat = &seats[0];
        let lane = Actor::Lane(seat.lane);
        authorize_lane_creation(&bus, epoch, seat);
        let parent_born = stamp("PLAN-moving001");
        let plan = workflow_handle(&parent_born);
        let mut parent_add = dependent_add(Vec::new(), Some("plan,todo"));
        parent_add.term = vec!["planning has an explicit execution contract".to_owned()];
        assert_committed(issue(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            CommandKind::TaskAdd,
            vec![Witness::Absence { task: parent_born }],
            &parent_add,
        ));
        claim_task(&bus, epoch, seat, &lane, &plan);
        let competing = todo_task(&bus, epoch, "PLAN-compete01", "Competing child");
        let held = board(&bus, epoch, lane.clone());
        let child_born = stamp("PLAN-stale0001");
        let child = workflow_handle(&child_born);
        let add = accepted_child();
        let command = fixture_command(
            epoch,
            lane.clone(),
            CommandKind::TaskAdd,
            public_task_add_expected(&held, &add, child_born),
            &add,
        );

        let outcome = task_add_with(&bus, &command, &add, None, || match movement {
            "statement" => add_dependency(&bus, epoch, &plan, &competing),
            "workflow" => {
                assert_committed(issue(
                    &bus,
                    epoch,
                    lane.clone(),
                    CommandKind::TaskDone,
                    progress_witnesses(&held, at(&held, &plan), seat.lane, seat.occupant, false)
                        .expect("held plan witnesses"),
                    &TaskDone {
                        operation: Some(completion_operation()),
                        ..finished(&plan, "execution contract is explicit")
                    },
                ));
            }
            "claim" => {
                let heads = at(&held, &plan);
                assert_committed(issue(
                    &bus,
                    epoch,
                    lane.clone(),
                    CommandKind::ClaimFree,
                    vec![
                        held_claim_witness(heads),
                        held_lease_witness(heads),
                        residency(&held, seat),
                    ],
                    &TaskRef { task: plan.clone() },
                ));
            }
            _ => unreachable!(),
        });
        assert!(
            matches!(&outcome, Outcome::Refusal(refusal)
                if refusal.reason() == "board::witness-moved"),
            "{movement} movement must refuse stale child creation: {outcome:?}",
        );
        let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
        assert!(
            current
                .tasks
                .iter()
                .all(|task| task.statement.body.handle != child),
            "stale {movement} authority must not publish a child"
        );
        clean(&root, bus);
    }
}
