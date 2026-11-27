use super::*;

/// A record outlives its publisher, and the reader refuses it unread.
///
/// Both halves run against one fixture and one set of published bytes, so the
/// only thing that changes between the launch and the refusal is whether a
/// process still holds the resident lock. The live half is what makes the
/// refusal mean something: without it, a reader that refused everything would
/// pass.
///
/// Nothing on disk distinguishes the two states. The record is byte-identical
/// and the lock file still names the pid that wrote it, which is exactly the
/// evidence a killed resident leaves behind — and exactly why the pid in it
/// cannot be the check.
#[test]
fn app_server_refuses_a_record_whose_publisher_no_longer_holds_the_resident_lock() {
    let mut fixture = Fixture::new("serve-publisher-gone");
    let discovery = fixture.publish();
    let mut launched = Vec::new();

    let live = open_app_with(
        &fixture.serve,
        || {},
        |endpoint| {
            launched.push(endpoint.to_owned());
            Ok(())
        },
    );
    assert!(
        matches!(live, Outcome::Reply(_)),
        "a record whose publisher is live must open: {live:?}"
    );
    assert_eq!(launched, [discovery.endpoint.as_str()]);

    let record = fs::read(&fixture.serve).expect("published record reads");
    let holder = fs::read(fixture.root.join("resident")).expect("holder line reads");
    fixture.release_publisher();
    assert_eq!(
        fs::read(&fixture.serve).expect("record survives the publisher"),
        record,
        "releasing the lock must not be a rewrite of the record under test"
    );
    assert_eq!(
        fs::read(fixture.root.join("resident")).expect("holder line survives"),
        holder,
        "a killed resident leaves its holder line naming a pid that is gone"
    );

    let gone = open_app_with(
        &fixture.serve,
        || {},
        |endpoint| {
            launched.push(endpoint.to_owned());
            Ok(())
        },
    );

    let Outcome::Fault(fault) = gone else {
        panic!("a record whose publisher is gone must fault");
    };
    assert_eq!(fault.reason, "serve::publisher-gone");
    assert_eq!(
        launched,
        [discovery.endpoint],
        "the endpoint must not reach the launcher a second time"
    );
    assert!(!fault.detail.contains(&discovery.capability.to_string()));
}

/// A lock that cannot be probed refuses under its own name, not the record's.
///
/// The published record here is byte-perfect and owner-only; only the lock is
/// unusable. Reporting that as a discovery fault would name the one file that
/// is not at fault, and admitting it would dial an endpoint nobody has shown
/// is served.
#[test]
fn app_server_refuses_a_publisher_whose_lock_cannot_be_probed() {
    let mut fixture = Fixture::new("serve-publisher-unknown");
    let discovery = fixture.publish();
    fixture.release_publisher();

    let resident = fixture.root.join("resident");
    fs::remove_file(&resident).expect("fixture lock file removes");
    fs::create_dir(&resident).expect("fixture lock becomes unprobeable");

    let mut launched = false;
    let outcome = open_app_with(
        &fixture.serve,
        || {},
        |_| {
            launched = true;
            Ok(())
        },
    );

    let Outcome::Fault(fault) = outcome else {
        panic!("an unprobeable lock must fault");
    };
    assert_eq!(fault.reason, "serve::publisher-unknown");
    assert!(!launched, "an unprobeable lock must not reach the launcher");
    assert!(!fault.detail.contains(&discovery.capability.to_string()));
}

/// Every byte the client wrote, captured underneath the server handshake.
///
/// The peer records at the socket rather than at the protocol so the recording
/// survives a client that writes nothing: `tungstenite::accept` reports EOF the
/// same way whether the request was withheld or malformed, and only the raw
/// buffer distinguishes those.
struct RecordingStream {
    stream: TcpStream,
    seen: Arc<Mutex<Vec<u8>>>,
}

impl Read for RecordingStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.stream.read(buf)?;
        self.seen
            .lock()
            .expect("recorded request bytes")
            .extend_from_slice(&buf[..read]);
        Ok(read)
    }
}

impl Write for RecordingStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// A peer that answers one opening, whatever the client decides to send it.
///
/// It always accepts the connection and always tries the handshake, because it
/// is the same peer in both halves of the test below. What differs is only
/// what the client puts on the wire.
fn answer_one_opening(
    listener: TcpListener,
    accepted: mpsc::Sender<()>,
    seen: Arc<Mutex<Vec<u8>>>,
    epoch: CanonicalUuid,
) {
    let (stream, _) = listener
        .accept()
        .expect("peer accepts the client connection");
    accepted
        .send(())
        .expect("peer announces the accepted connection");
    let Ok(mut socket) = tungstenite::accept(RecordingStream { stream, seen }) else {
        return;
    };
    let hello = socket
        .read()
        .expect("peer reads the client Hello")
        .into_data();
    let hello: wire::Hello = wire::decode(&hello).expect("client Hello decodes");
    socket
        .send(Message::Binary(
            wire::encode(&wire::Accepted {
                wire: wire::WireVersion::CURRENT,
                actor: Actor::TEST_PRINCIPAL,
                epoch: hello.epoch,
                release: "1.2.3".parse().expect("fixture release"),
            })
            .expect("peer acceptance encodes")
            .into(),
        ))
        .expect("peer sends its acceptance");
    assert_eq!(hello.epoch, epoch);
    // Held open until the client is done with it, so the reply cannot race a
    // close, and released as soon as the client drops its end.
    let _ = socket.read();
}

