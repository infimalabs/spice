use super::super::launch::test_support::{LaunchFixture, SESSION};
use super::*;

mod proof_window;

/// The row's own life is the seed its initial team took, not a terminal
/// marker: automatic wake starts a vacant drain row exactly as a drive row.
#[test]
fn automatic_wake_starts_a_vacant_lane_whatever_its_row_life() {
    let fixture = LaunchFixture::new();
    let bus = Bus::start(journal::Journal::in_memory().expect("fixture journal opens"))
        .expect("fixture bus starts");
    seed_vacant_lane(&bus, &fixture, "drain");
    let capabilities = serve::axum::Capabilities::default();
    let discovery = serve::Discovery {
        endpoint: "http://127.0.0.1:1".to_owned(),
        capability: fixture.capability,
        epoch: fixture.epoch,
    };
    let runtime = fixture.lane_runtime(&bus, &capabilities, &discovery);
    let mut seats = Seats::new();

    let outcome = wake_automatic(
        &bus,
        &fixture.paths,
        fixture.epoch,
        &mut seats,
        &runtime,
        &WakeRequest {
            lane: fixture.lane,
            occupant: fixture.occupant,
            decision: None,
            reserved: None,
            inbox: false,
        },
        None,
    );

    assert!(
        matches!(
            outcome.expect("a vacant drain row starts"),
            AutomaticWake::Started
        ),
        "automatic wake starts a vacant drain row"
    );
    seats.release_all();
    bus.verify().expect("automatic start verifies");
    bus.shutdown().expect("fixture bus stops");
}

#[test]
fn automatic_wake_never_overlaps_an_authoritatively_occupied_lane() {
    let fixture = LaunchFixture::new();
    let bus = Bus::start(journal::Journal::in_memory().expect("fixture journal opens"))
        .expect("fixture bus starts");
    seed_vacant_lane(&bus, &fixture, "drive");
    let observed = current_residency(&bus, fixture.epoch).expect("vacant residency reads");
    let mut occupied = observed.body().clone();
    let row = occupied.lanes.first_mut().expect("fixture lane exists");
    row.occupant = Some(fixture.occupant);
    row.lineage.push(fixture.occupant);
    row.disposition = OCCUPIED.to_owned();
    let mut write = write(Actor::Spice, fixture.epoch, Producer::ResidentRecognition)
        .expect("occupancy write starts");
    guard_residency(&mut write, &observed);
    let started = write
        .fact(
            wire::body::LaneStarted {
                lane: fixture.lane,
                occupant: fixture.occupant,
                epoch: fixture.epoch,
                group: 42,
                fresh_session: true,
            },
            FactCause::Existing(observed.position()),
        )
        .expect("fixture start fact is valid");
    write.residency_head(started, move |_| occupied);
    commit(&bus, write).expect("occupied residency commits");
    let capabilities = serve::axum::Capabilities::default();
    let discovery = serve::Discovery {
        endpoint: "http://127.0.0.1:1".to_owned(),
        capability: fixture.capability,
        epoch: fixture.epoch,
    };
    let runtime = fixture.lane_runtime(&bus, &capabilities, &discovery);

    let outcome = wake_automatic(
        &bus,
        &fixture.paths,
        fixture.epoch,
        &mut Seats::new(),
        &runtime,
        &WakeRequest {
            lane: fixture.lane,
            occupant: fixture.occupant,
            decision: None,
            reserved: None,
            inbox: false,
        },
        None,
    );

    match outcome {
        Err(Outcome::Refusal(refusal)) => {
            assert_eq!(refusal.reason(), "lifecycle::wake-capacity-moved");
        }
        Err(Outcome::Fault(fault)) => panic!("occupied wake faulted: {fault:?}"),
        Err(Outcome::Reply(_)) | Ok(_) => panic!("occupied lane launched a successor"),
    }
    bus.verify().expect("occupied wake refusal verifies");
    bus.shutdown().expect("fixture bus stops");
}

