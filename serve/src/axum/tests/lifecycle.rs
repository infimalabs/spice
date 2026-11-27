//! App lifecycle, queue, and capability admission cases.

use super::*;

mod identity;

const LOOPBACK_AUTHORITY: &str = "127.0.0.1:48123";
const LOOPBACK_ORIGIN: &str = "http://127.0.0.1:48123";

#[tokio::test(flavor = "current_thread")]
async fn authenticated_session_routes_multiple_typed_commands_and_correlates_answers() {
    let app = TestApp::new();
    let capabilities = operator_capabilities();
    let (server, mut control) = Server::bind(generation(&app, capabilities))
        .await
        .expect("binds");
    let address = server.local_addr().expect("local address");
    let (shutdown, stopped) = oneshot::channel();
    let server_task = tokio::spawn(server.up_until(async move {
        let _ = stopped.await;
    }));
    let client = tokio::task::spawn_blocking(move || command_client(address));
    answer_commands(&mut control).await;
    client.await.expect("client finishes");
    let _ = shutdown.send(());
    fixture_completion(server_task)
        .await
        .expect("server task")
        .expect("server stops");
}

/// Capability retirement is its own session event, even without a Bus stream.
///
/// The first request remains held throughout. An unrelated binding is inserted
/// and retired before a sibling command crosses the same socket successfully,
/// proving capability events are isolated by exact binding. Retiring the
/// authenticating binding then closes the socket and drops the held response
/// receiver without manufacturing a correlated command response.
#[tokio::test(flavor = "current_thread")]
async fn exact_capability_retirement_cancels_a_held_command_only_socket() {
    let app = TestApp::new();
    let capabilities = operator_capabilities();
    let (server, mut control) = bind_test_server(epoch(), &app, capabilities.clone())
        .await
        .expect("server binds");
    let address = server.local_addr().expect("address");
    let (shutdown, stopped) = oneshot::channel();
    let server_task = tokio::spawn(server.up_until(async move {
        let _ = stopped.await;
    }));
    let (mut socket, _) = authenticate(address, operator(), epoch()).await;

    let held_id = identity(0xa3);
    send_recall(&mut socket, held_id, identity(0xa4)).await;
    let held = control.recv().await.expect("held request enters Control");
    assert_eq!(held.command().id(), held_id);

    let unrelated_capability = identity(0xa5);
    let unrelated_lane = identity(0xa6);
    let unrelated_actor = wire::Actor::Lane(unrelated_lane);
    capabilities
        .insert(unrelated_capability, unrelated_actor.clone())
        .expect("unrelated capability inserts");
    assert!(capabilities.remove(unrelated_capability, &unrelated_actor));

    let sibling_id = identity(0xa7);
    send_recall(&mut socket, sibling_id, identity(0xa8)).await;
    let sibling = fixture_completion(control.recv())
        .await
        .expect("unrelated retirement leaves the socket responsive");
    assert_eq!(sibling.command().id(), sibling_id);
    let sibling_sent = sibling
        .respond(wire::Outcome::Refusal(wire::Refusal::bare(
            wire::RefusalDiagnostic::new(
                "fixture::unrelated-capability-survived".to_owned(),
                None,
                None,
                None,
            ),
        )))
        .expect("sibling response receiver remains open");
    assert_eq!(response(&mut socket).await.id, sibling_id);
    sibling_sent.wait().await;

    assert!(capabilities.remove(operator(), &owner()));
    let replacement = next_frame(&mut socket).await;
    assert!(
        wire::decode::<wire::Response<serde_json::Value>>(&replacement).is_err(),
        "retirement must not manufacture a correlated command response"
    );
    assert!(matches!(
        wire::decode::<wire::StreamFrame>(&replacement).expect("replacement fault frame"),
        wire::StreamFrame::Fault { fault, id: None, .. }
            if fault.reason == "stream::capability-replaced"
    ));
    assert_socket_terminated(&mut socket).await;
    assert!(
        held.respond(wire::Outcome::Refusal(wire::Refusal::bare(
            wire::RefusalDiagnostic::new("fixture::too-late".to_owned(), None, None, None)
        )))
        .is_err(),
        "session retirement drops every unresolved response receiver"
    );

    shutdown.send(()).expect("shutdown signals");
    fixture_completion(server_task)
        .await
        .expect("server task joins")
        .expect("server stops");
}

