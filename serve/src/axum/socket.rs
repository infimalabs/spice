//! Authenticated app socket session routing.

use super::*;
use ::axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use futures_util::StreamExt as _;
use futures_util::stream::FuturesUnordered;
use std::pin::Pin;

mod command;

pub(super) use command::CommandCompletion;
use command::{route_command_completion, route_inbound_command};

const CAPABILITY_REPLACED_REASON: &str = "stream::capability-replaced";
const CAPABILITY_REPLACED_DETAIL: &str =
    "the capability that authenticated this stream is no longer current";

pub(super) type PendingCommands =
    FuturesUnordered<Pin<Box<dyn Future<Output = CommandCompletion> + Send>>>;
pub(super) type HistoryCompletion = (
    wire::CanonicalUuid,
    Result<wire::StreamFrame, crate::stream::Failure>,
);
pub(super) type PendingHistories =
    FuturesUnordered<Pin<Box<dyn Future<Output = HistoryCompletion> + Send>>>;

pub(super) async fn socket(
    State(state): State<Arc<SurfaceState>>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !state.access.admits(query.as_deref(), &state.capabilities) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if admit_headers(&state, &headers, true).is_err() {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !state.opens_session().await {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    upgrade
        .max_message_size(SOCKET_MESSAGE_BYTES)
        .max_frame_size(SOCKET_MESSAGE_BYTES)
        .on_upgrade(move |socket| socket_session(socket, state))
        .into_response()
}

/// Which names a bound listener answers to, and the endpoint it publishes.
///
/// A wildcard bind has no address of its own to publish: the operator reaches
/// it by whatever route their network produces, and only the port is known
/// here. Loopback is published instead, because it is the one address this
/// machine can always dial itself, and every local client does.
pub(super) fn reachable_as(bound: SocketAddr) -> (Authority, String) {
    if bound.ip().is_unspecified() {
        let local = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), bound.port());
        return (
            Authority::AnyHostAt(bound.port()),
            format!("http://{local}"),
        );
    }
    (
        Authority::Only(bound.to_string()),
        format!("http://{bound}"),
    )
}

/// Admit a request by the endpoint it names, answering with that endpoint.
///
/// The origin is checked against the admitted `Host` rather than against the
/// published one. For a specific bind those are the same string, since only one
/// `Host` is admitted at all. For a wildcard bind they are not, and this is the
/// check that still holds: a browser sends the origin it loaded the page from,
/// so requiring it to match the host it is now addressing keeps another site
/// from driving this surface, whichever of the machine's addresses was used.
pub(super) fn admit_headers<'headers>(
    state: &SurfaceState,
    headers: &'headers HeaderMap,
    origin_required: bool,
) -> Result<&'headers str, ()> {
    admit_request_headers(state, headers, origin_required, false)
}

pub(super) fn admit_asset_headers<'headers>(
    state: &SurfaceState,
    headers: &'headers HeaderMap,
    renderer: bool,
) -> Result<&'headers str, ()> {
    admit_request_headers(state, headers, false, renderer)
}

fn admit_request_headers<'headers>(
    state: &SurfaceState,
    headers: &'headers HeaderMap,
    origin_required: bool,
    opaque_renderer: bool,
) -> Result<&'headers str, ()> {
    let Some(host) = headers.get(HOST).and_then(|value| value.to_str().ok()) else {
        return Err(());
    };
    if !state.authority.admits(host) {
        return Err(());
    }
    let same_origin = format!("http://{host}");
    let origin = headers.get(ORIGIN).and_then(|value| value.to_str().ok());
    if origin_required && origin != Some(same_origin.as_str()) {
        return Err(());
    }
    if origin.is_some_and(|origin| origin != same_origin && !(opaque_renderer && origin == "null"))
    {
        return Err(());
    }
    Ok(host)
}

struct ActiveSession {
    state: Arc<SurfaceState>,
    accepted: wire::Accepted,
    capability: wire::CanonicalUuid,
    retired: CancellationToken,
    subscription: Option<crate::stream::Subscription>,
    broadcasts: Option<bus::BroadcastSubscriber>,
    messages: Option<tokio::sync::broadcast::Receiver<wire::TranscriptGeneration>>,
    quota: Option<tokio::sync::broadcast::Receiver<Arc<wire::QuotaGeneration>>>,
    commands: PendingCommands,
    histories: PendingHistories,
    lead: Lead,
}