#[test]
fn a_vacated_job_horizon_renews_the_claim_through_successor_carry() {
    let fixture = LaunchFixture::new();
    let bus = Bus::start(journal::Journal::in_memory().expect("fixture journal opens"))
        .expect("fixture bus starts");
    let predecessor: CanonicalUuid = "018f82d4-6f59-7d06-92e3-bce9131163af"
        .parse()
        .expect("fixture predecessor");
    let task: wire::TaskStamp = "1kKRRsDPx".parse().expect("fixture task");
    let vacant = seed_vacant_lane_with_lineage(&bus, &fixture, "drive", vec![predecessor]);
    let mut claimed = vacant.clone();
    claimed.occupant = Some(predecessor);
    claimed.disposition = OCCUPIED.to_owned();
    let renewal: wire::Instant = "2031-04-12T18:44:10.271Z".parse().expect("fixture instant");
    let start = shifted(renewal, -20);
    let initial_expiry = shifted(start, 60);
    let maximum = shifted(start, crate::board_command::AUTOMATIC_MAXIMUM_SECONDS);
    crate::resident::lane_surface::seed_task_claim(
        &bus,
        &task,
        &claimed,
        fixture.epoch,
        start,
        initial_expiry,
        maximum,
    );

    let delivered = shifted(renewal, 10);
    let bound = shifted(delivered, crate::board_command::AUTOMATIC_ROLLING_SECONDS);
    assert_eq!(
        crate::resident::timers::renew_vacated_claim_at(&bus, &task, renewal, delivered)
            .expect("vacated claim renews"),
        crate::resident::timers::Fired::ClaimRenew,
    );
    let renewed = bus.task_snapshot().expect("renewed claim reads");
    let renewed = renewed
        .tasks()
        .iter()
        .find(|heads| heads.statement().body().born == task)
        .and_then(|heads| heads.latest_claim()?.lease())
        .expect("renewed lease remains")
        .clone();
    assert_eq!(renewed.body().renewal, renewal);
    assert!(renewed.body().expiry > delivered);
    assert!(renewed.body().expiry <= bound);

    let capabilities = serve::axum::Capabilities::default();
    let discovery = serve::Discovery {
        endpoint: "http://127.0.0.1:1".to_owned(),
        capability: fixture.capability,
        epoch: fixture.epoch,
    };
    let runtime = fixture.lane_runtime(&bus, &capabilities, &discovery);
    let decision = bus.task_snapshot().expect("wake decision reads");
    let mut seats = Seats::new();
    assert!(matches!(
        wake_automatic(
            &bus,
            &fixture.paths,
            fixture.epoch,
            &mut seats,
            &runtime,
            &WakeRequest {
                lane: fixture.lane,
                occupant: fixture.occupant,
                decision: Some(&decision),
                reserved: None,
                inbox: false,
            },
            Some(delivered),
        )
        .expect("successor wake commits"),
        AutomaticWake::Started,
    ));

    let carried = bus.task_snapshot().expect("carried claim reads");
    let carried = carried
        .tasks()
        .iter()
        .find(|heads| heads.statement().body().born == task)
        .expect("carried task remains");
    let claim = carried
        .latest_claim()
        .map(|held| held.claim())
        .expect("carried claim remains");
    let lease = carried
        .latest_claim()
        .and_then(|held| held.lease())
        .expect("carried lease remains");
    assert_eq!(claim.body().occupant, fixture.occupant);
    assert_eq!(claim.position(), lease.position());
    assert!(lease.body().expiry >= renewed.body().expiry);
    let facts = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("carried facts read");
    let carried_fact = facts
        .facts()
        .iter()
        .find(|fact| fact.seq == claim.position())
        .expect("claim head names its carried fact");
    assert_eq!(carried_fact.fact.kind(), wire::FactKind::ClaimCarried);

    seats.release_all();
    bus.verify().expect("carried authority verifies");
    bus.shutdown().expect("fixture bus stops");
}

fn shifted(at: wire::Instant, seconds: i64) -> wire::Instant {
    wire::Instant::new(
        at.get()
            .checked_add(jiff::SignedDuration::from_secs(seconds))
            .expect("fixture instant shifts"),
    )
    .expect("shifted fixture instant is canonical")
}