/// Open one session against a live peer, losing the publisher or keeping it.
///
/// The publisher is released from inside `after_connect`, which is the only
/// place the interval under test can be entered on purpose: the connection is
/// established and not one request byte has been written. Waiting for the
/// peer's own accept first means the connection is genuinely made — the
/// refusal cannot be explained by a client that never got that far.
fn open_against_peer(fixture: &mut Fixture, lose_publisher: bool) -> (ClientResult<()>, Vec<u8>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("peer binds an endpoint");
    let discovery = serve::Discovery {
        endpoint: format!(
            "http://{}",
            listener.local_addr().expect("peer endpoint address")
        ),
        capability: uuid(1),
        epoch: uuid(2),
    };
    discovery
        .publish(&fixture.serve)
        .expect("fixture discovery publishes");
    fixture.hold(discovery.epoch);

    let seen = Arc::new(Mutex::new(Vec::new()));
    let (accepted, connected) = mpsc::channel();
    let peer = std::thread::spawn({
        let seen = Arc::clone(&seen);
        let epoch = discovery.epoch;
        move || answer_one_opening(listener, accepted, seen, epoch)
    });

    let read = DiscoveryRead::open(&fixture.serve, &resident_lock_path(&fixture.serve))
        .expect("fixture discovery reads");
    let outcome = open_session_with(
        &COMMAND_CONTRACT,
        read,
        Instant::now() + UNREACHED_BOUND,
        UNREACHED_BOUND,
        || {
            connected
                .recv()
                .expect("peer accepts the connection before the publisher is decided");
            if lose_publisher {
                fixture.release_publisher();
            }
        },
    )
    // Dropping the session closes the client end, which is what lets the peer
    // finish; the opening either completed or it did not.
    .map(drop);
    peer.join().expect("peer thread");
    let seen = seen.lock().expect("recorded request bytes").clone();
    (outcome, seen)
}

/// The publisher dies after the connect, and the capability is never spent.
///
/// This is the interval the pre-connect check cannot cover. The record is
/// still byte-perfect and the connection is real and accepted, so nothing the
/// client can re-read says anything is wrong; only the kernel's answer about
/// the lock does. A port whose resident died is not the resident's any more,
/// and the listener that inherits it would be handed a capability by a client
/// that never learned the difference.
///
/// Both halves run the same peer against the same fixture, and the live half
/// is what makes the refusal mean something: without it a client that refused
/// every opening would pass. The capability is checked against the peer's own
/// recording rather than against the client's report, because what matters is
/// not what the client believes it withheld but what actually reached a
/// stranger's socket.
#[test]
fn app_server_refuses_a_publisher_lost_between_the_connect_and_the_handshake() {
    let mut fixture = Fixture::new("serve-publisher-lost-after-connect");
    let capability = uuid(1).to_string();

    let (live, live_seen) = open_against_peer(&mut fixture, false);
    assert!(
        live.is_ok(),
        "a publisher held throughout must open: {live:?}"
    );
    assert!(
        live_seen.starts_with(b"GET /socket?auth="),
        "the opening the guard permits is the one carrying the capability: {:?}",
        String::from_utf8_lossy(&live_seen)
    );
    assert!(
        live_seen
            .windows(capability.len())
            .any(|window| window == capability.as_bytes()),
        "the permitted opening puts the capability on the wire, which is what the refusal withholds"
    );

    let (gone, gone_seen) = open_against_peer(&mut fixture, true);
    let Err(Outcome::Fault(fault)) = gone else {
        panic!("a publisher lost after the connect must fault: {gone:?}");
    };
    assert_eq!(fault.reason, "serve::publisher-gone");
    assert!(
        gone_seen.is_empty(),
        "a refused opening writes nothing at all: {:?}",
        String::from_utf8_lossy(&gone_seen)
    );
    assert!(
        !gone_seen
            .windows(capability.len())
            .any(|window| window == capability.as_bytes()),
        "the capability must not reach a socket the publisher no longer owns"
    );
    assert!(!fault.detail.contains(&capability));
}

