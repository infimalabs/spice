use super::*;
use crate::board_command::routing::{Capture, Routed, Statement};
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

/// A statement no word can name: its only word is the reserved topic's.
fn nameless(phase: &Phase) -> Statement<'_> {
    Statement::new("Oops", "", None, &[], phase)
}

const SUBJECTS: [[&str; 4]; 3] = [
    [
        "Store the release signing key in the hardware vault",
        "Verify release signatures before the installer runs",
        "Publish the public half of the release signing key",
        "Record which signing key signed each release",
    ],
    [
        "Migrate the orders table to the partitioned schema",
        "Backfill the partitioned orders schema from the old table",
        "Add a rollback step to the orders schema migration",
        "Run the orders migration in batches to limit table locks",
    ],
    [
        "Let the dashboard switch between light and dark theme",
        "Persist the chosen dashboard theme per operator",
        "Fix contrast of chart labels in the dark dashboard theme",
        "Follow the system setting for the dashboard theme by default",
    ],
];
const ARRIVAL: &str = "Rotate the release signing key before it expires";

/// An activated statement whose pitch and acceptance term add no word of
/// their own.
fn hold(bus: &Bus, epoch: CanonicalUuid, actor: Actor, task: &str, title: &str, topic: &str) {
    let mut add = authored_add(title, topic, None, &[title]);
    add.pitch = title.to_owned();
    assert_committed(issue(
        bus,
        epoch,
        actor,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: stamp(task) }],
        &add,
    ));
}

/// Where the operator's signing-key statement is proposed on a board holding
/// the subjects above, each statement recorded wherever `recorded` says.
fn proposal(
    name: &str,
    recorded: impl Fn(usize) -> String,
    more: impl Fn(&Bus, CanonicalUuid, &Seat),
) -> Routed {
    let (root, bus, epoch, seats) = fixture(name, 1);
    let repository = Repository::discover(&root).unwrap();
    for (i, title) in SUBJECTS.as_flattened().iter().enumerate() {
        let (task, topic) = (format!("held-{i}"), recorded(i));
        hold(&bus, epoch, Actor::TEST_PRINCIPAL, &task, title, &topic);
    }
    more(&bus, epoch, &seats[0]);
    let _trees = warm_topics(&root, &bus);
    let snapshot = validated_snapshot(&bus, Some(&repository), Actor::TEST_PRINCIPAL).unwrap();
    let phase = Phase::Todo;
    let statement = Statement::new(ARRIVAL, ARRIVAL, None, &[], &phase);
    let routed = Capture::new(&bus, &snapshot, &Actor::TEST_PRINCIPAL, statement, None)
        .unwrap()
        .restricted()
        .unwrap();
    clean(&root, bus);
    routed
}

#[test]
fn a_route_waits_for_a_published_tree() {
    let (root, bus, _, _) = fixture("routing-cold-tree", 1);
    let repository = Repository::discover(&root).unwrap();
    let tree = topic_tree::TopicTree::shared(repository.admit_state().unwrap().data().to_owned());
    let snapshot = validated_snapshot(&bus, Some(&repository), Actor::TEST_PRINCIPAL).unwrap();
    let capture = || {
        Capture::new(
            &bus,
            &snapshot,
            &Actor::TEST_PRINCIPAL,
            query(&Phase::Todo),
            None,
        )
        .unwrap()
        .restricted()
        .unwrap()
    };
    assert!(
        matches!(capture().route, wire::intent::TaskRoute::Unavailable { reason, .. } if reason == "topic::preview-catching-up")
    );
    tree.update(&bus.task_snapshot().unwrap());
    assert_eq!(
        path(&capture().topic),
        Some("/compiler"),
        "an empty board starts itself from the statement's own heaviest word"
    );
    clean(&root, bus);
}

