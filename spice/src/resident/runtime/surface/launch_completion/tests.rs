use crate::resident::lane_surface::LaunchFixture;
use journal::{FactCause, JournalWrite};
use wire::{Actor, Producer};

use super::*;

#[test]
fn a_pending_launch_survives_refused_wakes_and_commits_before_dark() {
    let fixture = LaunchFixture::new();
    let bus = Bus::start(journal::Journal::in_memory().expect("fixture journal opens"))
        .expect("fixture bus starts");
    seed_vacant_lane(&bus, &fixture);
    send_steering(&bus, &fixture);
    let repository = Repository::discover(&fixture.root).expect("fixture repository resolves");
    let startup = crate::resident::load_startup_surface(&repository)
        .expect("fixture startup policy resolves");
    let capabilities = serve::axum::Capabilities::default();
    let discovery = serve::Discovery {
        endpoint: "http://127.0.0.1:1".to_owned(),
        capability: fixture.capability,
        epoch: fixture.epoch,
    };
    crate::resident::steering::publish_lane_surface(
        &fixture.paths,
        &discovery,
        fixture.lane,
        fixture.capability,
    )
    .expect("fixture lane surface publishes");
    let reads = read_materializations::CurrentReads::start_queueing(&bus, &fixture.paths);
    let resources = super::super::restore_resources(&bus, &repository, &startup.config)
        .expect("fixture resource ledger restores");
    let command_surface = command_surface::Surface::new(
        &bus,
        &repository,
        &fixture.paths,
        fixture.epoch,
        runtimes(
            &capabilities,
            &discovery,
            &fixture.reporter(),
            &reads,
            &resources,
        ),
        &startup.config,
        &reads,
    );
    let door_path = crate::resident::supervisor::command::kick_path(&fixture.paths, fixture.lane);
    let door = door::Door::bind(&door_path, 1024).expect("the lane's door binds");
    let kicked = || {
        door.recv(Some(std::time::Duration::ZERO)).ok().map(|kick| {
            serde_json::from_slice::<crate::resident::supervisor::command::Kick>(&kick)
                .expect("a kick")
        })
    };
    let mut seats = Seats::new();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime builds")
        .block_on(async {
            let mut lifecycle = lifecycle::Runtime::new(&startup.config.lane);
            let mut launches = super::super::super::launch_runtime::Runtime::new();
            let mut diagnostic = Vec::new();
            command_surface
                .reconcile_lifecycle(&mut lifecycle, &mut seats, &mut launches, &mut diagnostic)
                .expect("pending steering wakes the lane");
            assert!(
                launches.in_flight(),
                "the wake is still proving its session"
            );
            assert_eq!(lane_row(&bus, &fixture).occupant, None);
            assert_eq!(kicked(), None, "nothing kicks a lane before its start");

            assert_pending_wakes_are_request_local(
                &fixture,
                &bus,
                &command_surface,
                &mut seats,
                &mut lifecycle,
                &mut launches,
            )
            .await;

            settle_before_dark(
                &command_surface,
                &mut seats,
                &mut lifecycle,
                &mut launches,
                &mut diagnostic,
            )
            .await
            .expect("pending launches settle");

            assert!(!launches.in_flight());
            assert!(seats.occupied(fixture.lane));
            assert_eq!(
                kicked(),
                Some(crate::resident::supervisor::command::Kick::Inbox),
                "the committed start kicks the lane's inbox",
            );
            assert_eq!(kicked(), None, "one start kicks once");
        });
    assert_dark_adopts_the_proven_occupant(&bus, &fixture, &mut seats);
    crate::resident::steering::remove_lane_surface(&fixture.paths, fixture.lane);
    drop(door);
    drop(command_surface);
    drop(reads);
    bus.verify().expect("dark settlement authority verifies");
    bus.shutdown().expect("fixture bus stops");
}

