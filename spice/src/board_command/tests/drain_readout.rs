use super::preparation::add_controller;
use super::room::design_standing::win_named;
use super::*;

mod completion;

fn seed_parked_board(seats: &[Seat], bus: &Bus, epoch: CanonicalUuid) {
    let held = todo_task(bus, epoch, "DRAIN-held0001", "Held by a peer");
    claim_task(bus, epoch, &seats[1], &Actor::Lane(seats[1].lane), &held);
    let stale = todo_task(bus, epoch, "DRAIN-stale001", "Lapsed peer claim");
    claim_task(bus, epoch, &seats[2], &Actor::Lane(seats[2].lane), &stale);
    lapse_claim(bus, epoch, &seats[2], &stale);
    let prerequisite = todo_task(bus, epoch, "DRAIN-first001", "Open prerequisite");
    let blocked = todo_task(bus, epoch, "DRAIN-block001", "Waiting on the prerequisite");
    let before = board(bus, epoch, Actor::TEST_PRINCIPAL);
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDepends,
        dependency_witnesses(&before, &blocked, &[&prerequisite], None, false),
        &TaskDepends {
            task: blocked,
            after: vec![prerequisite],
            remove: false,
        },
    ));
    let waiting = todo_task(bus, epoch, "DRAIN-wait0001", "Deferred row");
    let before = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&before, &waiting);
    let until = instant_after(current_instant().expect("clock"), 300).expect("future instant");
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDefer,
        vec![workflow_witness(heads), deferral_witness(heads)],
        &TaskDefer {
            task: waiting,
            wait: until,
        },
    ));
    add_draft_row(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAIN-oops0001",
        "Parked triage row",
    );
}

#[test]
fn next_read_classifies_peer_only_parked_work_without_board_rows() {
    let (root, bus, epoch, seats) = fixture("next-read-parked", 3);
    seed_parked_board(&seats, &bus, epoch);
    subscription::configure_subscription(&bus, epoch, &seats[0], false);

    let answer = next_board(&bus, epoch, Actor::Lane(seats[0].lane));
    assert!(
        answer.tasks.is_empty(),
        "next-work never projects board rows"
    );
    let Some(NextWorkState::Dry {
        board: counts,
        drain,
        ..
    }) = answer.next
    else {
        panic!("peer-only parked work must leave this lane dry")
    };
    assert!(!drain);
    assert_eq!(
        counts,
        NextWorkCounts {
            active: 1,
            stale: 1,
            blocked: 0,
            workflow_waiting: 1,
            designs: Default::default(),
            deferred: 1,
            rounds_waiting: Vec::new(),
            boarded_elsewhere: Vec::new(),
            unowned_topics: Vec::new(),
        },
        "a peer's claim on work this session did not board names no owner"
    );
    clean(&root, bus);
}

#[test]
fn ownerless_work_is_blocked_once_per_topic_without_blocking_held_claims() {
    let (root, bus, epoch, seats) = fixture("next-read-ownerless", 2);
    let held = todo_task(&bus, epoch, "DRAIN-held0002", "Held task");
    let holder = Actor::Lane(seats[1].lane);
    claim_task(&bus, epoch, &seats[1], &holder, &held);
    let first = todo_task(&bus, epoch, "DRAIN-free0001", "First unowned task");
    todo_task(&bus, epoch, "DRAIN-free0002", "Second unowned task");
    let before = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let topic = at(&before, &first)
        .statement
        .body
        .topic
        .path
        .clone()
        .expect("the fixture task is placed");
    for seat in &seats {
        subscription::configure_subscription(&bus, epoch, seat, false);
    }
    let answer = next_board(&bus, epoch, Actor::Lane(seats[0].lane));
    let Some(NextWorkState::Dry { board: counts, .. }) = answer.next else {
        panic!("no team listens to automatic work");
    };
    assert_eq!(counts.active, 1);
    assert_eq!(counts.blocked, 2);
    assert_eq!(counts.unowned_topics, vec![topic]);
    let held_answer = next_board(&bus, epoch, holder);
    let Some(NextWorkState::Active { task }) = held_answer.next else {
        panic!("lack of an automatic owner does not revoke a held claim");
    };
    assert_eq!(
        task.statement.body.born,
        at(&before, &held).statement.body.born
    );
    assert_eq!(task.ownership.unwrap().teams, Vec::new());
    clean(&root, bus);
}

