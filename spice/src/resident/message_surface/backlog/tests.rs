use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

/// Tool records outgrowing one startup window at near 1.9 KiB each.
const SPARSE_TOOL_RECORDS: usize = 640;
/// Tool records outgrowing one history window at near 1.9 KiB each.
const SPARSE_HORIZON_RECORDS: usize = 4_600;

fn visible_cards(scan: &listen::Scan) -> usize {
    scan.records
        .iter()
        .map(|record| {
            let turn = usize::from(
                record.context.as_ref().and_then(|context| context.turn)
                    == Some(listen::TurnMoment::Started),
            );
            let listen::RecordPayload::Events(events) = &record.payload else {
                return turn;
            };
            turn + events
                .iter()
                .filter(|event| match &event.event {
                    listen::Event::Prose(prose) => prose.role == listen::ProseRole::Assistant,
                    listen::Event::Compaction(_) => true,
                    listen::Event::Tool(tool) => match &tool.moment {
                        listen::ToolMoment::Result(outcome) => !outcome.media.is_empty(),
                        listen::ToolMoment::Call { .. } | listen::ToolMoment::Update { .. } => {
                            false
                        }
                    },
                    _ => false,
                })
                .count()
        })
        .sum()
}

#[test]
fn startup_work_caps_lane_reads_and_total_source_bytes() {
    let mut aggregate = StartupWork::default();
    for _ in 0..STARTUP_LANE_READ_LIMIT {
        assert!(aggregate.admits_lane());
        aggregate.begin_lane();
        aggregate.add(listen::TailWork {
            source_bytes: STARTUP_SOURCE_BYTES,
            records: 1,
            passes: 1,
            duration: Duration::from_millis(1),
        });
    }
    assert!(!aggregate.admits_lane(), "the spent ceiling closes startup",);
    assert_eq!(aggregate.lanes, STARTUP_LANE_READ_LIMIT);
    assert_eq!(aggregate.source_bytes, STARTUP_SOURCE_BYTE_LIMIT);
    assert_eq!(aggregate.records, STARTUP_LANE_READ_LIMIT);
    assert_eq!(aggregate.passes, STARTUP_LANE_READ_LIMIT);
}

#[test]
fn a_settled_turn_materializes_history_as_one_final_card() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "spice-backlog-final-history-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    let tape = tape::Tape::open(&root).expect("final history tape opens");
    tape.tee(
        concat!(
            "{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",",
            "\"content\":[{\"type\":\"text\",\"text\":\"settled final\"}],",
            "\"usage\":{\"input\":1,\"output\":1},\"stopReason\":\"stop\"}}\n",
            "{\"type\":\"prompt_result\",\"id\":\"prompt-7\",\"agentInvoked\":true,\"status\":\"completed\",\"sessionSettled\":true}\n",
        )
        .as_bytes(),
    )
    .expect("final history is durable");
    let scan = listen::scan(&root, listen::Dialect::Omp, None).expect("final history scans");
    let messages = CollectedMessages::new(serve::stream::MESSAGE_HISTORY_PAGE_LIMIT, None);
    let mut reader = BacklogReader::default();
    let mut diagnostic = Vec::new();
    publish_scan(
        scan,
        "018f82d4-6f59-7d06-92e3-bce9131163e5"
            .parse()
            .expect("fixture lane"),
        "018f82d4-6f59-7d06-92e3-bce9131163ef"
            .parse()
            .expect("fixture author"),
        None,
        &messages,
        &mut reader,
        &mut diagnostic,
    )
    .expect("final history materializes");

    assert!(diagnostic.is_empty());
    let collected = messages.inner.into_inner().expect("history lock").messages;
    assert_eq!(collected.len(), 1);
    assert_eq!(collected[0].text, "settled final");
    assert!(collected[0].final_answer);
    fs::remove_dir_all(root).expect("final history fixture cleanup");
}

