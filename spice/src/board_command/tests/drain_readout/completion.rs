use super::*;

fn depend(bus: &Bus, epoch: CanonicalUuid, task: &TaskHandle, prerequisite: &TaskHandle) {
    let view = board(bus, epoch, Actor::TEST_PRINCIPAL);
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDepends,
        dependency_witnesses(&view, task, &[prerequisite], None, false),
        &TaskDepends {
            task: task.clone(),
            after: vec![prerequisite.clone()],
            remove: false,
        },
    ));
}

fn finish(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, task: &TaskHandle) {
    let actor = Actor::Lane(seat.lane);
    claim_task(bus, epoch, seat, &actor, task);
    let view = board(bus, epoch, actor.clone());
    assert_committed(issue(
        bus,
        epoch,
        actor,
        CommandKind::TaskDone,
        progress_witnesses(&view, at(&view, task), seat.lane, seat.occupant, false)
            .expect("completion witnesses"),
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(task, "Prerequisite completed")
        },
    ));
}

fn dry(bus: &Bus, epoch: CanonicalUuid, actor: Actor) -> NextWorkCounts {
    let answer = next_board(bus, epoch, actor);
    let Some(NextWorkState::Dry { board, .. }) = answer.next else {
        panic!("an arrived lane cannot fill another slot in its round")
    };
    board
}

#[test]
fn completed_dependencies_do_not_block_an_unmet_review_round() {
    let (root, bus, epoch, seats) = fixture("drain-completion-round", 6);
    let prerequisite = todo_task(&bus, epoch, "DRAIN-prereq01", "Prepare review");
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAIN-round001",
        "Review with one outstanding arrival",
        Some("review"),
    );
    depend(&bus, epoch, &task, &prerequisite);
    assert_committed(super::super::completion_target::set_target(
        &bus,
        epoch,
        &Actor::TEST_PRINCIPAL,
        &task,
        ("review", 7),
    ));
    finish(&bus, epoch, &seats[0], &prerequisite);
    for seat in &seats {
        claim_task(&bus, epoch, seat, &Actor::Lane(seat.lane), &task);
        assert_committed(super::super::preparation::review_clean(
            &bus, epoch, seat, &task,
        ));
    }
    let counts = dry(&bus, epoch, Actor::Lane(seats[0].lane));
    assert_eq!(counts.blocked, 0, "the prerequisite is durably completed");
    assert_eq!(counts.workflow_waiting, 0);
    assert_eq!(
        counts.rounds_waiting,
        [wire::intent::CompletionWait {
            task: task.clone(),
            phase: "review".parse().expect("review phase"),
            arrived: 6,
            target: 7,
            actor_arrived: true,
        }]
    );
    let waiting = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let waiting = at(&waiting, &task);
    assert!(waiting.workflow.body.status.is_pending());
    assert_eq!(waiting.completion.body.arrivals.len(), 6);
    assert!(waiting.claims.iter().all(|claim| !claim.is_active()));

    assert_committed(super::super::completion_target::set_target(
        &bus,
        epoch,
        &Actor::TEST_PRINCIPAL,
        &task,
        ("review", 6),
    ));
    let closed = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&closed, &task).workflow.body.status.is_completed());
    assert!(
        dry(&bus, epoch, Actor::Lane(seats[0].lane))
            .rounds_waiting
            .is_empty()
    );
    clean(&root, bus);
}

#[test]
fn completion_waits_do_not_mask_unfinished_dependencies() {
    let (root, bus, epoch, seats) = fixture("drain-completion-dependencies", 2);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAIN-round002",
        "A partially arrived round",
        Some("review"),
    );
    assert_committed(super::super::completion_target::set_target(
        &bus,
        epoch,
        &Actor::TEST_PRINCIPAL,
        &task,
        ("review", 3),
    ));
    claim_task(&bus, epoch, &seats[1], &Actor::Lane(seats[1].lane), &task);
    assert_committed(super::super::preparation::review_clean(
        &bus, epoch, &seats[1], &task,
    ));
    let actor = Actor::Lane(seats[0].lane);
    assert!(
        matches!(
            next_board(&bus, epoch, actor.clone()).next,
            Some(NextWorkState::Candidate { .. })
        ),
        "an unarrived lane still has ordinary capacity"
    );
    subscription::configure_subscription(&bus, epoch, &seats[0], false);
    let counts = dry(&bus, epoch, actor.clone());
    assert_eq!(
        counts.rounds_waiting,
        [wire::intent::CompletionWait {
            task: task.clone(),
            phase: "review".parse().expect("review phase"),
            arrived: 1,
            target: 3,
            actor_arrived: false,
        }]
    );

    let prerequisite = todo_task(&bus, epoch, "DRAIN-prereq02", "Deferred prerequisite");
    let dependent = todo_task(&bus, epoch, "DRAIN-depend02", "Wait on real work");
    depend(&bus, epoch, &dependent, &prerequisite);
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&view, &prerequisite);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDefer,
        vec![workflow_witness(heads), deferral_witness(heads)],
        &TaskDefer {
            task: prerequisite.clone(),
            wait: instant_after(current_instant().expect("clock"), 300).expect("defer until"),
        },
    ));
    let counts = dry(&bus, epoch, actor.clone());
    assert_eq!(counts.blocked, 1);
    assert_eq!(counts.deferred, 1);
    assert_eq!(counts.rounds_waiting[0].task, task);

    depend(&bus, epoch, &task, &prerequisite);
    let counts = dry(&bus, epoch, actor);
    assert_eq!(counts.blocked, 2);
    assert!(
        counts.rounds_waiting.is_empty(),
        "a real dependency takes precedence over an unmet round"
    );
    clean(&root, bus);
}
