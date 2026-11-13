use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use listen::{
    Checkpoint, Dialect, EventKind, Position, Record, RecordPayload, Scan, ScanError, Span,
    TailBudget, scan, scan_before, scan_tail, search_tail,
};
use std::ops::ControlFlow;
use tape::{Locus, Tape};

const PROSE: &[u8] = br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"working"}],"stopReason":"stop"}}"#;
const MULTI_EVENT: &[u8] = br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"working"},{"type":"toolCall","id":"tool-1","name":"bash","arguments":{"command":"cargo test"}}],"stopReason":"toolUse"}}"#;
const TEST_BOUNDARY_ALLOWANCE: u64 = 256 * 1024;
const TEST_SOURCE_LIMIT: u64 = 128 * 1024;
const TEST_RECORD_LIMIT: usize = 32 * 1024;
const TEST_PASS_LIMIT: usize = 64;
const TEST_DURATION: Duration = Duration::from_secs(1);
const SMALL_WINDOW_BYTES: u64 = 4 * 1024;
const SMALL_WINDOW_RECORDS: usize = 4 * 1024;
const OVERSIZED_RECORD_BYTES: usize = 512 * 1024;
/// Index bytes and one boundary chunk of overshoot a retreat may add.
const RETREAT_SLACK_BYTES: u64 = 64 * 1024;
const LARGE_SEASON_RECORDS: usize = 40_000;
const EQUAL_MEMBER_ADDITIONS: usize = 15;
const ACTIVE_MEMBER_RECORDS: usize = 5;
const RECORD_FIXTURE_COUNT: usize = 3;
const PENDING_WINDOW_BYTES: u64 = 4;
const MULTI_MEMBER_WINDOW_RECORDS: usize = 7;
/// Member bytes one forward scan reads at once.
const SCAN_CHUNK_BYTES: usize = 1024 * 1024;

#[test]
fn a_sealed_tail_cannot_hide_frame_decompression_behind_a_small_window() {
    let scratch = Scratch::new("sealed-tail-work");
    let tape = Tape::open(scratch.path()).unwrap();
    tape.tee(&line(PROSE).repeat(4096)).unwrap();
    tape.seal().unwrap();
    let error = scan_tail(
        scratch.path(),
        Dialect::Omp,
        TailBudget::new(
            SMALL_WINDOW_BYTES,
            TEST_SOURCE_LIMIT,
            TEST_RECORD_LIMIT,
            TEST_PASS_LIMIT,
            TEST_DURATION,
        )
        .unwrap(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ScanError::WorkLimit {
            dimension: "source bytes",
            ..
        }
    ));
    scratch.remove();
}

#[test]
fn a_sealed_tail_pays_for_the_frames_it_decodes_rather_than_its_member() {
    const FRAME: u64 = 256 * 1024;
    let scratch = Scratch::new("sealed-tail-frames");
    let tape = Tape::open(scratch.path()).unwrap();
    let records = line(PROSE).repeat(32 * 1024);
    let member = u64::try_from(records.len()).expect("member length fits");
    tape.tee(&records).unwrap();
    tape.seal().unwrap();
    // A retreat touches at most two frames, and so does the window it found.
    let allowance = 4 * FRAME + RETREAT_SLACK_BYTES;
    assert!(
        member > 2 * allowance,
        "the member alone exceeds the budget"
    );

    let tail = scan_tail(
        scratch.path(),
        Dialect::Omp,
        TailBudget::new(
            SMALL_WINDOW_BYTES,
            allowance,
            SMALL_WINDOW_RECORDS,
            TEST_PASS_LIMIT,
            TEST_DURATION,
        )
        .unwrap(),
    )
    .expect("a small window over an archived member is affordable");
    let span = u64::try_from(line(PROSE).len()).expect("record length fits");
    assert_eq!(
        tail.scan.records.len() as u64,
        SMALL_WINDOW_BYTES / span + 1,
        "the window holds the newest records whole"
    );
    assert_eq!(tail.start.next.member, 1);
    assert_eq!(
        tail.start.next.offset % span,
        0,
        "the window starts on a record"
    );
    assert!(
        tail.work.source_bytes <= allowance,
        "{}",
        tail.work.source_bytes
    );
    scratch.remove();
}
fn tail_budget(window_bytes: u64) -> TailBudget {
    TailBudget::new(
        window_bytes,
        window_bytes.saturating_add(TEST_BOUNDARY_ALLOWANCE),
        TEST_RECORD_LIMIT,
        TEST_PASS_LIMIT,
        TEST_DURATION,
    )
    .expect("positive test tail budget")
}

