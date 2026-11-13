use super::*;

#[test]
fn restart_inherits_a_wildcard_bind_and_its_insecure_consent() {
    let request = RefreshRequest {
        restart: true,
        ..RefreshRequest::default()
    };
    let launch = ResidentLaunch::new(
        "0.0.0.0:8767".parse().expect("wildcard launch address"),
        ResidentAccess::Insecure,
    );
    let binding = requested_binding(&request, Some("http://127.0.0.1:8767"), Some(launch))
        .expect("insecure launch metadata is complete");

    let command = restart_command(Path::new("/installed/spice"), &binding, &request);
    let arguments = command.get_args().collect::<Vec<_>>();

    assert_eq!(
        arguments,
        [
            "--json",
            "up",
            "--host",
            "0.0.0.0",
            "--port",
            "8767",
            "--allow-insecure-bind",
        ]
        .map(OsStr::new)
    );
}

#[test]
fn restart_keeps_loopback_local_and_refuses_to_drop_authentication() {
    let request = RefreshRequest {
        restart: true,
        ..RefreshRequest::default()
    };
    let local = ResidentLaunch::new(
        "127.0.0.1:8767".parse().expect("loopback launch address"),
        ResidentAccess::Local,
    );
    let local_binding = requested_binding(&request, Some("http://127.0.0.1:8767"), Some(local))
        .expect("local launch metadata is complete");
    assert_eq!(
        restart_command(Path::new("/installed/spice"), &local_binding, &request)
            .get_args()
            .collect::<Vec<_>>(),
        ["--json", "up", "--host", "127.0.0.1", "--port", "8767"].map(OsStr::new)
    );

    let authenticated = ResidentLaunch::new(
        "0.0.0.0:8767"
            .parse()
            .expect("authenticated launch address"),
        ResidentAccess::Authenticated,
    );
    let refused =
        match requested_binding(&request, Some("http://127.0.0.1:8767"), Some(authenticated)) {
            Err(error) => error,
            Ok(_) => panic!("authentication cannot be silently removed"),
        };
    assert!(
        matches!(&refused, RefreshError::ResidentAuthTokenRequired)
            && refused
                .to_string()
                .contains("resident::restart-auth-token-required"),
        "the refusal does not name token recovery: {refused}"
    );

    let token = "current-token-123";
    let authenticated_request = RefreshRequest {
        restart: true,
        up_arguments: ["--auth-token", token].map(OsString::from).into(),
        ..RefreshRequest::default()
    };
    let authenticated_binding = requested_binding(
        &authenticated_request,
        Some("http://127.0.0.1:8767"),
        Some(authenticated),
    )
    .expect("an explicitly supplied current token preserves authentication");
    assert_eq!(
        restart_command(
            Path::new("/installed/spice"),
            &authenticated_binding,
            &authenticated_request,
        )
        .get_args()
        .collect::<Vec<_>>(),
        [
            "--json",
            "up",
            "--host",
            "0.0.0.0",
            "--port",
            "8767",
            "--allow-insecure-bind",
            "--auth-token",
            token,
        ]
        .map(OsStr::new)
    );
}

#[test]
fn joined_forwarded_endpoint_options_also_override_published_values() {
    let request = RefreshRequest {
        restart: true,
        up_arguments: ["--host=0.0.0.0", "--port=9000"].map(OsString::from).into(),
        ..RefreshRequest::default()
    };

    let command = restart_command(
        Path::new("/installed/spice"),
        &restart_binding(Some("127.0.0.1"), Some(8767)),
        &request,
    );
    let arguments = command.get_args().collect::<Vec<_>>();

    assert_eq!(
        arguments,
        ["--json", "up", "--host=0.0.0.0", "--port=9000"].map(OsStr::new)
    );
}
