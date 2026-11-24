use crate::browser::connection::refresh::send_rack;

use super::*;
mod actions;
use actions::{dispatch_client_action, topology_pending_kind};
mod bootstrap;
use bootstrap::establish_connection;
pub(super) use bootstrap::loaded_app_generation;

mod settlement;
use settlement::{BorrowedTaskClose, BorrowedTaskDelete, send_task_close, send_task_delete};

const BROWSER_AUTH_TOKEN_STORAGE: &str = "spice:app-auth-token:v1";
/// The served app generation this tab last reloaded toward, so a document that
/// still cannot load it never reloads again.
const APP_RELOAD_GENERATION_STORAGE: &str = "spice:app-reload-generation:v1";
/// The status a generation-qualified app route answers once its generation is
/// no longer served.
const GONE: u16 = 410;

thread_local! {
    /// The mounted surface's unload guard, so a retired-generation answer to
    /// any app fetch leaves through the same handoff as the socket path.
    static UNLOAD: RefCell<Option<UnloadGuard>> = const { RefCell::new(None) };
}

/// Probe one generation-qualified app route after it failed to load. A `410
/// Gone` names the generation the resident serves in place of the loaded
/// document's; the tab reloads into it once, drafts handed off.
pub(in crate::browser) async fn reload_if_retired(route: String) {
    let probe = Request::get(&route).method(gloo_net::http::Method::HEAD);
    let Ok(response) = probe.send().await else {
        return;
    };
    let served = response
        .headers()
        .get(wire::hello::APP_GENERATION_HEADER)
        .and_then(|value| value.parse::<wire::Digest>().ok());
    let Some(served) = served.filter(|_| response.status() == GONE) else {
        return;
    };
    if loaded_app_generation().is_ok_and(|loaded| loaded != served) {
        leave_for_served_app(&served);
    }
}

/// Leave for the generation the resident serves through the mounted surface's
/// unload guard, returning whether the reload started.
fn leave_for_served_app(served: &wire::Digest) -> bool {
    UNLOAD.with_borrow(|unload| {
        unload
            .as_ref()
            .is_some_and(|unload| reload_into_served_app(served, unload))
    })
}

/// Capture the bearer token carried by a remote app opening before the
/// fragment becomes ordinary route state.
pub(super) fn capture_opening_auth_token() -> Result<bool, String> {
    let window = web_sys::window().ok_or_else(|| "browser window is unavailable".to_owned())?;
    let hash = window
        .location()
        .hash()
        .map_err(|_| "browser URL fragment is unavailable".to_owned())?;
    let Some(token) = crate::view::opening_auth_token(&hash) else {
        return Ok(false);
    };
    let storage = window
        .session_storage()
        .map_err(|_| "browser session storage is unavailable".to_owned())?
        .ok_or_else(|| "browser session storage is unavailable".to_owned())?;
    storage
        .set_item(BROWSER_AUTH_TOKEN_STORAGE, token)
        .map_err(|_| "browser auth token could not be retained".to_owned())?;
    Ok(true)
}