fn assert_dark_adopts_the_proven_occupant(bus: &Bus, fixture: &LaunchFixture, seats: &mut Seats) {
    let occupant = lane_row(bus, fixture)
        .occupant
        .expect("the proven occupant committed before dark");
    assert!(matches!(
        crate::resident::shutdown::drive_shutdown(
            bus,
            &fixture.paths,
            fixture.epoch,
            seats,
            wire::Actor::Spice,
        ),
        Outcome::Reply(_)
    ));
    let dark = current_residency(bus, fixture.epoch).expect("residency reads");
    assert_eq!(dark.body().state, wire::ResidentState::Dark);
    assert_eq!(
        lane_row(bus, fixture).occupant,
        Some(occupant),
        "the next generation adopts the occupant from its committed row"
    );
}

async fn assert_pending_wakes_are_request_local(
    fixture: &LaunchFixture,
    bus: &Bus,
    command_surface: &command_surface::Surface<'_>,
    seats: &mut Seats,
    lifecycle: &mut lifecycle::Runtime,
    launches: &mut super::super::super::launch_runtime::Runtime,
) {
    let (server, control) = command_server(fixture, bus).await;
    let address = server.local_addr().expect("command address reads");
    let (shutdown, stopped) = oneshot::channel();
    let serving = tokio::spawn(server.up_until(async move {
        let _ = stopped.await;
    }));
    let epoch = fixture.epoch;
    let lane = fixture.lane;
    let operator = fixture.occupant;
    let client =
        tokio::task::spawn_blocking(move || exercise_commands(address, epoch, lane, operator));
    let mut controls = super::super::control_dispatch::Controls {
        control,
        inbox_deliveries: InboxDeliveries::new(),
        live_changes: lane_surface::LiveChanges::start(&fixture.paths).expect("live changes start"),
        quota: quota::Runtime::start(
            command_surface.config.omp_binary.clone(),
            quota::pin_extension(&fixture.paths.runtime).expect("quota extension pins"),
            fixture.root.clone(),
            &command_surface.config.ambient,
        ),
    };
    for _ in 0..3 {
        let request = tokio::time::timeout(test_support::DEADLOCK_GUARD, controls.control.recv())
            .await
            .expect("the client submits the next command");
        let dispatched = super::super::control_dispatch::dispatch_control(
            command_surface,
            seats,
            lifecycle,
            launches,
            &mut controls,
            request,
            &mut Vec::new(),
        )
        .await
        .expect("a command refusal must not terminate the resident loop");
        assert!(!dispatched.reached_dark);
        assert!(lifecycle.launch_pending(fixture.lane));
        assert!(
            launches.in_flight(),
            "the original launch retains its completion"
        );
    }
    client.await.expect("command client finishes");
    let _ = shutdown.send(());
    serving.await.expect("server joins").expect("server stops");
}

async fn command_server(
    fixture: &LaunchFixture,
    bus: &Bus,
) -> (serve::axum::Server, serve::axum::Control) {
    let app_identity = "5555555555555555555555555555555555555555555555555555555555555555";
    let stage = fixture.root.join(serve::APP_STAGE);
    let generation = stage.join(serve::APP_STAGE_GENERATIONS).join(app_identity);
    fs::create_dir_all(&generation).expect("fixture app generation creates");
    for asset in serve::APP.assets() {
        fs::write(generation.join(asset.file()), asset.file()).expect("fixture app asset writes");
    }
    fs::write(
        stage.join(serve::APP_STAGE_CURRENT),
        format!("{app_identity}\n"),
    )
    .expect("fixture app pointer writes");
    let app = serve::canvas::CanvasAssets::DevelopmentDirectory(
        serve::AppSource::resolve(&fixture.root).expect("fixture app resolves"),
    );
    let version = wire::Version {
        product: "1.2.3".parse().expect("fixture product version"),
        binary: wire::Digest::compute(wire::Domain::DistArtifact, b"fixture native"),
        wire: wire::WireVersion::CURRENT,
        journal: journal::SCHEMA_VERSION,
        app_protocol: wire::AppProtocolVersion::CURRENT,
        app: app.generation().expect("fixture app reads"),
    };
    let roles = bus
        .read(|read| read.principal_roles())
        .expect("fixture roles read");
    let owner = roles.bootstrap().expect("the bootstrap principal is born");
    let capabilities = serve::axum::Capabilities::owned_by(fixture.occupant, owner)
        .expect("the owner's capability binds");
    capabilities.publish(roles).expect("fixture roles publish");
    serve::axum::Server::bind(serve::axum::Generation::new(
        fixture.epoch,
        capabilities,
        version,
        app,
    ))
    .await
    .expect("command server binds")
}

