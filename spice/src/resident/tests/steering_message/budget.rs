//! Free traffic never counts: active delivery and bodies joining a committed
//! start reach their lane without a start. A body that must start an idle lane
//! outside every round spends one of its ordered pair's five starts, or is
//! refused before any peer input exists.

use super::*;

/// Author one packet on `lane` and judge it live.
fn call(lanes: &Lanes, lane: u8, text: &str) -> Result<steering::Published, steering::Unpublished> {
    lanes.author(lane, text);
    let (_, outcomes) = lanes.hear_newest(lane);
    let [outcome] = <[_; 1]>::try_from(outcomes).expect("one packet per record");
    outcome
}

/// The admission the newest queued body recorded.
fn newest_queued(lanes: &Lanes) -> Option<wire::body::PeerAdmission> {
    let queued = lanes.queued_messages();
    let (_, body) = queued.last().expect("a body queued");
    body.admission
}

#[test]
fn an_idle_lane_outside_every_round_takes_five_starts_per_ordered_pair() {
    let lanes = Lanes::new("pair-starts");
    let first = lanes.address(FIRST);
    for _ in 0..6 {
        assert!(matches!(
            call(&lanes, SENDER, &format!("SYN {first}: active")),
            Ok(steering::Published::Sent { lanes: reached, .. }) if reached == [test_uuid(FIRST)]
        ));
    }
    assert!(
        lanes
            .messages()
            .iter()
            .all(|(_, body)| body.admission == Some(wire::body::PeerAdmission::Active)),
        "a running recipient takes every body as active delivery"
    );

    lanes.vacate(FIRST);
    assert!(matches!(
        call(&lanes, SENDER, &format!("SYN {first}: joins")),
        Ok(steering::Published::Sent { resuming, .. }) if resuming == [first]
    ));
    assert_eq!(
        newest_queued(&lanes),
        Some(wire::body::PeerAdmission::Cohort),
        "admitted bodies still pending already commit the lane's start"
    );
    for pending in lanes.pending(FIRST) {
        lanes.withdraw(pending.directive);
    }

    lanes.spend_starts(SENDER, FIRST);
    let refused = call(&lanes, SENDER, &format!("SYN {first}: one more"));
    let Err(steering::Unpublished::Recorded { reason, detail }) = &refused else {
        panic!("a pair with no start left publishes nothing: {refused:?}");
    };
    assert_eq!(reason, "steer::pair-starts-spent");
    assert!(
        detail.contains(&first.to_string()) && detail.contains("accrues at"),
        "{detail}"
    );
    assert!(
        lanes.pending(FIRST).is_empty(),
        "a refused body never becomes peer input"
    );

    assert!(matches!(
        call(&lanes, SECOND, &format!("SYN {first}: another pair")),
        Ok(steering::Published::Sent { resuming, .. }) if resuming == [first]
    ));
    assert!(
        matches!(
            newest_queued(&lanes),
            Some(wire::body::PeerAdmission::PairToken(spend)) if spend.remaining == 4
        ),
        "another sender's pair to the same lane holds its own starts"
    );
    lanes.finish();
}

#[test]
fn an_unlaunchable_vacant_lane_is_refused_before_input_or_pair_spend() {
    let lanes = Lanes::new("unlaunchable-pair-start");
    let first = lanes.address(FIRST);
    lanes.vacate_unlaunchable(FIRST);

    let refused = call(&lanes, SENDER, &format!("SYN {first}: cannot start"));
    let Err(steering::Unpublished::Recorded { reason, detail }) = &refused else {
        panic!("an unlaunchable lane publishes nothing: {refused:?}");
    };
    assert_eq!(reason, "steer::recipient-unlaunchable");
    assert!(
        detail.contains(&first.to_string()) && detail.contains("serve::lane-capability-invalid"),
        "{detail}"
    );
    assert!(
        lanes.pending(FIRST).is_empty(),
        "the refused body never becomes peer input"
    );
    let pair = lanes
        .bus
        .read(|read| read.peer_pair_head(test_uuid(SENDER), test_uuid(FIRST)))
        .expect("the pair projection reads");
    assert!(pair.is_none(), "the refused body spends no pair token");
    lanes.finish();
}

/// The exact first return to an opening its sender authored wakes that idle
/// sender without a start even when their pair has none left.
#[test]
fn a_first_return_wakes_its_idle_opener_once_without_a_start() {
    let lanes = Lanes::new("pair-first-return");
    let first = lanes.address(FIRST);
    lanes.author(SENDER, &format!("SYN {first}: is the parser settled?"));
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    lanes.declare(FIRST);
    let [opening] = <[_; 1]>::try_from(lanes.pending(FIRST)).expect("one opening");
    lanes.vacate(SENDER);
    lanes.spend_starts(FIRST, SENDER);

    lanes.answer(
        FIRST,
        &[&opening.key],
        &format!("ACK {}: Nearly.", opening.key),
    );
    let [returned] = <[_; 1]>::try_from(lanes.pending(SENDER)).expect("the return queues");
    assert!(returned.wakes, "the first return commits its wake");
    assert_eq!(
        newest_queued(&lanes),
        Some(wire::body::PeerAdmission::FirstReturn)
    );
    lanes.finish();
}