/// Run connections one after another for as long as the surface is mounted.
///
/// Each attempt is awaited to completion before the next begins, and the socket
/// it opens lives in that call's locals rather than a handle anything else can
/// reach. So an ending connection has no successor to confuse itself with: the
/// next one does not exist yet. A guard comparing the closing socket against
/// the current one would have nothing to compare, which is why none appears
/// here — the hazard it answers needs two live connections, and spawning a
/// second one is what would reintroduce it.
pub(super) async fn run_surface(
    active: Rc<Cell<bool>>,
    handles: SurfaceHandles,
    actions: ActionQueue,
    mut action_wakes: mpsc::Receiver<()>,
) {
    let mut data = SurfaceData::default();
    UNLOAD.set(Some(handles.unload.clone()));
    let mut backoff = crate::reconnect::Backoff::new();
    while active.get() {
        if matches!(&*handles.connection, Connection::Starting) {
            handles.connection.set(Connection::Connecting);
        }
        let result =
            run_connection(&active, &handles, &mut data, &actions, &mut action_wakes).await;
        if !active.get() || super::draft_handoff::leaving() {
            break;
        }
        // `run_connection` returns `Ok` only where its own `while active.get()`
        // guard stops, and the break above has already taken that exit. An `Ok`
        // arriving here would mean the two guards disagree, so it fails loudly
        // instead of publishing an offline reason no socket ever produced.
        let ended =
            result.expect_err("run_connection returns Ok only once the surface is inactive");
        handles.submissions.queue.borrow_mut().reconnect();
        handles.submissions.publish();
        actions.reconnect();
        // A stale document's session dies on the newer resident's DTOs, so its
        // admission proves nothing about the next attempt.
        let delay = backoff.settle(ended.admitted && ended.served_app.is_none());
        handles.connection.set(Connection::Offline(ended.reason));
        TimeoutFuture::new(delay).await;
    }
    UNLOAD.set(None);
}

/// Why one connection ended, and whether it ever became an admitted session.
///
/// Every end is a failure to the surface, but they are not the same evidence. A
/// connection that reached admission proved the resident reachable, so whatever
/// ended it says nothing about how long the next attempt should wait; one that
/// never got there is the only kind that should push the delay out.
struct Ended {
    reason: String,
    admitted: bool,
    /// The app generation this attempt's reachable resident serves in place of
    /// the loaded document's.
    served_app: Option<wire::Digest>,
}

/// Leave a superseded app generation for the one the resident serves,
/// returning whether the reload started.
///
/// Every app route revalidates, so an ordinary reload loads the served
/// generation; a stale tab must not keep running old code merely because the
/// app protocol still matches. Unsent drafts travel to the reloaded document;
/// only work no handoff can carry keeps the tab where it is behind the explicit
/// reload control. One reload per served generation keeps a document that still
/// loads stale from cycling.
pub(super) fn reload_into_served_app(served: &wire::Digest, unload: &UnloadGuard) -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    let Some(storage) = window.session_storage().ok().flatten() else {
        return false;
    };
    let served = served.to_string();
    if storage
        .get_item(APP_RELOAD_GENERATION_STORAGE)
        .ok()
        .flatten()
        .is_some_and(|attempted| attempted == served)
        || !unload.hand_off()
        || storage
            .set_item(APP_RELOAD_GENERATION_STORAGE, &served)
            .is_err()
    {
        return false;
    }
    super::draft_handoff::reload(&window);
    true
}

async fn run_connection(
    active: &Rc<Cell<bool>>,
    handles: &SurfaceHandles,
    data: &mut SurfaceData,
    actions: &ActionQueue,
    action_wakes: &mut mpsc::Receiver<()>,
) -> Result<(), Ended> {
    let abort = web_sys::AbortController::new().map_err(|_| Ended {
        reason: "browser could not create a connection abort controller".to_owned(),
        admitted: false,
        served_app: None,
    })?;
    let signal = abort.signal();
    let mut served_app = None;
    let established = {
        let handshake = establish_connection(
            &handles.connection,
            &handles.surface,
            &handles.identity,
            &handles.app_update,
            &mut served_app,
            data,
            &signal,
        )
        .fuse();
        let deadline = TimeoutFuture::new(CONNECTION_HANDSHAKE_TIMEOUT_MILLIS).fuse();
        futures_util::pin_mut!(handshake, deadline);
        select! {
            result = handshake => result,
            () = deadline => {
                abort.abort();
                Err(format!(
                    "connection handshake timed out after \
                     {CONNECTION_HANDSHAKE_TIMEOUT_MILLIS} ms"
                ))
            }
        }
    }
    .map_err(|reason| Ended {
        reason,
        admitted: false,
        served_app: served_app.take(),
    })?;
    // Reaching here *is* the admission: `establish_connection` only returns once
    // the acceptance frame has been admitted and the runtime published. Deriving
    // the flag from control flow rather than from a separate variable keeps the
    // two from ever disagreeing about whether this connection counted.
    pump_session(active, handles, data, actions, action_wakes, established)
        .await
        .map_err(|reason| Ended {
            reason,
            admitted: true,
            served_app,
        })
}