#[test]
fn recorded_topics_never_decide_where_a_statement_is_seated() {
    let misc = |_| "/misc".to_owned();
    let together = proposal("routing-pool-together", misc, |_, _, _| {});
    let first = path(&together.topic).and_then(|path| path.split('/').nth(1));
    assert!(
        first.is_some_and(|word| ["release", "signing", "key"].contains(&word)),
        "named by a word its group wrote: {:?}",
        together.route
    );
    let scattered = proposal(
        "routing-pool-scattered",
        |i| ["/alpha/one", "/beta", "/gamma/two/three"][i % 3].to_owned(),
        |_, _, _| {},
    );
    assert_eq!(scattered.route, together.route);
    let hidden = proposal("routing-pool-hidden", misc, |bus, epoch, seat| {
        authorize_lane_creation(bus, epoch, seat);
        let private = seat.lane.to_string();
        for j in 0..4 {
            let (task, title) = (format!("owned-{j}"), format!("{ARRIVAL} in vault {j}"));
            hold(bus, epoch, Actor::Lane(seat.lane), &task, &title, &private);
        }
    });
    assert_eq!(
        hidden.route, together.route,
        "private statements are never compared"
    );
}

#[test]
fn a_name_in_use_is_that_topic_and_is_never_numbered() {
    let misc = |_| "/misc".to_owned();
    let fresh = proposal("routing-name-fresh", misc, |_, _, _| {});
    let named = fresh.topic.path.clone().unwrap();
    assert!(fresh.topic.nodes.is_none_or(|nodes| nodes.node.is_none()));
    let taken = |i| if i == 5 { named.to_string() } else { misc(i) };
    let reused = proposal("routing-name-reused", taken, |_, _, _| {});
    assert_eq!(reused.topic.path.as_ref(), Some(&named));
    assert!(
        reused.topic.nodes.is_some_and(|nodes| nodes.node.is_some()),
        "an unrelated statement already filed there makes it the same topic"
    );
}

#[test]
fn a_statement_seated_with_a_described_topic_is_proposed_that_topic() {
    let charter = |text: &'static str| {
        let misc = |_| "/misc".to_owned();
        proposal("routing-fixed-point", misc, move |bus, _, _| {
            describe(bus, "/security/custody", text);
        })
    };
    let fixed = charter("Who may hold, rotate and publish a release signing key");
    assert_eq!(
        path(&fixed.topic),
        Some("/security/custody"),
        "{:?}",
        fixed.route
    );
    let moved = charter("Quarterly ledger of travel receipts");
    assert_ne!(
        path(&moved.topic),
        Some("/security/custody"),
        "a rewritten description stops attracting what the old one did"
    );
}

#[test]
fn competing_descriptions_choose_by_likeness_then_path() {
    let route = |name, first, second| {
        proposal(
            name,
            |_| "/misc".into(),
            |bus, _, _| {
                describe(bus, "/security/zeta", second);
                describe(bus, "/security/alpha", first);
            },
        )
    };
    // The longer charter shares one extra word (expires), but its other
    // subjects dilute the match to the release-key statement.
    let focused = "Rotate the release signing key";
    let broad = "Rotate the release signing key before it expires; publish custody audits, hardware vault inventory and quarterly incident reports";
    let unequal = route("routing-competing-descriptions", broad, focused);
    assert_eq!(path(&unequal.topic), Some("/security/zeta"));
    let equal = route("routing-tied-descriptions", focused, focused);
    assert_eq!(path(&equal.topic), Some("/security/alpha"));
}

#[test]
fn described_topics_keep_their_seats_beside_new_tasks() {
    let topic = "/security/custody";
    let latest = "latest-signing-task";
    let charter = "Who may hold, rotate and publish a release signing key";
    let routed = proposal(
        "routing-held-descriptions",
        |_| "/misc".into(),
        |bus, epoch, seat| {
            let mut write = publication(topic, charter);
            for i in 0..topic::CAPACITY {
                write
                    .fact(
                        TopicDescribed {
                            topic: format!("/charter-{i}").parse().unwrap(),
                            description: "Quarterly ledger of travel receipts".into(),
                        },
                        FactCause::Root,
                    )
                    .unwrap();
            }
            write
                .fact(
                    TopicDescribed {
                        topic: seat.lane.to_string().parse().unwrap(),
                        description: ARRIVAL.into(),
                    },
                    FactCause::Root,
                )
                .unwrap();
            assert!(matches!(
                bus.commit(write).unwrap(),
                CommitOutcome::Committed(_)
            ));
            hold(bus, epoch, Actor::TEST_PRINCIPAL, latest, ARRIVAL, "/misc");
            assert!(
                bus.task_snapshot()
                    .unwrap()
                    .described_topics()
                    .iter()
                    .all(|node| node.path.private_owner().is_none())
            );
        },
    );
    assert_eq!(path(&routed.topic), Some(topic));
    assert!(
        matches!(routed.route, wire::intent::TaskRoute::Pick { companions, .. } if companions.contains(&stamp(latest)))
    );
}

