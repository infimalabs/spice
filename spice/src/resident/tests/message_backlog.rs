//! Restart reconstruction of the bounded app conversation backlog.

use super::*;

const PROSE: &[u8] = br#"{"timestamp":"2030-04-12T18:44:09.271Z","type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"durable restart backlog"}]}}
"#;
const PROSE_WITHOUT_TIME: &[u8] = br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"durable restart backlog"}],"stopReason":"stop"}}
"#;
const PROMPT_RESULT: &[u8] = br#"{"type":"prompt_result","id":"prompt-7","agentInvoked":true,"status":"completed","sessionSettled":true}
"#;
const COMPACTION_WITHOUT_TIME: &[u8] =
    br#"{"type":"auto_compaction_end","result":{"firstKeptEntryId":"entry-1","tokensBefore":100}}
"#;
const APPEND: &[u8] = br#"{"timestamp":"2030-04-12T18:45:09.271Z","type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"appended after initial backlog"}]}}
"#;
const DISTRIBUTED_ACK: &[u8] = br#"{"timestamp":"2030-04-12T18:46:09.271Z","type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"ACK\t1kQwPZtKx nope 1kQwPZtMx: One response.\nNAK\t1kQwPZtLx: Unsafe."}]}}
"#;

#[test]
fn app_steering_recovery_chunks_keys_at_the_bus_batch_ceiling() {
    let bus = Bus::start(journal::Journal::in_memory().expect("message authority opens"))
        .expect("message bus starts");
    let keys = (0..=bus::MAX_STEERING_PRESENTATION_KEYS)
        .map(|index| {
            format!("{index:09}")
                .parse::<steer::Key>()
                .expect("current fixture steering key")
        })
        .collect();

    let rows = message_surface::load_steering_presentations(&bus.reader(), &keys)
        .expect("app recovery pages an oversized key set");

    assert!(rows.is_empty());
    bus.shutdown().expect("message bus stops");
}

fn history_cursors(generation: &wire::TranscriptGeneration) -> Vec<wire::MessageHistoryCursor> {
    generation
        .frontiers
        .iter()
        .filter(|frontier| frontier.state == wire::TranscriptLaneState::Truncated)
        .map(|frontier| wire::MessageHistoryCursor {
            generation: generation.generation,
            lane: frontier.lane,
            before: frontier.before,
        })
        .collect()
}

fn assert_bootstrap(generation: &wire::TranscriptGeneration, lane: wire::CanonicalUuid) {
    assert_eq!(
        history_cursors(generation)
            .into_iter()
            .find(|cursor| cursor.lane == lane),
        Some(wire::MessageHistoryCursor {
            generation: generation.generation,
            lane,
            before: None,
        }),
    );
}

