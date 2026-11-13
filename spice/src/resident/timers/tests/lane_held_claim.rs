//! Renewal tests for a claim won through allocation rather than the resident.

use super::*;

/// One claim minted the way allocation mints it: a fixed hour, never automatic.
///
/// `age_seconds` places the fixture inside that hour, so a fixture older than
/// the rolling horizon is already due for its first renewal.
fn lane_held_fixture(age_seconds: i64) -> Fixture {
    let policy_root = PolicyRoot::new();
    let now = Instant::new(jiff::Timestamp::now()).expect("fixture clock is canonical");
    let start = shifted(now, -age_seconds);
    let expiry = shifted(start, spice::claim_lease::AUTOMATIC_MAXIMUM_SECONDS);
    let maximum = expiry;
    let authority = Authority {
        task: task(),
        lane: uuid(20),
        occupant: uuid(21),
        epoch: policy_root.1,
        worktree: format!("/fixture/{}", uuid(20)),
        start,
        expiry,
        maximum,
    };
    let mut journal = Journal::in_memory().expect("fixture journal opens");
    journal
        .apply(seed_write(&authority))
        .expect("fixture lane-held authority seeds");
    Fixture {
        bus: Bus::start(journal).expect("fixture bus starts"),
        authority,
        policy_root,
    }
}

#[test]
fn lane_held_claim_rolls_past_the_hour_its_allocation_minted() {
    let fixture = lane_held_fixture(spice::claim_lease::AUTOMATIC_MAXIMUM_SECONDS - 30);
    let minted_expiry = fixture.authority.expiry;
    let snapshot = fixture.bus.task_snapshot().expect("seeded snapshot reads");
    assert!(
        !crate::board_command::is_automatic_claim(
            lapsed_heads(&fixture)
                .latest_claim()
                .map(|held| held.claim())
                .expect("seeded claim head exists")
                .body(),
            snapshot.facts(),
        ),
        "the fixture exercises an allocated claim, not a resident-minted automatic one",
    );
    let mut timers = start_timers().expect("timer runtime starts");
    timers
        .synchronize(&fixture.bus)
        .expect("lane-held lease arms a renewal");
    assert_eq!(timers.renewals.len(), 1);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("fixture async runtime builds");
    let event = runtime
        .block_on(timers.recv())
        .expect("due lane-held renewal fires");

    assert_eq!(event.kind(), TimerKind::ClaimRenew);
    assert_eq!(
        timers.consume(&fixture.bus, event, Some(&fixture.repository())),
        Ok(Fired::ClaimRenew)
    );
    timers
        .synchronize(&fixture.bus)
        .expect("renewed lane-held lease re-arms");
    let heads = lapsed_heads(&fixture);
    assert_eq!(
        heads
            .latest_claim()
            .map(|held| held.claim())
            .expect("claim head remains")
            .body()
            .disposition,
        ClaimDisposition::Active,
    );
    let lease = heads
        .latest_claim()
        .and_then(|held| held.lease())
        .expect("lease head remains");
    assert!(
        lease.body().expiry > minted_expiry,
        "the rolled lease outlives the minted hour",
    );
    assert_eq!(
        timers
            .claims
            .get(&fixture.key())
            .expect("lapse timer follows the rolled lease")
            .authority
            .expiry,
        lease.body().expiry,
    );
    assert_eq!(
        fixture
            .bus
            .transact(|journal| bus::JournalTransaction::settled(
                journal.fact_snapshot().expect("journal snapshot")
            ))
            .expect("renewal facts read")
            .facts()
            .iter()
            .filter(|fact| fact.fact.kind() == wire::FactKind::ClaimRenewed)
            .count(),
        1,
    );

    drop(timers);
    fixture.bus.shutdown().expect("fixture bus stops");
}

#[test]
fn lane_held_claim_stops_rolling_once_its_occupant_leaves_the_lane() {
    let fixture = lane_held_fixture(spice::claim_lease::AUTOMATIC_MAXIMUM_SECONDS - 30);
    let minted_expiry = fixture.authority.expiry;
    let mut timers = start_timers().expect("timer runtime starts");
    timers
        .synchronize(&fixture.bus)
        .expect("lane-held lease arms a renewal");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("fixture async runtime builds");
    let event = runtime
        .block_on(timers.recv())
        .expect("due lane-held renewal fires");
    move_residency(&fixture, fixture.authority.epoch, uuid(24));

    assert_eq!(
        timers.consume(&fixture.bus, event, Some(&fixture.repository())),
        Ok(Fired::Stale)
    );
    timers
        .synchronize(&fixture.bus)
        .expect("departed occupant disarms the renewal");
    assert!(timers.renewals.is_empty());
    assert!(
        fixture
            .bus
            .transact(|journal| bus::JournalTransaction::settled(
                journal.fact_snapshot().expect("journal snapshot")
            ))
            .expect("facts read")
            .facts()
            .iter()
            .all(|fact| fact.fact.kind() != wire::FactKind::ClaimRenewed)
    );
    assert_eq!(
        timers
            .claims
            .get(&fixture.key())
            .expect("lapse timer remains armed")
            .authority
            .expiry,
        minted_expiry,
        "the abandoned claim still lapses on the lease allocation minted",
    );

    drop(timers);
    fixture.bus.shutdown().expect("fixture bus stops");
}
