//! Which accounted records close a clean context boundary, and for whom.

use journal::LaneCheckpointHead;
use journal::crossing::body::CheckpointBoundary;

use super::super::Occupancy;
use super::*;

const OCCUPANT: &str = "018f82d4-6f59-7d06-92e3-bce913116306";
const SUCCESSOR: &str = "018f82d4-6f59-7d06-92e3-bce913116307";
const RUN: &[u8] = br#"{"type":"agent_start"}"#;
const COMPACTED: &[u8] = br#"{"type":"auto_compaction_end","action":"remote","result":{"summary":"kept","shortSummary":"kept"},"aborted":false,"willRetry":false}"#;
const COMPACTION_FAILED: &[u8] = br#"{"type":"auto_compaction_end","action":"remote","aborted":false,"willRetry":false,"errorMessage":"input exceeds the context window"}"#;

fn stopped(reason: &str) -> Vec<u8> {
    format!(
        r#"{{"type":"message_end","message":{{"role":"assistant","content":[{{"type":"text","text":"done"}}],"stopReason":"{reason}"}}}}"#
    )
    .into_bytes()
}

fn prompt(invoked: bool, status: &str) -> Vec<u8> {
    format!(
        r#"{{"type":"prompt_result","id":"prompt-1","agentInvoked":{invoked},"status":"{status}","sessionSettled":true}}"#
    )
    .into_bytes()
}

fn occupied(root: &Path, occupant: &str, from: u64) -> Ingestion {
    Ingestion {
        occupancy: Some(Occupancy {
            occupant: occupant.parse().expect("fixture occupant"),
            from: position(1, from),
        }),
        ..ingestion(root)
    }
}

fn newest(journal: &Journal) -> Option<LaneCheckpointHead> {
    let lane: CanonicalUuid = LANE.parse().expect("fixture lane");
    journal
        .read(|read| read.lane_checkpoint(lane))
        .expect("the checkpoint index reads")
}

fn qualified(journal: &Journal) -> Option<LaneCheckpointHead> {
    let lane: CanonicalUuid = LANE.parse().expect("fixture lane");
    journal
        .read(|read| read.qualified_checkpoint(lane))
        .expect("the checkpoint index reads")
}

fn checkpoints(journal: &Journal) -> usize {
    journal
        .fact_snapshot()
        .expect("fact snapshot")
        .facts()
        .iter()
        .filter(|envelope| envelope.fact.kind() == FactKind::LaneCheckpoint)
        .count()
}