#[test]
fn cold_replay_reconstructs_a_source_bound_image_path_binding() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "spice-backlog-media-hydration-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    let tape_root = root.join("tape");
    let tape = tape::Tape::open(&tape_root).expect("media replay tape opens");
    let bytes = b"\x89PNG\r\n\x1a\ncold-replay";
    let media =
        wire::Media::from_bytes("provider.png", "image/png", bytes).expect("fixture image admits");
    let payload = media
        .data_url
        .split_once(";base64,")
        .expect("canonical data URL")
        .1;
    let rows = [
        serde_json::json!({
            "type": "message_end",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "toolCall",
                    "id": "call-image",
                    "name": "read",
                    "arguments": {"i": "Reading image", "path": "/tmp/cold.png"}
                }],
                "stopReason": "toolUse"
            }
        }),
        serde_json::json!({
            "type": "message_end",
            "message": {
                "role": "toolResult",
                "toolCallId": "call-image",
                "toolName": "read",
                "content": [
                    {"type": "text", "text": "Read image file [image/png]"},
                    {"type": "image", "data": payload, "mimeType": "image/png"}
                ],
                "isError": false
            }
        }),
        serde_json::json!({
            "type": "message_end",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "text",
                    "text": "![cold proof](/tmp/cold.png \"Cold title\")"
                }],
                "stopReason": "stop"
            }
        }),
    ];
    let contents = rows
        .into_iter()
        .map(|row| format!("{row}\n"))
        .collect::<String>();
    tape.tee(contents.as_bytes())
        .expect("media replay records are durable");

    let scan = listen::scan(&tape_root, listen::Dialect::Omp, None).expect("media replay scans");
    let messages = CollectedMessages::new(serve::stream::MESSAGE_HISTORY_PAGE_LIMIT, None);
    let mut reader = BacklogReader::default();
    let mut diagnostic = Vec::new();
    publish_scan(
        scan,
        "018f82d4-6f59-7d06-92e3-bce9131163e5"
            .parse()
            .expect("fixture lane"),
        "018f82d4-6f59-7d06-92e3-bce9131163ef"
            .parse()
            .expect("fixture author"),
        Some(&tape_root),
        &messages,
        &mut reader,
        &mut diagnostic,
    )
    .expect("media replay materializes");

    assert!(diagnostic.is_empty());
    let collected = messages.inner.into_inner().expect("history lock").messages;
    let prose = collected
        .iter()
        .find(|message| message.text.contains("cold proof"))
        .expect("assistant image prose projects");
    assert_eq!(prose.markdown_images.len(), 1);
    assert_eq!(prose.markdown_images[0].destination, "/tmp/cold.png");
    assert_eq!(prose.markdown_images[0].media.identity(), media.identity());
    fs::remove_dir_all(root).expect("media replay fixture cleanup");
}

#[test]
fn replay_keeps_vendor_turn_boundaries_after_compaction() {
    assert_replay_boundary(concat!(
        "{\"type\":\"auto_compaction_end\",\"timestamp\":\"2026-08-09T01:00:00.000Z\",",
        "\"result\":{\"summary\":\"compacted context\"}}\n",
        "{\"type\":\"agent_start\",\"timestamp\":\"2026-08-09T01:00:01.000Z\"}\n",
        "{\"type\":\"message_end\",\"timestamp\":\"2026-08-09T01:00:02.000Z\",",
        "\"message\":{\"role\":\"assistant\",",
        "\"content\":[{\"type\":\"text\",\"text\":\"resumed work\"}]}}\n",
    ));
}

