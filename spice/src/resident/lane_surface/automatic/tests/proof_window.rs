//! Starts whose session proof completes after other commits landed.

use super::super::super::propose::tests::seed_task_claim;
use super::*;

const OTHER_LANE: u64 = 40;
const OTHER_OCCUPANT: u64 = 41;
const DIRECTIVE: u64 = 43;
const LATE_DIRECTIVE: u64 = 44;

struct Window {
    fixture: LaunchFixture,
    bus: Bus,
    capabilities: serve::axum::Capabilities,
    discovery: serve::Discovery,
    seats: Seats,
}

impl Window {
    fn new() -> Self {
        let fixture = LaunchFixture::new();
        let bus = Bus::start(journal::Journal::in_memory().expect("fixture journal opens"))
            .expect("fixture bus starts");
        seed_two_lanes(&bus, &fixture);
        let discovery = serve::Discovery {
            endpoint: "http://127.0.0.1:1".to_owned(),
            capability: fixture.capability,
            epoch: fixture.epoch,
        };
        super::super::super::super::steering::publish_lane_surface(
            &fixture.paths,
            &discovery,
            fixture.lane,
            fixture.capability,
        )
        .expect("fixture lane surface publishes");
        Self {
            fixture,
            bus,
            capabilities: serve::axum::Capabilities::default(),
            discovery,
            seats: Seats::new(),
        }
    }

    /// Another lane starts and an unrelated task is claimed while proof runs.
    fn land_unrelated_commits(&self) {
        let observed = current_residency(&self.bus, self.fixture.epoch).expect("residency reads");
        let mut next = observed.body().clone();
        let other = next
            .lanes
            .iter_mut()
            .find(|row| row.lane == uuid(OTHER_LANE))
            .expect("other lane exists");
        other.occupant = Some(uuid(OTHER_OCCUPANT));
        other.lineage.push(uuid(OTHER_OCCUPANT));
        other.disposition = OCCUPIED.to_owned();
        let durable = other.clone();
        let mut write = write(
            Actor::Spice,
            self.fixture.epoch,
            Producer::ResidentRecognition,
        )
        .expect("other start write opens");
        guard_residency(&mut write, &observed);
        let started = write
            .fact(
                wire::body::LaneStarted {
                    lane: uuid(OTHER_LANE),
                    occupant: uuid(OTHER_OCCUPANT),
                    epoch: self.fixture.epoch,
                    group: 7,
                    fresh_session: true,
                },
                FactCause::Existing(observed.position()),
            )
            .expect("other start fact is valid");
        write.residency_head(started, move |_| next);
        commit(&self.bus, write).expect("other lane start commits");
        let unrelated: wire::TaskStamp = "1kQpV9Gwx".parse().expect("unrelated task");
        claim_task(&self.bus, &unrelated, &durable, self.fixture.epoch);
    }

    fn row(&self) -> wire::LaneResidency {
        current_residency(&self.bus, self.fixture.epoch)
            .expect("residency reads")
            .body()
            .lanes
            .iter()
            .find(|row| row.lane == self.fixture.lane)
            .cloned()
            .expect("fixture lane exists")
    }

    fn close(mut self) {
        self.seats.release_all();
        super::super::super::super::steering::remove_lane_surface(
            &self.fixture.paths,
            self.fixture.lane,
        );
        self.bus.verify().expect("proof window authority verifies");
        self.bus.shutdown().expect("fixture bus stops");
    }
}