#[test]
fn startup_history_conflict_keeps_accepted_cards_and_publishes_the_remaining_backlog() {
    let (root, paths) = resident_fixture("message-backlog-conflict");
    let bus = Bus::start(journal::Journal::in_memory().expect("backlog authority opens"))
        .expect("backlog bus starts");
    let epoch = test_uuid(4);
    let lane = test_uuid(5);
    let author = test_uuid(7);
    seat_backlog_residency(&bus, epoch, lane, test_uuid(6), author, &root);
    let tape = tape::Tape::open(paths.lane_tape(lane).join("omp")).expect("the backlog tape opens");
    tape.tee(PROSE).expect("durable prose is captured");
    let source = message_source(bus.reader(), epoch);
    let publisher = source.message_publisher();
    let residency = current_residency(&bus, epoch).expect("residency reads");
    let mut reader = message_surface::BacklogReader::indexed();
    let mut diagnostic = Vec::new();
    message_surface::restore_agent_messages_indexed(
        &bus.reader(),
        &paths,
        residency.body(),
        &publisher,
        &mut reader,
        &mut diagnostic,
    )
    .expect("first backlog restores");
    let accepted = operator_session(epoch);
    let request = wire::StreamRequest::Subscribe {
        v: wire::WireVersion::CURRENT,
        id: test_uuid(8),
        epoch,
        subjects: vec![wire::ViewSubject::Messages],
        after: None,
    };
    let (_, frames) = source
        .begin(&accepted, &request)
        .expect("messages subscribe");
    let message = frames
        .iter()
        .find_map(|frame| match frame {
            wire::StreamFrame::Messages { generation, .. } => generation.messages.first(),
            _ => None,
        })
        .expect("first card is published");
    publisher
        .revise_presentation(
            lane,
            message.locus,
            serve::stream::MessagePresentation::plain("accepted presentation".to_owned()),
        )
        .expect("accepted card has a later presentation");
    tape.tee(APPEND).expect("another durable card is captured");
    diagnostic.clear();
    message_surface::restore_agent_messages_indexed(
        &bus.reader(),
        &paths,
        residency.body(),
        &publisher,
        &mut message_surface::BacklogReader::indexed(),
        &mut diagnostic,
    )
    .expect("a conflicting history projection must not terminate the resident");
    let (_, frames) = source
        .begin(&accepted, &request)
        .expect("messages still subscribe");
    let messages = frames
        .iter()
        .find_map(|frame| match frame {
            wire::StreamFrame::Messages { generation, .. } => Some(&generation.messages),
            _ => None,
        })
        .expect("restored cards are available");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].text, "accepted presentation");
    assert_eq!(messages[1].text, "appended after initial backlog");
    assert!(String::from_utf8_lossy(&diagnostic).contains("serve::message-publish-failed"));
    bus.shutdown().expect("backlog bus stops");
    fs::remove_dir_all(root).expect("backlog fixture cleanup");
}

#[test]
fn each_resident_generation_restores_durable_prose_before_app_subscription() {
    let (root, paths) = resident_fixture("message-backlog");
    let bus = Bus::start(journal::Journal::in_memory().expect("backlog authority opens"))
        .expect("backlog bus starts");
    let epoch = test_uuid(4);
    let lane = test_uuid(5);
    let empty_lane = test_uuid(6);
    let author = test_uuid(7);
    seat_backlog_residency(&bus, epoch, lane, empty_lane, author, &root);
    let tape = tape::Tape::open(paths.lane_tape(lane).join("omp")).expect("the backlog tape opens");
    tape.tee(PROSE_WITHOUT_TIME)
        .expect("durable prose is captured");
    tape.tee(PROMPT_RESULT)
        .expect("durable turn completion is captured");
    tape.tee(COMPACTION_WITHOUT_TIME)
        .expect("durable compaction is captured");
    let mut ingest_diagnostic = Vec::new();
    let (observations, _) = ingest_transcripts(
        &bus,
        &paths,
        Some(lane),
        lane::ingest::Bound::IMMEDIATE,
        lane::ingest::Coalescing::new(),
        &mut ingest_diagnostic,
    )
    .expect("durable tape is accounted");
    assert_eq!(observations.len(), 1);
    assert!(ingest_diagnostic.is_empty());
    let observed = observations[0].at;

    for generation in 0..2 {
        let source = message_source(bus.reader(), epoch);
        let publisher = source.message_publisher();
        let bus_reader = bus.reader();
        let mut reader = message_surface::BacklogReader::indexed();
        let mut diagnostic = Vec::new();
        let residency = current_residency(&bus, epoch).expect("residency reads");
        message_surface::restore_agent_messages_indexed(
            &bus_reader,
            &paths,
            residency.body(),
            &publisher,
            &mut reader,
            &mut diagnostic,
        )
        .expect("durable backlog restores");
        assert!(
            diagnostic.is_empty(),
            "generation {generation} restore diagnosed: {}",
            String::from_utf8_lossy(&diagnostic),
        );

        let accepted = operator_session(epoch);
        let request = wire::StreamRequest::Subscribe {
            v: wire::WireVersion::CURRENT,
            id: test_uuid(8),
            epoch,
            subjects: vec![wire::ViewSubject::Messages],
            after: None,
        };
        let (_, frames) = source
            .begin(&accepted, &request)
            .expect("app subscription begins");
        let wire::StreamFrame::Messages {
            limit, generation, ..
        } = &frames[0]
        else {
            panic!("expected message snapshot")
        };
        let messages = &generation.messages;
        assert_eq!(*limit, 256, "a missing sibling tape is not zero capacity");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].lane, lane);
        assert_eq!(messages[0].author, author);
        assert_eq!(messages[0].text, "durable restart backlog");
        assert!(messages[0].final_answer);
        assert_eq!(
            messages[0]
                .automatic_speech
                .as_ref()
                .map(|speech| speech.kind),
            Some(wire::AutomaticSpeechKind::Final),
        );
        assert!(messages[0].authored_synthetic);
        assert_eq!(messages[0].observed.as_ref(), Some(&observed));
        assert_eq!(messages[1].lane, lane);
        assert_eq!(messages[1].author, author);
        assert_eq!(messages[1].activity, wire::AgentActivityKind::Compaction);
        assert!(messages[1].authored_synthetic);
        assert_eq!(messages[1].observed.as_ref(), Some(&observed));
        assert_bootstrap(generation, empty_lane);
        drop(source);
    }

    bus.shutdown().expect("backlog bus stops");
    fs::remove_dir_all(root).expect("backlog fixture cleanup");
}

