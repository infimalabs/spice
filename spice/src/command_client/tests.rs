use std::net::{Shutdown, TcpListener};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use sill::{Acquisition, Possession};
use spice::resident_lock::holder_record;

use wire::intent::{StateAnswer, UpChange};
use wire::{ResidentState, Seq, SeqRange};

use super::*;
use crate::cli::{ExitOutcome, JournalAccess, ResultClass};
use crate::render::{self, Form};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

const COMMAND_CONTRACT: Contract = Contract {
    path: &["fixture", "command"],
    syntax: "fixture command",
    principal: Principal::Principal,
    journal: JournalAccess::Command,
    reply_position: ReplyPositionLaw::Committed,
    facts: &[],
    guards: &[Guard::Resident, Guard::OperatorCapability],
    tools: &[],
    result: ResultClass::Reply,
    outcomes: ExitOutcome::ALL,
    battery: "command-client::fixture",
};

struct Fixture {
    root: PathBuf,
    serve: PathBuf,
    publisher: Option<Possession>,
}

impl Fixture {
    /// A runtime whose published records have a resident still holding the
    /// lock, because that is the only state in which a client may dial them.
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "spice-command-client-{name}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("fixture runtime directory");
        let serve = root.join("serve");
        let lock = LockFile::new(
            LockName::new("resident").expect("resident is a non-empty lock name"),
            resident_lock_path(&serve),
        );
        let Acquisition::Acquired(publisher) = lock.try_acquire().expect("fixture resident lock")
        else {
            panic!("a fixture runtime nothing else has seen cannot be contended");
        };
        Self {
            root,
            serve,
            publisher: Some(publisher),
        }
    }

    /// Lose the publisher the way a killed resident does.
    ///
    /// Dropping the possession releases the kernel lock and nothing else: the
    /// record it published stays exactly as written, and so does the holder
    /// line in the lock file naming a pid. `ResidentLock`'s own `Drop`, which
    /// truncates that line, is deliberately not run here — a fixture that
    /// cleaned up after itself would only prove the orderly path.
    fn release_publisher(&mut self) {
        self.publisher = None;
    }

    fn publish(&self) -> serve::Discovery {
        let discovery = serve::Discovery {
            endpoint: "http://127.0.0.1:48123".to_owned(),
            capability: uuid(1),
            epoch: uuid(2),
        };
        discovery
            .publish(&self.serve)
            .expect("fixture discovery publishes");
        self.hold(discovery.epoch);
        discovery
    }

    /// Record the generation this fixture's resident published.
    ///
    /// A real resident writes this line the instant it takes the lock and
    /// rewrites it with its own generation before publishing anything, so a
    /// fixture holding the lock without one is a state no resident produces —
    /// and every case here that expects an admission would be asserting
    /// against a record the client is right to refuse.
    fn hold(&self, epoch: CanonicalUuid) {
        self.publisher
            .as_ref()
            .expect("fixture resident still holds its lock")
            .rewrite_record(holder_record(std::process::id(), Some(epoch)).as_bytes())
            .expect("fixture holder record writes");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("fixture cleanup");
    }
}

fn uuid(last: u8) -> CanonicalUuid {
    format!("00000000-0000-4000-8000-{last:012x}")
        .parse()
        .expect("canonical fixture UUID")
}

fn mapped() -> MappedCommand {
    MappedCommand::new(
        &COMMAND_CONTRACT,
        &["fixture", "command"],
        CommandKind::UpChange,
        vec![Witness::Transition {
            from: ResidentState::Seated,
            to: ResidentState::Dark,
        }],
        &UpChange {
            state: ResidentState::Dark,
        },
    )
    .expect("fixture command maps")
}

fn response(
    id: CanonicalUuid,
    epoch: CanonicalUuid,
    position: ReplyPosition,
    body: serde_json::Value,
) -> Vec<u8> {
    wire::encode(&Response {
        v: wire::WireVersion::CURRENT,
        id,
        epoch,
        outcome: Outcome::Reply(Reply { position, body }),
    })
    .expect("fixture response encodes")
}

fn committed() -> ReplyPosition {
    ReplyPosition::Committed(SeqRange {
        first: Seq::new(1).expect("fixture position"),
        last: Seq::new(2).expect("fixture position"),
    })
}

fn state_body() -> serde_json::Value {
    serde_json::to_value(StateAnswer {
        from: ResidentState::Seated,
        to: ResidentState::Dark,
    })
    .expect("fixture answer encodes")
}