#[test]
fn cold_replay_marks_only_a_session_opened_without_messages() {
    const STATE: &str = "{\"id\":\"spice-state\",\"type\":\"response\",\"command\":\"get_state\",\"success\":true,\"data\":";
    let prose = |minute: u32, text: &str| {
        format!(
            "{{\"type\":\"message_end\",\"timestamp\":\"2026-08-09T01:{minute:02}:00.000Z\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}}}\n"
        )
    };
    let records = [
        prose(0, "before renewal"),
        format!("{STATE}{{\"sessionId\":\"fresh\",\"messageCount\":0}}}}\n"),
        prose(1, "first fresh answer"),
        format!("{STATE}{{\"sessionId\":\"fresh\",\"messageCount\":2}}}}\n"),
        prose(2, "after resume"),
        format!("{STATE}{{\"sessionId\":\"fresh\"}}}}\n"),
        prose(3, "after unclassified start"),
    ];
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "spice-backlog-fresh-session-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    let tape = tape::Tape::open(&root).expect("fresh session tape opens");
    tape.tee(records.concat().as_bytes())
        .expect("fresh session sequence is durable");
    let scan = listen::scan(&root, listen::Dialect::Omp, None).expect("session sequence scans");
    let messages = CollectedMessages::new(serve::stream::MESSAGE_HISTORY_PAGE_LIMIT, None);
    let mut reader = BacklogReader::default();
    let mut diagnostic = Vec::new();
    publish_scan(
        scan,
        "018f82d4-6f59-7d06-92e3-bce9131163e5"
            .parse()
            .expect("fixture lane"),
        "018f82d4-6f59-7d06-92e3-bce9131163ef"
            .parse()
            .expect("fixture author"),
        None,
        &messages,
        &mut reader,
        &mut diagnostic,
    )
    .expect("session sequence materializes");

    assert!(diagnostic.is_empty());
    let collected = messages.inner.into_inner().expect("history lock").messages;
    let sessions = collected
        .iter()
        .filter(|message| message.activity == wire::AgentActivityKind::Session)
        .collect::<Vec<_>>();
    assert_eq!(
        sessions.len(),
        1,
        "a resumed session and a state without a message count mark nothing"
    );
    let fresh_offset = u64::try_from(records[0].len()).expect("fixture offset fits");
    assert_eq!(
        (sessions[0].locus.offset, sessions[0].locus.length),
        (
            fresh_offset,
            u64::try_from(records[1].len()).expect("fixture length fits")
        ),
        "the divider is anchored at the state record that opened the session",
    );
    assert!(sessions[0].activity.is_boundary());
    fs::remove_dir_all(root).expect("fresh session fixture cleanup");
}

fn assert_replay_boundary(contents: &str) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "spice-backlog-turn-boundary-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    let tape = tape::Tape::open(&root).expect("turn boundary tape opens");
    tape.tee(contents.as_bytes())
        .expect("compacted turn sequence is durable");
    let scan = listen::scan(&root, listen::Dialect::Omp, None).expect("turn sequence scans");
    let messages = CollectedMessages::new(serve::stream::MESSAGE_HISTORY_PAGE_LIMIT, None);
    let mut reader = BacklogReader::default();
    let mut diagnostic = Vec::new();
    publish_scan(
        scan,
        "018f82d4-6f59-7d06-92e3-bce9131163e5"
            .parse()
            .expect("fixture lane"),
        "018f82d4-6f59-7d06-92e3-bce9131163ef"
            .parse()
            .expect("fixture author"),
        None,
        &messages,
        &mut reader,
        &mut diagnostic,
    )
    .expect("compacted turn sequence materializes");

    assert!(diagnostic.is_empty());
    let collected = messages.inner.into_inner().expect("history lock").messages;
    assert_eq!(
        collected
            .iter()
            .map(|message| message.activity)
            .collect::<Vec<_>>(),
        vec![
            wire::AgentActivityKind::Compaction,
            wire::AgentActivityKind::Turn,
            wire::AgentActivityKind::Prose,
        ]
    );
    assert_eq!(collected[2].text, "resumed work");
    fs::remove_dir_all(root).expect("turn boundary fixture cleanup");
}

fn timestamp(index: usize) -> String {
    format!(
        "2030-04-12T18:{:02}:{:02}.000Z",
        40 + index / 60,
        index % 60,
    )
}