#[test]
fn startup_fills_the_visible_floor_across_a_sparse_tool_horizon() {
    let (root, paths) = resident_fixture("message-backlog-sparse-startup");
    let bus = Bus::start(journal::Journal::in_memory().expect("backlog authority opens"))
        .expect("backlog bus starts");
    let epoch = test_uuid(49);
    let lane = test_uuid(50);
    let empty_lane = test_uuid(51);
    let author = test_uuid(52);
    seat_backlog_residency(&bus, epoch, lane, empty_lane, author, &root);
    let tape =
        tape::Tape::open(paths.lane_tape(lane).join("omp")).expect("the sparse backlog tape opens");
    write_sparse_startup_history(&tape);
    let mut ingest_diagnostic = Vec::new();
    ingest_transcripts(
        &bus,
        &paths,
        Some(lane),
        lane::ingest::Bound::IMMEDIATE,
        lane::ingest::Coalescing::new(),
        &mut ingest_diagnostic,
    )
    .expect("sparse tape is accounted");
    assert!(ingest_diagnostic.is_empty());

    let source = message_source(bus.reader(), epoch);
    let publisher = source.message_publisher();
    let bus_reader = bus.reader();
    let mut reader = message_surface::BacklogReader::indexed();
    let mut diagnostic = Vec::new();
    let residency = current_residency(&bus, epoch).expect("residency reads");
    message_surface::restore_agent_messages_indexed(
        &bus_reader,
        &paths,
        residency.body(),
        &publisher,
        &mut reader,
        &mut diagnostic,
    )
    .expect("sparse backlog restores");
    assert!(
        diagnostic.is_empty(),
        "restore diagnosed: {}",
        String::from_utf8_lossy(&diagnostic),
    );

    let accepted = operator_session(epoch);
    let generation = message_snapshot(&source, &accepted, epoch, test_uuid(53));
    let prose = generation
        .messages
        .iter()
        .filter(|message| message.activity.is_prose())
        .collect::<Vec<_>>();
    assert_eq!(
        prose.len(),
        wire::MESSAGE_FLOOR,
        "hidden JSONL records spend startup bytes without shrinking the shown-card floor",
    );
    assert_eq!(
        prose.last().map(|message| message.text.as_str()),
        Some("current epoch"),
    );
    assert!(
        generation.messages.iter().any(|message| {
            message.activity == wire::AgentActivityKind::Compaction
                && message.text == "Context compacted"
        }),
        "the current epoch keeps its preceding compaction boundary",
    );
    assert!(
        generation
            .messages
            .iter()
            .all(|message| message.lane == lane),
        "the empty sibling contributes no borrowed history",
    );
    assert_bootstrap(&generation, empty_lane);
    let reconnected = message_snapshot(&source, &accepted, epoch, test_uuid(54));
    assert_eq!(
        reconnected, generation,
        "every cold reader receives the same authoritative retained window",
    );

    drop(source);
    bus.shutdown().expect("backlog bus stops");
    fs::remove_dir_all(root).expect("backlog fixture cleanup");
}

