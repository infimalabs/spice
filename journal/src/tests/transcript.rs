//! The transcript ingestion fact and its keyed source head.
//!
//! One accounted batch and the checkpoint it advances are written, verified,
//! and read back together, because they are only correct together: a batch
//! whose checkpoint did not move would be re-accounted, and a checkpoint that
//! moved without its batch would skip spend.

use super::*;

/// An accounted batch survives the write path it is actually written through.
///
/// The fixtures elsewhere in this file plant rows with SQL, which proves replay
/// reads what is stored but never that the guarded writer can store it. This
/// goes the other way: the batch and the checkpoint it advances cross together
/// through `apply`, replay accepts the result, and the sample reads back with
/// its spans, its ordinal, and its absent reasoning counter intact.
#[test]
fn an_accounted_batch_and_its_checkpoint_round_trip_through_the_write_path() {
    let journal = &mut Journal::in_memory().expect("create write-path fixture");
    let head = transcript_source_head();
    let batch = ingested_batch();

    let mut write = crate::JournalWrite::new(
        Actor::Spice,
        "lane:018f82d4-6f59-7d06-92e3-bce913116305"
            .parse::<Origin>()
            .expect("resident origin"),
        instant(),
        wire::Producer::ResidentRecognition,
    );
    let pending = write
        .fact(batch.clone(), crate::FactCause::Root)
        .expect("an ingestion batch crosses the writer");
    write.transcript_source_head(pending, {
        let head = head.clone();
        move |_seq| head
    });
    journal.apply(write).expect("the batch applies");

    let verified = journal.verify().expect("the written authority verifies");
    assert_eq!(verified.facts, 1);
    assert_eq!(verified.heads, 1);

    let stored = journal
        .read(|read| read.transcript_source_head(&head.key()))
        .expect("read the written checkpoint")
        .expect("the write path stored the checkpoint");
    assert_eq!(stored.body(), &head);
    let indexed_lane: String = journal
        .connection
        .query_row(
            "SELECT lane FROM transcript_source_heads WHERE stream = ?1",
            [head.key().as_str()],
            |row| row.get(0),
        )
        .expect("written lane predicate reads");
    assert_eq!(indexed_lane, head.lane.to_string());

    let snapshot = journal.fact_snapshot().expect("fact snapshot");
    let decoded: wire::body::JournalIngested = snapshot.facts()[0]
        .fact
        .read()
        .expect("the stored fact reads back as its registered body");
    assert_eq!(decoded, batch);
    assert_eq!(decoded.usage[0].reasoning_output_tokens, None);
    assert_eq!(decoded.usage[0].spans.len(), 2);
}

/// A transcript head is stored under the stream its own body names.
///
/// The row key is recomposed from lane, vendor and stream rather than stored
/// beside them, so a body that disagrees with its key is not a second spelling
/// of the same stream: it is either a head written under the wrong key or a
/// stream silently renamed under a key some reader still resolves. Either way
/// the next batch would advance a checkpoint belonging to another source.
#[test]
fn a_transcript_head_whose_stream_disagrees_with_its_row_is_refused() {
    let journal = authority_fixture();
    journal
        .connection
        .execute(
            "UPDATE transcript_source_heads SET lane = \
             '018f82d4-6f59-7d06-92e3-bce913116399'",
            [],
        )
        .expect("corrupt transcript lane predicate");
    assert!(
        refused_violations(&journal)
            .iter()
            .any(|violation| { violation.class == ViolationClass::Head })
    );

    let mut renamed = transcript_source_head();
    renamed.stream = "sidecar".to_owned();
    journal
        .connection
        .execute(
            "UPDATE transcript_source_heads SET lane = ?1, body = ?2",
            params![
                renamed.lane.to_string(),
                wire::encode(&renamed).expect("encode renamed transcript head")
            ],
        )
        .expect("corrupt transcript head fixture");

    assert!(
        refused_violations(&journal)
            .iter()
            .any(|violation| violation.class == ViolationClass::Head)
    );
}

/// The transcript head survives the bundle path an operator actually restores.
///
/// Replay over a live handle and replay over a captured generation are separate
/// readers of the same plane, and only this one crosses a file. A head table the
/// backup path did not know about would verify in memory and then be absent, or
/// silently uncounted, in the bundle every restore is judged against.
#[test]
fn a_captured_bundle_verifies_the_transcript_head_it_carries() {
    use wire::manifest::{JOURNAL_BACKUP_VERSION, JournalBackupManifest, JournalBackupMember};

    let scratch = Scratch::new();
    let authority = scratch.path().join("spicejournal.sqlite3");
    drop(authority_file_fixture(&authority));

    let bytes = fs::read(&authority).expect("captured authority reads");
    let manifest = JournalBackupManifest {
        version: JOURNAL_BACKUP_VERSION,
        wire: WireVersion::CURRENT,
        journal_schema: SCHEMA_VERSION,
        authority: "spicejournal.sqlite3".to_owned(),
        members: vec![JournalBackupMember {
            path: "spicejournal.sqlite3".to_owned(),
            mode: sill::authority::file_mode(
                &fs::File::open(&authority).expect("captured authority opens"),
            )
            .expect("captured authority mode reads"),
            bytes: u64::try_from(bytes.len()).expect("authority length crosses"),
            digest: Digest::compute(wire::Domain::JournalBackupMember, &bytes),
        }],
    };
    fs::write(
        scratch.path().join("manifest.json"),
        wire::encode(&manifest).expect("manifest encodes"),
    )
    .expect("manifest writes");

    let verified = crate::verify_restore_bundle(scratch.path()).expect("populated bundle verifies");
    assert_eq!(verified.journal_schema, SCHEMA_VERSION);
    assert_eq!(verified.heads, 9);

    let restored = Journal::open(&authority).expect("restored authority opens");
    let stored = restored
        .read(|read| read.transcript_source_head(&transcript_source_head().key()))
        .expect("read restored transcript head")
        .expect("restored bundle carries the transcript head");
    assert_eq!(stored.body(), &transcript_source_head());
}