fn answer_replacement_opening(
    listener: TcpListener,
    epoch: CanonicalUuid,
    answer_command: bool,
) -> usize {
    let (stream, _) = listener.accept().expect("replacement peer accepts");
    let mut socket = tungstenite::accept(stream).expect("replacement peer opens socket");
    let hello: wire::Hello = wire::decode(
        &socket
            .read()
            .expect("replacement peer reads Hello")
            .into_data(),
    )
    .expect("replacement Hello decodes");
    assert_eq!(hello.epoch, epoch);
    socket
        .send(Message::Binary(
            wire::encode(&wire::Accepted {
                wire: wire::WireVersion::CURRENT,
                actor: Actor::TEST_PRINCIPAL,
                epoch,
                release: "1.2.3".parse().expect("fixture release"),
            })
            .expect("replacement acceptance encodes")
            .into(),
        ))
        .expect("replacement peer accepts client");
    if !answer_command {
        return 0;
    }
    let command: Command<serde_json::Value> = wire::decode(
        &socket
            .read()
            .expect("replacement peer reads command")
            .into_data(),
    )
    .expect("replacement command decodes");
    assert_eq!(command.kind, CommandKind::BoardRead);
    let response = Response::<serde_json::Value> {
        v: wire::WireVersion::CURRENT,
        id: command.id,
        epoch,
        outcome: Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
            "fixture::task-status-reached-replacement".to_owned(),
            None,
            None,
            None,
        ))),
    };
    socket
        .send(Message::Binary(
            wire::encode(&response)
                .expect("replacement response encodes")
                .into(),
        ))
        .expect("replacement command response sends");
    1
}

fn accept_fixture_session(listener: TcpListener, epoch: CanonicalUuid) -> WebSocket<TcpStream> {
    let (stream, _) = listener.accept().expect("fixture peer accepts");
    let mut socket = tungstenite::accept(stream).expect("fixture peer opens socket");
    let hello: wire::Hello =
        wire::decode(&socket.read().expect("fixture peer reads Hello").into_data())
            .expect("fixture Hello decodes");
    assert_eq!(hello.epoch, epoch);
    socket
        .send(Message::Binary(
            wire::encode(&wire::Accepted {
                wire: wire::WireVersion::CURRENT,
                actor: Actor::TEST_PRINCIPAL,
                epoch,
                release: "1.2.3".parse().expect("fixture release"),
            })
            .expect("fixture acceptance encodes")
            .into(),
        ))
        .expect("fixture peer accepts client");
    socket
}

fn read_fixture_command(socket: &mut WebSocket<TcpStream>) -> Command<serde_json::Value> {
    wire::decode(
        &socket
            .read()
            .expect("fixture peer reads command")
            .into_data(),
    )
    .expect("fixture command decodes")
}

#[derive(Clone, Copy)]
enum InterruptedAt {
    BeforeCommand,
    AfterCommand,
}

fn recovering_submission(
    name: &str,
    interrupted_at: InterruptedAt,
    replacement_outcome: Outcome<serde_json::Value>,
) -> (
    Outcome<CommandAnswer>,
    Option<Command<serde_json::Value>>,
    Command<serde_json::Value>,
    Vec<String>,
) {
    let fixture = Fixture::new(name);
    let initial_listener = TcpListener::bind("127.0.0.1:0").expect("initial peer binds");
    let replacement_listener = TcpListener::bind("127.0.0.1:0").expect("replacement peer binds");
    let initial_epoch = uuid(40);
    let replacement_epoch = uuid(41);
    serve::Discovery {
        endpoint: format!(
            "http://{}",
            initial_listener.local_addr().expect("initial peer address")
        ),
        capability: uuid(42),
        epoch: initial_epoch,
    }
    .publish(&fixture.serve)
    .expect("initial discovery publishes");
    fixture.hold(initial_epoch);
    let replacement = serve::Discovery {
        endpoint: format!(
            "http://{}",
            replacement_listener
                .local_addr()
                .expect("replacement peer address")
        ),
        capability: uuid(43),
        epoch: replacement_epoch,
    };

    let initial_peer = std::thread::spawn(move || {
        let mut socket = accept_fixture_session(initial_listener, initial_epoch);
        let command = match interrupted_at {
            InterruptedAt::BeforeCommand => None,
            InterruptedAt::AfterCommand => Some(read_fixture_command(&mut socket)),
        };
        socket
            .get_mut()
            .shutdown(Shutdown::Both)
            .expect("initial peer resets connection");
        command
    });
    let replacement_peer = std::thread::spawn(move || {
        let mut socket = accept_fixture_session(replacement_listener, replacement_epoch);
        let command = read_fixture_command(&mut socket);
        let response = Response {
            v: wire::WireVersion::CURRENT,
            id: command.id,
            epoch: replacement_epoch,
            outcome: replacement_outcome,
        };
        socket
            .send(Message::Binary(
                wire::encode(&response)
                    .expect("replacement response encodes")
                    .into(),
            ))
            .expect("replacement response sends");
        command
    });
    let command = mapped();
    let command_id = command.id;
    let mut transients = Vec::new();
    let outcome = execute_mapped_with(
        &fixture.serve,
        &COMMAND_CONTRACT,
        Connection::Operator,
        Ok(command),
        &Ambient::default(),
        |fault| {
            transients.push(fault.reason.clone());
            if fault.reason == "serve::send-failed"
                || fault.reason == "serve::receive-failed"
                || fault.reason == "serve::closed-without-answer"
            {
                fixture.hold(replacement_epoch);
                replacement
                    .publish(&fixture.serve)
                    .expect("replacement discovery publishes after reset");
            }
        },
    );
    let initial = initial_peer.join().expect("initial peer joins");
    let replacement = replacement_peer.join().expect("replacement peer joins");
    assert_eq!(replacement.id, command_id);
    (outcome, initial, replacement, transients)
}