/// A successful Dark response stays behind every earlier response on its
/// socket, even when resident test logic answers Dark first.
///
/// The response receipt is the production completion signal, so observing it
/// directly proves the write attempt has not happened; no elapsed-time proxy
/// stands in for the ordering fact.
#[tokio::test(flavor = "current_thread")]
async fn dark_drains_an_earlier_same_socket_response_before_closing() {
    let app = TestApp::new();
    let capabilities = operator_capabilities();
    let (server, mut control) = Server::bind_with_capacity(
        Binding::new(LOOPBACK_EPHEMERAL, Access::local()),
        generation(&app, capabilities),
        None,
        2,
    )
    .await
    .expect("binds");
    let address = server.local_addr().expect("local address");
    let (shutdown, stopped) = oneshot::channel();
    let server_task = tokio::spawn(server.up_until(async move {
        let _ = stopped.await;
    }));
    let (mut socket, _) = authenticate(address, operator(), epoch()).await;

    let first_id = identity(0xd5);
    send_change(&mut socket, first_id, epoch(), wire::ResidentState::Seated).await;
    let first = control.recv().await.expect("first request enters Control");

    let dark_id = identity(0xd6);
    send_change(&mut socket, dark_id, epoch(), wire::ResidentState::Dark).await;
    let dark = control.recv().await.expect("Dark request enters Control");
    assert_eq!(
        (first.command().id(), dark.command().id()),
        (first_id, dark_id)
    );
    let dark_receipt = dark
        .respond(state_reply(wire::ResidentState::Dark))
        .expect("Dark response receiver remains open");
    let (watching, watched) = oneshot::channel();
    let (attempted, mut observed) = oneshot::channel();
    let attempted_task = tokio::spawn(async move {
        let _ = watching.send(());
        dark_receipt.wait().await;
        let _ = attempted.send(());
    });
    watched.await.expect("receipt observer starts");
    assert!(matches!(
        observed.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));

    let first_receipt = first
        .respond(state_reply(wire::ResidentState::Seated))
        .expect("first response receiver remains open");
    assert_eq!(response(&mut socket).await.id, first_id);
    first_receipt.wait().await;
    assert_eq!(response(&mut socket).await.id, dark_id);
    observed.await.expect("Dark write attempt is observed");
    attempted_task.await.expect("receipt observer joins");
    assert_socket_terminated(&mut socket).await;

    shutdown.send(()).expect("server stops");
    fixture_completion(server_task)
        .await
        .expect("server task")
        .expect("server stops");
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_closes_partial_upgrade_pre_hello_and_active_sockets() {
    let app = TestApp::new();
    let capabilities = operator_capabilities();
    let (server, _control) = Server::bind(generation(&app, capabilities))
        .await
        .expect("binds");
    let state = Arc::clone(&server.state);
    let address = server.local_addr().expect("local address");
    let (shutdown, stopped) = oneshot::channel();
    let server_task = tokio::spawn(server.up_until(async move {
        let _ = stopped.await;
    }));

    let mut partial = tokio::net::TcpStream::connect(address)
        .await
        .expect("partial upgrade connects");
    partial
        .write_all(format!("GET /socket HTTP/1.1\r\nHost: {address}\r\n").as_bytes())
        .await
        .expect("partial upgrade writes");
    let mut pre_hello = websocket(address).await;
    let (mut active, accepted) = authenticate(address, operator(), epoch()).await;
    assert_eq!(accepted.actor, owner());

    shutdown.send(()).expect("shutdown requests");
    // Graceful shutdown cannot wait on accepted sockets.
    fixture_completion(server_task)
        .await
        .expect("server task")
        .expect("server stops");
    assert!(*state.closed.subscribe().borrow());
    assert!(state.connections.is_cancelled());

    let mut remaining = Vec::new();
    fixture_completion(partial.read_to_end(&mut remaining))
        .await
        .expect("partial upgrade reads to closure");
    assert_socket_terminated(&mut pre_hello).await;
    assert_socket_terminated(&mut active).await;
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_forces_transport_closed_when_a_session_cannot_finish_draining() {
    assert_eq!(SESSION_DRAIN_GRACE, Duration::from_secs(1));
    let app = TestApp::new();
    let capabilities = operator_capabilities();
    let (server, _control) = Server::bind(generation(&app, capabilities))
        .await
        .expect("binds");
    let state = Arc::clone(&server.state);
    let session = SessionGuard::enter(&state);

    // The session never drains, so only the finite grace ends the close.
    fixture_completion(state.close_with_grace(Duration::from_millis(10))).await;
    assert!(state.connections.is_cancelled());
    assert_eq!(state.sessions.load(Ordering::Acquire), 1);

    drop(session);
    assert_eq!(state.sessions.load(Ordering::Acquire), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn operator_lane_replacement_and_reconnect_cross_the_live_door() {
    let app = TestApp::new();
    let lane = identity(0xd0);
    let lane_capability = identity(0xd1);
    let capabilities = operator_capabilities();
    capabilities
        .insert(lane_capability, wire::Actor::Lane(lane))
        .expect("lane capability");
    let (server, mut control) = Server::bind(generation(&app, capabilities))
        .await
        .expect("binds");
    let address = server.local_addr().expect("local address");
    let (shutdown, stopped) = oneshot::channel();
    let server_task = tokio::spawn(server.up_until(async move {
        let _ = stopped.await;
    }));

    let (mut lane_socket, accepted) = authenticate(address, lane_capability, epoch()).await;
    assert_eq!(accepted.actor, wire::Actor::Lane(lane));
    let lane_request = identity(0xd2);
    send_change(
        &mut lane_socket,
        lane_request,
        epoch(),
        wire::ResidentState::Dark,
    )
    .await;
    let lane_response = response(&mut lane_socket).await;
    assert_eq!(
        (lane_response.id, lane_response.epoch),
        (lane_request, epoch())
    );
    assert!(matches!(
        lane_response.outcome,
        wire::Outcome::Refusal(ref refusal)
            if refusal.reason() == "command::authority"
    ));
    assert!(control.is_empty());

    let (mut operator_socket, accepted) = authenticate(address, operator(), epoch()).await;
    assert_eq!(accepted.actor, owner());
    let operator_request = identity(0xd3);
    send_change(
        &mut operator_socket,
        operator_request,
        epoch(),
        wire::ResidentState::Seated,
    )
    .await;
    let request = control.recv().await.expect("operator request enters queue");
    assert_eq!(request.command().id(), operator_request);
    request
        .respond(state_reply(wire::ResidentState::Seated))
        .expect("operator awaits response");
    let operator_response = response(&mut operator_socket).await;
    assert_eq!(operator_response.id, operator_request);

    shutdown.send(()).expect("first generation stops");
    fixture_completion(server_task)
        .await
        .expect("server task")
        .expect("server stops");

    let replacement_epoch = identity(0xd4);
    let replacement = Capabilities::default();
    replacement
        .insert(operator(), wire::Actor::Lane(lane))
        .expect("replacement generation capability");
    let (server, _control) = bind_test_server(replacement_epoch, &app, replacement)
        .await
        .expect("replacement binds");
    let address = server.local_addr().expect("replacement address");
    let (shutdown, stopped) = oneshot::channel();
    let server_task = tokio::spawn(server.up_until(async move {
        let _ = stopped.await;
    }));

    let mut stale = websocket(address).await;
    send_client(
        &mut stale,
        &wire::Hello {
            wire: wire::WireVersion::CURRENT,
            capability: operator(),
            epoch: epoch(),
        },
    )
    .await;
    let stale_fault: wire::Fault =
        wire::decode(&next_frame(&mut stale).await).expect("stale fault");
    assert_eq!(stale_fault.reason, "hello::epoch-mismatch");

    let mut removed = websocket(address).await;
    send_client(
        &mut removed,
        &wire::Hello {
            wire: wire::WireVersion::CURRENT,
            capability: lane_capability,
            epoch: replacement_epoch,
        },
    )
    .await;
    let removed_refusal: wire::Refusal =
        wire::decode(&next_frame(&mut removed).await).expect("removed capability refusal");
    assert_eq!(removed_refusal.reason(), "hello::capability-unknown");

    let (mut current, accepted) = authenticate(address, operator(), replacement_epoch).await;
    assert_eq!(accepted.actor, wire::Actor::Lane(lane));
    shutdown.send(()).expect("replacement stops");
    fixture_completion(server_task)
        .await
        .expect("server task")
        .expect("server stops");
    assert_socket_terminated(&mut current).await;
}

#[tokio::test(flavor = "current_thread")]
async fn saturated_queue_orders_concurrent_work_before_dark_and_refuses_later_work() {
    let app = TestApp::new();
    let capabilities = operator_capabilities();
    let (server, mut control) = Server::bind_with_capacity(
        Binding::new(LOOPBACK_EPHEMERAL, Access::local()),
        generation(&app, capabilities),
        None,
        1,
    )
    .await
    .expect("binds");
    let state = Arc::clone(&server.state);
    let mut observed = state.admission_observed.subscribe();
    let address = server.local_addr().expect("local address");
    let (shutdown, stopped) = oneshot::channel();
    let server_task = tokio::spawn(server.up_until(async move {
        let _ = stopped.await;
    }));
    let (mut first, _) = authenticate(address, operator(), epoch()).await;
    let (mut blocked, _) = authenticate(address, operator(), epoch()).await;
    let (mut dark, _) = authenticate(address, operator(), epoch()).await;

    let first_id = identity(0xe0);
    send_change(&mut first, first_id, epoch(), wire::ResidentState::Seated).await;
    wait_for_admission(&mut observed, AdmissionObservation::Queued(first_id)).await;

    let blocked_id = identity(0xe1);
    send_change(
        &mut blocked,
        blocked_id,
        epoch(),
        wire::ResidentState::Seated,
    )
    .await;
    wait_for_admission(&mut observed, AdmissionObservation::Reserving(blocked_id)).await;

    let dark_id = identity(0xe2);
    send_change(&mut dark, dark_id, epoch(), wire::ResidentState::Dark).await;

    let request = control.recv().await.expect("first request");
    assert_eq!(request.command().id(), first_id);
    request
        .respond(state_reply(wire::ResidentState::Seated))
        .expect("first client waits");
    assert_eq!(response(&mut first).await.id, first_id);

    let request = control.recv().await.expect("blocked request");
    assert_eq!(request.command().id(), blocked_id);
    request
        .respond(state_reply(wire::ResidentState::Seated))
        .expect("blocked client waits");
    assert_eq!(response(&mut blocked).await.id, blocked_id);

    wait_for_admission(&mut observed, AdmissionObservation::Queued(dark_id)).await;
    assert_eq!(*state.admission.lock().await, Admission::Dark);

    let refused_id = identity(0xe3);
    send_change(&mut first, refused_id, epoch(), wire::ResidentState::Seated).await;
    let refused = response(&mut first).await;
    assert_eq!((refused.id, refused.epoch), (refused_id, epoch()));
    assert!(matches!(
        refused.outcome,
        wire::Outcome::Refusal(ref refusal)
            if refusal.reason() == "resident::not-admitting"
    ));

    let request = control.recv().await.expect("Dark request");
    assert_eq!(request.command().id(), dark_id);
    request
        .respond(state_reply(wire::ResidentState::Dark))
        .expect("Dark client waits");
    assert_eq!(response(&mut dark).await.id, dark_id);
    assert!(control.is_empty());

    shutdown.send(()).expect("server stops");
    fixture_completion(server_task)
        .await
        .expect("server task")
        .expect("server stops");
}

#[tokio::test(flavor = "current_thread")]
async fn a_session_waiting_for_a_slot_still_delivers_the_answer_the_resident_awaits() {
    let app = TestApp::new();
    let capabilities = operator_capabilities();
    let (server, mut control) = Server::bind_with_capacity(
        Binding::new(LOOPBACK_EPHEMERAL, Access::local()),
        generation(&app, capabilities),
        None,
        1,
    )
    .await
    .expect("binds");
    let mut observed = server.state.admission_observed.subscribe();
    let address = server.local_addr().expect("local address");
    let (shutdown, stopped) = oneshot::channel();
    let server_task = tokio::spawn(server.up_until(async move {
        let _ = stopped.await;
    }));
    let (mut pipelining, _) = authenticate(address, operator(), epoch()).await;
    let (mut sibling, _) = authenticate(address, operator(), epoch()).await;
    let seated = wire::ResidentState::Seated;

    let answered_id = identity(0xf0);
    send_change(&mut pipelining, answered_id, epoch(), seated).await;
    wait_for_admission(&mut observed, AdmissionObservation::Queued(answered_id)).await;
    let answered = control.recv().await.expect("first request");
    let sibling_id = identity(0xf1);
    send_change(&mut sibling, sibling_id, epoch(), seated).await;
    wait_for_admission(&mut observed, AdmissionObservation::Queued(sibling_id)).await;
    let waiting_id = identity(0xf2);
    send_change(&mut pipelining, waiting_id, epoch(), seated).await;
    wait_for_admission(&mut observed, AdmissionObservation::Reserving(waiting_id)).await;

    let receipt = answered
        .respond(state_reply(seated))
        .expect("pipelining client waits");
    // A session waiting for a slot still writes its earlier answer.
    fixture_completion(receipt.wait()).await;
    assert_eq!(response(&mut pipelining).await.id, answered_id);
    for (id, socket) in [(sibling_id, &mut sibling), (waiting_id, &mut pipelining)] {
        let request = control.recv().await.expect("queued request");
        assert_eq!(request.command().id(), id);
        request
            .respond(state_reply(seated))
            .expect("client waits")
            .wait()
            .await;
        assert_eq!(response(socket).await.id, id);
    }

    shutdown.send(()).expect("server stops");
    fixture_completion(server_task)
        .await
        .expect("server task")
        .expect("server stops");
}

#[tokio::test(flavor = "current_thread")]
async fn a_saturated_ordinary_class_cannot_block_an_authenticated_demand() {
    let app = TestApp::new();
    let capabilities = operator_capabilities();
    let (server, mut control) = Server::bind_with_capacity(
        Binding::new(LOOPBACK_EPHEMERAL, Access::local()),
        generation(&app, capabilities),
        None,
        1,
    )
    .await
    .expect("binds");
    let state = Arc::clone(&server.state);
    let mut observed = state.admission_observed.subscribe();
    let address = server.local_addr().expect("local address");
    let (shutdown, stopped) = oneshot::channel();
    let server_task = tokio::spawn(server.up_until(async move {
        let _ = stopped.await;
    }));
    let (mut first, _) = authenticate(address, operator(), epoch()).await;
    let (mut blocked, _) = authenticate(address, operator(), epoch()).await;
    let (mut demand, _) = authenticate(address, operator(), epoch()).await;

    let first_id = identity(0xb0);
    send_change(&mut first, first_id, epoch(), wire::ResidentState::Seated).await;
    wait_for_admission(&mut observed, AdmissionObservation::Queued(first_id)).await;

    let blocked_id = identity(0xb1);
    send_change(
        &mut blocked,
        blocked_id,
        epoch(),
        wire::ResidentState::Seated,
    )
    .await;
    wait_for_admission(&mut observed, AdmissionObservation::Reserving(blocked_id)).await;

    let demand_id = identity(0xb2);
    send_team_configuration(&mut demand, demand_id).await;
    wait_for_admission(&mut observed, AdmissionObservation::Queued(demand_id)).await;

    let prioritized = control.recv().await.expect("demand enters Control");
    assert_eq!(prioritized.command().id(), demand_id);
    refuse_and_observe(prioritized, &mut demand, demand_id).await;

    let first_request = control.recv().await.expect("first ordinary request");
    assert_eq!(first_request.command().id(), first_id);
    refuse_and_observe(first_request, &mut first, first_id).await;
    wait_for_admission(&mut observed, AdmissionObservation::Queued(blocked_id)).await;

    let blocked_request = control.recv().await.expect("blocked ordinary request");
    assert_eq!(blocked_request.command().id(), blocked_id);
    refuse_and_observe(blocked_request, &mut blocked, blocked_id).await;

    shutdown.send(()).expect("server stops");
    fixture_completion(server_task)
        .await
        .expect("server task")
        .expect("server stops");
}

#[tokio::test(flavor = "current_thread")]
async fn priority_is_three_to_one_and_dark_waits_for_both_classes() {
    let app = TestApp::new();
    let capabilities = operator_capabilities();
    let (server, mut control) = Server::bind_with_capacity(
        Binding::new(LOOPBACK_EPHEMERAL, Access::local()),
        generation(&app, capabilities),
        None,
        8,
    )
    .await
    .expect("binds");
    let state = Arc::clone(&server.state);
    let mut observed = state.admission_observed.subscribe();
    let address = server.local_addr().expect("local address");
    let (shutdown, stopped) = oneshot::channel();
    let server_task = tokio::spawn(server.up_until(async move {
        let _ = stopped.await;
    }));
    let (mut ordinary, _) = authenticate(address, operator(), epoch()).await;
    let (mut dark, _) = authenticate(address, operator(), epoch()).await;
    let mut demands = Vec::new();
    for _ in 0..4 {
        demands.push(authenticate(address, operator(), epoch()).await.0);
    }

    let ordinary_id = identity(0xc0);
    send_change(
        &mut ordinary,
        ordinary_id,
        epoch(),
        wire::ResidentState::Seated,
    )
    .await;
    wait_for_admission(&mut observed, AdmissionObservation::Queued(ordinary_id)).await;

    let demand_ids = [
        identity(0xc1),
        identity(0xc2),
        identity(0xc3),
        identity(0xc4),
    ];
    for (socket, id) in demands.iter_mut().zip(demand_ids) {
        send_team_configuration(socket, id).await;
        wait_for_admission(&mut observed, AdmissionObservation::Queued(id)).await;
    }

    let dark_id = identity(0xc5);
    send_change(&mut dark, dark_id, epoch(), wire::ResidentState::Dark).await;
    wait_for_admission(&mut observed, AdmissionObservation::Queued(dark_id)).await;
    assert_eq!(*state.admission.lock().await, Admission::Dark);

    let refused_id = identity(0xc6);
    send_change(
        &mut ordinary,
        refused_id,
        epoch(),
        wire::ResidentState::Seated,
    )
    .await;
    let refused = response(&mut ordinary).await;
    assert_eq!(refused.id, refused_id);
    assert!(matches!(
        refused.outcome,
        wire::Outcome::Refusal(ref refusal)
            if refusal.reason() == "resident::not-admitting"
    ));

    for (socket, id) in demands.iter_mut().zip(demand_ids).take(3) {
        let request = control.recv().await.expect("prioritized demand");
        assert_eq!(request.command().id(), id);
        refuse_and_observe(request, socket, id).await;
    }

    let ordinary_request = control.recv().await.expect("bounded ordinary turn");
    assert_eq!(ordinary_request.command().id(), ordinary_id);
    refuse_and_observe(ordinary_request, &mut ordinary, ordinary_id).await;

    let last_demand = control.recv().await.expect("pre-Dark demand");
    assert_eq!(last_demand.command().id(), demand_ids[3]);
    refuse_and_observe(last_demand, &mut demands[3], demand_ids[3]).await;

    let dark_request = control.recv().await.expect("Dark barrier");
    assert_eq!(dark_request.command().id(), dark_id);
    let dark_receipt = dark_request
        .respond(state_reply(wire::ResidentState::Dark))
        .expect("Dark client waits");
    assert_eq!(response(&mut dark).await.id, dark_id);
    dark_receipt.wait().await;
    assert!(control.is_empty());

    shutdown.send(()).expect("server stops");
    fixture_completion(server_task)
        .await
        .expect("server task")
        .expect("server stops");
}

async fn send_team_configuration(socket: &mut ClientSocket, id: wire::CanonicalUuid) {
    send_client(
        socket,
        &wire::Command {
            v: wire::WireVersion::CURRENT,
            id,
            epoch: epoch(),
            kind: wire::CommandKind::TeamConfigure,
            expected: Vec::new(),
            body: wire::intent::TeamConfigure {
                topology_revision: 1,
                team: identity(0xb3),
                new_team: false,
                member: None,
                drive: Some(false),
                drain: None,
                name: None,
                clear_name: false,
                fast_mode: None,
            },
        },
    )
    .await;
}

async fn refuse_and_observe(
    request: ControlRequest,
    socket: &mut ClientSocket,
    id: wire::CanonicalUuid,
) {
    let receipt = request
        .respond(wire::Outcome::Refusal(wire::Refusal::bare(
            wire::RefusalDiagnostic::new(
                "fixture::scheduler-observed".to_owned(),
                None,
                None,
                None,
            ),
        )))
        .expect("client waits for response");
    assert_eq!(response(socket).await.id, id);
    receipt.wait().await;
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_interrupts_a_queue_reservation_blocked_by_saturation() {
    let app = TestApp::new();
    let capabilities = operator_capabilities();
    let (server, _control) = Server::bind_with_capacity(
        Binding::new(LOOPBACK_EPHEMERAL, Access::local()),
        generation(&app, capabilities),
        None,
        1,
    )
    .await
    .expect("binds");
    let state = Arc::clone(&server.state);
    let mut observed = state.admission_observed.subscribe();
    let address = server.local_addr().expect("local address");
    let (shutdown, stopped) = oneshot::channel();
    let server_task = tokio::spawn(server.up_until(async move {
        let _ = stopped.await;
    }));
    let (mut queued, _) = authenticate(address, operator(), epoch()).await;
    let (mut blocked, _) = authenticate(address, operator(), epoch()).await;

    let queued_id = identity(0xe4);
    send_change(&mut queued, queued_id, epoch(), wire::ResidentState::Seated).await;
    wait_for_admission(&mut observed, AdmissionObservation::Queued(queued_id)).await;

    let blocked_id = identity(0xe5);
    send_change(
        &mut blocked,
        blocked_id,
        epoch(),
        wire::ResidentState::Seated,
    )
    .await;
    wait_for_admission(&mut observed, AdmissionObservation::Reserving(blocked_id)).await;

    shutdown.send(()).expect("shutdown requests");
    fixture_completion(server_task)
        .await
        .expect("server task")
        .expect("server stops");
    assert_socket_terminated(&mut queued).await;
    assert_socket_terminated(&mut blocked).await;
}

fn command_client(address: SocketAddr) {
    use tungstenite::client::IntoClientRequest;
    use tungstenite::http::header::ORIGIN;

    let stream = std::net::TcpStream::connect(address).expect("connects");
    let mut request = format!("ws://{address}/socket")
        .into_client_request()
        .expect("request");
    request
        .headers_mut()
        .insert(ORIGIN, format!("http://{address}").parse().expect("origin"));
    let (mut socket, _) = tungstenite::client(request, stream).expect("opens");
    let hello = wire::Hello {
        wire: wire::WireVersion::CURRENT,
        capability: operator(),
        epoch: epoch(),
    };
    socket
        .send(tungstenite::Message::Binary(
            wire::encode(&hello).expect("hello encodes").into(),
        ))
        .expect("hello sends");
    let opening = socket.read().expect("acceptance").into_data();
    let accepted: wire::Accepted = wire::decode(&opening).expect("accepted frame");
    assert_eq!(accepted.actor, owner());

    for (id, to, succeeds) in command_cases() {
        let command = wire::Command {
            v: wire::WireVersion::CURRENT,
            id,
            epoch: epoch(),
            kind: wire::CommandKind::UpChange,
            expected: vec![wire::Witness::Transition {
                from: wire::ResidentState::Seated,
                to,
            }],
            body: wire::intent::UpChange { state: to },
        };
        socket
            .send(tungstenite::Message::Binary(
                wire::encode(&command).expect("command encodes").into(),
            ))
            .expect("command sends");
        let frame = socket.read().expect("response").into_data();
        let response: wire::Response<serde_json::Value> =
            wire::decode(&frame).expect("response decodes");
        assert_eq!((response.id, response.epoch), (id, epoch()));
        assert_eq!(
            matches!(response.outcome, wire::Outcome::Reply(_)),
            succeeds
        );
    }
}

fn command_cases() -> [(wire::CanonicalUuid, wire::ResidentState, bool); 3] {
    [
        (
            uuid("018f82d4-6f59-7d06-92e3-bce9131163c6"),
            wire::ResidentState::Dark,
            false,
        ),
        (
            uuid("018f82d4-6f59-7d06-92e3-bce9131163c7"),
            wire::ResidentState::Seated,
            true,
        ),
        (
            uuid("018f82d4-6f59-7d06-92e3-bce9131163c8"),
            wire::ResidentState::Dark,
            true,
        ),
    ]
}

async fn answer_commands(control: &mut Control) {
    for (index, (_, to, _)) in command_cases().into_iter().enumerate() {
        let request = control.recv().await.expect("resident request");
        assert_eq!(request.command().actor(), &owner());
        assert!(matches!(
            request.command().body(),
            wire::intent::Intent::UpChange(wire::intent::UpChange { state }) if *state == to
        ));
        let outcome = if index == 0 {
            transition_refusal()
        } else {
            state_reply(to)
        };
        request.respond(outcome).expect("client waits for answer");
    }
}

fn transition_refusal() -> wire::Outcome<wire::intent::CommandAnswer> {
    wire::Outcome::Refusal(wire::Refusal::bare(wire::RefusalDiagnostic::new(
        "resident::transition-moved".to_owned(),
        Some("transition".to_owned()),
        Some("seated->dark".to_owned()),
        Some("quiescing->dark".to_owned()),
    )))
}

fn state_reply(to: wire::ResidentState) -> wire::Outcome<wire::intent::CommandAnswer> {
    wire::Outcome::Reply(wire::Reply {
        position: wire::ReplyPosition::Direct {},
        body: wire::intent::CommandAnswer::State(wire::intent::StateAnswer {
            from: wire::ResidentState::Seated,
            to,
        }),
    })
}

#[test]
fn duplicate_capability_refuses_without_replacing_its_principal() {
    let capabilities = operator_capabilities();
    let lane = uuid("018f82d4-6f59-7d06-92e3-bce9131163c5");

    assert_eq!(
        capabilities.insert(operator(), wire::Actor::Lane(lane)),
        Err(CapabilityError::Duplicate)
    );
    let accepted = capabilities
        .admit(
            &wire::Hello {
                wire: wire::WireVersion::CURRENT,
                capability: operator(),
                epoch: epoch(),
            },
            epoch(),
            &"1.2.3".parse().expect("fixture release"),
        )
        .expect("original registration survives");
    assert_eq!(accepted.actor, owner());
    assert_eq!(accepted.release.as_str(), "1.2.3");
}

#[test]
fn unknown_capability_refuses_without_echoing_the_credential() {
    let capabilities = Capabilities::default();
    let unknown = operator();
    let hello = wire::Hello {
        wire: wire::WireVersion::CURRENT,
        capability: unknown,
        epoch: epoch(),
    };

    let refusal = capabilities
        .admit(&hello, epoch(), &"1.2.3".parse().expect("fixture release"))
        .expect_err("unknown capability refuses");

    assert!(matches!(
        refusal,
        OpeningError::Refused(ref refusal)
            if refusal.reason() == "hello::capability-unknown"
    ));
    assert!(!format!("{refusal:?}").contains(&unknown.to_string()));
}

#[test]
fn host_and_origin_must_name_the_published_endpoint() {
    let app = TestApp::new();
    let state = SurfaceState {
        authority: Authority::Only(LOOPBACK_AUTHORITY.to_owned()),
        access: Access::local(),
        origin: LOOPBACK_ORIGIN.to_owned(),
        epoch: epoch(),
        capabilities: Capabilities::default(),
        version: app.version(),
        app: app.source(),
        history: super::super::history::HistoryLimits::new(),
        admission: Mutex::new(Admission::Open),
        admission_changed: watch::channel(Admission::Open).0,
        commands: control::channel(1).0,
        stream: None,
        closed: watch::channel(false).0,
        connections: CancellationToken::new(),
        sessions: AtomicUsize::new(0),
        sessions_closed: Notify::new(),
        admission_observed: watch::channel(None).0,
    };
    let mut headers = HeaderMap::new();
    headers.insert(HOST, HeaderValue::from_static("attacker.example"));
    headers.insert(ORIGIN, HeaderValue::from_static("https://attacker.example"));
    assert!(admit_headers(&state, &headers, true).is_err());

    headers.insert(HOST, HeaderValue::from_static(LOOPBACK_AUTHORITY));
    headers.insert(ORIGIN, HeaderValue::from_static(LOOPBACK_ORIGIN));
    assert_eq!(
        admit_headers(&state, &headers, true),
        Ok(LOOPBACK_AUTHORITY)
    );

    // A specific bind admits its own name alone, so a second address of this
    // machine is refused even though the surface is listening on this port.
    headers.insert(HOST, HeaderValue::from_static("10.0.0.5:48123"));
    headers.insert(ORIGIN, HeaderValue::from_static("http://10.0.0.5:48123"));
    assert!(admit_headers(&state, &headers, true).is_err());
}