impl ActiveSession {
    fn new(
        state: Arc<SurfaceState>,
        accepted: wire::Accepted,
        capability: wire::CanonicalUuid,
        retired: CancellationToken,
    ) -> Self {
        let broadcasts = state.stream.as_ref().map(crate::stream::Source::subscribe);
        let messages = state
            .stream
            .as_ref()
            .map(crate::stream::Source::subscribe_messages);
        let quota = state
            .stream
            .as_ref()
            .and_then(crate::stream::Source::subscribe_quota);
        Self {
            state,
            accepted,
            capability,
            retired,
            subscription: None,
            broadcasts,
            messages,
            quota,
            commands: PendingCommands::new(),
            histories: PendingHistories::new(),
            lead: Lead::Journal,
        }
    }

    async fn next(&mut self, socket: &mut WebSocket, closed: &mut watch::Receiver<bool>) -> Event {
        let live = self
            .subscription
            .as_ref()
            .is_some_and(crate::stream::Subscription::is_live);
        let continuing = self
            .subscription
            .as_ref()
            .is_some_and(crate::stream::Subscription::has_continuation);
        next_event(
            &mut self.lead,
            socket,
            SessionSignals::new(closed, &self.retired),
            WorkSources {
                broadcasts: &mut self.broadcasts,
                messages: &mut self.messages,
                quota: &mut self.quota,
                commands: &mut self.commands,
                histories: &mut self.histories,
            },
            SessionReadiness { live, continuing },
        )
        .await
    }

    async fn route(
        &mut self,
        socket: &mut WebSocket,
        closed: &watch::Receiver<bool>,
        event: Event,
    ) -> SessionFlow {
        match event {
            Event::Closed(changed) => self.route_closed(socket, closed, changed).await,
            Event::Retired => {
                send_replaced(
                    socket,
                    self.state.epoch,
                    self.subscription
                        .as_ref()
                        .map(crate::stream::Subscription::id),
                )
                .await;
                SessionFlow::Close
            }
            Event::Journal(notice) => {
                route_broadcast(
                    socket,
                    &self.state,
                    &self.accepted,
                    self.capability,
                    self.subscription
                        .as_mut()
                        .expect("live branch has a subscription"),
                    notice,
                )
                .await
            }
            Event::Continuation => {
                route_broadcast_continuation(
                    socket,
                    &self.state,
                    &self.accepted,
                    self.capability,
                    self.subscription
                        .as_mut()
                        .expect("continuation branch has a subscription"),
                )
                .await
            }
            Event::Message(notice) => {
                route_message_broadcast(
                    socket,
                    &self.state,
                    &self.accepted,
                    self.capability,
                    self.subscription
                        .as_mut()
                        .expect("live branch has a subscription"),
                    notice,
                )
                .await
            }
            Event::Quota(notice) => {
                route_quota_broadcast(
                    socket,
                    &self.state,
                    &self.accepted,
                    self.capability,
                    self.subscription
                        .as_mut()
                        .expect("live branch has a subscription"),
                    notice,
                )
                .await
            }
            Event::History(completed) => {
                route_history_completion(
                    socket,
                    &self.state,
                    &self.accepted,
                    self.capability,
                    completed,
                )
                .await
            }
            Event::Command(completed) => {
                route_command_completion(socket, &self.state, completed).await
            }
            Event::Inbound(message) => self.route_inbound(socket, message).await,
        }
    }

    async fn route_closed(
        &mut self,
        socket: &mut WebSocket,
        closed: &watch::Receiver<bool>,
        changed: Result<(), watch::error::RecvError>,
    ) -> SessionFlow {
        if changed.is_ok() && !*closed.borrow() {
            return SessionFlow::Continue;
        }
        if let Some(frame) = self
            .subscription
            .as_ref()
            .and_then(crate::stream::Subscription::closing)
        {
            send_encoded(socket, &frame).await;
        }
        SessionFlow::Close
    }

    async fn route_inbound(
        &mut self,
        socket: &mut WebSocket,
        message: Option<Result<Message, ::axum::Error>>,
    ) -> SessionFlow {
        let Some(Ok(message)) = message else {
            return SessionFlow::Close;
        };
        let bytes = match message {
            Message::Binary(bytes) => bytes.to_vec(),
            Message::Text(text) => text.as_bytes().to_vec(),
            Message::Ping(_) | Message::Pong(_) => return SessionFlow::Continue,
            Message::Close(_) => return SessionFlow::Close,
        };
        if is_stream_request(&bytes) {
            route_stream(
                socket,
                &self.state,
                &self.accepted,
                self.capability,
                &bytes,
                &mut self.subscription,
                &mut self.histories,
            )
            .await
        } else {
            route_inbound_command(
                socket,
                &self.state,
                &self.accepted,
                self.capability,
                &mut self.commands,
                &mut self.histories,
                &bytes,
            )
            .await
        }
    }
}