#[test]
fn a_reset_before_commit_refreshes_authority_and_submits_the_command_once() {
    let (outcome, initial, replacement, transients) = recovering_submission(
        "reset-before-command",
        InterruptedAt::BeforeCommand,
        Outcome::Reply(Reply {
            position: committed(),
            body: state_body(),
        }),
    );

    assert!(initial.is_none(), "the interrupted resident saw no command");
    assert_eq!(replacement.kind, CommandKind::UpChange);
    assert!(matches!(outcome, Outcome::Reply(_)), "{outcome:?}");
    assert!(transients.iter().any(|reason| {
        matches!(
            reason.as_str(),
            "serve::send-failed" | "serve::receive-failed" | "serve::closed-without-answer"
        )
    }));
}

#[test]
fn a_reset_after_commit_reconciles_the_same_command_identity() {
    let (outcome, initial, replacement, _) = recovering_submission(
        "reset-after-command",
        InterruptedAt::AfterCommand,
        Outcome::Reply(Reply {
            position: committed(),
            body: state_body(),
        }),
    );

    let initial = initial.expect("the interrupted resident committed the command");
    assert_eq!(initial.id, replacement.id);
    assert_eq!(initial.kind, replacement.kind);
    assert_eq!(initial.expected, replacement.expected);
    assert_eq!(initial.body, replacement.body);
    assert!(matches!(outcome, Outcome::Reply(_)), "{outcome:?}");
}

#[test]
fn refreshed_authority_returns_a_moved_claim_with_current_owner_evidence() {
    let current_owner = Actor::Lane(uuid(44));
    let (outcome, initial, replacement, _) = recovering_submission(
        "reset-moved-claim",
        InterruptedAt::AfterCommand,
        Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
            "claim::ownership-refused",
            Some("claim".to_owned()),
            Some(Actor::TEST_PRINCIPAL.to_string()),
            Some(current_owner.to_string()),
        ))),
    );

    assert_eq!(
        initial
            .expect("the interrupted resident saw the command")
            .id,
        replacement.id
    );
    let Outcome::Refusal(refusal) = outcome else {
        panic!("moved claim must stop as the current refusal: {outcome:?}");
    };
    assert_eq!(refusal.reason(), "claim::ownership-refused");
    let diagnostic = refusal
        .diagnostic()
        .expect("moved claim retains its authority diagnostic");
    assert_eq!(
        diagnostic.observed(),
        Some(current_owner.to_string().as_str())
    );
}