struct PagedHistoryFixture {
    root: PathBuf,
    lane: CanonicalUuid,
    source_end: listen::Checkpoint,
    history: DurableHistory,
    future: wire::AgentMessage,
}

/// A tape of 140 wide assistant messages with a committed compaction after
/// the 30th, 60th, and 90th.
fn paged_history_fixture() -> PagedHistoryFixture {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "spice-backlog-paged-history-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    let tape = tape::Tape::open(&root).expect("paged tape opens");
    let filler = "x".repeat(1_800);
    let mut tape_contents = Vec::new();
    for index in 0..140 {
        let timestamp = timestamp(index);
        tape_contents.extend_from_slice(
            format!(
                "{{\"type\":\"message_end\",\"timestamp\":\"{timestamp}\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"message {index} {filler}\"}}]}}}}\n"
            )
            .as_bytes(),
        );
        if matches!(index, 30 | 60 | 90) {
            tape_contents.extend_from_slice(
                format!(
                    "{{\"type\":\"auto_compaction_end\",\"timestamp\":\"{timestamp}\",\"action\":\"snapcompact\",\"result\":{{\"summary\":\"so far\",\"firstKeptEntryId\":\"e{index}\",\"tokensBefore\":900000}},\"aborted\":false,\"willRetry\":false}}\n"
                )
                .as_bytes(),
            );
        }
    }
    tape.tee(&tape_contents).expect("paged tape is durable");

    let lane: CanonicalUuid = "018f82d4-6f59-7d06-92e3-bce9131163e5"
        .parse()
        .expect("fixture lane");
    let author: CanonicalUuid = "018f82d4-6f59-7d06-92e3-bce9131163ef"
        .parse()
        .expect("fixture author");
    let source_end = listen::scan(&root, listen::Dialect::Omp, None)
        .expect("paged tape scans")
        .checkpoint;
    let history = DurableHistory {
        lanes: BTreeMap::from([(
            lane,
            DurableLane {
                tape: root.clone(),
                dialect: listen::Dialect::Omp,
                author,
                source: Some(source_end.clone()),
            },
        )]),
        progress: HistoryProgress::default(),
        hydration: Mutex::new(Hydration::default()),
        bus: None,
        steering: PresentationDirectives::default(),
    };
    let future = wire::AgentMessage {
        prose_origin: None,
        lane,
        author,
        locus: wire::MessageLocus {
            member: source_end.next.member,
            offset: source_end.next.offset,
            length: 0,
            event: 0,
        },
        authored: "2031-04-12T18:44:09.271Z"
            .parse()
            .expect("future fixture instant"),
        authored_synthetic: false,
        observed: None,
        activity: wire::AgentActivityKind::Prose,
        compaction_method: None,
        final_answer: false,
        acknowledgement: None,
        text: "future".to_owned(),
        provider_error: None,
        quote_target: None,
        directive_contexts: Vec::new(),
        sends: Vec::new(),
        quotes: Vec::new(),
        automatic_speech: None,
        media: Vec::new(),
        media_quotes: Vec::new(),
        markdown_images: Vec::new(),
        context_bytes: 0,
    };
    PagedHistoryFixture {
        root,
        lane,
        source_end,
        history,
        future,
    }
}

