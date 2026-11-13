use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const HISTORY_LENGTH: u64 = 300;
const QUIET_HISTORY_LENGTH: u64 = 40;
const FIRST_RETAINED_BOUNDARY: u64 = 80;
const SECOND_RETAINED_BOUNDARY: u64 = 220;

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

fn complete_lane_history(
    source: &Source,
    lane: wire::CanonicalUuid,
    initial: &[AgentMessage],
    advertised_more: &[wire::MessageHistoryCursor],
) -> Vec<AgentMessage> {
    let mut messages = initial
        .iter()
        .filter(|message| message.lane == lane)
        .cloned()
        .collect::<Vec<_>>();
    let mut cursor = advertised_more
        .iter()
        .find(|cursor| cursor.lane == lane)
        .copied();
    while let Some(requested) = cursor {
        let request = StreamRequest::MessageHistory {
            v: WireVersion::CURRENT,
            id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c5"),
            epoch: epoch(),
            frontiers: vec![requested],
        };
        let frame = source
            .message_history(&accepted(Actor::TEST_PRINCIPAL), &request)
            .expect("older message page answers");
        let StreamFrame::MessageHistory {
            requested: answered,
            generation,
            ..
        } = frame
        else {
            panic!("expected older message page")
        };
        assert_eq!(answered, [requested]);
        assert!(
            generation
                .messages
                .iter()
                .all(|message| message.lane == lane)
        );
        messages.splice(0..0, generation.messages);
        cursor = generation
            .frontiers
            .iter()
            .find(|frontier| {
                frontier.lane == lane && frontier.state == wire::TranscriptLaneState::Truncated
            })
            .map(|frontier| wire::MessageHistoryCursor {
                generation: generation.generation,
                lane,
                before: frontier.before,
            });
    }
    messages
}

#[test]
fn one_batch_publishes_one_sorted_generation_with_every_lane_frontier() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let publisher = source.message_publisher();
    let first_lane = message(0xe5, 1).lane;
    let second_lane = message(0xe6, 1).lane;
    let unavailable_lane = message(0xe7, 1).lane;
    publisher
        .set_lane_availability(
            [first_lane, second_lane, unavailable_lane],
            [first_lane, second_lane],
        )
        .expect("startup availability records");
    let mut updates = source.subscribe_messages();

    let second = message(0xe6, 2);
    let mut first = message(0xe5, 1);
    first.final_answer = true;
    let batch = publisher.batch().expect("batch begins");
    batch.publish(second.clone()).expect("later message stages");
    batch
        .publish(message(0xe5, 1))
        .expect("earlier message stages");
    batch
        .revise_final(first.clone())
        .expect("terminal revision stages");
    assert!(batch.commit().expect("batch commits"));

    let generation = updates.try_recv().expect("one generation broadcasts");
    assert_eq!(generation.generation, 1);
    assert_eq!(generation.messages, [first, second]);
    assert!(
        generation
            .messages
            .windows(2)
            .all(|pair| pair[0].chronology_key() < pair[1].chronology_key())
    );
    assert_eq!(
        generation
            .frontiers
            .iter()
            .map(|frontier| (frontier.lane, frontier.state))
            .collect::<Vec<_>>(),
        [
            (first_lane, wire::TranscriptLaneState::Complete),
            (second_lane, wire::TranscriptLaneState::Complete),
            (unavailable_lane, wire::TranscriptLaneState::Unavailable),
        ],
    );
    assert_eq!(
        updates.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty),
        "the cohort has no partial publication",
    );

    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[test]
