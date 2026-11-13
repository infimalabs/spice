use std::time::Duration;

use lane::occupant::{DetachedProcess, DetachedWait};
use sill::{WaitError, WaitOutcome};

use super::super::launch::test_support::{LaunchFixture, SESSION};
use super::*;

struct RunningProcess;

impl DetachedProcess for RunningProcess {
    fn group(&self) -> u32 {
        77
    }

    fn termination_grace(&self) -> Duration {
        Duration::ZERO
    }

    fn reap_bound(&self) -> Duration {
        Duration::ZERO
    }

    fn wait_for(self: Box<Self>, _duration: Duration) -> Result<DetachedWait, WaitError> {
        Ok(DetachedWait::Running(self))
    }

    fn terminate(&self) -> Result<(), WaitError> {
        Ok(())
    }

    fn kill(&self) -> Result<(), WaitError> {
        Ok(())
    }

    fn wait(self: Box<Self>) -> Result<WaitOutcome, WaitError> {
        panic!("the command-only fixture never waits for its process")
    }
}

#[test]
fn request_duplicate_and_cancel_keep_one_pre_handoff_transition() {
    let fixture = LaunchFixture::new();
    let bus = seeded(&fixture, None, true);
    let discovery = publish_surface(&fixture);
    let mut seats = Seats::new();
    seats
        .seat_detached(fixture.lane, fixture.occupant, Box::new(RunningProcess))
        .expect("fixture predecessor seats");
    let renewal_request = request(&fixture, true);

    assert_invalid_requests(&fixture, &bus, &seats, renewal_request);
    assert_first_request(&fixture, &bus, &seats, renewal_request);
    assert_duplicate_request(&fixture, &bus, &seats, renewal_request);
    assert_cancellation(&fixture, &bus, &seats);

    seats.release_all();
    super::super::super::steering::remove_lane_surface(&fixture.paths, fixture.lane);
    drop(discovery);
    bus.verify().expect("renewal command authority verifies");
    bus.shutdown().expect("renewal command bus stops");
}

#[test]
fn predecessor_supervisor_failure_advances_requested_renewal_to_handoff() {
    let fixture = LaunchFixture::new();
    let bus = seeded(&fixture, Some(requested(&fixture, false)), true);

    supervisor_failed(
        &bus,
        fixture.epoch,
        fixture.lane,
        fixture.occupant,
        "detached supervisor ended",
    )
    .expect("terminal predecessor failure commits");
    let current = current_residency(&bus, fixture.epoch).expect("failed predecessor reads");
    assert_eq!(current.body().lanes[0].occupant, None);
    assert_eq!(current.body().lanes[0].disposition, DEGRADED);
    assert!(matches!(
        current.body().renewals[0].state,
        wire::LaneRenewalState::HandoffPending { .. }
    ));

    bus.verify().expect("failed predecessor authority verifies");
    bus.shutdown().expect("failed predecessor bus stops");
}

#[test]
fn vacant_request_enters_handoff_without_starting_or_notifying_the_predecessor() {
    let fixture = LaunchFixture::new();
    let bus = seeded(&fixture, None, false);
    publish_surface(&fixture);
    let seats = Seats::new();
    let request = request(&fixture, true);
    let outcome = set(
        &bus,
        &fixture.paths,
        fixture.epoch,
        &seats,
        &command(&fixture, 10, &request),
        &request,
    );
    assert!(matches!(outcome, Outcome::Reply(_)), "{outcome:?}");
    let current = current_residency(&bus, fixture.epoch).expect("renewal state reads");
    assert!(matches!(
        &current.body().renewals[0].state,
        wire::LaneRenewalState::HandoffPending { predecessor, team_slot: 0 }
            if predecessor.occupant == fixture.occupant && predecessor.session == SESSION
    ));
    assert_eq!(current.body().lanes[0].occupant, None);
    assert!(!seats.occupied(fixture.lane));
    assert!(controls(&bus, fixture.lane).is_empty());
    bus.verify().expect("vacant renewal authority verifies");
    bus.shutdown().expect("vacant renewal bus stops");
}