async fn socket_session(mut socket: WebSocket, state: Arc<SurfaceState>) {
    let _session = SessionGuard::enter(&state);
    let mut closed = state.closed.subscribe();
    let Some((accepted, capability, retired)) =
        open_session(&mut socket, &state, &mut closed).await
    else {
        return;
    };
    send_encoded(&mut socket, &accepted).await;
    let mut session = ActiveSession::new(state, accepted, capability, retired);
    loop {
        let event = session.next(&mut socket, &mut closed).await;
        if matches!(
            session.route(&mut socket, &closed, event).await,
            SessionFlow::Close
        ) {
            return;
        }
    }
}

/// Which work source a session evaluates first on one iteration.
///
/// Shutdown is deliberately not a member. It leads every arm because its
/// priority is correctness rather than fairness: a session that can observe
/// close must not route another frame first. Rotating it behind a work source
/// would trade a starvation bug for a close-ordering bug.
#[derive(Clone, Copy)]
pub(super) enum Lead {
    Journal,
    Message,
    Quota,
    Inbound,
    Command,
    History,
    Continuation,
}

impl Lead {
    /// Take this turn's lead and leave the next one in its place.
    ///
    /// Advancing is folded into the read so a caller cannot hold the lead
    /// still: a loop that forgot to rotate would be a fixed order again, and
    /// there would be no separate statement for a test to hold responsible.
    const fn take(&mut self) -> Self {
        let leading = *self;
        *self = match leading {
            Self::Journal => Self::Message,
            Self::Message => Self::Quota,
            Self::Quota => Self::Inbound,
            Self::Inbound => Self::Command,
            Self::Command => Self::History,
            Self::History => Self::Continuation,
            Self::Continuation => Self::Journal,
        };
        leading
    }
}

/// The receiving half of a session transport.
///
/// Dispatch order is the property worth guarding here, and a live socket can
/// only be made ready by a real peer racing the session. Naming the inbound
/// source lets the rotation be decided against a source that is ready on
/// demand, so the ordering is observed rather than timed.
pub(super) trait Inbound {
    /// Receive the next transport message, or `None` once the peer is gone.
    fn next_message(&mut self) -> impl Future<Output = Option<Result<Message, ::axum::Error>>>;
}

impl Inbound for WebSocket {
    fn next_message(&mut self) -> impl Future<Output = Option<Result<Message, ::axum::Error>>> {
        self.recv()
    }
}

/// One event a session acted on, named so the sources can be reordered.
pub(super) enum Event {
    Closed(Result<(), tokio::sync::watch::error::RecvError>),
    Retired,
    Journal(Result<bus::JournalCommit, tokio::sync::broadcast::error::RecvError>),
    Message(Result<Box<wire::TranscriptGeneration>, tokio::sync::broadcast::error::RecvError>),
    Quota(Result<Arc<wire::QuotaGeneration>, tokio::sync::broadcast::error::RecvError>),
    Inbound(Option<Result<Message, ::axum::Error>>),
    Command(Option<CommandCompletion>),
    History(Option<HistoryCompletion>),
    Continuation,
}

/// The two lifecycle signals that permanently end an authenticated session.
pub(super) struct SessionSignals<'session> {
    closed: &'session mut tokio::sync::watch::Receiver<bool>,
    retired: &'session CancellationToken,
}

/// The session's own work sources beyond its socket: the fleet broadcasts it
/// subscribed to and the operator commands and history requests it has in
/// flight. The fair wait rotates among these and the inbound socket.
pub(super) struct WorkSources<'session> {
    pub(super) broadcasts: &'session mut Option<bus::BroadcastSubscriber>,
    pub(super) messages:
        &'session mut Option<tokio::sync::broadcast::Receiver<wire::TranscriptGeneration>>,
    pub(super) quota:
        &'session mut Option<tokio::sync::broadcast::Receiver<Arc<wire::QuotaGeneration>>>,
    pub(super) commands: &'session mut PendingCommands,
    pub(super) histories: &'session mut PendingHistories,
}

/// Work branches enabled by the subscription's current delivery state.
#[derive(Clone, Copy)]
pub(super) struct SessionReadiness {
    live: bool,
    continuing: bool,
}

impl SessionReadiness {
    #[cfg(test)]
    pub(super) const fn live(continuing: bool) -> Self {
        Self {
            live: true,
            continuing,
        }
    }
}

impl<'session> SessionSignals<'session> {
    pub(super) const fn new(
        closed: &'session mut tokio::sync::watch::Receiver<bool>,
        retired: &'session CancellationToken,
    ) -> Self {
        Self { closed, retired }
    }
}