fn terminal_revision_replaces_one_retained_message_and_broadcasts_the_change() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let publisher = source.message_publisher();
    let ordinary = message(0xe5, 7);
    publisher
        .publish(ordinary.clone())
        .expect("ordinary prose publishes");
    let mut updates = source.subscribe_messages();

    let mut terminal = ordinary.clone();
    terminal.final_answer = true;
    terminal.automatic_speech = Some(wire::AutomaticSpeech {
        kind: wire::AutomaticSpeechKind::Final,
        text: terminal.text.clone(),
    });
    assert!(
        publisher
            .revise_final(terminal.clone())
            .expect("terminal revision publishes")
    );
    let generation = updates.try_recv().expect("terminal revision broadcasts");
    assert_eq!(generation.messages, [terminal.clone()]);
    assert!(
        !publisher
            .revise_final(terminal.clone())
            .expect("identical terminal revision deduplicates")
    );

    let (_, frames) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Messages], None),
        )
        .expect("message subscription begins");
    let StreamFrame::Messages { generation, .. } = &frames[0] else {
        panic!("expected retained message snapshot")
    };
    assert_eq!(generation.messages, [terminal.clone()]);

    let mut conflicting = terminal;
    conflicting.text = "different authored prose".to_owned();
    assert_eq!(
        publisher.revise_final(conflicting),
        Err(MessagePublishError::IdentityConflict),
    );
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[test]
fn presentation_revision_moves_only_semantics_and_keeps_the_final_state() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let publisher = source.message_publisher();
    let mut spoken = message(0xe5, 7);
    spoken.text = "> Keep the current path.\n\nApplied.".to_owned();
    spoken.acknowledgement = Some(wire::MessageAcknowledgement::Pending);
    publisher
        .publish(spoken.clone())
        .expect("spoken acknowledgement publishes");
    let mut terminal = spoken.clone();
    terminal.final_answer = true;
    assert!(
        publisher
            .revise_final(terminal.clone())
            .expect("terminal revision publishes")
    );
    let mut updates = source.subscribe_messages();

    let retired = crate::stream::MessagePresentation {
        text: spoken.text.clone(),
        acknowledgement: Some(wire::MessageAcknowledgement::Acknowledged),
        ..crate::stream::MessagePresentation::default()
    };
    assert!(
        publisher
            .revise_presentation(spoken.lane, spoken.locus, retired.clone())
            .expect("retired presentation publishes")
    );
    let mut expected = terminal;
    expected.text.clone_from(&retired.text);
    expected.acknowledgement = retired.acknowledgement;
    let generation = updates.try_recv().expect("retired presentation broadcasts");
    assert_eq!(
        generation.messages,
        [expected.clone()],
        "the final state the card earned survives its re-presentation"
    );
    assert!(
        !publisher
            .revise_presentation(spoken.lane, spoken.locus, retired.clone())
            .expect("identical presentation deduplicates")
    );
    let absent = message(0xe6, 1);
    assert!(
        !publisher
            .revise_presentation(absent.lane, absent.locus, retired)
            .expect("a card retention does not hold is not revised")
    );
    assert!(updates.try_recv().is_err(), "nothing else broadcasts");

    let (_, frames) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Messages], None),
        )
        .expect("message subscription begins");
    let StreamFrame::Messages { generation, .. } = &frames[0] else {
        panic!("expected retained message snapshot")
    };
    assert_eq!(generation.messages, [expected]);
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[test]
fn retained_message_capacity_is_independent_for_each_lane() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let publisher = source.message_publisher();
    for lane in [0xe5, 0xe6] {
        for offset in 0..MESSAGE_CAPACITY {
            publisher
                .publish(message(lane, u64::try_from(offset).expect("offset fits")))
                .expect("lane backlog publishes");
        }
    }
    publisher
        .publish(message(
            0xe5,
            u64::try_from(MESSAGE_CAPACITY).expect("capacity fits"),
        ))
        .expect("new lane activity publishes");

    let (_, frames) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Messages], None),
        )
        .expect("message subscription begins");
    let StreamFrame::Messages {
        limit, generation, ..
    } = &frames[0]
    else {
        panic!("expected retained message snapshot")
    };
    let messages = &generation.messages;
    let more = history_cursors(generation);
    assert_eq!(*limit as usize, MESSAGE_CAPACITY);
    assert_eq!(messages.len(), wire::MESSAGE_FLOOR * 2);
    assert_eq!(more.len(), 2);
    for lane in [0xe5, 0xe6] {
        let lane = message(lane, 0).lane;
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.lane == lane)
                .count(),
            25
        );
        assert_eq!(
            complete_lane_history(&source, lane, messages, &more).len(),
            256
        );
    }
    let busy_lane = message(0xe5, 0).lane;
    assert!(
        messages
            .iter()
            .all(|message| message.lane != busy_lane || message.locus.offset > 0),
        "the busy lane evicts only its own oldest row"
    );
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[test]
fn retained_message_capacity_starts_at_the_oldest_complete_compaction_window() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let publisher = source.message_publisher();
    let busy_lane = message(0xe5, 0).lane;
    let quiet_lane = message(0xe6, 0).lane;
    for offset in 0..HISTORY_LENGTH {
        let mut activity = message(0xe5, offset);
        if matches!(
            offset,
            20 | FIRST_RETAINED_BOUNDARY | SECOND_RETAINED_BOUNDARY
        ) {
            activity.activity = wire::AgentActivityKind::Compaction;
        }
        publisher.publish(activity).expect("busy history publishes");
        if offset < QUIET_HISTORY_LENGTH {
            publisher
                .publish(message(0xe6, offset))
                .expect("interleaved quiet history publishes");
        }
    }

    let (_, frames) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Messages], None),
        )
        .expect("message subscription begins");
    let StreamFrame::Messages {
        limit, generation, ..
    } = &frames[0]
    else {
        panic!("expected retained message snapshot")
    };
    let messages = &generation.messages;
    let more = history_cursors(generation);
    assert_eq!(*limit as usize, MESSAGE_CAPACITY);
    let busy = complete_lane_history(&source, busy_lane, messages, &more);
    assert_eq!(busy.len(), 220);
    assert_eq!(busy[0].locus.offset, FIRST_RETAINED_BOUNDARY);
    assert_eq!(busy[0].activity, wire::AgentActivityKind::Compaction);
    assert_eq!(busy[140].locus.offset, SECOND_RETAINED_BOUNDARY);
    assert_eq!(busy[140].activity, wire::AgentActivityKind::Compaction);
    assert_eq!(
        busy.last().expect("busy tail").locus.offset,
        HISTORY_LENGTH - 1
    );
    assert_eq!(
        complete_lane_history(&source, quiet_lane, messages, &more)
            .iter()
            .map(|message| message.locus.offset)
            .collect::<Vec<_>>(),
        (0..QUIET_HISTORY_LENGTH).collect::<Vec<_>>(),
        "one lane's compaction floor does not disturb interleaved lanes",
    );
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[test]
fn late_session_boundaries_are_inserted_before_retention_selects_its_floor() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let publisher = source.message_publisher();
    for offset in 0..HISTORY_LENGTH {
        if matches!(offset, FIRST_RETAINED_BOUNDARY | SECOND_RETAINED_BOUNDARY) {
            continue;
        }
        publisher
            .publish(message(0xe5, offset))
            .expect("stdout history publishes");
    }
    for offset in [FIRST_RETAINED_BOUNDARY, SECOND_RETAINED_BOUNDARY] {
        let mut boundary = message(0xe5, offset);
        boundary.activity = wire::AgentActivityKind::Compaction;
        publisher
            .publish(boundary)
            .expect("preserved-session boundary publishes late");
    }

    let (_, frames) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Messages], None),
        )
        .expect("message subscription begins");
    let StreamFrame::Messages { generation, .. } = &frames[0] else {
        panic!("expected retained message snapshot")
    };
    let more = history_cursors(generation);
    let retained =
        complete_lane_history(&source, message(0xe5, 0).lane, &generation.messages, &more);
    assert_eq!(retained[0].locus.offset, FIRST_RETAINED_BOUNDARY);
    assert_eq!(retained[0].activity, wire::AgentActivityKind::Compaction);
    assert_eq!(retained[140].locus.offset, SECOND_RETAINED_BOUNDARY);
    assert_eq!(retained[140].activity, wire::AgentActivityKind::Compaction);
    assert_eq!(
        retained.last().expect("retained tail").locus.offset,
        HISTORY_LENGTH - 1,
    );
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[derive(Debug)]
struct FixtureHistory {
    messages: Vec<AgentMessage>,
    page_limit: usize,
    reads: AtomicUsize,
}

