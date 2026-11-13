//! Listener address, wildcard-origin, and remote-access cases.

use super::*;

const REMOTE_TOKEN: &str = "correct-horse-battery-staple";

#[tokio::test(flavor = "current_thread")]
async fn server_refuses_an_identity_for_another_app_generation_before_binding() {
    let app = TestApp::new();
    let mut version = app.version();
    version.app = wire::Digest::compute(wire::Domain::DistArtifact, b"another generation");

    let error = Server::bind(Generation::new(
        epoch(),
        Capabilities::default(),
        version,
        app.source(),
    ))
    .await
    .expect_err("mismatched app identity refuses");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[tokio::test(flavor = "current_thread")]
async fn server_binds_ipv4_loopback_and_publishes_that_endpoint() {
    let app = TestApp::new();
    let capabilities = operator_capabilities();
    let (server, _control) = Server::bind(generation(&app, capabilities))
        .await
        .expect("binds");
    let address = server.local_addr().expect("local address");
    let discovery = server.discovery(operator()).expect("operator discovery");

    assert!(address.ip().is_loopback());
    assert_eq!(discovery.endpoint, format!("http://{address}"));
    assert_eq!(discovery.epoch, epoch());
}

/// A surface bound off loopback answers to the address the operator dialed.
///
/// This is the remote case the whole flag exists for: the operator's browser is
/// on another machine, so the `Host` it sends is a routable address this
/// process never chose and cannot enumerate. The wildcard bind therefore admits
/// it by port, hands the bootstrap that same address back so the socket the
/// page opens reaches here rather than the browser's own loopback, and still
/// refuses a request carrying another site's origin.
#[tokio::test(flavor = "current_thread")]
async fn a_wildcard_bind_answers_the_dialed_address_and_refuses_a_foreign_origin() {
    let app = TestApp::new();
    let capabilities = operator_capabilities();
    let (server, _control) = Server::bind_with_capacity(
        Binding::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            Access::authenticated(REMOTE_TOKEN.to_owned()),
        ),
        generation(&app, capabilities),
        None,
        1,
    )
    .await
    .expect("binds every address");
    let address = server.local_addr().expect("local address");
    assert!(
        !address.ip().is_loopback(),
        "a wildcard bind is not loopback"
    );
    assert!(
        !format!("{:?}", server.state.access).contains("correct-horse"),
        "the access contract never debugs its credential"
    );

    // Nothing routable can be published for a bind with no address of its own,
    // so the local clients that read this document keep a dialable one.
    let discovery = server.discovery(operator()).expect("operator discovery");
    assert_eq!(
        discovery.endpoint,
        format!("http://127.0.0.1:{}", address.port())
    );

    let dial = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), address.port());
    let task = tokio::spawn(server.up());
    let remote = format!("10.0.0.5:{}", address.port());

    let unauthenticated = request(dial, &remote, "/bootstrap").await;
    assert!(unauthenticated.starts_with(b"HTTP/1.1 401 Unauthorized\r\n"));
    let wrong = request(dial, &remote, "/bootstrap?auth=wrong-token-value").await;
    assert!(wrong.starts_with(b"HTTP/1.1 401 Unauthorized\r\n"));

    let opening = format!("http://{remote}#{REMOTE_TOKEN}");
    let (origin, fragment) = opening
        .split_once('#')
        .expect("opening carries auth fragment");
    let opening_host = origin
        .strip_prefix("http://")
        .expect("opening has HTTP origin");
    assert_eq!(opening_host, remote);
    assert!(!origin.contains(REMOTE_TOKEN));
    let admitted = request(dial, opening_host, &format!("/bootstrap?auth={fragment}")).await;
    assert!(admitted.starts_with(b"HTTP/1.1 200 OK\r\n"));
    let bootstrap: wire::AppBootstrap =
        wire::decode(response_body(&admitted)).expect("bootstrap decodes");
    assert_eq!(bootstrap.endpoint, format!("http://{remote}"));

    let local_origin = format!("http://{remote}");
    let rejected = remote_websocket(dial, &remote, &local_origin, None)
        .await
        .expect_err("an unauthenticated remote socket is refused");
    assert!(
        matches!(&rejected, tokio_tungstenite::tungstenite::Error::Http(response)
            if response.status() == StatusCode::UNAUTHORIZED),
        "remote socket refusal is authentication-specific: {rejected:?}"
    );
    let capability = bootstrap.capability.to_string();
    let mut socket = remote_websocket(dial, &remote, &local_origin, Some(&capability))
        .await
        .expect("the bootstrapped capability authenticates the remote socket");
    send_client(
        &mut socket,
        &wire::Hello {
            wire: wire::WireVersion::CURRENT,
            capability: bootstrap.capability,
            epoch: bootstrap.epoch,
        },
    )
    .await;
    let _: wire::Accepted = wire::decode(&next_frame(&mut socket).await)
        .expect("the authenticated VPN workflow opens one current session");

    // The port is all a wildcard bind can check, so a host naming another one
    // is not this surface however it was routed here.
    let elsewhere = request(
        dial,
        "10.0.0.5:1",
        "/bootstrap?auth=correct-horse-battery-staple",
    )
    .await;
    assert!(elsewhere.starts_with(b"HTTP/1.1 403 Forbidden\r\n"));

    // Same host, another site's origin: the check that survives the wildcard.
    let foreign = remote_websocket(dial, &remote, "https://attacker.example", Some(&capability))
        .await
        .expect_err("a foreign origin is refused");
    assert!(
        matches!(&foreign, tokio_tungstenite::tungstenite::Error::Http(response)
            if response.status() == StatusCode::FORBIDDEN),
        "foreign-origin refusal is origin-specific: {foreign:?}"
    );

    task.abort();
}