#[test]
fn repository_route_distinguishes_local_foreign_and_operator_callers() {
    let fixture = Fixture::new("repository-route");
    let source = fixture.root.join("source.git");
    let addressed = fixture.root.join("addressed.git");
    let wrapper = source.join(".spice-install/bin/spice");
    let local_discovery = source.join(".spice/runtime/serve");
    let foreign_discovery = addressed.join(".spice/runtime/serve");
    fs::create_dir_all(
        wrapper
            .parent()
            .expect("installed wrapper has one bounded parent"),
    )
    .expect("source install exists");
    fs::create_dir_all(
        local_discovery
            .parent()
            .expect("local discovery has one bounded parent"),
    )
    .expect("local runtime exists");
    fs::create_dir_all(
        foreign_discovery
            .parent()
            .expect("foreign discovery has one bounded parent"),
    )
    .expect("foreign runtime exists");
    fs::write(&wrapper, []).expect("wrapper fixture exists");

    assert!(
        foreign_repository_from_paths(Connection::Lane(uuid(27)), &wrapper, &local_discovery)
            .is_none(),
        "one Git-common authority is local even while its lane surface is absent"
    );
    assert!(
        foreign_repository_from_paths(Connection::Operator, &wrapper, &foreign_discovery).is_none(),
        "operator commands never borrow lane repository identity"
    );
    let foreign =
        foreign_repository_from_paths(Connection::Lane(uuid(28)), &wrapper, &foreign_discovery)
            .expect("distinct Git-common authorities are foreign");
    assert_eq!(
        foreign.source,
        fs::canonicalize(source).expect("source authority canonicalizes")
    );
    assert_eq!(
        foreign.addressed,
        fs::canonicalize(addressed).expect("addressed authority canonicalizes")
    );
}

#[test]
fn a_live_foreign_resident_refuses_the_lane_before_discovery_waits() {
    let fixture = Fixture::new("foreign-resident");
    fixture.publish();
    let lane_surface = selected_discovery_path(&fixture.serve, Connection::Lane(uuid(28)));
    let before = fs::read(&fixture.serve).expect("operator discovery is readable");
    let foreign = ForeignRepository {
        source: fixture.root.join("source.git"),
        addressed: fixture.root.join("addressed.git"),
    };
    let outcome = match acquire_session_with(
        &SessionRoute {
            contract: &COMMAND_CONTRACT,
            operator_discovery: &fixture.serve,
            discovery_path: &lane_surface,
            resident_lock: &resident_lock_path(&fixture.serve),
            foreign: Some(&foreign),
        },
        test_support::DEADLOCK_GUARD,
        |_| panic!("a proved foreign resident does not enter publication handoff"),
    ) {
        Err(outcome) => outcome,
        Ok(_) => panic!("a foreign resident cannot authenticate this lane"),
    };

    let Outcome::Refusal(refusal) = outcome else {
        panic!("foreign repository mismatch must be a typed refusal: {outcome:?}");
    };
    assert_eq!(refusal.reason(), "serve::foreign-repository");
    let diagnostic = refusal
        .diagnostic()
        .expect("foreign repository refusal carries both authorities");
    assert_eq!(diagnostic.expected(), foreign.source.to_str());
    assert_eq!(diagnostic.observed(), foreign.addressed.to_str());
    assert_eq!(
        fs::read(&fixture.serve).expect("operator discovery remains readable"),
        before
    );
    assert!(
        !lane_surface.exists(),
        "refusal creates no foreign lane surface"
    );
}

#[test]
fn a_local_lane_reacquires_its_surface_from_a_live_replacement() {
    let fixture = Fixture::new("replacement-lane-authentication");
    let lane_surface = selected_discovery_path(&fixture.serve, Connection::Lane(uuid(29)));
    fs::create_dir(
        lane_surface
            .parent()
            .expect("lane discovery has one bounded parent"),
    )
    .expect("lane discovery family exists");
    let listener = TcpListener::bind("127.0.0.1:0").expect("replacement peer binds");
    let epoch = uuid(30);
    let discovery = serve::Discovery {
        endpoint: format!(
            "http://{}",
            listener.local_addr().expect("replacement peer address")
        ),
        capability: uuid(31),
        epoch,
    };
    fixture.hold(epoch);
    let peer = std::thread::spawn(move || answer_replacement_opening(listener, epoch, false));
    let mut transients = Vec::new();

    let session = acquire_session_with(
        &SessionRoute {
            contract: &COMMAND_CONTRACT,
            operator_discovery: &fixture.serve,
            discovery_path: &lane_surface,
            resident_lock: &resident_lock_path(&fixture.serve),
            foreign: None,
        },
        test_support::DEADLOCK_GUARD,
        |fault| {
            transients.push(fault.reason.clone());
            discovery
                .publish(&lane_surface)
                .expect("replacement lane discovery publishes");
        },
    )
    .expect("authenticated lane read reaches its replacement");

    assert_eq!(session.accepted.epoch, epoch);
    assert_eq!(transients, ["serve::discovery-unavailable"]);
    drop(session);
    assert_eq!(peer.join().expect("replacement peer joins"), 0);
}

fn mapped_task_status() -> (&'static Contract, MappedCommand) {
    let contract = crate::cli::contract(&["task", "status"])
        .expect("the generated CLI registry carries task status");
    let mapped = MappedCommand::new(
        contract,
        contract.path,
        CommandKind::BoardRead,
        Vec::new(),
        &wire::intent::BoardRead::All { family: None },
    )
    .expect("task status maps before discovery");
    (contract, mapped)
}