fn exercise_commands(
    address: std::net::SocketAddr,
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    operator: CanonicalUuid,
) {
    use tungstenite::Message;
    use tungstenite::client::IntoClientRequest as _;
    use tungstenite::http::header::ORIGIN;

    let stream = std::net::TcpStream::connect(address).expect("command client connects");
    stream
        .set_read_timeout(Some(test_support::DEADLOCK_GUARD))
        .expect("client deadlock guard sets");
    let mut request = format!("ws://{address}/socket")
        .into_client_request()
        .expect("socket request builds");
    request
        .headers_mut()
        .insert(ORIGIN, format!("http://{address}").parse().expect("origin"));
    let (mut socket, _) = tungstenite::client(request, stream).expect("command socket opens");
    socket
        .send(Message::Binary(
            wire::encode(&wire::Hello {
                wire: wire::WireVersion::CURRENT,
                capability: operator,
                epoch,
            })
            .expect("hello encodes")
            .into(),
        ))
        .expect("hello sends");
    let _: wire::Accepted = wire::decode(&socket.read().expect("accepted frame").into_data())
        .expect("operator authenticates");
    for turn in 0..3 {
        let id: CanonicalUuid = format!("018f82d4-6f59-7d06-92e3-0000000000{:02x}", 70 + turn)
            .parse()
            .expect("command identity");
        let (kind, body) = if turn < 2 {
            (
                wire::CommandKind::LaneState,
                serde_json::to_value(wire::intent::LaneState {
                    lane,
                    action: wire::intent::LaneAction::Wake,
                    worktree: None,
                })
                .expect("wake body encodes"),
            )
        } else {
            (
                wire::CommandKind::LaneRead,
                serde_json::to_value(wire::intent::LaneRead {}).expect("read body encodes"),
            )
        };
        let command = wire::Command {
            v: wire::WireVersion::CURRENT,
            id,
            epoch,
            kind,
            expected: Vec::new(),
            body,
        };
        socket
            .send(Message::Binary(
                wire::encode(&command).expect("command encodes").into(),
            ))
            .expect("command sends");
        let response: wire::Response<serde_json::Value> =
            wire::decode(&socket.read().expect("correlated response").into_data())
                .expect("command response decodes");
        assert_eq!((response.id, response.epoch), (id, epoch));
        if turn < 2 {
            assert!(matches!(response.outcome, Outcome::Refusal(ref refusal)
                if refusal.reason() == "lifecycle::launch-pending"));
        } else {
            let Outcome::Reply(reply) = response.outcome else {
                panic!("lane reads remain available during session proof");
            };
            let answer: wire::intent::LaneAnswer = serde_json::from_value(reply.body)
                .expect("the next command receives the lane table");
            assert_eq!(answer.epoch, epoch);
            assert_eq!(answer.state, wire::ResidentState::Seated);
            let row = answer
                .lanes
                .iter()
                .find(|row| row.lane == lane)
                .expect("the pending lane remains seated");
            assert_eq!(
                row.occupant, None,
                "session proof has not committed a start"
            );
        }
    }
}

