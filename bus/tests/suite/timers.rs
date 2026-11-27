use bus::timer::{TimerKind, TimerWorker};
use jiff::{SignedDuration, Timestamp};

#[test]
fn blocking_worker_emits_a_due_deadline_and_closes_on_shutdown() {
    let (worker, events) = TimerWorker::start().expect("timer worker starts");
    let due_deadline = instant_after(-60_000);
    let due = worker
        .arm("BUS-a1", TimerKind::ClaimLapse, due_deadline)
        .expect("due timer arms");

    let event = events
        .recv_timeout(test_support::DEADLOCK_GUARD)
        .expect("the due deadline fires");
    assert_eq!(event.id(), due);
    assert_eq!(event.owner(), "BUS-a1");
    assert_eq!(event.kind(), TimerKind::ClaimLapse);
    assert_eq!(event.deadline(), due_deadline);

    assert_eq!(worker.shutdown().expect("worker shuts down"), 0);
    assert!(
        events.recv_timeout(test_support::DEADLOCK_GUARD).is_err(),
        "shutdown joins the worker, which closes its events"
    );
}

fn instant_after(milliseconds: i64) -> journal::crossing::Instant {
    let shifted = Timestamp::now()
        .checked_add(SignedDuration::from_millis(milliseconds))
        .expect("fixture timestamp is in range");
    journal::crossing::Instant::new(shifted).expect("fixture timestamp has a wire spelling")
}