impl MessageHistoryProvider for FixtureHistory {
    fn history(
        &self,
        lane: wire::CanonicalUuid,
        before: Option<&AgentMessage>,
        limit: usize,
    ) -> Result<Option<MessageHistoryPage>, String> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let visible = self
            .messages
            .iter()
            .filter(|message| {
                message.lane == lane
                    && before
                        .is_none_or(|before| message.chronology_key() < before.chronology_key())
            })
            .cloned()
            .collect::<Vec<_>>();
        let start = visible.len().saturating_sub(limit.min(self.page_limit));
        Ok(Some(MessageHistoryPage {
            messages: visible[start..].to_vec(),
            more: start > 0,
        }))
    }
}

#[derive(Debug)]
struct RebasedHistory {
    before: wire::MessageLocus,
    messages: Vec<AgentMessage>,
}

impl MessageHistoryProvider for RebasedHistory {
    fn history(
        &self,
        lane: wire::CanonicalUuid,
        before: Option<&AgentMessage>,
        _limit: usize,
    ) -> Result<Option<MessageHistoryPage>, String> {
        if before.map(|message| message.locus) != Some(self.before) {
            return Ok(None);
        }
        Ok(Some(MessageHistoryPage {
            messages: self
                .messages
                .iter()
                .filter(|message| message.lane == lane)
                .cloned()
                .collect(),
            more: false,
        }))
    }
}

fn history_page(source: &Source, cursor: wire::MessageHistoryCursor) -> wire::TranscriptGeneration {
    let frame = source
        .message_history(
            &accepted(Actor::TEST_PRINCIPAL),
            &StreamRequest::MessageHistory {
                v: WireVersion::CURRENT,
                id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c5"),
                epoch: epoch(),
                frontiers: vec![cursor],
            },
        )
        .expect("older message page answers");
    let StreamFrame::MessageHistory { generation, .. } = frame else {
        panic!("expected older message page")
    };
    generation
}