/// Carry one admitted session until it fails or the surface goes away.
///
/// The select is the entire event loop. Its fixed-cadence probe observes both
/// connection liveness and repository configuration without another poller.
/// Unchanged configuration does not cause a board read or render.
///
/// A half-open socket is the case this exists for. It stays open, reports no
/// error, and simply stops delivering, so the reader future never completes and
/// every assertion downstream of it waits forever. The probe converts that into
/// an ordinary failure, which the one existing reconnect boundary already
/// handles -- deliberately not a second reconnect path, which would race the
/// first and double-subscribe.
async fn pump_session(
    active: &Rc<Cell<bool>>,
    handles: &SurfaceHandles,
    data: &SurfaceData,
    actions: &ActionQueue,
    action_wakes: &mut mpsc::Receiver<()>,
    established: EstablishedConnection,
) -> Result<(), String> {
    let EstablishedConnection {
        session,
        mut writer,
        mut reader,
        principal,
        mut hydrating,
    } = established;
    send_subscription(&principal, data, &mut writer).await?;
    send_refresh(&principal, &mut writer, actions, &handles.submissions).await?;
    refresh::send_task_detail_reads(&principal, &mut writer, actions, data).await?;
    drain_submissions(&principal, &mut writer, actions, &handles.submissions).await?;
    actions.rearm().map_err(|error| error.to_string())?;
    let mut probe_due = TimeoutFuture::new(LIVENESS_PROBE_MILLIS).fuse();
    let mut lane_refresh = futures_util::future::Fuse::terminated();
    let mut probed = false;
    while active.get() {
        let incoming = reader.next().fuse();
        let action_wake = action_wakes.next().fuse();
        futures_util::pin_mut!(incoming, action_wake);
        futures_util::select_biased! {
            () = lane_refresh => {
                refresh::send_lanes(&principal, &mut writer, actions, &handles.submissions).await?;
            },
            frame = incoming => {
                // Traffic answers the outstanding probe but does not postpone
                // the next one: probes also observe repository configuration.
                probed = false;
                if let Some(deadline) = apply_incoming_frame(
                    frame,
                    &session,
                    Surface { handles, data, actions },
                    &principal,
                    &mut writer,
                    !hydrating,
                ).await? {
                    lane_refresh = match deadline {
                        Some(millis) => TimeoutFuture::new(millis).fuse(),
                        None => futures_util::future::Fuse::terminated(),
                    };
                }
                if hydrating && actions.current_reads_settled() {
                    finish_hydration(handles, data, &principal)?;
                    hydrating = false;
                    actions.rearm().map_err(|error| error.to_string())?;
                }
            },
            wake = action_wake => {
                wake.ok_or_else(|| "command channel closed".to_owned())?;
                if hydrating {
                    continue;
                }
                service_action_wake(
                    &principal,
                    data,
                    &mut writer,
                    handles,
                    actions,
                ).await?;
            },
            () = probe_due => {
                if probed {
                    return Err(format!(
                        "socket stayed open without answering a liveness probe within \
                         {LIVENESS_PROBE_MILLIS} ms"
                    ));
                }
                probed = true;
                send_probe(&principal, &mut writer).await?;
                probe_due = TimeoutFuture::new(LIVENESS_PROBE_MILLIS).fuse();
            },
        }
    }
    Ok(())
}

fn finish_hydration(
    handles: &SurfaceHandles,
    data: &SurfaceData,
    principal: &Principal,
) -> Result<(), String> {
    let complete = data.read().as_ref().is_some_and(AppRuntime::is_hydrated);
    if !complete {
        return Err("replacement scope settled without every current snapshot".to_owned());
    }
    handles.surface.set(data.publication());
    handles.connection.set(Connection::Online {
        epoch: principal.epoch().to_string(),
    });
    Ok(())
}