fn collect_history_pages(
    fixture: &PagedHistoryFixture,
) -> (Vec<usize>, Vec<wire::AgentMessage>, wire::AgentMessage) {
    let source = fixture
        .history
        .lanes
        .get(&fixture.lane)
        .expect("fixture lane source");
    let initial = fixture
        .history
        .tape_messages(
            fixture.lane,
            source,
            Some(&fixture.future),
            &fixture.source_end,
            wire::MESSAGE_FLOOR,
        )
        .expect("initial tape tail reads");
    let initial_before = initial.messages[0].clone();
    let mut before = initial_before.clone();
    let mut restored = Vec::new();
    let mut page_lengths = Vec::new();
    for _ in 0..128 {
        let page = serve::stream::MessageHistoryProvider::history(
            &fixture.history,
            fixture.lane,
            Some(&before),
            serve::stream::MESSAGE_HISTORY_PAGE_LIMIT,
        )
        .expect("merged history reads")
        .expect("fixture lane is owned");
        assert!(
            !page.messages.is_empty(),
            "history never answers an open lane with an empty page",
        );
        assert!(page.messages.len() <= serve::stream::MESSAGE_HISTORY_PAGE_LIMIT);
        page_lengths.push(page.messages.len());
        let oldest = page.messages[0].clone();
        restored.splice(0..0, page.messages);
        if !page.more {
            return (page_lengths, restored, initial_before);
        }
        before = oldest;
    }
    panic!(
        "merged history did not exhaust after {} visible page(s) and {} restored rows; last page lengths {:?}",
        page_lengths.len(),
        restored.len(),
        &page_lengths[page_lengths.len().saturating_sub(8)..],
    )
}

/// Write an omp season of `messages` visible agent messages followed by
/// `tools` tool calls without a visible card, each record near 1.9 KiB.
fn season_tape(tag: &str, messages: usize, tools: usize) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "spice-backlog-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    let tape = tape::Tape::open(&root).expect("season tape opens");
    let filler = "x".repeat(1_800);
    let mut season = Vec::new();
    for index in 0..messages {
        season.extend_from_slice(
            format!(
                "{{\"type\":\"message_end\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{index} {filler}\"}}]}}}}\n"
            )
            .as_bytes(),
        );
    }
    for index in 0..tools {
        season.extend_from_slice(
            format!(
                "{{\"type\":\"tool_execution_start\",\"toolCallId\":\"call-{index}\",\"toolName\":\"bash\",\"args\":{{\"command\":\"echo {filler}\"}}}}\n"
            )
            .as_bytes(),
        );
    }
    tape.tee(&season).expect("season is durable");
    root
}

fn season_history(root: &Path) -> (DurableHistory, CanonicalUuid, listen::Checkpoint) {
    let lane: CanonicalUuid = "018f82d4-6f59-7d06-92e3-bce9131163e5"
        .parse()
        .expect("fixture lane");
    let author: CanonicalUuid = "018f82d4-6f59-7d06-92e3-bce9131163ef"
        .parse()
        .expect("fixture author");
    let source_end = listen::scan(root, listen::Dialect::Omp, None)
        .expect("season endpoint reads")
        .checkpoint;
    let history = DurableHistory {
        lanes: BTreeMap::from([(
            lane,
            DurableLane {
                tape: root.to_path_buf(),
                dialect: listen::Dialect::Omp,
                author,
                source: Some(source_end.clone()),
            },
        )]),
        progress: HistoryProgress::default(),
        hydration: Mutex::new(Hydration::default()),
        bus: None,
        steering: PresentationDirectives::default(),
    };
    (history, lane, source_end)
}

#[test]
fn startup_tail_reads_one_fixed_window_before_byte_zero() {
    // Eight hundred records exceed the 1 MiB startup window, while many
    // initial pages fit inside it. Cold replay reads that one window and
    // leaves the rest to pageable history.
    const DENSE_LANE_MESSAGES: usize = 800;
    let root = season_tape("tail", DENSE_LANE_MESSAGES, 0);

    let tail = tail_backlog(&root, listen::Dialect::Omp).expect("season tail scans");
    assert!(
        visible_cards(&tail.scan) > wire::MESSAGE_FLOOR,
        "one startup window carries more than the initial page of a dense lane",
    );
    assert!(visible_cards(&tail.scan) < DENSE_LANE_MESSAGES);
    assert!(tail.work.source_bytes <= STARTUP_SOURCE_BYTES);
    assert!(tail.work.source_bytes > STARTUP_WINDOW_BYTES);
    assert_eq!(tail.work.records, tail.scan.records.len());
    assert!(tail.work.passes <= tail_passes(STARTUP_SOURCE_BYTES));
    let first_offset = tail
        .scan
        .records
        .first()
        .and_then(|record| match &record.payload {
            listen::RecordPayload::Events(events) => events.first(),
            listen::RecordPayload::Residue(_) => None,
        })
        .and_then(|event| event.spans.first())
        .map(|span| span.offset)
        .expect("tail has a first sourced event");
    assert!(first_offset > 0, "startup does not scan back to byte zero");

    fs::remove_dir_all(root).expect("season fixture cleanup");
}

