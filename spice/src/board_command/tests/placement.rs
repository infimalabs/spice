use super::*;
use wire::intent::{TaskPlace, TaskPlaceAnswer, TaskRoute};

fn picked(route: &TaskRoute) -> &str {
    match route {
        TaskRoute::Pick { topic, .. } => topic.as_str(),
        TaskRoute::Unavailable { reason, detail } => panic!("no pick: {reason} {detail}"),
    }
}

fn undecided(route: &TaskRoute) -> bool {
    matches!(route, TaskRoute::Unavailable { reason, .. } if reason == "task::route-undecided")
}

fn topic_of(bus: &Bus, epoch: CanonicalUuid, task: &TaskHandle) -> Option<String> {
    let visible = board(bus, epoch, Actor::TEST_PRINCIPAL);
    at(&visible, task)
        .statement
        .body
        .topic
        .path
        .as_ref()
        .map(ToString::to_string)
}

fn describe(bus: &Bus, epoch: CanonicalUuid, topic: &str, description: &str) {
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        format!("resident:{epoch}").parse().unwrap(),
        current_instant().unwrap(),
        Producer::CommandDoor,
    );
    write
        .fact(
            wire::topic::TopicDescribed {
                topic: topic.parse().unwrap(),
                description: description.to_owned(),
            },
            FactCause::Root,
        )
        .unwrap();
    assert!(matches!(
        bus.commit(write).unwrap(),
        CommitOutcome::Committed(_)
    ));
}

fn command(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    task: &TaskHandle,
    apply: Option<Digest>,
) -> Outcome<CommandAnswer> {
    let visible = board(bus, epoch, actor.clone());
    let heads = at(&visible, task);
    let request = TaskPlace {
        task: task.clone(),
        apply,
    };
    let command = fixture_command(
        epoch,
        actor,
        CommandKind::TaskPlace,
        vec![statement_witness(heads), workflow_witness(heads)],
        &request,
    );
    super::super::placement::run(bus, &command, &request, None)
}

fn preview(outcome: Outcome<CommandAnswer>) -> TaskPlaceAnswer {
    let Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(_),
        body: CommandAnswer::TaskPlace(answer),
    }) = outcome
    else {
        panic!("expected placement preview, got {outcome:?}")
    };
    *answer
}

fn create(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, draft: bool) -> TaskHandle {
    authorize_lane_creation(bus, epoch, seat);
    let mut add = authored_add(
        "compiler parser syntax",
        &seat.lane.to_string(),
        Some("todo"),
        &["the parser work is verified"],
    );
    add.pitch = "Make the parser reject malformed input".to_owned();
    add.draft = draft;
    let born = stamp("router-command-source");
    let CommandAnswer::Task(answer) = assert_committed(issue(
        bus,
        epoch,
        Actor::Lane(seat.lane),
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born }],
        &add,
    )) else {
        panic!("creation returns a task receipt")
    };
    answer.task
}

#[test]
fn an_explicit_topic_bypasses_forecast_even_when_public_roots_are_ambiguous() {
    let (root, bus, epoch, seats) = fixture("placement-pitch", 1);
    let seat = &seats[0];
    authorize_lane_creation(&bus, epoch, seat);
    describe(&bus, epoch, "/compiler", "compiler parser syntax");
    describe(&bus, epoch, "/storage", "durable storage records");

    let explicit = authored_add("Compiler parser syntax", "/storage", Some("todo"), &[]);
    let explicit_born = stamp("pitch-explicit-creation");
    let CommandAnswer::Task(explicit_answer) = assert_committed(issue(
        &bus,
        epoch,
        Actor::Lane(seat.lane),
        CommandKind::TaskAdd,
        vec![Witness::Absence {
            task: explicit_born,
        }],
        &explicit,
    )) else {
        panic!("explicit creation returns a task receipt")
    };
    let explicit_placement = explicit_answer
        .placement
        .expect("explicit creation describes its placement");
    assert_eq!(explicit_placement.topic.unwrap().as_str(), "/storage");
    assert_eq!(explicit_placement.router, None);

    clean(&root, bus);
}