async fn service_action_wake(
    principal: &Principal,
    data: &SurfaceData,
    writer: &mut SocketWriter,
    handles: &SurfaceHandles,
    actions: &ActionQueue,
) -> Result<(), String> {
    let Some(lease) = actions.lease() else {
        return Ok(());
    };
    dispatch_client_action(principal, data, writer, actions, handles, lease).await?;
    actions.rearm().map_err(|error| error.to_string())
}

/// The surface one socket feeds: the handles it publishes to, the data it
/// updates, and the action queue it drains.
#[derive(Clone, Copy)]
struct Surface<'a> {
    handles: &'a SurfaceHandles,
    data: &'a SurfaceData,
    actions: &'a ActionQueue,
}

async fn apply_incoming_frame(
    frame: Option<Result<Message, WebSocketError>>,
    session: &Session,
    surface: Surface<'_>,
    principal: &Principal,
    writer: &mut SocketWriter,
    publish: bool,
) -> Result<Option<Option<u32>>, String> {
    let Surface {
        handles,
        data,
        actions,
    } = surface;
    let frame = frame
        .ok_or_else(|| "resident closed the socket".to_owned())?
        .map_err(websocket_error)?;
    let bytes = binary_message(frame)?;
    diagnostics::instant("frame.received", || diagnostics::Metadata {
        frame_bytes: Some(bytes.len()),
        ..diagnostics::Metadata::default()
    });
    let application = handle_incoming(session, handles, data, actions, &bytes, publish)?;
    relay_directives(principal, writer, application.directives).await?;
    send_after_frame(
        principal,
        writer,
        actions,
        &handles.submissions,
        application.refresh,
        application.stale,
    )
    .await?;
    refresh::send_task_detail_reads(principal, writer, actions, data).await?;
    refresh::send_principal_input_texts(principal, writer, actions, data).await?;
    refresh::send_contribution_reads(principal, writer, actions, data).await?;
    drain_submissions(principal, writer, actions, &handles.submissions).await?;
    actions.rearm().map_err(|error| error.to_string())?;
    Ok(application.lane_refresh)
}

/// Ask the resident to prove it is still reading this socket.
async fn send_probe(principal: &Principal, writer: &mut SocketWriter) -> Result<(), String> {
    send_stream_request(
        writer,
        &StreamRequest::Ping {
            v: WireVersion::CURRENT,
            id: random_uuid()?,
            epoch: principal.epoch(),
        },
    )
    .await
}

/// Carry back the stream directives one admitted frame produced.
async fn relay_directives(
    principal: &Principal,
    writer: &mut SocketWriter,
    directives: Vec<StreamDirective>,
) -> Result<(), String> {
    for directive in directives {
        if send_stream_directive(principal, writer, directive).await? {
            return Err("resident stream requested reconnect".to_owned());
        }
    }
    Ok(())
}

