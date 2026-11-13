//! Bootstrap product and build identity at the pre-socket boundary.

use super::*;

#[tokio::test(flavor = "current_thread")]
async fn bootstrap_names_the_exact_operator_generation_and_is_not_cached() {
    let app = TestApp::new();
    let capabilities = operator_capabilities();
    let (server, _control) = Server::bind(generation(&app, capabilities))
        .await
        .expect("binds");
    let address = server.local_addr().expect("local address");
    let task = tokio::spawn(server.up());

    let response = request(address, &address.to_string(), "/bootstrap").await;
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("response carries headers")
        + 4;
    assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
    assert!(
        response[..split]
            .windows(b"cache-control: no-store".len())
            .any(|window| window == b"cache-control: no-store")
    );
    let expected_version = format!(
        "{}: {}\r\n",
        wire::hello::APP_PROTOCOL_VERSION_HEADER,
        wire::AppProtocolVersion::CURRENT
    );
    assert!(
        response[..split]
            .windows(expected_version.len())
            .any(|window| window == expected_version.as_bytes()),
        "bootstrap response omitted {expected_version:?}"
    );
    let expected_generation = format!(
        "{}: {FIXTURE_GENERATION}\r\n",
        wire::hello::APP_GENERATION_HEADER,
    );
    assert!(
        response[..split]
            .windows(expected_generation.len())
            .any(|window| window == expected_generation.as_bytes()),
        "bootstrap response omitted {expected_generation:?}"
    );
    let discovery: wire::AppBootstrap =
        wire::decode(&response[split..]).expect("bootstrap decodes");
    assert_eq!(discovery.endpoint, format!("http://{address}"));
    assert_eq!(discovery.capability, operator());
    assert_eq!(discovery.epoch, epoch());
    assert_eq!(discovery.version, app.version());
    assert_eq!(
        discovery.version.app_protocol,
        wire::AppProtocolVersion::CURRENT
    );
    assert_eq!(discovery.version.app.as_str(), FIXTURE_GENERATION);

    task.abort();
    assert!(task.await.expect_err("aborted server task").is_cancelled());
}
