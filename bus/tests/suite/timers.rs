use bus::timer::{TimerKind, TimerWorker};
use jiff::{SignedDuration, Timestamp};

/// A deadline no test outlives; one already past is due whatever the host's
/// speed. Ordering among due deadlines is the schedule's own in-process
/// contract, driven there by an injected instant.
const YEAR_MILLISECONDS: i64 = 365 * 24 * 60 * 60 * 1_000;

#[test]
fn blocking_worker_emits_due_deadlines_and_skips_cancellation() {
    let (worker, events) = TimerWorker::start().expect("timer worker starts");
    let pending = worker
        .arm(
            "shutdown",
            TimerKind::LaneHold,
            instant_after(YEAR_MILLISECONDS),
        )
        .expect("pending timer arms");
    let canceled = worker
        .arm(
            "canceled",
            TimerKind::SteerLapse,
            instant_after(YEAR_MILLISECONDS),
        )
        .expect("cancelable timer arms");
    assert!(worker.cancel(canceled).expect("timer cancellation answers"));
    let due_deadline = instant_after(-60_000);
    let due = worker
        .arm("BUS-a1", TimerKind::ClaimLapse, due_deadline)
        .expect("due timer arms");
    assert!(pending.get() < canceled.get());
    assert!(canceled.get() < due.get());

    let event = events.recv().expect("the due deadline fires");
    assert_eq!(event.id(), due);
    assert_eq!(event.owner(), "BUS-a1");
    assert_eq!(event.kind(), TimerKind::ClaimLapse);
    assert_eq!(event.deadline(), due_deadline);

    assert_eq!(
        worker.shutdown().expect("worker shuts down"),
        1,
        "only the uncanceled pending timer remains"
    );
    assert!(
        events.recv().is_err(),
        "shutdown joins the worker, which closes its events"
    );
}

fn instant_after(milliseconds: i64) -> journal::crossing::Instant {
    let shifted = Timestamp::now()
        .checked_add(SignedDuration::from_millis(milliseconds))
        .expect("fixture timestamp is in range");
    journal::crossing::Instant::new(shifted).expect("fixture timestamp has a wire spelling")
}