fn lane_row(bus: &Bus, fixture: &LaunchFixture) -> wire::LaneResidency {
    current_residency(bus, fixture.epoch)
        .expect("residency reads")
        .body()
        .lanes
        .iter()
        .find(|row| row.lane == fixture.lane)
        .cloned()
        .expect("fixture lane exists")
}

fn seed_vacant_lane(bus: &Bus, fixture: &LaunchFixture) {
    let mut proposal =
        write(Actor::Spice, fixture.epoch, Producer::CommandDoor).expect("fixture write starts");
    let seated = proposal
        .fact(
            wire::body::UpSeated {
                epoch: fixture.epoch,
            },
            FactCause::Root,
        )
        .expect("fixture resident seats");
    let lane = fixture.lane;
    let row = wire::LaneResidency {
        lane,
        team: lane,
        name: None,
        worktree: fixture.root.display().to_string(),
        life: "drive".to_owned(),
        vendor: fixture.vendor.clone(),
        model: None,
        effort: None,
        fast_mode: false,
        occupant: None,
        lineage: Vec::new(),
        capabilities: vec![fixture.capability],
        session: None,
        disposition: "vacant".to_owned(),
    };
    let epoch = fixture.epoch;
    proposal.residency_head(seated, move |_| wire::ResidencyHead {
        epoch,
        state: wire::ResidentState::Seated,
        topology_revision: 1,
        lanes: vec![row],
        subscriptions: vec![wire::TeamSubscription {
            team: lane,
            formed_at: None,
            drive: true,
            drain: false,
            name: None,
            fast_mode: false,
        }],
        renewals: Vec::new(),
    });
    let principal = owner();
    let operator = std::collections::BTreeSet::from([wire::principal::PrincipalRole::Operator]);
    let born = proposal
        .fact(
            wire::principal::PrincipalAdmitted {
                principal,
                profile: wire::principal::PrincipalProfile::default(),
                roles: operator.clone(),
                capability: None,
            },
            FactCause::Root,
        )
        .expect("the fixture's operator principal is born");
    proposal.principal_head(born, move |born| wire::principal::PrincipalHead {
        principal,
        born,
        profile: wire::principal::PrincipalProfile::default(),
        roles: operator,
        capability: None,
    });
    commit(bus, proposal).expect("fixture residency commits");
}

/// The fixture's bootstrap principal, born holding the operator role.
fn owner() -> CanonicalUuid {
    "018f82d4-6f59-7d06-92e3-bce9131163b0"
        .parse()
        .expect("fixture principal")
}

fn send_steering(bus: &Bus, fixture: &LaunchFixture) {
    let epoch = fixture.epoch;
    let residency = current_residency(bus, epoch).expect("residency reads");
    let at = wire::Instant::new(jiff::Timestamp::now()).expect("steering instant");
    let origin: wire::Origin = format!("resident:{epoch}")
        .parse()
        .expect("steering origin");
    let mut sent = JournalWrite::new(
        Actor::Principal(owner()),
        origin.clone(),
        at,
        Producer::CommandDoor,
    );
    let body = wire::body::SteerSent {
        sender: Actor::Principal(owner()),
        room: None,
        directive: "018f82d4-6f59-7d06-92e3-00000000002b"
            .parse()
            .expect("fixture directive"),
        fanout: None,
        recipients: Vec::new(),
        key: "1kJnbzx1x".to_owned(),
        recipient: Actor::Lane(fixture.lane),
        text: "deliver me".to_owned(),
        control: None,
        attachments: Vec::new(),
        expires: "2099-01-01T00:00:00.000Z".parse().expect("fixture expiry"),
    };
    let pending = sent
        .fact(body.clone(), FactCause::Existing(residency.position()))
        .expect("steering fact is valid");
    sent.steering_head(pending, move |seq| {
        steer::Ledger::sent_head(&body, origin, at, seq).expect("steering head is valid")
    });
    commit(bus, sent).expect("pending steering commits");
}