#[test]
fn an_operator_wake_commits_after_unrelated_commits() {
    let mut window = Window::new();
    let request = LaneState {
        lane: window.fixture.lane,
        action: wire::intent::LaneAction::Wake,
        worktree: None,
    };
    let runtime = window
        .fixture
        .lane_runtime(&window.bus, &window.capabilities, &window.discovery);
    let launch = begin_wake(
        &window.bus,
        &window.fixture.paths,
        window.fixture.epoch,
        &window.seats,
        &runtime,
        &request,
    )
    .unwrap_or_else(|outcome| panic!("operator wake refused admission: {outcome:?}"));
    window.land_unrelated_commits();
    let occupant = launch.pending.occupant;
    let (launch, pending) = launch.into_parts();
    let outcome = finish_wake(
        &window.bus,
        &window.fixture.paths,
        window.fixture.epoch,
        &mut window.seats,
        &operator_wake(&window.fixture, &request),
        pending,
        launch.establish(),
    );
    assert!(matches!(outcome, Outcome::Reply(_)), "{outcome:?}");
    assert_eq!(window.row().occupant, Some(occupant));
    window.close();
}

#[test]
fn an_operator_wake_carries_the_claim_and_its_successor_lease_keeps_renewing() {
    let task: wire::TaskStamp = "1kQpV9Gtx".parse().expect("held task");
    let mut window = Window::new();
    let minted_expiry = claim_task_due(
        &window.bus,
        &task,
        &predecessor_row(&window),
        window.fixture.epoch,
    );
    let before = window.bus.task_snapshot().expect("predecessor claim reads");
    let before = before
        .tasks()
        .iter()
        .find(|heads| heads.statement().body().born == task)
        .expect("predecessor task exists");
    let predecessor = before
        .latest_claim()
        .map(|held| held.claim())
        .expect("predecessor claim exists")
        .clone();
    let predecessor_phase = before.workflow().body().phase;
    let request = LaneState {
        lane: window.fixture.lane,
        action: wire::intent::LaneAction::Wake,
        worktree: None,
    };
    let runtime = window
        .fixture
        .lane_runtime(&window.bus, &window.capabilities, &window.discovery);
    let launch = begin_wake(
        &window.bus,
        &window.fixture.paths,
        window.fixture.epoch,
        &window.seats,
        &runtime,
        &request,
    )
    .unwrap_or_else(|outcome| panic!("operator wake refused admission: {outcome:?}"));
    let successor = launch.pending.occupant;
    let (launch, pending) = launch.into_parts();
    let wake_started =
        wire::Instant::new(jiff::Timestamp::now()).expect("wake instant is canonical");
    let outcome = finish_wake(
        &window.bus,
        &window.fixture.paths,
        window.fixture.epoch,
        &mut window.seats,
        &operator_wake(&window.fixture, &request),
        pending,
        launch.establish(),
    );
    assert!(matches!(outcome, Outcome::Reply(_)), "{outcome:?}");

    let carried_snapshot = window.bus.task_snapshot().expect("carried claim reads");
    let residency = carried_snapshot
        .residency()
        .expect("carried residency exists");
    let carried = carried_snapshot
        .tasks()
        .iter()
        .find(|heads| heads.statement().body().born == task)
        .expect("carried task exists");
    let claim = carried
        .latest_claim()
        .map(|held| held.claim())
        .expect("carried claim exists");
    let lease = carried
        .latest_claim()
        .and_then(|held| held.lease())
        .expect("carried lease exists");
    assert_eq!(claim.body().occupant, successor);
    assert_eq!(claim.body().epoch, window.fixture.epoch);
    assert_eq!(lease.body().epoch, window.fixture.epoch);
    assert!(lease.body().renewal >= wake_started);
    let rolling_floor = wire::Instant::new(
        wake_started
            .get()
            .checked_add(jiff::SignedDuration::from_secs(
                crate::board_command::AUTOMATIC_ROLLING_SECONDS,
            ))
            .expect("rolling deadline shifts"),
    )
    .expect("rolling deadline is canonical");
    assert!(lease.body().expiry >= rolling_floor);
    assert!(lease.body().expiry > minted_expiry);
    let carried_expiry = lease.body().expiry;
    assert_eq!(claim.body().claimed, predecessor.body().claimed);
    assert_eq!(claim.body().cell, predecessor.body().cell);
    assert_eq!(claim.body().acknowledged, predecessor.body().acknowledged);
    assert_eq!(carried.workflow().body().phase, predecessor_phase);
    assert_eq!(claim.position(), lease.position());
    assert_eq!(claim.position(), residency.position());
    let facts = window
        .bus
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

    assert_successor_renews(&window, &task, successor, carried_expiry);
    window.close();
}