/// A marked replacement waits without creating steering that could wake it.
#[test]
fn a_pending_replacement_reconciles_without_starting_a_successor() {
    let fixture = LaunchFixture::new();
    let bus = seeded(&fixture, Some(requested(&fixture, false)), true);
    let discovery = publish_surface(&fixture);

    reconcile(&bus, &fixture.paths, fixture.epoch).expect("requested renewal reconciles");
    assert!(controls(&bus, fixture.lane).is_empty());

    supervisor_failed(
        &bus,
        fixture.epoch,
        fixture.lane,
        fixture.occupant,
        "detached supervisor ended",
    )
    .expect("terminal predecessor failure commits");
    reconcile(&bus, &fixture.paths, fixture.epoch).expect("pending renewal reconciles");

    let current = current_residency(&bus, fixture.epoch).expect("pending renewal reads");
    assert!(
        matches!(
            current.body().renewals[0].state,
            wire::LaneRenewalState::HandoffPending { .. }
        ),
        "reconciliation moved the mark instead of leaving it for the next start",
    );
    assert_eq!(current.body().lanes[0].occupant, None);
    assert!(
        controls(&bus, fixture.lane).is_empty(),
        "a directive standing on the vacant lane would wake it",
    );

    let withdraw = request(&fixture, false);
    let outcome = set(
        &bus,
        &fixture.paths,
        fixture.epoch,
        &Seats::new(),
        &command(&fixture, 12, &withdraw),
        &withdraw,
    );
    assert!(matches!(outcome, Outcome::Reply(_)), "{outcome:?}");
    assert!(
        current_residency(&bus, fixture.epoch)
            .expect("withdrawn renewal reads")
            .body()
            .renewals
            .is_empty(),
        "a mark no start has consumed stays withdrawable",
    );
    super::super::super::steering::remove_lane_surface(&fixture.paths, fixture.lane);

    drop(discovery);
    bus.verify().expect("pending renewal authority verifies");
    bus.shutdown().expect("pending renewal bus stops");
}

/// The start is what renews: it seats a fresh successor and records it.
#[test]
fn the_next_start_consumes_the_pending_replacement() {
    let fixture = LaunchFixture::new();
    let bus = seeded(&fixture, Some(requested(&fixture, true)), false);
    let discovery = publish_surface(&fixture);
    let capabilities = serve::axum::Capabilities::default();
    let runtime = fixture.lane_runtime(&bus, &capabilities, &discovery);
    let mut seats = Seats::new();
    let request = LaneState {
        lane: fixture.lane,
        action: LaneAction::Wake,
        worktree: None,
    };

    let outcome = super::super::state(
        &bus,
        &fixture.paths,
        fixture.epoch,
        &mut seats,
        &runtime,
        &wake_command(&fixture, 11, &request),
        &request,
    );
    assert!(matches!(outcome, Outcome::Reply(_)), "{outcome:?}");

    let current = current_residency(&bus, fixture.epoch).expect("started successor reads");
    assert!(
        matches!(
            &current.body().renewals[0].state,
            wire::LaneRenewalState::SuccessorStarted { predecessor, successor, .. }
                if predecessor.occupant == fixture.occupant
                    && successor.occupant == current.body().lanes[0]
                        .occupant
                        .expect("the started lane is occupied")
        ),
        "{:?}",
        current.body().renewals,
    );

    seats.release_all();
    super::super::super::steering::remove_lane_surface(&fixture.paths, fixture.lane);
    drop(discovery);
    bus.verify().expect("consumed renewal authority verifies");
    bus.shutdown().expect("consumed renewal bus stops");
}

