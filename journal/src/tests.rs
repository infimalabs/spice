use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::{Connection, params};
use serde_json::json;
use wire::{
    Actor, AnyFact, CanonicalUuid, ClaimDisposition, ClaimHead, Command, CommandKind, Digest,
    Envelope, FactKind, Instant, LaneResidency, LeaseHead, Origin, Outcome, Phase, Reply,
    ReplyPosition, ResidencyHead, ResidentState, Response, Seq, SeqRange, TaskHead, TaskStamp,
    TrustHead, TrustState, WireVersion, Witness, WorkflowHead,
};

use wire::body::TranscriptPosition;
use wire::head::TranscriptSourceHead;

use crate::guard::{indexed_read_metrics, reset_indexed_read_metrics};
use crate::{
    FactPageStart, FactReadBudget, Journal, JournalReader, MAX_DURABLE_FACT_ENVELOPE_BYTES,
    MAX_RETAINED_FACT_PAGE_BYTES, MAX_RETAINED_FACT_PAGE_ROWS, MigrationError, OpenError,
    SCHEMA_VERSION, ViolationClass,
};

mod boundary;
mod claim_index;
mod fact_bounds;
mod generation;
mod page_budget;
mod peer_line_verification;
mod read;
mod release_landings;
mod renewal;
mod rooms;
mod schema;
mod steering_index;
mod steering_verification;
mod task_intent;
mod thread;
mod transcript;
mod wal;

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

/// The journal position of a fixture's `ordinal`th fact.
fn seq(ordinal: u64) -> Seq {
    Seq::new(Seq::FIRST.get() + ordinal - 1).expect("fixture position is nonzero")
}

fn fact_budget(rows: usize) -> FactReadBudget {
    FactReadBudget::new(rows, MAX_RETAINED_FACT_PAGE_BYTES)
        .expect("fixture rows form a repository fact budget")
}

#[test]
fn new_journals_start_at_the_four_character_sequence_boundary() {
    let mut journal = Journal::in_memory().expect("create empty journal");
    let first_sequence = Seq::FIRST;
    let second_sequence = seq(2);
    let third_sequence = seq(3);
    let mut first = crate::JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:1kKRRsDPx".parse().expect("fixture origin"),
        instant(),
        wire::Producer::ResidentRecognition,
    );
    for note in ["first fact", "second fact"] {
        first
            .fact(text_fact(note.to_owned()), crate::FactCause::Root)
            .expect("initial fact builds");
    }
    let first = journal.apply(first).expect("initial facts commit");
    assert_eq!(
        (first.first(), first.last()),
        (first_sequence, second_sequence)
    );
    assert_eq!(
        journal.read(|read| read.cursor()).unwrap(),
        Some(second_sequence)
    );

    let mut next = crate::JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:1kKRRsDPx".parse().expect("fixture origin"),
        instant(),
        wire::Producer::ResidentRecognition,
    );
    next.fact(
        text_fact("next fact".to_owned()),
        crate::FactCause::Existing(second_sequence),
    )
    .expect("next fact builds");
    let next = journal.apply(next).expect("next fact commits");
    assert_eq!(
        (next.first(), next.last()),
        (third_sequence, third_sequence)
    );
    journal
        .verify()
        .expect("the four-character journal stream verifies");
}

#[test]
fn a_fact_stream_that_begins_past_the_first_position_refuses() {
    let late = fact_at(
        seq(2),
        None,
        FactKind::JournalBacked,
        text_body("a fact with no first position before it".to_owned()),
    );
    assert!(crate::FactSnapshot::from_facts(vec![late.clone()]).is_err());

    let journal = Journal::in_memory().expect("create empty journal");
    insert_fact(&journal, late);
    assert!(refused_classes(&journal).contains(&ViolationClass::Sequence));
}

#[test]
fn coherent_authority_verifies_every_required_plane() {
    let journal = authority_fixture();

    let verified = journal.verify().expect("coherent fixture verifies");
    assert_eq!(verified.facts, 2);
    assert_eq!(verified.threads, 2);
    assert_eq!(verified.receipts, 1);
    assert_eq!(verified.heads, 9);
}

#[test]
fn an_authoritative_receipt_cannot_claim_a_daemonless_direct_position() {
    let journal = authority_fixture();
    let response = Response {
        v: WireVersion::CURRENT,
        id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c4"),
        epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
        outcome: Outcome::Reply(Reply {
            position: ReplyPosition::Direct {},
            body: json!({"task": "JOURNAL-a1", "status": "review"}),
        }),
    };
    journal
        .connection
        .execute(
            "UPDATE command_receipts SET response = ?1",
            params![wire::encode(&response).expect("encode direct response")],
        )
        .expect("plant direct response in authoritative receipt");

    assert!(
        refused_violations(&journal)
            .iter()
            .any(|violation| violation.class == ViolationClass::Receipt)
    );
}