/// Send one operator action as the typed command the resident admits.
async fn dispatch_action(
    principal: &Principal,
    data: &SurfaceData,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    submissions: &SubmissionHandle,
    action: &ClientAction,
) -> Result<Option<CanonicalUuid>, String> {
    match action {
        ClientAction::Rack { request, respond } => {
            send_rack(principal, writer, actions, request, respond)
                .await
                .map(Some)
        }
        ClientAction::TrustShow { digest, respond } => {
            refresh::send_trust_show(principal, writer, actions, digest, respond)
                .await
                .map(Some)
        }
        ClientAction::Jobs { respond } => refresh::send_jobs(principal, writer, actions, respond)
            .await
            .map(Some),
        ClientAction::Job {
            target,
            stop,
            respond,
        } => refresh::send_job(principal, writer, actions, target, *stop, respond)
            .await
            .map(Some),
        ClientAction::ComposeMessage {
            lane,
            message,
            attachments,
            respond,
        } => send_composed_message(
            principal,
            data,
            writer,
            actions,
            ComposedMessage {
                lane: *lane,
                message,
                attachments,
                respond,
            },
        )
        .await
        .map(Some),
        action @ (ClientAction::RoomPost(_)
        | ClientAction::SteerControl { .. }
        | ClientAction::Preparation { .. }) => {
            dispatch_room_action(principal, writer, actions, action).await
        }
        ClientAction::Principal { request, respond } => {
            refresh::send_principal_write(principal, writer, actions, request, respond)
                .await
                .map(Some)
        }
        action @ (ClientAction::Draft { .. }
        | ClientAction::Conclude { .. }
        | ClientAction::Deferral { .. }) => send_task_change(principal, writer, actions, action)
            .await
            .map(Some),
        ClientAction::DrainSubmissions => {
            drain_submissions(principal, writer, actions, submissions)
                .await
                .map(|_| None)
        }
        // Owed reads live in the runtime, which `dispatch_client_action`
        // holds; it pumps them before reaching this send.
        ClientAction::TaskDetails => Ok(None),
        ClientAction::TaskRelations { key } => {
            refresh::send_task_relations(principal, writer, actions, key)
                .await
                .map(|_| None)
        }
        ClientAction::TrustDecision {
            digest,
            decision,
            expected,
            ..
        } => send_trust_decision(principal, writer, actions, digest, *decision, expected)
            .await
            .map(Some),
        ClientAction::GlobalSettings(settings) => {
            send_global_settings(principal, writer, actions, settings)
                .await
                .map(Some)
        }
        ClientAction::MessageHistory {
            generation,
            lane,
            before,
        } => send_message_history(principal, writer, *generation, *lane, *before)
            .await
            .map(|_| None),
        ClientAction::LaneState { request } => {
            send_member_lane_state(principal, writer, actions, request)
                .await
                .map(|_| None)
        }
        ClientAction::RenewLane { .. }
        | ClientAction::RetuneLane { .. }
        | ClientAction::ArrangeTeams { .. }
        | ClientAction::ConfigureTeam { .. }
        | ClientAction::ConsentRuling { .. } => {
            unreachable!("topology and consent dispatch precede ordinary operator actions")
        }
        action @ (ClientAction::Close { .. }
        | ClientAction::Delete { .. }
        | ClientAction::Steer { .. }) => {
            dispatch_authored_action(principal, writer, actions, action)
                .await
                .map(Some)
        }
    }
}

async fn dispatch_room_action(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    action: &ClientAction,
) -> Result<Option<CanonicalUuid>, String> {
    match action {
        ClientAction::RoomPost(post) => rooms::send_room_post(principal, writer, actions, post)
            .await
            .map(Some),
        ClientAction::SteerControl {
            request,
            line_respond,
            directive_respond,
        } => {
            rooms::send_steer_control(
                principal,
                writer,
                actions,
                request,
                line_respond,
                directive_respond,
            )
            .await
        }
        ClientAction::Preparation { request, respond } => {
            rooms::send_preparation(principal, writer, actions, request, respond)
                .await
                .map(Some)
        }
        _ => unreachable!("only room actions reach room dispatch"),
    }
}

/// Submit one native ruling on an exact executable trust requirement.
async fn send_trust_decision(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    digest: &wire::Digest,
    decision: crate::client_action::TrustDecision,
    expected: &[Witness],
) -> Result<CanonicalUuid, String> {
    send_typed(
        principal,
        writer,
        actions,
        PendingKind::TrustDecision {
            digest: digest.clone(),
            decision,
        },
        decision.command(),
        expected.to_vec(),
        wire::intent::TrustRef {
            digest: digest.clone(),
            worktree: None,
        },
    )
    .await
}

