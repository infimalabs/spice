//! Durable fact-envelope size admission and enforcement.

use super::*;
use crate::{FactCause, JournalWrite};

fn note_envelope_length(note_bytes: usize, position: u64) -> usize {
    let fact = wire::Fact::new(text_fact("x".repeat(note_bytes)));
    let fact_bytes = wire::encode(&fact).expect("fixture fact encodes");
    let fact = wire::decode::<AnyFact>(&fact_bytes).expect("fixture fact erases");
    wire::encode(&Envelope {
        v: WireVersion::CURRENT,
        seq: seq(position),
        at: instant(),
        actor: Actor::TEST_PRINCIPAL,
        origin: "task:1kKRRsDPx".parse().expect("fixture origin"),
        cause: Some(seq(position - 1)),
        fact,
    })
    .expect("fixture envelope encodes")
    .len()
}

fn note_bytes_for_envelope(target: usize, position: u64) -> usize {
    target
        .checked_sub(note_envelope_length(0, position))
        .expect("target exceeds fixed wire")
}

fn note_write(note_bytes: usize) -> JournalWrite {
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:1kKRRsDPx".parse().expect("fixture origin"),
        instant(),
        wire::Producer::ResidentRecognition,
    );
    write
        .fact(
            text_fact("x".repeat(note_bytes)),
            FactCause::Existing(seq(2)),
        )
        .expect("fixture note builds");
    write
}

#[test]
fn writes_admit_the_exact_limit_and_refuse_a_larger_batch_before_any_effect() {
    let mut exact = authority_fixture();
    let exact_note = note_bytes_for_envelope(MAX_DURABLE_FACT_ENVELOPE_BYTES, 3);
    let applied = exact
        .apply(note_write(exact_note))
        .expect("exact-limit fact commits");
    assert_eq!(applied.last(), seq(3));
    let stored: i64 = exact
        .connection
        .query_row(
            "SELECT length(envelope) FROM fact_envelopes WHERE seq = ?1",
            [sql_seq(3)],
            |row| row.get(0),
        )
        .expect("exact-limit fact length reads");
    assert_eq!(stored, MAX_DURABLE_FACT_ENVELOPE_BYTES as i64);

    let mut refused = authority_fixture();
    let before = authority_shape(&refused.connection);
    let mut batch = note_write(1);
    batch
        .fact(
            text_fact("x".repeat(note_bytes_for_envelope(
                MAX_DURABLE_FACT_ENVELOPE_BYTES + 1,
                4,
            ))),
            FactCause::Existing(seq(2)),
        )
        .expect("oversized member remains valid wire");
    refused
        .apply(batch)
        .expect_err("limit-plus-one batch is refused");
    assert_eq!(authority_shape(&refused.connection), before);
}

fn authority_shape(connection: &Connection) -> (i64, Option<i64>, i64, i64, i64, i64) {
    let facts = connection
        .query_row("SELECT count(*) FROM fact_envelopes", [], |row| row.get(0))
        .expect("fact count reads");
    let cursor = connection
        .query_row(
            "SELECT seq FROM cursor_head WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .expect("cursor reads");
    let heads = connection
        .query_row(
            "SELECT (SELECT count(*) FROM task_heads) + (SELECT count(*) FROM workflow_heads)",
            [],
            |row| row.get(0),
        )
        .expect("head count reads");
    let receipts = connection
        .query_row("SELECT count(*) FROM command_receipts", [], |row| {
            row.get(0)
        })
        .expect("receipt count reads");
    let threads = connection
        .query_row("SELECT count(*) FROM thread_index", [], |row| row.get(0))
        .expect("thread count reads");
    let steering = connection
        .query_row(
            "SELECT indexed_seq FROM steering_index WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .expect("steering cursor reads");
    (facts, cursor, heads, receipts, threads, steering)
}

#[test]
fn finalized_triggers_defend_insert_and_update_and_verification_detects_tampering() {
    let current = authority_fixture();
    let oversized = MAX_DURABLE_FACT_ENVELOPE_BYTES + 1;
    let (first, third) = (sql_seq(1), sql_seq(3));
    for sql in [
        format!("UPDATE fact_envelopes SET envelope = zeroblob({oversized}) WHERE seq = {first}"),
        format!(
            "INSERT INTO fact_envelopes(seq, wire_version, instant, actor, origin, cause, kind, envelope)
             SELECT {third}, wire_version, instant, actor, origin, NULL, kind, zeroblob({oversized})
             FROM fact_envelopes WHERE seq = {first}"
        ),
    ] {
        let error = current
            .connection
            .execute(&sql, [])
            .expect_err("direct SQL cannot cross the durable size maximum");
        assert!(
            error
                .to_string()
                .contains(&format!("fact envelope exceeds {MAX_DURABLE_FACT_ENVELOPE_BYTES} bytes"))
        );
    }

    current
        .connection
        .execute("DROP TRIGGER fact_envelopes_envelope_size_update", [])
        .expect("remove one enforcement object");
    current
        .connection
        .execute(
            "UPDATE fact_envelopes SET envelope = zeroblob(?1) WHERE seq = ?2",
            [MAX_DURABLE_FACT_ENVELOPE_BYTES as i64 + 1, first],
        )
        .expect("plant oversized current-schema corruption beneath removed update trigger");
    let violations = refused_violations(&current);
    assert!(violations.iter().any(|violation| {
        violation.class == ViolationClass::Schema
            && violation.subject == crate::migrate::FACT_ENVELOPE_UPDATE_TRIGGER
            && violation.detail.contains("missing")
    }));
    assert!(violations.iter().any(|violation| {
        violation.class == ViolationClass::Envelope
            && violation.subject == first.to_string()
            && violation
                .detail
                .contains(&(MAX_DURABLE_FACT_ENVELOPE_BYTES + 1).to_string())
    }));
}