#[test]
fn tail_allowances_refuse_every_zero_dimension() {
    let positive = (1, 1, 1, Duration::from_nanos(1));
    for (window, bytes, records, passes, duration, field) in [
        (
            0,
            positive.0,
            positive.1,
            positive.2,
            positive.3,
            "window bytes",
        ),
        (
            positive.0,
            0,
            positive.1,
            positive.2,
            positive.3,
            "source bytes",
        ),
        (
            positive.0,
            positive.0,
            0,
            positive.2,
            positive.3,
            "decoded records",
        ),
        (
            positive.0,
            positive.0,
            positive.1,
            0,
            positive.3,
            "scan passes",
        ),
        (
            positive.0,
            positive.0,
            positive.1,
            positive.2,
            Duration::ZERO,
            "wall duration",
        ),
    ] {
        let error = TailBudget::new(window, bytes, records, passes, duration)
            .expect_err("zero allowance must refuse");
        assert!(error.to_string().contains(field));
    }
}

#[test]
fn oversized_and_delimiter_free_tails_stop_at_the_same_byte_ceiling() {
    fn refused(label: &str, suffix: &[u8]) -> (u64, u64) {
        let scratch = Scratch::new(label);
        let tape = Tape::open(scratch.path()).expect("tape opens");
        let mut bytes = vec![b'x'; OVERSIZED_RECORD_BYTES];
        bytes.extend_from_slice(suffix);
        tape.tee(&bytes).expect("large record is durable");
        let error = scan_tail(
            scratch.path(),
            Dialect::Omp,
            TailBudget::new(
                SMALL_WINDOW_BYTES,
                TEST_SOURCE_LIMIT,
                SMALL_WINDOW_RECORDS,
                TEST_PASS_LIMIT,
                TEST_DURATION,
            )
            .expect("bounded tail"),
        )
        .expect_err("boundary beyond the allowance must refuse");
        scratch.remove();
        let ScanError::WorkLimit {
            dimension,
            limit,
            consumed,
            work,
        } = error
        else {
            panic!("oversized tail returned a non-budget error: {error}")
        };
        assert!(matches!(dimension, "source bytes" | "scan passes"));
        assert!(consumed <= limit);
        assert_eq!(work.records, 0);
        assert!(work.source_bytes <= TEST_SOURCE_LIMIT);
        assert!(work.passes <= TEST_PASS_LIMIT);
        (limit, consumed)
    }

    assert_eq!(
        refused("oversized-tail", b"\n"),
        refused("delimiter-free-tail", b"")
    );
}

#[test]
fn a_window_that_begins_inside_a_record_pays_for_its_retreat_once() {
    let scratch = Scratch::new("retreat-once");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let prose = line(PROSE);
    tape.tee(&prose).expect("older record is durable");
    let mut oversized = vec![b'x'; OVERSIZED_RECORD_BYTES];
    oversized.push(b'\n');
    tape.tee(&oversized).expect("oversized record is durable");
    tape.tee(&prose).expect("newest record is durable");

    // The window asks for the newest few kibibytes and begins inside the
    // oversized record; retreating to its start costs that record once, not
    // once to find the boundary and again to read from it.
    let retreat = u64::try_from(OVERSIZED_RECORD_BYTES).expect("retreat fits");
    let allowance = SMALL_WINDOW_BYTES + retreat + RETREAT_SLACK_BYTES;
    let budget = TailBudget::new(
        SMALL_WINDOW_BYTES,
        allowance,
        SMALL_WINDOW_RECORDS,
        TEST_PASS_LIMIT,
        TEST_DURATION,
    )
    .expect("bounded tail");
    let tail = scan_tail(scratch.path(), Dialect::Omp, budget)
        .expect("one retreat over the record is affordable");
    assert_eq!(
        tail.start.next,
        Position {
            member: 1,
            offset: u64::try_from(prose.len()).expect("offset fits"),
        },
        "the window begins where the straddled record does"
    );
    assert_eq!(
        tail.scan.records.len(),
        2,
        "the straddled record and the newest"
    );
    assert!(tail.work.source_bytes > retreat, "the retreat is paid");
    assert!(
        tail.work.source_bytes <= allowance,
        "{}",
        tail.work.source_bytes
    );
    scratch.remove();
}