fn write_sparse_startup_history(tape: &tape::Tape) {
    let mut history = Vec::new();
    for index in 0..30 {
        history.extend_from_slice(
            format!(
                "{{\"type\":\"message_end\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"message {index}\"}}]}}}}\n"
            )
            .as_bytes(),
        );
    }
    history.extend_from_slice(COMPACTION_WITHOUT_TIME);
    append_hidden_tool_horizon(&mut history);
    history.extend_from_slice(
        br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"current epoch"}]}}
"#,
    );
    tape.tee(&history).expect("sparse history is durable");
}

fn append_hidden_tool_horizon(history: &mut Vec<u8>) {
    let filler = "x".repeat(1_800);
    for index in 0..640 {
        history.extend_from_slice(
            format!(
                "{{\"type\":\"message_end\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"toolCall\",\"id\":\"call-{index}\",\"name\":\"bash\",\"arguments\":{{\"command\":\"echo {filler}\"}}}}],\"stopReason\":\"toolUse\"}}}}\n"
            )
            .as_bytes(),
        );
    }
}

fn message_snapshot(
    source: &serve::stream::Source,
    accepted: &wire::Accepted,
    epoch: wire::CanonicalUuid,
    request: wire::CanonicalUuid,
) -> wire::TranscriptGeneration {
    let (_, frames) = source
        .begin(
            accepted,
            &wire::StreamRequest::Subscribe {
                v: wire::WireVersion::CURRENT,
                id: request,
                epoch,
                subjects: vec![wire::ViewSubject::Messages],
                after: None,
            },
        )
        .expect("app subscription begins");
    let wire::StreamFrame::Messages { generation, .. } =
        frames.into_iter().next().expect("message frame exists")
    else {
        panic!("expected message snapshot")
    };
    generation
}

#[test]
fn repeated_backlog_read_resumes_at_its_source_checkpoint_without_missing_an_append() {
    let (root, paths) = resident_fixture("message-backlog-resume");
    let bus = Bus::start(journal::Journal::in_memory().expect("backlog authority opens"))
        .expect("backlog bus starts");
    let epoch = test_uuid(14);
    let lane = test_uuid(15);
    let empty_lane = test_uuid(16);
    let author = test_uuid(17);
    seat_backlog_residency(&bus, epoch, lane, empty_lane, author, &root);
    let tape = tape::Tape::open(paths.lane_tape(lane).join("omp")).expect("the backlog tape opens");
    tape.tee(PROSE).expect("initial prose is captured");
    let source = message_source(bus.reader(), epoch);
    let publisher = source.message_publisher();
    let residency = current_residency(&bus, epoch).expect("residency reads");
    let mut reader = message_surface::BacklogReader::default();
    let mut diagnostic = Vec::new();

    message_surface::restore_agent_messages(
        &paths,
        residency.body(),
        &publisher,
        &mut reader,
        &mut diagnostic,
    )
    .expect("initial backlog restores");
    tape.tee(APPEND).expect("appended prose is captured");
    message_surface::restore_agent_messages(
        &paths,
        residency.body(),
        &publisher,
        &mut reader,
        &mut diagnostic,
    )
    .expect("appended backlog resumes");

    let accepted = operator_session(epoch);
    let request = wire::StreamRequest::Subscribe {
        v: wire::WireVersion::CURRENT,
        id: test_uuid(18),
        epoch,
        subjects: vec![wire::ViewSubject::Messages],
        after: None,
    };
    let (_, frames) = source
        .begin(&accepted, &request)
        .expect("app subscription begins");
    let wire::StreamFrame::Messages { generation, .. } = &frames[0] else {
        panic!("expected message snapshot")
    };
    let messages = &generation.messages;
    assert_eq!(
        messages
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["durable restart backlog", "appended after initial backlog"],
    );
    assert!(
        diagnostic.is_empty(),
        "checkpoint resume diagnosed: {}",
        String::from_utf8_lossy(&diagnostic),
    );

    drop(source);
    bus.shutdown().expect("backlog bus stops");
    fs::remove_dir_all(root).expect("backlog fixture cleanup");
}