/// One ending: its label, the records the tape holds, and the boundary those
/// records close.
type Ending = (&'static str, Vec<Vec<u8>>, Option<CheckpointBoundary>);

/// A turn closes only when the prompt completed an agent run whose own last
/// assistant message stopped by itself, and a compaction only when it
/// completed. Every failed, cut, aborted, still-open, or unseen ending
/// accounts its bytes and closes nothing.
#[test]
fn only_a_clean_stop_or_a_completed_compaction_closes_a_boundary() {
    let done = prompt(true, "completed");
    let cases: Vec<Ending> = vec![
        (
            "stop",
            vec![stopped("stop"), done.clone()],
            Some(CheckpointBoundary::Turn),
        ),
        ("length", vec![stopped("length"), done.clone()], None),
        ("error", vec![stopped("error"), done.clone()], None),
        ("aborted", vec![stopped("aborted"), done.clone()], None),
        ("tool", vec![stopped("toolUse"), done.clone()], None),
        (
            "reopened",
            vec![stopped("stop"), RUN.to_vec(), done.clone()],
            None,
        ),
        ("unseen", vec![done.clone()], None),
        (
            "local",
            vec![stopped("stop"), prompt(false, "completed")],
            None,
        ),
        (
            "prompt-error",
            vec![stopped("stop"), prompt(true, "error")],
            None,
        ),
        (
            "prompt-aborted",
            vec![stopped("stop"), prompt(true, "aborted")],
            None,
        ),
        (
            "compacted",
            vec![COMPACTED.to_vec()],
            Some(CheckpointBoundary::Compaction),
        ),
        ("compaction-failed", vec![COMPACTION_FAILED.to_vec()], None),
    ];
    for (label, records, expected) in cases {
        let scratch = Scratch::new(&format!("boundary-{label}"));
        let tape = Tape::open(scratch.path()).expect("tape opens");
        let mut end = 0;
        for record in &records {
            tape.tee(&line(record)).expect("the record is durable");
            end += line(record).len() as u64;
        }
        let journal = &mut authority();
        let pass = ingest(journal, occupied(scratch.path(), OCCUPANT, 0));
        assert!(matches!(pass, Ok(Ingested::Accounted { .. })), "{label}");

        let closed = qualified(journal);
        assert_eq!(
            closed.as_ref().map(|closed| closed.body.boundary),
            expected,
            "{label}"
        );
        if let Some(closed) = closed {
            assert_eq!(closed.body.through, position(1, end), "{label}");
            assert_eq!(
                closed.body.occupant,
                OCCUPANT.parse().expect("fixture occupant"),
                "{label}"
            );
        }
        scratch.remove();
    }
}

/// A boundary belongs to the occupant that wrote it. A vacant lane closes
/// nothing, records before the seated occupant's declaration are its
/// predecessor's and close nothing for it, and its own first clean stop is
/// the first checkpoint the lane has.
#[test]
fn a_predecessors_stop_never_closes_its_successors_context() {
    let scratch = Scratch::new("succession");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let journal = &mut authority();
    let done = prompt(true, "completed");

    tape.tee(&line(&stopped("stop"))).expect("durable");
    tape.tee(&line(&done)).expect("durable");
    let vacant = ingest(journal, ingestion(scratch.path()));
    assert!(matches!(vacant, Ok(Ingested::Accounted { .. })));
    assert_eq!(newest(journal), None, "a vacant lane closed a boundary");

    tape.tee(&line(&stopped("stop"))).expect("durable");
    tape.tee(&line(&done)).expect("durable");
    let turn = (line(&stopped("stop")).len() + line(&done).len()) as u64;
    let declared = 2 * turn;
    let inherited = ingest(journal, occupied(scratch.path(), SUCCESSOR, declared));
    assert!(matches!(inherited, Ok(Ingested::Accounted { .. })));
    assert_eq!(newest(journal), None, "a predecessor's stop was inherited");

    tape.tee(&line(&stopped("stop"))).expect("durable");
    tape.tee(&line(&done)).expect("durable");
    let own = ingest(journal, occupied(scratch.path(), SUCCESSOR, declared));
    assert!(matches!(own, Ok(Ingested::Accounted { .. })));
    let closed = qualified(journal).expect("the successor's own stop closes");
    assert_eq!(
        closed.body.occupant,
        SUCCESSOR.parse().expect("fixture occupant")
    );
    assert_eq!(closed.body.through, position(1, 3 * turn));
    scratch.remove();
}

/// The stop and the prompt that reports it usually land in different passes.
/// The pass that continues exactly where the stop was read still closes the
/// turn, and it accounts at once rather than holding a boundary nobody can
/// read. A pass that did not read every record since the stop cannot know it
/// still stands, so it closes nothing.
#[test]
fn a_stop_reaches_only_the_pass_that_continues_from_it() {
    let scratch = Scratch::new("carried");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let journal = &mut authority();
    let bound = Bound::new(u64::MAX, usize::MAX);
    let done = prompt(true, "completed");
    let seated = |root: &Path| occupied(root, OCCUPANT, 0);

    let coalescing = &mut Coalescing::new();
    tape.tee(&line(&stopped("stop"))).expect("durable");
    let held = ingest_bounded(journal, seated(scratch.path()), coalescing, bound);
    assert!(matches!(held, Ok(Ingested::Deferred { .. })), "{held:?}");
    tape.tee(&line(&done)).expect("durable");
    let closed = ingest_bounded(journal, seated(scratch.path()), coalescing, bound);
    assert!(
        matches!(closed, Ok(Ingested::Accounted { .. })),
        "{closed:?}"
    );
    assert_eq!(checkpoints(journal), 1, "the held stop closed no turn");

    // The same stream, after its hold was accounted: the ending still stands
    // for the pass that continues from the accounted head.
    tape.tee(&line(&stopped("stop"))).expect("durable");
    settle(journal, seated(scratch.path()), coalescing);
    tape.tee(&line(&done)).expect("durable");
    settle(journal, seated(scratch.path()), coalescing);
    assert_eq!(checkpoints(journal), 2, "an accounted stop was forgotten");

    // A restarted resident holds nothing, so the prompt alone proves nothing.
    tape.tee(&line(&stopped("stop"))).expect("durable");
    settle(journal, seated(scratch.path()), coalescing);
    tape.tee(&line(&done)).expect("durable");
    settle(journal, seated(scratch.path()), &mut Coalescing::new());
    assert_eq!(checkpoints(journal), 2, "an unseen stop closed a turn");

    // Another pass accounted a failed ending this one never read.
    tape.tee(&line(&stopped("stop"))).expect("durable");
    settle(journal, seated(scratch.path()), coalescing);
    tape.tee(&line(&stopped("error"))).expect("durable");
    settle(journal, seated(scratch.path()), &mut Coalescing::new());
    tape.tee(&line(&done)).expect("durable");
    settle(journal, seated(scratch.path()), coalescing);
    assert_eq!(checkpoints(journal), 2, "a superseded stop closed a turn");
    scratch.remove();
}

/// A checkpoint is reusable only while its stream's accounted head still
/// covers it. Replacing the tape generation leaves the fact in history and
/// withdraws its qualification.
#[test]
fn a_replaced_generation_withdraws_the_checkpoint_it_carried() {
    let scratch = Scratch::new("withdrawn");
    let root = scratch.path().join("tape");
    let tape = Tape::open(&root).expect("tape opens");
    let journal = &mut authority();
    tape.tee(&line(&stopped("stop"))).expect("durable");
    tape.tee(&line(&prompt(true, "completed")))
        .expect("durable");
    let first = ingest(journal, occupied(&root, OCCUPANT, 0));
    assert!(matches!(first, Ok(Ingested::Accounted { .. })));
    assert!(
        qualified(journal).is_some(),
        "the clean stop did not qualify"
    );

    fs::remove_dir_all(&root).expect("the generation is withdrawn");
    let replacement = Tape::open(&root).expect("the replacement opens");
    replacement.tee(&line(PROSE)).expect("durable");
    let second = ingest(journal, occupied(&root, OCCUPANT, 0));
    assert!(matches!(second, Ok(Ingested::Accounted { .. })));

    assert!(newest(journal).is_some(), "history lost the checkpoint");
    assert_eq!(qualified(journal), None, "a retired generation qualified");
    scratch.remove();
}

/// One immediate pass over whatever the tape gained.
fn settle(journal: &mut Journal, ingestion: Ingestion, coalescing: &mut Coalescing) {
    let pass = ingest_bounded(journal, ingestion, coalescing, Bound::IMMEDIATE);
    assert!(matches!(pass, Ok(Ingested::Accounted { .. })), "{pass:?}");
}