#[test]
fn a_window_inside_the_tapes_first_record_retreats_to_byte_zero() {
    let scratch = Scratch::new("retreat-to-start");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let mut oversized = vec![b'x'; OVERSIZED_RECORD_BYTES];
    oversized.push(b'\n');
    tape.tee(&oversized).expect("first record is durable");
    tape.tee(&line(PROSE)).expect("newest record is durable");

    // No newline precedes the record the window begins inside, because it is
    // the first one on the tape. Its own first byte is the boundary; a
    // retreat that reaches byte zero has found it, not run out of allowance.
    let retreat = u64::try_from(OVERSIZED_RECORD_BYTES).expect("retreat fits");
    let budget = TailBudget::new(
        SMALL_WINDOW_BYTES,
        SMALL_WINDOW_BYTES + retreat + RETREAT_SLACK_BYTES,
        SMALL_WINDOW_RECORDS,
        TEST_PASS_LIMIT,
        TEST_DURATION,
    )
    .expect("bounded tail");
    let tail = scan_tail(scratch.path(), Dialect::Omp, budget)
        .expect("the tape's first record is crossable");
    assert_eq!(tail.start.next, Position::start());
    assert_eq!(
        tail.scan.records.len(),
        2,
        "the straddled first record and the newest"
    );
    scratch.remove();
}

#[test]
fn tail_record_allowance_refuses_before_decoding_an_extra_record() {
    let scratch = Scratch::new("tail-record-ceiling");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let prose = line(PROSE);
    tape.tee(&prose.repeat(RECORD_FIXTURE_COUNT))
        .expect("three records are durable");
    let error = scan_tail(
        scratch.path(),
        Dialect::Omp,
        TailBudget::new(
            u64::try_from(prose.len() * RECORD_FIXTURE_COUNT).expect("window fits"),
            TEST_BOUNDARY_ALLOWANCE,
            1,
            TEST_PASS_LIMIT,
            TEST_DURATION,
        )
        .expect("record-bounded tail"),
    )
    .expect_err("a second record must not be decoded");
    let ScanError::WorkLimit {
        dimension,
        limit,
        consumed,
        work,
    } = error
    else {
        panic!("record ceiling returned a non-budget error: {error}")
    };
    assert_eq!((dimension, limit, consumed), ("decoded records", 1, 1));
    assert_eq!(work.records, 1);
    assert!(work.source_bytes > 0);
    assert!(work.passes > 0);
    scratch.remove();
}

#[test]
fn wall_exhaustion_carries_every_tail_work_counter() {
    let scratch = Scratch::new("tail-wall-counters");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    tape.tee(&line(PROSE)).expect("one record is durable");

    let error = scan_tail(
        scratch.path(),
        Dialect::Omp,
        TailBudget::new(
            SMALL_WINDOW_BYTES,
            TEST_SOURCE_LIMIT,
            SMALL_WINDOW_RECORDS,
            TEST_PASS_LIMIT,
            Duration::from_nanos(1),
        )
        .expect("deadline-bounded tail"),
    )
    .expect_err("one-nanosecond tail must exhaust its wall allowance");

    let work = *error
        .tail_work()
        .expect("wall refusal carries complete work");
    assert!(work.source_bytes <= TEST_SOURCE_LIMIT);
    assert!(work.records <= SMALL_WINDOW_RECORDS);
    assert!(work.passes <= TEST_PASS_LIMIT);
    assert!(!work.duration.is_zero());
    let diagnostic = error.to_string();
    for field in ["source bytes", "records", "passes"] {
        assert!(diagnostic.contains(field), "{diagnostic}");
    }
    scratch.remove();
}

