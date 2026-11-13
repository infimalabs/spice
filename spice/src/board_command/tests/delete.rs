use super::*;

/// Drive one withdrawal through the door the CLI leaf reaches.
///
/// `Law::DELETE` names the claim head as well as the statement and workflow,
/// and the witnesses are read immediately before the command, so a scenario
/// that withdraws after a claim lands observes the claim the lane won rather
/// than the absence that preceded it.
fn withdraw(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    delete: &TaskDelete,
) -> Outcome<CommandAnswer> {
    let visible = board(bus, epoch, actor.clone());
    let heads = at(&visible, &delete.task);
    issue(
        bus,
        epoch,
        actor,
        CommandKind::TaskDelete,
        transition_witnesses(heads),
        delete,
    )
}

fn withdrawal(task: &TaskHandle, reason: &str, force_claimed: bool) -> TaskDelete {
    TaskDelete {
        task: task.clone(),
        reason: reason.to_owned(),
        force_claimed,
    }
}

/// Reason and overridden holder recorded by each withdrawal of this task.
fn delete_facts(bus: &Bus, task: &TaskHandle) -> Vec<(String, Option<String>)> {
    let born = task.born().expect("fixture handle carries a birth stamp");
    bus.transact(|journal| {
        bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
    })
    .expect("withdrawal facts read")
    .facts()
    .iter()
    .filter(|fact| {
        fact.fact.kind() == wire::FactKind::TaskDeleted
            && fact
                .fact
                .body()
                .get("task")
                .and_then(serde_json::Value::as_str)
                == Some(born.as_str())
    })
    .map(|fact| {
        (
            fact.fact
                .body()
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .expect("a withdrawal fact carries its reason")
                .to_owned(),
            fact.fact
                .body()
                .get("claimed")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        )
    })
    .collect()
}

fn listed(answer: &BoardAnswer, status: Option<TaskListStatus>) -> String {
    task_list(
        answer,
        &TaskListOptions {
            all: false,
            topic: None,
            limit: None,
            status,
        },
    )
}

/// A task filed wrong leaves the working board while its reason stays readable.
///
/// The journal is append-only and a fact is permanent, so withdrawal is a
/// terminal status rather than the removal of a row. That only serves the
/// operator if both halves hold at once: the row must stop appearing where
/// work is chosen, and the recorded reason must still be reachable. Asking
/// for the completed rows has to miss it too, or a withdrawal would be
/// counted as work that got done.
#[test]
fn a_withdrawal_takes_the_row_off_the_board_and_keeps_its_reason_readable() {
    let (root, bus, epoch, seats) = fixture("delete-record", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-drop00001",
        "An agent-technique lesson filed as a product defect",
        Some("todo"),
    );

    let outcome = withdraw(
        &bus,
        epoch,
        lane,
        &withdrawal(
            &task,
            "an agent-technique lesson, not a product defect",
            false,
        ),
    );
    let CommandAnswer::Task(answer) = assert_committed(outcome) else {
        panic!("a withdrawal answers with a task status");
    };
    assert_eq!(answer.task, task);
    assert_eq!(answer.status, "deleted");
    assert_eq!(answer.next.as_deref(), Some("spice task next"));
    assert_eq!(
        delete_facts(&bus, &task),
        vec![(
            "an agent-technique lesson, not a product defect".to_owned(),
            None
        )],
        "an unforced withdrawal took nobody's work, so it names no holder"
    );

    let after = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let row = at(&after, &task);
    assert!(row.workflow.body.status.is_deleted());
    assert!(
        row.workflow.body.status.completion().is_some(),
        "a withdrawal settles the row, which is what every open-task reader asks"
    );
    assert_eq!(
        row.presentation_title(),
        "An agent-technique lesson filed as a product defect",
        "the statement survives the withdrawal; only the workflow moved"
    );

    assert!(
        !listed(&after, None).contains(task.as_str()),
        "a withdrawn row must not appear where work is chosen"
    );
    assert!(
        !listed(&after, Some(TaskListStatus::Completed)).contains(task.as_str()),
        "a withdrawal is not a completion and must not be counted as one"
    );
    let withdrawn = listed(&after, Some(TaskListStatus::Deleted));
    assert!(
        withdrawn.lines().any(|line| line.starts_with(&format!(
            "{task} [deleted] /board/test An agent-technique lesson"
        ))),
        "a withdrawn row is reachable and shows how it ended, not the phase \
             it stopped in: {withdrawn}"
    );
    clean(&root, bus);
}

