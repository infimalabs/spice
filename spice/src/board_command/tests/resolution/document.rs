use super::*;

#[test]
fn document_ledger_pages_one_snapshot_and_exports_the_exact_document() {
    let (root, bus, epoch, _) = fixture("document-ledger-output", 0);
    let task = add_task_in_topic(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DOC-ledger01",
        "Export one ledger",
        "/task/document",
        Some("todo"),
    );
    let document = format!("# Export one ledger\n\n{}\n", "é".repeat(4_500));
    let budget = wire::output::INTERACTIVE
        .budget(wire::output::Request::default())
        .expect("interactive defaults admit");
    let digest = Digest::compute(Domain::CliExport, document.as_bytes());
    let first = wire::output::clip(&document, budget.limits().bytes);
    let offset = first.text.len();
    let preview = ledger_output(&task, &document, None, budget, 0, None).expect("preview renders");
    assert!(preview.starts_with(&first.text), "{preview}");
    assert!(
        preview.contains(&format!(
            "continue: spice doc ledger {task} --max-bytes {} --offset {offset} --digest {digest}",
            budget.limits().bytes
        )),
        "{preview}"
    );
    assert!(
        preview.contains(&format!(
            "complete: spice doc ledger {task} --output {task}-ledger.md"
        )),
        "{preview}"
    );
    let remainder = ledger_output(&task, &document, None, budget, offset, Some(&digest))
        .expect("continuation renders");
    assert_eq!(format!("{}{remainder}", first.text), document);

    let moved = format!("{document}later");
    let refusal = ledger_output(&task, &moved, None, budget, offset, Some(&digest))
        .expect_err("a continuation never splices another ledger snapshot");
    assert_eq!(refusal.reason(), "doc::ledger-snapshot-moved");

    let destination = root.join("ledger.md");
    ledger_output(
        &task,
        &document,
        Some(destination.to_str().expect("fixture path is UTF-8")),
        budget,
        0,
        None,
    )
    .expect("ledger exports");
    assert_eq!(
        std::fs::read_to_string(destination).expect("exported ledger reads"),
        document
    );
    clean(&root, bus);
}

/// A bare or contractless capture rests submitted in draft on its typed
/// route; only an accepted capture activates at creation.
#[test]
fn shape_cases_415_417_rest_contractless_captures_in_draft() {
    let (root, bus, epoch, _) = fixture("plan-shape-cases", 0);
    let bare = add_raw_task(
        &bus,
        epoch,
        "PLAN-1kLGp001x",
        "Shape 415 bare capture",
        &[],
        None,
    );
    let accepted = add_raw_task(
        &bus,
        epoch,
        "PLAN-1kLGp002x",
        "Shape 416 accepted capture",
        &["accepted work can begin"],
        None,
    );
    let explicit = add_raw_task(
        &bus,
        epoch,
        "PLAN-1kLGp003x",
        "Shape 417 explicit flow capture",
        &[],
        Some("todo"),
    );
    let shaped = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_workflow(&shaped, &bare, "draft", &["draft", "todo", "review"]);
    assert!(at(&shaped, &bare).workflow.body.is_submitted_draft());
    assert_workflow(&shaped, &accepted, "todo", &["draft", "todo", "review"]);
    assert_workflow(&shaped, &explicit, "draft", &["draft", "todo"]);
    assert!(at(&shaped, &explicit).workflow.body.is_submitted_draft());
    clean(&root, bus);
}