#[test]
fn a_failed_inbox_start_parks_only_the_steering_it_was_woken_for() {
    let window = Window::new();
    send_steering(&window, DIRECTIVE, "1kJnbzx1x");
    let woken = super::super::super::super::steering::woken_inbox(&window.bus, window.fixture.lane)
        .expect("woken steering reads");
    window.land_unrelated_commits();
    send_steering(&window, LATE_DIRECTIVE, "1kJnbzx2x");
    let failure = Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            "lane::session-authority-missing".to_owned(),
            None,
            None,
            None,
        )
        .with_detail("fixture proof failure"),
    ));

    let parked = super::super::super::super::steering::dead_letter_pending(
        &window.bus,
        &window.fixture.paths,
        window.fixture.epoch,
        window.fixture.lane,
        &woken,
        &failure,
    )
    .expect("woken steering parks despite unrelated commits");

    assert_eq!(
        parked
            .parked
            .iter()
            .map(|parked| parked.directive)
            .collect::<Vec<_>>(),
        [uuid(DIRECTIVE)]
    );
    let pending = window
        .bus
        .read({
            let lane = window.fixture.lane;
            move |read| crate::steering_read::pending(read, Some(lane))
        })
        .expect("pending steering reads");
    assert_eq!(
        pending
            .iter()
            .map(|head| head.body().directive)
            .collect::<Vec<_>>(),
        [uuid(LATE_DIRECTIVE)],
        "steering that arrived during proof stays pending for the next wake"
    );
    window.close();
}

fn seed_two_lanes(bus: &Bus, fixture: &LaunchFixture) {
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
    let row = |lane: CanonicalUuid, capabilities| wire::LaneResidency {
        lane,
        team: lane,
        name: None,
        worktree: fixture.root.display().to_string(),
        life: "drive".to_owned(),
        vendor: fixture.vendor.clone(),
        model: None,
        effort: None,
        fast_mode: false,
        occupant: None,
        lineage: Vec::new(),
        capabilities,
        session: None,
        disposition: VACANT.to_owned(),
    };
    let subscription = |team| wire::TeamSubscription {
        team,
        formed_at: None,
        drive: true,
        drain: false,
        name: None,
        fast_mode: false,
    };
    let head = wire::ResidencyHead {
        epoch: fixture.epoch,
        state: ResidentState::Seated,
        topology_revision: 1,
        lanes: vec![
            row(fixture.lane, vec![fixture.capability]),
            row(uuid(OTHER_LANE), Vec::new()),
        ],
        subscriptions: vec![subscription(fixture.lane), subscription(uuid(OTHER_LANE))],
        renewals: Vec::new(),
    };
    write.residency_head(seated, move |_| head);
    commit(bus, write).expect("fixture residency commits");
}

fn predecessor_row(window: &Window) -> wire::LaneResidency {
    let mut row = window.row();
    row.occupant = Some(window.fixture.occupant);
    row
}

fn claim_task(bus: &Bus, task: &wire::TaskStamp, row: &wire::LaneResidency, epoch: CanonicalUuid) {
    let now = wire::Instant::new(jiff::Timestamp::now()).expect("fixture clock is canonical");
    let shifted = |seconds| {
        wire::Instant::new(
            now.get()
                .checked_add(jiff::SignedDuration::from_secs(seconds))
                .expect("fixture instant shifts"),
        )
        .expect("fixture instant")
    };
    seed_task_claim(
        bus,
        task,
        row,
        epoch,
        shifted(-60),
        shifted(3600),
        shifted(7200),
    );
}