/// A live claim refuses the withdrawal, and forcing it names whose work it took.
///
/// This is the whole reason the claim head is a witness rather than a field
/// the door reads for itself. Refusing by default keeps an operator from
/// silently deleting work a lane is part-way through; forcing records the
/// displaced holder in the fact, so the withdrawal reports whose work it
/// took rather than a bare success. Freeing the claim in the same
/// transaction is what stops the lane renewing a lease against a row that
/// can never advance.
#[test]
fn a_live_claim_refuses_the_withdrawal_until_it_is_forced_and_then_names_the_holder() {
    let (root, bus, epoch, seats) = fixture("delete-claimed", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-drop00002",
        "Work a lane is part-way through",
        Some("todo"),
    );
    hold(&bus, epoch, &seats[0], &lane, &task);

    let refused = withdraw(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &withdrawal(&task, "filed against the wrong surface", false),
    );
    assert_eq!(
        refused_as(&refused),
        ("task::delete-claimed".to_owned(), Some(lane.to_string()),),
        "the refusal names the holder, so the operator knows whose work forcing would take"
    );
    let held = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&held, &task).workflow.body.status.is_pending());
    assert_eq!(
        at(&held, &task)
            .latest_claim()
            .map(|held| &held.claim)
            .expect("the claim survives the refusal")
            .body
            .disposition,
        ClaimDisposition::Active
    );
    assert!(
        delete_facts(&bus, &task).is_empty(),
        "a refused withdrawal writes nothing"
    );

    let forced = withdraw(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &withdrawal(&task, "filed against the wrong surface", true),
    );
    let CommandAnswer::Task(answer) = assert_committed(forced) else {
        panic!("a forced withdrawal answers with a task status");
    };
    assert_eq!(answer.status, "deleted");
    assert_eq!(
        delete_facts(&bus, &task),
        vec![(
            "filed against the wrong surface".to_owned(),
            Some(lane.to_string())
        )],
        "the forced withdrawal attributes the work it took to the lane that held it"
    );

    let after = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let row = at(&after, &task);
    assert!(row.workflow.body.status.is_deleted());
    assert_eq!(
        row.latest_claim()
            .map(|held| &held.claim)
            .expect("the claim head survives as history")
            .body
            .disposition,
        ClaimDisposition::Freed,
        "the same transaction releases the lane, or it renews against a settled row"
    );
    let Outcome::Reply(Reply {
        body: CommandAnswer::TaskDetail(detail),
        ..
    }) = issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDetail,
        Vec::new(),
        &wire::intent::TaskDetail {
            task: task.born().expect("fixture stamp"),
        },
    )
    else {
        panic!("task detail answers with the withdrawn task");
    };
    assert!(detail.closure.is_none());
    let deletion = detail
        .deletion
        .expect("task detail reads the withdrawal back beside the closure record");
    assert_eq!(
        (deletion.author, deletion.reason.as_str(), deletion.claimed),
        (
            Actor::TEST_PRINCIPAL,
            "filed against the wrong surface",
            Some(lane)
        ),
        "the detail names who withdrew it, why, and whose claim it took"
    );
    clean(&root, bus);
}