/// Await the next event for one session, rotating which work source leads.
///
/// `tokio::select!` under `biased` evaluates its arms in source order every
/// iteration, so a source that is ready every time keeps every later arm from
/// running at all. One fixed order therefore lets a committing fleet's journal
/// broadcasts hold off this session's own operator commands and ephemeral
/// updates without a bound, which per-connection isolation does not help: the
/// flood and the starved requests share the connection.
///
/// Rotating which of the seven work sources is evaluated first bounds the wait
/// at six iterations rather than at one source falling quiet. No source is ever
/// disabled to make room, because a disabled broadcast receiver stops draining
/// its channel and converts a scheduling question into a lag question.
pub(super) async fn next_event(
    lead: &mut Lead,
    socket: &mut impl Inbound,
    signals: SessionSignals<'_>,
    sources: WorkSources<'_>,
    readiness: SessionReadiness,
) -> Event {
    let SessionReadiness { live, continuing } = readiness;
    let WorkSources {
        broadcasts,
        messages,
        quota,
        commands,
        histories,
    } = sources;
    let journal = broadcasts.is_some() && live;
    let transcript = messages.is_some() && live;
    let quota_ready = quota.is_some() && live;
    let command = !commands.is_empty();
    let history = !histories.is_empty();
    let SessionSignals { closed, retired } = signals;
    match lead.take() {
        Lead::Journal => tokio::select! {
            biased;
            changed = closed.changed() => Event::Closed(changed),
            () = retired.cancelled() => Event::Retired,
            notice = recv_journal(broadcasts), if journal => Event::Journal(notice),
            notice = recv_message(messages), if transcript => Event::Message(notice),
            notice = recv_quota(quota), if quota_ready => Event::Quota(notice),
            message = socket.next_message() => Event::Inbound(message),
            completed = commands.next(), if command => Event::Command(completed),
            completed = histories.next(), if history => Event::History(completed),
            _ = std::future::ready(()), if continuing => Event::Continuation,
        },
        Lead::Message => tokio::select! {
            biased;
            changed = closed.changed() => Event::Closed(changed),
            () = retired.cancelled() => Event::Retired,
            notice = recv_message(messages), if transcript => Event::Message(notice),
            notice = recv_quota(quota), if quota_ready => Event::Quota(notice),
            message = socket.next_message() => Event::Inbound(message),
            completed = commands.next(), if command => Event::Command(completed),
            completed = histories.next(), if history => Event::History(completed),
            _ = std::future::ready(()), if continuing => Event::Continuation,
            notice = recv_journal(broadcasts), if journal => Event::Journal(notice),
        },
        Lead::Quota => tokio::select! {
            biased;
            changed = closed.changed() => Event::Closed(changed),
            () = retired.cancelled() => Event::Retired,
            notice = recv_quota(quota), if quota_ready => Event::Quota(notice),
            message = socket.next_message() => Event::Inbound(message),
            completed = commands.next(), if command => Event::Command(completed),
            completed = histories.next(), if history => Event::History(completed),
            _ = std::future::ready(()), if continuing => Event::Continuation,
            notice = recv_journal(broadcasts), if journal => Event::Journal(notice),
            notice = recv_message(messages), if transcript => Event::Message(notice),
        },
        Lead::Inbound => tokio::select! {
            biased;
            changed = closed.changed() => Event::Closed(changed),
            () = retired.cancelled() => Event::Retired,
            message = socket.next_message() => Event::Inbound(message),
            completed = commands.next(), if command => Event::Command(completed),
            completed = histories.next(), if history => Event::History(completed),
            _ = std::future::ready(()), if continuing => Event::Continuation,
            notice = recv_journal(broadcasts), if journal => Event::Journal(notice),
            notice = recv_message(messages), if transcript => Event::Message(notice),
            notice = recv_quota(quota), if quota_ready => Event::Quota(notice),
        },
        Lead::Command => tokio::select! {
            biased;
            changed = closed.changed() => Event::Closed(changed),
            () = retired.cancelled() => Event::Retired,
            completed = commands.next(), if command => Event::Command(completed),
            completed = histories.next(), if history => Event::History(completed),
            _ = std::future::ready(()), if continuing => Event::Continuation,
            notice = recv_journal(broadcasts), if journal => Event::Journal(notice),
            notice = recv_message(messages), if transcript => Event::Message(notice),
            notice = recv_quota(quota), if quota_ready => Event::Quota(notice),
            message = socket.next_message() => Event::Inbound(message),
        },
        Lead::History => tokio::select! {
            biased;
            changed = closed.changed() => Event::Closed(changed),
            () = retired.cancelled() => Event::Retired,
            completed = histories.next(), if history => Event::History(completed),
            _ = std::future::ready(()), if continuing => Event::Continuation,
            notice = recv_journal(broadcasts), if journal => Event::Journal(notice),
            notice = recv_message(messages), if transcript => Event::Message(notice),
            notice = recv_quota(quota), if quota_ready => Event::Quota(notice),
            message = socket.next_message() => Event::Inbound(message),
            completed = commands.next(), if command => Event::Command(completed),
        },
        Lead::Continuation => tokio::select! {
            biased;
            changed = closed.changed() => Event::Closed(changed),
            () = retired.cancelled() => Event::Retired,
            _ = std::future::ready(()), if continuing => Event::Continuation,
            notice = recv_journal(broadcasts), if journal => Event::Journal(notice),
            notice = recv_message(messages), if transcript => Event::Message(notice),
            notice = recv_quota(quota), if quota_ready => Event::Quota(notice),
            message = socket.next_message() => Event::Inbound(message),
            completed = commands.next(), if command => Event::Command(completed),
            completed = histories.next(), if history => Event::History(completed),
        },
    }
}