#[test]
fn fact_snapshot_reads_the_canonical_authority_stream() {
    let journal = authority_fixture();
    let snapshot = journal.fact_snapshot().expect("fact snapshot");

    assert_eq!(snapshot.facts().len(), 2);
    assert_eq!(snapshot.horizon(), Some(seq(2)));
    assert_eq!(snapshot.facts()[0].fact.kind(), FactKind::TaskAdded);
    assert_eq!(snapshot.facts()[1].fact.kind(), FactKind::TaskPhased);
}

/// The authority fixture extended to one 200-fact cause thread rooted at 1.
fn long_thread_fixture() -> Journal {
    let journal = authority_fixture();
    for position in 3..=200 {
        extend_thread(
            &journal,
            position,
            FactKind::TaskPhased,
            json!({"task": "1kKRRsDPx", "phase": "review"}),
        );
    }
    advance_cursor(&journal, 200);
    journal
}

/// Append the fixture's `ordinal`th fact, caused by the one before it, to the
/// indexed thread rooted at the first.
fn extend_thread(journal: &Journal, ordinal: u64, kind: FactKind, body: serde_json::Value) {
    insert_fact(journal, fact(ordinal, Some(ordinal - 1), kind, body));
    journal
        .connection
        .execute(
            "INSERT INTO thread_index(seq, origin, cause, root, depth) \
             VALUES (?1, 'task:1kKRRsDPx', ?2, ?3, ?4)",
            params![
                sql_seq(ordinal),
                sql_seq(ordinal - 1),
                sql_seq(1),
                i64::try_from(ordinal - 1).expect("fixture depth fits SQLite"),
            ],
        )
        .expect("extend fixture thread");
}

/// Move the fixture's cursor to its `ordinal`th position.
fn advance_cursor(journal: &Journal, ordinal: u64) {
    journal
        .connection
        .execute(
            "UPDATE cursor_head SET seq = ?1 WHERE singleton = 1",
            [sql_seq(ordinal)],
        )
        .expect("advance fixture cursor");
}

#[test]
fn indexed_canvas_pages_decode_only_the_bounded_tail_and_causal_window() {
    let journal = long_thread_fixture();
    reset_indexed_read_metrics();
    let causal = journal
        .read(|read| read.causal_page(seq(200), fact_budget(2)))
        .expect("indexed causal page reads")
        .expect("indexed causal position exists");
    assert_eq!(
        causal.rows.iter().map(|fact| fact.seq).collect::<Vec<_>>(),
        [seq(200), seq(199)]
    );
    assert_eq!(
        causal.overflow.expect("causal page names its tail"),
        wire::Overflow {
            omitted: 198,
            continuation: seq(198),
        }
    );
    let causal_metrics = indexed_read_metrics();
    assert_eq!((causal_metrics.queries, causal_metrics.rows), (1, 3));
    assert!(causal_metrics.decoded_bytes > 0);

    reset_indexed_read_metrics();
    let tail = journal
        .read(|read| {
            read.fact_page(
                FactPageStart::After(Some(seq(198))),
                seq(200),
                fact_budget(1),
            )
        })
        .expect("indexed fact tail reads");
    assert_eq!(tail.rows[0].seq, seq(199));
    assert_eq!(
        tail.overflow.expect("fact page names its tail"),
        wire::Overflow {
            omitted: 1,
            continuation: seq(200),
        }
    );
    let tail_metrics = indexed_read_metrics();
    assert_eq!((tail_metrics.queries, tail_metrics.rows), (1, 2));
    assert!(tail_metrics.decoded_bytes > 0);
}

#[test]
fn read_only_snapshot_and_thread_page_follow_the_cause_chain() {
    let scratch = Scratch::new();
    let path = scratch.path().join("authority.sqlite3");
    drop(authority_file_fixture(&path));

    let reader = JournalReader::open(&path).expect("open authority read-only");
    let snapshot = reader.fact_snapshot().expect("read-only fact snapshot");
    assert_eq!(snapshot.horizon(), Some(seq(2)));

    let (root, thread) = reader
        .read(|read| {
            let root = read
                .thread_root(&seq(2).to_string())
                .expect("resolve descendant sequence");
            Ok((root, read.thread_page(root, None, fact_budget(4))?))
        })
        .expect("read-only thread page");
    let thread = thread.expect("an unanchored page opens the thread");
    assert_eq!(root, seq(1));
    assert_eq!(
        thread
            .rows
            .iter()
            .map(|entry| entry.seq)
            .collect::<Vec<_>>(),
        [seq(1), seq(2)]
    );
    assert_eq!(thread.rows[1].cause, Some(seq(1)));
}