#[test]
fn preview_uses_creator_roots_and_apply_rejects_changed_corpus_without_advancing_a_draft() {
    let (root, bus, epoch, seats) = fixture("placement-draft", 1);
    fs::write(
        root.join("spice.toml"),
        "[team.default]\ndrive=[{topic=['/parser']}]\n",
    )
    .unwrap();
    let task = create(&bus, epoch, &seats[0], true);
    let _tree = warm_topics(&root, &bus);
    let before = bus.cursor().unwrap();
    let first = preview(command(&bus, epoch, Actor::TEST_PRINCIPAL, &task, None));
    assert_eq!(bus.cursor().unwrap(), before, "preview is read-only");
    assert_eq!(picked(&first.route), "/parser");
    assert_eq!(first.ownership.as_ref().unwrap().teams, vec![seats[0].team]);
    assert_eq!(first.phase, wire::Phase::Draft);

    describe(&bus, epoch, "/allowed", "durable storage journal records");
    let before = bus.cursor().unwrap();
    let stale = command(&bus, epoch, Actor::TEST_PRINCIPAL, &task, first.preview);
    assert!(matches!(stale, Outcome::Refusal(_)), "{stale:?}");
    assert_eq!(bus.cursor().unwrap(), before);
    let current = preview(command(&bus, epoch, Actor::TEST_PRINCIPAL, &task, None));
    assert_eq!(picked(&current.route), "/parser");
    assert!(current.preview.is_some());
    let CommandAnswer::TaskPlace(applied) = assert_committed(command(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &task,
        current.preview,
    )) else {
        panic!("placement returns its committed receipt")
    };
    assert!(applied.applied);
    assert_eq!(applied.phase, wire::Phase::Draft);
    assert_eq!(
        applied.next,
        Some(format!("spice task file {}", applied.task))
    );
    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let placed = at(&visible, &applied.task);
    assert_eq!(
        placed
            .statement
            .body
            .topic
            .path
            .as_ref()
            .map(wire::Topic::as_str),
        Some("/parser")
    );
    assert!(placed.workflow.body.is_draft());
    assert!(placed.claims.is_empty());
    clean(&root, bus);
}

#[test]
fn a_proposal_the_creator_team_may_not_use_offers_no_placement_and_leaves_the_task_in_place() {
    let (root, bus, epoch, seats) = fixture("placement-filed-denial", 1);
    let task = create(&bus, epoch, &seats[0], false);
    fs::write(
        root.join("spice.toml"),
        "[team.default]\ndrive=[{topic=['/allowed/**']}]\n",
    )
    .unwrap();
    let actor = Actor::Lane(seats[0].lane);
    let _trees = warm_topics(&root, &bus);
    let before = topic_of(&bus, epoch, &task);
    let shown = preview(command(&bus, epoch, actor, &task, None));
    assert!(
        matches!(&shown.route, TaskRoute::Unavailable { reason, .. } if reason == "task::route-not-admitted"),
        "{:?}",
        shown.route
    );
    assert_eq!(shown.preview, None);
    assert_eq!(shown.phase, wire::Phase::Todo);
    assert_eq!(topic_of(&bus, epoch, &task), before);
    assert_eq!(before, Some(seats[0].lane.to_string()));
    clean(&root, bus);
}

/// The reserved topic's own word: the one statement no word can name.
const NAMELESS: &str = "Oops";

fn sparse_add(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, born: &str) -> Outcome<CommandAnswer> {
    let mut add = authored_add(NAMELESS, "/unused", Some("todo"), &[NAMELESS]);
    add.pitch = NAMELESS.to_owned();
    add.topic = None;
    issue(
        bus,
        epoch,
        Actor::Lane(seat.lane),
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: stamp(born) }],
        &add,
    )
}

#[test]
fn undecided_creation_rests_in_draft_until_task_place_lands_it_on_the_declared_unmatched_topic() {
    let (root, bus, epoch, seats) = fixture("placement-unmatched", 1);
    let seat = &seats[0];
    authorize_lane_creation(&bus, epoch, seat);
    let _trees = warm_topics(&root, &bus);
    let CommandAnswer::Task(answer) =
        assert_committed(sparse_add(&bus, epoch, seat, "sparse-undeclared"))
    else {
        panic!("an undecided creation lands submitted in draft")
    };
    assert!(
        answer
            .placement
            .as_ref()
            .and_then(|p| p.router.as_ref())
            .is_some_and(undecided)
    );
    assert!(
        answer
            .diagnostic
            .as_deref()
            .is_some_and(|diagnostic| diagnostic.contains("task::placement-required")),
        "{:?}",
        answer.diagnostic
    );
    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let unplaced = &at(&visible, &answer.task).statement.body.topic;
    assert_eq!((unplaced.path.as_ref(), unplaced.tail()), (None, None));
    let shown = preview(command(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &answer.task,
        None,
    ));
    assert!(undecided(&shown.route), "{:?}", shown.route);
    assert_eq!((shown.from, shown.preview), (None, None));

    fs::write(
        root.join("spice.toml"),
        "[team.default]\nunmatched = \"/triage\"\n",
    )
    .unwrap();
    let shown = preview(command(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &answer.task,
        None,
    ));
    let TaskRoute::Pick {
        topic,
        unmatched: true,
        ..
    } = &shown.route
    else {
        panic!("a declared unmatched topic receives unresolved work")
    };
    assert_eq!(topic.as_str(), "/triage");
    assert_committed(command(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &answer.task,
        shown.preview,
    ));
    assert_eq!(
        topic_of(&bus, epoch, &answer.task).as_deref(),
        Some("/triage")
    );

    clean(&root, bus);
}