fn claim_task_due(
    bus: &Bus,
    task: &wire::TaskStamp,
    row: &wire::LaneResidency,
    epoch: CanonicalUuid,
) -> wire::Instant {
    let now = wire::Instant::new(jiff::Timestamp::now()).expect("fixture clock is canonical");
    let shifted = |seconds| {
        wire::Instant::new(
            now.get()
                .checked_add(jiff::SignedDuration::from_secs(seconds))
                .expect("fixture instant shifts"),
        )
        .expect("fixture instant")
    };
    let expiry = shifted(60);
    seed_task_claim(bus, task, row, epoch, shifted(-60), expiry, shifted(3600));
    expiry
}

fn assert_successor_renews(
    window: &Window,
    task: &wire::TaskStamp,
    successor: CanonicalUuid,
    carried_expiry: wire::Instant,
) {
    let mut timers = crate::resident::timers::Runtime::start(
        steer::Config::default(),
        window.fixture.paths.supervisors.clone(),
    )
    .expect("timer runtime starts");
    timers
        .synchronize(&window.bus)
        .expect("successor lease arms renewal");
    assert!(matches!(
        timers.consume_renewal_at_deadline(&window.bus, task),
        Ok(crate::resident::timers::Fired::ClaimRenew)
    ));
    let renewed = window.bus.task_snapshot().expect("renewed claim reads");
    let renewed = renewed
        .tasks()
        .iter()
        .find(|heads| heads.statement().body().born == *task)
        .expect("renewed task exists");
    assert_eq!(
        renewed
            .latest_claim()
            .map(|held| held.claim())
            .expect("renewed claim exists")
            .body()
            .occupant,
        successor
    );
    assert!(
        renewed
            .latest_claim()
            .and_then(|held| held.lease())
            .expect("renewed lease exists")
            .body()
            .expiry
            > carried_expiry,
        "the successor extends the promoted lease"
    );
}

fn send_steering(window: &Window, directive: u64, key: &str) {
    let epoch = window.fixture.epoch;
    let residency = current_residency(&window.bus, epoch).expect("residency reads");
    let at = wire::Instant::new(jiff::Timestamp::now()).expect("steering instant");
    let origin: wire::Origin = format!("resident:{epoch}")
        .parse()
        .expect("steering origin");
    let mut sent = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin.clone(),
        at,
        Producer::CommandDoor,
    );
    let body = wire::body::SteerSent {
        sender: Actor::TEST_PRINCIPAL,
        room: None,
        directive: uuid(directive),
        fanout: None,
        recipients: Vec::new(),
        key: key.to_owned(),
        recipient: Actor::Lane(window.fixture.lane),
        text: "deliver me".to_owned(),
        control: None,
        attachments: Vec::new(),
        expires: "2099-01-01T00:00:00.000Z".parse().expect("fixture expiry"),
    };
    let pending = sent
        .fact(body.clone(), FactCause::Existing(residency.position()))
        .expect("steering fact is valid");
    sent.steering_head(pending, move |seq| {
        steer::Ledger::sent_head(&body, origin, at, seq).expect("steering head is valid")
    });
    commit(&window.bus, sent).expect("pending steering commits");
}

fn operator_wake(fixture: &LaunchFixture, request: &LaneState) -> wire::AdmittedCommand {
    wire::Command {
        v: wire::WireVersion::CURRENT,
        id: uuid(45),
        epoch: fixture.epoch,
        kind: wire::CommandKind::LaneState,
        expected: Vec::new(),
        body: serde_json::to_value(request).expect("lane state request encodes"),
    }
    .admit(
        &wire::Accepted {
            wire: wire::WireVersion::CURRENT,
            actor: Actor::TEST_PRINCIPAL,
            epoch: fixture.epoch,
            release: "1.2.3".parse().expect("fixture release"),
        },
        &wire::principal::PrincipalRoles::default(),
    )
    .expect("operator wake command admits")
}

fn uuid(value: u64) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-{value:012x}")
        .parse()
        .expect("fixture UUID")
}