/// The lane a forced withdrawal displaced is stopped at the next door it reaches.
///
/// Recording the withdrawal is only half the job. The lane that held the
/// task is still mid-flight and will report its work next, and completing a
/// row the board has already withdrawn would put the two authorities into
/// disagreement. Freeing the claim is what prevents that, so the refusal it
/// produces is the observable that proves the release was real rather than
/// cosmetic.
#[test]
fn a_forced_withdrawal_stops_the_lane_it_displaced() {
    let (root, bus, epoch, seats) = fixture("delete-displaced", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-drop00007",
        "Work withdrawn out from under a lane",
        Some("todo"),
    );
    hold(&bus, epoch, &seats[0], &lane, &task);
    assert_committed(withdraw(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &withdrawal(&task, "the surface it names was never built", true),
    ));

    let after = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let expected = progress_witnesses(
        &after,
        at(&after, &task),
        seats[0].lane,
        seats[0].occupant,
        false,
    )
    .expect("the freed claim and its lease are still readable heads");
    let stopped = issue(
        &bus,
        epoch,
        lane,
        CommandKind::TaskDone,
        expected,
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(&task, "work the operator withdrew")
        },
    );
    assert_eq!(
        refused_as(&stopped).0,
        "claim::active-refused",
        "the freed claim is what stops the holder: {stopped:?}"
    );
    assert!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task)
            .workflow
            .body
            .status
            .is_deleted(),
        "the refused completion left the withdrawal standing"
    );
    clean(&root, bus);
}

/// A withdrawn prerequisite stops blocking whatever waited on it.
///
/// A withdrawn task will never complete, so counting it as unsatisfied would
/// block every dependent permanently with no move left that could clear the
/// edge. The dependent is the only observable that proves it: it is
/// unreachable while the prerequisite stands and allocatable once the
/// prerequisite is withdrawn.
#[test]
fn a_withdrawn_prerequisite_releases_the_task_that_waited_on_it() {
    let (root, bus, epoch, seats) = fixture("delete-depends", 1);
    let lane = Actor::Lane(seats[0].lane);
    let blocker = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-drop00003",
        "A prerequisite filed by mistake",
        Some("todo"),
    );
    let dependent_stamp = wire::TaskStamp::for_micros(40_004).expect("fixture task stamp");
    let dependent = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &dependent_stamp,
    );
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence {
            task: dependent_stamp,
        }],
        &TaskAdd {
            topic: Some("/board/test".parse().expect("fixture topic")),
            origin: Some(fixture_origin()),
            flow: Some("todo".to_owned()),
            depends: vec![blocker.clone()],
            ..adding(
                "Work waiting on the mistake",
                "This work keeps the fixture honest.",
            )
        },
    ));

    assert_committed(withdraw(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &withdrawal(&blocker, "the prerequisite was never real work", false),
    ));

    let available = board(&bus, epoch, lane.clone());
    let CommandAnswer::Claim(won) = assert_committed(issue(
        &bus,
        epoch,
        lane,
        CommandKind::ClaimWin,
        claim_expected(&available, &dependent, &seats[0]),
        &next_rule(),
    )) else {
        panic!("a claim answers with a claim");
    };
    assert_eq!(
        won.task, dependent,
        "the only allocatable task is the one the withdrawal released"
    );
    clean(&root, bus);
}

/// What a withdrawal releases, the operator's own status report agrees it released.
///
/// The allocator and status report each decide which prerequisites are
/// satisfied. Before withdrawal the dependent waits neutrally behind advancing
/// prerequisite work; afterward that same row is ready. Neither surface may
/// retain an obsolete wait after the prerequisite settles.
#[test]
fn a_released_dependent_moves_from_waiting_to_ready_in_status() {
    let (root, bus, epoch, seats) = fixture("delete-status", 1);
    let lane = Actor::Lane(seats[0].lane);
    let blocker = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-drop00008",
        "A prerequisite filed by mistake",
        Some("todo"),
    );
    let dependent_stamp = wire::TaskStamp::for_micros(40_009).expect("fixture task stamp");
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence {
            task: dependent_stamp,
        }],
        &TaskAdd {
            topic: Some("/board/test".parse().expect("fixture topic")),
            origin: Some(fixture_origin()),
            flow: Some("todo".to_owned()),
            depends: vec![blocker.clone()],
            ..adding(
                "Work waiting on the mistake",
                "This work keeps the fixture honest.",
            )
        },
    ));

    let before = task_status(&board(&bus, epoch, lane.clone())).expect("status reads");
    assert!(
        before.lines().any(|line| line == "waiting 1"),
        "the dependent waits while its prerequisite stands: {before}"
    );

    assert_committed(withdraw(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &withdrawal(&blocker, "the prerequisite was never real work", false),
    ));

    let after = task_status(&board(&bus, epoch, lane)).expect("status reads");
    assert!(
        after.lines().any(|line| line == "waiting 0"),
        "a withdrawn prerequisite leaves no dependency wait: {after}"
    );
    assert!(
        after.lines().any(|line| line == "ready 1"),
        "the released dependent is the one row ready to work: {after}"
    );
    clean(&root, bus);
}