#[test]
fn reader_verification_keeps_every_plane_on_the_snapshot_before_a_concurrent_commit() {
    let scratch = Scratch::new();
    let path = scratch.path().join("authority.sqlite3");
    let mut writer = authority_file_fixture(&path);
    let reader = JournalReader::open(&path).expect("open immutable verification reader");
    let reader_changes = reader.connection.total_changes();
    let mut committed = false;

    let (verified, snapshot) =
        crate::replay::verify_reader_connection_with_concurrent_commit(&reader.connection, || {
            let mut write = crate::JournalWrite::new(
                Actor::TEST_PRINCIPAL,
                "task:1kKRRsDPx".parse().expect("fixture origin"),
                instant(),
                wire::Producer::ResidentRecognition,
            );
            write
                .fact(
                    text_fact("committed between verification planes".to_owned()),
                    crate::FactCause::Existing(seq(2)),
                )
                .expect("concurrent fact builds");
            let applied = writer.apply(write).expect("concurrent generation commits");
            assert_eq!(applied.last(), seq(3));
            committed = true;
        })
        .expect("one old immutable generation verifies");

    assert!(
        committed,
        "the fixture never crossed the verification planes"
    );
    assert_eq!((verified.facts, verified.threads), (2, 2));
    assert_eq!(snapshot.horizon(), Some(seq(2)));
    assert_eq!(reader.connection.total_changes(), reader_changes);

    let current = reader
        .verify()
        .expect("the concurrently committed generation verifies");
    assert_eq!((current.facts, current.threads), (3, 3));
    assert_eq!(reader.connection.total_changes(), reader_changes);
}

#[test]
fn reader_verification_retains_complete_genuine_violation_diagnostics() {
    let scratch = Scratch::new();
    let path = scratch.path().join("authority.sqlite3");
    let writer = authority_file_fixture(&path);
    writer
        .connection
        .execute_batch(&format!(
            "UPDATE thread_index SET depth = 9 WHERE seq = {second};
             DROP TRIGGER cursor_head_monotone;
             UPDATE cursor_head SET seq = {first}, wake = {first} WHERE singleton = 1;",
            first = sql_seq(1),
            second = sql_seq(2),
        ))
        .expect("plant genuine thread and cursor defects");
    drop(writer);

    let reader = JournalReader::open(&path).expect("open corrupt authority read-only");
    let reader_changes = reader.connection.total_changes();
    let violations = reader
        .verify()
        .expect_err("genuine authority defects must refuse")
        .violations()
        .to_vec();

    let thread = violations
        .iter()
        .find(|violation| violation.class == ViolationClass::Thread)
        .expect("thread violation survives the reader boundary");
    assert_eq!(thread.subject, sql_seq(2).to_string());
    assert_eq!(
        thread.detail,
        "thread row disagrees with replayed origin, cause, root, or depth"
    );
    assert_eq!(
        thread.to_string(),
        format!(
            "Thread:{}:thread row disagrees with replayed origin, cause, root, or depth",
            sql_seq(2)
        )
    );

    let cursor = violations
        .iter()
        .find(|violation| violation.class == ViolationClass::Cursor)
        .expect("cursor violation survives the reader boundary");
    assert_eq!(cursor.subject, "cursor_head:1");
    assert_eq!(
        cursor.detail,
        format!(
            "cursor seq Some({first}), wake Some({first}), and fact maximum Some({second}) disagree",
            first = sql_seq(1),
            second = sql_seq(2),
        )
    );
    assert_eq!(reader.connection.total_changes(), reader_changes);
}