#[test]
fn history_reports_a_cursor_unavailable_after_the_durable_tape_source_is_replaced() {
    let (root, paths) = resident_fixture("message-history-source-replaced");
    let bus = Bus::start(journal::Journal::in_memory().expect("backlog authority opens"))
        .expect("backlog bus starts");
    let epoch = test_uuid(64);
    let lane = test_uuid(65);
    let empty_lane = test_uuid(66);
    let author = test_uuid(67);
    seat_backlog_residency(&bus, epoch, lane, empty_lane, author, &root);
    let tape_path = paths.lane_tape(lane).join("omp");
    let tape = tape::Tape::open(&tape_path).expect("the backlog tape opens");
    let history = (0..80)
        .map(|index| {
            format!(
                "{{\"type\":\"message_end\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"message {index}\"}}]}}}}\n"
            )
        })
        .collect::<String>();
    tape.tee(history.as_bytes())
        .expect("durable history is captured");
    drop(tape);

    let source = message_source(bus.reader(), epoch);
    let publisher = source.message_publisher();
    let residency = current_residency(&bus, epoch).expect("residency reads");
    let mut reader = message_surface::BacklogReader::default();
    let mut diagnostic = Vec::new();
    message_surface::restore_agent_messages(
        &paths,
        residency.body(),
        &publisher,
        &mut reader,
        &mut diagnostic,
    )
    .expect("initial backlog restores");
    assert!(diagnostic.is_empty());

    let accepted = operator_session(epoch);
    let (_, frames) = source
        .begin(
            &accepted,
            &wire::StreamRequest::Subscribe {
                v: wire::WireVersion::CURRENT,
                id: test_uuid(68),
                epoch,
                subjects: vec![wire::ViewSubject::Messages],
                after: None,
            },
        )
        .expect("app subscription begins");
    let wire::StreamFrame::Messages { generation, .. } = &frames[0] else {
        panic!("expected message snapshot")
    };
    let first = history_cursors(generation)
        .into_iter()
        .find(|cursor| cursor.lane == lane)
        .expect("the lane advertises older history");
    let history_page = |id, cursor| {
        source.message_history(
            &accepted,
            &wire::StreamRequest::MessageHistory {
                v: wire::WireVersion::CURRENT,
                id: test_uuid(id),
                epoch,
                frontiers: vec![cursor],
            },
        )
    };

    fs::remove_dir_all(&tape_path).expect("old tape source is removed");
    let cached =
        history_page(71, first).expect("retained history does not reread a missing durable source");
    let wire::StreamFrame::MessageHistory {
        generation: interval,
        ..
    } = cached
    else {
        panic!("expected retained look-behind page");
    };
    assert_eq!(
        interval
            .messages
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["message 54"],
    );
    assert_eq!(
        interval.frontiers[0].state,
        wire::TranscriptLaneState::Truncated,
        "the cached page preserves the durable frontier",
    );
    let next = history_cursors(&interval)[0];
    let reason = lane_history_fault(history_page(69, next), lane);
    assert!(
        reason.contains("transcript source disappeared"),
        "source disappearance remains explicit: {reason}",
    );

    let replacement = tape::Tape::open(&tape_path).expect("replacement tape opens");
    replacement
        .tee(history.as_bytes())
        .expect("replacement history is captured");

    let reason = lane_history_fault(history_page(70, next), lane);
    assert!(
        reason.contains("transcript source changed"),
        "replacement diagnosis preserves the source mismatch: {reason}",
    );

    drop(source);
    bus.shutdown().expect("backlog bus stops");
    fs::remove_dir_all(root).expect("backlog fixture cleanup");
}