/// Read one journal commit from the enabled Bus broadcast.
async fn recv_journal(
    broadcasts: &mut Option<bus::BroadcastSubscriber>,
) -> Result<bus::JournalCommit, tokio::sync::broadcast::error::RecvError> {
    broadcasts
        .as_mut()
        .expect("enabled branch has a Bus broadcast")
        .recv()
        .await
}

/// Read one atomic transcript generation from the enabled broadcast.
async fn recv_message(
    messages: &mut Option<tokio::sync::broadcast::Receiver<wire::TranscriptGeneration>>,
) -> Result<Box<wire::TranscriptGeneration>, tokio::sync::broadcast::error::RecvError> {
    messages
        .as_mut()
        .expect("enabled branch has a message broadcast")
        .recv()
        .await
        .map(Box::new)
}

/// Read one redacted quota generation from the enabled broadcast.
async fn recv_quota(
    quota: &mut Option<tokio::sync::broadcast::Receiver<Arc<wire::QuotaGeneration>>>,
) -> Result<Arc<wire::QuotaGeneration>, tokio::sync::broadcast::error::RecvError> {
    quota
        .as_mut()
        .expect("enabled branch has a quota broadcast")
        .recv()
        .await
}

async fn route_message_broadcast(
    socket: &mut WebSocket,
    state: &SurfaceState,
    accepted: &wire::Accepted,
    capability: wire::CanonicalUuid,
    subscription: &mut crate::stream::Subscription,
    notice: Result<Box<wire::TranscriptGeneration>, tokio::sync::broadcast::error::RecvError>,
) -> SessionFlow {
    if !state.capabilities.matches(capability, &accepted.actor) {
        send_replaced(socket, state.epoch, Some(subscription.id())).await;
        return SessionFlow::Close;
    }
    match notice {
        Ok(generation) => {
            let generation_id = generation.generation;
            let Some(frame) = subscription.message(*generation) else {
                return SessionFlow::Continue;
            };
            if send_encoded(socket, &frame).await {
                subscription.admit_message(generation_id);
                SessionFlow::Continue
            } else {
                SessionFlow::Close
            }
        }
        Err(tokio::sync::broadcast::error::RecvError::Lagged(dropped)) => {
            let frame = subscription
                .overflow(dropped)
                .unwrap_or_else(|failure| failure.frame(state.epoch, Some(subscription.id())));
            send_encoded(socket, &frame).await;
            SessionFlow::Close
        }
        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
            let frame = stream_fault(
                state.epoch,
                Some(subscription.id()),
                wire::FaultClass::Environment,
                "stream::message-broadcast-closed",
                "resident transcript-activity broadcast closed",
            );
            send_encoded(socket, &frame).await;
            SessionFlow::Close
        }
    }
}

async fn route_quota_broadcast(
    socket: &mut WebSocket,
    state: &SurfaceState,
    accepted: &wire::Accepted,
    capability: wire::CanonicalUuid,
    subscription: &mut crate::stream::Subscription,
    notice: Result<Arc<wire::QuotaGeneration>, tokio::sync::broadcast::error::RecvError>,
) -> SessionFlow {
    if !state.capabilities.matches(capability, &accepted.actor) {
        send_replaced(socket, state.epoch, Some(subscription.id())).await;
        return SessionFlow::Close;
    }
    match notice {
        Ok(generation) => {
            let generation_id = generation.generation;
            let Some(frame) = subscription.quota(generation) else {
                return SessionFlow::Continue;
            };
            if send_encoded(socket, &frame).await {
                subscription.admit_quota(generation_id);
                SessionFlow::Continue
            } else {
                SessionFlow::Close
            }
        }
        Err(tokio::sync::broadcast::error::RecvError::Lagged(dropped)) => {
            let frame = stream_fault(
                state.epoch,
                Some(subscription.id()),
                wire::FaultClass::Protocol,
                "stream::quota-broadcast-lagged",
                &format!("quota subscription dropped {dropped} generations"),
            );
            send_encoded(socket, &frame).await;
            SessionFlow::Close
        }
        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
            let frame = stream_fault(
                state.epoch,
                Some(subscription.id()),
                wire::FaultClass::Environment,
                "stream::quota-broadcast-closed",
                "resident quota broadcast closed",
            );
            send_encoded(socket, &frame).await;
            SessionFlow::Close
        }
    }
}

