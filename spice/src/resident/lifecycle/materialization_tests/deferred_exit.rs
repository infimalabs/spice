//! An exit notice the resident refused, or could not durably account.

use lane::capture::CaptureExit;
use lane::occupant::Seats;
use wire::FaultClass;

use super::reaped_seat::{
    SeatedProcess, fact_kinds, fixture_root, occupied_seed, residency_row, runtime_paths,
    seat_published, session,
};
use super::*;
use crate::resident::runtime::reap::account_exit;
use crate::resident::supervisor::test_support::record_exited_for_tests;
use crate::resident::{lane_surface, lifecycle};

/// A trigger a second connection installs so the next fact insert aborts,
/// which is what the journal reports when its store fails mid-commit.
const WITHHOLD_FACTS: &str = "CREATE TRIGGER withhold_facts BEFORE INSERT ON fact_envelopes \
    BEGIN SELECT RAISE(ABORT, 'fixture withheld the journal'); END;";

#[test]
fn an_exit_naming_neither_the_durable_row_nor_the_seat_is_written_down_with_both() {
    let root = fixture_root("mismatch");
    let paths = runtime_paths(&root);
    let joining = crate::board_command::JoiningFixture::discover(&root);
    let durable = uuid(3);
    let seated = uuid(4);
    let exiting = uuid(5);
    let bus = seeded_bus(
        Journal::in_memory().expect("mismatch journal opens"),
        durable,
    );
    let mut seats = Seats::new();
    seats
        .seat_detached(lane(), seated, Box::new(SeatedProcess))
        .expect("the successor seats");
    let before = fact_kinds(&bus);
    let mut diagnostic = Vec::new();

    lane_surface::occupant_exited(
        &bus,
        &paths,
        &steer::Config::default(),
        (epoch(), joining.joining()),
        &mut seats,
        CaptureExit {
            lane: lane(),
            occupant: exiting,
        },
        &mut diagnostic,
    )
    .expect("a notice for a stranger is inert");

    assert_eq!(
        String::from_utf8(diagnostic).expect("diagnostic is UTF-8"),
        format!(
            "lane {} exit of occupant {exiting} not accounted: durable row holds {durable}, seat holds {seated}\n",
            lane()
        ),
        "the line names the lane, the exiting occupant, the row occupant and the seat occupant"
    );
    assert!(seats.occupied(lane()), "the seated successor is untouched");
    assert_eq!(residency_row(&bus).occupant, Some(durable));
    assert_eq!(
        fact_kinds(&bus),
        before,
        "a refused notice journals nothing"
    );
    bus.shutdown().expect("mismatch fixture stops");
    std::fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn an_exit_whose_commit_hit_a_store_fault_is_accounted_by_the_next_seat_sweep() {
    let root = fixture_root("deferred");
    let paths = runtime_paths(&root);
    let joining = crate::board_command::JoiningFixture::discover(&root);
    let authority = root.join("authority.sqlite3");
    let occupant = uuid(3);
    let bus = seeded_bus(
        Journal::open(&authority).expect("durable journal opens"),
        occupant,
    );
    let config = lane::Config::default();
    let mut lifecycle = lifecycle::Runtime::new(&config);
    record_exited_for_tests(&paths, lane(), occupant, session(&root, occupant))
        .expect("the supervisor exit record writes");
    let mut seats = seat_published(&paths, &bus, occupant);
    let before = fact_kinds(&bus);
    let mut diagnostic = Vec::new();

    // The journal is withheld for exactly the commit that accounts the exit.
    let withheld = rusqlite::Connection::open(&authority).expect("second connection opens");
    withheld
        .execute_batch(WITHHOLD_FACTS)
        .expect("the withholding trigger installs");
    let exit = CaptureExit {
        lane: lane(),
        occupant,
    };
    let outcome = account_exit(
        &bus,
        &paths,
        (&steer::Config::default(), epoch(), joining.joining()),
        &mut seats,
        &mut lifecycle,
        exit,
        &mut diagnostic,
    )
    .expect_err("the accounting commit faults");
    assert_store_fault(&outcome);
    assert!(
        !seats.occupied(lane()),
        "the seat was vacated before the commit"
    );
    assert_eq!(
        residency_row(&bus).occupant,
        Some(occupant),
        "the durable row is the only place the occupant still lives"
    );
    assert_eq!(
        fact_kinds(&bus),
        before,
        "the faulted commit journaled nothing"
    );
    assert!(diagnostic.is_empty(), "a deferred exit is not a mismatch");
    withheld
        .execute_batch("DROP TRIGGER withhold_facts;")
        .expect("the withholding trigger drops");
    drop(withheld);

    lane_surface::reconcile_seats(
        &bus,
        &paths,
        &steer::Config::default(),
        (epoch(), joining.joining()),
        &mut seats,
        &mut diagnostic,
    )
    .expect("the seat sweep accounts the deferred exit");

    let row = residency_row(&bus);
    assert_eq!(row.occupant, None);
    assert_eq!(row.disposition, "vacant");
    assert_eq!(
        fact_kinds(&bus)[before.len()..],
        [wire::FactKind::LaneExited],
        "the sweep journals exactly the exit the fault deferred"
    );
    assert!(
        !seats.occupied(lane()),
        "the re-adopted seat is vacated again"
    );
    assert!(
        diagnostic.is_empty(),
        "{}",
        String::from_utf8_lossy(&diagnostic)
    );
    bus.verify().expect("the accounted authority verifies");
    bus.shutdown().expect("deferred fixture stops");
    std::fs::remove_dir_all(root).expect("fixture cleanup");
}

fn assert_store_fault(outcome: &wire::Outcome<()>) {
    let wire::Outcome::Fault(fault) = outcome else {
        panic!("a withheld journal is a store fault, not {outcome:?}");
    };
    assert_eq!(fault.class, FaultClass::Store);
    assert_eq!(fault.reason, "journal::commit-failed");
    assert!(
        fault.detail.contains("fixture withheld the journal"),
        "{}",
        fault.detail
    );
}

fn seeded_bus(mut journal: Journal, occupant: CanonicalUuid) -> Bus {
    journal
        .apply(occupied_seed(occupant))
        .expect("occupied fixture seeds");
    Bus::start(journal).expect("occupied fixture bus starts")
}