/// The failure one lane's history interval names for that lane alone.
fn lane_history_fault(
    answer: Result<wire::StreamFrame, serve::stream::Failure>,
    lane: wire::CanonicalUuid,
) -> String {
    let frame = answer.expect("a lane's source failure answers that lane");
    let wire::StreamFrame::MessageHistory { generation, .. } = frame else {
        panic!("expected history interval");
    };
    assert_eq!(
        generation.frontiers[0].state,
        wire::TranscriptLaneState::Unavailable,
        "a failed read is neither complete nor truncated history",
    );
    assert!(generation.messages.is_empty());
    let [fault] = generation.faults.as_slice() else {
        panic!("the unavailable lane names its failure");
    };
    assert_eq!(fault.lane, lane);
    fault.reason.clone()
}

#[test]
fn horizontal_acknowledgements_publish_live_and_replay_with_stable_context() {
    let (root, paths) = resident_fixture("message-backlog-acknowledgements");
    let bus = Bus::start(journal::Journal::in_memory().expect("backlog authority opens"))
        .expect("backlog bus starts");
    let epoch = test_uuid(24);
    let lane = test_uuid(25);
    let empty_lane = test_uuid(26);
    let author = test_uuid(27);
    seat_backlog_residency(&bus, epoch, lane, empty_lane, author, &root);
    seed_steering(
        &bus,
        epoch,
        lane,
        test_uuid(28),
        "1kQwPZtKx",
        "First direction.",
    );
    seed_steering(
        &bus,
        epoch,
        lane,
        test_uuid(29),
        "1kQwPZtLx",
        "Second direction.",
    );
    // The live record names the bytes the tape holds, so the one locus a
    // retirement records governs the live card and every replay alike.
    let tape = tape::Tape::open(paths.lane_tape(lane).join("omp")).expect("the backlog tape opens");
    let locus = tape
        .tee(DISTRIBUTED_ACK)
        .expect("distributed acknowledgements are captured");
    let record = mixed_acknowledgement_record(lane, author, locus);
    // Keep the acknowledgement outside the fixed startup scan. The initial
    // server page must hydrate keys discovered by its wider history scan.
    let mut hidden = Vec::new();
    append_hidden_tool_horizon(&mut hidden);
    tape.tee(&hidden)
        .expect("the hidden startup horizon is captured");
    let presented = concat!(
        "> First direction.\n\n",
        "Nope 1kQwPZtMx: One response.\n",
        "> Second direction.\n\n",
        "Unsafe."
    );

    let live = message_source(bus.reader(), epoch);
    let publisher = live.message_publisher();
    let steering =
        message_surface::directive_context::steering_ledger(&bus.reader(), &record.decoded)
            .expect("live steering context reads")
            .expect("canonical horizontal acknowledgement requests context");
    let mut state = MessageState::default();
    message_surface::publish_agent_messages(&publisher, &record, &mut state, Some(&steering))
        .expect("mixed acknowledgements publish live");
    assert_mixed_acknowledgement_message(
        &live,
        epoch,
        test_uuid(30),
        presented,
        wire::MessageAcknowledgement::Declined,
    );
    restore_mixed_acknowledgement_message(
        &bus,
        &paths,
        epoch,
        test_uuid(31),
        presented,
        wire::MessageAcknowledgement::Declined,
    );
    let state = std::sync::Mutex::new(state);

    // Nothing retires before the journal says so: a settle with the key
    // still pending leaves the card as spoken.
    message_surface::settle_pending_presentations(&bus, &publisher, &state)
        .expect("a pending key settles nothing");
    assert_mixed_acknowledgement_message(
        &live,
        epoch,
        test_uuid(32),
        presented,
        wire::MessageAcknowledgement::Declined,
    );

    // steer::acked lands for the acknowledged key: the live card and every
    // replay now read the acknowledged state from the journal disposition.
    retire_steering(&bus, epoch, lane, test_uuid(28), "1kQwPZtKx", locus);
    message_surface::settle_pending_presentations(&bus, &publisher, &state)
        .expect("a retired key re-presents its card");
    assert_mixed_acknowledgement_message(
        &live,
        epoch,
        test_uuid(33),
        presented,
        wire::MessageAcknowledgement::Declined,
    );
    drop(live);
    for generation in 0..2 {
        restore_mixed_acknowledgement_message(
            &bus,
            &paths,
            epoch,
            test_uuid(34 + generation),
            presented,
            wire::MessageAcknowledgement::Declined,
        );
    }

    bus.shutdown().expect("backlog bus stops");
    fs::remove_dir_all(root).expect("backlog fixture cleanup");
}