fn seed_vacant_lane(bus: &Bus, fixture: &LaunchFixture, life: &str) {
    seed_vacant_lane_with_lineage(bus, fixture, life, Vec::new());
}

fn seed_vacant_lane_with_lineage(
    bus: &Bus,
    fixture: &LaunchFixture,
    life: &str,
    lineage: Vec<CanonicalUuid>,
) -> wire::LaneResidency {
    let worktree = fixture.root.display().to_string();
    let session = lineage.last().copied().map(|occupant| {
        let record = format!(
            r#"{{"id":"spice-state","type":"response","command":"get_state","success":true,"data":{{"sessionId":"{SESSION}"}}}}"#
        );
        let tape = tape::Tape::open(
            fixture
                .paths
                .lane_tape(fixture.lane)
                .join(fixture.vendor.as_str()),
        )
        .expect("fixture tape opens");
        let locus = tape
            .tee(record.as_bytes())
            .expect("fixture session declaration commits");
        wire::LaneSession {
            lane: fixture.lane,
            occupant,
            worktree: worktree.clone(),
            vendor: fixture.vendor.clone(),
            launch: wire::body::LaneLaunchIdentity::new("sonnet", "high"),
            epoch: fixture.epoch,
            stream: "stdout".to_owned(),
            identity: SESSION.to_owned(),
            locus: wire::TapeLocus {
                member: locus.member,
                offset: locus.offset,
                length: locus.length,
            },
        }
    });
    let row = wire::LaneResidency {
        lane: fixture.lane,
        team: fixture.lane,
        name: None,
        worktree,
        life: life.to_owned(),
        vendor: fixture.vendor.clone(),
        model: None,
        effort: None,
        fast_mode: false,
        occupant: None,
        lineage,
        capabilities: vec![fixture.capability],
        session,
        disposition: VACANT.to_owned(),
    };
    let mut write =
        write(Actor::Spice, fixture.epoch, Producer::CommandDoor).expect("fixture write starts");
    let seated = write
        .fact(
            wire::body::UpSeated {
                epoch: fixture.epoch,
            },
            FactCause::Root,
        )
        .expect("fixture resident seats");
    let epoch = fixture.epoch;
    let lane = fixture.lane;
    let durable = row.clone();
    write.residency_head(seated, move |_| wire::ResidencyHead {
        epoch,
        state: ResidentState::Seated,
        topology_revision: 1,
        lanes: vec![durable],
        subscriptions: vec![wire::TeamSubscription {
            team: lane,
            formed_at: None,
            drive: true,
            drain: false,
            name: None,
            fast_mode: false,
        }],
        renewals: Vec::new(),
    });
    commit(bus, write).expect("fixture residency commits");
    row
}