/// Wake, drain, or remove one resident member.
async fn send_member_lane_state(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    request: &crate::runtime::MemberRequest,
) -> Result<CanonicalUuid, String> {
    use crate::runtime::MemberCommand;
    let action = match request.command {
        MemberCommand::Wake => LaneAction::Wake,
        MemberCommand::Drain => LaneAction::Drain,
        MemberCommand::Remove => LaneAction::Remove,
        MemberCommand::Renew { .. }
        | MemberCommand::Retune { .. }
        | MemberCommand::RetuneEffort { .. }
        | MemberCommand::RetuneFast { .. } => {
            return Err("a lane state change carried a non-lifecycle member command".to_owned());
        }
    };
    send_typed(
        principal,
        writer,
        actions,
        PendingKind::MemberLane {
            request: request.clone(),
        },
        CommandKind::LaneState,
        vec![],
        LaneState {
            lane: request.lane,
            action,
            worktree: None,
        },
    )
    .await
}

async fn dispatch_authored_action(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    action: &ClientAction,
) -> Result<CanonicalUuid, String> {
    match action {
        ClientAction::Close {
            id,
            task,
            validation,
            expected,
            force_claimed,
        } => {
            send_task_close(
                principal,
                writer,
                actions,
                *id,
                expected,
                BorrowedTaskClose {
                    task,
                    validation,
                    force_claimed: *force_claimed,
                },
            )
            .await
        }
        ClientAction::Delete {
            id,
            task,
            reason,
            expected,
            force_claimed,
        } => {
            send_task_delete(
                principal,
                writer,
                actions,
                *id,
                expected,
                BorrowedTaskDelete {
                    task,
                    reason,
                    force_claimed: *force_claimed,
                },
            )
            .await
        }
        ClientAction::Steer {
            id,
            fanout,
            lane,
            occupant,
            epoch,
            message,
            attachments,
        } => {
            send_steer(
                principal,
                writer,
                actions,
                *id,
                BorrowedSteerSend {
                    fanout: *fanout,
                    lane: *lane,
                    message,
                    attachments,
                },
                Witness::Residency {
                    lane: *lane,
                    occupant: Some(*occupant),
                    epoch: *epoch,
                },
            )
            .await
        }
        _ => unreachable!("authored action dispatch is partitioned"),
    }
}

struct ComposedMessage<'a> {
    lane: Option<CanonicalUuid>,
    message: &'a str,
    attachments: &'a [Media],
    respond: &'a crate::client_action::MessageResponse,
}

async fn send_composed_message(
    principal: &Principal,
    data: &SurfaceData,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    request: ComposedMessage<'_>,
) -> Result<CanonicalUuid, String> {
    let expected = if let Some(lane) = request.lane {
        let data = data.read();
        let lanes = data
            .as_ref()
            .and_then(AppRuntime::lanes)
            .ok_or_else(|| "explicit message recipient has no current lane table".to_owned())?;
        let member = lanes
            .member(lane)
            .ok_or_else(|| format!("explicit message recipient lane {lane} is unavailable"))?;
        vec![Witness::Residency {
            lane,
            occupant: member.occupant,
            epoch: lanes.epoch,
        }]
    } else {
        Vec::new()
    };
    send_typed(
        principal,
        writer,
        actions,
        PendingKind::ComposeMessage {
            respond: request.respond.clone(),
        },
        CommandKind::SteerSend,
        expected,
        SteerSend {
            fanout: random_uuid()?,
            recipients: Vec::new(),
            lane: request.lane,
            message: request.message.to_owned(),
            attachments: request.attachments.to_vec(),
        },
    )
    .await
}

async fn send_global_settings(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    settings: &GlobalSettingsSet,
) -> Result<CanonicalUuid, String> {
    send_typed(
        principal,
        writer,
        actions,
        PendingKind::Settings,
        CommandKind::GlobalSettingsSet,
        vec![],
        settings.clone(),
    )
    .await
}

async fn send_message_history(
    principal: &Principal,
    writer: &mut SocketWriter,
    generation: u64,
    lane: CanonicalUuid,
    before: Option<wire::MessageLocus>,
) -> Result<(), String> {
    send_stream_request(
        writer,
        &StreamRequest::MessageHistory {
            v: WireVersion::CURRENT,
            id: random_uuid()?,
            epoch: principal.epoch(),
            frontiers: vec![wire::MessageHistoryCursor {
                generation,
                lane,
                before,
            }],
        },
    )
    .await
}

