//! Renewal tests for a seated occupant whose retained OMP process is idle.

use super::*;

/// A timer runtime whose every seated occupant is idle and whose lanes keep
/// the jobs `bound` describes.
fn start_idle_timers(bound: Option<Instant>) -> Runtime {
    Runtime::start_with_records(
        steer::Config::default(),
        Box::new(FixedRecords { idle: true, bound }),
    )
    .expect("timer runtime starts")
}

fn fixture_renewal(fixture: &Fixture, records: &FixedRecords) -> Option<RenewalAuthority> {
    let snapshot = fixture.bus.task_snapshot().expect("seeded snapshot reads");
    desired_renewals_with_clock(
        snapshot.timer_frontier(),
        snapshot.residency(),
        records,
        || {
            Instant::new(jiff::Timestamp::now())
                .map_err(|error| store_fault("clock", &error.to_string()))
        },
    )
    .expect("renewal authority folds")
    .remove(&fixture.key())
}

#[test]
fn an_idle_occupant_renews_only_while_its_lane_keeps_a_bounded_job() {
    let fixture = automatic_fixture();
    let expiry = fixture.authority.expiry;
    let past_expiry = shifted(expiry, 1);

    let attended = fixture_renewal(
        &fixture,
        &FixedRecords {
            idle: false,
            bound: None,
        },
    )
    .expect("a working occupant rolls its claim");
    assert_eq!(attended.seat, RenewalSeat::Occupied);
    assert_eq!(attended.bound, None);
    assert!(
        fixture_renewal(
            &fixture,
            &FixedRecords {
                idle: true,
                bound: None
            }
        )
        .is_none(),
        "an idle occupant without a bounded job lapses on the lease it carries",
    );
    assert!(
        fixture_renewal(
            &fixture,
            &FixedRecords {
                idle: true,
                bound: Some(expiry)
            }
        )
        .is_none(),
        "a job ending inside the lease extends nothing",
    );
    let unattended = fixture_renewal(
        &fixture,
        &FixedRecords {
            idle: true,
            bound: Some(past_expiry),
        },
    )
    .expect("an idle occupant rolls on its unfinished job");
    assert_eq!(unattended.seat, RenewalSeat::Occupied);
    assert_eq!(unattended.bound, Some(past_expiry));

    fixture.bus.shutdown().expect("fixture bus stops");
}

#[test]
fn an_idle_renewal_rolls_on_the_job_when_it_fires() {
    let fixture = automatic_fixture();
    let bound = shifted(fixture.authority.expiry, 1);
    let records = FixedRecords {
        idle: true,
        bound: Some(bound),
    };
    let authority = fixture_renewal(&fixture, &records).expect("idle renewal arms");
    let at = authority.deadline;

    assert_eq!(
        super::super::renew_claim(&fixture.bus, authority, at, &records),
        Ok(Fired::ClaimRenew)
    );
    let lease = lapsed_heads(&fixture)
        .latest_claim()
        .and_then(|held| held.lease())
        .expect("lease head remains")
        .body()
        .clone();
    assert_eq!(
        lease.expiry, bound,
        "the idle claim rolls no further than its job"
    );

    fixture.bus.shutdown().expect("fixture bus stops");
}

#[test]
fn an_idle_renewal_keeps_a_turn_that_began_before_it_fired() {
    let fixture = automatic_fixture();
    let bound = shifted(fixture.authority.expiry, 1);
    let authority = fixture_renewal(
        &fixture,
        &FixedRecords {
            idle: true,
            bound: Some(bound),
        },
    )
    .expect("idle renewal arms");
    let at = authority.deadline;

    assert_eq!(
        super::super::renew_claim(
            &fixture.bus,
            authority,
            at,
            &FixedRecords {
                idle: false,
                bound: Some(bound)
            }
        ),
        Ok(Fired::ClaimRenew)
    );
    let lease = lapsed_heads(&fixture)
        .latest_claim()
        .and_then(|held| held.lease())
        .expect("lease head remains")
        .body()
        .clone();
    assert_eq!(
        lease.expiry,
        shifted(at, crate::board_command::AUTOMATIC_ROLLING_SECONDS),
        "a reactivated occupant rolls the full window, not its stale job bound",
    );

    fixture.bus.shutdown().expect("fixture bus stops");
}

#[test]
fn a_parked_idle_claim_lapses_on_the_lease_it_carries() {
    let fixture = automatic_fixture_with_age(61);
    let mut timers = start_idle_timers(None);
    timers
        .synchronize(&fixture.bus)
        .expect("idle occupant folds");
    assert!(timers.renewals.is_empty());
    assert_eq!(timers.claims.len(), 1);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("fixture async runtime builds");
    let event = runtime
        .block_on(timers.recv())
        .expect("idle claim lapse fires");

    assert_eq!(event.kind(), TimerKind::ClaimLapse);
    let Ok(Fired::ClaimLapse(Some(lapsed))) =
        timers.consume(&fixture.bus, event, Some(fixture.hosted().host()))
    else {
        panic!("the idle claim lapse commits");
    };
    assert_eq!(lapsed.lane, fixture.authority.lane);
    assert_eq!(
        lapsed_heads(&fixture)
            .latest_claim()
            .map(|held| held.claim())
            .expect("lapsed claim head remains")
            .body()
            .disposition,
        ClaimDisposition::Lapsed,
    );

    drop(timers);
    fixture.bus.shutdown().expect("fixture bus stops");
}
