use super::*;
use wire::intent::{RouteConfidence, RouteStop, TaskPlace, TaskPlaceAnswer, TaskRoute};

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
fn pitch_routes_placement_without_overriding_an_explicit_topic() {
    let (root, bus, epoch, seats) = fixture("placement-pitch", 1);
    let seat = &seats[0];
    authorize_lane_creation(&bus, epoch, seat);
    describe(&bus, epoch, "/compiler", "compiler parser syntax");
    describe(&bus, epoch, "/storage", "durable storage records");

    let mut automatic = authored_add("Maintain neutral operation", "/unused", Some("todo"), &[]);
    automatic.pitch = "Compiler parser syntax needs ownership.".to_owned();
    automatic.topic = None;
    let automatic_born = stamp("pitch-routed-creation");
    let CommandAnswer::Task(automatic_answer) = assert_committed(issue(
        &bus,
        epoch,
        Actor::Lane(seat.lane),
        CommandKind::TaskAdd,
        vec![Witness::Absence {
            task: automatic_born,
        }],
        &automatic,
    )) else {
        panic!("automatic creation returns a task receipt")
    };
    let automatic_placement = automatic_answer
        .placement
        .as_ref()
        .expect("a blank creation routes at creation");
    assert_eq!(automatic_placement.topic.as_str(), "/compiler");
    assert!(
        automatic_answer.task.as_str().starts_with("DRAFT-"),
        "without acceptance the routed row rests submitted in draft: {}",
        automatic_answer.task
    );
    assert_eq!(
        topic_of(&bus, epoch, &automatic_answer.task).as_deref(),
        Some("/compiler")
    );
    let shown = preview(command(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &automatic_answer.task,
        None,
    ));
    assert_eq!(
        picked(&shown.route),
        "/compiler",
        "task place scores the saved statement pitch"
    );

    let mut explicit = automatic;
    explicit.topic = Some("/storage".parse().unwrap());
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
    assert_eq!(explicit_placement.topic.as_str(), "/storage");
    assert_eq!(
        picked(explicit_placement.router.as_ref().unwrap()),
        "/compiler",
        "the router is counterfactual when an explicit destination is named"
    );
    assert_eq!(
        picked(
            &preview(command(
                &bus,
                epoch,
                Actor::TEST_PRINCIPAL,
                &explicit_answer.task,
                None,
            ))
            .route
        ),
        "/compiler",
        "task place scores the saved statement pitch"
    );

    clean(&root, bus);
}

#[test]
fn preview_uses_creator_roots_and_apply_rejects_changed_corpus_without_advancing_a_draft() {
    let (root, bus, epoch, seats) = fixture("placement-draft", 1);
    fs::write(
        root.join("spice.toml"),
        "[team.default]\ndrive=[{topic=['/allowed']}]\n",
    )
    .unwrap();
    describe(&bus, epoch, "/allowed", "durable storage records");
    describe(&bus, epoch, "/preferred", "compiler parser syntax");
    let task = create(&bus, epoch, &seats[0], true);
    let before = bus.cursor().unwrap();
    let first = preview(command(&bus, epoch, Actor::TEST_PRINCIPAL, &task, None));
    assert_eq!(bus.cursor().unwrap(), before, "preview is read-only");
    assert_eq!(picked(&first.route), "/allowed");
    assert_eq!(picked(first.unrestricted.as_ref().unwrap()), "/preferred");
    assert_eq!(first.ownership.as_ref().unwrap().teams, vec![seats[0].team]);
    assert_eq!(first.phase, wire::Phase::Draft);

    describe(&bus, epoch, "/allowed", "durable storage journal records");
    let before = bus.cursor().unwrap();
    let stale = command(&bus, epoch, Actor::TEST_PRINCIPAL, &task, first.preview);
    assert!(matches!(stale, Outcome::Refusal(_)), "{stale:?}");
    assert_eq!(bus.cursor().unwrap(), before);
    let current = preview(command(&bus, epoch, Actor::TEST_PRINCIPAL, &task, None));
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
        Some("/allowed")
    );
    assert!(placed.workflow.body.is_draft());
    assert!(placed.claims.is_empty());
    clean(&root, bus);
}

#[test]
fn an_unfileable_routing_root_offers_no_placement_and_leaves_the_task_in_place() {
    let (root, bus, epoch, seats) = fixture("placement-filed-denial", 1);
    describe(&bus, epoch, "/allowed", "compiler parser syntax");
    let task = create(&bus, epoch, &seats[0], false);
    fs::write(
        root.join("spice.toml"),
        "[team.default]\ndrive=[{topic=['/allowed/**']}]\n",
    )
    .unwrap();
    let actor = Actor::Lane(seats[0].lane);
    let before = topic_of(&bus, epoch, &task);
    let shown = preview(command(&bus, epoch, actor, &task, None));
    assert!(undecided(&shown.route), "{:?}", shown.route);
    assert_eq!(shown.preview, None);
    assert_eq!(shown.phase, wire::Phase::Todo);
    assert_eq!(topic_of(&bus, epoch, &task), before);
    assert_eq!(before, Some(seats[0].lane.to_string()));
    clean(&root, bus);
}