fn page_offsets(generation: &wire::TranscriptGeneration) -> Vec<u64> {
    generation
        .messages
        .iter()
        .map(|message| message.locus.offset)
        .collect()
}

#[test]
fn a_located_history_cursor_keeps_paging_across_live_publication() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let publisher = source.message_publisher();
    let head = message(0xe5, 100);
    publisher
        .set_history_provider(Arc::new(FixtureHistory {
            messages: (1..=6).map(|index| message(0xe5, index * 10)).collect(),
            page_limit: 2,
            reads: AtomicUsize::new(0),
        }))
        .expect("history provider installs");
    publisher
        .set_history_open([head.lane])
        .expect("lane history opens");
    publisher
        .publish(head.clone())
        .expect("retained head publishes");
    let (_, frames) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Messages], None),
        )
        .expect("message subscription begins");
    let StreamFrame::Messages {
        generation: initial,
        ..
    } = &frames[0]
    else {
        panic!("expected message bootstrap")
    };

    let first = history_page(&source, history_cursors(initial)[0]);
    assert_eq!(page_offsets(&first), [50, 60]);
    let deep = history_cursors(&first)[0];
    assert!(
        publisher
            .publish(message(0xe5, 110))
            .expect("live work publishes")
    );

    let second = history_page(&source, deep);
    assert!(second.generation > deep.generation);
    assert_eq!(page_offsets(&second), [30, 40]);

    let stale = history_cursors(&second)[0];
    publisher
        .set_history_provider(Arc::new(RebasedHistory {
            before: head.locus,
            messages: [message(0xe5, 70), message(0xe5, 80)].to_vec(),
        }))
        .expect("rebased history provider installs");
    let resumed = history_page(&source, stale);
    assert!(resumed.messages.is_empty());
    assert_eq!(
        resumed.frontiers[0].before,
        Some(head.locus),
        "a cached anchor rejected by the current source resumes at the retained edge",
    );
    let rebased = history_page(&source, history_cursors(&resumed)[0]);
    assert_eq!(page_offsets(&rebased), [70, 80]);
    assert!(
        history_cursors(&rebased).is_empty(),
        "the rebased source reaches its terminal boundary",
    );

    let unissued = wire::MessageHistoryCursor {
        before: Some(message(0xe5, 45).locus),
        ..deep
    };
    let resumed = history_page(&source, unissued);
    assert!(resumed.messages.is_empty());
    assert_eq!(
        resumed.frontiers[0].before,
        Some(head.locus),
        "an anchor the resident does not hold resumes at the retained edge",
    );
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

/// An operator direction quoted without recipients or a packet.
fn operator_attribution() -> wire::QuoteAttribution {
    wire::QuoteAttribution {
        author: wire::QuoteAuthor::Directive {
            actor: wire::Actor::TEST_PRINCIPAL,
        },
        recipients: Vec::new(),
        packet: false,
    }
}

fn assert_quote_disposition(quotes: &[wire::QuotePresentation], expected: wire::Disposition) {
    assert!(matches!(
        quotes,
        [wire::QuotePresentation::Resolved {
            disposition: Some(actual),
            ..
        }] if *actual == expected
    ));
}