/// A withdrawn task refuses everything a completed one refuses.
///
/// Withdrawal added a second terminal status, and every guard that asked
/// "is this completed" was really asking "is this settled". Correcting a row
/// that has left the board publishes a revision the working board never
/// shows, and deferring or waking one promises a return the allocator cannot
/// make: it admits pending rows only, so the wake would report success and
/// change nothing.
#[test]
fn a_withdrawn_task_refuses_the_moves_a_completed_task_refuses() {
    let (root, bus, epoch, _) = fixture("delete-settled", 1);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-drop00010",
        "Work withdrawn before anyone started it",
        Some("todo"),
    );
    assert_committed(withdraw(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &withdrawal(&task, "the surface it names was never built", false),
    ));

    let corrected = correct(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &TaskEdit {
            description: Some("a meaning the withdrawn row will never carry".to_owned()),
            ..only(&task)
        },
    );
    assert_eq!(
        refused_as(&corrected).0,
        "task::settled",
        "a withdrawn statement is as settled as a completed one"
    );

    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&visible, &task);
    for (kind, intent) in [
        (
            CommandKind::TaskDefer,
            serde_json::json!({ "task": task, "wait": "2026-09-01T00:00:00.000Z" }),
        ),
        (CommandKind::TaskWake, serde_json::json!({ "task": task })),
    ] {
        let outcome = issue(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            kind,
            vec![workflow_witness(heads), deferral_witness(heads)],
            &intent,
        );
        assert_eq!(
            refused_as(&outcome).0,
            "task::settled",
            "{kind:?} moved a row the allocator can never hand out again"
        );
    }
    assert!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task)
            .workflow
            .body
            .status
            .is_deleted(),
        "every refusal left the withdrawal exactly as it found it"
    );
    clean(&root, bus);
}

/// Every withdrawal the door declines, and the state each one leaves behind.
///
/// A reason is what separates a considered withdrawal from a mistake, so an
/// empty one is refused at the door rather than recorded as blank. A
/// completed task is history and a withdrawn one is already settled, and
/// both refusals have to leave the row exactly as they found it.
#[test]
fn a_withdrawal_without_a_reason_or_against_a_settled_task_is_refused() {
    let (root, bus, epoch, seats) = fixture("delete-refusals", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-drop00005",
        "Work still open",
        Some("todo"),
    );

    for blank in ["", "   "] {
        let outcome = withdraw(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            &withdrawal(&task, blank, false),
        );
        assert_eq!(
            refused_as(&outcome).0,
            "task::delete-reason-empty",
            "a withdrawal recording no reason cannot be told from one made in error"
        );
    }
    let standing = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&standing, &task).workflow.body.status.is_pending());
    assert!(delete_facts(&bus, &task).is_empty());

    assert_committed(withdraw(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &withdrawal(&task, "filed twice by mistake", false),
    ));
    let again = withdraw(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &withdrawal(&task, "filed twice by mistake", false),
    );
    assert_eq!(refused_as(&again).0, "task::already-deleted");
    assert_eq!(
        delete_facts(&bus, &task).len(),
        1,
        "the second withdrawal recorded nothing over the first"
    );

    let settled = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-drop00006",
        "Work already finished",
        Some("todo"),
    );
    claim_and_complete_task(&bus, epoch, &seats[0], &lane, &settled);
    let outcome = withdraw(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &withdrawal(&settled, "no longer wanted", false),
    );
    assert_eq!(
        refused_as(&outcome).0,
        "task::settled",
        "a completed task is history and stays on the board"
    );
    assert!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &settled)
            .workflow
            .body
            .status
            .is_completed()
    );
    assert!(delete_facts(&bus, &settled).is_empty());
    clean(&root, bus);
}
