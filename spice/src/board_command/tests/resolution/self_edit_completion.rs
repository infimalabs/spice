use super::*;

use std::sync::mpsc;
use std::thread;

use sill::{Acquisition, LockFile, LockName};

#[test]
fn a_holder_self_edit_is_retried_by_the_completion_client() {
    let (root, bus, epoch, seats) = fixture("self-edit-completion", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add_task_with_term(
        &bus,
        epoch,
        "EDIT-selfdone1",
        "Edit then complete",
        "/board/test",
        Some("todo"),
        &["the completion lands"],
    );
    claim_task(&bus, epoch, &seats[0], &lane, &task);

    let (outcome, bus) = complete_through_client(
        &root,
        bus,
        epoch,
        &seats[0],
        &task,
        lane.clone(),
        "Current title",
    );

    assert_committed(outcome);
    let completed = board(&bus, epoch, lane);
    assert!(at(&completed, &task).workflow.body.status.is_completed());
    assert_eq!(at(&completed, &task).presentation_title(), "Current title");
    clean(&root, bus);
}

#[test]
fn a_foreign_edit_is_returned_as_witness_movement_by_the_completion_client() {
    let (root, bus, epoch, seats) = fixture("foreign-edit-completion", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add_task_with_term(
        &bus,
        epoch,
        "EDIT-foreign01",
        "Do not cross a foreign edit",
        "/board/test",
        Some("todo"),
        &["the foreign edit remains visible"],
    );
    claim_task(&bus, epoch, &seats[0], &lane, &task);

    let (outcome, bus) = complete_through_client(
        &root,
        bus,
        epoch,
        &seats[0],
        &task,
        Actor::TEST_PRINCIPAL,
        "Operator title",
    );

    assert!(matches!(
        outcome,
        Outcome::Refusal(refusal) if refusal.reason() == "board::witness-moved"
    ));
    let pending = board(&bus, epoch, lane);
    assert!(at(&pending, &task).workflow.body.status.is_pending());
    assert_eq!(at(&pending, &task).presentation_title(), "Operator title");
    clean(&root, bus);
}

/// Run the generated `task done` leaf through its authenticated socket path.
///
/// The server lands `edit_actor`'s edit after returning the client's first
/// board snapshot and before admitting the resulting completion. This gives the
/// real completion client one stale submission without timing or process races.
fn complete_through_client(
    root: &Path,
    bus: Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    task: &TaskHandle,
    edit_actor: Actor,
    title: &'static str,
) -> (Outcome<CommandAnswer>, Bus) {
    fs::create_dir_all(root.join(".git")).expect("completion client fixture is a repository");
    let repository =
        Repository::discover(root).expect("completion client fixture repository resolves");
    let state = repository
        .admit_state()
        .expect("completion client fixture state admits");
    fs::create_dir_all(
        state
            .resident()
            .parent()
            .expect("resident possession has a runtime parent"),
    )
    .expect("completion client runtime exists");
    fs::create_dir_all(state.lane_serve()).expect("lane discovery directory exists");

    let lock = LockFile::new(
        LockName::new("resident").expect("resident is a valid lock name"),
        state.resident(),
    );
    let Acquisition::Acquired(publisher) = lock
        .try_acquire()
        .expect("completion client resident possession opens")
    else {
        panic!("a private completion fixture cannot have a resident contender");
    };
    publisher
        .rewrite_record(
            spice::resident_lock::holder_record(std::process::id(), Some(epoch)).as_bytes(),
        )
        .expect("completion fixture resident generation records");

    let capability = completion_operation();
    let capabilities = serve::axum::Capabilities::default();
    capabilities
        .insert(capability, Actor::Lane(seat.lane))
        .expect("fixture lane capability registers");
    let (app, version) = fixture_app(root);
    let (endpoint, stop_sender, worker) = spawn_completion_server(
        bus,
        epoch,
        serve::axum::Generation::new(epoch, capabilities, version, app),
        task.clone(),
        edit_actor,
        title,
    );
    serve::Discovery {
        endpoint,
        capability,
        epoch,
    }
    .publish(state.lane_serve().join(seat.lane.to_string()))
    .expect("fixture lane discovery publishes");

    let task_argument = task.to_string();
    let matches = crate::cli::command()
        .try_get_matches_from([
            "spice",
            "task",
            "done",
            task_argument.as_str(),
            "--validation",
            "fixture validation",
        ])
        .expect("task done leaf parses");
    let contract = crate::cli::selected_contract(&matches).expect("task done contract is selected");
    let leaf = crate::cli::selected_matches(&matches);
    let outcome =
        crate::command_client::as_lane(seat.lane, || run_at(host(&repository), leaf, contract));

    let _ = stop_sender.send(());
    let bus = worker
        .join()
        .expect("completion fixture server thread joins");
    drop(publisher);
    (outcome, bus)
}

fn spawn_completion_server(
    bus: Bus,
    epoch: CanonicalUuid,
    generation: serve::axum::Generation,
    task: TaskHandle,
    edit_actor: Actor,
    title: &'static str,
) -> (
    String,
    tokio::sync::oneshot::Sender<()>,
    thread::JoinHandle<Bus>,
) {
    let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
    let (stop_sender, stop_receiver) = tokio::sync::oneshot::channel();
    let worker = thread::Builder::new()
        .name("spice-completion-client-test".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("completion fixture runtime builds");
            runtime.block_on(async move {
                let (server, mut control) = serve::axum::Server::bind(generation)
                    .await
                    .expect("completion fixture server binds");
                ready_sender
                    .send(format!(
                        "http://{}",
                        server
                            .local_addr()
                            .expect("completion fixture bound address reads")
                    ))
                    .expect("completion fixture endpoint is observed");
                let serving = tokio::spawn(server.up_until(async move {
                    let _ = stop_receiver.await;
                }));
                let mut board_reads = 0_usize;
                while let Some(request) = control.recv().await {
                    let kind = request.command().kind();
                    let outcome = dispatch(&bus, request.command())
                        .expect("board command owns completion fixture request");
                    request
                        .respond(outcome)
                        .expect("completion fixture client awaits its response")
                        .wait()
                        .await;
                    if kind == CommandKind::BoardRead {
                        board_reads += 1;
                        if board_reads == 1 {
                            edit(&bus, epoch, edit_actor.clone(), &task, title);
                        }
                    }
                }
                serving
                    .await
                    .expect("completion fixture server task joins")
                    .expect("completion fixture server stops cleanly");
                bus
            })
        })
        .expect("completion fixture server thread starts");
    let endpoint = ready_receiver
        .recv()
        .expect("completion fixture server publishes its endpoint");
    (endpoint, stop_sender, worker)
}

fn fixture_app(root: &Path) -> (serve::canvas::CanvasAssets, wire::Version) {
    const GENERATION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let stage = root.join(serve::APP_STAGE);
    let generation = stage.join(serve::APP_STAGE_GENERATIONS).join(GENERATION);
    fs::create_dir_all(&generation).expect("fixture app generation exists");
    for asset in serve::APP.assets() {
        fs::write(
            generation.join(asset.file()),
            format!("fixture {}", asset.file()),
        )
        .expect("fixture app asset writes");
    }
    fs::write(
        stage.join(serve::APP_STAGE_CURRENT),
        format!("{GENERATION}\n"),
    )
    .expect("fixture app current pointer writes");
    let app = serve::canvas::CanvasAssets::DevelopmentDirectory(
        serve::AppSource::resolve(root).expect("fixture app resolves"),
    );
    let version = wire::Version {
        product: "1.2.3".parse().expect("fixture product version"),
        binary: wire::Digest::compute(wire::Domain::DistArtifact, b"fixture native"),
        wire: wire::WireVersion::CURRENT,
        journal: journal::SCHEMA_VERSION,
        app_protocol: wire::AppProtocolVersion::CURRENT,
        app: app.generation().expect("fixture app generation reads"),
    };
    (app, version)
}

fn edit(bus: &Bus, epoch: CanonicalUuid, actor: Actor, task: &TaskHandle, title: &str) {
    let visible = board(bus, epoch, actor.clone());
    assert_committed(issue(
        bus,
        epoch,
        actor,
        CommandKind::TaskEdit,
        vec![statement_witness(at(&visible, task))],
        &TaskEdit {
            title: Some(title.to_owned()),
            ..only(task)
        },
    ));
}