#[test]
fn quote_presentations_are_complete_on_bootstrap_and_history_and_refresh_from_authority() {
    let bus = fixture(1);
    let disposition = Arc::new(Mutex::new(wire::Disposition::Live));
    let quoted_text = "quoted direction";
    let target = wire::QuoteTarget::Steering {
        key: "1kQnrzNZx".to_owned(),
        digest: wire::QuoteTarget::text_digest(quoted_text),
    };
    let target_spelling = target.to_string();
    let quote_presenter = QuotePresenter::new({
        let disposition = disposition.clone();
        let target_spelling = target_spelling.clone();
        move |_| {
            vec![wire::QuotePresentation::Resolved {
                target: target_spelling.clone(),
                text: quoted_text.to_owned(),
                attribution: operator_attribution(),
                disposition: Some(*disposition.lock().expect("disposition lock")),
                quotes: Vec::new(),
                media_quotes: Vec::new(),
            }]
        }
    });
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        quote_presenter,
        operator_book(),
        config_revision(),
    );
    let publisher = source.message_publisher();
    let mut older = message(0xe5, 5);
    older.text = target.markdown();
    let mut retained = message(0xe5, 10);
    retained.text = target.markdown();
    let lane = retained.lane;
    publisher
        .set_history_provider(Arc::new(FixtureHistory {
            messages: vec![older],
            page_limit: usize::MAX,
            reads: AtomicUsize::new(0),
        }))
        .expect("history provider installs");
    publisher
        .set_history_open([lane])
        .expect("lane history opens");
    publisher
        .publish(retained)
        .expect("retained card publishes");

    let (_, frames) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Messages], None),
        )
        .expect("message subscription begins");
    let StreamFrame::Messages {
        generation: initial,
        ..
    } = &frames[0]
    else {
        panic!("expected message bootstrap")
    };
    assert_quote_disposition(&initial.messages[0].quotes, wire::Disposition::Live);

    *disposition.lock().expect("disposition lock") = wire::Disposition::Acked;
    let history = source
        .message_history(
            &accepted(Actor::TEST_PRINCIPAL),
            &StreamRequest::MessageHistory {
                v: WireVersion::CURRENT,
                id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c5"),
                epoch: epoch(),
                frontiers: history_cursors(initial),
            },
        )
        .expect("message history answers");
    let StreamFrame::MessageHistory {
        generation: history,
        ..
    } = history
    else {
        panic!("expected message history")
    };
    assert_quote_disposition(&history.messages[0].quotes, wire::Disposition::Acked);

    let (_, refreshed) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc6, vec![ViewSubject::Messages], None),
        )
        .expect("message refresh begins");
    let StreamFrame::Messages {
        generation: refreshed,
        ..
    } = &refreshed[0]
    else {
        panic!("expected refreshed message snapshot")
    };
    assert_quote_disposition(&refreshed.messages[0].quotes, wire::Disposition::Acked);
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[test]
fn coordinated_history_returns_one_bounded_globally_sorted_lane_interval() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let publisher = source.message_publisher();
    let lanes = [message(0xe5, 0).lane, message(0xe6, 0).lane];
    let history = Arc::new(FixtureHistory {
        messages: [0xe5, 0xe6]
            .into_iter()
            .flat_map(|lane| (0..30).map(move |offset| message(lane, offset)))
            .collect(),
        page_limit: usize::MAX,
        reads: AtomicUsize::new(0),
    });
    publisher
        .set_history_provider(history.clone())
        .expect("history provider installs");
    publisher
        .set_history_open(lanes)
        .expect("both lane histories open");
    let (_, frames) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Messages], None),
        )
        .expect("message subscription begins");
    let StreamFrame::Messages {
        generation: initial,
        ..
    } = &frames[0]
    else {
        panic!("expected retained message snapshot")
    };
    let requested = history_cursors(initial);
    assert_eq!(requested.len(), 2);

    let frame = source
        .message_history(
            &accepted(Actor::TEST_PRINCIPAL),
            &StreamRequest::MessageHistory {
                v: WireVersion::CURRENT,
                id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c5"),
                epoch: epoch(),
                frontiers: requested.clone(),
            },
        )
        .expect("coordinated history answers");
    let StreamFrame::MessageHistory {
        requested: answered,
        generation,
        ..
    } = frame
    else {
        panic!("expected coordinated history interval")
    };
    assert_eq!(answered, requested);
    assert_eq!(generation.messages.len(), MESSAGE_HISTORY_PAGE_LIMIT);
    assert!(
        generation
            .messages
            .windows(2)
            .all(|pair| pair[0].chronology_key() < pair[1].chronology_key())
    );
    for lane in lanes {
        assert_eq!(
            generation
                .messages
                .iter()
                .filter(|message| message.lane == lane)
                .count(),
            MESSAGE_HISTORY_PAGE_LIMIT / lanes.len(),
        );
    }
    assert!(generation.frontiers.iter().all(|frontier| {
        frontier.state == wire::TranscriptLaneState::Truncated && frontier.before.is_some()
    }));
    assert_eq!(history.reads.load(Ordering::Relaxed), lanes.len());

    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[test]
fn cold_readers_page_a_full_cache_without_reading_the_provider() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let publisher = source.message_publisher();
    let history = Arc::new(FixtureHistory {
        messages: Vec::new(),
        page_limit: usize::MAX,
        reads: AtomicUsize::new(0),
    });
    publisher.set_history_provider(history.clone()).unwrap();
    for lane in [0xe5, 0xe6] {
        for offset in 0..MESSAGE_CAPACITY as u64 {
            publisher.publish(message(lane, offset)).unwrap();
        }
    }
    for reader in [0xc4, 0xc5] {
        let (_, frames) = source
            .begin(
                &accepted(Actor::TEST_PRINCIPAL),
                &request(reader, vec![ViewSubject::Messages], None),
            )
            .unwrap();
        let StreamFrame::Messages { generation, .. } = &frames[0] else {
            panic!("expected initial messages");
        };
        let more = history_cursors(generation);
        for lane in [0xe5, 0xe6] {
            let restored =
                complete_lane_history(&source, message(lane, 0).lane, &generation.messages, &more);
            assert_eq!(
                restored
                    .iter()
                    .map(|message| message.locus.offset)
                    .collect::<Vec<_>>(),
                (0..MESSAGE_CAPACITY as u64).collect::<Vec<_>>(),
            );
        }
    }
    assert_eq!(history.reads.load(Ordering::Relaxed), 0);
    drop(source);
    bus.shutdown().unwrap();
}

