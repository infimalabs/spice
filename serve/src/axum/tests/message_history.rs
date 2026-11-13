use super::*;
use crate::stream::{MessageHistoryPage, MessageHistoryProvider, MessagePublisher};

fn message_offsets(messages: &[wire::AgentMessage]) -> Vec<u64> {
    messages
        .iter()
        .map(|message| message.locus.offset)
        .collect()
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

#[tokio::test(flavor = "current_thread")]
async fn authenticated_socket_pages_member_history_before_the_initial_window() {
    let app = TestApp::new();
    let bus = bus::Bus::start(journal::Journal::in_memory().expect("authority opens"))
        .expect("bus starts");
    commit_stream(&bus, false);
    let capabilities = operator_capabilities();
    let (server, _control) = Server::bind_test_streaming(
        Binding::new(LOOPBACK_EPHEMERAL, Access::local()),
        generation(&app, capabilities),
        bus.reader(),
        8,
    )
    .await
    .expect("streaming server binds");
    let publisher = server
        .message_publisher()
        .expect("streaming server publishes messages");
    for offset in 0..80 {
        assert!(
            publisher
                .publish(agent_message(0xe6, offset, &format!("message {offset}")))
                .expect("message publishes")
        );
    }
    let address = server.local_addr().expect("address");
    let (shutdown, stopped) = oneshot::channel();
    let server_task = tokio::spawn(server.up_until(async move {
        let _ = stopped.await;
    }));
    let (mut socket, _) = authenticate(address, operator(), epoch()).await;
    send_client(
        &mut socket,
        &wire::StreamRequest::Subscribe {
            v: wire::WireVersion::CURRENT,
            id: identity(0xd0),
            epoch: epoch(),
            subjects: vec![wire::ViewSubject::Messages],
            after: None,
        },
    )
    .await;
    let initial = stream_frame(&mut socket).await;
    let wire::StreamFrame::Messages { generation, .. } = initial else {
        panic!("expected bounded message snapshot")
    };
    let mut cursor = history_cursors(&generation)
        .into_iter()
        .next()
        .expect("lane advertises older history");
    assert_eq!(cursor.lane, identity(0xe5));
    assert_eq!(cursor.before, Some(generation.messages[0].locus));
    assert_eq!(
        message_offsets(&generation.messages),
        (55..80).collect::<Vec<_>>()
    );

    let mut recovered = generation.messages;
    for (request, expected, has_more) in
        [(identity(0xd1), 5..55, true), (identity(0xd2), 0..5, false)]
    {
        send_client(
            &mut socket,
            &wire::StreamRequest::MessageHistory {
                v: wire::WireVersion::CURRENT,
                id: request,
                epoch: epoch(),
                frontiers: vec![cursor],
            },
        )
        .await;
        let page = stream_frame(&mut socket).await;
        let wire::StreamFrame::MessageHistory {
            id,
            requested,
            generation,
            ..
        } = page
        else {
            panic!("expected member history page")
        };
        assert_eq!(id, request);
        assert_eq!(requested, [cursor]);
        assert_eq!(
            message_offsets(&generation.messages),
            expected.collect::<Vec<_>>()
        );
        let next = generation.frontiers[0];
        assert_eq!(next.state == wire::TranscriptLaneState::Truncated, has_more);
        cursor.before = next.before;
        recovered.splice(0..0, generation.messages);
    }
    assert_eq!(
        message_offsets(&recovered),
        (0..80).collect::<Vec<_>>(),
        "history pages reconstruct the retained lane in stable order",
    );

    shutdown.send(()).expect("shutdown signals");
    assert!(matches!(
        stream_frame(&mut socket).await,
        wire::StreamFrame::Closing { .. }
    ));
    assert_socket_terminated(&mut socket).await;
    server_task
        .await
        .expect("server task joins")
        .expect("server stops");
    bus.shutdown().expect("bus shuts down");
}

/// A history provider the test holds shut until it releases it.
///
/// `history` reports every lane that entered and then blocks its thread on the
/// latch. On the surface runtime thread that would freeze every socket, the
/// test included, so a read arriving on the thread that built the provider
/// refuses instead and drops the entry report, which fails the test at once
/// rather than hanging it.
#[derive(Debug)]
struct LatchedHistory {
    runtime: std::thread::ThreadId,
    entered: tokio::sync::mpsc::UnboundedSender<wire::CanonicalUuid>,
    released: std::sync::Mutex<bool>,
    release: std::sync::Condvar,
}

impl LatchedHistory {
    fn new() -> (
        Arc<Self>,
        tokio::sync::mpsc::UnboundedReceiver<wire::CanonicalUuid>,
    ) {
        let (entered, observed) = tokio::sync::mpsc::unbounded_channel();
        let provider = Arc::new(Self {
            runtime: std::thread::current().id(),
            entered,
            released: std::sync::Mutex::new(false),
            release: std::sync::Condvar::new(),
        });
        (provider, observed)
    }

    fn release(&self) {
        *self.released.lock().expect("latch lock") = true;
        self.release.notify_all();
    }
}

impl MessageHistoryProvider for LatchedHistory {
    fn history(
        &self,
        lane: wire::CanonicalUuid,
        _before: Option<&wire::AgentMessage>,
        _limit: usize,
    ) -> Result<Option<MessageHistoryPage>, String> {
        if std::thread::current().id() == self.runtime {
            return Err("history ran on the surface runtime thread".to_owned());
        }
        let _ = self.entered.send(lane);
        let mut released = self.released.lock().expect("latch lock");
        while !*released {
            released = self.release.wait(released).expect("latch condvar");
        }
        let mut message = agent_message(0xe6, 5, "held page");
        message.lane = lane;
        Ok(Some(MessageHistoryPage {
            messages: vec![message],
            more: false,
        }))
    }
}

/// Authenticate one socket and subscribe it to the message view.
///
/// Returns the socket and the history cursors its initial snapshot carried.
async fn subscribe_messages(
    address: SocketAddr,
    id: wire::CanonicalUuid,
) -> (ClientSocket, Vec<wire::MessageHistoryCursor>) {
    let (mut socket, _) = authenticate(address, operator(), epoch()).await;
    send_client(
        &mut socket,
        &wire::StreamRequest::Subscribe {
            v: wire::WireVersion::CURRENT,
            id,
            epoch: epoch(),
            subjects: vec![wire::ViewSubject::Messages],
            after: None,
        },
    )
    .await;
    let wire::StreamFrame::Messages { generation, .. } = stream_frame(&mut socket).await else {
        panic!("expected bounded message snapshot")
    };
    (socket, history_cursors(&generation))
}

/// Ask for the newest history page of the open lane.
async fn request_history(
    socket: &mut ClientSocket,
    id: wire::CanonicalUuid,
    cursor: wire::MessageHistoryCursor,
) {
    send_client(
        socket,
        &wire::StreamRequest::MessageHistory {
            v: wire::WireVersion::CURRENT,
            id,
            epoch: epoch(),
            frontiers: vec![cursor],
        },
    )
    .await;
}

/// A socket is answered in full while another socket waits on its read.
///
/// The watcher is pinged and then handed a card published during the hold.
async fn assert_served_during_hold(
    watcher: &mut ClientSocket,
    watch: wire::CanonicalUuid,
    publisher: &MessagePublisher,
    live: &wire::AgentMessage,
) {
    send_client(
        watcher,
        &wire::StreamRequest::Ping {
            v: wire::WireVersion::CURRENT,
            id: identity(0xd3),
            epoch: epoch(),
        },
    )
    .await;
    assert!(matches!(
        stream_frame(watcher).await,
        wire::StreamFrame::Pong { id, .. } if id == identity(0xd3)
    ));
    assert!(
        publisher
            .publish(live.clone())
            .expect("live message publishes")
    );
    let wire::StreamFrame::MessageGeneration { id, generation, .. } = stream_frame(watcher).await
    else {
        panic!("expected live transcript generation")
    };
    assert_eq!(id, watch);
    assert_eq!(generation.messages.as_slice(), std::slice::from_ref(live));
}

/// The held cursor is refused after the live generation crosses its boundary.
async fn assert_held_page_refused(reader: &mut ClientSocket, live: &wire::AgentMessage) {
    let frames = [stream_frame(reader).await, stream_frame(reader).await];
    assert!(
        frames.iter().any(|frame| matches!(
            frame,
            wire::StreamFrame::Refusal { id, refusal, .. }
                if *id == identity(0xd1)
                    && refusal.reason() == "stream::message-generation-stale"
        )),
        "the held history cursor is refused at the crossed generation",
    );
    assert!(
        frames.iter().any(|frame| matches!(
            frame,
            wire::StreamFrame::MessageGeneration { id, generation, .. }
                if *id == identity(0xd0) && generation.messages == [live.clone()]
        )),
        "the concurrent live generation remains independently deliverable",
    );
}

#[tokio::test(flavor = "current_thread")]
async fn held_history_read_leaves_the_runtime_serving_other_sockets() {
    let app = TestApp::new();
    let bus = bus::Bus::start(journal::Journal::in_memory().expect("authority opens"))
        .expect("bus starts");
    commit_stream(&bus, false);
    let capabilities = operator_capabilities();
    let (server, _control) = Server::bind_test_streaming(
        Binding::new(LOOPBACK_EPHEMERAL, Access::local()),
        generation(&app, capabilities),
        bus.reader(),
        8,
    )
    .await
    .expect("streaming server binds");
    let publisher = server
        .message_publisher()
        .expect("streaming server publishes messages");
    let (provider, mut entered) = LatchedHistory::new();
    publisher
        .set_history_provider(provider.clone())
        .expect("latched provider installs");
    publisher
        .set_history_open([identity(0xe5)])
        .expect("lane opens durable history");
    let address = server.local_addr().expect("address");
    let (shutdown, stopped) = oneshot::channel();
    let server_task = tokio::spawn(server.up_until(async move {
        let _ = stopped.await;
    }));

    let (mut reader, more) = subscribe_messages(address, identity(0xd0)).await;
    let cursor = more
        .first()
        .copied()
        .expect("open lane advertises its bootstrap frontier");
    assert_eq!(cursor.lane, identity(0xe5));
    assert_eq!(cursor.before, None);
    request_history(&mut reader, identity(0xd1), cursor).await;
    timeout(ASSERTION_DEADLINE, entered.recv())
        .await
        .expect("provider is entered before the deadline")
        .expect("provider reports its entry");

    // A second socket is served in full while the first read is still held.
    let (mut watcher, _) = subscribe_messages(address, identity(0xd2)).await;
    let live = agent_message(0xe7, 90, "published while the read is held");
    assert_served_during_hold(&mut watcher, identity(0xd2), &publisher, &live).await;

    // Releasing the latch makes the crossed history generation refuse strictly.
    provider.release();
    assert_held_page_refused(&mut reader, &live).await;

    shutdown.send(()).expect("shutdown signals");
    for socket in [&mut reader, &mut watcher] {
        assert!(matches!(
            stream_frame(socket).await,
            wire::StreamFrame::Closing { .. }
        ));
        assert_socket_terminated(socket).await;
    }
    server_task
        .await
        .expect("server task joins")
        .expect("server stops");
    bus.shutdown().expect("bus shuts down");
}

#[tokio::test(flavor = "current_thread")]
async fn one_socket_starts_lane_history_reads_concurrently() {
    let app = TestApp::new();
    let bus = bus::Bus::start(journal::Journal::in_memory().expect("authority opens"))
        .expect("bus starts");
    commit_stream(&bus, false);
    let capabilities = operator_capabilities();
    let (server, _control) = Server::bind_test_streaming(
        Binding::new(LOOPBACK_EPHEMERAL, Access::local()),
        generation(&app, capabilities),
        bus.reader(),
        8,
    )
    .await
    .expect("streaming server binds");
    let publisher = server
        .message_publisher()
        .expect("streaming server publishes messages");
    let (provider, mut entered) = LatchedHistory::new();
    publisher
        .set_history_provider(provider.clone())
        .expect("latched provider installs");
    publisher
        .set_history_open([identity(0xe5), identity(0xe7)])
        .expect("lanes open durable history");
    let address = server.local_addr().expect("address");
    let (shutdown, stopped) = oneshot::channel();
    let server_task = tokio::spawn(server.up_until(async move {
        let _ = stopped.await;
    }));

    let (mut socket, cursors) = subscribe_messages(address, identity(0xd0)).await;
    assert_eq!(cursors.len(), 2);
    request_history(&mut socket, identity(0xd1), cursors[0]).await;
    request_history(&mut socket, identity(0xd2), cursors[1]).await;

    let first = timeout(ASSERTION_DEADLINE, entered.recv()).await;
    let second = timeout(ASSERTION_DEADLINE, entered.recv()).await;
    if first.is_err() || second.is_err() {
        provider.release();
    }
    let mut entered_lanes = [
        first
            .expect("first provider call enters before the deadline")
            .expect("first provider call reports its lane"),
        second
            .expect("second provider call enters while the first is held")
            .expect("second provider call reports its lane"),
    ];
    entered_lanes.sort();
    assert_eq!(entered_lanes, [identity(0xe5), identity(0xe7)]);

    send_client(
        &mut socket,
        &wire::StreamRequest::Ping {
            v: wire::WireVersion::CURRENT,
            id: identity(0xd3),
            epoch: epoch(),
        },
    )
    .await;
    let pong = timeout(ASSERTION_DEADLINE, stream_frame(&mut socket)).await;
    provider.release();
    assert!(matches!(
        pong.expect("the held reads do not block the same socket"),
        wire::StreamFrame::Pong { id, .. } if id == identity(0xd3)
    ));

    let mut answered = [
        stream_frame(&mut socket).await,
        stream_frame(&mut socket).await,
    ]
    .map(|frame| match frame {
        wire::StreamFrame::MessageHistory { id, .. } => id,
        other => panic!("expected history answer, got {other:?}"),
    });
    answered.sort();
    assert_eq!(answered, [identity(0xd1), identity(0xd2)]);

    shutdown.send(()).expect("shutdown signals");
    assert!(matches!(
        stream_frame(&mut socket).await,
        wire::StreamFrame::Closing { .. }
    ));
    assert_socket_terminated(&mut socket).await;
    server_task
        .await
        .expect("server task joins")
        .expect("server stops");
    bus.shutdown().expect("bus shuts down");
}

#[tokio::test(flavor = "current_thread")]
async fn history_reads_hold_their_slot_until_the_scan_returns() {
    let limits = super::super::history::HistoryLimits::with(1, 2);
    let first = limits.admit().expect("first request is admitted");
    let second = limits.admit().expect("second request is admitted");
    assert!(
        limits.admit().is_none(),
        "demand beyond the surface bound is refused",
    );

    let (entered, mut observed) = tokio::sync::mpsc::unbounded_channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let first_entered = entered.clone();
    let held = tokio::spawn(first.run(move || {
        first_entered.send(1).expect("entry reports");
        released.recv().expect("release arrives");
    }));
    let mut entry = async || {
        tokio::time::timeout(Duration::from_secs(10), observed.recv())
            .await
            .expect("a scan starts")
            .expect("a scan reports its entry")
    };
    assert_eq!(entry().await, 1, "the first scan starts");

    // The closing socket drops its future, but the scan it started runs on
    // and keeps the only slot, so a later read queues behind it.
    held.abort();
    assert!(held.await.expect_err("aborted").is_cancelled());
    assert_eq!(limits.idle_reads(), 0, "the running scan keeps its slot");
    let waiting = tokio::spawn(second.run(move || entered.send(2).expect("entry reports")));

    release.send(()).expect("scan released");
    assert_eq!(entry().await, 2, "the queued scan starts next");
    waiting
        .await
        .expect("queued read joins")
        .expect("queued read completes");
    assert!(limits.admit().is_some(), "answered demand frees its place",);
}