#[test]
fn startup_ready_promotes_expiry_and_preserves_a_one_hour_maximum() {
    let epoch = "018f82d4-6f59-7d06-92e3-bce9131163aa"
        .parse()
        .expect("fixture epoch");
    let prior_epoch = "018f82d4-6f59-7d06-92e3-bce9131163ab"
        .parse()
        .expect("fixture prior epoch");
    let start: wire::Instant = "2026-08-30T08:00:00.000Z".parse().expect("fixture start");
    let provisional = wire::Instant::new(
        start
            .get()
            .checked_add(jiff::SignedDuration::from_secs(
                crate::board_command::AUTOMATIC_PROVISIONAL_SECONDS,
            ))
            .expect("provisional deadline shifts"),
    )
    .expect("provisional deadline");
    let ready: wire::Instant = "2026-08-30T08:00:10.000Z"
        .parse()
        .expect("fixture ready time");
    let initial_maximum = wire::Instant::new(
        start
            .get()
            .checked_add(jiff::SignedDuration::from_secs(
                crate::board_command::AUTOMATIC_MAXIMUM_SECONDS,
            ))
            .expect("maximum deadline shifts"),
    )
    .expect("maximum deadline");
    let promoted = promoted_lease(
        &wire::LeaseHead {
            claimed: wire::Seq::new(1).expect("claim identity"),
            start,
            renewal: start,
            maximum: initial_maximum,
            expiry: provisional,
            epoch: prior_epoch,
        },
        epoch,
        ready,
        false,
    )
    .expect("startup promotion succeeds");

    assert_eq!(promoted.start, start);
    assert_eq!(promoted.renewal, ready);
    assert_eq!(promoted.epoch, epoch);
    assert_eq!(
        promoted.expiry,
        wire::Instant::new(
            ready
                .get()
                .checked_add(jiff::SignedDuration::from_secs(
                    crate::board_command::AUTOMATIC_ROLLING_SECONDS,
                ))
                .expect("rolling deadline shifts")
        )
        .expect("rolling deadline")
    );
    assert_eq!(
        promoted.maximum,
        wire::Instant::new(
            ready
                .get()
                .checked_add(jiff::SignedDuration::from_secs(
                    crate::board_command::AUTOMATIC_MAXIMUM_SECONDS,
                ))
                .expect("maximum deadline shifts")
        )
        .expect("maximum deadline")
    );
    assert!(promoted.expiry < promoted.maximum);

    let preserved = promoted_lease(
        &wire::LeaseHead {
            claimed: wire::Seq::new(1).expect("claim identity"),
            start,
            renewal: start,
            maximum: initial_maximum,
            expiry: initial_maximum,
            epoch: prior_epoch,
        },
        epoch,
        ready,
        false,
    )
    .expect("a longer existing lease remains valid");
    assert_eq!(preserved.expiry, initial_maximum);
}

#[test]
fn startup_ready_cannot_revive_an_expired_provisional_lease() {
    let epoch = "018f82d4-6f59-7d06-92e3-bce9131163aa"
        .parse()
        .expect("fixture epoch");
    let start: wire::Instant = "2026-08-30T08:00:00.000Z".parse().expect("fixture start");
    let expiry: wire::Instant = "2026-08-30T08:01:00.000Z".parse().expect("fixture expiry");
    let observed = wire::LeaseHead {
        claimed: wire::Seq::new(1).expect("claim identity"),
        start,
        renewal: start,
        maximum: expiry,
        expiry,
        epoch,
    };

    let refusal = promoted_lease(&observed, epoch, expiry, false)
        .expect_err("an expired provisional lease cannot be promoted");

    assert!(format!("{refusal:?}").contains("lifecycle::claim-promotion-expired"));
}

#[test]
fn thaw_replaces_the_indefinite_horizon_with_ordinary_automatic_bounds() {
    let epoch = "018f82d4-6f59-7d06-92e3-bce9131163aa"
        .parse()
        .expect("fixture epoch");
    let start: wire::Instant = "2026-08-30T08:00:00.000Z".parse().expect("fixture start");
    let ready: wire::Instant = "2026-08-30T08:05:00.000Z".parse().expect("fixture ready");
    let horizon = wire::Instant::indefinite();
    let thawed = promoted_lease(
        &wire::LeaseHead {
            claimed: wire::Seq::new(1).expect("claim identity"),
            start,
            renewal: start,
            expiry: horizon,
            maximum: horizon,
            epoch,
        },
        epoch,
        ready,
        true,
    )
    .expect("iced claim thaws");

    assert_eq!(thawed.start, start);
    assert_eq!(thawed.renewal, ready);
    assert!(!spice::claim_lease::is_indefinite(&thawed));
    assert_eq!(
        thawed.expiry,
        wire::Instant::new(
            ready
                .get()
                .checked_add(jiff::SignedDuration::from_secs(
                    crate::board_command::AUTOMATIC_ROLLING_SECONDS,
                ))
                .expect("rolling deadline shifts")
        )
        .expect("rolling deadline")
    );
    assert_eq!(
        thawed.maximum,
        wire::Instant::new(
            ready
                .get()
                .checked_add(jiff::SignedDuration::from_secs(
                    crate::board_command::AUTOMATIC_MAXIMUM_SECONDS,
                ))
                .expect("maximum deadline shifts")
        )
        .expect("maximum deadline")
    );
}
