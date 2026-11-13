use super::*;

/// An inbound transport that always has another message waiting.
///
/// The session only ever asks whether a transport message is available, so a
/// source that answers immediately is a faithful stand-in for a peer that keeps
/// the socket busy, and it is ready on every poll rather than on a race.
struct ReadyInbound;

impl super::super::socket::Inbound for ReadyInbound {
    async fn next_message(
        &mut self,
    ) -> Option<Result<::axum::extract::ws::Message, ::axum::Error>> {
        Some(Ok(::axum::extract::ws::Message::Close(None)))
    }
}

/// The bus and each external source a session serves, with their receivers.
type ReadySources = (
    bus::Bus,
    tokio::sync::broadcast::Sender<wire::TranscriptGeneration>,
    tokio::sync::broadcast::Sender<Arc<wire::QuotaGeneration>>,
    Option<bus::BroadcastSubscriber>,
    Option<tokio::sync::broadcast::Receiver<wire::TranscriptGeneration>>,
    Option<tokio::sync::broadcast::Receiver<Arc<wire::QuotaGeneration>>>,
);

/// Every external source a session can serve, each one ready before any is read.
fn every_source_ready() -> ReadySources {
    let bus = bus::Bus::start(journal::Journal::in_memory().expect("authority opens"))
        .expect("bus starts");
    let broadcasts = bus.reader().subscribe();
    for _ in 0..8 {
        commit_stream(&bus, false);
    }
    let (publisher, transcript) = tokio::sync::broadcast::channel(16);
    for offset in 0..8 {
        publisher
            .send(wire::TranscriptGeneration {
                generation: offset + 1,
                horizon: seq(1),
                messages: vec![agent_message(0xe9, offset, "ready message")],
                frontiers: Vec::new(),
            })
            .expect("the retained receiver holds the channel open");
    }
    let (quota_publisher, quota) = tokio::sync::broadcast::channel(16);
    for generation in 1..=8 {
        quota_publisher
            .send(Arc::new(wire::QuotaGeneration {
                generation,
                ..wire::QuotaGeneration::default()
            }))
            .expect("the retained quota receiver holds the channel open");
    }
    (
        bus,
        publisher,
        quota_publisher,
        Some(broadcasts),
        Some(transcript),
        Some(quota),
    )
}

/// Each rotation step serves its own source while all seven stay ready.
///
/// Every source is ready before the first call and none is exhausted by the
/// eight, so the lead is the only thing left that can decide which event comes
/// back. One fixed order answers `Journal` every time; a rotation that fails to
/// cycle answers `Journal` after the first step; and the seventh call proves
/// the cycle closes rather than parking on its last source. One lead is carried
/// across all eight calls, so the advance a session depends on is under test.
///
/// Readiness is established by the assertions themselves rather than by timing.
/// Were the journal or the transcript source empty, the very step that must
/// yield it would return a later source instead and fail there, so this cannot
/// decay into a test that passes because nothing was ever ready.
#[tokio::test(flavor = "current_thread")]
async fn each_rotation_step_serves_its_own_source_while_all_seven_stay_ready() {
    use super::super::socket::{
        CommandCompletion, Event, Lead, PendingCommands, SessionReadiness, SessionSignals,
        WorkSources, next_event,
    };

    let (bus, _publisher, _quota_publisher, mut broadcasts, mut messages, mut quota) =
        every_source_ready();
    let (_closed, mut closed) = tokio::sync::watch::channel(false);
    let mut inbound = ReadyInbound;
    let mut lead = Lead::Journal;
    let retired = CancellationToken::new();
    let mut commands = PendingCommands::new();
    for _ in 0..8 {
        commands.push(Box::pin(async { CommandCompletion::Closed }));
    }
    let mut histories = ready_histories();

    for expected in [
        "journal",
        "message",
        "quota",
        "inbound",
        "command",
        "history",
        "continuation",
        "journal",
    ] {
        let event = next_event(
            &mut lead,
            &mut inbound,
            SessionSignals::new(&mut closed, &retired),
            WorkSources {
                broadcasts: &mut broadcasts,
                messages: &mut messages,
                quota: &mut quota,
                commands: &mut commands,
                histories: &mut histories,
            },
            SessionReadiness::live(true),
        )
        .await;
        let served = match event {
            Event::Closed(_) => "closed",
            Event::Retired => "retired",
            Event::Journal(notice) => {
                notice.expect("the journal source stayed ready");
                "journal"
            }
            Event::Message(notice) => {
                notice.expect("the transcript source stayed ready");
                "message"
            }
            Event::Quota(notice) => {
                notice.expect("the quota source stayed ready");
                "quota"
            }
            Event::Inbound(_) => "inbound",
            Event::Command(completed) => {
                assert!(matches!(completed, Some(CommandCompletion::Closed)));
                "command"
            }
            Event::History(completed) => {
                assert!(matches!(
                    completed,
                    Some((_, Ok(wire::StreamFrame::Pong { .. })))
                ));
                "history"
            }
            Event::Continuation => "continuation",
        };
        assert_eq!(
            served, expected,
            "this rotation step must serve its own source while the others are ready"
        );
    }

    bus.shutdown().expect("bus shuts down");
}

/// Global close outranks retirement and every work source.
///
/// Fairness is bought by rotating the work sources, and the risk of that is
/// trading a starvation bug for a close-ordering one. Shutdown is re-signalled
/// before each step because a watch marks its value seen once observed, so each
/// step is a fresh race between a ready close, capability retirement, and seven
/// ready work sources.
#[tokio::test(flavor = "current_thread")]
async fn close_outranks_every_ready_work_source_on_every_lead() {
    use super::super::socket::{
        CommandCompletion, Event, Lead, PendingCommands, SessionReadiness, SessionSignals,
        WorkSources, next_event,
    };

    let (bus, _publisher, _quota_publisher, mut broadcasts, mut messages, mut quota) =
        every_source_ready();
    let (shutdown, mut closed) = tokio::sync::watch::channel(false);
    let mut inbound = ReadyInbound;
    let mut lead = Lead::Journal;
    let retired = CancellationToken::new();
    retired.cancel();
    let mut commands = PendingCommands::new();
    for _ in 0..8 {
        commands.push(Box::pin(async { CommandCompletion::Closed }));
    }
    let mut histories = ready_histories();

    for _ in 0..7 {
        shutdown
            .send(true)
            .expect("the session holds the watch open");
        assert!(
            matches!(
                next_event(
                    &mut lead,
                    &mut inbound,
                    SessionSignals::new(&mut closed, &retired),
                    WorkSources {
                        broadcasts: &mut broadcasts,
                        messages: &mut messages,
                        quota: &mut quota,
                        commands: &mut commands,
                        histories: &mut histories,
                    },
                    SessionReadiness::live(true),
                )
                .await,
                Event::Closed(Ok(()))
            ),
            "global close must lead even with retirement and every work source ready"
        );
    }

    bus.shutdown().expect("bus shuts down");
}