fn mixed_acknowledgement_record(
    lane: CanonicalUuid,
    author: CanonicalUuid,
    locus: listen::Locus,
) -> lane::capture::CaptureRecord {
    lane::capture::CaptureRecord {
        lane,
        occupant: author,
        stream: vendor::RecordStream::Stdout,
        boundary: sill::RecordBoundary::Complete,
        locus,
        declaration: None,
        decoded: listen::Decoded::Events {
            context: Box::new(listen::RecordContext {
                timestamp: Some("2030-04-12T18:46:09.271Z".to_owned()),
                ..listen::RecordContext::default()
            }),
            events: vec![listen::Event::Prose(listen::Prose {
                role: listen::ProseRole::Assistant,
                text: concat!(
                    "ACK\t1kQwPZtKx nope 1kQwPZtMx: One response.\n",
                    "NAK\t1kQwPZtLx: Unsafe."
                )
                .to_owned(),
                phase: None,
                media: Vec::new(),
                external_media: Vec::new(),
                locus,
            })],
        },
    }
}

fn restore_mixed_acknowledgement_message(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    request: CanonicalUuid,
    expected: &str,
    acknowledgement: wire::MessageAcknowledgement,
) {
    let source = message_source(bus.reader(), epoch);
    let publisher = source.message_publisher();
    let bus_reader = bus.reader();
    let mut reader = message_surface::BacklogReader::indexed();
    let residency = current_residency(bus, epoch).expect("residency reads");
    let mut diagnostic = Vec::new();
    message_surface::restore_agent_messages_indexed(
        &bus_reader,
        paths,
        residency.body(),
        &publisher,
        &mut reader,
        &mut diagnostic,
    )
    .expect("mixed acknowledgements restore");
    assert!(diagnostic.is_empty());
    assert_mixed_acknowledgement_message(&source, epoch, request, expected, acknowledgement);
}

fn operator_session(epoch: CanonicalUuid) -> wire::Accepted {
    wire::Accepted {
        wire: wire::WireVersion::CURRENT,
        actor: Actor::TEST_PRINCIPAL,
        epoch,
        release: "1.2.3".parse().expect("fixture release"),
    }
}

fn assert_mixed_acknowledgement_message(
    source: &serve::stream::Source,
    epoch: CanonicalUuid,
    request: CanonicalUuid,
    expected: &str,
    acknowledgement: wire::MessageAcknowledgement,
) {
    let accepted = operator_session(epoch);
    let request = wire::StreamRequest::Subscribe {
        v: wire::WireVersion::CURRENT,
        id: request,
        epoch,
        subjects: vec![wire::ViewSubject::Messages],
        after: None,
    };
    let (_, frames) = source
        .begin(&accepted, &request)
        .expect("app subscription begins");
    let wire::StreamFrame::Messages { generation, .. } = &frames[0] else {
        panic!("expected message snapshot")
    };
    let messages = generation
        .messages
        .iter()
        .filter(|message| message.activity.is_prose())
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 1, "one prose event remains one card");
    assert_eq!(messages[0].text, expected);
    assert!(!messages[0].text.contains("ACK 1kQwPZtKx"));
    assert!(!messages[0].text.contains("NAK 1kQwPZtLx"));
    assert!(!messages[0].text.contains("[ACK"));
    assert!(!messages[0].text.contains("[NAK"));
    assert_eq!(
        messages[0].acknowledgement,
        Some(acknowledgement),
        "one authenticated disposition carries the mixed-header precedence"
    );
    assert_eq!(messages[0].text.matches("Nope").count(), 1);
    assert_eq!(messages[0].text.matches("1kQwPZtMx").count(), 1);
    assert_eq!(messages[0].text.matches("One response.").count(), 1);
    assert_eq!(messages[0].text.matches("Unsafe.").count(), 1);
}

