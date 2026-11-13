//! A seated occupant whose supervisor ending never reached the resident.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use lane::occupant::Seats;

use super::*;
use crate::resident::runtime::reap::sweep_seats;
use crate::resident::supervisor::test_support::{record_exited_for_tests, record_failed_for_tests};
use crate::resident::supervisor::{Adoption, adopt};
use crate::resident::{RuntimePaths, lifecycle};

#[test]
fn a_seat_whose_exit_notice_was_lost_is_reaped_and_its_pending_inbox_wake_fires() {
    let root = fixture_root("exit");
    let paths = runtime_paths(&root);
    let joining = crate::board_command::JoiningFixture::discover(&root);
    let occupant = uuid(3);
    let (bus, mut projection) = occupied_fixture(occupant);
    let config = lane::Config::default();
    let mut lifecycle = lifecycle::Runtime::new(&config);
    let mut diagnostic = Vec::new();

    // Nothing published yet: the seat is the live authority and stays.
    let mut seats = Seats::new();
    seats
        .seat_detached(lane(), occupant, Box::new(SeatedProcess))
        .expect("occupant seats");
    let directed = bus.task_snapshot().expect("directed snapshot reads");
    assert!(
        targets(&directed, &projection).is_empty(),
        "an occupied row targets no wake, so the lost notice starves the inbox"
    );
    sweep_seats(
        &bus,
        (&steer::Config::default(), epoch(), joining.joining()),
        &paths,
        &mut seats,
        &mut lifecycle,
        &BTreeSet::new(),
        &mut diagnostic,
    )
    .expect("a seat without a published ending sweeps clean");
    assert!(seats.occupied(lane()));
    assert_eq!(residency_row(&bus).occupant, Some(occupant));
    let untouched = fact_kinds(&bus);
    drop(seats);

    // The supervisor reaped its agent and published the exit, but the one
    // datagram carrying that notice never arrived: the resident still holds
    // the handle it seated while the agent ran.
    record_exited_for_tests(&paths, lane(), occupant, session(&root, occupant))
        .expect("the supervisor exit record writes");
    let mut seats = seat_published(&paths, &bus, occupant);
    sweep_seats(
        &bus,
        (&steer::Config::default(), epoch(), joining.joining()),
        &paths,
        &mut seats,
        &mut lifecycle,
        &BTreeSet::new(),
        &mut diagnostic,
    )
    .expect("the sweep accounts the published exit");

    assert!(
        !seats.occupied(lane()),
        "the reaped occupant is not left seated"
    );
    let row = residency_row(&bus);
    assert_eq!(row.occupant, None);
    assert_eq!(row.disposition, "vacant");
    let kinds = fact_kinds(&bus);
    assert_eq!(
        kinds[untouched.len()..],
        [wire::FactKind::LaneExited],
        "the running resident journals exactly the exit the supervisor recorded"
    );
    let diagnostic = String::from_utf8(diagnostic).expect("diagnostic is UTF-8");
    assert!(
        diagnostic.contains("ended without a supervisor notice"),
        "{diagnostic}"
    );

    let reaped = bus.task_snapshot().expect("reaped snapshot reads");
    projection
        .advance(&reaped)
        .expect("reaped projection advances");
    let reaped_targets = targets(&reaped, &projection);
    assert!(reaped_targets[0].pending_inbox);
    assert_eq!(
        resolve(&reaped_targets, &Backoff::default(), instant(50),),
        Resolution::Attempt {
            trigger: Trigger::PendingInbox,
            lane: lane(),
        },
        "the operator direction the stale seat starved wakes the lane"
    );

    // A vacated seat is nothing to sweep, and the record stays on disk for the
    // startup sweep to find settled.
    sweep_seats(
        &bus,
        (&steer::Config::default(), epoch(), joining.joining()),
        &paths,
        &mut seats,
        &mut lifecycle,
        &BTreeSet::new(),
        &mut Vec::new(),
    )
    .expect("an empty seat table sweeps clean");
    assert_eq!(fact_kinds(&bus), kinds);
    bus.verify().expect("the reaped authority verifies");
    bus.shutdown().expect("reaped fixture stops");
    std::fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn a_seat_whose_ending_is_at_the_door_is_left_to_the_notice_arm() {
    let root = fixture_root("noticed");
    let paths = runtime_paths(&root);
    let joining = crate::board_command::JoiningFixture::discover(&root);
    let occupant = uuid(4);
    let (bus, _projection) = occupied_fixture(occupant);
    let config = lane::Config::default();
    let mut lifecycle = lifecycle::Runtime::new(&config);
    let mut diagnostic = Vec::new();
    record_exited_for_tests(&paths, lane(), occupant, session(&root, occupant))
        .expect("the supervisor exit record writes");
    let mut seats = seat_published(&paths, &bus, occupant);
    let kinds = fact_kinds(&bus);

    // The supervisor published its exit and its notice reached the door
    // during this turn: the sweep leaves the seat to the notice arm.
    let noticed = BTreeSet::from([(lane(), occupant)]);
    sweep_seats(
        &bus,
        (&steer::Config::default(), epoch(), joining.joining()),
        &paths,
        &mut seats,
        &mut lifecycle,
        &noticed,
        &mut diagnostic,
    )
    .expect("a noticed ending is left to its notice");

    assert!(seats.occupied(lane()), "the seat waits for the notice arm");
    assert_eq!(residency_row(&bus).occupant, Some(occupant));
    assert_eq!(
        fact_kinds(&bus),
        kinds,
        "nothing is journaled ahead of the notice"
    );
    assert!(
        diagnostic.is_empty(),
        "{}",
        String::from_utf8_lossy(&diagnostic)
    );
    bus.shutdown().expect("noticed fixture stops");
    std::fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn a_seat_whose_failure_notice_was_lost_is_degraded_from_its_published_state() {
    let root = fixture_root("failure");
    let paths = runtime_paths(&root);
    let joining = crate::board_command::JoiningFixture::discover(&root);
    let occupant = uuid(3);
    let (bus, _) = occupied_fixture(occupant);
    let config = lane::Config::default();
    let mut lifecycle = lifecycle::Runtime::new(&config);
    let mut seats = Seats::new();
    seats
        .seat_detached(lane(), occupant, Box::new(SeatedProcess))
        .expect("occupant seats");
    let before = fact_kinds(&bus);
    record_failed_for_tests(
        &paths,
        lane(),
        occupant,
        session(&root, occupant),
        "agent lost its worktree",
    )
    .expect("the supervisor failure record writes");

    sweep_seats(
        &bus,
        (&steer::Config::default(), epoch(), joining.joining()),
        &paths,
        &mut seats,
        &mut lifecycle,
        &BTreeSet::new(),
        &mut Vec::new(),
    )
    .expect("the sweep accounts the published failure");

    assert!(!seats.occupied(lane()));
    let row = residency_row(&bus);
    assert_eq!(row.occupant, None);
    assert_eq!(row.disposition, "degraded");
    assert_eq!(
        fact_kinds(&bus)[before.len()..],
        [wire::FactKind::LaneDegraded]
    );
    bus.verify().expect("the degraded authority verifies");
    bus.shutdown().expect("degraded fixture stops");
    std::fs::remove_dir_all(root).expect("fixture cleanup");
}

/// The handle a resident holds for an occupant whose agent is still running.
pub(super) struct SeatedProcess;

impl lane::occupant::DetachedProcess for SeatedProcess {
    fn group(&self) -> u32 {
        79
    }

    fn termination_grace(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }

    fn reap_bound(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }

    fn wait_for(
        self: Box<Self>,
        _duration: std::time::Duration,
    ) -> Result<lane::occupant::DetachedWait, sill::WaitError> {
        Ok(lane::occupant::DetachedWait::Running(self))
    }

    fn terminate(&self) -> Result<(), sill::WaitError> {
        panic!("a seat the sweep leaves alone is never signalled")
    }

    fn kill(&self) -> Result<(), sill::WaitError> {
        panic!("a seat the sweep leaves alone is never signalled")
    }

    fn wait(self: Box<Self>) -> Result<sill::WaitOutcome, sill::WaitError> {
        panic!("a seat the sweep leaves alone is never waited for")
    }
}

/// Seat the exact handle a resident reads back from published supervisor
/// state, which is what its own seated handle observes once the agent ends.
pub(super) fn seat_published(paths: &RuntimePaths, bus: &Bus, occupant: CanonicalUuid) -> Seats {
    let row = residency_row(bus);
    let Some(Adoption::Exited(launch)) = adopt(paths, &row).expect("published state adopts") else {
        panic!("the published exit was not adoptable");
    };
    let mut seats = Seats::new();
    seats
        .seat_detached(lane(), occupant, launch.process)
        .expect("published occupant seats");
    seats
}

fn occupied_fixture(occupant: CanonicalUuid) -> (Bus, Projection) {
    let mut journal = Journal::in_memory().expect("occupied fixture journal opens");
    journal
        .apply(occupied_seed(occupant))
        .expect("occupied fixture seeds");
    let bus = Bus::start(journal).expect("occupied fixture bus starts");
    committed(
        bus.commit(operator_direction_write())
            .expect("operator direction commits"),
    );
    let mut projection = Projection::new();
    projection
        .advance(
            &bus.task_snapshot()
                .expect("occupied fixture snapshot reads"),
        )
        .expect("occupied fixture projection builds");
    assert!(!projection.operator_steering(lane()).is_empty());
    (bus, projection)
}

pub(super) fn occupied_seed(occupant: CanonicalUuid) -> JournalWrite {
    occupied_seed_at(occupant, "/fixture")
}

/// Seat the fixture lane on `worktree`, which names a repository when the
/// scenario publishes the lane inbox and any path at all when it does not.
pub(super) fn occupied_seed_at(occupant: CanonicalUuid, worktree: &str) -> JournalWrite {
    occupied_seed_with_session(occupant, worktree, None)
}

/// Seat the fixture lane on `worktree` with the session its occupant
/// declared, when the scenario reads the occupant's transcript by that
/// declaration.
pub(super) fn occupied_seed_with_session(
    occupant: CanonicalUuid,
    worktree: &str,
    session: Option<wire::LaneSession>,
) -> JournalWrite {
    let origin = format!("resident:{}", epoch())
        .parse::<Origin>()
        .expect("fixture resident origin");
    let mut write = JournalWrite::new(Actor::Spice, origin, instant(1), Producer::CommandDoor);
    let seated = write
        .fact(body::UpSeated { epoch: epoch() }, FactCause::Root)
        .expect("resident fact builds");
    let worktree = worktree.to_owned();
    write.residency_head(seated, move |_| ResidencyHead {
        epoch: epoch(),
        state: ResidentState::Seated,
        topology_revision: 1,
        lanes: vec![LaneResidency {
            lane: lane(),
            team: team(),
            name: None,
            worktree,
            life: "drive".to_owned(),
            vendor: "omp".parse().expect("fixture vendor"),
            model: None,
            effort: None,
            fast_mode: false,
            occupant: Some(occupant),
            lineage: vec![occupant],
            capabilities: vec![uuid(9)],
            session,
            disposition: "occupied".to_owned(),
        }],
        subscriptions: vec![wire::TeamSubscription {
            team: team(),
            formed_at: None,
            drive: true,
            drain: true,
            name: None,
            fast_mode: false,
        }],
        renewals: Vec::new(),
    });
    write
}

fn targets<'a>(snapshot: &bus::TaskSnapshot, projection: &'a Projection) -> Vec<Target<'a>> {
    project_targets(
        snapshot,
        projection,
        instant(50),
        &Holds::new(3),
        &board::NextConfig::default(),
        None,
        &owners(snapshot),
    )
    .expect("lifecycle targets project")
}