#[test]
fn a_dry_lane_hears_who_holds_the_work_its_session_boarded() {
    let (root, bus, epoch, seats) = fixture("next-read-boarded", 3);
    declare_session(&bus, epoch, &seats[0], uuid(9_201), 201);
    seed_creation_transcript(&bus, seats[0].lane);
    let creator = Actor::Lane(seats[0].lane);
    let boarded = add(
        &bus,
        epoch,
        creator.clone(),
        "DRAIN-board001",
        "Boarded",
        Some("todo"),
    );
    let holder = Actor::Lane(seats[1].lane);
    claim_task(&bus, epoch, &seats[1], &holder, &boarded);
    let foreign = todo_task(&bus, epoch, "DRAIN-board003", "Boarded by the operator");
    claim_task(
        &bus,
        epoch,
        &seats[2],
        &Actor::Lane(seats[2].lane),
        &foreign,
    );
    let unclaimed = add(
        &bus,
        epoch,
        creator.clone(),
        "DRAIN-board002",
        "Waiting",
        Some("todo"),
    );
    subscription::configure_subscription(&bus, epoch, &seats[0], false);

    let answer = next_board(&bus, epoch, creator);
    let Some(NextWorkState::Dry { board: counts, .. }) = answer.next else {
        panic!("a steered lane stays dry")
    };
    assert_eq!(
        counts.boarded_elsewhere,
        [wire::intent::BoardedElsewhere {
            task: boarded,
            holder,
        }],
        "only claimed work this session boarded names its owner, not {unclaimed} or {foreign}"
    );
    clean(&root, bus);
}

#[test]
fn next_read_keeps_speculative_design_work_out_of_its_board_summary() {
    let (root, bus, epoch, seats) = fixture("next-read-design", 2);
    let design_born = stamp("DRAIN-design01");
    let design = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &design_born,
    );
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: design_born }],
        &TaskAdd {
            intent: Some(wire::TaskIntent::Design),
            topic: Some("/board/test".parse().expect("fixture topic")),
            term: vec!["Reach a decision".to_owned()],
            origin: Some(fixture_origin()),
            flow: Some("design".to_owned()),
            ..adding("Explore a Design", "This work keeps the fixture honest.")
        },
    ));
    let output = stamp("DRAIN-output01");
    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let controller = at(&current, &design);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![
            Witness::Absence {
                task: output.clone(),
            },
            statement_witness(controller),
            workflow_witness(controller),
        ],
        &TaskAdd {
            preparation: Some(design.clone()),
            topic: Some("/board/test".parse().expect("fixture topic")),
            term: vec!["The candidate is selected".to_owned()],
            origin: Some(fixture_origin()),
            ..adding("Candidate output", "This work keeps the fixture honest.")
        },
    ));
    let prerequisite = todo_task(&bus, epoch, "DRAIN-first002", "Executable prerequisite");
    let blocked = todo_task(&bus, epoch, "DRAIN-block002", "Ordinary waiting work");
    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDepends,
        dependency_witnesses(&current, &blocked, &[&prerequisite], None, false),
        &TaskDepends {
            task: blocked,
            after: vec![prerequisite],
            remove: false,
        },
    ));
    subscription::configure_subscription(&bus, epoch, &seats[0], false);

    let answer = next_board(&bus, epoch, Actor::Lane(seats[0].lane));
    let Some(NextWorkState::Dry { board: counts, .. }) = answer.next else {
        panic!("a steered lane stays dry")
    };
    assert_eq!(
        counts,
        NextWorkCounts {
            active: 0,
            stale: 0,
            blocked: 0,
            workflow_waiting: 1,
            designs: wire::rooms::DesignCycleCounts {
                gathering_input: 1,
                ..Default::default()
            },
            deferred: 0,
            rounds_waiting: Vec::new(),
            boarded_elsewhere: Vec::new(),
            unowned_topics: Vec::new(),
        },
        "the Design counts by its cycle, its Draft output is not execution work, and the \
         ordinary dependent waits neutrally"
    );
    clean(&root, bus);
}