fn sparse_add(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, born: &str) -> Outcome<CommandAnswer> {
    let mut add = authored_add(
        "Maintain neutral operation",
        "/unused",
        Some("todo"),
        &["neutral operation is maintained"],
    );
    add.pitch = "Nothing here names a subject.".to_owned();
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
    describe(&bus, epoch, "/compiler", "compiler parser syntax");
    describe(&bus, epoch, "/storage", "durable storage records");
    let CommandAnswer::Task(answer) =
        assert_committed(sparse_add(&bus, epoch, seat, "sparse-undeclared"))
    else {
        panic!("an undecided creation lands submitted in draft")
    };
    assert_eq!(answer.placement, None);
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
    let TaskRoute::Pick { topic, evidence } = &shown.route else {
        panic!("a declared unmatched topic receives unresolved work")
    };
    assert_eq!(topic.as_str(), "/triage");
    assert_eq!(evidence.stop, RouteStop::Unmatched);
    assert_eq!(evidence.confidence, RouteConfidence::Low);
    assert_eq!(evidence.overlap, 0);
    assert!(
        evidence.residual.contains(&"neutral".to_owned()),
        "{:?}",
        evidence.residual
    );
    let suffix = evidence
        .suffix
        .as_ref()
        .expect("residual terms recommend a suffix");
    assert_eq!(suffix.parent.as_str(), "/triage");
    assert_eq!(suffix.components, evidence.residual);
    assert!(suffix.topic_revision.is_some());
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

    let mut evidenced = authored_add("Parser syntax", "/unused", Some("todo"), &[]);
    evidenced.pitch = "Compiler parser syntax needs ownership.".to_owned();
    evidenced.topic = None;
    let CommandAnswer::Task(answer) = assert_committed(issue(
        &bus,
        epoch,
        Actor::Lane(seat.lane),
        CommandKind::TaskAdd,
        vec![Witness::Absence {
            task: stamp("evidenced-after-sparse"),
        }],
        &evidenced,
    )) else {
        panic!("evidenced creation returns a task receipt")
    };
    assert_eq!(
        answer
            .placement
            .as_ref()
            .map(|placement| placement.topic.as_str()),
        Some("/compiler"),
        "evidence routes creation"
    );
    let shown = preview(command(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &answer.task,
        None,
    ));
    let TaskRoute::Pick { topic, evidence } = &shown.route else {
        panic!("evidenced placement picks")
    };
    assert_eq!(topic.as_str(), "/compiler");
    assert_eq!(evidence.confidence, RouteConfidence::Evidenced);
    assert_eq!(evidence.stop, RouteStop::Leaf);
    assert!(evidence.overlap > 0);
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
    let mut add = authored_add("Maintain neutral operation", "/unused", Some("todo"), &[]);
    add.pitch = "Nothing here names a subject.".to_owned();
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
    assert_eq!(placement.topic.as_str(), "/triage/intake");
    let TaskRoute::Pick { evidence, .. } = placement.router.unwrap() else {
        panic!("a declared unmatched destination remains available")
    };
    assert_eq!(evidence.confidence, RouteConfidence::Low);
    assert_eq!(evidence.stop, RouteStop::Unmatched);
    assert_eq!(
        topic_of(&bus, epoch, &answer.task).as_deref(),
        Some("/triage/intake")
    );
    clean(&root, bus);
}

#[test]
fn tail_creation_recaptures_a_changed_corpus_without_changing_authored_input() {
    let (root, bus, epoch, seats) = fixture("placement-corpus-race", 1);
    let seat = &seats[0];
    authorize_lane_creation(&bus, epoch, seat);
    describe(&bus, epoch, "/compiler", "compiler parser syntax");
    describe(&bus, epoch, "/storage", "durable storage records");
    let mut add = authored_add("Compiler parser syntax", "/unused", Some("todo"), &[]);
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
            describe(&bus, epoch, "/compiler", "durable storage records");
            describe(&bus, epoch, "/storage", "compiler parser syntax");
        }))
    else {
        panic!("automatic creation returns a committed task")
    };
    assert_eq!(answer.placement.unwrap().topic.as_str(), "/storage/notes");
    let saved = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let saved = &at(&saved, &answer.task).statement.body;
    assert_eq!(saved.born, born);
    assert_eq!(saved.pitch, add.pitch);
    assert_eq!(
        saved.topic.path.as_ref().map(wire::Topic::as_str),
        Some("/storage/notes")
    );
    clean(&root, bus);
}

#[test]
fn an_undecided_tail_creation_stays_unplaced_and_placement_composes_the_tail() {
    let (root, bus, epoch, seats) = fixture("placement-stored-tail", 1);
    let seat = &seats[0];
    authorize_lane_creation(&bus, epoch, seat);
    describe(&bus, epoch, "/compiler", "compiler parser syntax");
    describe(&bus, epoch, "/storage", "durable storage records");
    let tail: wire::TopicInput = "render/css".parse().unwrap();
    let mut add = authored_add("Maintain neutral operation", "/unused", Some("todo"), &[]);
    add.pitch = "Nothing here names a subject.".to_owned();
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
    assert_eq!(
        created.placement, None,
        "an undecided tail leaves creation unplaced"
    );
    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let unplaced = &at(&visible, &created.task).statement.body.topic;
    assert_eq!(unplaced.path, None);
    assert_eq!(unplaced.named.as_ref(), Some(&tail));

    describe(&bus, epoch, "/storage", "neutral operation subject records");
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