async fn route_broadcast(
    socket: &mut WebSocket,
    state: &SurfaceState,
    accepted: &wire::Accepted,
    capability: wire::CanonicalUuid,
    subscription: &mut crate::stream::Subscription,
    notice: Result<bus::JournalCommit, tokio::sync::broadcast::error::RecvError>,
) -> SessionFlow {
    if !state.capabilities.matches(capability, &accepted.actor) {
        send_replaced(socket, state.epoch, Some(subscription.id())).await;
        return SessionFlow::Close;
    }
    match notice {
        Ok(commit) => {
            send_broadcast_update(socket, state, accepted, capability, subscription, commit).await
        }
        Err(tokio::sync::broadcast::error::RecvError::Lagged(dropped)) => {
            let frame = subscription
                .overflow(dropped)
                .unwrap_or_else(|failure| failure.frame(state.epoch, Some(subscription.id())));
            send_encoded(socket, &frame).await;
            SessionFlow::Close
        }
        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
            let frame = stream_fault(
                state.epoch,
                Some(subscription.id()),
                wire::FaultClass::Environment,
                "stream::broadcast-closed",
                "resident Bus cursor broadcast closed",
            );
            send_encoded(socket, &frame).await;
            SessionFlow::Close
        }
    }
}

async fn send_broadcast_update(
    socket: &mut WebSocket,
    state: &SurfaceState,
    accepted: &wire::Accepted,
    capability: wire::CanonicalUuid,
    subscription: &mut crate::stream::Subscription,
    commit: bus::JournalCommit,
) -> SessionFlow {
    let delivery = match subscription.advance(
        state.stream.as_ref().expect("broadcast has a source"),
        commit,
    ) {
        Ok(delivery) => delivery,
        Err(failure) => {
            let frame = failure.frame(state.epoch, Some(subscription.id()));
            send_encoded(socket, &frame).await;
            return SessionFlow::Close;
        }
    };
    send_live_delivery(socket, state, accepted, capability, subscription, delivery).await
}

async fn route_broadcast_continuation(
    socket: &mut WebSocket,
    state: &SurfaceState,
    accepted: &wire::Accepted,
    capability: wire::CanonicalUuid,
    subscription: &mut crate::stream::Subscription,
) -> SessionFlow {
    let delivery = match subscription.continue_live(
        state
            .stream
            .as_ref()
            .expect("continuation has a stream source"),
    ) {
        Ok(delivery) => delivery,
        Err(failure) => {
            let frame = failure.frame(state.epoch, Some(subscription.id()));
            send_encoded(socket, &frame).await;
            return SessionFlow::Close;
        }
    };
    send_live_delivery(socket, state, accepted, capability, subscription, delivery).await
}

async fn send_live_delivery(
    socket: &mut WebSocket,
    state: &SurfaceState,
    accepted: &wire::Accepted,
    capability: wire::CanonicalUuid,
    subscription: &mut crate::stream::Subscription,
    delivery: Option<crate::stream::LiveDelivery>,
) -> SessionFlow {
    if !state.capabilities.matches(capability, &accepted.actor) {
        send_replaced(socket, state.epoch, Some(subscription.id())).await;
        return SessionFlow::Close;
    }
    let Some(delivery) = delivery else {
        return SessionFlow::Continue;
    };
    for frame in delivery.frames() {
        if !send_encoded(socket, frame).await {
            return SessionFlow::Close;
        }
    }
    subscription.admit(delivery);
    SessionFlow::Continue
}

async fn open_session(
    socket: &mut WebSocket,
    state: &SurfaceState,
    closed: &mut watch::Receiver<bool>,
) -> Option<(wire::Accepted, wire::CanonicalUuid, CancellationToken)> {
    let message = receive_message(socket, closed).await?;
    let bytes = match message {
        Message::Binary(bytes) => bytes.to_vec(),
        Message::Text(text) => text.as_bytes().to_vec(),
        Message::Close(_) | Message::Ping(_) | Message::Pong(_) => return None,
    };
    match wire::decode::<wire::Hello>(&bytes) {
        Ok(hello) => {
            match state
                .capabilities
                .admit_session(&hello, state.epoch, &state.version.product)
            {
                Ok(admission) => Some((admission.accepted, hello.capability, admission.retired)),
                Err(OpeningError::Refused(refusal)) => {
                    send_encoded(socket, &refusal).await;
                    None
                }
                Err(OpeningError::Fault(fault)) => {
                    send_encoded(socket, &fault).await;
                    None
                }
            }
        }
        Err(error) => {
            send_encoded(
                socket,
                &wire::Fault {
                    class: wire::FaultClass::Protocol,
                    reason: "hello::undecodable".to_owned(),
                    detail: error.to_string(),
                },
            )
            .await;
            None
        }
    }
}