pub(super) fn residency_row(bus: &Bus) -> LaneResidency {
    bus.residency_head()
        .expect("residency reads")
        .expect("residency exists")
        .body()
        .lanes[0]
        .clone()
}

pub(super) fn fact_kinds(bus: &Bus) -> Vec<wire::FactKind> {
    bus.transact(|journal| {
        bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
    })
    .expect("facts read")
    .facts()
    .iter()
    .map(|fact| fact.fact.kind())
    .collect()
}

pub(super) fn session(root: &Path, occupant: CanonicalUuid) -> wire::LaneSession {
    wire::LaneSession {
        lane: lane(),
        occupant,
        worktree: root.to_string_lossy().into_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        launch: wire::body::LaneLaunchIdentity::new("sonnet", "high"),
        epoch: epoch(),
        stream: "stdout".to_owned(),
        identity: "reaped-session".to_owned(),
        locus: wire::TapeLocus {
            member: 0,
            offset: 0,
            length: 1,
        },
    }
}

/// One fixture root, marked as a repository so its joins resolve.
pub(super) fn fixture_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("spice-reaped-seat-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(root.join(".git")).expect("fixture root");
    root
}

pub(super) fn runtime_paths(root: &Path) -> RuntimePaths {
    RuntimePaths {
        worktree: root.to_path_buf(),
        common: root.join("common"),
        runtime: root.join("runtime"),
        live: root.join("runtime").join("runtime"),
        data: root.join("data"),
        journal: root.join("journal"),
        resident: root.join("resident"),
        serve: root.join("serve"),
        lane_serve: root.join("lane-serve"),
        degraded: root.join("degraded"),
        tape: root.join("lanes"),
        supervisors: root.join("lanes"),
        steering_media: root.join("lanes"),
        releases: root.join("authority/releases"),
        stage: root.join("stage"),
    }
}
