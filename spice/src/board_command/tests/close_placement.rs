//! The completion that closes an unplaced task's first phase places it in the
//! same transaction, and a router that cannot decide refuses and moves nothing.

use super::*;
use wire::topic::TopicDescribed;

const UNMATCHED_PITCH: &str = "Quixotic vexation needs judgment.";

fn describe(bus: &Bus, topic: &str, text: &str) {
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "routing:contract".parse().unwrap(),
        current_instant().unwrap(),
        Producer::CommandDoor,
    );
    write
        .fact(
            TopicDescribed {
                topic: topic.parse().unwrap(),
                description: text.to_owned(),
            },
            FactCause::Root,
        )
        .unwrap();
    assert!(matches!(
        bus.commit(write).unwrap(),
        CommitOutcome::Committed(_)
    ));
}

/// A creation the router cannot place: no topic, or a relative tail nothing
/// on the empty corpus explains. It rests submitted in draft, the one phase
/// that may be unplaced, until the done that advances it places it.
fn unplaced_add(title: &str, tail: Option<&str>, flow: &str) -> TaskAdd {
    let mut add = authored_add(
        title,
        "/unused",
        Some(flow),
        &["the placed work is verified"],
    );
    add.pitch = UNMATCHED_PITCH.to_owned();
    add.topic = tail.map(|tail| tail.parse().expect("fixture tail"));
    add
}

fn create(bus: &Bus, epoch: CanonicalUuid, name: &str, add: &TaskAdd) -> TaskHandle {
    let CommandAnswer::Task(answer) = assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: stamp(name) }],
        add,
    )) else {
        panic!("creation returns a task receipt")
    };
    assert!(
        answer.task.as_str().starts_with("DRAFT-"),
        "{}",
        answer.task
    );
    assert!(
        answer
            .diagnostic
            .as_deref()
            .is_some_and(|diagnostic| diagnostic.contains("task::placement-required")),
        "an unroutable creation rests submitted in draft with the placement diagnostic: {:?}",
        answer.diagnostic
    );
    answer.task
}

fn add_unplaced(
    bus: &Bus,
    epoch: CanonicalUuid,
    name: &str,
    title: &str,
    tail: Option<&str>,
    flow: &str,
) -> TaskHandle {
    create(bus, epoch, name, &unplaced_add(title, tail, flow))
}

fn placed_handle(topic: &str, task: &TaskHandle) -> TaskHandle {
    TaskHandle::for_topic(
        &topic.parse::<wire::Topic>().expect("placed topic"),
        &task.born().expect("task stamp"),
    )
}

fn topic(view: &BoardAnswer, task: &TaskHandle) -> Option<String> {
    at(view, task)
        .statement
        .body
        .topic
        .path
        .as_ref()
        .map(ToString::to_string)
}

fn done(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    task: &TaskHandle,
    disposition: Option<wire::body::PlanDisposition>,
) -> Outcome<CommandAnswer> {
    let lane = Actor::Lane(seat.lane);
    let held = board(bus, epoch, lane.clone());
    issue(
        bus,
        epoch,
        lane,
        CommandKind::TaskDone,
        progress_witnesses(&held, at(&held, task), seat.lane, seat.occupant, false)
            .expect("the claimant has progress witnesses"),
        &TaskDone {
            task: task.clone(),
            validation: "the placed work is verified".to_owned(),
            disposition,
            operation: Some(completion_operation()),
        },
    )
}

fn review_clean(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    task: &TaskHandle,
) -> Outcome<CommandAnswer> {
    let lane = Actor::Lane(seat.lane);
    let view = board(bus, epoch, lane.clone());
    issue(
        bus,
        epoch,
        lane,
        CommandKind::TaskReview,
        progress_witnesses(&view, at(&view, task), seat.lane, seat.occupant, true)
            .expect("the reviewer has progress witnesses"),
        &verdict(task, "clean"),
    )
}