#[test]
fn cold_history_keeps_one_page_and_loads_older_pages_without_live_updates() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let publisher = source.message_publisher();
    let lane = message(0xe5, 0).lane;
    let startup = (0..80)
        .map(|offset| message(0xe5, offset))
        .collect::<Vec<_>>();
    for activity in &startup {
        publisher
            .publish(activity.clone())
            .expect("startup history publishes");
    }
    let mut durable = startup.clone();
    durable[54].text = "stale source projection".to_owned();
    let history = Arc::new(FixtureHistory {
        messages: durable,
        page_limit: usize::MAX,
        reads: AtomicUsize::new(0),
    });
    publisher
        .set_history_provider(history.clone())
        .expect("durable history provider installs");
    publisher
        .set_history_open([lane])
        .expect("durable history is advertised");
    publisher
        .bound_initial_history()
        .expect("startup history is bounded");
    let mut updates = source.subscribe_messages();

    let (_, frames) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Messages], None),
        )
        .expect("message subscription begins");
    let StreamFrame::Messages { generation, .. } = &frames[0] else {
        panic!("expected retained message snapshot")
    };
    let messages = &generation.messages;
    let more = history_cursors(generation);
    assert_eq!(messages.len(), wire::MESSAGE_FLOOR);
    assert_eq!(messages[0].locus.offset, 55);
    assert_eq!(
        more,
        &[wire::MessageHistoryCursor {
            generation: generation.generation,
            lane,
            before: Some(messages[0].locus),
        }]
    );

    let restored = complete_lane_history(&source, lane, messages, &more);
    assert_eq!(
        restored
            .iter()
            .map(|message| message.locus.offset)
            .collect::<Vec<_>>(),
        (0..80).collect::<Vec<_>>(),
    );
    assert_eq!(history.reads.load(Ordering::Relaxed), 2);
    assert_eq!(restored[54].text, startup[54].text);
    assert!(matches!(
        updates.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    let (_, frames) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc5, vec![ViewSubject::Messages], None),
        )
        .expect("new reader subscribes after source exhaustion");
    let StreamFrame::Messages { generation, .. } = &frames[0] else {
        panic!("expected new reader snapshot");
    };
    let more = history_cursors(generation);
    assert_eq!(
        complete_lane_history(&source, lane, &generation.messages, &more),
        restored,
        "each reader traverses the same retained history, including current presentation",
    );
    assert_eq!(
        history.reads.load(Ordering::Relaxed),
        4,
        "each generation-bound reader walks its own source interval",
    );
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[test]
fn short_provider_pages_continue_until_source_complete_with_bounded_reads() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let publisher = source.message_publisher();
    let lane = message(0xe5, 0).lane;
    let durable = (0..HISTORY_LENGTH)
        .map(|offset| message(0xe5, offset))
        .collect::<Vec<_>>();
    for activity in &durable {
        publisher
            .publish(activity.clone())
            .expect("startup history publishes");
    }
    let history = Arc::new(FixtureHistory {
        messages: durable,
        page_limit: 17,
        reads: AtomicUsize::new(0),
    });
    publisher
        .set_history_provider(history.clone())
        .expect("durable history provider installs");
    publisher
        .set_history_open([lane])
        .expect("durable history is advertised");
    publisher
        .bound_initial_history()
        .expect("startup history is bounded");

    let (_, frames) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Messages], None),
        )
        .expect("message subscription begins");
    let StreamFrame::Messages { generation, .. } = &frames[0] else {
        panic!("expected retained message snapshot")
    };
    let more = history_cursors(generation);
    let restored = complete_lane_history(&source, lane, &generation.messages, &more);
    assert_eq!(
        restored
            .iter()
            .map(|message| message.locus.offset)
            .collect::<Vec<_>>(),
        (0..HISTORY_LENGTH).collect::<Vec<_>>(),
    );
    assert_eq!(
        history.reads.load(Ordering::Relaxed),
        17,
        "each bounded short read advances one generation-scoped source cursor",
    );

    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[test]