#[test]
fn task_status_reacquires_discovery_before_sending_once_to_a_live_replacement() {
    let fixture = Fixture::new("replacement-command");
    let listener = TcpListener::bind("127.0.0.1:0").expect("replacement peer binds");
    let epoch = uuid(32);
    let discovery = serve::Discovery {
        endpoint: format!(
            "http://{}",
            listener.local_addr().expect("replacement peer address")
        ),
        capability: uuid(33),
        epoch,
    };
    fixture.hold(epoch);
    let peer = std::thread::spawn(move || answer_replacement_opening(listener, epoch, true));
    let mut transients = Vec::new();
    let (contract, mapped) = mapped_task_status();

    let outcome = execute_mapped_with(
        &fixture.serve,
        contract,
        Connection::Operator,
        Ok(mapped),
        &Ambient::default(),
        |fault| {
            transients.push(fault.reason.clone());
            discovery
                .publish(&fixture.serve)
                .expect("replacement discovery publishes");
        },
    );

    let Outcome::Refusal(refusal) = outcome else {
        panic!("task status must reach the replacement response: {outcome:?}");
    };
    assert_eq!(refusal.reason(), "fixture::task-status-reached-replacement");
    assert_eq!(transients, ["serve::discovery-unavailable"]);
    assert_eq!(peer.join().expect("replacement peer joins"), 1);
}

#[test]
fn task_status_reports_an_absent_resident_before_command_delivery() {
    let mut fixture = Fixture::new("absent-resident-command");
    fixture.release_publisher();
    let (contract, mapped) = mapped_task_status();
    let mut transients = Vec::new();

    let outcome = execute_mapped_with(
        &fixture.serve,
        contract,
        Connection::Operator,
        Ok(mapped),
        &Ambient::default(),
        |fault| transients.push(fault.reason.clone()),
    );

    let Outcome::Fault(fault) = outcome else {
        panic!("task status without a resident must fault: {outcome:?}");
    };
    assert_eq!(fault.reason, "serve::publisher-gone");
    assert!(
        transients.is_empty(),
        "an available resident lock is terminal, not a publication handoff"
    );
}

#[test]
fn task_status_reports_a_resident_released_during_publication_handoff() {
    let mut fixture = Fixture::new("released-resident-command");
    let serve = fixture.serve.clone();
    let (contract, mapped) = mapped_task_status();
    let mut transients = Vec::new();

    let outcome = execute_mapped_with(
        &serve,
        contract,
        Connection::Operator,
        Ok(mapped),
        &Ambient::default(),
        |fault| {
            transients.push(fault.reason.clone());
            fixture.release_publisher();
        },
    );

    let Outcome::Fault(fault) = outcome else {
        panic!("task status must stop when the starting resident exits: {outcome:?}");
    };
    assert_eq!(transients, ["serve::discovery-unavailable"]);
    assert_eq!(fault.reason, "serve::publisher-gone");
}

#[test]
fn task_status_keeps_an_unprobeable_resident_lock_fail_closed() {
    let mut fixture = Fixture::new("unknown-resident-command");
    fixture.release_publisher();
    let resident = resident_lock_path(&fixture.serve);
    fs::remove_file(&resident).expect("remove fixture resident lock");
    fs::create_dir(&resident).expect("replace resident lock with a directory");
    let (contract, mapped) = mapped_task_status();

    let outcome = execute_mapped_with(
        &fixture.serve,
        contract,
        Connection::Operator,
        Ok(mapped),
        &Ambient::default(),
        |_| panic!("an unprobeable lock must not enter publication handoff"),
    );

    let Outcome::Fault(fault) = outcome else {
        panic!("task status with an unprobeable lock must fault: {outcome:?}");
    };
    assert_eq!(fault.reason, "serve::publisher-unknown");
}

#[test]
fn mapping_rejects_the_wrong_contract_and_incomplete_body_before_discovery() {
    let wrong_path = MappedCommand::selected(&COMMAND_CONTRACT, SelectedCommand::Down);
    let Err(Outcome::Fault(fault)) = wrong_path else {
        panic!("wrong path must fail mapping");
    };
    assert_eq!(fault.reason, "command::mapping-unsupported");

    let incomplete = MappedCommand::new(
        &COMMAND_CONTRACT,
        &["fixture", "command"],
        CommandKind::UpChange,
        vec![Witness::Transition {
            from: ResidentState::Seated,
            to: ResidentState::Dark,
        }],
        &serde_json::json!({}),
    );
    let Err(Outcome::Fault(fault)) = incomplete else {
        panic!("incomplete body must fail mapping");
    };
    assert_eq!(fault.reason, "command::body-incomplete");
}