#[test]
fn a_proposal_below_the_teams_minimum_lead_stays_unplaced_with_its_reason() {
    let routed = |name: &str, minimum: u8, title: &str| {
        let (root, bus, epoch, _) = fixture(name, 1);
        fs::write(
            root.join("spice.toml"),
            format!("[team.default]\nplacement_min_lead = {minimum}\n"),
        )
        .unwrap();
        let repository = Repository::discover(&root).unwrap();
        for (i, held) in SUBJECTS.as_flattened().iter().enumerate() {
            let task = format!("held-{i}");
            hold(&bus, epoch, Actor::TEST_PRINCIPAL, &task, held, "/misc");
        }
        let _trees = warm_topics(&root, &bus);
        let snapshot = validated_snapshot(&bus, Some(&repository), Actor::TEST_PRINCIPAL).unwrap();
        let phase = Phase::Todo;
        let terms = [title.to_owned()];
        let statement = Statement::new(title, title, None, &terms, &phase);
        let routed = Capture::new(&bus, &snapshot, &Actor::TEST_PRINCIPAL, statement, None)
            .unwrap()
            .restricted()
            .unwrap();
        clean(&root, bus);
        routed
    };
    let held_back = |routed: &Routed| {
        routed.topic.path.is_none()
            && matches!(&routed.route, wire::intent::TaskRoute::Unavailable { reason, .. } if reason == "task::placement-weak-match")
    };
    let nearer = routed("routing-lead-nearer", 1, ARRIVAL);
    assert!(
        path(&nearer.topic).is_some(),
        "its seat holds its nearest neighbour: {:?}",
        nearer.route
    );
    let strict = routed("routing-lead-strict", 100, ARRIVAL);
    assert!(
        held_back(&strict),
        "only a restatement like nothing else leads by everything: {:?}",
        strict.route
    );
    let exact = routed("routing-lead-exact", 100, SUBJECTS[1][0]);
    assert!(path(&exact.topic).is_some(), "{:?}", exact.route);
    let alone = routed(
        "routing-lead-alone",
        1,
        "Calibrate greenhouse humidity sensors",
    );
    assert!(
        held_back(&alone),
        "a statement that joined no group has no seat to lead in: {:?}",
        alone.route
    );
}

#[test]
fn a_proposal_the_filer_may_not_use_is_reported_and_files_nothing() {
    let (root, bus, _, seats) = fixture("routing-creator", 1);
    fs::write(
        root.join("spice.toml"),
        "[team.default]\ndrive = [{ topic = [\"/storage\"] }]\ndrain = true\n",
    )
    .unwrap();
    let repository = Repository::discover(&root).unwrap();
    let _trees = warm_topics(&root, &bus);
    let snapshot = validated_snapshot(&bus, Some(&repository), Actor::TEST_PRINCIPAL).unwrap();
    let tail: wire::TopicInput = "syntax".parse().unwrap();
    let phase = Phase::Todo;
    let operator = Capture::new(
        &bus,
        &snapshot,
        &Actor::TEST_PRINCIPAL,
        query(&phase).with_topic(Some(&tail)),
        None,
    )
    .unwrap();
    assert_eq!(
        path(&operator.restricted().unwrap().topic),
        Some("/compiler/syntax")
    );
    let team = Capture::new_for_team(
        &bus,
        &snapshot,
        Some(seats[0].team),
        query(&phase).with_topic(Some(&tail)),
        None,
    )
    .unwrap();
    let refused = team.restricted().unwrap();
    assert_eq!(path(&refused.topic), None);
    assert!(
        matches!(
            &refused.route,
            wire::intent::TaskRoute::Unavailable { reason, detail, .. }
                if reason == "task::route-not-admitted" && detail.contains("/compiler/syntax")
        ),
        "{:?}",
        refused.route
    );
    assert_ne!(team.fingerprint(), operator.fingerprint());
    clean(&root, bus);
}