#[test]
fn reindex_rebuilds_only_derived_threads_and_rolls_back_on_bad_history() {
    let mut journal = authority_fixture();
    let head_before = journal
        .connection
        .query_row(
            "SELECT body FROM task_heads WHERE stamp = '1kKRRsDPx'",
            [],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .expect("read task head before reindex");
    journal
        .connection
        .execute("DELETE FROM thread_index", [])
        .expect("remove derived thread rows");

    let rebuilt = journal.reindex().expect("reindex valid history");
    assert_eq!((rebuilt.facts, rebuilt.threads), (2, 2));
    journal.verify().expect("rebuilt authority verifies");
    let head_after = journal
        .connection
        .query_row(
            "SELECT body FROM task_heads WHERE stamp = '1kKRRsDPx'",
            [],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .expect("read task head after reindex");
    assert_eq!(head_after, head_before, "reindex changed an authority head");

    journal
        .connection
        .execute(
            "UPDATE fact_envelopes SET envelope = X'00' WHERE seq = ?1",
            [sql_seq(2)],
        )
        .expect("corrupt immutable fact fixture");
    let threads_before = thread_index_bytes(&journal.connection);
    let error = journal
        .reindex()
        .expect_err("corrupt history must refuse reindex");
    assert!(matches!(error, crate::ReindexError::Violations(_)));
    assert_eq!(
        thread_index_bytes(&journal.connection),
        threads_before,
        "failed reindex changed derived rows"
    );
}

fn thread_index_bytes(connection: &Connection) -> Vec<(i64, String, Option<i64>, i64, i64)> {
    let mut statement = connection
        .prepare("SELECT seq, origin, cause, root, depth FROM thread_index ORDER BY seq")
        .expect("prepare thread snapshot");
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .expect("query thread snapshot")
        .map(|row| row.expect("read thread row"))
        .collect()
}

#[test]
fn verify_names_envelope_head_receipt_cursor_and_thread_defects() {
    let classes = refused_classes(&content_defect_fixture());
    assert!(classes.contains(&ViolationClass::Envelope));
    assert!(classes.contains(&ViolationClass::Head));
    assert!(classes.contains(&ViolationClass::Receipt));
    assert!(classes.contains(&ViolationClass::Cursor));
    assert!(classes.contains(&ViolationClass::Thread));
}

#[test]
fn verify_names_schema_and_sequence_defects() {
    let violations = refused_violations(&structure_defect_fixture());
    let sequence = violations
        .iter()
        .find(|violation| violation.class == ViolationClass::Sequence)
        .expect("sequence plane names the history gap");
    assert_eq!(sequence.subject, sql_seq(4).to_string());

    let schema = violations
        .iter()
        .find(|violation| violation.class == ViolationClass::Schema)
        .expect("schema plane names the unresolved foreign key");
    assert!(schema.subject.contains("thread_index"));
    assert!(schema.detail.contains("fact_envelopes"));
}

#[test]
fn violation_errors_render_every_class_subject_and_detail() {
    let cursor = crate::Violation {
        class: ViolationClass::Cursor,
        subject: "cursor_head:1".to_owned(),
        detail: concat!(
            "cursor seq Some(8914), wake None, and fact maximum ",
            "Some(8913) disagree"
        )
        .to_owned(),
    };
    let thread = crate::Violation {
        class: ViolationClass::Thread,
        subject: "8914".to_owned(),
        detail: "fact has no thread row".to_owned(),
    };

    assert_eq!(
        crate::VerifyError::Violations(vec![cursor.clone()]).to_string(),
        concat!(
            "journal verification found 1 violation(s): ",
            "Cursor:cursor_head:1:cursor seq Some(8914), wake None, ",
            "and fact maximum Some(8913) disagree"
        )
    );
    assert_eq!(
        crate::ReindexError::Violations(vec![cursor, thread]).to_string(),
        concat!(
            "journal reindex refused 2 immutable-history violation(s): ",
            "Cursor:cursor_head:1:cursor seq Some(8914), wake None, ",
            "and fact maximum Some(8913) disagree | ",
            "Thread:8914:fact has no thread row"
        )
    );
}

#[test]
fn every_violation_plane_has_a_fixture_that_produces_it() {
    // `ViolationClass` declares itself closed, and a closed set earns nothing
    // unless something enforces it. `plane_fixture` matches it exhaustively
    // from outside `replay`, so a new plane fails the build here until it is
    // named. This assertion then proves each plane is actually reached, which
    // is what stops a plane from being declared and never verified.
    let mut observed = refused_classes(&content_defect_fixture());
    observed.extend(refused_classes(&structure_defect_fixture()));

    for class in [
        ViolationClass::Schema,
        ViolationClass::Envelope,
        ViolationClass::Sequence,
        ViolationClass::Thread,
        ViolationClass::Head,
        ViolationClass::Receipt,
        ViolationClass::Cursor,
    ] {
        assert!(
            observed.contains(&class),
            "no fixture produces the {class:?} plane; {} is where it belongs",
            plane_fixture(class),
        );
    }
}

/// The fixture that is supposed to produce each violation plane.
///
/// This is a match rather than a list so that adding a plane fails the build
/// naming the variant nobody assigned a fixture to.
fn plane_fixture(class: ViolationClass) -> &'static str {
    match class {
        ViolationClass::Envelope
        | ViolationClass::Thread
        | ViolationClass::Head
        | ViolationClass::Receipt
        | ViolationClass::Cursor => "content_defect_fixture",
        ViolationClass::Schema | ViolationClass::Sequence => "structure_defect_fixture",
    }
}

fn refused_classes(journal: &Journal) -> BTreeSet<ViolationClass> {
    refused_violations(journal)
        .into_iter()
        .map(|violation| violation.class)
        .collect()
}

fn refused_violations(journal: &Journal) -> Vec<crate::Violation> {
    journal
        .verify()
        .expect_err("a corrupt authority must fail verification")
        .violations()
        .to_vec()
}

/// An authority whose rows decode but disagree with what they are stored under.
fn content_defect_fixture() -> Journal {
    let journal = authority_fixture();
    journal
        .connection
        .execute(
            "UPDATE fact_envelopes SET envelope = X'00' WHERE seq = ?1",
            [sql_seq(2)],
        )
        .expect("corrupt envelope fixture");

    let mut wrong_task = task_head();
    wrong_task.born = "1kKRRsDQx".parse().expect("alternate task stamp");
    journal
        .connection
        .execute(
            "UPDATE task_heads SET body = ?1 WHERE stamp = '1kKRRsDPx'",
            params![wire::encode(&wrong_task).expect("encode wrong task")],
        )
        .expect("corrupt head fixture");
    journal
        .connection
        .execute(
            "UPDATE command_receipts SET command_id = '018f82d4-6f59-7d06-92e3-bce913116399'",
            [],
        )
        .expect("corrupt receipt fixture");
    journal
        .connection
        .execute(
            "UPDATE thread_index SET depth = 9 WHERE seq = ?1",
            [sql_seq(2)],
        )
        .expect("corrupt thread fixture");
    journal
        .connection
        .execute_batch(&format!(
            "DROP TRIGGER cursor_head_monotone; \
             UPDATE cursor_head SET seq = {first}, wake = {first};",
            first = sql_seq(1),
        ))
        .expect("corrupt cursor fixture");
    journal
}

/// An authority whose history has a hole and whose index points at nothing.
///
/// Both defects are invisible to the content checks: every stored row here
/// decodes and agrees with its own columns. What is wrong is the shape of the
/// history around them, which is the only thing the sequence and schema planes
/// look at.
fn structure_defect_fixture() -> Journal {
    let journal = authority_fixture();
    insert_fact(
        &journal,
        fact(
            4,
            None,
            FactKind::JournalBacked,
            text_body("Journal fixture fact".to_owned()),
        ),
    );
    journal
        .connection
        .execute_batch(&format!(
            "INSERT INTO thread_index(seq, origin, cause, root, depth) VALUES ({fourth}, 'task:JOURNAL-a1', NULL, {fourth}, 0);
             UPDATE cursor_head SET seq = {fourth} WHERE singleton = 1;
             PRAGMA foreign_keys = OFF;
             INSERT INTO thread_index(seq, origin, cause, root, depth) VALUES ({orphan}, 'task:JOURNAL-a1', NULL, {orphan}, 0);
             PRAGMA foreign_keys = ON;",
            fourth = sql_seq(4),
            orphan = sql_seq(9),
        ))
        .expect("open a hole at seq 3 and orphan a thread row");
    journal
}

#[test]
fn lease_heads_must_carry_an_ordered_life() {
    // One way to put a lease's instants out of order. The lease row is the one
    // head with no key to disagree with, so before this its body could say
    // anything at all and still verify.
    type Disorder = fn(&mut LeaseHead);

    let cases: [(Disorder, &str); 3] = [
        (
            |lease| lease.renewal = earlier(),
            "renewal precedes the start of possession",
        ),
        (
            |lease| lease.expiry = earlier(),
            "expiry precedes its most recent renewal",
        ),
        (
            |lease| lease.maximum = earlier(),
            "expiry passes the maximum renewal may not extend beyond",
        ),
    ];

    for (disorder, expected) in cases {
        let journal = authority_fixture();
        let mut lease = lease_head();
        disorder(&mut lease);
        journal
            .connection
            .execute(
                "UPDATE lease_heads SET body = ?1 WHERE stamp = '1kKRRsDPx'",
                params![wire::encode(&lease).expect("encode disordered lease")],
            )
            .expect("corrupt lease fixture");

        let error = journal
            .verify()
            .expect_err("a disordered lease must fail verification");
        let details = error
            .violations()
            .iter()
            .filter(|violation| violation.class == ViolationClass::Head)
            .map(|violation| violation.detail.clone())
            .collect::<Vec<_>>();
        assert!(
            details.iter().any(|detail| detail.contains(expected)),
            "no head violation named {expected:?}, found {details:?}",
        );
    }
}

fn authority_fixture() -> Journal {
    populate_authority(Journal::in_memory().expect("create authority fixture"))
}

fn authority_file_fixture(path: &Path) -> Journal {
    populate_authority(Journal::open(path).expect("create file authority fixture"))
}

fn populate_authority(journal: Journal) -> Journal {
    insert_fact(
        &journal,
        fact(
            1,
            None,
            FactKind::TaskAdded,
            json!({
                "task": "1kKRRsDPx",
                "title": "Journal fixture",
                "pitch": "A fixture needs one task to read.",
                "topic": {"path": "/journal"},
                "depends": [],
                "flow": ["draft", "todo", "review"],
                "phase": "todo",
                "revision": seq(1),
                "creation": {"kind": "unavailable", "reason": "principal-actor"}
            }),
        ),
    );
    insert_fact(
        &journal,
        fact(
            2,
            Some(1),
            FactKind::TaskPhased,
            json!({"task": "1kKRRsDPx", "phase": "review"}),
        ),
    );
    journal
        .connection
        .execute(
            "INSERT INTO thread_index(seq, origin, cause, root, depth) VALUES (?1, 'task:1kKRRsDPx', NULL, ?1, 0), (?2, 'task:1kKRRsDPx', ?1, ?1, 1)",
            [sql_seq(1), sql_seq(2)],
        )
        .expect("insert thread fixture");
    insert_heads(&journal);
    insert_receipt(&journal);
    journal
        .connection
        .execute_batch(&format!(
            "UPDATE cursor_head SET seq = {second}, wake = {first} WHERE singleton = 1;
             UPDATE steering_index SET indexed_seq = {second} WHERE singleton = 1;",
            first = sql_seq(1),
            second = sql_seq(2),
        ))
        .expect("advance cursor fixture");
    journal
}

fn insert_fact(journal: &Journal, envelope: Envelope<AnyFact>) {
    let bytes = wire::encode(&envelope).expect("encode fact fixture");
    let kind = envelope.fact.kind().as_str();
    let seq = i64::try_from(envelope.seq.get()).expect("fixture seq fits SQLite");
    let cause = envelope
        .cause
        .map(Seq::get)
        .map(|cause| i64::try_from(cause).expect("fixture cause fits SQLite"));
    journal
        .connection
        .execute(
            "INSERT INTO fact_envelopes(seq, wire_version, instant, actor, origin, cause, kind, envelope) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                seq, envelope.v.0, envelope.at.to_string(), envelope.actor.to_string(),
                envelope.origin.as_str(), cause, kind, bytes,
            ],
        )
        .expect("insert fact fixture");
}