#[test]
fn tail_creation_can_start_with_only_the_policy_declared_destination() {
    let (root, bus, epoch, seats) = fixture("placement-empty-corpus", 1);
    let seat = &seats[0];
    authorize_lane_creation(&bus, epoch, seat);
    fs::write(
        root.join("spice.toml"),
        "[team.default]\nunmatched = \"/triage\"\n",
    )
    .unwrap();
    let _trees = warm_topics(&root, &bus);
    let mut add = authored_add(NAMELESS, "/unused", Some("todo"), &[]);
    add.pitch = NAMELESS.to_owned();
    add.topic = Some("intake".parse().unwrap());
    let CommandAnswer::Task(answer) = assert_committed(issue(
        &bus,
        epoch,
        Actor::Lane(seat.lane),
        CommandKind::TaskAdd,
        vec![Witness::Absence {
            task: stamp("empty-corpus"),
        }],
        &add,
    )) else {
        panic!("tail creation returns a task receipt")
    };
    let placement = answer.placement.unwrap();
    assert_eq!(placement.topic.unwrap().as_str(), "/triage/intake");
    assert!(matches!(
        placement.router,
        Some(TaskRoute::Pick {
            unmatched: true,
            ..
        })
    ));
    assert_eq!(
        topic_of(&bus, epoch, &answer.task).as_deref(),
        Some("/triage/intake")
    );
    clean(&root, bus);
}

#[test]
fn tail_creation_recaptures_a_changed_topic_snapshot_without_changing_authored_input() {
    let (root, bus, epoch, seats) = fixture("placement-corpus-race", 1);
    let seat = &seats[0];
    authorize_lane_creation(&bus, epoch, seat);
    let _tree = warm_topics(&root, &bus);
    let mut add = authored_add("Compiler parser syntax", "/unused", Some("todo"), &[]);
    add.pitch = "Make the compiler reject malformed input".to_owned();
    add.topic = Some("notes".parse().unwrap());
    let born = stamp("automatic-corpus-race");
    let request = fixture_command(
        epoch,
        Actor::Lane(seat.lane),
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born.clone() }],
        &add,
    );
    let CommandAnswer::Task(answer) =
        assert_committed(mutation::task_add_with(&bus, &request, &add, None, || {
            describe(&bus, epoch, "/storage", "durable storage records");
        }))
    else {
        panic!("automatic creation returns a committed task")
    };
    let placement = answer.placement.unwrap();
    assert_eq!(
        placement.topic.as_ref().map(wire::Topic::as_str),
        Some("/compiler/notes")
    );
    assert!(matches!(
        placement.router,
        Some(TaskRoute::Pick {
            unmatched: false,
            ..
        })
    ));
    let saved = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let saved = &at(&saved, &answer.task).statement.body;
    assert_eq!(saved.born, born);
    assert_eq!(saved.pitch, add.pitch);
    assert_eq!(
        saved.topic.path.as_ref().map(wire::Topic::as_str),
        Some("/compiler/notes")
    );
    assert_eq!(saved.topic.named, add.topic);
    clean(&root, bus);
}

#[test]
fn an_undecided_tail_creation_stays_unplaced_and_placement_composes_the_tail() {
    let (root, bus, epoch, seats) = fixture("placement-stored-tail", 1);
    let seat = &seats[0];
    authorize_lane_creation(&bus, epoch, seat);
    let _trees = warm_topics(&root, &bus);
    let tail: wire::TopicInput = "render/css".parse().unwrap();
    let mut add = authored_add(NAMELESS, "/unused", Some("todo"), &[]);
    add.pitch = NAMELESS.to_owned();
    add.topic = Some(tail.clone());
    let CommandAnswer::Task(created) = assert_committed(issue(
        &bus,
        epoch,
        Actor::Lane(seat.lane),
        CommandKind::TaskAdd,
        vec![Witness::Absence {
            task: stamp("stored-tail"),
        }],
        &add,
    )) else {
        panic!("sparse creation returns a task receipt")
    };
    assert!(
        created
            .placement
            .as_ref()
            .and_then(|p| p.router.as_ref())
            .is_some_and(undecided)
    );
    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let unplaced = &at(&visible, &created.task).statement.body.topic;
    assert_eq!(unplaced.path, None);
    assert_eq!(unplaced.named.as_ref(), Some(&tail));

    fs::write(
        root.join("spice.toml"),
        "[team.default]\nunmatched = \"/storage\"\n",
    )
    .unwrap();
    let shown = preview(command(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &created.task,
        None,
    ));
    assert_eq!(picked(&shown.route), "/storage/render/css");
    let CommandAnswer::TaskPlace(applied) = assert_committed(command(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &created.task,
        shown.preview,
    )) else {
        panic!("placement returns its committed receipt")
    };
    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let placed = at(&visible, &applied.task);
    let topic = &placed.statement.body.topic;
    assert_eq!(
        topic.path.as_ref().map(wire::Topic::as_str),
        Some("/storage/render/css")
    );
    assert_eq!(
        topic.placed.as_ref().map(wire::Topic::as_str),
        Some("/storage")
    );
    assert_eq!(topic.named.as_ref(), Some(&tail));
    assert!(
        placed.workflow.body.is_submitted_draft(),
        "placement lands the row without advancing it"
    );
    clean(&root, bus);
}