#[test]
fn ordinary_routing_excludes_the_reserved_oops_subtree() {
    let (root, bus, _, _) = fixture("routing-reserved-oops", 1);
    let repository = Repository::discover(&root).unwrap();
    describe(&bus, "/oops", "compiler parser");
    describe(&bus, "/oops/design", "compiler parser syntax");
    let _trees = warm_topics(&root, &bus);
    let snapshot = validated_snapshot(&bus, Some(&repository), Actor::TEST_PRINCIPAL).unwrap();
    let phase = Phase::Todo;
    let capture = |statement| {
        Capture::new(&bus, &snapshot, &Actor::TEST_PRINCIPAL, statement, None)
            .unwrap()
            .restricted()
            .unwrap()
    };
    assert_eq!(path(&capture(query(&phase)).topic), Some("/compiler"));
    assert_eq!(path(&capture(nameless(&phase)).topic), None);
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
        "[team.default]\ndrive = [{ topic = [\"/compiler\"], phase = [\"review\"] }]\ndrain = true\n",
    )
    .unwrap();
    let repository = Repository::discover(&root).unwrap();
    let _trees = warm_topics(&root, &bus);
    let snapshot = validated_snapshot(&bus, Some(&repository), Actor::TEST_PRINCIPAL).unwrap();
    let review = Capture::new_for_team(
        &bus,
        &snapshot,
        Some(seats[0].team),
        query(&"review".parse().unwrap()),
        None,
    )
    .unwrap();
    assert_eq!(path(&review.restricted().unwrap().topic), Some("/compiler"));
    let todo = Capture::new_for_team(
        &bus,
        &snapshot,
        Some(seats[0].team),
        query(&"todo".parse().unwrap()),
        None,
    )
    .unwrap();
    let refused = todo.restricted().unwrap();
    assert_eq!(path(&refused.topic), None);
    assert!(matches!(
        refused.route,
        wire::intent::TaskRoute::Unavailable { ref reason, .. } if reason == "task::route-not-admitted"
    ));

    fs::write(
        root.join("spice.toml"),
        "[team.default]\ndrive = [{ topic = [\"/compiler\"] }]\ndrain = [{ topic = [\"/other/**\"] }]\n",
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
    let _trees = warm_topics(&root, &bus);
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
            nameless(&"todo".parse().unwrap()),
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
    let configured = |statement| {
        Capture::new_for_team(&bus, &snapshot, Some(seats[0].team), statement, None)
            .unwrap()
            .restricted()
            .unwrap()
    };
    let phase = Phase::Todo;
    assert_eq!(
        path(&configured(nameless(&phase)).topic),
        Some("/engine/intake")
    );
    assert_eq!(
        path(&configured(query(&phase)).topic),
        None,
        "a proposal outside the team's topics is refused, not sent to its unmatched topic"
    );

    fs::write(root.join("spice.toml"), "[team.default]\ndrive = []\n").unwrap();
    let snapshot = validated_snapshot(&bus, Some(&repository), Actor::TEST_PRINCIPAL).unwrap();
    let departed = Capture::new_for_team(
        &bus,
        &snapshot,
        Some(uuid(9_876_543)),
        nameless(&"todo".parse().unwrap()),
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
fn a_proposal_at_the_unmatched_topic_is_filed_as_unmatched() {
    let (root, bus, _, _) = fixture("routing-unmatched-descendant", 1);
    fs::write(
        root.join("spice.toml"),
        "[team.default]\ndrive = true\ndrain = true\nunmatched = \"/compiler\"\n",
    )
    .unwrap();
    let repository = Repository::discover(&root).unwrap();
    let _trees = warm_topics(&root, &bus);
    let snapshot = validated_snapshot(&bus, Some(&repository), Actor::TEST_PRINCIPAL).unwrap();
    let phase: Phase = "todo".parse().unwrap();
    let routed = Capture::new(&bus, &snapshot, &Actor::TEST_PRINCIPAL, query(&phase), None)
        .unwrap()
        .restricted()
        .unwrap();
    assert_eq!(path(&routed.topic), Some("/compiler"));
    assert!(routed.topic.unmatched);
    clean(&root, bus);
}

#[test]
fn routing_keeps_an_admitted_destination_when_its_owner_pauses_drain() {
    let (root, bus, epoch, seats) = fixture("routing-paused-drain", 1);
    let repository = Repository::discover(&root).unwrap();
    describe(&bus, "/compiler", "compiler parser");
    subscription::configure_subscription(&bus, epoch, &seats[0], false);
    let _trees = warm_topics(&root, &bus);
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