fn assert_invalid_requests(
    fixture: &LaunchFixture,
    bus: &Bus,
    seats: &Seats,
    renewal_request: wire::intent::LaneRenewal,
) {
    let mut stale = renewal_request;
    stale.topology_revision = 0;
    assert!(matches!(
        set(
            bus,
            &fixture.paths,
            fixture.epoch,
            seats,
            &command(fixture, 8, &stale),
            &stale,
        ),
        Outcome::Refusal(refusal) if refusal.reason() == "lane::renewal-topology-moved"
    ));
    let mut moved = renewal_request;
    moved.team = fixture_uuid(99);
    assert!(matches!(
        set(
            bus,
            &fixture.paths,
            fixture.epoch,
            seats,
            &command(fixture, 9, &moved),
            &moved,
        ),
        Outcome::Refusal(refusal) if refusal.reason() == "lane::renewal-team-moved"
    ));
    assert!(controls(bus, fixture.lane).is_empty());
}

fn assert_first_request(
    fixture: &LaunchFixture,
    bus: &Bus,
    seats: &Seats,
    renewal_request: wire::intent::LaneRenewal,
) {
    let first = set(
        bus,
        &fixture.paths,
        fixture.epoch,
        seats,
        &command(fixture, 10, &renewal_request),
        &renewal_request,
    );
    assert!(matches!(first, Outcome::Reply(_)), "{first:?}");
    let head = current_residency(bus, fixture.epoch).expect("requested head reads");
    assert!(matches!(
        head.body().renewals[0].state,
        wire::LaneRenewalState::Requested { team_slot: 0, .. }
    ));
    assert!(controls(bus, fixture.lane).is_empty());
}

fn assert_duplicate_request(
    fixture: &LaunchFixture,
    bus: &Bus,
    seats: &Seats,
    renewal_request: wire::intent::LaneRenewal,
) {
    let before = bus.cursor().expect("requested cursor reads");
    let duplicate = set(
        bus,
        &fixture.paths,
        fixture.epoch,
        seats,
        &command(fixture, 11, &renewal_request),
        &renewal_request,
    );
    assert!(matches!(
        duplicate,
        Outcome::Refusal(refusal) if refusal.reason() == "lane::renewal-already-active"
    ));
    reconcile(bus, &fixture.paths, fixture.epoch).expect("requested renewal projects privately");
    assert_eq!(bus.cursor().expect("reconciled cursor reads"), before);
    assert!(controls(bus, fixture.lane).is_empty());
}

fn assert_cancellation(fixture: &LaunchFixture, bus: &Bus, seats: &Seats) {
    let clear = request(fixture, false);
    let canceled = set(
        bus,
        &fixture.paths,
        fixture.epoch,
        seats,
        &command(fixture, 12, &clear),
        &clear,
    );
    assert!(matches!(canceled, Outcome::Reply(_)), "{canceled:?}");
    assert!(
        current_residency(bus, fixture.epoch)
            .expect("cleared head reads")
            .body()
            .renewals
            .is_empty()
    );
    assert!(controls(bus, fixture.lane).is_empty());
}

fn request(fixture: &LaunchFixture, requested: bool) -> wire::intent::LaneRenewal {
    wire::intent::LaneRenewal {
        topology_revision: 1,
        team: fixture.lane,
        lane: fixture.lane,
        requested,
    }
}