/// Every single-command resident task CLI the registry names must reach the
/// resident. `task import` is a bounded client aggregate of ordinary TaskAdd
/// commands, not a new wire mutation kind.
#[test]
fn every_registered_task_command_maps_to_a_wire_kind() {
    let unmapped = crate::cli::contracts()
        .iter()
        .filter(|contract| {
            matches!(contract.path, ["task", _])
                && contract.journal == crate::cli::JournalAccess::Command
                && contract.guards.contains(&crate::cli::Guard::Resident)
                && contract.path != ["task", "import"]
                && super::typed::kind_for(contract.path).is_none()
        })
        .map(|contract| contract.path.join(" "))
        .collect::<Vec<_>>();
    assert_eq!(unmapped, Vec::<String>::new());
}

#[test]
fn mapping_enforces_the_wire_witness_law() {
    let missing = MappedCommand::new(
        &COMMAND_CONTRACT,
        &["fixture", "command"],
        CommandKind::UpChange,
        Vec::new(),
        &UpChange {
            state: ResidentState::Dark,
        },
    );
    let Err(Outcome::Fault(fault)) = missing else {
        panic!("missing witness must fail mapping");
    };
    assert_eq!(fault.reason, "command::witness-law");
}

#[test]
fn trust_pend_maps_the_current_digest_to_the_existing_pended_witness() {
    let contract = crate::cli::contract(&["trust", "pend"])
        .expect("the generated CLI registry carries trust pend");
    let digest = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        .parse::<wire::Digest>()
        .expect("fixture digest is canonical");
    let mapped = MappedCommand::selected(
        contract,
        SelectedCommand::TrustPend {
            digest: &digest,
            worktree: "/worktrees/producer",
        },
    )
    .expect("trust pend maps before resident discovery");

    assert_eq!(mapped.kind, CommandKind::TrustPend);
    assert_eq!(
        mapped.expected,
        [Witness::Trust {
            digest: digest.clone(),
            standing: wire::TrustState::Pended,
        }],
    );
    assert_eq!(
        wire::decode::<wire::intent::TrustRef>(
            &wire::encode(&mapped.body).expect("mapped body encodes")
        )
        .expect("mapped body retains TrustRef"),
        wire::intent::TrustRef {
            digest,
            worktree: Some("/worktrees/producer".to_owned()),
        },
    );
}

#[test]
fn proof_manifest_maps_to_the_current_observation_head() {
    let contract = crate::cli::contract(&["release", "seal"])
        .expect("the generated CLI registry carries release seal");
    let digest = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
        .parse::<wire::Digest>()
        .expect("fixture digest is canonical");
    let head = Seq::new(17).expect("fixture position is positive");
    let manifest = proof::PublishedManifest {
        manifest: wire::manifest::ReleaseManifest {
            version: wire::manifest::RELEASE_MANIFEST_VERSION,
            source: digest.clone(),
            archive: digest.clone(),
            targets: Vec::new(),
        },
        bytes: Vec::new(),
        digest: digest.clone(),
        pointer: "release/manifest.json".into(),
    };
    let mapped = MappedCommand::selected(
        contract,
        SelectedCommand::ProofRecognize {
            manifest: &manifest,
            head,
        },
    )
    .expect("proof recognition maps before resident discovery");

    assert_eq!(mapped.kind, CommandKind::ProofRecognize);
    assert_eq!(
        mapped.expected,
        [Witness::Observation {
            key: digest.clone(),
            head,
        }],
    );
    assert_eq!(
        wire::decode::<wire::intent::Recognize>(
            &wire::encode(&mapped.body).expect("mapped body encodes")
        )
        .expect("mapped body retains Recognize"),
        wire::intent::Recognize { key: digest },
    );
}