/// Steer one lane's occupant, `residency` naming the occupancy the resident
/// must still hold for the steer to land.
async fn send_steer(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    id: CanonicalUuid,
    body: BorrowedSteerSend<'_>,
    residency: Witness,
) -> Result<CanonicalUuid, String> {
    send_typed_as(
        principal,
        writer,
        actions,
        TrackedCommand {
            id,
            pending_kind: PendingKind::Steer,
            kind: CommandKind::SteerSend,
            expected: vec![residency],
        },
        body,
    )
    .await
}

async fn drain_submissions(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    submissions: &SubmissionHandle,
) -> Result<(), String> {
    let ready = submissions.queue.borrow().ready();
    for submission in ready {
        let (occupant, epoch) = submission.residency().ok_or_else(|| {
            format!(
                "composer submission {} became steerable without residency",
                submission.id
            )
        })?;
        let body = SteerSend {
            fanout: submission.fanout,
            recipients: submission.recipients().to_vec(),
            lane: Some(submission.lane),
            message: submission.message().to_owned(),
            attachments: submission.attachments().to_vec(),
        };
        let expected = vec![Witness::Residency {
            lane: submission.lane,
            occupant,
            epoch,
        }];
        send_typed_as(
            principal,
            writer,
            actions,
            TrackedCommand {
                id: submission.id,
                pending_kind: PendingKind::SteerSubmission {
                    lane: submission.lane,
                },
                kind: CommandKind::SteerSend,
                expected,
            },
            body,
        )
        .await?;
        submissions.queue.borrow_mut().sent(submission.id)?;
        submissions.publish();
    }
    Ok(())
}

#[derive(Serialize)]
struct BorrowedTeamArrange<'a> {
    topology_revision: u64,
    teams: &'a [TeamArrangement],
}

#[derive(Serialize)]
struct BorrowedSteerSend<'a> {
    fanout: CanonicalUuid,
    lane: CanonicalUuid,
    message: &'a str,
    #[serde(skip_serializing_if = "slice_is_empty")]
    attachments: &'a [Media],
}

fn slice_is_empty<T>(value: &&[T]) -> bool {
    value.is_empty()
}

fn browser_auth_token() -> Result<Option<String>, String> {
    let Some(storage) = web_sys::window()
        .ok_or_else(|| "browser window is unavailable".to_owned())?
        .session_storage()
        .ok()
        .flatten()
    else {
        return Ok(None);
    };
    let token = storage
        .get_item(BROWSER_AUTH_TOKEN_STORAGE)
        .map_err(|_| "browser auth token could not be read".to_owned())?;
    if let Some(token) = token.as_deref()
        && !crate::view::app_auth_token_is_valid(token)
    {
        return Err("retained browser auth token is invalid".to_owned());
    }
    Ok(token)
}

fn authenticated_path(path: &str, token: Option<&str>) -> String {
    token.map_or_else(|| path.to_owned(), |token| format!("{path}?auth={token}"))
}

async fn send_typed<B: Serialize>(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    pending_kind: PendingKind,
    kind: CommandKind,
    expected: Vec<Witness>,
    body: B,
) -> Result<CanonicalUuid, String> {
    send_typed_classified(
        principal,
        writer,
        actions,
        pending_kind,
        kind,
        expected,
        body,
    )
    .await
    .map_err(Into::into)
}

async fn send_typed_classified<B: Serialize>(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    pending_kind: PendingKind,
    kind: CommandKind,
    expected: Vec<Witness>,
    body: B,
) -> Result<CanonicalUuid, crate::client_action::CommandSendError> {
    let id = random_uuid().map_err(crate::client_action::CommandSendError::Rejected)?;
    send_typed_as_classified(
        principal,
        writer,
        actions,
        TrackedCommand {
            id,
            pending_kind,
            kind,
            expected,
        },
        body,
    )
    .await
}

