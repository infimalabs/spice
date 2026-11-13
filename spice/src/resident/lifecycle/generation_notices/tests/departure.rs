use super::*;
use crate::resident::team_departure;

fn standings(head: &wire::ResidencyHead) -> Vec<wire::body::TeamStanding> {
    head.subscriptions
        .iter()
        .map(|team| wire::body::TeamStanding {
            team: team.team,
            lanes: head
                .lanes
                .iter()
                .filter(|row| row.team == team.team)
                .map(|row| row.lane)
                .collect(),
        })
        .collect()
}

fn configured_fixture() -> (LaunchFixture, Bus, TaskStamp) {
    let fixture = LaunchFixture::new();
    std::fs::write(
        fixture.root.join("spice.toml"),
        "[team.default]\ndrive = []\n[team.authoring]\ndrive = true\n",
    )
    .unwrap();
    let (bus, task) = claimed_fixture_in_topic(
        &fixture,
        "/retained/claim".parse().expect("ordinary fixture topic"),
    );
    let snapshot = bus.task_snapshot().unwrap();
    let old = snapshot.residency().unwrap();
    let mut head = old.body().clone();
    let formation = uuid(41);
    head.lanes[0].team = formation;
    for id in [42, 43] {
        let mut peer = head.lanes[0].clone();
        peer.lane = uuid(id);
        peer.occupant = None;
        peer.lineage.clear();
        peer.capabilities.clear();
        peer.disposition = "vacant".into();
        head.lanes.push(peer);
    }
    head.subscriptions = vec![wire::TeamSubscription {
        team: formation,
        formed_at: Some(now()),
        drive: true,
        drain: true,
        name: Some("authoring".into()),
        fast_mode: false,
    }];
    head.topology_revision += 1;
    let mut write = JournalWrite::new(
        Actor::Spice,
        format!("resident:{}", fixture.epoch).parse().unwrap(),
        now(),
        Producer::ResidentRecognition,
    );
    let fact = write
        .fact(
            wire::body::TeamArranged {
                from: vec![wire::body::TeamStanding {
                    team: fixture.lane,
                    lanes: vec![fixture.lane],
                }],
                teams: standings(&head),
                sources: Vec::new(),
            },
            FactCause::Root,
        )
        .unwrap();
    write.residency_head(fact, move |_| head);
    committed(bus.commit(write).unwrap());
    (fixture, bus, task)
}

fn departure(bus: &Bus, fixture: &LaunchFixture) -> JournalWrite {
    let snapshot = bus.task_snapshot().unwrap();
    let before = snapshot.residency().unwrap().body();
    let mut after = before.clone();
    after.lanes[0].team = fixture.lane;
    after.subscriptions.push(wire::TeamSubscription {
        team: fixture.lane,
        formed_at: None,
        drive: true,
        drain: true,
        name: None,
        fast_mode: false,
    });
    after.topology_revision += 1;
    let mut write = JournalWrite::new(
        Actor::Spice,
        format!("resident:{}", fixture.epoch).parse().unwrap(),
        now(),
        Producer::ResidentRecognition,
    );
    let fact = write
        .fact(
            wire::body::TeamArranged {
                from: standings(before),
                teams: standings(&after),
                sources: Vec::new(),
            },
            FactCause::Root,
        )
        .unwrap();
    team_departure::append(
        bus,
        &fixture.paths.worktree,
        &mut write,
        before,
        &after,
        fact,
    )
    .unwrap();
    write.residency_head(fact, move |_| after);
    write
}

#[test]
fn departure_keeps_claims_and_delivers_one_durable_notice_across_reconciliation_replay() {
    let (fixture, bus, task) = configured_fixture();
    let before = bus.task_snapshot().unwrap();
    let claim = before.tasks()[0].latest_claim().unwrap().claim().clone();
    committed(bus.commit(departure(&bus, &fixture)).unwrap());
    let departed = bus.task_snapshot().unwrap();
    assert_eq!(departed.tasks()[0].latest_claim().unwrap().claim(), &claim);
    assert_eq!(
        departed.residency().unwrap().body().lanes[0].team,
        fixture.lane
    );
    let notices = departed
        .facts()
        .iter()
        .filter_map(|fact| fact.fact.read::<wire::body::TeamDepartureNoticed>())
        .collect::<Vec<_>>();
    assert_eq!(notices.len(), 1);
    assert_eq!(
        notices[0].tasks,
        [TaskHandle::for_topic(
            &departed.tasks()[0].statement().body().topic.path,
            &task
        )]
    );
    assert!(
        team_departure::reconcile(
            &bus,
            &fixture.paths,
            fixture.epoch,
            &departed,
            None,
            Duration::from_secs(3600)
        )
        .unwrap()
    );
    let delivered = bus.cursor().unwrap();
    let replay = bus.task_snapshot().unwrap();
    assert!(
        !team_departure::reconcile(
            &bus,
            &fixture.paths,
            fixture.epoch,
            &replay,
            None,
            Duration::from_secs(3600)
        )
        .unwrap()
    );
    assert_eq!(bus.cursor().unwrap(), delivered);
    let pending = bus
        .read(move |read| crate::steering_read::pending(read, Some(fixture.lane)))
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        bus.task_snapshot().unwrap().tasks()[0]
            .latest_claim()
            .unwrap()
            .claim(),
        &claim
    );
    finish(bus);
}

#[test]
fn departed_work_is_not_warned_when_new_policy_admits_it() {
    let (fixture, bus, _) = configured_fixture();
    std::fs::write(
        fixture.root.join("spice.toml"),
        "[team.default]\ndrive = true\n[team.authoring]\ndrive = true\n",
    )
    .unwrap();
    committed(bus.commit(departure(&bus, &fixture)).unwrap());
    assert!(!bus.task_snapshot().unwrap().facts().iter().any(|fact| {
        fact.fact
            .read::<wire::body::TeamDepartureNoticed>()
            .is_some()
    }));
    finish(bus);
}

#[test]
fn departure_authority_movement_refuses_topology_and_notice_together() {
    for changed in ["policy", "statement"] {
        let (fixture, bus, task) = configured_fixture();
        let proposal = departure(&bus, &fixture);
        match changed {
            "policy" => std::fs::write(
                fixture.root.join("spice.toml"),
                "[team.default]\ndrive = true\n",
            )
            .unwrap(),
            "statement" => {
                restate(&bus, &task, Actor::TEST_PRINCIPAL, Hold::CarriedThrough);
            }
            _ => unreachable!(),
        }
        let cursor = bus.cursor().unwrap();
        let outcome = bus.commit(proposal).unwrap();
        assert!(matches!(outcome, CommitOutcome::Refused(_)), "{outcome:?}");
        assert_eq!(bus.cursor().unwrap(), cursor);
        let snapshot = bus.task_snapshot().unwrap();
        assert_eq!(snapshot.residency().unwrap().body().lanes[0].team, uuid(41));
        assert!(!snapshot.facts().iter().any(|fact| {
            fact.fact
                .read::<wire::body::TeamDepartureNoticed>()
                .is_some()
        }));
        finish(bus);
    }
}