#[test]
fn tail_work_is_constant_as_older_sparse_history_grows() {
    let scratch = Scratch::new("tail-work-ceiling");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let residue = b"{}\n";
    let season = residue.repeat(LARGE_SEASON_RECORDS);
    tape.tee(&season).expect("older residue is durable");
    let budget = TailBudget::new(
        SMALL_WINDOW_BYTES,
        TEST_SOURCE_LIMIT,
        SMALL_WINDOW_RECORDS,
        TEST_PASS_LIMIT,
        TEST_DURATION,
    )
    .expect("bounded tail");
    let first = scan_tail(scratch.path(), Dialect::Omp, budget).expect("first bounded tail");
    tape.tee(&season)
        .expect("more older-shaped residue is durable");
    let second = scan_tail(scratch.path(), Dialect::Omp, budget).expect("second bounded tail");

    assert_eq!(first.work.source_bytes, second.work.source_bytes);
    assert_eq!(first.work.records, second.work.records);
    assert_eq!(first.work.passes, second.work.passes);
    assert_eq!(first.work.records, first.scan.records.len());
    assert!(first.work.source_bytes <= TEST_SOURCE_LIMIT);
    assert!(first.work.passes <= TEST_PASS_LIMIT);
    scratch.remove();
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "spice-listen-stream-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("scratch root is exclusively created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn remove(self) {
        fs::remove_dir_all(&self.0).expect("scratch root is explicitly removed");
        std::mem::forget(self);
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn line(bytes: &[u8]) -> Vec<u8> {
    let mut line = bytes.to_vec();
    line.push(b'\n');
    line
}

#[test]
fn complete_events_and_residue_advance_while_a_partial_record_does_not() {
    let scratch = Scratch::new("checkpoint");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let prose = line(PROSE);
    let residue = b"not-json\n";
    let partial = br#"{"type":"agent_end"#;
    tape.tee(&prose).expect("typed record is durable");
    tape.tee(residue).expect("residue record is durable");
    tape.tee(partial).expect("partial record is durable");

    let first = scan(scratch.path(), Dialect::Omp, None).expect("initial scan succeeds");

    assert_eq!(first.records.len(), 2, "both complete records advance");
    assert_eq!(
        first.checkpoint.next,
        Position {
            member: 1,
            offset: (prose.len() + residue.len()) as u64,
        },
        "pending bytes are excluded from the next-record checkpoint"
    );
    assert_eq!(first.pending.len(), 1);
    let checkpoint_json = serde_json::to_string(&first.checkpoint).expect("checkpoint serializes");
    assert_eq!(
        serde_json::from_str::<Checkpoint>(&checkpoint_json).expect("checkpoint reopens"),
        first.checkpoint
    );
    let mut malformed: serde_json::Value =
        serde_json::from_str(&checkpoint_json).expect("checkpoint is JSON");
    malformed["source"] = "NOT-A-SOURCE".into();
    assert!(
        serde_json::from_value::<Checkpoint>(malformed).is_err(),
        "checkpoint deserialization does not admit another source spelling"
    );
    match &first.records[0].payload {
        RecordPayload::Events(events) => {
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].ordinal, 0);
            assert_eq!(events[0].event.kind(), EventKind::Prose);
            assert_eq!(
                events[0].spans,
                vec![Span {
                    member: 1,
                    offset: 0,
                    length: prose.len() as u64,
                }]
            );
        }
        RecordPayload::Residue(_) => panic!("typed prose became residue"),
    }
    assert!(matches!(
        first.records[1].payload,
        RecordPayload::Residue(_)
    ));

    let suffix = b",\"messages\":[]}\n";
    tape.tee(suffix).expect("partial record completes");
    let resumed = scan(scratch.path(), Dialect::Omp, Some(&first.checkpoint))
        .expect("scan resumes at the prior complete boundary");
    assert_eq!(resumed.records.len(), 1);
    assert!(resumed.pending.is_empty());
    assert_eq!(resumed.records[0].next, resumed.checkpoint);
    scratch.remove();
}

