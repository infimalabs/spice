use super::super::lane_surface::LaunchFixture;
use super::*;
use crate::resident::runtime::reap::account_exit;
use crate::resident::supervisor::test_support::record_exited_for_tests;
use crate::resident::supervisor::{Adoption, adopt};

const LOOSE_SESSION: &str = "018f7777-7777-7444-8888-999999999999";

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

#[test]
fn a_claimless_exit_with_loose_work_immediately_prepares_its_successor() {
    let fixture = LaunchFixture::new();
    prepare_loose_repository(&fixture);
    let bus = Bus::start(journal::Journal::in_memory().expect("fixture journal opens"))
        .expect("fixture bus starts");
    let session = seed_occupied_org(&bus, &fixture);
    record_exited_for_tests(&fixture.paths, fixture.lane, fixture.occupant, session)
        .expect("fixture supervisor exit publishes");
    let row = current_residency(&bus, fixture.epoch)
        .expect("occupied residency reads")
        .body()
        .lanes[0]
        .clone();
    let Some(Adoption::Exited(launch)) =
        adopt(&fixture.paths, &row).expect("published fixture exit adopts")
    else {
        panic!("published fixture exit was not adoptable");
    };
    let mut seats = Seats::new();
    seats
        .seat_detached(fixture.lane, fixture.occupant, launch.process)
        .expect("fixture occupant seats");

    let repository = Repository::discover(&fixture.root).expect("fixture repository resolves");
    let surface =
        super::super::load_startup_surface(&repository).expect("fixture startup policy resolves");
    let joining = crate::board_command::JoiningFixture::discover(&fixture.root);
    let mut runtime = Runtime::new(&surface.config.lane);
    account_exit(
        &bus,
        &fixture.paths,
        (&surface.config.steering, fixture.epoch, joining.joining()),
        &mut seats,
        &mut runtime,
        lane::capture::CaptureExit {
            lane: fixture.lane,
            occupant: fixture.occupant,
        },
        &mut Vec::new(),
    )
    .expect("claimless occupant exit commits");
    assert!(!seats.occupied(fixture.lane));

    let snapshot = bus.task_snapshot().expect("exit-committed snapshot reads");
    runtime
        .synchronize_holds(&snapshot, None)
        .expect("durable holds synchronize");
    let snapshot = runtime
        .advance_releasing_vendor_holds(&bus, fixture.epoch, snapshot)
        .expect("exit commit advances lifecycle projection");
    let (_, owners) = crate::board_command::ownership::capture(&repository, &snapshot)
        .expect("fixture ownership captures");
    let now = current_instant().expect("fixture instant reads");
    let mut targets = project_targets(
        &snapshot,
        &runtime.projection,
        now,
        &runtime.holds,
        &surface.config.allocation,
        None,
        &owners,
    )
    .expect("vacant target projects");
    runtime.loose.project(
        &mut targets,
        &snapshot,
        &surface.config.ambient,
        &mut Vec::new(),
    );
    assert_eq!(targets.len(), 1);
    assert!(targets[0].loose_worktree);
    assert_eq!(targets[0].held_claim, Available::None);
    assert_eq!(
        resolve(&targets, &Backoff::default(), now),
        Resolution::Attempt {
            trigger: Trigger::LooseWorktree,
            lane: fixture.lane,
        },
        "the exit commit alone makes the loose worktree due"
    );

    let capabilities = serve::axum::Capabilities::default();
    let discovery = serve::Discovery {
        endpoint: "http://127.0.0.1:1".to_owned(),
        capability: fixture.capability,
        epoch: fixture.epoch,
    };
    let lane_runtime = fixture.lane_runtime(&bus, &capabilities, &discovery);
    let resident = Resident {
        bus: &bus,
        repository: &repository,
        paths: &fixture.paths,
        epoch: fixture.epoch,
        lane_runtime: &lane_runtime,
        config: &surface.config,
        profiles: None,
    };
    let attempted = runtime
        .attempt(
            (Trigger::LooseWorktree, fixture.lane),
            false,
            &resident,
            &mut seats,
            &snapshot,
            &mut Vec::new(),
        )
        .expect("claimless loose-work wake prepares");
    assert!(
        matches!(attempted, Attempted::Launching(_)),
        "the ordinary claimless begin_work path prepares the successor"
    );
    assert!(
        snapshot.tasks().is_empty(),
        "loose work launches without manufacturing a task claim"
    );
    assert_eq!(runtime.held_until(fixture.lane), None);
    bus.verify().expect("loose-work authority verifies");
    bus.shutdown().expect("fixture bus stops");
}

fn prepare_loose_repository(fixture: &LaunchFixture) {
    std::fs::remove_dir_all(fixture.root.join(".git"))
        .expect("empty fixture Git directory removes");
    crate::test_repository::initialize(&fixture.root);
    crate::test_repository::stage_all(&fixture.root);
    let baseline = crate::test_repository::commit(&fixture.root, "baseline");
    std::fs::write(
        fixture.root.join(".git/refs/heads/baseline"),
        format!("{baseline}\n"),
    )
    .expect("baseline reference writes");
    let config =
        std::fs::read_to_string(fixture.root.join(".git/config")).expect("Git config reads");
    std::fs::write(
        fixture.root.join(".git/config"),
        format!("{config}\n[branch \"main\"]\n\tremote = .\n\tmerge = refs/heads/baseline\n"),
    )
    .expect("integration baseline config writes");
    std::fs::write(fixture.root.join("loose.txt"), "claimless work\n")
        .expect("loose fixture work writes");
}

fn seed_occupied_org(bus: &Bus, fixture: &LaunchFixture) -> wire::LaneSession {
    let record = format!(
        r#"{{"id":"spice-state","type":"response","command":"get_state","success":true,"data":{{"sessionId":"{LOOSE_SESSION}"}}}}"#
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
    let session = wire::LaneSession {
        lane: fixture.lane,
        occupant: fixture.occupant,
        worktree: fixture.root.display().to_string(),
        vendor: fixture.vendor.clone(),
        launch: wire::body::LaneLaunchIdentity::new("sonnet", "high"),
        epoch: fixture.epoch,
        stream: "stdout".to_owned(),
        identity: LOOSE_SESSION.to_owned(),
        locus: wire::TapeLocus {
            member: locus.member,
            offset: locus.offset,
            length: locus.length,
        },
    };
    let row = wire::LaneResidency {
        lane: fixture.lane,
        team: fixture.lane,
        name: None,
        worktree: fixture.root.display().to_string(),
        life: "drive".to_owned(),
        vendor: fixture.vendor.clone(),
        model: None,
        effort: None,
        fast_mode: false,
        occupant: Some(fixture.occupant),
        lineage: vec![fixture.occupant],
        capabilities: vec![fixture.capability],
        session: Some(session.clone()),
        disposition: "occupied".to_owned(),
    };
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
    let epoch = fixture.epoch;
    let team = fixture.lane;
    proposal.residency_head(seated, move |_| wire::ResidencyHead {
        epoch,
        state: wire::ResidentState::Seated,
        topology_revision: 1,
        lanes: vec![row],
        subscriptions: vec![wire::TeamSubscription {
            team,
            formed_at: None,
            drive: true,
            drain: false,
            name: None,
            fast_mode: false,
        }],
        renewals: Vec::new(),
    });
    commit(bus, proposal).expect("fixture occupancy commits");
    session
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