/// A caller holding the three logical fields reaches the head it wrote.
///
/// The round-trip tests above address the row with `head.key()`, which asks the
/// stored body to address itself and so cannot tell whether the reader agrees
/// with the writer about what a key is. This addresses it the way a caller
/// actually would -- from lane, vendor and stream, with no key composed by hand
/// -- and pins the miss too: the same head read under a key built from a
/// different lane is `Ok(None)`, so the reader discriminates on all three
/// fields.
#[test]
fn a_transcript_head_is_addressed_by_lane_vendor_and_stream_together() {
    let mut journal = authority_fixture();
    let head = transcript_source_head();
    let before = measured_lane_heads(&journal, &head.lane);

    let addressed = wire::TranscriptStreamKey::new(&head.lane, &head.vendor, &head.stream);
    let stored = journal
        .read(|read| read.transcript_source_head(&addressed))
        .expect("read the fixture checkpoint")
        .expect("the three logical fields address the stored head");
    assert_eq!(stored.body(), &head);

    let other_lane = "018f82d4-6f59-7d06-92e3-bce913116306"
        .parse::<CanonicalUuid>()
        .expect("a second lane identity");
    let elsewhere = wire::TranscriptStreamKey::new(&other_lane, &head.vendor, &head.stream);
    assert!(
        journal
            .read(|read| read.transcript_source_head(&elsewhere))
            .expect("read another lane's checkpoint")
            .is_none(),
        "the same stream name under another lane resolved to this lane's head",
    );

    let transaction = journal
        .connection
        .transaction()
        .expect("unrelated transcript growth transaction begins");
    for offset in 0..64 {
        let mut unrelated = head.clone();
        unrelated.lane = other_lane;
        unrelated.stream = format!("retained-{offset:03}");
        transaction
            .execute(
                "INSERT INTO transcript_source_heads(stream, lane, head_seq, body) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    unrelated.key().as_str(),
                    unrelated.lane.to_string(),
                    sql_seq(2),
                    wire::encode(&unrelated).expect("encode unrelated transcript head")
                ],
            )
            .expect("insert unrelated transcript head");
    }
    transaction
        .commit()
        .expect("unrelated transcript heads commit");

    assert_eq!(measured_lane_heads(&journal, &head.lane), before);
    assert_eq!(before.queries, 1);
    assert_eq!(before.rows, 1);
    assert!(before.decoded_bytes > 0);
    let plan = journal
        .connection
        .prepare(
            "EXPLAIN QUERY PLAN SELECT stream, lane, head_seq, body \
             FROM transcript_source_heads WHERE lane = ?1 ORDER BY stream",
        )
        .expect("lane query plan prepares")
        .query_map([head.lane.to_string()], |row| row.get::<_, String>(3))
        .expect("lane query plan rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("lane query plan reads")
        .join("\n");
    assert!(
        plan.contains("transcript_source_heads_lane_stream"),
        "lane transcript lookup must use its covering order: {plan}"
    );
}

fn measured_lane_heads(
    journal: &Journal,
    lane: &CanonicalUuid,
) -> crate::guard::IndexedReadMetrics {
    crate::guard::reset_indexed_read_metrics();
    let heads = journal
        .read(|read| read.transcript_source_heads(lane))
        .expect("lane transcript heads read");
    assert_eq!(heads.len(), 1);
    crate::guard::indexed_read_metrics()
}

/// One batch that advanced the fixture checkpoint to where it now stands.
fn ingested_batch() -> wire::body::JournalIngested {
    let head = transcript_source_head();
    wire::body::JournalIngested {
        lane: head.lane,
        vendor: head.vendor.clone(),
        stream: head.stream.clone(),
        source: head.source,
        supersedes: None,
        from: TranscriptPosition {
            member: 1,
            offset: 0,
        },
        to: head.next,
        usage: vec![wire::body::TranscriptUsage {
            turns: 1,
            input_tokens: 22005,
            cached_input_tokens: 4213,
            cache_write_input_tokens: 0,
            output_tokens: 311,
            reasoning_output_tokens: None,
            spans: vec![
                wire::body::TranscriptSpan {
                    member: 1,
                    offset: 0,
                    length: 2048,
                },
                wire::body::TranscriptSpan {
                    member: 1,
                    offset: 2048,
                    length: 2048,
                },
            ],
            ordinal: 0,
        }],
        non_usage_records: 3,
        residue_records: 1,
    }
}