#[test]
fn a_transport_failure_names_the_elapsed_bound_before_it_names_a_direction() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("classifier peer binds");
    let address = listener.local_addr().expect("classifier peer address");
    let bound = crate::cli::APP_ANSWER_BOUND;
    // A later attempt inherits less than the whole bound; faults still name
    // the bound the caller declared.
    let mut stream = DeadlineStream::connect(&address, Instant::now() + bound / 2, bound)
        .expect("classifier peer accepts");

    for (recorded, assumed, reason) in [
        (None, IoDirection::Read, "serve::receive-failed"),
        (None, IoDirection::Write, "serve::send-failed"),
        (
            Some((IoDirection::Read, false)),
            IoDirection::Write,
            "serve::receive-failed",
        ),
        (
            Some((IoDirection::Write, false)),
            IoDirection::Read,
            "serve::send-failed",
        ),
        (
            Some((IoDirection::Read, true)),
            IoDirection::Read,
            "serve::deadline-elapsed",
        ),
        (
            Some((IoDirection::Write, true)),
            IoDirection::Read,
            "serve::deadline-elapsed",
        ),
    ] {
        stream.last_failure = recorded.map(|(direction, deadline_elapsed)| IoFailure {
            direction,
            deadline_elapsed,
        });
        let Outcome::Fault(fault) = transport_fault(&stream, "peer detail", assumed) else {
            panic!("a transport failure must fault");
        };
        assert_eq!(fault.reason, reason);
        assert_eq!(fault.class, FaultClass::Environment);
        let elapsed = reason == "serve::deadline-elapsed";
        assert_eq!(
            fault.detail.contains("20s"),
            elapsed,
            "only an elapsed bound names itself: {fault:?}"
        );
        assert_eq!(
            fault.detail.contains("peer detail"),
            !elapsed,
            "a peer failure keeps its detail; an elapsed bound is the whole story: {fault:?}"
        );
    }
}

#[test]
fn a_delivered_write_timeout_names_its_command_and_waited_authority() {
    let contract = crate::cli::contract(&["room", "contribute"])
        .expect("the registry carries room contribute");
    let outcome = delivered_unanswered(
        environment_fault(
            "serve::deadline-elapsed",
            "resident did not answer within the 20s client bound",
        ),
        contract,
    );

    let Outcome::Fault(fault) = outcome else {
        panic!("an unanswered delivered write must remain a typed fault");
    };
    assert_eq!(fault.reason, "serve::deadline-elapsed");
    assert!(fault.detail.contains("spice room contribute"));
    assert!(fault.detail.contains("resident authority"));
    assert!(
        fault.detail.contains("check its effect"),
        "a delivered write retains its uncertain-effect warning"
    );
}

/// Every command shares the temporary app answer bound.
#[test]
fn the_answer_bound_registry_declares_the_current_resident_answer_bound() {
    for kind in CommandKind::ALL {
        assert_eq!(
            crate::cli::answer_bound(*kind),
            Duration::from_secs(20),
            "{kind} declares the wrong answer bound",
        );
    }
    assert_eq!(OPENING_BOUND, Duration::from_secs(20));
}

#[test]
fn task_completion_position_law_admits_only_handoff_or_terminal_commit() {
    assert!(position_matches(
        ReplyPositionLaw::DirectOrCommitted,
        ReplyPosition::Direct {},
    ));
    assert!(position_matches(
        ReplyPositionLaw::DirectOrCommitted,
        committed(),
    ));
    assert!(!position_matches(
        ReplyPositionLaw::DirectOrCommitted,
        ReplyPosition::Snapshot(wire::Seq::new(1).expect("snapshot sequence")),
    ));
}

#[test]
fn discovery_replacement_is_detected_by_file_identity() {
    let fixture = Fixture::new("replacement");
    let original = fixture.publish();
    let read = DiscoveryRead::open(&fixture.serve, &fixture.root.join("resident"))
        .expect("discovery opens");
    let replacement = serve::Discovery {
        capability: uuid(3),
        epoch: uuid(4),
        ..original
    };
    replacement
        .publish(&fixture.serve)
        .expect("replacement publishes");

    let Err(Outcome::Fault(fault)) = read.verify_current() else {
        panic!("replacement must invalidate the captured discovery");
    };
    assert_eq!(fault.reason, "serve::discovery-replaced");
}

#[test]
fn app_server_opens_the_exact_origin_and_never_crosses_the_capability() {
    let fixture = Fixture::new("serve-open");
    let discovery = fixture.publish();
    let mut launched = None;

    let outcome = open_app_with(
        &fixture.serve,
        || {},
        |endpoint| {
            launched = Some(endpoint.to_owned());
            Ok(())
        },
    );

    assert_eq!(launched.as_deref(), Some(discovery.endpoint.as_str()));
    let Outcome::Reply(Reply { position, body }) = &outcome else {
        panic!("current app discovery must open: {outcome:?}");
    };
    assert_eq!(*position, ReplyPosition::Direct {});
    assert_eq!(
        body,
        &wire::AppOpen {
            endpoint: discovery.endpoint,
            epoch: discovery.epoch,
        }
    );

    let capability = discovery.capability.to_string();
    let text = render::render(&outcome, Form::Text).expect("app receipt renders as text");
    let text_body: wire::AppOpen =
        wire::decode(text.document()).expect("text carries the typed body");
    assert_eq!(text_body, *body);
    assert!(!String::from_utf8_lossy(text.document()).contains(&capability));

    let json = render::render(&outcome, Form::Json).expect("app receipt renders as JSON");
    let json_outcome: Outcome<wire::AppOpen> =
        wire::decode(json.document()).expect("JSON carries the typed outcome");
    assert_eq!(json_outcome, outcome);
    assert!(!String::from_utf8_lossy(json.document()).contains(&capability));
}