/// One fixture fact at the `ordinal`th journal position, caused by the
/// `cause`th.
fn fact(
    ordinal: u64,
    cause: Option<u64>,
    kind: FactKind,
    body: serde_json::Value,
) -> Envelope<AnyFact> {
    fact_at(seq(ordinal), cause.map(seq), kind, body)
}

/// One fixture fact at `position`, caused by `cause`.
fn fact_at(
    position: Seq,
    cause: Option<Seq>,
    kind: FactKind,
    body: serde_json::Value,
) -> Envelope<AnyFact> {
    let mut fields = body.as_object().cloned().expect("a fact body is an object");
    fields.insert(
        "kind".to_owned(),
        serde_json::to_value(kind).expect("a fact kind encodes"),
    );
    let bytes = serde_json::to_vec(&fields).expect("fact fields encode");
    let fact = wire::decode(&bytes).expect("fact fields match the registered body");
    Envelope {
        v: WireVersion::CURRENT,
        seq: position,
        at: instant(),
        actor: Actor::Spice,
        origin: "task:1kKRRsDPx".parse::<Origin>().expect("fixture origin"),
        cause,
        fact,
    }
}

fn insert_heads(journal: &Journal) {
    insert_task_head(&journal.connection, "1kKRRsDPx", seq(1), &task_head());
    insert_head(
        &journal.connection,
        "workflow_heads",
        "stamp",
        "1kKRRsDPx",
        2,
        &workflow_head(),
    );
    insert_head(
        &journal.connection,
        "completion_heads",
        "stamp",
        "1kKRRsDPx",
        2,
        &wire::CompletionHead::opened(seq(2)),
    );
    journal
        .connection
        .execute(
            "INSERT INTO claim_heads(stamp, claimed, head_seq, body) VALUES ('1kKRRsDPx', ?1, ?1, ?2)",
            params![
                sql_seq(2),
                wire::encode(&claim_head()).expect("encode claim head")
            ],
        )
        .expect("insert claim head fixture");
    journal
        .connection
        .execute(
            "INSERT INTO claim_index(stamp, claimed, actor, lane, disposition, head_seq) \
             VALUES ('1kKRRsDPx', ?1, 'spice', '018f82d4-6f59-7d06-92e3-bce913116302', 'active', ?1)",
            [sql_seq(2)],
        )
        .expect("insert claim index fixture");
    journal
        .connection
        .execute(
            "INSERT INTO lease_heads(stamp, claimed, head_seq, body) VALUES ('1kKRRsDPx', ?1, ?1, ?2)",
            params![
                sql_seq(2),
                wire::encode(&lease_head()).expect("encode lease head")
            ],
        )
        .expect("insert lease head fixture");
    let trust_digest = digest("a");
    insert_head(
        &journal.connection,
        "trust_heads",
        "digest",
        trust_digest.as_str(),
        2,
        &TrustHead {
            digest: trust_digest.clone(),
            state: TrustState::Pended,
            worktree: None,
        },
    );
    let transcript = transcript_source_head();
    journal
        .connection
        .execute(
            "INSERT INTO transcript_source_heads(stream, lane, head_seq, body) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                transcript.key().as_str(),
                transcript.lane.to_string(),
                sql_seq(2),
                wire::encode(&transcript).expect("encode transcript source head")
            ],
        )
        .expect("insert transcript source head fixture");
    journal
        .connection
        .execute(
            "INSERT INTO residency_head(singleton, head_seq, body) VALUES (1, ?1, ?2)",
            params![
                sql_seq(2),
                wire::encode(&residency_head()).expect("encode residency")
            ],
        )
        .expect("insert residency head");
}