/// Retire `key` the way prose disposal does: steer::acked names the transcript
/// locus of the acknowledgement, and the head moves to the acked disposition.
fn retire_steering(
    bus: &Bus,
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    directive: CanonicalUuid,
    key: &str,
    locus: listen::Locus,
) {
    let at = wire::Instant::new(jiff::Timestamp::now()).expect("retirement instant");
    let origin = format!("resident:{epoch}")
        .parse::<wire::Origin>()
        .expect("retirement origin");
    let current = bus
        .read(move |read| read.steering_head(directive))
        .expect("steering head reads")
        .expect("the seeded directive has a head");
    let body = wire::body::SteerAcked {
        directive,
        key: key.to_owned(),
        recipient: Actor::Lane(lane),
        evidence: wire::body::SteeringEvidence::Transcript {
            epoch,
            locus: wire::body::TranscriptLocus {
                source: Some(epoch),
                member: locus.member,
                offset: locus.offset,
                length: locus.length,
                event: 0,
                line: 1,
                column: 5,
            },
        },
    };
    let mut write = JournalWrite::new(Actor::Lane(lane), origin, at, Producer::AckScanner);
    let acked = write
        .fact(body.clone(), FactCause::Root)
        .expect("retirement fact builds");
    let next = steer::Ledger::transition_head(current.body(), steer::Steering::Acked(&body), at)
        .expect("retirement head builds");
    write.steering_head(acked, move |_| next);
    assert!(matches!(bus.commit(write), Ok(CommitOutcome::Committed(_))));
}

fn seed_steering(
    bus: &Bus,
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    directive: CanonicalUuid,
    key: &str,
    text: &str,
) {
    let at = wire::Instant::new(jiff::Timestamp::now()).expect("steering instant");
    let origin = format!("resident:{epoch}")
        .parse::<wire::Origin>()
        .expect("steering origin");
    let body = wire::body::SteerSent {
        sender: Actor::TEST_PRINCIPAL,
        room: None,
        directive,
        fanout: None,
        recipients: Vec::new(),
        key: key.to_owned(),
        recipient: Actor::Lane(lane),
        text: text.to_owned(),
        control: None,
        attachments: Vec::new(),
        expires: "2030-04-12T19:44:09.271Z".parse().expect("steering expiry"),
    };
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin.clone(),
        at,
        Producer::CommandDoor,
    );
    let sent = write
        .fact(body.clone(), FactCause::Root)
        .expect("steering fact builds");
    write.steering_head(sent, move |seq| {
        steer::Ledger::sent_head(&body, origin, at, seq).expect("steering head builds")
    });
    assert!(matches!(bus.commit(write), Ok(CommitOutcome::Committed(_))));
}

fn seat_backlog_residency(
    bus: &Bus,
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    empty_lane: CanonicalUuid,
    author: CanonicalUuid,
    worktree: &Path,
) {
    let at = wire::Instant::new(jiff::Timestamp::now()).expect("fixture instant");
    let origin = format!("resident:{epoch}")
        .parse::<wire::Origin>()
        .expect("fixture origin");
    let mut write = JournalWrite::new(Actor::Spice, origin, at, Producer::CommandDoor);
    let seated = write
        .fact(wire::body::UpSeated { epoch }, FactCause::Root)
        .expect("resident fact");
    let worktree = worktree.display().to_string();
    write.residency_head(seated, move |_| wire::ResidencyHead {
        epoch,
        state: wire::ResidentState::Seated,
        topology_revision: 0,
        lanes: [lane, empty_lane]
            .into_iter()
            .map(|lane| wire::LaneResidency {
                lane,
                team: lane,
                name: None,
                worktree: worktree.clone(),
                life: "steer".to_owned(),
                vendor: "omp".parse().expect("fixture vendor"),
                model: None,
                effort: None,
                fast_mode: false,
                occupant: None,
                lineage: vec![author],
                capabilities: Vec::new(),
                session: None,
                disposition: "vacant".to_owned(),
            })
            .collect(),
        subscriptions: Vec::new(),
        renewals: Vec::new(),
    });
    bus.commit(write).expect("residency seeds");
}