#[test]
fn a_record_beyond_the_admitted_length_refuses_within_its_allowance() {
    // Nothing admitted can land a record this long, so the retreat runs
    // out of source bytes rather than reading the tape without a bound. The
    // refusal names the dimension it spent and reports the work it did.
    let root = season_tape("oversize-record", 30, 0);
    let tape = tape::Tape::open(&root).expect("season tape reopens");
    // The retreat pays for every byte of the record below the window it was
    // asked for, so the record has to outrun the whole allowance, not the
    // admitted length alone.
    let mut oversize =
        vec![b'x'; STARTUP_SOURCE_BYTES as usize + 2 * STARTUP_WINDOW_BYTES as usize];
    oversize.push(b'\n');
    tape.tee(&oversize).expect("the long record is durable");

    let error = tail_backlog(&root, listen::Dialect::Omp)
        .expect_err("a record beyond the admitted length is refused");
    let listen::ScanError::WorkLimit {
        dimension,
        limit,
        work,
        ..
    } = &error
    else {
        panic!("the refusal names the allowance it spent: {error}");
    };
    assert_eq!(*dimension, "source bytes");
    assert_eq!(*limit, STARTUP_SOURCE_BYTES);
    assert!(work.source_bytes <= STARTUP_SOURCE_BYTES);
    assert!(work.passes <= tail_passes(STARTUP_SOURCE_BYTES));

    fs::remove_dir_all(root).expect("oversize fixture cleanup");
}

#[test]
fn tool_only_tail_stops_at_one_fixed_read_with_a_backward_cursor() {
    let root = season_tape("sparse-tail", 30, SPARSE_TOOL_RECORDS);

    let tail = tail_backlog(&root, listen::Dialect::Omp).expect("sparse tail scans");
    assert_eq!(
        visible_cards(&tail.scan),
        0,
        "one startup window need not cross sparse tools to reach a visible card",
    );
    assert!(
        tail.scan.records.len() < SPARSE_TOOL_RECORDS,
        "the scanner does not cross the complete sparse tool tail",
    );
    assert!(tail.start.next > listen::Position::start());
    assert!(tail.work.source_bytes <= STARTUP_SOURCE_BYTES);
    assert_eq!(tail.work.records, tail.scan.records.len());
    assert!(tail.work.passes <= tail_passes(STARTUP_SOURCE_BYTES));

    fs::remove_dir_all(root).expect("sparse fixture cleanup");
}