/// The undecided close refused naming both remedies and changed nothing.
fn assert_placement_required(
    bus: &Bus,
    epoch: CanonicalUuid,
    outcome: Outcome<CommandAnswer>,
    before: &BoardAnswer,
    task: &TaskHandle,
) {
    let Outcome::Refusal(refusal) = &outcome else {
        panic!("an undecided close refuses: {outcome:?}")
    };
    assert_eq!(refusal.reason(), "task::placement-required");
    let text = format!("{refusal:?}");
    for remedy in [
        format!("spice task place {task}"),
        format!("spice task modify {task} --topic"),
    ] {
        assert!(text.contains(&remedy), "the refusal names {remedy}: {text}");
    }
    let after = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let (before, after) = (at(before, task), at(&after, task));
    assert_eq!(after.statement.body.topic.path, None);
    assert_eq!(after.workflow, before.workflow, "flow and phase stay");
    assert_eq!(after.claims, before.claims, "every claim stays");
}

#[test]
fn a_sole_done_composes_the_stored_tail_under_the_router_prefix() {
    let (root, bus, epoch, seats) = fixture("close-placement-tail", 1);
    let task = add_unplaced(
        &bus,
        epoch,
        "close-tail",
        "Durable storage records",
        Some("render"),
        "todo,review",
    );
    let created = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(
        at(&created, &task)
            .statement
            .body
            .topic
            .tail()
            .map(|tail| tail.as_str()),
        Some("render"),
        "an undecided tail creation keeps the tail"
    );
    claim_task(&bus, epoch, &seats[0], &Actor::Lane(seats[0].lane), &task);
    describe(&bus, "/storage", "durable storage records");
    let CommandAnswer::Task(answer) = assert_committed(done(&bus, epoch, &seats[0], &task, None))
    else {
        panic!("done returns a task answer")
    };
    let placed = placed_handle("/storage/render", &task);
    assert_eq!(answer.task, placed, "the answer names the placed handle");
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let row = at(&view, &task);
    assert_eq!(
        row.statement
            .body
            .topic
            .path
            .as_ref()
            .map(wire::Topic::as_str),
        Some("/storage/render")
    );
    assert_eq!(
        row.statement
            .body
            .topic
            .placed
            .as_ref()
            .map(wire::Topic::as_str),
        Some("/storage")
    );
    assert_eq!(row.statement.body.handle, placed);
    assert_eq!(row.workflow.body.phase.as_str(), "todo");
    clean(&root, bus);
}

#[test]
fn an_undecided_done_refuses_until_the_router_can_place_the_statement() {
    let (root, bus, epoch, seats) = fixture("close-placement-undecided", 1);
    let task = add_unplaced(
        &bus,
        epoch,
        "close-undecided",
        "Durable storage records",
        None,
        "todo,review",
    );
    claim_task(&bus, epoch, &seats[0], &Actor::Lane(seats[0].lane), &task);
    let before = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let cursor = bus.cursor().unwrap();
    let refused = done(&bus, epoch, &seats[0], &task, None);
    assert_placement_required(&bus, epoch, refused, &before, &task);
    assert_eq!(
        bus.cursor().unwrap(),
        cursor,
        "a refused close publishes nothing"
    );

    describe(&bus, "/storage", "durable storage records");
    assert_committed(done(&bus, epoch, &seats[0], &task, None));
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(topic(&view, &task).as_deref(), Some("/storage"));
    assert_eq!(at(&view, &task).workflow.body.phase.as_str(), "todo");
    clean(&root, bus);
}