#[test]
fn app_server_launcher_interruption_is_one_typed_fault() {
    let fixture = Fixture::new("serve-launch-interruption");
    let discovery = fixture.publish();

    let outcome = open_app_with(
        &fixture.serve,
        || {},
        |_| Err("fixture launcher was interrupted".to_owned()),
    );

    let Outcome::Fault(fault) = outcome else {
        panic!("launcher interruption must fault");
    };
    assert_eq!(fault.reason, "serve::launch-failed");
    assert_eq!(fault.detail, "fixture launcher was interrupted");
    assert!(!fault.detail.contains(&discovery.capability.to_string()));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn app_server_platform_launcher_has_one_fixed_program_and_exact_endpoint() {
    use std::ffi::OsStr;

    let endpoint = "http://127.0.0.1:48123";
    let command = browser_command(endpoint).expect("this platform has one launcher");
    #[cfg(target_os = "macos")]
    {
        assert_eq!(command.get_program(), OsStr::new("/usr/bin/open"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [OsStr::new("-u"), OsStr::new(endpoint)]
        );
    }
    #[cfg(target_os = "linux")]
    {
        assert_eq!(command.get_program(), OsStr::new("/usr/bin/xdg-open"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [OsStr::new(endpoint)]
        );
    }
}

#[cfg(unix)]
#[test]
fn discovery_symlinks_and_public_modes_are_refused() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let fixture = Fixture::new("insecure");
    let discovery = fixture.publish();
    fs::set_permissions(&fixture.serve, fs::Permissions::from_mode(0o644))
        .expect("fixture mode changes");
    let Err(Outcome::Fault(fault)) =
        DiscoveryRead::open(&fixture.serve, &fixture.root.join("resident"))
    else {
        panic!("public discovery must be refused");
    };
    assert_eq!(fault.reason, "serve::discovery-insecure");

    fs::remove_file(&fixture.serve).expect("public fixture removes");
    let target = fixture.root.join("target");
    discovery.publish(&target).expect("target publishes");
    symlink(&target, &fixture.serve).expect("fixture symlink");
    let Err(Outcome::Fault(fault)) =
        DiscoveryRead::open(&fixture.serve, &fixture.root.join("resident"))
    else {
        panic!("symlink discovery must be refused");
    };
    assert_eq!(fault.reason, "serve::discovery-insecure");
}

#[cfg(unix)]
#[test]
fn discovery_fifo_substitution_is_nonblocking_and_refused_from_the_descriptor() {
    use rustix::fs::{OFlags, fcntl_getfl};
    use std::ffi::CString;
    use std::fs::OpenOptions;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let fixture = Fixture::new("fifo-substitution");
    let discovery = fixture.publish();
    let mut anchor = None;

    let outcome = DiscoveryRead::open_with(&fixture.serve, &fixture.root.join("resident"), || {
        fs::remove_file(&fixture.serve).expect("regular discovery removes");
        let path = CString::new(fixture.serve.as_os_str().as_bytes())
            .expect("fixture path has no interior NUL");
        // SAFETY: `path` is a live NUL-terminated pathname and the mode is valid.
        let result = unsafe { libc::mkfifo(path.as_ptr(), libc::S_IRUSR | libc::S_IWUSR) };
        assert_eq!(result, 0, "create FIFO substitution");
        anchor = Some(
            OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
                .open(&fixture.serve)
                .expect("fixture anchor opens without waiting"),
        );
    });

    let Err(Outcome::Fault(fault)) = outcome else {
        panic!("FIFO substitution must fault");
    };
    assert_eq!(fault.reason, "serve::discovery-insecure");
    assert!(!fault.detail.contains(&discovery.capability.to_string()));

    let opened = open_discovery(&fixture.serve).expect("FIFO descriptor opens");
    let flags = fcntl_getfl(&opened).expect("FIFO descriptor flags read");
    assert!(flags.contains(OFlags::NONBLOCK));
    drop(anchor);
}

fn accepted(epoch: CanonicalUuid, release: &str) -> wire::Accepted {
    wire::Accepted {
        wire: wire::WireVersion::CURRENT,
        actor: wire::Actor::TEST_PRINCIPAL,
        epoch,
        release: release.parse().expect("fixture release"),
    }
}

#[test]
fn response_binds_identity_epoch_position_and_answer_shape() {
    let mapped = mapped();
    let epoch = uuid(5);
    let session = accepted(epoch, spice::version::product().as_str());
    let bytes = response(mapped.id, epoch, committed(), state_body());
    let outcome = decode_response(
        &bytes,
        &COMMAND_CONTRACT,
        mapped.id,
        mapped.kind,
        &session,
        uuid(6),
    );
    assert!(matches!(
        outcome,
        Outcome::Reply(Reply {
            position: ReplyPosition::Committed(_),
            body: CommandAnswer::State(StateAnswer {
                to: ResidentState::Dark,
                ..
            })
        })
    ));

    let replay = decode_response(
        &bytes,
        &COMMAND_CONTRACT,
        uuid(7),
        mapped.kind,
        &session,
        uuid(6),
    );
    let Outcome::Fault(fault) = replay else {
        panic!("a replayed response cannot answer a different command");
    };
    assert_eq!(fault.reason, "command::answer-mismatch");

    let snapshot = response(
        mapped.id,
        epoch,
        ReplyPosition::Snapshot(Seq::new(1).expect("fixture position")),
        state_body(),
    );
    let Outcome::Fault(fault) = decode_response(
        &snapshot,
        &COMMAND_CONTRACT,
        mapped.id,
        mapped.kind,
        &session,
        uuid(6),
    ) else {
        panic!("snapshot cannot answer a mutating contract");
    };
    assert_eq!(fault.reason, "command::position-mismatch");

    let malformed = response(
        mapped.id,
        epoch,
        committed(),
        serde_json::json!({ "from": "seated" }),
    );
    let Outcome::Fault(fault) = decode_response(
        &malformed,
        &COMMAND_CONTRACT,
        mapped.id,
        mapped.kind,
        &session,
        uuid(6),
    ) else {
        panic!("wrong answer shape must fault");
    };
    assert_eq!(fault.reason, "command::answer-binding");
    assert!(!fault.detail.contains("release"));
}

/// An answer from another release refuses without being read, and names both
/// releases with the side that must move: an older resident restarts on the
/// client's release, while an older client gives way to the resident's.
#[test]
fn foreign_release_answer_names_both_releases_and_the_side_that_moves() {
    let mapped = mapped();
    let epoch = uuid(5);
    let foreign = response(
        mapped.id,
        epoch,
        committed(),
        serde_json::json!({ "from": "seated" }),
    );
    let Outcome::Fault(fault) = decode_response(
        &foreign,
        &COMMAND_CONTRACT,
        mapped.id,
        mapped.kind,
        &accepted(epoch, "999999.0.0"),
        uuid(6),
    ) else {
        panic!("a foreign answer must fault");
    };
    assert_eq!(fault.reason, "command::answer-binding");
    assert!(fault.detail.contains(spice::version::product().as_str()));
    assert!(fault.detail.contains("installed with release 999999.0.0"));

    let release =
        |text: &str| -> wire::manifest::ProductVersion { text.parse().expect("fixture release") };
    let Outcome::Fault(newer_client) =
        crate::command_client::answer_binding_fault(&release("2.0.0"), &release("1.4.0"))
    else {
        panic!("an older resident's foreign answer faults");
    };
    assert!(newer_client.detail.contains("resident release 1.4.0"));
    assert!(
        newer_client
            .detail
            .contains("restart the resident on release 2.0.0")
    );
    let Outcome::Fault(older_client) = crate::command_client::answer_binding_fault(
        &release("1.4.0-alpha.dW0a"),
        &release("1.4.0-alpha.dW0b"),
    ) else {
        panic!("a newer resident's foreign answer faults");
    };
    assert!(
        older_client
            .detail
            .contains("client release 1.4.0-alpha.dW0a")
    );
    assert!(
        older_client
            .detail
            .contains("installed with release 1.4.0-alpha.dW0b")
    );
}

#[test]
fn remote_outcomes_cannot_echo_the_discovery_capability() {
    let capability = uuid(8);
    let discovery = serve::Discovery {
        endpoint: "http://127.0.0.1:48123".to_owned(),
        capability,
        epoch: uuid(9),
    };
    let refusal = Refusal::bare(wire::RefusalDiagnostic::new(
        "hello::fixture".to_owned(),
        None,
        None,
        Some(capability.to_string()),
    ));
    let opening = wire::encode(&refusal).expect("fixture refusal encodes");
    let Err(Outcome::Fault(fault)) = decode_opening(&opening, &discovery) else {
        panic!("opening capability echo must fault");
    };
    assert_eq!(fault.reason, "serve::capability-disclosed");
    assert!(!fault.detail.contains(&capability.to_string()));

    let mapped = mapped();
    let response = wire::encode(&Response {
        v: wire::WireVersion::CURRENT,
        id: mapped.id,
        epoch: discovery.epoch,
        outcome: Outcome::<serde_json::Value>::Refusal(refusal),
    })
    .expect("fixture response encodes");
    let Outcome::Fault(fault) = decode_response(
        &response,
        &COMMAND_CONTRACT,
        mapped.id,
        mapped.kind,
        &accepted(discovery.epoch, spice::version::product().as_str()),
        capability,
    ) else {
        panic!("response capability echo must fault");
    };
    assert_eq!(fault.reason, "serve::capability-disclosed");
    assert!(!fault.detail.contains(&capability.to_string()));
}