#[test]
fn held_review_corrections_inherit_topic_while_unrelated_claim_origin_routes() {
    let (root, bus, epoch, seats) = fixture("plan-shape-followup", 2);
    authorize_lane_creation(&bus, epoch, &seats[1]);
    seed_document_route(&bus, epoch);
    let lane_one = Actor::Lane(seats[0].lane);
    let lane_two = Actor::Lane(seats[1].lane);
    let reviewed = add_raw_task(
        &bus,
        epoch,
        "PLAN-1kLGp004x",
        "Review the held parent",
        &["review reaches follow-up creation"],
        Some("todo,review"),
    );
    advance_task_to_review(&bus, epoch, &seats[0], &lane_one, &reviewed);
    let assigned = board(&bus, epoch, lane_two.clone());
    assert_committed(issue(
        &bus,
        epoch,
        lane_two.clone(),
        CommandKind::ClaimWin,
        claim_expected(&assigned, &reviewed, &seats[1]),
        &next_rule(),
    ));
    let mut unrelated = authored_add(
        "Authored document plans and tasks",
        "/unused",
        Some("todo"),
        &["Document tasks remain independent of the held review"],
    );
    unrelated.topic = None;
    unrelated.origin = None;
    let CommandAnswer::Task(unrelated) = assert_committed(issue(
        &bus,
        epoch,
        lane_two.clone(),
        CommandKind::TaskAdd,
        vec![Witness::Absence {
            task: stamp("unrelated-review-routing"),
        }],
        &unrelated,
    )) else {
        panic!("ordinary creation returns a task receipt")
    };
    assert_eq!(
        unrelated.placement.as_ref().unwrap().topic.as_str(),
        "/documents/contracts"
    );
    let assigned = board(&bus, epoch, lane_two.clone());
    let proposed: TaskHandle = "PLAN-1kLGp005x".parse().expect("follow-up handle");
    let followup_stamp = proposed
        .born()
        .expect("follow-up handle carries a birth stamp");
    let followup = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &followup_stamp,
    );
    let mut expected = progress_witnesses(
        &assigned,
        at(&assigned, &reviewed),
        seats[1].lane,
        seats[1].occupant,
        true,
    )
    .expect("assigned review has progress witnesses");
    expected.push(Witness::Absence {
        task: followup_stamp.clone(),
    });
    assert_committed(issue(
        &bus,
        epoch,
        lane_two,
        CommandKind::TaskReview,
        expected,
        &TaskReview {
            then: vec![ReviewFollowup {
                task: followup_stamp,
                add: adding(
                    "Authored document plans and tasks",
                    "This correction lets the held review finish.",
                ),
            }],
            ..verdict(&reviewed, "review needs follow-up")
        },
    ));
    let followed = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_workflow(&followed, &followup, "draft", &["draft", "todo"]);
    assert!(at(&followed, &followup).workflow.body.is_submitted_draft());
    let inherited = "/board/test".parse().unwrap();
    assert_eq!(
        at(&followed, &followup).statement.body.topic.path,
        Some(inherited)
    );
    assert!(
        at(&followed, &unrelated.task)
            .statement
            .body
            .dependencies
            .is_empty()
    );
    clean(&root, bus);
}

#[test]
fn a_submitted_draft_holder_advances_it_only_once_acceptance_exists() {
    let (root, bus, epoch, seats) = fixture("plan-acceptance", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add_raw_task(
        &bus,
        epoch,
        "PLAN-1kLGp006x",
        "WK-16 plan needs acceptance",
        &[],
        None,
    );
    claim_task(&bus, epoch, &seats[0], &lane, &task);
    let held = board(&bus, epoch, lane.clone());
    let refused = issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::TaskDone,
        progress_witnesses(
            &held,
            at(&held, &task),
            seats[0].lane,
            seats[0].occupant,
            false,
        )
        .expect("plan task has progress witnesses"),
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(&task, "plan is not accepted")
        },
    );
    assert_refusal(refused, "task::term-missing");

    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        vec![statement_witness(at(&visible, &task))],
        &TaskEdit {
            term: Some(vec!["implementation has a checkable contract".to_owned()]),
            ..only(&task)
        },
    ));
    // The operator changed the held task, so the holder acknowledges it first.
    refresh_held(&bus, epoch, &lane, &seats[0], &task);
    let repaired = board(&bus, epoch, lane.clone());
    assert_committed(issue(
        &bus,
        epoch,
        lane,
        CommandKind::TaskDone,
        progress_witnesses(
            &repaired,
            at(&repaired, &task),
            seats[0].lane,
            seats[0].occupant,
            false,
        )
        .expect("accepted plan has progress witnesses"),
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(&task, "acceptance exists on the task")
        },
    ));
    let advanced = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let activated = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &task.born().expect("fixture identity"),
    );
    assert_workflow(&advanced, &activated, "todo", &["draft", "todo", "review"]);
    clean(&root, bus);
}