#[test]
fn a_draft_close_places_before_plan_and_carries_the_placed_claim() {
    let (root, bus, epoch, seats) = fixture("close-placement-plan", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add_unplaced(
        &bus,
        epoch,
        "close-plan",
        "Durable storage records",
        None,
        "plan,todo,review",
    );
    claim_task(&bus, epoch, &seats[0], &lane, &task);
    describe(&bus, "/storage", "durable storage records");
    let CommandAnswer::Task(answer) = assert_committed(done(&bus, epoch, &seats[0], &task, None))
    else {
        panic!("done returns a task answer")
    };
    let placed = placed_handle("/storage", &task);
    assert_eq!(answer.task, placed);
    let carried = answer
        .claim
        .as_ref()
        .expect("the plan close carries its claim");
    assert_eq!(
        carried.task, placed,
        "the carried claim names the placed handle"
    );
    assert_eq!(
        carried.phase.as_ref().map(|phase| phase.name.as_str()),
        Some("plan")
    );
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let row = at(&view, &task);
    assert_eq!(topic(&view, &task).as_deref(), Some("/storage"));
    assert_eq!(row.workflow.body.phase.as_str(), "plan");
    let claim = &row.latest_claim().expect("the carried claim").claim.body;
    assert_eq!(claim.disposition, ClaimDisposition::Active);
    assert_eq!(
        claim.cell.topic.as_ref().map(wire::Topic::as_str),
        Some("/storage"),
        "the carried claim records the placed topic"
    );

    // The next close is ordinary: the task is placed for the rest of its life.
    assert_committed(done(&bus, epoch, &seats[0], &task, None));
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&view, &task).workflow.body.phase.as_str(), "todo");
    assert_eq!(topic(&view, &task).as_deref(), Some("/storage"));
    clean(&root, bus);
}

/// A disposition settles or advances a Plan's own plan phase, never its
/// draft: the draft leaves through placement first, and the placed plan then
/// takes the disposition.
#[test]
fn a_disposition_never_leaves_draft_but_settles_the_placed_plan() {
    let (root, bus, epoch, seats) = fixture("close-placement-disposition", 1);
    let lane = Actor::Lane(seats[0].lane);
    let no_work = || Some(wire::body::PlanDisposition::NoWork {});
    let mut add = unplaced_add("Durable storage records", None, "plan,todo");
    add.intent = Some(wire::TaskIntent::Plan);
    let plan = create(&bus, epoch, "close-disposed", &add);
    claim_task(&bus, epoch, &seats[0], &lane, &plan);
    let before = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let refused = done(&bus, epoch, &seats[0], &plan, no_work());
    assert!(
        matches!(&refused, Outcome::Refusal(refusal) if refusal.reason() == "task::disposition-refused"),
        "{refused:?}"
    );
    let after = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&after, &plan).workflow, at(&before, &plan).workflow);

    describe(&bus, "/storage", "durable storage records");
    assert_committed(done(&bus, epoch, &seats[0], &plan, None));
    let placed = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(topic(&placed, &plan).as_deref(), Some("/storage"));
    assert_eq!(at(&placed, &plan).workflow.body.phase.as_str(), "plan");
    assert_committed(done(&bus, epoch, &seats[0], &plan, no_work()));
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(topic(&view, &plan).as_deref(), Some("/storage"));
    assert!(!at(&view, &plan).workflow.body.status.is_pending());
    clean(&root, bus);
}
#[test]
fn a_draft_entering_review_places_or_refuses_before_any_verdict() {
    let (root, bus, epoch, seats) = fixture("close-placement-review", 1);
    let task = add_unplaced(
        &bus,
        epoch,
        "close-review",
        "Durable storage records",
        None,
        "review",
    );
    claim_task(&bus, epoch, &seats[0], &Actor::Lane(seats[0].lane), &task);
    let before = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let refused = done(&bus, epoch, &seats[0], &task, None);
    assert_placement_required(&bus, epoch, refused, &before, &task);

    describe(&bus, "/storage", "durable storage records");
    assert_committed(done(&bus, epoch, &seats[0], &task, None));
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(topic(&view, &task).as_deref(), Some("/storage"));
    assert_eq!(at(&view, &task).workflow.body.phase.as_str(), "review");
    assert!(
        at(&view, &task)
            .latest_claim()
            .is_none_or(|held| { held.claim.body.disposition != ClaimDisposition::Active }),
        "entering review frees the author's claim"
    );
    claim_task(&bus, epoch, &seats[0], &Actor::Lane(seats[0].lane), &task);
    assert_committed(review_clean(&bus, epoch, &seats[0], &task));
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(!at(&view, &task).workflow.body.status.is_pending());
    clean(&root, bus);
}