#[test]
fn scan_preserves_context_and_locus_for_a_zero_event_turn_start() {
    let scratch = Scratch::new("turn-start-context");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let started = line(br#"{"timestamp":"2026-08-09T01:00:00Z","type":"agent_start"}"#);
    tape.tee(&started).expect("turn start is durable");

    let scanned = scan(scratch.path(), Dialect::Omp, None).expect("turn start scans");

    let [record] = scanned.records.as_slice() else {
        panic!("turn start remains one record")
    };
    assert_eq!(record.locus, Locus::new(1, 0, started.len() as u64));
    let context = record.context.as_ref().expect("typed context survives");
    assert_eq!(context.timestamp.as_deref(), Some("2026-08-09T01:00:00Z"));
    assert_eq!(context.turn, Some(listen::TurnMoment::Started));
    assert!(matches!(&record.payload, RecordPayload::Events(events) if events.is_empty()));
    scratch.remove();
}

#[test]
fn tail_scan_starts_near_eof_and_its_source_checkpoint_reads_only_appends() {
    let scratch = Scratch::new("tail-window");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let prose = line(PROSE);
    tape.tee(&prose.repeat(400))
        .expect("season records are durable");

    let tail = scan_tail(
        scratch.path(),
        Dialect::Omp,
        tail_budget(u64::try_from(prose.len() * RECORD_FIXTURE_COUNT).expect("window fits")),
    )
    .expect("tail scan succeeds");
    assert!(
        tail.start.next > Position::start(),
        "a season-long tape is not scanned from byte zero"
    );
    assert_eq!(tail.scan.records.len(), 3);

    tape.tee(&prose).expect("one append is durable");
    let appended = scan(scratch.path(), Dialect::Omp, Some(&tail.scan.checkpoint))
        .expect("source checkpoint resumes");
    assert_eq!(appended.records.len(), 1, "only the append is decoded");
    scratch.remove();
}

#[test]
fn bounded_history_windows_end_at_the_supplied_checkpoint_without_overlap() {
    let scratch = Scratch::new("history-window");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let prose = line(PROSE);
    for _ in 0..12 {
        tape.tee(&prose).expect("history record is durable");
    }
    let complete = scan(scratch.path(), Dialect::Omp, None).expect("season scans");
    let before = complete.records[9].next.clone();
    let bytes = u64::try_from(prose.len() * 3).expect("window fits");
    tape.tee(&prose)
        .expect("an append after the history boundary is durable");

    let newer = scan_before(scratch.path(), Dialect::Omp, &before, tail_budget(bytes))
        .expect("newer history window scans");
    assert_eq!(newer.scan.records.len(), 3);
    assert_eq!(newer.scan.checkpoint, before);
    let older = scan_before(
        scratch.path(),
        Dialect::Omp,
        &newer.start,
        tail_budget(bytes),
    )
    .expect("adjacent older history window scans");
    assert_eq!(older.scan.records.len(), 3);
    assert_eq!(older.scan.checkpoint, newer.start);
    assert!(
        older.scan.records.last().expect("older record").next.next <= newer.start.next,
        "the older window never reads into the already-served newer window",
    );
    scratch.remove();
}

#[test]
fn tail_checkpoint_stays_before_an_unterminated_record_until_it_completes() {
    let scratch = Scratch::new("tail-pending");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    tape.tee(&line(PROSE)).expect("older prose is durable");
    let partial = br#"{"type":"agent_end"#;
    tape.tee(partial).expect("partial record is durable");

    let tail = scan_tail(
        scratch.path(),
        Dialect::Omp,
        tail_budget(PENDING_WINDOW_BYTES),
    )
    .expect("tail scan succeeds");
    assert!(tail.scan.records.is_empty());
    assert!(!tail.scan.pending.is_empty());
    assert_eq!(
        tail.scan.checkpoint, tail.start,
        "the resume cursor stays before incomplete bytes"
    );

    tape.tee(b",\"error\":{\"message\":\"stopped\"}}\n")
        .expect("partial record completes");
    let completed = scan(scratch.path(), Dialect::Omp, Some(&tail.scan.checkpoint))
        .expect("completion resumes from the safe boundary");
    assert_eq!(completed.records.len(), 1);
    scratch.remove();
}

#[test]
fn tail_window_crosses_sealed_members_without_reopening_the_season_start() {
    let scratch = Scratch::new("tail-sealed");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let prose = line(PROSE);
    for _ in 0..20 {
        tape.tee(&prose).expect("older member grows");
    }
    tape.seal().expect("older member seals");
    for _ in 0..5 {
        tape.tee(&prose).expect("active member grows");
    }

    let tail = scan_tail(
        scratch.path(),
        Dialect::Omp,
        tail_budget(u64::try_from(prose.len() * MULTI_MEMBER_WINDOW_RECORDS).expect("window fits")),
    )
    .expect("cross-member tail scans");
    assert_eq!(tail.scan.records.len(), 7);
    assert!(
        tail.start.next
            > Position {
                member: 1,
                offset: 0,
            },
        "the tail begins inside the season rather than at byte zero",
    );
    let members = tail
        .scan
        .records
        .iter()
        .filter_map(|record| match &record.payload {
            RecordPayload::Events(events) => events.first(),
            RecordPayload::Residue(_) => None,
        })
        .filter_map(|event| event.spans.first())
        .map(|span| span.member)
        .collect::<Vec<_>>();
    assert!(members.contains(&1) && members.contains(&2));
    let first_work = (tail.work.source_bytes, tail.work.records, tail.work.passes);

    for _ in 0..EQUAL_MEMBER_ADDITIONS {
        tape.tee(&prose).expect("second member reaches equal size");
    }
    tape.seal().expect("second member seals");
    for _ in 0..ACTIVE_MEMBER_RECORDS {
        tape.tee(&prose).expect("third member grows");
    }
    let grown = scan_tail(
        scratch.path(),
        Dialect::Omp,
        tail_budget(u64::try_from(prose.len() * MULTI_MEMBER_WINDOW_RECORDS).expect("window fits")),
    )
    .expect("grown multi-member tail scans");
    assert_eq!(grown.work.records, first_work.1);
    assert!(grown.work.source_bytes > first_work.0);
    assert!(grown.work.passes > first_work.2);
    assert!(grown.work.passes <= TEST_PASS_LIMIT);
    assert!(
        grown.work.source_bytes
            <= u64::try_from(prose.len() * MULTI_MEMBER_WINDOW_RECORDS)
                .expect("window fits")
                .saturating_add(TEST_BOUNDARY_ALLOWANCE)
    );
    scratch.remove();
}

#[test]
fn sealed_member_discovery_stops_at_the_same_pass_ceiling_as_the_index_grows() {
    fn refused(label: &str, members: usize) -> (u64, u64) {
        let scratch = Scratch::new(label);
        Tape::open(scratch.path()).expect("tape opens");
        let mut index = String::from("tape 1\n");
        for member in 1..=members {
            index.push_str(&format!("member {member:012} 1\n"));
        }
        fs::write(scratch.path().join("index"), index).expect("large index is published");
        let error = scan_tail(
            scratch.path(),
            Dialect::Omp,
            TailBudget::new(
                SMALL_WINDOW_BYTES,
                TEST_SOURCE_LIMIT,
                SMALL_WINDOW_RECORDS,
                TEST_PASS_LIMIT,
                TEST_DURATION,
            )
            .expect("bounded tail"),
        )
        .expect_err("extent discovery must stop at its pass ceiling");
        scratch.remove();
        let ScanError::WorkLimit {
            dimension,
            limit,
            consumed,
            work,
        } = error
        else {
            panic!("large index returned a non-budget error: {error}")
        };
        assert_eq!(dimension, "scan passes");
        assert_eq!(work.passes as u64, consumed);
        assert_eq!(work.records, 0);
        assert!(work.source_bytes > 0);
        (limit, consumed)
    }

    assert_eq!(
        refused("extent-limit-small", TEST_PASS_LIMIT + 20),
        refused("extent-limit-large", TEST_PASS_LIMIT + 200),
    );
}

#[test]
fn oversized_index_refuses_before_extent_parsing() {
    const INDEX_BYTE_LIMIT: u64 = 32;
    let scratch = Scratch::new("extent-index-byte-limit");
    Tape::open(scratch.path()).expect("tape opens");
    fs::write(
        scratch.path().join("index"),
        "tape 1\nmember 000000000001 1\nmember 000000000002 1\n",
    )
    .expect("oversized canonical index is published");

    let error = scan_tail(
        scratch.path(),
        Dialect::Omp,
        TailBudget::new(
            1,
            INDEX_BYTE_LIMIT,
            SMALL_WINDOW_RECORDS,
            TEST_PASS_LIMIT,
            TEST_DURATION,
        )
        .expect("byte-bounded tail"),
    )
    .expect_err("oversized index must stop before parsing");

    let ScanError::WorkLimit {
        dimension,
        limit,
        consumed,
        work,
    } = error
    else {
        panic!("index ceiling returned a non-budget error: {error}")
    };
    assert_eq!(
        (dimension, limit, consumed),
        ("source bytes", INDEX_BYTE_LIMIT, 0)
    );
    assert_eq!((work.source_bytes, work.records, work.passes), (0, 0, 1));
    scratch.remove();
}

#[test]
fn one_record_can_cross_a_seal_without_losing_identity_or_exact_spans() {
    let scratch = Scratch::new("cross-member");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let split = PROSE.len() / 2;
    tape.tee(&PROSE[..split]).expect("record prefix is durable");
    let source = tape.source_id().expect("source reads before sealing");
    tape.seal().expect("prefix member seals");
    tape.tee(&line(&PROSE[split..]))
        .expect("record suffix is durable");

    let scanned = scan(scratch.path(), Dialect::Omp, None).expect("cross-member scan succeeds");

    assert_eq!(scanned.checkpoint.source, source);
    let RecordPayload::Events(events) = &scanned.records[0].payload else {
        panic!("cross-member prose became residue");
    };
    assert_eq!(events[0].spans.len(), 2);
    assert_eq!(events[0].spans[0].member, 1);
    assert_eq!(events[0].spans[1].member, 2);
    scratch.remove();
}

#[test]
fn a_forward_scan_joins_a_record_across_read_chunks_and_resumes_inside_a_sealed_member() {
    let scratch = Scratch::new("forward-chunks");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let prose = line(PROSE);
    let lead = SCAN_CHUNK_BYTES - prose.len() / 2;
    tape.tee(&filler(lead))
        .expect("filler ends inside the first read");
    tape.tee(&prose).expect("prose straddles the read boundary");
    tape.tee(&prose).expect("prose follows in the same member");
    tape.seal().expect("member seals");
    tape.tee(&prose).expect("active member grows");

    let full = scan(scratch.path(), Dialect::Omp, None).expect("forward scan succeeds");
    let straddling = Span {
        member: 1,
        offset: lead as u64,
        length: prose.len() as u64,
    };
    let RecordPayload::Events(events) = &full.records[1].payload else {
        panic!("straddling prose became residue");
    };
    assert_eq!(full.records.len(), 4);
    assert_eq!(events[0].spans, [straddling], "one contiguous span");
    assert_eq!(events[0].event.kind(), EventKind::Prose);

    let resumed = scan(scratch.path(), Dialect::Omp, Some(&full.records[0].next))
        .expect("a checkpoint inside the sealed member resumes");
    assert_eq!(resumed.records, full.records[1..]);
    assert_eq!(resumed.checkpoint, full.checkpoint);
    scratch.remove();
}

#[test]
fn same_generation_retreat_and_gap_are_refused() {
    let scratch = Scratch::new("continuity");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let complete = line(PROSE);
    tape.tee(&complete).expect("record is durable");
    let scanned = scan(scratch.path(), Dialect::Omp, None).expect("initial scan succeeds");

    fs::write(scratch.path().join("000000000001.plain"), b"short")
        .expect("fixture retreats the active member");
    assert!(matches!(
        scan(scratch.path(), Dialect::Omp, Some(&scanned.checkpoint)),
        Err(ScanError::CheckpointRetreated { .. })
    ));

    let gap = Checkpoint {
        source: scanned.checkpoint.source.clone(),
        next: Position {
            member: 3,
            offset: 0,
        },
    };
    assert!(matches!(
        scan(scratch.path(), Dialect::Omp, Some(&gap)),
        Err(ScanError::CheckpointGap { .. })
    ));
    scratch.remove();
}

#[test]
fn a_replacement_generation_can_only_begin_at_byte_zero() {
    let scratch = Scratch::new("replacement");
    let path = scratch.path().join("selected");
    let tape = Tape::open(&path).expect("first generation opens");
    tape.tee(&line(PROSE)).expect("first generation grows");
    let old = scan(&path, Dialect::Omp, None).expect("first generation scans");
    fs::remove_dir_all(&path).expect("first generation is removed");
    let replacement = Tape::open(&path).expect("replacement generation opens");

    assert!(matches!(
        scan(&path, Dialect::Omp, Some(&old.checkpoint)),
        Err(ScanError::SourceChanged { .. })
    ));

    let reset = Checkpoint {
        source: old.checkpoint.source,
        next: Position::start(),
    };
    let restarted = scan(&path, Dialect::Omp, Some(&reset))
        .expect("another generation may begin only at byte zero");
    assert_eq!(
        restarted.checkpoint.source,
        replacement.source_id().expect("replacement source reads")
    );
    scratch.remove();
}

#[test]
fn missing_malformed_and_unreadable_source_identity_all_refuse_scanning() {
    for (label, damage) in [("missing", 0_u8), ("malformed", 1_u8), ("unreadable", 2_u8)] {
        let scratch = Scratch::new(label);
        Tape::open(scratch.path()).expect("fixture tape opens");
        let source = scratch.path().join("source");
        fs::remove_file(&source).expect("fixture removes current identity");
        match damage {
            0 => {}
            1 => fs::write(&source, b"not a tape identity\n").expect("malformed identity writes"),
            2 => fs::create_dir(&source).expect("directory obstructs identity read"),
            _ => unreachable!(),
        }

        assert!(
            matches!(
                scan(scratch.path(), Dialect::Omp, None),
                Err(ScanError::Tape { .. })
            ),
            "{label} identity did not refuse ingestion"
        );
        scratch.remove();
    }
}

/// This consumer seam compiles using public listen records and checkpoints;
/// it has neither provider JSON nor an effort-owned persistence shape.
fn consume_public_scan(scan: Scan) -> Vec<(Checkpoint, u64, EventKind, Vec<Span>)> {
    scan.records
        .into_iter()
        .flat_map(|record| match record.payload {
            RecordPayload::Events(events) => events
                .into_iter()
                .map(|event| {
                    (
                        record.next.clone(),
                        event.ordinal,
                        event.event.kind(),
                        event.spans,
                    )
                })
                .collect(),
            RecordPayload::Residue(_) => Vec::new(),
        })
        .collect()
}

#[test]
fn consumers_receive_only_public_typed_events_and_checkpoints() {
    let scratch = Scratch::new("public-seam");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    tape.tee(&line(PROSE)).expect("record is durable");

    let consumed = consume_public_scan(
        scan(scratch.path(), Dialect::Omp, None).expect("public scan succeeds"),
    );

    assert_eq!(consumed.len(), 1);
    assert_eq!((consumed[0].1, consumed[0].2), (0, EventKind::Prose));
    scratch.remove();
}

#[test]
fn event_ordinals_distinguish_events_sharing_the_same_record_spans() {
    let scratch = Scratch::new("ordinals");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    tape.tee(&line(MULTI_EVENT))
        .expect("multi-event record is durable");

    let scanned = scan(scratch.path(), Dialect::Omp, None).expect("record scans");
    let RecordPayload::Events(events) = &scanned.records[0].payload else {
        panic!("current omp record became residue");
    };

    assert_eq!(
        events
            .iter()
            .map(|event| (event.ordinal, event.event.kind()))
            .collect::<Vec<_>>(),
        vec![(0, EventKind::Prose), (1, EventKind::Tool)]
    );
    assert_eq!(events[0].spans, events[1].spans);
    scratch.remove();
}

const NEEDLE: &str = "needle-7Qx";
/// Active-member bytes one backward search read covers.
const SEARCH_CHUNK_BYTES: usize = 1024 * 1024;

fn needle_prose(label: &str) -> Vec<u8> {
    format!(r#"{{"type":"message_end","message":{{"role":"assistant","content":[{{"type":"text","text":"{NEEDLE} {label}"}}],"stopReason":"stop"}}}}"#)
        .into_bytes()
}

/// One complete residue record of exactly `bytes` bytes, delimiter included.
fn filler(bytes: usize) -> Vec<u8> {
    let mut filler = vec![b' '; bytes - 3];
    filler.extend_from_slice(b"{}\n");
    filler
}

fn search_budget(window_bytes: u64) -> TailBudget {
    TailBudget::new(
        window_bytes,
        64 * 1024 * 1024,
        16,
        64,
        Duration::from_secs(10),
    )
    .expect("positive search budget")
}

fn prose_text(record: &Record) -> String {
    let RecordPayload::Events(events) = &record.payload else {
        panic!("a matching record became residue");
    };
    events[0].event.prose().expect("prose").text.clone()
}

#[test]
fn search_decodes_only_needle_records_newest_first_across_chunks_and_seals() {
    let scratch = Scratch::new("search-needles");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    tape.tee(&line(&needle_prose("one"))).expect("oldest match");
    let crossing = line(&needle_prose("two"));
    let split = crossing
        .windows(NEEDLE.len())
        .position(|window| window == NEEDLE.as_bytes())
        .expect("the record names the needle")
        + "needle".len();
    assert!(
        crossing[..split].ends_with(b"needle"),
        "the seal splits the needle"
    );
    tape.tee(&crossing[..split]).expect("needle prefix");
    tape.seal().expect("member seals inside the needle");
    tape.tee(&crossing[split..]).expect("needle suffix");
    tape.tee(&filler(64 * 1024)).expect("older filler");
    let chunked = line(&needle_prose("three"));
    let at = chunked
        .windows(NEEDLE.len())
        .position(|window| window == NEEDLE.as_bytes())
        .unwrap();
    let unterminated = needle_prose("tail");
    tape.tee(&chunked).expect("chunk-straddling match");
    tape.tee(&filler(
        SEARCH_CHUNK_BYTES + 2 + at - chunked.len() - unterminated.len(),
    ))
    .expect("newer filler");
    tape.tee(&unterminated).expect("unterminated match");

    let mut visited = Vec::new();
    let search = search_tail(
        scratch.path(),
        Dialect::Omp,
        &[NEEDLE.as_bytes()],
        search_budget(16 * 1024 * 1024),
        |record| {
            visited.push(prose_text(&record));
            ControlFlow::Continue(())
        },
    )
    .expect("bounded search completes");

    assert_eq!(
        visited,
        [
            format!("{NEEDLE} three"),
            format!("{NEEDLE} two"),
            format!("{NEEDLE} one")
        ]
    );
    assert_eq!(search.work.records, 3, "only matching records are decoded");
    assert_eq!(search.start, Position::start());
    scratch.remove();
}

#[test]
fn search_window_stops_before_older_history() {
    let scratch = Scratch::new("search-window");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    tape.tee(&line(&needle_prose("old"))).expect("older match");
    for _ in 0..32 {
        tape.tee(&filler(64 * 1024)).expect("newer filler");
    }

    let search = search_tail(
        scratch.path(),
        Dialect::Omp,
        &[NEEDLE.as_bytes()],
        search_budget(4 * 1024),
        |_| panic!("a match beyond the window is visited"),
    )
    .expect("bounded search stops");

    assert!(search.start > Position::start());
    assert!(search.work.source_bytes <= SEARCH_CHUNK_BYTES as u64 + 64 * 1024);
    scratch.remove();
}