fn explicit_history_hint_bootstraps_a_lane_without_an_initial_card() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let publisher = source.message_publisher();
    let lane = message(0xe5, 0).lane;
    let durable = (0..20)
        .map(|offset| message(0xe5, offset))
        .collect::<Vec<_>>();
    let history = Arc::new(FixtureHistory {
        messages: durable,
        page_limit: usize::MAX,
        reads: AtomicUsize::new(0),
    });
    publisher
        .set_history_provider(history.clone())
        .expect("durable history provider installs");
    publisher
        .set_history_open([lane])
        .expect("partial startup continuation is exposed");

    let (_, frames) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Messages], None),
        )
        .expect("message subscription begins");
    let StreamFrame::Messages { generation, .. } = &frames[0] else {
        panic!("expected retained message snapshot")
    };
    let messages = &generation.messages;
    let more = history_cursors(generation);
    assert!(messages.is_empty());
    assert_eq!(
        more,
        [wire::MessageHistoryCursor {
            generation: generation.generation,
            lane,
            before: None,
        }]
    );

    let restored = complete_lane_history(&source, lane, messages, &more);
    assert_eq!(
        restored
            .iter()
            .map(|message| message.locus.offset)
            .collect::<Vec<_>>(),
        (0..20).collect::<Vec<_>>(),
    );
    assert_eq!(history.reads.load(Ordering::Relaxed), 1);

    drop(source);
    bus.shutdown().expect("bus shuts down");
}

/// A compaction inside the newest window never costs the lane its floor.
///
/// Trimming below a compaction drops the previous context epoch, which is
/// what an operator wants once newer messages fill the view. Here they do not:
/// the epoch that just ended is all this agent has said.
#[test]
fn cold_history_keeps_its_floor_across_a_compaction_inside_the_initial_window() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let publisher = source.message_publisher();
    for offset in 0..40 {
        let mut activity = message(0xe6, offset);
        if matches!(offset, 30) {
            activity.activity = wire::AgentActivityKind::Compaction;
        }
        publisher
            .publish(activity)
            .expect("startup history publishes");
    }
    publisher
        .bound_initial_history()
        .expect("startup history is bounded");

    let (_, frames) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc5, vec![ViewSubject::Messages], None),
        )
        .expect("message subscription begins");
    let StreamFrame::Messages { generation, .. } = &frames[0] else {
        panic!("expected retained message snapshot")
    };
    let messages = &generation.messages;
    let more = history_cursors(generation);
    assert_eq!(
        messages
            .iter()
            .map(|message| message.locus.offset)
            .collect::<Vec<_>>(),
        (14..40).collect::<Vec<_>>(),
        "the newest 25 messages stay, and the compaction stands among them",
    );
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.activity.is_prose())
            .count(),
        wire::MESSAGE_FLOOR,
    );
    assert!(more.is_empty());

    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[test]
fn retained_tool_status_does_not_consume_visible_lane_capacity() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let publisher = source.message_publisher();
    for offset in 0..MESSAGE_CAPACITY {
        publisher
            .publish(message(0xe5, u64::try_from(offset).expect("offset fits")))
            .expect("visible backlog publishes");
    }
    for (offset, activity) in [
        (10_000, wire::AgentActivityKind::Tool),
        (10_001, wire::AgentActivityKind::Tool),
        (10_002, wire::AgentActivityKind::Reasoning),
    ] {
        let mut status = message(0xe5, offset);
        status.activity = activity;
        publisher
            .publish(status)
            .expect("presence status publishes");
    }

    let (_, frames) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Messages], None),
        )
        .expect("message subscription begins");
    let StreamFrame::Messages { generation, .. } = &frames[0] else {
        panic!("expected retained message snapshot")
    };
    let messages = &generation.messages;
    let more = history_cursors(generation);
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.activity.is_prose())
            .count(),
        wire::MESSAGE_FLOOR,
    );
    assert_eq!(more.len(), 1);
    assert_eq!(
        messages
            .iter()
            .filter(|message| !message.activity.is_prose())
            .count(),
        1,
        "only the latest non-card status is retained",
    );
    let latest = messages.last().expect("latest status");
    assert_eq!(latest.locus.offset, 10_002);
    assert_eq!(latest.activity, wire::AgentActivityKind::Reasoning);
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

