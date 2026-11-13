use super::*;
use crate::board_command::routing::{Capture, Statement};
use wire::{Phase, topic::TopicDescribed};

fn describe(bus: &Bus, topic: &str, text: &str) {
    assert!(matches!(
        bus.commit(publication(topic, text)).unwrap(),
        CommitOutcome::Committed(_)
    ));
}

fn publication(topic: &str, text: &str) -> JournalWrite {
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "routing:contract".parse().unwrap(),
        "2026-11-06T00:00:00.000Z".parse().unwrap(),
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
    write
}

fn query(phase: &Phase) -> Statement<'_> {
    Statement::new("compiler parser", "", None, &[], phase)
}

fn path(topic: &wire::TaskTopic) -> Option<&str> {
    topic.path.as_ref().map(wire::Topic::as_str)
}

#[test]
fn routing_capture_uses_task_creator_scope_instead_of_the_invoking_operator() {
    let (root, bus, _, seats) = fixture("routing-creator", 1);
    fs::write(
        root.join("spice.toml"),
        "[team.default]\ndrive = [{ topic = [\"/storage/**\"] }]\ndrain = true\n",
    )
    .unwrap();
    let repository = Repository::discover(&root).unwrap();
    describe(&bus, "/compiler", "compiler parser");
    describe(&bus, "/storage", "filesystem disk");
    let snapshot = validated_snapshot(&bus, Some(&repository), Actor::TEST_PRINCIPAL).unwrap();
    let existing = Capture::new_for_team(
        &bus,
        &snapshot,
        Some(seats[0].team),
        query(&"todo".parse().unwrap()),
        None,
    )
    .unwrap();
    assert_eq!(
        path(&existing.restricted().unwrap().topic),
        None,
        "nothing the creator's team may drive explains the statement"
    );
    assert_eq!(
        path(&existing.unrestricted().unwrap().topic),
        Some("/compiler")
    );
    let operator = Capture::new(
        &bus,
        &snapshot,
        &Actor::TEST_PRINCIPAL,
        query(&"todo".parse().unwrap()),
        None,
    )
    .unwrap();
    assert_eq!(
        path(&operator.restricted().unwrap().topic),
        Some("/compiler")
    );
    assert_ne!(existing.fingerprint(), operator.fingerprint());
    clean(&root, bus);
}

#[test]
fn ordinary_routing_excludes_the_reserved_oops_subtree() {
    let (root, bus, _, _) = fixture("routing-reserved-oops", 1);
    let repository = Repository::discover(&root).unwrap();
    describe(&bus, "/oops", "compiler parser");
    describe(&bus, "/oops/design", "compiler parser syntax");
    describe(&bus, "/compiler", "compiler source");
    let snapshot = validated_snapshot(&bus, Some(&repository), Actor::TEST_PRINCIPAL).unwrap();
    let capture = Capture::new(
        &bus,
        &snapshot,
        &Actor::TEST_PRINCIPAL,
        query(&"todo".parse().unwrap()),
        None,
    )
    .unwrap();
    assert_eq!(
        path(&capture.restricted().unwrap().topic),
        Some("/compiler")
    );
    assert_eq!(
        path(&capture.unrestricted().unwrap().topic),
        Some("/compiler")
    );
    clean(&root, bus);
}

