//! A held claim waiting on its own deadline-bounded jobs.

use super::tests::{instant, lane};
use super::*;

/// A vacant lane with a settled held claim and other work it could take.
fn vacant_holder() -> Target<'static> {
    Target {
        lane: lane(1),
        last_claim: None,
        pending_inbox: false,
        inbox_reminder_only: false,
        held_claim: Available::At(instant(3)),
        drain_queue: Available::Ready,
        last_resort: false,
        task: None,
        matched: None,
        park: None,
    }
}

/// An occupied idle lane whose retained claim would otherwise continue now.
fn retained_holder() -> Target<'static> {
    Target {
        drain_queue: Available::None,
        ..vacant_holder()
    }
}

fn held(targets: &[Target], now: Instant) -> Resolution {
    resolve(targets, &Backoff::default(), now)
}

#[test]
fn an_unfinished_regular_job_holds_claim_continuation_but_not_urgent_steering() {
    let delivered = instant(40);
    let mut targets = [vacant_holder()];
    await_regular_jobs(&mut targets, instant(10), |_| Some(delivered));
    assert_eq!(targets[0].held_claim, Available::At(delivered));
    assert_eq!(targets[0].drain_queue, Available::None);
    let job = Some(wire::body::LanePark::Job { until: delivered });
    assert_eq!(targets[0].park, job, "the deferred continuation is parked");
    assert_eq!(
        held(&targets, instant(10)),
        Resolution::Wait {
            trigger: Trigger::HeldClaim,
            lane: lane(1),
            deadline: delivered,
        },
        "the job's admitted expiry is the one scheduled seat"
    );

    let mut retained = [retained_holder()];
    await_regular_jobs(&mut retained, instant(10), |_| Some(delivered));
    assert_eq!(
        retained[0].park, job,
        "an occupied idle holder parks at once"
    );
    assert_eq!(
        held(&retained, instant(10)),
        Resolution::Wait {
            trigger: Trigger::HeldClaim,
            lane: lane(1),
            deadline: delivered,
        },
        "an occupied idle holder also waits for its bounded job"
    );

    targets[0].pending_inbox = true;
    assert_eq!(
        held(&targets, instant(10)),
        Resolution::Attempt {
            trigger: Trigger::PendingInbox,
            lane: lane(1),
        },
        "operator steering is never held behind a job"
    );

    // The admitted expiry arrives with no keeper notification.
    let mut targets = [vacant_holder()];
    await_regular_jobs(&mut targets, delivered, |_| Some(delivered));
    assert_eq!(targets[0].park, None, "an expired job parks nothing");
    assert_eq!(
        held(&targets, delivered),
        Resolution::Attempt {
            trigger: Trigger::HeldClaim,
            lane: lane(1),
        }
    );
}

#[test]
fn a_finished_job_or_no_regular_job_reseats_the_held_claim_at_once() {
    // The keeper published the ending, or only services and ended jobs remain.
    let mut targets = [vacant_holder()];
    await_regular_jobs(&mut targets, instant(10), |_| None);
    assert_eq!(targets, [vacant_holder()]);
    assert_eq!(
        held(&targets, instant(10)),
        Resolution::Attempt {
            trigger: Trigger::HeldClaim,
            lane: lane(1),
        }
    );

    let mut retained = [retained_holder()];
    await_regular_jobs(&mut retained, instant(10), |_| None);
    assert_eq!(retained[0].park, None);
    assert_eq!(
        held(&retained, instant(10)),
        Resolution::Attempt {
            trigger: Trigger::HeldClaim,
            lane: lane(1),
        },
        "services and ended jobs do not hold a retained continuation"
    );
}

#[test]
fn a_job_holds_only_a_repeat_peer_wake_without_a_live_claim() {
    let mut free = vacant_holder();
    free.held_claim = Available::None;
    let mut ordinary = [free];
    await_regular_jobs(&mut ordinary, instant(10), |_| Some(instant(40)));
    assert_eq!(
        held(&ordinary, instant(10)),
        Resolution::Attempt {
            trigger: Trigger::DrainQueue,
            lane: lane(1),
        },
        "a finite job alone does not reserve a claim-free lane"
    );

    let mut reminder = free;
    reminder.pending_inbox = true;
    reminder.inbox_reminder_only = true;
    reminder.drain_queue = Available::None;
    let mut reminders = [reminder];
    await_regular_jobs(&mut reminders, instant(10), |_| Some(instant(40)));
    assert!(!reminders[0].pending_inbox);
    assert_eq!(
        [ordinary[0].park, reminders[0].park],
        [None, None],
        "a job without a held claim parks nothing"
    );
    assert_eq!(
        held(&reminders, instant(10)),
        Resolution::Idle,
        "a repeat peer presentation does not reseat the idle job owner"
    );

    let mut finished = [reminder];
    await_regular_jobs(&mut finished, instant(10), |_| None);
    assert_eq!(
        held(&finished, instant(10)),
        Resolution::Attempt {
            trigger: Trigger::PendingInbox,
            lane: lane(1),
        },
        "job completion exposes the queued reminder at the next reconciliation"
    );
}