#[test]
fn a_submitted_draft_holder_advances_on_connected_child_acceptance() {
    let (root, bus, epoch, seats) = fixture("plan-child-proof", 1);
    let lane = Actor::Lane(seats[0].lane);
    let parent = add_raw_task(
        &bus,
        epoch,
        "PLAN-1kLGp007x",
        "WK-27 parent plan",
        &[],
        None,
    );
    let child = add_raw_task(
        &bus,
        epoch,
        "PLAN-1kLGp008x",
        "WK-28 accepted child",
        &["child carries acceptance"],
        None,
    );
    claim_task(&bus, epoch, &seats[0], &lane, &parent);
    connect_child(&bus, epoch, &parent, &child);

    let held = board(&bus, epoch, lane.clone());
    assert_committed(issue(
        &bus,
        epoch,
        lane,
        CommandKind::TaskDone,
        progress_witnesses(
            &held,
            at(&held, &parent),
            seats[0].lane,
            seats[0].occupant,
            false,
        )
        .expect("connected child proof is guarded"),
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(&parent, "accepted child is connected")
        },
    ));
    let advanced = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let activated = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &parent.born().expect("fixture identity"),
    );
    assert_workflow(&advanced, &activated, "todo", &["draft", "todo", "review"]);
    clean(&root, bus);
}

#[test]
fn a_plan_only_flow_completes_with_a_connected_accepted_child() {
    let (root, bus, epoch, seats) = fixture("plan-only-child-completes", 1);
    let lane = Actor::Lane(seats[0].lane);
    let parent = add_raw_task(
        &bus,
        epoch,
        "PLAN-1kLGp00Bx",
        "WK-30 plan-only parent",
        &[],
        Some("plan"),
    );
    let child = add_raw_task(
        &bus,
        epoch,
        "PLAN-1kLGp00Cx",
        "WK-30 accepted child",
        &["child owns implementation acceptance"],
        None,
    );
    claim_task(&bus, epoch, &seats[0], &lane, &parent);
    connect_child(&bus, epoch, &parent, &child);
    let held = board(&bus, epoch, lane.clone());
    assert_committed(issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::TaskDone,
        progress_witnesses(
            &held,
            at(&held, &parent),
            seats[0].lane,
            seats[0].occupant,
            false,
        )
        .expect("plan-only child proof is witnessed"),
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(&parent, "accepted child is connected")
        },
    ));
    let advanced = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let plan = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &parent.born().expect("fixture identity"),
    );
    assert_workflow(&advanced, &plan, "plan", &["draft", "plan"]);
    assert!(
        at(&advanced, &plan)
            .latest_claim()
            .is_some_and(|held| held.claim.body.disposition == ClaimDisposition::Active),
        "leaving draft carries the holder's claim into plan"
    );
    let held = board(&bus, epoch, lane.clone());
    assert_committed(issue(
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
        .expect("plan-only child proof is witnessed"),
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(&plan, "accepted child is connected")
        },
    ));
    let completed = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&completed, &plan).workflow.body.status.is_completed());
    clean(&root, bus);
}

fn seed_document_route(bus: &Bus, epoch: CanonicalUuid) {
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        format!("resident:{epoch}").parse().unwrap(),
        current_instant().unwrap(),
        Producer::CommandDoor,
    );
    write
        .fact(
            wire::topic::TopicDescribed {
                topic: "/documents/contracts".parse().unwrap(),
                description: "Authored document plans and tasks".to_owned(),
            },
            FactCause::Root,
        )
        .unwrap();
    assert!(matches!(
        bus.commit(write).unwrap(),
        CommitOutcome::Committed(_)
    ));
}