/// A dry lane's Gathering count mixes a Design another lane already holds
/// with one nobody holds: both count once under the cycle.
#[test]
fn a_dry_gathering_count_mixes_held_and_unheld_designs() {
    let (root, bus, epoch, seats) = fixture("next-read-gathering-held", 2);
    let held = add_controller(&bus, epoch, "Held design", wire::TaskIntent::Design, None);
    let unheld = add_controller(&bus, epoch, "Unheld design", wire::TaskIntent::Design, None);
    wake_design(&bus, epoch, &held);
    wake_design(&bus, epoch, &unheld);
    assert_committed(win_named(&bus, epoch, &seats[1], &held, "capture"));
    subscription::configure_subscription(&bus, epoch, &seats[0], false);

    let answer = next_board(&bus, epoch, Actor::Lane(seats[0].lane));
    let Some(NextWorkState::Dry { board: counts, .. }) = answer.next else {
        panic!("a steered lane stays dry")
    };
    assert_eq!(
        counts.designs,
        wire::rooms::DesignCycleCounts {
            gathering_input: 2,
            ..Default::default()
        },
        "the held Gathering Design counts under its cycle beside the unheld one"
    );
    clean(&root, bus);
}

#[test]
fn next_read_returns_an_owned_claim_outside_allocator_routing() {
    let (root, bus, epoch, seats) = fixture("next-read-owned-unrouted", 2);
    let held = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAIN-own00001",
        "Continue owned work",
        None,
    );
    let actor = Actor::Lane(seats[0].lane);
    claim_task(&bus, epoch, &seats[0], &actor, &held);
    subscription::configure_subscription(&bus, epoch, &seats[0], false);

    let answer = next_board(&bus, epoch, actor);
    let Some(NextWorkState::Active { task }) = answer.next else {
        panic!("possession must precede allocator routing")
    };
    assert_eq!(
        task.statement.body.creation.creator_team(),
        None,
        "a held claim remains actionable when automatic allocation is disabled"
    );
    assert_eq!(
        TaskHandle::for_topic(&task.statement.body.topic.path, &task.statement.body.born),
        held
    );
    assert!(
        answer.tasks.is_empty(),
        "next-work never projects board rows"
    );
    clean(&root, bus);
}