#[test]
fn routing_capture_refuses_changed_corpus_or_policy_without_partial_publication() {
    let (root, bus, _, _) = fixture("routing-publication", 1);
    let repository = Repository::discover(&root).unwrap();
    describe(&bus, "/compiler", "compiler parser");
    let snapshot = validated_snapshot(&bus, Some(&repository), Actor::TEST_PRINCIPAL).unwrap();
    let captured = Capture::new(
        &bus,
        &snapshot,
        &Actor::TEST_PRINCIPAL,
        query(&"todo".parse().unwrap()),
        None,
    )
    .unwrap();
    describe(&bus, "/compiler", "filesystem diagnostics");
    let before = bus.cursor().unwrap();
    let mut write = publication("/stale_corpus", "must not publish");
    captured.guard(&mut write);
    assert!(matches!(
        bus.commit(write).unwrap(),
        CommitOutcome::Refused(_)
    ));
    assert_eq!(bus.cursor().unwrap(), before);
    assert!(
        bus.read(|read| read.topic_node(&"/stale_corpus".parse().unwrap()))
            .unwrap()
            .is_none()
    );

    let snapshot = validated_snapshot(&bus, Some(&repository), Actor::TEST_PRINCIPAL).unwrap();
    let captured = Capture::new(
        &bus,
        &snapshot,
        &Actor::TEST_PRINCIPAL,
        query(&"todo".parse().unwrap()),
        None,
    )
    .unwrap();
    fs::write(root.join("spice.toml"), "[team.default]\ndrive = []\n").unwrap();
    let mut write = publication("/stale_policy", "must not publish");
    captured.guard(&mut write);
    assert!(matches!(
        bus.commit(write).unwrap(),
        CommitOutcome::Refused(_)
    ));
    assert_eq!(bus.cursor().unwrap(), before);
    assert!(
        bus.read(|read| read.topic_node(&"/stale_policy".parse().unwrap()))
            .unwrap()
            .is_none()
    );
    clean(&root, bus);
}

#[test]
fn routing_binds_exact_statement_and_origin_revision_even_when_scores_are_unchanged() {
    let (root, bus, epoch, seats) = fixture("routing-source-authority", 1);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    let add = authored_add(
        "Compiler parser origin",
        &seats[0].lane.to_string(),
        Some("todo"),
        &[],
    );
    let CommandAnswer::Task(parent) = assert_committed(issue(
        &bus,
        epoch,
        Actor::Lane(seats[0].lane),
        CommandKind::TaskAdd,
        vec![Witness::Absence {
            task: stamp("origin001"),
        }],
        &add,
    )) else {
        panic!("origin task creation returns a receipt")
    };
    let parent = parent.task;
    describe(&bus, "/compiler", "compiler parser");
    let provenance = wire::TaskProvenance::Task(parent.born().unwrap());
    let capture = |title: &str| {
        let snapshot = validated_snapshot(&bus, None, Actor::TEST_PRINCIPAL).unwrap();
        Capture::new(
            &bus,
            &snapshot,
            &Actor::TEST_PRINCIPAL,
            Statement::new(title, "", None, &[], &"todo".parse().unwrap()),
            super::super::routing::OriginView::of(&bus, &provenance).unwrap(),
        )
        .unwrap()
    };
    let first = capture("compiler parser");
    let respelled = capture("Compiler parser");
    assert_eq!(
        first.restricted().unwrap().route,
        respelled.restricted().unwrap().route
    );
    assert_ne!(first.fingerprint(), respelled.fingerprint());
    assert_committed(correct(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &TaskEdit {
            title: Some("Compiler parser origin!".to_owned()),
            ..only(&parent)
        },
    ));
    let before = bus.cursor().unwrap();
    let mut write = publication("/stale_origin", "must not publish");
    first.guard(&mut write);
    assert!(matches!(
        bus.commit(write).unwrap(),
        CommitOutcome::Refused(refusal) if refusal.reason() == "task::routing-origin-moved"
    ));
    assert_eq!(bus.cursor().unwrap(), before);
    let current = capture("compiler parser");
    assert_eq!(
        first.restricted().unwrap().route,
        current.restricted().unwrap().route
    );
    assert_ne!(first.fingerprint(), current.fingerprint());
    let mut write = publication("/fresh_origin", "may publish");
    current.guard(&mut write);
    assert!(matches!(
        bus.commit(write).unwrap(),
        CommitOutcome::Committed(_)
    ));
    clean(&root, bus);
}