#[test]
fn the_wasm_query_separator_is_only_admitted_when_empty() {
    let serialized = format!("auth={REMOTE_TOKEN}&");
    assert_eq!(query_secret(Some(&serialized)), Some(REMOTE_TOKEN));
    let ambiguous = format!("auth={REMOTE_TOKEN}&other=value");
    assert_eq!(query_secret(Some(&ambiguous)), None);
}

#[tokio::test(flavor = "current_thread")]
async fn a_non_loopback_listener_refuses_unauthenticated_access_before_bind() {
    let app = TestApp::new();
    let unavailable: SocketAddr = "192.0.2.1:9".parse().expect("documentation address");
    let error = Server::bind_with_capacity(
        Binding::new(unavailable, Access::local()),
        generation(&app, Capabilities::default()),
        None,
        1,
    )
    .await
    .expect_err("remote unauthenticated access is refused before the OS bind");
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
}

/// IPv6 loopback is loopback, and one definition has to say so at both gates.
///
/// The resident decides consent and token requirements with the standard
/// `IpAddr::is_loopback`, which is true for `::1`, so `spice up --host ::1`
/// arrives here carrying [`Access::local`]. A second, narrower definition on
/// this side would refuse the very address the resident just admitted, and the
/// operator would be told their machine-local bind is remote.
///
/// The wildcards are asserted in the same test because the positive alone
/// cannot fail a predicate that simply answers loopback to everything, and
/// they are two families rather than two spellings of one: a rule that reads
/// only `Ipv4Addr` would still admit `::`.
#[tokio::test(flavor = "current_thread")]
async fn ipv6_loopback_binds_unauthenticated_while_both_wildcards_refuse() {
    let app = TestApp::new();
    let (server, _control) = Server::bind_with_capacity(
        Binding::new(
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0),
            Access::local(),
        ),
        generation(&app, Capabilities::default()),
        None,
        1,
    )
    .await
    .expect("IPv6 loopback is reachable from this machine alone");
    let address = server.local_addr().expect("local address");
    assert!(address.ip().is_loopback(), "{address} is loopback");

    for wildcard in [
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    ] {
        let error = Server::bind_with_capacity(
            Binding::new(SocketAddr::new(wildcard, 0), Access::local()),
            generation(&app, Capabilities::default()),
            None,
            1,
        )
        .await
        .expect_err("a wildcard bind is not loopback");
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::PermissionDenied,
            "{wildcard} bound without authentication"
        );
    }
}