#[test]
fn historical_page_wades_through_a_sparse_tool_horizon_to_fill_the_page() {
    // The tool horizon outgrows one history window, so the first window
    // carries no visible card and the request must chain an older one.
    let root = season_tape("sparse-history", 300, SPARSE_HORIZON_RECORDS);
    let (history, lane, source_end) = season_history(&root);
    let source = history.lanes.get(&lane).expect("season lane");
    let limit = serve::stream::MESSAGE_HISTORY_PAGE_LIMIT;

    let newest = history
        .tape_messages(lane, source, None, &source_end, limit)
        .expect("newest history window reads");
    assert!(newest.messages.is_empty(), "one window holds only tools");
    assert!(newest.more);
    assert!(newest.start.next > listen::Position::start());
    assert!(newest.work.records < SPARSE_HORIZON_RECORDS);
    let newest_start = newest.start.next;
    let newest_end = newest.end.clone();

    let filled = history
        .fill_tape_messages(lane, source, None, limit, newest)
        .expect("history fills across windows");
    assert_eq!(
        filled.messages.len(),
        limit,
        "the page reaches its visible target"
    );
    assert!(filled.more, "older messages remain behind the page");
    assert!(
        filled.start.next < newest_start,
        "the request read older windows"
    );
    assert_eq!(filled.end, newest_end);
    assert!(filled.work.source_bytes > HISTORY_WINDOW_BYTES);
    assert!(filled.work.source_bytes <= HISTORY_SOURCE_BYTES * HISTORY_READ_LIMIT as u64);
    assert!(filled.work.records > SPARSE_HORIZON_RECORDS);
    assert!(filled.messages[0].text.starts_with("250 "));
    assert!(filled.messages[limit - 1].text.starts_with("299 "));
    assert!(
        filled
            .messages
            .windows(2)
            .all(|pair| pair[0].chronology_key() < pair[1].chronology_key()),
    );

    let page = serve::stream::MessageHistoryProvider::history(&history, lane, None, limit)
        .expect("served history reads")
        .expect("season lane is owned");
    assert_eq!(page.messages.len(), limit);
    assert!(page.more);
    let oldest = page.messages[0].clone();
    let older =
        serve::stream::MessageHistoryProvider::history(&history, lane, Some(&oldest), limit)
            .expect("older history reads")
            .expect("season lane is owned");
    assert_eq!(older.messages.len(), limit);
    assert!(older.messages[0].text.starts_with("200 "));

    fs::remove_dir_all(root).expect("sparse history fixture cleanup");
}

#[test]
fn history_request_stops_at_the_tape_start_without_a_visible_card() {
    let root = season_tape("tool-only-history", 0, SPARSE_TOOL_RECORDS);
    let (history, lane, source_end) = season_history(&root);
    let source = history.lanes.get(&lane).expect("season lane");
    let limit = serve::stream::MESSAGE_HISTORY_PAGE_LIMIT;

    let newest = history
        .tape_messages(lane, source, None, &source_end, limit)
        .expect("newest history window reads");
    let filled = history
        .fill_tape_messages(lane, source, None, limit, newest)
        .expect("history reaches the tape start");
    assert!(filled.messages.is_empty());
    assert!(
        !filled.more,
        "a tape with no visible card is exhausted, not pending"
    );
    assert_eq!(filled.start.next, listen::Position::start());
    assert_eq!(filled.work.records, SPARSE_TOOL_RECORDS);

    fs::remove_dir_all(root).expect("tool-only history fixture cleanup");
}

#[test]
fn history_pages_retreat_the_tape_without_gaps_or_rescans() {
    let fixture = paged_history_fixture();
    let (page_lengths, restored, _) = collect_history_pages(&fixture);
    assert!(
        page_lengths
            .iter()
            .all(|length| *length <= serve::stream::MESSAGE_HISTORY_PAGE_LIMIT),
    );
    assert_eq!(
        restored.len(),
        140 - wire::MESSAGE_FLOOR + 3,
        "every message before the initial page and every compaction is restored",
    );
    assert_eq!(
        restored
            .iter()
            .filter(|message| message.activity == wire::AgentActivityKind::Compaction)
            .count(),
        3,
    );
    assert!(
        restored
            .windows(2)
            .all(|pair| pair[0].chronology_key() < pair[1].chronology_key()),
        "merged pages remain in strict shared chronology",
    );
    assert_eq!(
        restored
            .iter()
            .map(|message| (message.locus, message.activity))
            .collect::<std::collections::HashSet<_>>()
            .len(),
        restored.len(),
        "source continuation returns no duplicate card identities",
    );
    let prose = restored
        .iter()
        .filter(|message| message.activity == wire::AgentActivityKind::Prose)
        .collect::<Vec<_>>();
    assert_eq!(prose.len(), 140 - wire::MESSAGE_FLOOR);
    for (index, message) in prose.into_iter().enumerate() {
        assert!(
            message.text.starts_with(&format!("message {index} ")),
            "every tape message before the initial visible boundary is restored exactly",
        );
    }
    fs::remove_dir_all(&fixture.root).expect("paged fixture cleanup");
}