async fn receive_message(
    socket: &mut WebSocket,
    closed: &mut watch::Receiver<bool>,
) -> Option<Message> {
    loop {
        if *closed.borrow() {
            return None;
        }
        let message = tokio::select! {
            changed = closed.changed() => {
                changed.ok()?;
                continue;
            }
            message = socket.recv() => message,
        };
        let Some(Ok(message)) = message else {
            return None;
        };
        if *closed.borrow() {
            return None;
        }
        return Some(message);
    }
}

#[derive(Clone, Copy)]
enum SessionFlow {
    Continue,
    Close,
}

fn is_stream_request(bytes: &[u8]) -> bool {
    wire::decode::<serde_json::Value>(bytes)
        .ok()
        .and_then(|value| {
            value
                .as_object()
                .and_then(|object| object.get("stream"))
                .cloned()
        })
        .is_some()
}

async fn route_stream(
    socket: &mut WebSocket,
    state: &SurfaceState,
    accepted: &wire::Accepted,
    capability: wire::CanonicalUuid,
    bytes: &[u8],
    subscription: &mut Option<crate::stream::Subscription>,
    histories: &mut PendingHistories,
) -> SessionFlow {
    if !state.capabilities.matches(capability, &accepted.actor) {
        send_replaced(
            socket,
            state.epoch,
            subscription.as_ref().map(crate::stream::Subscription::id),
        )
        .await;
        return SessionFlow::Close;
    }
    let request = match wire::decode::<wire::StreamRequest>(bytes) {
        Ok(request) => request,
        Err(error) => {
            let frame = stream_fault(
                state.epoch,
                None,
                wire::FaultClass::Protocol,
                "stream::undecodable",
                &error.to_string(),
            );
            send_encoded(socket, &frame).await;
            return SessionFlow::Continue;
        }
    };
    let Some(source) = state.stream.as_ref() else {
        let frame = stream_fault(
            state.epoch,
            Some(request.id()),
            wire::FaultClass::Environment,
            "stream::unavailable",
            "this app socket has no resident Bus stream",
        );
        send_encoded(socket, &frame).await;
        return SessionFlow::Continue;
    };
    if matches!(request, wire::StreamRequest::MessageHistory { .. }) {
        let Some(demand) = state.history.admit() else {
            let refusal =
                crate::stream::Failure::Refusal(wire::Refusal::bare(wire::RefusalDiagnostic::new(
                    "stream::message-history-busy",
                    Some("serve".to_owned()),
                    Some("capacity for another history request".to_owned()),
                    Some("every history slot awaiting an answer".to_owned()),
                )));
            send_encoded(socket, &refusal.frame(state.epoch, Some(request.id()))).await;
            return SessionFlow::Continue;
        };
        histories.push(Box::pin(message_history_off_runtime(
            source.clone(),
            demand,
            accepted.clone(),
            request,
        )));
        return SessionFlow::Continue;
    }

    let result = match &request {
        wire::StreamRequest::Subscribe { .. } => {
            if subscription.is_some() {
                Err(crate::stream::Failure::Refusal(wire::Refusal::bare(
                    wire::RefusalDiagnostic::new(
                        "stream::already-subscribed",
                        Some("session".to_owned()),
                        Some("one subscription".to_owned()),
                        Some("a second subscription".to_owned()),
                    ),
                )))
            } else {
                match source.begin(accepted, &request) {
                    Ok((opened, frames)) => {
                        if !state.capabilities.matches(capability, &accepted.actor) {
                            send_replaced(socket, state.epoch, Some(request.id())).await;
                            return SessionFlow::Close;
                        }
                        for frame in frames {
                            if !send_encoded(socket, &frame).await {
                                return SessionFlow::Close;
                            }
                        }
                        *subscription = Some(opened);
                        return SessionFlow::Continue;
                    }
                    Err(failure) => Err(failure),
                }
            }
        }
        wire::StreamRequest::Continue { .. } => match subscription.as_mut() {
            Some(subscription) => subscription.continue_page(source, &request),
            None => Err(crate::stream::Failure::Refusal(wire::Refusal::bare(
                wire::RefusalDiagnostic::new(
                    "stream::subscription-required",
                    Some("session".to_owned()),
                    Some("an active subscription".to_owned()),
                    Some("no subscription".to_owned()),
                ),
            ))),
        },
        wire::StreamRequest::Thread { .. } => source.thread(accepted, &request),
        wire::StreamRequest::MessageHistory { .. } => {
            unreachable!("history requests are scheduled before synchronous routing")
        }
        wire::StreamRequest::Ping { .. } => config_probe_off_runtime(source, &request).await,
    };
    route_stream_result(socket, state, accepted, capability, request.id(), result).await
}