fn command(
    fixture: &LaunchFixture,
    id: u64,
    request: &wire::intent::LaneRenewal,
) -> wire::AdmittedCommand {
    wire::Command {
        v: wire::WireVersion::CURRENT,
        id: fixture_uuid(id),
        epoch: fixture.epoch,
        kind: wire::CommandKind::LaneRenewal,
        expected: Vec::new(),
        body: serde_json::to_value(request).expect("renewal request encodes"),
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
    .expect("operator renewal command admits")
}

fn wake_command(fixture: &LaunchFixture, id: u64, request: &LaneState) -> wire::AdmittedCommand {
    wire::Command {
        v: wire::WireVersion::CURRENT,
        id: fixture_uuid(id),
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

fn requested(fixture: &LaunchFixture, handoff_pending: bool) -> wire::LaneRenewalRecord {
    let predecessor = wire::RenewalIdentity {
        occupant: fixture.occupant,
        session: SESSION.to_owned(),
        thread: SESSION.to_owned(),
    };
    wire::LaneRenewalRecord {
        team: fixture.lane,
        lane: fixture.lane,
        revision: 1,
        state: if handoff_pending {
            wire::LaneRenewalState::HandoffPending {
                predecessor,
                team_slot: 0,
            }
        } else {
            wire::LaneRenewalState::Requested {
                predecessor,
                team_slot: 0,
            }
        },
    }
}

fn seeded(
    fixture: &LaunchFixture,
    renewal: Option<wire::LaneRenewalRecord>,
    occupied: bool,
) -> Bus {
    let bus = Bus::start(journal::Journal::in_memory().expect("fixture journal opens"))
        .expect("fixture bus starts");
    let mut proposal =
        write(Actor::Spice, fixture.epoch, Producer::CommandDoor).expect("fixture write starts");
    let seated = proposal
        .fact(
            wire::body::UpSeated {
                epoch: fixture.epoch,
            },
            FactCause::Root,
        )
        .expect("fixture resident seats");
    let session = wire::LaneSession {
        lane: fixture.lane,
        occupant: fixture.occupant,
        worktree: fixture.root.display().to_string(),
        vendor: fixture.vendor.clone(),
        launch: wire::body::LaneLaunchIdentity::new("sonnet", "high"),
        epoch: fixture.epoch,
        stream: "stdout".to_owned(),
        identity: SESSION.to_owned(),
        locus: wire::TapeLocus {
            member: 0,
            offset: 0,
            length: 1,
        },
    };
    let lane = fixture.lane;
    let occupant = fixture.occupant;
    let capability = fixture.capability;
    let worktree = fixture.root.display().to_string();
    let vendor = fixture.vendor.clone();
    let epoch = fixture.epoch;
    proposal.residency_head(seated, move |_| wire::ResidencyHead {
        epoch,
        state: wire::ResidentState::Seated,
        topology_revision: 1,
        lanes: vec![wire::LaneResidency {
            lane,
            team: lane,
            name: None,
            worktree,
            life: "drive".to_owned(),
            vendor,
            model: None,
            effort: None,
            fast_mode: false,
            occupant: occupied.then_some(occupant),
            lineage: vec![occupant],
            capabilities: vec![capability],
            session: Some(session),
            disposition: if occupied { OCCUPIED } else { VACANT }.to_owned(),
        }],
        subscriptions: vec![wire::TeamSubscription {
            team: lane,
            formed_at: None,
            drive: true,
            drain: false,
            name: None,
            fast_mode: false,
        }],
        renewals: renewal.into_iter().collect(),
    });
    commit(&bus, proposal).expect("fixture residency commits");
    bus
}

fn publish_surface(fixture: &LaunchFixture) -> serve::Discovery {
    let discovery = serve::Discovery {
        endpoint: "http://127.0.0.1:1".to_owned(),
        capability: fixture.capability,
        epoch: fixture.epoch,
    };
    super::super::super::steering::publish_lane_surface(
        &fixture.paths,
        &discovery,
        fixture.lane,
        fixture.capability,
    )
    .expect("fixture lane surface publishes");
    discovery
}

fn controls(bus: &Bus, lane: CanonicalUuid) -> Vec<String> {
    bus.read(move |read| crate::steering_read::pending(read, Some(lane)))
        .expect("pending controls read")
        .iter()
        .filter_map(|head| {
            head.body()
                .control
                .as_deref()
                .map(|control| control.split(':').next().unwrap_or(control).to_owned())
        })
        .collect()
}

fn fixture_uuid(value: u64) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-{value:012x}")
        .parse()
        .expect("fixture UUID")
}