/// One visible card authored `second` seconds into a fixed minute.
fn card_at(member: u64, second: usize) -> wire::AgentMessage {
    wire::AgentMessage {
        prose_origin: None,
        lane: "018f82d4-6f59-7d06-92e3-bce9131163e5"
            .parse()
            .expect("card lane"),
        author: "018f82d4-6f59-7d06-92e3-bce9131163ef"
            .parse()
            .expect("card author"),
        locus: wire::MessageLocus {
            member,
            offset: second as u64,
            length: 1,
            event: 0,
        },
        authored: format!("2030-04-12T18:40:{second:02}.000Z")
            .parse()
            .expect("card instant"),
        authored_synthetic: false,
        observed: None,
        activity: wire::AgentActivityKind::Prose,
        compaction_method: None,
        final_answer: false,
        acknowledgement: None,
        text: format!("card {second}"),
        provider_error: None,
        quote_target: None,
        directive_contexts: Vec::new(),
        sends: Vec::new(),
        quotes: Vec::new(),
        automatic_speech: None,
        media: Vec::new(),
        media_quotes: Vec::new(),
        markdown_images: Vec::new(),
        context_bytes: 0,
    }
}

#[test]
fn a_tape_continuation_resumes_from_physical_progress_not_the_anchor() {
    let at = |member, offset| listen::Checkpoint {
        source: "018f82d4-6f59-7d06-92e3-bce9131163aa"
            .parse()
            .expect("fixture source"),
        next: listen::Position { member, offset },
    };
    let scan = |messages: Vec<wire::AgentMessage>, complete| TapeHistoryScan {
        messages,
        more: true,
        complete,
        start: at(0, 5),
        end: at(1, 90),
        work: listen::TailWork {
            source_bytes: 0,
            records: 0,
            passes: 0,
            duration: std::time::Duration::ZERO,
        },
    };
    let anchor = card_at(1, 10);

    // The page took every card the scan found, so nothing visible lies
    // between where the scan began and the anchor.
    let taken = scan(vec![card_at(1, 10), card_at(1, 20)], true);
    assert_eq!(tape_continuation(&taken, &anchor), at(0, 5));
    let empty = scan(Vec::new(), true);
    assert_eq!(tape_continuation(&empty, &anchor), at(0, 5));

    // Cards the scan dropped, or kept but the page did not serve, lie
    // behind the anchor and must be read again.
    let dropped = scan(vec![card_at(1, 10), card_at(1, 20)], false);
    assert_eq!(tape_continuation(&dropped, &anchor), at(1, 10));
    let unserved = scan(vec![card_at(1, 7), card_at(1, 10)], true);
    assert_eq!(tape_continuation(&unserved, &anchor), at(1, 10));
}

#[test]
fn a_final_revision_of_an_evicted_card_leaves_the_page_intact() {
    let messages = CollectedMessages::new(1, Some(&card_at(1, 50)));
    messages
        .publish(card_at(1, 10))
        .expect("older card collects");
    messages
        .publish(card_at(1, 20))
        .expect("newer card evicts the older");
    let mut evicted = card_at(1, 10);
    evicted.final_answer = true;
    assert!(
        messages
            .revise_final(evicted)
            .expect("an evicted card's revision is dropped")
    );
    let mut kept = card_at(1, 20);
    kept.final_answer = true;
    kept.text = "settled".to_owned();
    assert!(
        messages
            .revise_final(kept.clone())
            .expect("a kept card revises")
    );

    let collected = messages.inner.into_inner().expect("history lock");
    assert!(collected.dropped);
    assert_eq!(collected.messages, vec![kept]);
}