fn insert_head<T: serde::Serialize>(
    connection: &Connection,
    table: &str,
    key_column: &str,
    key: &str,
    seq: u64,
    head: &T,
) {
    let body = wire::encode(head).expect("encode head");
    let seq = sql_seq(seq);
    if table == "trust_heads" {
        let head: wire::TrustHead = wire::decode(&body).expect("decode trust head fixture");
        let rank = match head.state {
            wire::TrustState::Pended => 0,
            wire::TrustState::Granted => 1,
            wire::TrustState::Revoked => 2,
        };
        connection
            .execute(
                "INSERT INTO trust_heads(digest, head_seq, body, standing_rank) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![key, seq, body, rank],
            )
            .expect("insert trust head fixture");
        return;
    }
    let sql = format!("INSERT INTO {table}({key_column}, head_seq, body) VALUES (?1, ?2, ?3)");
    connection
        .execute(&sql, params![key, seq, body])
        .expect("insert head fixture");
}

/// Insert a task head in its stored form, creating its topic nodes at `at`.
fn insert_task_head(connection: &Connection, key: &str, at: Seq, head: &wire::TaskHead) {
    connection
        .execute(
            "INSERT INTO task_heads(stamp, head_seq, body) VALUES (?1, ?2, ?3)",
            params![
                key,
                sql_position(at),
                stored_task_head(connection, at, head)
            ],
        )
        .expect("insert task head fixture");
}

