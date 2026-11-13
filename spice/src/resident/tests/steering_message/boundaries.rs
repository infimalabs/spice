//! The resident's sweep closes a context only for the seated occupant whose
//! own declared session wrote the boundary.

use super::*;

/// End one agent turn on `lane`: a final assistant message that stopped by
/// itself, then the completed prompt that ran it.
fn end_turn(lanes: &Lanes, lane: u8) {
    lanes.land(
        lane,
        &[],
        serde_json::json!([{"type": "text", "text": "done"}]),
        "stop",
    );
    lanes
        .tape(lane)
        .tee(
            b"{\"type\":\"prompt_result\",\"id\":\"prompt-1\",\"agentInvoked\":true,\
              \"status\":\"completed\",\"sessionSettled\":true}\n",
        )
        .expect("the prompt result is durable");
}

/// The checkpoint a later judgment may reuse on `lane` after one more sweep.
fn swept(lanes: &Lanes, lane: u8) -> Option<wire::body::LaneCheckpoint> {
    lanes.restart();
    let lane = test_uuid(lane);
    lanes
        .bus
        .read(move |read| read.qualified_checkpoint(lane))
        .expect("the checkpoint index reads")
        .map(|head| head.body)
}

/// A clean stop closes the context of the occupant whose declared session
/// wrote it. An occupant that declared none closes nothing, and a successor
/// seated over its predecessor's session inherits that predecessor's
/// checkpoint under the predecessor's name until its own session stops.
#[test]
fn a_sweep_closes_a_context_only_for_the_occupant_whose_session_stopped() {
    let lanes = Lanes::new("boundary-sweep");
    let (first, successor) = (test_uuid(FIRST + 1), test_uuid(50));

    end_turn(&lanes, FIRST);
    assert_eq!(
        swept(&lanes, FIRST),
        None,
        "a stop before any declared session closes nothing"
    );

    lanes.declare(FIRST);
    end_turn(&lanes, FIRST);
    let closed = swept(&lanes, FIRST).expect("the declared occupant's stop closes its context");
    assert_eq!(
        (closed.occupant, closed.boundary),
        (first, wire::body::CheckpointBoundary::Turn)
    );

    lanes.vacate(FIRST);
    lanes.renew(FIRST, successor);
    end_turn(&lanes, FIRST);
    assert_eq!(
        swept(&lanes, FIRST),
        Some(closed.clone()),
        "a stop under the predecessor's session closes nothing for the successor"
    );

    lanes.declare(FIRST);
    end_turn(&lanes, FIRST);
    let renewed = swept(&lanes, FIRST).expect("the successor's own stop closes its context");
    assert_eq!(renewed.occupant, successor);
    assert_ne!(renewed.through, closed.through);
    lanes.finish();
}

const COMPACTED: &str = concat!(
    r#"{"type":"auto_compaction_end","action":"remote","result":{"summary":"kept","shortSummary":"kept"},"aborted":false,"willRetry":false}"#,
    "\n"
);

fn compact(lanes: &Lanes, lane: u8) {
    lanes
        .tape(lane)
        .tee(COMPACTED.as_bytes())
        .expect("the compaction is durable");
}

fn fail_turn(lanes: &Lanes, lane: u8) {
    lanes.land(
        lane,
        &[],
        serde_json::json!([{"type": "text", "text": "failed"}]),
        "error",
    );
    lanes
        .tape(lane)
        .tee(
            b"{\"type\":\"prompt_result\",\"id\":\"prompt-1\",\"agentInvoked\":true,\
              \"status\":\"completed\",\"sessionSettled\":true}\n",
        )
        .expect("the failed prompt result is durable");
}

fn settled_line(lanes: &Lanes) -> wire::intent::PendingDirective {
    lanes.declare(SENDER);
    lanes.declare(FIRST);
    let first = lanes.address(FIRST);
    lanes.author(SENDER, &format!("SYN {first}: inspect the parser"));
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    let opening = delivered(lanes, FIRST);
    lanes.answer(FIRST, &[&opening.key], &format!("ACK {}", opening.key));
    opening
}

fn automatic_finishes(lanes: &Lanes, line: CanonicalUuid) -> usize {
    lanes
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("line facts read")
        .facts()
        .iter()
        .filter_map(|envelope| envelope.fact.read::<wire::body::SteerLineTransition>())
        .filter(|transition| {
            transition.line == line
                && transition.action == wire::body::PeerLineAction::AutomaticFinish
        })
        .count()
}

/// A line closes after both lanes end a turn even when the first lane has
/// compacted since that turn and its occupant has left the seat.
#[test]
fn two_turns_close_one_fully_settled_line_after_vacancy() {
    let lanes = Lanes::new("line-turn-close");
    let opening = settled_line(&lanes);

    end_turn(&lanes, SENDER);
    assert_eq!(
        swept(&lanes, SENDER)
            .expect("the turn checkpoint qualifies")
            .boundary,
        wire::body::CheckpointBoundary::Turn
    );
    compact(&lanes, SENDER);
    assert_eq!(
        swept(&lanes, SENDER)
            .expect("the later compaction qualifies")
            .boundary,
        wire::body::CheckpointBoundary::Compaction
    );
    lanes.vacate(SENDER);
    end_turn(&lanes, FIRST);
    let _ = swept(&lanes, FIRST);

    assert_eq!(automatic_finishes(&lanes, opening.directive), 1);
    lanes.restart();
    assert_eq!(
        automatic_finishes(&lanes, opening.directive),
        1,
        "reconciliation is idempotent"
    );
    lanes.finish();
}

/// Compaction proves no conversation boundary, and one lane's clean turn
/// cannot close a line until its peer also ends a later turn.
#[test]
fn compaction_and_one_turn_leave_a_settled_line_open() {
    let lanes = Lanes::new("line-compaction-open");
    let opening = settled_line(&lanes);

    compact(&lanes, SENDER);
    compact(&lanes, FIRST);
    let _ = swept(&lanes, SENDER);
    let _ = swept(&lanes, FIRST);
    assert_eq!(automatic_finishes(&lanes, opening.directive), 0);

    end_turn(&lanes, SENDER);
    let _ = swept(&lanes, SENDER);
    assert_eq!(automatic_finishes(&lanes, opening.directive), 0);

    end_turn(&lanes, FIRST);
    let _ = swept(&lanes, FIRST);
    assert_eq!(automatic_finishes(&lanes, opening.directive), 1);
    lanes.finish();
}

/// A failed turn and the lane's later exit write no clean turn checkpoint.
#[test]
fn failed_turn_and_exit_leave_a_settled_line_open() {
    let lanes = Lanes::new("line-failed-turn-open");
    let opening = settled_line(&lanes);

    end_turn(&lanes, SENDER);
    let _ = swept(&lanes, SENDER);
    fail_turn(&lanes, FIRST);
    let _ = swept(&lanes, FIRST);
    lanes.vacate(FIRST);
    lanes.restart();

    assert_eq!(automatic_finishes(&lanes, opening.directive), 0);
    lanes.finish();
}