#[test]
fn next_read_returns_one_typed_state_without_board_rows() {
    let (root, bus, epoch, seats) = fixture("task-next-bounded-read", 1);
    let task = todo_task(&bus, epoch, "DRAIN-next0001", "Bounded candidate");
    let actor = Actor::Lane(seats[0].lane);
    let Outcome::Reply(Reply {
        position,
        body: CommandAnswer::Board(answer),
    }) = issue(
        &bus,
        epoch,
        actor.clone(),
        CommandKind::BoardRead,
        Vec::new(),
        &BoardRead::Next,
    )
    else {
        panic!("next-work read does not return a board answer")
    };
    assert_eq!(position, ReplyPosition::Snapshot(answer.cursor));
    assert!(
        answer.tasks.is_empty(),
        "next-work never projects board rows"
    );
    let Some(NextWorkState::Candidate { task: selected }) = answer.next else {
        panic!("eligible work does not return the candidate state")
    };
    assert_eq!(
        TaskHandle::for_topic(
            &selected.statement.body.topic.path,
            &selected.statement.body.born
        ),
        task
    );

    claim_task(&bus, epoch, &seats[0], &actor, &task);
    let Outcome::Reply(Reply {
        body: CommandAnswer::Board(answer),
        ..
    }) = issue(
        &bus,
        epoch,
        actor.clone(),
        CommandKind::BoardRead,
        Vec::new(),
        &BoardRead::Next,
    )
    else {
        panic!("held next-work read does not return a board answer")
    };
    assert!(matches!(answer.next, Some(NextWorkState::Active { .. })));
    assert_eq!(
        bus.task_snapshot()
            .expect("claimed snapshot reads")
            .task_head_update_visits(),
        1,
        "claim win updates one live head"
    );

    free_active(&bus, epoch, &actor, &seats[0], &task);
    let released = bus.task_snapshot().expect("released snapshot reads");
    assert_eq!(
        released.task_head_update_visits(),
        1,
        "claim release updates one live head"
    );
    assert_eq!(released.allocation_frontier().count(), 1);
    let Outcome::Reply(Reply {
        body: CommandAnswer::Board(answer),
        ..
    }) = issue(
        &bus,
        epoch,
        actor.clone(),
        CommandKind::BoardRead,
        Vec::new(),
        &BoardRead::Next,
    )
    else {
        panic!("released next-work read does not return a board answer")
    };
    assert!(matches!(answer.next, Some(NextWorkState::Candidate { .. })));

    complete_and_delete_delta_heads(&bus, epoch, &seats[0], actor, task);
    clean(&root, bus);
}
fn complete_and_delete_delta_heads(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    actor: Actor,
    task: TaskHandle,
) {
    claim_task(bus, epoch, seat, &actor, &task);
    let held = board(bus, epoch, actor.clone());
    assert_committed(issue(
        bus,
        epoch,
        actor,
        CommandKind::TaskDone,
        progress_witnesses(&held, at(&held, &task), seat.lane, seat.occupant, false)
            .expect("held task has completion witnesses"),
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(&task, "delta frontier completion fixture")
        },
    ));
    let completed = bus.task_snapshot().expect("completed snapshot reads");
    assert_eq!(completed.task_head_update_visits(), 1);
    let born = task.born().expect("completed task identity");
    assert!(
        completed
            .allocation_frontier()
            .all(|heads| heads.statement().body().born != born),
        "completed work leaves the frontier from its changed head alone"
    );

    let withdrawn = todo_task(bus, epoch, "DRAIN-delete01", "Deleted work");
    let visible = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&visible, &withdrawn);
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDelete,
        transition_witnesses(heads),
        &TaskDelete {
            task: withdrawn,
            reason: "delta frontier deletion fixture".to_owned(),
            force_claimed: false,
        },
    ));
    let deleted = bus.task_snapshot().expect("deleted snapshot reads");
    assert_eq!(deleted.task_head_update_visits(), 1);
    assert_eq!(
        deleted.allocation_frontier().count(),
        0,
        "deleted work leaves the frontier from its changed head alone"
    );
}

#[test]
fn next_read_offers_an_advanced_task_in_its_next_phase() {
    let (root, bus, epoch, seats) = fixture("next-read-phase-advance", 2);
    let author = Actor::Lane(seats[0].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAIN-phase001",
        "Phased work",
        Some("todo,review"),
    );
    claim_task(&bus, epoch, &seats[0], &author, &task);
    let held = board(&bus, epoch, author.clone());
    assert_committed(issue(
        &bus,
        epoch,
        author,
        CommandKind::TaskDone,
        progress_witnesses(
            &held,
            at(&held, &task),
            seats[0].lane,
            seats[0].occupant,
            false,
        )
        .expect("held task has completion witnesses"),
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(&task, "phase advance fixture")
        },
    ));

    let answer = next_board(&bus, epoch, Actor::Lane(seats[1].lane));
    let Some(NextWorkState::Candidate { task: selected }) = answer.next else {
        panic!("an advanced task must stay allocatable in its next phase")
    };
    assert_eq!(
        TaskHandle::for_topic(
            &selected.statement.body.topic.path,
            &selected.statement.body.born
        ),
        task
    );
    assert_eq!(selected.workflow.body.phase.as_str(), "review");
    clean(&root, bus);
}