/// A task head's stored bytes, creating its topic nodes at `at`.
fn stored_task_head(connection: &Connection, at: Seq, head: &wire::TaskHead) -> Vec<u8> {
    crate::topic::store_task_head(head, |path| {
        crate::topic::resolve(connection, path, sql_position(at)).map_err(|error| error.to_string())
    })
    .expect("store task head fixture")
}

/// The `ordinal`th journal position as SQLite stores it.
fn sql_seq(ordinal: u64) -> i64 {
    sql_position(seq(ordinal))
}

/// A journal position as SQLite stores it.
fn sql_position(position: Seq) -> i64 {
    i64::try_from(position.get()).expect("fixture position fits SQLite")
}

fn insert_receipt(journal: &Journal) {
    let id = uuid("018f82d4-6f59-7d06-92e3-bce9131163c4");
    let epoch = uuid("018f82d4-6f59-7d06-92e3-bce9131163c3");
    let command = Command {
        v: WireVersion::CURRENT,
        id,
        epoch,
        kind: CommandKind::TaskDone,
        expected: Vec::new(),
        body: json!({"task": "JOURNAL-1kKRRsDPx", "validation": "fixture"}),
    };
    let response = Response {
        v: WireVersion::CURRENT,
        id,
        epoch,
        outcome: Outcome::Reply(Reply {
            position: ReplyPosition::Committed(SeqRange {
                first: seq(1),
                last: seq(2),
            }),
            body: json!({"task": "JOURNAL-1kKRRsDPx", "status": "review"}),
        }),
    };
    journal.connection.execute(
        "INSERT INTO command_receipts(command_id, actor, command_kind, request, evaluated_witnesses, response, first_seq, last_seq, snapshot_seq, epoch) VALUES (?1, 'principal:00000000-0000-0000-0000-000000000000', 'task::done', ?2, ?3, ?4, ?6, ?7, NULL, ?5)",
        params![
            id.to_string(), command.canonical_json().expect("canonical request").into_bytes(),
            wire::encode(&Vec::<Witness>::new()).expect("encode witnesses"),
            wire::encode(&response).expect("encode response"), epoch.to_string(),
            sql_seq(1), sql_seq(2),
        ],
    ).expect("insert command receipt");
}

fn task_head() -> TaskHead {
    TaskHead {
        preparation: None,
        intent: None,
        born: "1kKRRsDPx".parse().expect("task stamp"),
        topic_origin: seq(1),
        title: "Lay journal baseline".to_owned(),
        pitch: "A baseline makes every later read comparable.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic: wire::TaskTopic {
            path: Some("/journal".parse().expect("fixture topic")),
            placed: None,
            named: None,
            unmatched: false,
            nodes: None,
        },
        priority: None,
        taste: None,
        dependencies: Vec::new(),
        provenance: "task:1kKRRsDPx".parse().expect("provenance"),
        creation: wire::body::TaskCreationContext::unavailable(
            wire::body::TaskCreationUnavailable::SpiceActor,
        ),
        revision: seq(1),
    }
}

fn workflow_head() -> WorkflowHead {
    WorkflowHead {
        flow: vec![phase("draft"), phase("todo"), phase("review")],
        phase: phase("review"),
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    }
}

fn claim_head() -> ClaimHead {
    ClaimHead {
        actor: Actor::Spice,
        fact: seq(2),
        claimed: seq(2),
        cell: wire::ClaimCell {
            topic: Some("/journal".parse().expect("fixture topic")),
            phase: phase("review"),
        },
        lane: uuid("018f82d4-6f59-7d06-92e3-bce913116302"),
        occupant: uuid("018f82d4-6f59-7d06-92e3-bce913116303"),
        epoch: uuid("018f82d4-6f59-7d06-92e3-bce913116304"),
        recovery: None,
        disposition: ClaimDisposition::Active,
        acknowledged: 0,
    }
}

fn lease_head() -> LeaseHead {
    LeaseHead {
        claimed: seq(2),
        start: instant(),
        renewal: instant(),
        maximum: instant(),
        expiry: instant(),
        epoch: uuid("018f82d4-6f59-7d06-92e3-bce913116304"),
    }
}

fn transcript_source_head() -> TranscriptSourceHead {
    TranscriptSourceHead {
        lane: uuid("018f82d4-6f59-7d06-92e3-bce913116305"),
        vendor: "omp".parse().expect("canonical fixture vendor"),
        stream: "session".to_owned(),
        source: uuid("018f82d4-6f59-7d06-92e3-bce913116306"),
        next: TranscriptPosition {
            member: 1,
            offset: 4096,
        },
    }
}