/// Lifecycle markers ride beside messages; they never spend the floor.
///
/// Turn and compaction cards are boundaries between messages, not messages.
/// An agent whose recent transcript is dense with them still owes the operator
/// its newest messages, so the markers are carried in addition to the floor.
#[test]
fn interleaved_markers_do_not_spend_an_agents_message_floor() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let publisher = source.message_publisher();
    for offset in 0..60 {
        let mut card = message(0xe5, offset);
        if offset % 3 == 2 {
            card.activity = wire::AgentActivityKind::Turn;
        }
        publisher.publish(card).expect("lane backlog publishes");
    }
    publisher
        .bound_initial_history()
        .expect("startup bounds the retained page");

    let (_, frames) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Messages], None),
        )
        .expect("message subscription begins");
    let StreamFrame::Messages { generation, .. } = &frames[0] else {
        panic!("expected retained message snapshot")
    };
    let messages = &generation.messages;
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.activity.is_prose())
            .count(),
        wire::MESSAGE_FLOOR,
    );
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

/// A compaction never leaves an agent showing less than its message floor.
///
/// Trimming below a compaction drops the previous context epoch, which is
/// right while messages newer than the compaction still fill the floor. A
/// compaction that just happened has nothing newer, and the operator still
/// needs the agent's newest messages.
#[test]
fn a_fresh_compaction_does_not_evict_an_agents_message_floor() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let publisher = source.message_publisher();
    for offset in 0..40 {
        publisher
            .publish(message(0xe5, offset))
            .expect("lane backlog publishes");
    }
    let mut compaction = message(0xe5, 40);
    compaction.activity = wire::AgentActivityKind::Compaction;
    publisher
        .publish(compaction)
        .expect("the compaction publishes");
    publisher
        .bound_initial_history()
        .expect("startup bounds the retained page");

    let (_, frames) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Messages], None),
        )
        .expect("message subscription begins");
    let StreamFrame::Messages { generation, .. } = &frames[0] else {
        panic!("expected retained message snapshot")
    };
    let messages = &generation.messages;
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.activity.is_prose())
            .count(),
        wire::MESSAGE_FLOOR,
    );
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[test]
fn prose_reconciliation_keeps_original_blocks_and_scopes_reused_provider_ids() {
    let prose = |offset, block, streamed, text: &str| {
        let mut card = message(0xe5, offset);
        card.text = text.to_owned();
        card.prose_origin = Some(wire::MessageProseOrigin {
            source: uuid("018f82d4-6f59-7d06-92e3-bce9131163c1"),
            message: "provider-7".to_owned(),
            timestamp: 200,
            block,
            streamed,
        });
        card
    };
    let first = prose(1, 1, true, "1kQwPZtKx");
    let second = prose(2, 2, true, "1kQwPZtLx");
    let settled = prose(4, 1, false, "1kQwPZtKx");
    let mut later_message = prose(5, 1, false, "1kQwPZtKx");
    later_message
        .prose_origin
        .as_mut()
        .expect("origin")
        .timestamp += 1;
    let mut other_source = prose(6, 1, false, "1kQwPZtKx");
    other_source.prose_origin.as_mut().expect("origin").source =
        uuid("018f82d4-6f59-7d06-92e3-bce9131163c2");
    let other_block = prose(7, 3, false, "1kQwPZtKx");
    let mut other_author = prose(8, 1, false, "1kQwPZtKx");
    other_author.author = uuid("018f82d4-6f59-7d06-92e3-bce9131163ee");
    let mut other_lane = prose(9, 1, false, "1kQwPZtKx");
    other_lane.lane = uuid("018f82d4-6f59-7d06-92e3-bce9131163e6");
    let expected = vec![
        first.clone(),
        second.clone(),
        later_message.clone(),
        other_source.clone(),
        other_block.clone(),
        other_author.clone(),
        other_lane.clone(),
    ];
    for streamed_first in [false, true] {
        let bus = fixture(1);
        let source = Source::new(
            bus.reader(),
            epoch(),
            20,
            presenter(),
            operator_book(),
            config_revision(),
        );
        let publisher = source.message_publisher();
        let cards = if streamed_first {
            vec![first.clone(), second.clone(), settled.clone()]
        } else {
            vec![settled.clone(), first.clone(), second.clone()]
        };
        for card in cards.into_iter().chain([
            later_message.clone(),
            other_source.clone(),
            other_block.clone(),
            other_author.clone(),
            other_lane.clone(),
        ]) {
            publisher.publish(card).expect("authored card publishes");
        }
        let (_, frames) = source
            .begin(
                &accepted(Actor::TEST_PRINCIPAL),
                &request(0xc4, vec![ViewSubject::Messages], None),
            )
            .expect("reconnected snapshot begins");
        let StreamFrame::Messages { generation, .. } = &frames[0] else {
            panic!("expected retained messages")
        };
        assert_eq!(generation.messages, expected);
        drop(source);
        bus.shutdown().expect("bus stops");
    }
}
