use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use bus::timer::{TimerKind, TimerWorker};
use jiff::{SignedDuration, Timestamp};

#[test]
fn blocking_worker_orders_reverse_armed_deadlines_and_skips_cancellation() {
    let (worker, events) = TimerWorker::start().expect("timer worker starts");
    let late_deadline = instant_after(250);
    let late = worker
        .arm("late", TimerKind::LaneResume, late_deadline)
        .expect("late timer arms first");
    let lease_deadline = instant_after(150);
    let lease_id = worker
        .arm("BUS-a1", TimerKind::ClaimLapse, lease_deadline)
        .expect("claim-lapse timer arms");
    let first_deadline = instant_after(50);
    let first = worker
        .arm("first", TimerKind::TaskWake, first_deadline)
        .expect("earliest timer arms last");
    let canceled = worker
        .arm("canceled", TimerKind::SteerLapse, instant_after(60_000))
        .expect("cancelable timer arms");
    assert!(worker.cancel(canceled).expect("timer cancellation answers"));
    assert!(late.get() < lease_id.get());
    assert!(lease_id.get() < first.get());

    let first_event = events
        .recv_timeout(Duration::from_secs(2))
        .expect("first actual deadline");
    assert_eq!(first_event.id(), first);
    assert_eq!(first_event.owner(), "first");
    assert_eq!(first_event.kind(), TimerKind::TaskWake);
    assert_eq!(first_event.deadline(), first_deadline);

    let lease_event = events
        .recv_timeout(Duration::from_secs(2))
        .expect("lease actual deadline");
    assert_eq!(lease_event.id(), lease_id);
    assert_eq!(lease_event.owner(), "BUS-a1");
    assert_eq!(lease_event.kind(), TimerKind::ClaimLapse);
    assert_eq!(lease_event.deadline(), lease_deadline);

    let late_event = events
        .recv_timeout(Duration::from_secs(2))
        .expect("late actual deadline");
    assert_eq!(late_event.id(), late);
    assert_eq!(late_event.owner(), "late");
    assert_eq!(late_event.deadline(), late_deadline);
    assert!(matches!(
        events.recv_timeout(Duration::from_millis(20)),
        Err(RecvTimeoutError::Timeout)
    ));

    let pending = worker
        .arm("shutdown", TimerKind::LaneHold, instant_after(60_000))
        .expect("pending timer arms");
    assert!(pending.get() > late.get());
    assert_eq!(worker.shutdown().expect("worker shuts down"), 1);
    assert!(matches!(
        events.recv_timeout(Duration::from_millis(20)),
        Err(RecvTimeoutError::Disconnected)
    ));
}

#[test]
fn a_past_durable_deadline_fires_promptly_without_a_caller_duration() {
    let (worker, events) = TimerWorker::start().expect("timer worker starts");
    let past = instant_after(-60_000);
    let id = worker
        .arm("past", TimerKind::TaskWake, past)
        .expect("past timer arms");

    let event = events
        .recv_timeout(Duration::from_secs(2))
        .expect("past deadline fires promptly");
    assert_eq!(event.id(), id);
    assert_eq!(event.deadline(), past);
    assert_eq!(worker.shutdown().expect("worker shuts down"), 0);
}

fn instant_after(milliseconds: i64) -> journal::crossing::Instant {
    shifted(Timestamp::now(), milliseconds)
}

fn shifted(timestamp: Timestamp, milliseconds: i64) -> journal::crossing::Instant {
    let shifted = timestamp
        .checked_add(SignedDuration::from_millis(milliseconds))
        .expect("fixture timestamp is in range");
    journal::crossing::Instant::new(shifted).expect("fixture timestamp has a wire spelling")
}
