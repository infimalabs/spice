use super::tests::{instant, lane, target};
use super::*;

fn chosen(targets: &[Target]) -> (Trigger, CanonicalUuid) {
    match resolve(targets, &Backoff::default(), instant(10)) {
        Resolution::Attempt { trigger, lane } | Resolution::Wait { trigger, lane, .. } => {
            (trigger, lane)
        }
        Resolution::Idle => panic!("a due lane resolves"),
    }
}

#[test]
fn due_lanes_wake_by_match_then_newest_claim_then_lane_id() {
    let lane_at = |lane: u8, claim: Option<u64>, matched: Option<f64>, available| Target {
        last_claim: claim.map(|seq| Seq::new(seq).expect("claim position")),
        drain_queue: available,
        matched,
        ..target(lane)
    };
    let ready = |lane, claim, matched| lane_at(lane, Some(claim), matched, Available::Ready);
    let work = |n| (Trigger::DrainQueue, lane(n));
    let never = lane_at(4, None, None, Available::Ready);

    // The higher match wins over a more recent claim and a lower id.
    assert_eq!(
        chosen(&[ready(1, 90, Some(0.2)), ready(2, 10, Some(0.6))]),
        work(2)
    );
    // Any score outranks no score, even a zero one.
    assert_eq!(
        chosen(&[ready(1, 90, None), ready(2, 10, Some(0.0))]),
        work(2)
    );
    // Equal or absent matches fall to the newest claim, whatever the lane
    // ids or input order, and the woken lane keeps waking while it claims.
    assert_eq!(
        chosen(&[ready(1, 10, Some(0.5)), ready(2, 90, Some(0.5))]),
        work(2)
    );
    assert_eq!(
        chosen(&[ready(1, 90, None), ready(3, 40, None), ready(2, 70, None)]),
        work(1)
    );
    assert_eq!(
        chosen(&[ready(1, 92, None), ready(3, 91, None), ready(2, 70, None)]),
        work(1)
    );
    // A never-claimed lane comes last; exact ties fall to lane id.
    assert_eq!(chosen(&[ready(1, 5, None), never]), work(1));
    assert_eq!(
        chosen(&[never, lane_at(3, None, None, Available::Ready)]),
        work(3)
    );
    assert_eq!(chosen(&[ready(2, 7, None), ready(1, 7, None)]), work(1));
    // Equal wait deadlines name the same lane the rank would wake.
    let later = |lane, claim| lane_at(lane, Some(claim), None, Available::At(instant(20)));
    assert_eq!(chosen(&[later(1, 50), later(2, 30)]), work(1));
}

#[test]
fn inbox_and_held_claim_wakes_rank_by_recency_alone() {
    let held = |lane: u8, claim: u64, matched| Target {
        last_claim: Some(Seq::new(claim).expect("claim position")),
        held_claim: Available::Ready,
        matched,
        ..target(lane)
    };
    let inbox = |lane: u8, claim: u64, matched| Target {
        pending_inbox: true,
        ..held(lane, claim, matched)
    };
    // Rank never overrides trigger precedence.
    let ready = Target {
        held_claim: Available::None,
        drain_queue: Available::Ready,
        matched: Some(0.9),
        ..held(1, 99, None)
    };
    assert_eq!(
        chosen(&[ready, held(5, 1, None)]),
        (Trigger::HeldClaim, lane(5))
    );
    // Among held-claim and inbox wakes the newest claim wins however the
    // lanes score.
    assert_eq!(
        chosen(&[held(1, 10, Some(0.9)), held(2, 90, Some(0.1))]),
        (Trigger::HeldClaim, lane(2))
    );
    assert_eq!(
        chosen(&[held(1, 10, Some(0.9)), held(2, 90, None)]),
        (Trigger::HeldClaim, lane(2))
    );
    assert_eq!(
        chosen(&[inbox(1, 10, Some(0.9)), inbox(2, 90, None)]),
        (Trigger::PendingInbox, lane(2))
    );
    assert_eq!(
        chosen(&[held(2, 7, Some(0.9)), held(1, 7, None)]),
        (Trigger::HeldClaim, lane(1))
    );
}

#[test]
fn loose_work_wakes_after_held_claims_and_before_new_work() {
    let loose = |lane: u8| Target {
        loose_worktree: true,
        ..target(lane)
    };
    let work = Target {
        drain_queue: Available::Ready,
        ..target(2)
    };
    let held = Target {
        held_claim: Available::Ready,
        ..target(3)
    };
    assert_eq!(chosen(&[work, loose(1)]), (Trigger::LooseWorktree, lane(1)));
    assert_eq!(chosen(&[loose(1), held]), (Trigger::HeldClaim, lane(3)));
    assert_eq!(
        chosen(&[Target {
            drain_queue: Available::Ready,
            ..loose(1)
        }]),
        (Trigger::LooseWorktree, lane(1)),
        "a lane settles its loose work before it takes new work"
    );
}

#[test]
fn ordinary_work_wakes_before_a_lanes_own_last_resort() {
    let due = |lane: u8, claim: u64, last_resort, available| Target {
        last_claim: Some(Seq::new(claim).expect("claim position")),
        drain_queue: available,
        last_resort,
        ..target(lane)
    };
    // The author's claim is the newest, so recency alone would wake it
    // first; the class decides before recency.
    let author = due(1, 90, true, Available::Ready);
    let peer = due(2, 10, false, Available::Ready);
    let fresh = Target {
        last_claim: None,
        ..peer
    };
    assert_eq!(chosen(&[author, peer]), (Trigger::DrainQueue, lane(2)));
    assert_eq!(chosen(&[author, fresh]), (Trigger::DrainQueue, lane(2)));
    // An ordinary lane outranks a better-matched lane's own last resort.
    let matched_author = Target {
        matched: Some(0.9),
        ..author
    };
    assert_eq!(
        chosen(&[matched_author, peer]),
        (Trigger::DrainQueue, lane(2))
    );
    // A peerless author still wakes for its last resort.
    assert_eq!(chosen(&[author]), (Trigger::DrainQueue, lane(1)));
    // Within one class the newest claim still decides.
    let other_author = due(3, 50, true, Available::Ready);
    assert_eq!(
        chosen(&[author, other_author]),
        (Trigger::DrainQueue, lane(1))
    );
    // The class never overrides trigger precedence, and only work triggers
    // carry it.
    let holding = Target {
        held_claim: Available::Ready,
        ..author
    };
    assert_eq!(chosen(&[peer, holding]), (Trigger::HeldClaim, lane(1)));
    let holding_peer = Target {
        lane: lane(4),
        last_claim: Some(Seq::new(10).expect("claim position")),
        last_resort: false,
        ..holding
    };
    assert_eq!(
        chosen(&[holding, holding_peer]),
        (Trigger::HeldClaim, lane(1))
    );
    // A due last resort still wakes before a peer whose work is not yet due.
    let pending_peer = due(2, 10, false, Available::At(instant(20)));
    assert_eq!(
        chosen(&[author, pending_peer]),
        (Trigger::DrainQueue, lane(1))
    );
    // Equal wait deadlines name the ordinary lane.
    let later = |target: Target<'static>| Target {
        drain_queue: Available::At(instant(20)),
        ..target
    };
    assert_eq!(
        chosen(&[later(author), later(peer)]),
        (Trigger::DrainQueue, lane(2))
    );
}
