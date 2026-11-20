use super::*;

#[derive(Debug)]
struct FailingLaneHistory {
    failing: wire::CanonicalUuid,
    served: FixtureHistory,
}

impl MessageHistoryProvider for FailingLaneHistory {
    fn history(
        &self,
        lane: wire::CanonicalUuid,
        before: Option<&AgentMessage>,
        limit: usize,
    ) -> Result<Option<MessageHistoryPage>, String> {
        if lane == self.failing {
            return Err("tape member 8 is unreadable".to_owned());
        }
        self.served.history(lane, before, limit)
    }
}

#[test]
fn one_lane_history_failure_is_confined_to_that_lane() {
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
    let healthy = message(0xe5, 100);
    let failing = message(0xe6, 100);
    let older = (1..=2)
        .flat_map(|index| [message(0xe5, index * 10), message(0xe6, index * 10)])
        .collect::<Vec<_>>();
    publisher
        .set_history_provider(Arc::new(FailingLaneHistory {
            failing: failing.lane,
            served: FixtureHistory {
                messages: older.clone(),
                page_limit: 10,
                reads: AtomicUsize::new(0),
            },
        }))
        .expect("history provider installs");
    publisher
        .set_history_open([healthy.lane, failing.lane])
        .expect("lane history opens");
    publisher.publish(healthy).expect("healthy head publishes");
    publisher
        .publish(failing.clone())
        .expect("failing head publishes");
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
    let cursors = history_cursors(initial);
    assert_eq!(cursors.len(), 2);

    let frame = source
        .message_history(
            &accepted(Actor::TEST_PRINCIPAL),
            &StreamRequest::MessageHistory {
                v: WireVersion::CURRENT,
                id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c5"),
                epoch: epoch(),
                frontiers: cursors.clone(),
            },
        )
        .expect("one lane's failed read leaves the interval answerable");
    let StreamFrame::MessageHistory { generation, .. } = frame else {
        panic!("expected older message page")
    };
    assert_eq!(page_offsets(&generation), [10, 20]);
    assert!(
        generation
            .messages
            .iter()
            .all(|message| message.lane != failing.lane)
    );
    let state = |lane| {
        generation
            .frontiers
            .iter()
            .find(|frontier| frontier.lane == lane)
            .map(|frontier| frontier.state)
    };
    assert_eq!(
        state(older[0].lane),
        Some(wire::TranscriptLaneState::Complete)
    );
    assert_eq!(
        state(failing.lane),
        Some(wire::TranscriptLaneState::Unavailable)
    );
    assert_eq!(
        generation.faults,
        [wire::TranscriptLaneFault {
            lane: failing.lane,
            reason: "tape member 8 is unreadable".to_owned(),
        }],
    );

    publisher
        .set_history_provider(Arc::new(FixtureHistory {
            messages: older,
            page_limit: 10,
            reads: AtomicUsize::new(0),
        }))
        .expect("recovered history provider installs");
    let failed_cursor = cursors
        .into_iter()
        .find(|cursor| cursor.lane == failing.lane)
        .expect("the failing lane advertised history");
    let retried = history_page(&source, failed_cursor);
    assert_eq!(
        page_offsets(&retried),
        [10, 20],
        "the same cursor reads the lane again once its source recovers",
    );
    assert!(retried.faults.is_empty());
    drop(source);
    bus.shutdown().expect("bus shuts down");
}