async fn config_probe_off_runtime(
    source: &crate::stream::Source,
    request: &wire::StreamRequest,
) -> Result<wire::StreamFrame, crate::stream::Failure> {
    let source = source.clone();
    let request = request.clone();
    tokio::task::spawn_blocking(move || source.pong(&request))
        .await
        .unwrap_or_else(|error| {
            Err(crate::stream::Failure::Fault(wire::Fault {
                class: wire::FaultClass::Environment,
                reason: "stream::config-capture-abandoned".to_owned(),
                detail: error.to_string(),
            }))
        })
}

/// Answer one history request from the blocking pool, not the runtime thread.
///
/// The provider behind [`crate::stream::Source::message_history`] may chain
/// several multi-megabyte tape windows and hold its thread for seconds. The
/// surface runtime is one thread shared by every socket, live publication,
/// and the resident loop, so each read leaves it, waiting its turn among the
/// surface's bounded read slots. The session keeps receiving requests while
/// these joins race to completion.
async fn message_history_off_runtime(
    source: crate::stream::Source,
    demand: super::history::HistoryDemand,
    accepted: wire::Accepted,
    request: wire::StreamRequest,
) -> HistoryCompletion {
    let id = request.id();
    let result = demand
        .run(move || source.message_history(&accepted, &request))
        .await
        .unwrap_or_else(|detail| {
            Err(crate::stream::Failure::Fault(wire::Fault {
                class: wire::FaultClass::Environment,
                reason: "stream::message-history-abandoned".to_owned(),
                detail,
            }))
        });
    (id, result)
}

async fn route_history_completion(
    socket: &mut WebSocket,
    state: &SurfaceState,
    accepted: &wire::Accepted,
    capability: wire::CanonicalUuid,
    completed: Option<HistoryCompletion>,
) -> SessionFlow {
    let Some((id, result)) = completed else {
        return SessionFlow::Close;
    };
    route_stream_result(socket, state, accepted, capability, id, result).await
}

async fn route_stream_result(
    socket: &mut WebSocket,
    state: &SurfaceState,
    accepted: &wire::Accepted,
    capability: wire::CanonicalUuid,
    id: wire::CanonicalUuid,
    result: Result<wire::StreamFrame, crate::stream::Failure>,
) -> SessionFlow {
    if !state.capabilities.matches(capability, &accepted.actor) {
        send_replaced(socket, state.epoch, Some(id)).await;
        return SessionFlow::Close;
    }
    match result {
        Ok(frame) => {
            if send_encoded(socket, &frame).await {
                SessionFlow::Continue
            } else {
                SessionFlow::Close
            }
        }
        Err(failure) => {
            let frame = failure.frame(state.epoch, Some(id));
            send_encoded(socket, &frame).await;
            SessionFlow::Continue
        }
    }
}

async fn send_replaced(
    socket: &mut WebSocket,
    epoch: wire::CanonicalUuid,
    id: Option<wire::CanonicalUuid>,
) {
    let frame = stream_fault(
        epoch,
        id,
        wire::FaultClass::Protocol,
        CAPABILITY_REPLACED_REASON,
        CAPABILITY_REPLACED_DETAIL,
    );
    send_encoded(socket, &frame).await;
}

fn stream_fault(
    epoch: wire::CanonicalUuid,
    id: Option<wire::CanonicalUuid>,
    class: wire::FaultClass,
    reason: &str,
    detail: &str,
) -> wire::StreamFrame {
    wire::StreamFrame::Fault {
        v: wire::WireVersion::CURRENT,
        epoch,
        id,
        fault: wire::Fault {
            class,
            reason: reason.to_owned(),
            detail: detail.to_owned(),
        },
    }
}

pub(super) fn is_dark(command: &wire::AdmittedCommand) -> bool {
    matches!(
        command.body(),
        wire::intent::Intent::UpChange(wire::intent::UpChange {
            state: wire::ResidentState::Dark
        })
    )
}

async fn send_encoded<T: serde::Serialize>(socket: &mut WebSocket, value: &T) -> bool {
    let Ok(bytes) = wire::encode(value) else {
        return false;
    };
    socket.send(Message::Binary(bytes.into())).await.is_ok()
}