#[test]
fn routing_uses_final_phase_and_drain_policy_before_leaving_work_unplaced() {
    let (root, bus, _, seats) = fixture("routing-final-admission", 1);
    fs::write(
        root.join("spice.toml"),
        "[team.default]\ndrive = [{ topic = [\"/engine/**\"], phase = [\"review\"] }]\ndrain = true\n",
    ).unwrap();
    let repository = Repository::discover(&root).unwrap();
    describe(&bus, "/engine/compiler", "compiler parser");
    let snapshot = validated_snapshot(&bus, Some(&repository), Actor::TEST_PRINCIPAL).unwrap();
    let review = Capture::new_for_team(
        &bus,
        &snapshot,
        Some(seats[0].team),
        query(&"review".parse().unwrap()),
        None,
    )
    .unwrap();
    assert_eq!(
        path(&review.restricted().unwrap().topic),
        Some("/engine/compiler")
    );
    let todo = Capture::new_for_team(
        &bus,
        &snapshot,
        Some(seats[0].team),
        query(&"todo".parse().unwrap()),
        None,
    )
    .unwrap();
    let undecided = todo.restricted().unwrap();
    assert_eq!(path(&undecided.topic), None);
    assert!(matches!(
        undecided.route,
        wire::intent::TaskRoute::Unavailable { ref reason, .. } if reason == "task::route-undecided"
    ));

    fs::write(
        root.join("spice.toml"),
        "[team.default]\ndrive = [{ topic = [\"/engine/**\"] }]\ndrain = [{ topic = [\"/other/**\"] }]\n",
    ).unwrap();
    let snapshot = validated_snapshot(&bus, Some(&repository), Actor::TEST_PRINCIPAL).unwrap();
    let undrainable = Capture::new_for_team(
        &bus,
        &snapshot,
        Some(seats[0].team),
        query(&"review".parse().unwrap()),
        None,
    )
    .unwrap();
    assert_eq!(path(&undrainable.restricted().unwrap().topic), None);
    clean(&root, bus);
}

#[test]
fn configured_unmatched_requires_exact_admission_and_departed_creators_stay_unplaced() {
    let (root, bus, _, seats) = fixture("routing-intake-authority", 1);
    let repository = Repository::discover(&root).unwrap();
    for unmatched in ["/engine", "/storage/intake"] {
        fs::write(
            root.join("spice.toml"),
            format!("[team.default]\ndrive = [{{ topic = [\"/engine/**\"] }}]\nunmatched = \"{unmatched}\"\n"),
        )
        .unwrap();
        let snapshot = validated_snapshot(&bus, Some(&repository), Actor::TEST_PRINCIPAL).unwrap();
        let inapplicable = Capture::new_for_team(
            &bus,
            &snapshot,
            Some(seats[0].team),
            query(&"todo".parse().unwrap()),
            None,
        )
        .unwrap();
        assert_eq!(path(&inapplicable.restricted().unwrap().topic), None);
    }

    fs::write(
        root.join("spice.toml"),
        "[team.default]\ndrive = [{ topic = [\"/engine/**\"] }]\nunmatched = \"/engine/intake\"\n",
    )
    .unwrap();
    let snapshot = validated_snapshot(&bus, Some(&repository), Actor::TEST_PRINCIPAL).unwrap();
    let configured = Capture::new_for_team(
        &bus,
        &snapshot,
        Some(seats[0].team),
        query(&"todo".parse().unwrap()),
        None,
    )
    .unwrap();
    assert_eq!(
        path(&configured.restricted().unwrap().topic),
        Some("/engine/intake")
    );

    fs::write(root.join("spice.toml"), "[team.default]\ndrive = []\n").unwrap();
    let snapshot = validated_snapshot(&bus, Some(&repository), Actor::TEST_PRINCIPAL).unwrap();
    let departed = Capture::new_for_team(
        &bus,
        &snapshot,
        Some(uuid(9_876_543)),
        query(&"todo".parse().unwrap()),
        None,
    )
    .unwrap();
    assert_eq!(path(&departed.restricted().unwrap().topic), None);
    let owners = snapshot.owners.owners_for(
        &departed.restricted().unwrap().topic,
        &"todo".parse().unwrap(),
        Some(uuid(9_876_543)),
    );
    assert_eq!(owners.teams, vec![seats[0].team]);
    assert!(!owners.reserved);
    clean(&root, bus);
}