fn residency_head() -> ResidencyHead {
    ResidencyHead {
        epoch: uuid("018f82d4-6f59-7d06-92e3-bce913116304"),
        state: ResidentState::Seated,
        topology_revision: 0,
        lanes: vec![LaneResidency {
            lane: uuid("018f82d4-6f59-7d06-92e3-bce913116302"),
            team: uuid("018f82d4-6f59-7d06-92e3-bce913116302"),
            name: None,
            worktree: "/worktrees/lane-a".to_owned(),
            life: "drive".to_owned(),
            vendor: "omp".parse().expect("fixture vendor"),
            model: None,
            effort: None,
            fast_mode: false,
            occupant: Some(uuid("018f82d4-6f59-7d06-92e3-bce913116303")),
            lineage: Vec::new(),
            capabilities: Vec::new(),
            session: None,
            disposition: "active".to_owned(),
        }],
        subscriptions: vec![wire::TeamSubscription {
            team: uuid("018f82d4-6f59-7d06-92e3-bce913116302"),
            formed_at: None,
            drive: true,
            drain: false,
            name: None,
            fast_mode: false,
        }],
        renewals: Vec::new(),
    }
}

fn schema_fixture(
    version: u32,
    state: &str,
    from: Option<u32>,
    to: Option<u32>,
    digest: Option<&str>,
) -> Connection {
    let connection = Connection::open_in_memory().expect("open schema fixture");
    connection.execute_batch(
        "CREATE TABLE journal_schema(singleton INTEGER PRIMARY KEY, version INTEGER NOT NULL, state TEXT NOT NULL, from_version INTEGER, to_version INTEGER, plan_digest TEXT);",
    ).expect("create schema fixture");
    connection.execute(
        "INSERT INTO journal_schema(singleton, version, state, from_version, to_version, plan_digest) VALUES (1, ?1, ?2, ?3, ?4, ?5)",
        params![version, state, from, to, digest],
    ).expect("insert schema fixture");
    connection
        .pragma_update(None, "user_version", version)
        .expect("set fixture version");
    connection
}

fn schema_file_fixture(
    path: &Path,
    version: u32,
    state: &str,
    from: Option<u32>,
    to: Option<u32>,
    digest: Option<&str>,
) -> Connection {
    let connection = Connection::open(path).expect("open schema file fixture");
    connection.execute_batch(
        "CREATE TABLE journal_schema(singleton INTEGER PRIMARY KEY, version INTEGER NOT NULL, state TEXT NOT NULL, from_version INTEGER, to_version INTEGER, plan_digest TEXT);",
    ).expect("create schema file fixture");
    connection.execute(
        "INSERT INTO journal_schema(singleton, version, state, from_version, to_version, plan_digest) VALUES (1, ?1, ?2, ?3, ?4, ?5)",
        params![version, state, from, to, digest],
    ).expect("insert schema file fixture");
    connection
        .pragma_update(None, "user_version", version)
        .expect("set fixture version");
    connection
}

fn directory_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fs::read_dir(root)
        .expect("read schema scratch")
        .map(|entry| {
            let path = entry.expect("read schema scratch entry").path();
            let relative = path
                .strip_prefix(root)
                .expect("entry stays below scratch")
                .to_path_buf();
            let bytes = fs::read(&path).expect("read schema scratch file");
            (relative, bytes)
        })
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
struct FileState {
    bytes: Vec<u8>,
    length: u64,
    modified: std::time::SystemTime,
}

impl FileState {
    fn read(path: &Path) -> Self {
        let metadata = fs::metadata(path).expect("read fixture metadata");
        Self {
            bytes: fs::read(path).expect("read fixture bytes"),
            length: metadata.len(),
            modified: metadata.modified().expect("read modification instant"),
        }
    }
}

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        for _ in 0..1024 {
            let candidate = std::env::temp_dir().join(format!(
                "spice-journal-readonly-{}-{}",
                std::process::id(),
                NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed),
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => return Self { path: candidate },
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create scratch {}: {error}", candidate.display()),
            }
        }
        panic!("could not create a distinct read-only journal scratch directory");
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).expect("remove read-only journal scratch directory");
    }
}

fn directory_entries(path: &Path) -> Vec<String> {
    let mut entries = fs::read_dir(path)
        .expect("read scratch directory")
        .map(|entry| {
            entry
                .expect("read scratch entry")
                .file_name()
                .into_string()
                .expect("scratch entry is UTF-8")
        })
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

fn uuid(text: &str) -> CanonicalUuid {
    text.parse().expect("canonical fixture uuid")
}

fn digest(nibble: &str) -> Digest {
    nibble.repeat(64).parse().expect("canonical digest")
}

fn instant() -> Instant {
    "2030-04-12T18:44:09.271Z".parse().expect("fixture instant")
}

/// A current fact of arbitrary size that no projection folds.
fn text_fact(text: String) -> wire::body::JournalBacked {
    wire::body::JournalBacked {
        bundle: text,
        manifest: Vec::new(),
    }
}

/// [`text_fact`] as the raw body of a planted [`FactKind::JournalBacked`] row.
fn text_body(text: String) -> serde_json::Value {
    serde_json::to_value(text_fact(text)).expect("fixture body encodes")
}

/// One millisecond before [`instant`], the finest step the spelling admits.
fn earlier() -> Instant {
    "2030-04-12T18:44:09.270Z".parse().expect("earlier instant")
}

fn phase(text: &str) -> Phase {
    text.parse().expect("fixture phase")
}
