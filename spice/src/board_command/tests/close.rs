use super::*;

/// Drive one operator closure against the heads the board shows right now.
fn close(
    bus: &Bus,
    epoch: CanonicalUuid,
    task: &TaskHandle,
    validation: &str,
    force_claimed: bool,
) -> Outcome<CommandAnswer> {
    let visible = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&visible, task);
    issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskClose,
        transition_witnesses(heads),
        &TaskClose {
            task: task.clone(),
            validation: validation.to_owned(),
            force_claimed,
        },
    )
}

/// Every closure fact recorded for this task.
fn closures(bus: &Bus, task: &TaskHandle) -> Vec<body::TaskClosed> {
    let born = task.born().expect("fixture handle carries a birth stamp");
    bus.transact(|journal| {
        bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
    })
    .expect("closure facts read")
    .facts()
    .iter()
    .filter_map(|fact| fact.fact.read::<body::TaskClosed>())
    .filter(|closed| closed.task == born)
    .collect()
}

/// The operator closes work mid-flow and whatever waited on it can proceed.
///
/// The closure is the operator's own completion, so it must land as the same
/// terminal `completed` status a finished review would, from whichever phase
/// the task stood in, with the operator's text kept as its validation. The
/// dependent becoming allocatable is the observable that proves the board
/// counts it as done rather than as a withdrawal or a note.
#[test]
fn an_operator_closure_completes_from_any_phase_and_releases_its_dependents() {
    let (root, bus, epoch, seats) = fixture("close-completes", 1);
    let lane = Actor::Lane(seats[0].lane);
    let blocker = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-close0001",
        "Work the operator verified themselves",
        Some("todo,review"),
    );
    let dependent_stamp = wire::TaskStamp::for_micros(50_004).expect("fixture task stamp");
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
                "Work waiting on the verified task",
                "This work keeps the fixture honest.",
            )
        },
    ));

    let CommandAnswer::Task(answer) = assert_committed(close(
        &bus,
        epoch,
        &blocker,
        "  verified in the browser  ",
        false,
    )) else {
        panic!("a closure answers with a task status");
    };
    assert_eq!(answer.status, "closed");
    let after = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let row = at(&after, &blocker);
    assert!(row.workflow.body.status.is_completed());
    assert_eq!(row.workflow.body.phase.as_str(), "todo");
    assert!(row.workflow.body.status.completion().is_some());
    assert_eq!(
        closures(&bus, &blocker),
        vec![body::TaskClosed {
            task: blocker.born().expect("fixture stamp"),
            validation: "  verified in the browser  ".to_owned(),
            phase: "todo".parse().expect("fixture phase"),
            claimed: None,
        }],
        "the closure records the operator's text as validation and the phase it cut short"
    );

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
        "the closure settled the dependency edge"
    );
    clean(&root, bus);
}

/// A live claim refuses the closure, and forcing it names and frees the holder.
#[test]
fn a_live_claim_refuses_the_closure_until_it_is_forced_and_then_names_the_holder() {
    let (root, bus, epoch, seats) = fixture("close-claimed", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-close0002",
        "Work a lane is part-way through",
        Some("todo,review"),
    );
    hold(&bus, epoch, &seats[0], &lane, &task);

    assert_eq!(
        refused_as(&close(&bus, epoch, &task, "done already", false)),
        ("task::close-claimed".to_owned(), Some(lane.to_string())),
    );
    assert!(
        closures(&bus, &task).is_empty(),
        "a refused closure writes nothing"
    );

    assert_committed(close(&bus, epoch, &task, "done already", true));
    let row = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let row = at(&row, &task);
    assert!(row.workflow.body.status.is_completed());
    assert_eq!(
        row.latest_claim()
            .map(|held| &held.claim)
            .expect("the claim head survives as history")
            .body
            .disposition,
        ClaimDisposition::Freed,
        "the same transaction releases the lane, or it renews against a settled row"
    );
    assert_eq!(
        closures(&bus, &task)
            .into_iter()
            .map(|closed| closed.claimed)
            .collect::<Vec<_>>(),
        vec![Some(lane.clone())],
        "the forced closure attributes the work it took to the lane that held it"
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
        panic!("task detail answers with the task");
    };
    let closure = detail
        .closure
        .expect("task detail reads the closure back, since it records no note");
    assert_eq!(
        (
            closure.author,
            closure.validation.as_str(),
            closure.phase.as_str(),
            closure.claimed
        ),
        (Actor::TEST_PRINCIPAL, "done already", "todo", Some(lane)),
        "the detail names who closed it, why, where it stood, and whose claim it took"
    );
    clean(&root, bus);
}

/// Unknown handles, blank validation and settled rows are refused, leaving
/// the row as it was.
#[test]
fn a_closure_without_validation_or_against_a_settled_task_is_refused() {
    let (root, bus, epoch, _) = fixture("close-refusals", 0);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-close0003",
        "Work to close once",
        Some("todo"),
    );
    let absent = wire::TaskStamp::for_micros(50_020).expect("fixture task stamp");
    let head = wire::Seq::new(1).expect("fixture sequence");
    let unknown = issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskClose,
        vec![
            Witness::Task {
                task: absent.clone(),
                head,
            },
            Witness::Workflow {
                task: absent.clone(),
                head,
                phase: "todo".parse().expect("fixture phase"),
            },
            Witness::Claim {
                task: absent.clone(),
                claimed: None,
                head: None,
            },
        ],
        &TaskClose {
            task: TaskHandle::for_topic(
                &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
                &absent,
            ),
            validation: "verified".to_owned(),
            force_claimed: false,
        },
    );
    assert_eq!(refused_as(&unknown).0, "task::unknown");
    assert_eq!(
        refused_as(&close(&bus, epoch, &task, " \n\t", false)).0,
        "task::close-validation-empty"
    );
    assert!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task)
            .workflow
            .body
            .status
            .is_pending()
    );
    assert_committed(close(&bus, epoch, &task, "verified", false));
    assert_eq!(
        refused_as(&close(&bus, epoch, &task, "verified again", false)).0,
        "task::settled"
    );

    let withdrawn = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-close0004",
        "Work withdrawn before closing",
        Some("todo"),
    );
    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&visible, &withdrawn);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDelete,
        transition_witnesses(heads),
        &TaskDelete {
            task: withdrawn.clone(),
            reason: "filed by mistake".to_owned(),
            force_claimed: false,
        },
    ));
    assert_eq!(
        refused_as(&close(&bus, epoch, &withdrawn, "verified", false)).0,
        "task::already-deleted"
    );
    assert_eq!(closures(&bus, &task).len(), 1);
    assert!(closures(&bus, &withdrawn).is_empty());
    clean(&root, bus);
}

/// Only the operator may close work it does not hold.
#[test]
fn a_lane_principal_cannot_close_a_task() {
    let epoch = uuid(1);
    let rejected = Command {
        v: WireVersion::CURRENT,
        id: uuid(3),
        epoch,
        kind: CommandKind::TaskClose,
        expected: Vec::new(),
        body: serde_json::json!({ "task": "BOARD-1kKRRsDPx", "validation": "done" }),
    }
    .admit(
        &Accepted {
            wire: WireVersion::CURRENT,
            actor: Actor::Lane(uuid(2)),
            epoch,
            release: "1.2.3".parse().expect("fixture release"),
        },
        &wire::principal::PrincipalRoles::default(),
    );
    let Err(wire::CommandRejection::Refusal(refusal)) = rejected else {
        panic!("a lane principal is refused before the door reads the board");
    };
    assert_eq!(refusal.reason(), "command::authority");
}