#[test]
fn one_policy_unmatched_does_not_hide_a_shared_topic_from_another() {
    let (root, bus, _, _) = fixture("routing-unmatched-subtree", 1);
    fs::write(
        root.join("spice.toml"),
        "[team.default]\ndrive = true\ndrain = true\n\
         [team.generic]\ndrive = true\ndrain = true\nunmatched = \"/triage\"\n",
    )
    .unwrap();
    let repository = Repository::discover(&root).unwrap();
    describe(&bus, "/alpha", "storage");
    describe(&bus, "/beta", "frontend");
    describe(&bus, "/triage/coldleaf", "compiler parser");
    let snapshot = validated_snapshot(&bus, Some(&repository), Actor::TEST_PRINCIPAL).unwrap();
    let phase: Phase = "todo".parse().unwrap();

    let semantic = Capture::new(&bus, &snapshot, &Actor::TEST_PRINCIPAL, query(&phase), None)
        .unwrap()
        .restricted()
        .unwrap();
    assert_eq!(path(&semantic.topic), Some("/triage/coldleaf"));
    let wire::intent::TaskRoute::Pick { evidence, .. } = semantic.route else {
        panic!("the other policy can select the shared topic")
    };
    assert_eq!(evidence.stop, wire::intent::RouteStop::Leaf);
    assert!(evidence.overlap > 0);

    let tail: wire::TopicInput = "coldleaf/next".parse().unwrap();
    let structural = Capture::new(
        &bus,
        &snapshot,
        &Actor::TEST_PRINCIPAL,
        Statement::new("unrelated sentinel", "", None, &[], &phase).with_topic(Some(&tail)),
        None,
    )
    .unwrap()
    .restricted()
    .unwrap();
    assert_eq!(path(&structural.topic), Some("/triage/coldleaf/next"));
    let wire::intent::TaskRoute::Pick { evidence, .. } = structural.route else {
        panic!("the other policy can structurally select the shared topic")
    };
    assert_eq!(evidence.stop, wire::intent::RouteStop::Leaf);
    let structure = evidence.structure.expect("the authored tail is reported");
    assert_eq!(structure.matched.unwrap().as_str(), "/triage/coldleaf");
    assert_eq!(structure.matched_components, 1);
    clean(&root, bus);
}

#[test]
fn the_declaring_policy_never_routes_into_its_unmatched_descendants() {
    let (root, bus, _, _) = fixture("routing-unmatched-descendant", 1);
    fs::write(
        root.join("spice.toml"),
        "[team.default]\ndrive = true\ndrain = true\nunmatched = \"/triage\"\n",
    )
    .unwrap();
    let repository = Repository::discover(&root).unwrap();
    describe(&bus, "/alpha", "storage");
    describe(&bus, "/beta", "frontend");
    describe(&bus, "/triage/coldleaf", "compiler parser");
    let snapshot = validated_snapshot(&bus, Some(&repository), Actor::TEST_PRINCIPAL).unwrap();
    let phase: Phase = "todo".parse().unwrap();
    let routed = Capture::new(&bus, &snapshot, &Actor::TEST_PRINCIPAL, query(&phase), None)
        .unwrap()
        .restricted()
        .unwrap();
    assert_eq!(path(&routed.topic), Some("/triage"));
    assert!(routed.topic.unmatched);
    let wire::intent::TaskRoute::Pick { evidence, .. } = routed.route else {
        panic!("unresolved work still lands on the declared unmatched topic")
    };
    assert_eq!(evidence.stop, wire::intent::RouteStop::Unmatched);
    assert_eq!(evidence.overlap, 0);
    clean(&root, bus);
}

#[test]
fn routing_keeps_an_admitted_destination_when_its_owner_pauses_drain() {
    let (root, bus, epoch, seats) = fixture("routing-paused-drain", 1);
    let repository = Repository::discover(&root).unwrap();
    describe(&bus, "/compiler", "compiler parser");
    subscription::configure_subscription(&bus, epoch, &seats[0], false);
    let snapshot = validated_snapshot(&bus, Some(&repository), Actor::TEST_PRINCIPAL).unwrap();
    let captured = Capture::new_for_team(
        &bus,
        &snapshot,
        Some(seats[0].team),
        query(&"todo".parse().unwrap()),
        None,
    )
    .unwrap();
    let topic = captured.restricted().unwrap().topic;
    assert_eq!(path(&topic), Some("/compiler"));
    assert!(
        snapshot
            .owners
            .owners_for(&topic, &"todo".parse().unwrap(), Some(seats[0].team),)
            .teams
            .is_empty()
    );
    clean(&root, bus);
}