#[test]
fn app_server_refuses_missing_malformed_and_unresolved_discovery_without_launching() {
    let fixture = Fixture::new("serve-refusals");
    let mut launches = 0_u8;
    let missing = open_app_with(
        &fixture.serve,
        || {},
        |_| {
            launches += 1;
            Ok(())
        },
    );
    let Outcome::Fault(missing) = missing else {
        panic!("missing discovery must fault");
    };
    assert_eq!(missing.reason, "serve::discovery-unavailable");

    fs::write(&fixture.serve, b"not current discovery").expect("malformed fixture writes");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(&fixture.serve, fs::Permissions::from_mode(0o600))
            .expect("malformed fixture becomes owner-only");
    }
    let malformed = open_app_with(
        &fixture.serve,
        || {},
        |_| {
            launches += 1;
            Ok(())
        },
    );
    let Outcome::Fault(malformed) = malformed else {
        panic!("malformed discovery must fault");
    };
    assert_eq!(malformed.reason, "serve::discovery-malformed");

    // A name is not a socket. The address a client dials has to be the one the
    // resident published, not whatever a resolver answers with at the moment
    // the client asks, so an endpoint that needs resolving is refused unread.
    serve::Discovery {
        endpoint: "http://serve.example:48123".to_owned(),
        capability: uuid(3),
        epoch: uuid(4),
    }
    .publish(&fixture.serve)
    .expect("unresolved fixture publishes");
    let unresolved = open_app_with(
        &fixture.serve,
        || {},
        |_| {
            launches += 1;
            Ok(())
        },
    );
    let Outcome::Fault(unresolved) = unresolved else {
        panic!("named-host discovery must fault");
    };
    assert_eq!(unresolved.reason, "serve::endpoint-invalid");
    assert_eq!(launches, 0, "no refused discovery reached the launcher");
}

/// A surface the operator bound off loopback is still opened, not refused.
///
/// The client once required a loopback endpoint, which would now refuse the
/// exact address `spice up --host` was told to bind: the resident would be
/// running and reachable while every local command declined to dial it. The
/// endpoint has to be one literal socket, and past that the client's business
/// is whether the discovery is current, not which interface it names.
#[test]
fn app_server_opens_a_discovery_naming_an_address_the_operator_bound() {
    let fixture = Fixture::new("serve-routable");
    let mut opened = None;
    serve::Discovery {
        endpoint: "http://192.0.2.1:48123".to_owned(),
        capability: uuid(3),
        epoch: uuid(4),
    }
    .publish(&fixture.serve)
    .expect("routable fixture publishes");
    fixture.hold(uuid(4));

    let outcome = open_app_with(
        &fixture.serve,
        || {},
        |endpoint| {
            opened = Some(endpoint.to_owned());
            Ok(())
        },
    );

    assert!(
        matches!(outcome, Outcome::Reply(_)),
        "a routable discovery opens: {outcome:?}"
    );
    assert_eq!(opened.as_deref(), Some("http://192.0.2.1:48123"));
}

#[test]
fn app_server_rechecks_discovery_identity_immediately_before_launch() {
    let fixture = Fixture::new("serve-stale");
    let original = fixture.publish();
    let replacement = serve::Discovery {
        capability: uuid(5),
        epoch: uuid(6),
        ..original.clone()
    };
    let mut launched = false;

    let outcome = open_app_with(
        &fixture.serve,
        || {
            replacement
                .publish(&fixture.serve)
                .expect("replacement publishes before the launch check");
        },
        |_| {
            launched = true;
            Ok(())
        },
    );

    let Outcome::Fault(fault) = outcome else {
        panic!("replaced discovery must fault");
    };
    assert_eq!(fault.reason, "serve::discovery-replaced");
    assert!(!launched, "stale discovery must not reach the launcher");
    assert!(!fault.detail.contains(&original.capability.to_string()));
}

mod late;
