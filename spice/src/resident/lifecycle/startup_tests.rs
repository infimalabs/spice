use super::super::lane_surface::LaunchFixture;
use super::*;

#[test]
fn resident_startup_leaves_a_vacant_drain_org_idle_without_work() {
    let fixture = LaunchFixture::new();
    let bus = Bus::start(journal::Journal::in_memory().expect("fixture journal opens"))
        .expect("fixture bus starts");
    seed_vacant_org(&bus, &fixture, true);
    let repository = Repository::discover(&fixture.root).expect("fixture repository resolves");
    let surface =
        super::super::load_startup_surface(&repository).expect("fixture startup policy resolves");
    let capabilities = serve::axum::Capabilities::default();
    let discovery = serve::Discovery {
        endpoint: "http://127.0.0.1:1".to_owned(),
        capability: fixture.capability,
        epoch: fixture.epoch,
    };
    let lane_runtime = fixture.lane_runtime(&bus, &capabilities, &discovery);
    let mut runtime = Runtime::new(&surface.config.lane);
    let mut launches = super::super::launch_runtime::Runtime::new();
    let mut seats = Seats::new();
    let mut diagnostic = Vec::new();

    let resident = Resident {
        bus: &bus,
        repository: &repository,
        paths: &fixture.paths,
        epoch: fixture.epoch,
        lane_runtime: &lane_runtime,
        config: &surface.config,
        profiles: None,
    };
    let deadline = runtime
        .reconcile(&resident, &mut seats, &mut launches, &mut diagnostic)
        .expect("Drain startup reconciles");

    assert_eq!(deadline, None);
    assert!(!seats.occupied(fixture.lane));
    assert!(diagnostic.is_empty());
    assert!(runtime.backoff.timed.is_empty());
    assert!(runtime.backoff.parked.is_empty());
    assert!(
        bus.task_snapshot()
            .expect("idle Drain snapshot reads")
            .tasks()
            .is_empty(),
        "idle reconciliation does not manufacture an automatic claim"
    );
    bus.verify().expect("Drain startup authority verifies");
    bus.shutdown().expect("fixture bus stops");
}

fn seed_vacant_org(bus: &Bus, fixture: &LaunchFixture, drain: bool) {
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
    let root = fixture.root.display().to_string();
    let vendor = fixture.vendor.clone();
    let epoch = fixture.epoch;
    let capability = fixture.capability;
    let team = fixture.lane;
    let lanes = vec![wire::LaneResidency {
        lane: team,
        team,
        name: None,
        worktree: root,
        life: "drive".to_owned(),
        vendor,
        model: None,
        effort: None,
        fast_mode: false,
        occupant: None,
        lineage: Vec::new(),
        capabilities: vec![capability],
        session: None,
        disposition: "vacant".to_owned(),
    }];
    proposal.residency_head(seated, move |_| wire::ResidencyHead {
        epoch,
        state: wire::ResidentState::Seated,
        topology_revision: 1,
        lanes,
        subscriptions: vec![wire::TeamSubscription {
            team,
            formed_at: None,
            drive: true,
            drain,
            name: None,
            fast_mode: false,
        }],
        renewals: Vec::new(),
    });
    commit(bus, proposal).expect("fixture residency commits");
}