async fn send_untracked_as<B: Serialize>(
    principal: &Principal,
    writer: &mut SocketWriter,
    id: CanonicalUuid,
    kind: CommandKind,
    expected: Vec<Witness>,
    body: B,
) -> Result<(), String> {
    let command = SteerPane::over(principal)
        .command(id, kind, expected, body)
        .map_err(|error| error.to_string())?;
    let bytes = wire::encode(&command).map_err(|error| error.to_string())?;
    writer
        .send(Message::Bytes(bytes))
        .await
        .map_err(websocket_error)
}

/// The envelope of one tracked command: the identity the queue pends it
/// under, how the queue classifies it, the typed command it sends, and the
/// witnesses the resident must confirm.
struct TrackedCommand {
    id: CanonicalUuid,
    pending_kind: PendingKind,
    kind: CommandKind,
    expected: Vec<Witness>,
}

async fn send_typed_as<B: Serialize>(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    tracked: TrackedCommand,
    body: B,
) -> Result<CanonicalUuid, String> {
    send_typed_as_classified(principal, writer, actions, tracked, body)
        .await
        .map_err(Into::into)
}

async fn send_typed_as_classified<B: Serialize>(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    tracked: TrackedCommand,
    body: B,
) -> Result<CanonicalUuid, crate::client_action::CommandSendError> {
    use crate::client_action::CommandSendError;

    let TrackedCommand {
        id,
        pending_kind,
        kind,
        expected,
    } = tracked;
    if actions.pending_contains(id) {
        return Err(CommandSendError::Rejected(format!(
            "command identity {id} is already pending"
        )));
    }
    let command = SteerPane::over(principal)
        .command(id, kind, expected, body)
        .map_err(|error| CommandSendError::Rejected(error.to_string()))?;
    let bytes =
        wire::encode(&command).map_err(|error| CommandSendError::Rejected(error.to_string()))?;
    actions
        .reserve_pending(id, pending_kind, bytes.len())
        .map_err(|error| CommandSendError::Rejected(error.to_string()))?;
    if let Err(error) = writer.send(Message::Bytes(bytes)).await {
        actions.cancel_pending(id);
        return Err(CommandSendError::Transport(websocket_error(error)));
    }
    Ok(id)
}

async fn send_subscription(
    principal: &Principal,
    data: &SurfaceData,
    writer: &mut SocketWriter,
) -> Result<(), String> {
    let (subjects, after) = {
        let data = data.read();
        let runtime = data
            .as_ref()
            .ok_or_else(|| "app runtime is absent".to_owned())?;
        (runtime.subjects(), runtime.resume())
    };
    send_stream_request(
        writer,
        &StreamRequest::Subscribe {
            v: WireVersion::CURRENT,
            id: random_uuid()?,
            epoch: principal.epoch(),
            subjects,
            after,
        },
    )
    .await?;
    Ok(())
}

async fn send_stream_directive(
    principal: &Principal,
    writer: &mut SocketWriter,
    directive: StreamDirective,
) -> Result<bool, String> {
    let request = match directive {
        StreamDirective::Continue { subject, cursor } => StreamRequest::Continue {
            v: WireVersion::CURRENT,
            id: random_uuid()?,
            epoch: principal.epoch(),
            subject,
            cursor,
        },
        StreamDirective::Reconnect => return Ok(true),
    };
    send_stream_request(writer, &request).await?;
    Ok(false)
}

async fn send_stream_request(
    writer: &mut SocketWriter,
    request: &StreamRequest,
) -> Result<(), String> {
    writer
        .send(Message::Bytes(
            wire::encode(request).map_err(|error| error.to_string())?,
        ))
        .await
        .map_err(websocket_error)
}

mod drafts;
mod refresh;
mod responses;
mod rooms;
use drafts::send_task_change;
use refresh::{send_after_frame, send_refresh};
use responses::handle_incoming;
