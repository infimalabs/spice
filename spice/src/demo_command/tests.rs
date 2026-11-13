use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

const FIXTURE_APP_IDENTITY: &str =
    "5555555555555555555555555555555555555555555555555555555555555555";

/// A staged lane app the seed copies into the demo root, so the stage
/// itself lives only as long as the seed that reads it.
fn fixture_app() -> (Scratch, serve::AppSource) {
    let root = Scratch::new("app");
    stage_fixture_app(&root.0, FIXTURE_APP_IDENTITY, "demo fixture app");
    let source = serve::AppSource::resolve(&root.0).expect("demo fixture app resolves");
    (root, source)
}

fn stage_fixture_app(root: &Path, identity: &str, marker: &str) {
    let stage = root.join(serve::APP_STAGE);
    let generation = stage.join(serve::APP_STAGE_GENERATIONS).join(identity);
    fs::create_dir_all(&generation).expect("demo fixture app generation");
    for asset in serve::APP.assets() {
        fs::write(
            generation.join(asset.file()),
            format!("{marker} {}\n", asset.file()),
        )
        .expect("demo fixture app asset");
    }
    fs::write(
        stage.join(serve::APP_STAGE_CURRENT),
        format!("{identity}\n"),
    )
    .expect("demo fixture app pointer");
}

fn seed_test_environment(root: Option<&Path>, executable: &Path) -> DemoResult<Environment> {
    require_utf8(executable, "current spice executable")?;
    let (root, automatic) = prepare_root(root)?;
    let (_stage, app) = fixture_app();
    let result = born_repository(&root)
        .and_then(|()| finish_seed_prepared_root(&root, executable, automatic, &app));
    result.map_err(|failure| preserve_failure(&root, executable, *failure).into())
}

fn seed_test_prepared_root(
    root: &Path,
    executable: &Path,
    automatic: bool,
) -> DemoResult<Environment> {
    born_repository(root)?;
    let (_stage, app) = fixture_app();
    finish_seed_prepared_root(root, executable, automatic, &app)
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "spice-demo-unit-{label}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("demo fixture root is exclusively created");
        Self(root)
    }

    fn child(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn new_seed_copies_the_invoking_linked_worktree_app() {
    const APPLIANCE_IDENTITY: &str =
        "4444444444444444444444444444444444444444444444444444444444444444";
    const LINKED_IDENTITY: &str =
        "6666666666666666666666666666666666666666666666666666666666666666";

    let scratch = Scratch::new("linked-app-source");
    let appliance = scratch.child("appliance-source");
    let common = appliance.join(".git");
    let linked_git = common.join("worktrees/invoking-lane");
    fs::create_dir_all(&linked_git).unwrap();
    stage_fixture_app(&appliance, APPLIANCE_IDENTITY, "appliance app");

    let linked = scratch.child("invoking-lane");
    fs::create_dir(&linked).unwrap();
    fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", linked_git.display()),
    )
    .unwrap();
    fs::write(linked_git.join("commondir"), "../..\n").unwrap();
    stage_fixture_app(&linked, LINKED_IDENTITY, "linked app");
    let nested_start = linked.join("nested/current-directory");
    fs::create_dir_all(&nested_start).unwrap();

    let demo = scratch.child("demo");
    seed_environment_from(Some(&demo), Path::new("/opt/spice"), &nested_start)
        .expect("linked-worktree demo seed succeeds");

    let copied_stage = demo.join(serve::APP_STAGE);
    assert_eq!(
        fs::read_to_string(copied_stage.join(serve::APP_STAGE_CURRENT)).unwrap(),
        format!("{LINKED_IDENTITY}\n")
    );
    assert_eq!(
        fs::read_to_string(
            copied_stage
                .join(serve::APP_STAGE_GENERATIONS)
                .join(LINKED_IDENTITY)
                .join(wire::manifest::APP_MODULE),
        )
        .unwrap(),
        format!("linked app {}\n", wire::manifest::APP_MODULE)
    );
    assert!(
        !copied_stage
            .join(serve::APP_STAGE_GENERATIONS)
            .join(APPLIANCE_IDENTITY)
            .exists()
    );
}

fn assert_seeded_design_history(facts: &journal::FactSnapshot) {
    let output: wire::TaskStamp = DEMO_FINISHED_TASK_STAMP.parse().expect("demo output stamp");
    let decision = facts
        .facts()
        .iter()
        .find_map(|envelope| envelope.fact.read::<wire::rooms::PreparationDecided>())
        .expect("demo Design decision is retained");
    assert_eq!(
        decision
            .decision
            .basis
            .outputs
            .iter()
            .map(|revision| &revision.task)
            .collect::<Vec<_>>(),
        [&output]
    );
    assert!(matches!(
        &decision.decision.outcome,
        wire::rooms::DecisionOutcome::Resolved { outputs, .. }
            if outputs.len() == 1
                && outputs[0].output == output
                && outputs[0].verdict == wire::rooms::OutputVerdict::Accepted
    ));
    let completion = facts
        .facts()
        .iter()
        .find_map(|envelope| {
            envelope
                .fact
                .read::<body::TaskDone>()
                .filter(|done| done.task == output)
        })
        .expect("demo output completion is retained");
    assert_eq!(
        completion.git.as_ref().map(|git| git.integrated.as_str()),
        Some(SEED_COMMIT)
    );
}

fn assert_seeded_principal_identity(facts: &journal::FactSnapshot) {
    let principal = uuid(DEMO_PRINCIPAL);
    let admitted = facts
        .facts()
        .iter()
        .filter_map(|envelope| envelope.fact.read::<wire::principal::PrincipalAdmitted>())
        .collect::<Vec<_>>();
    assert_eq!(admitted.len(), 1);
    assert_eq!(admitted[0].principal, principal);
    assert_eq!(
        admitted[0].roles,
        std::collections::BTreeSet::from([wire::principal::PrincipalRole::Operator])
    );

    let principal_actors = facts
        .facts()
        .iter()
        .filter_map(|envelope| match envelope.actor {
            Actor::Principal(actor) => Some(actor),
            Actor::Spice | Actor::Lane(_) => None,
        })
        .collect::<Vec<_>>();
    assert!(!principal_actors.is_empty());
    assert!(principal_actors.iter().all(|actor| *actor == principal));

    let routed_principals = facts
        .facts()
        .iter()
        .filter_map(|envelope| envelope.fact.read::<wire::rooms::RoomConversed>())
        .flat_map(|conversed| conversed.routes)
        .filter_map(|route| match route.destination {
            wire::rooms::RoomDestination::Principal { principal } => Some(principal),
            wire::rooms::RoomDestination::Lane { .. }
            | wire::rooms::RoomDestination::Task { .. } => None,
        })
        .collect::<Vec<_>>();
    assert!(!routed_principals.is_empty());
    assert!(routed_principals.iter().all(|routed| *routed == principal));

    for sent in facts
        .facts()
        .iter()
        .filter_map(|envelope| envelope.fact.read::<body::SteerSent>())
    {
        for actor in [&sent.sender, &sent.recipient] {
            if let Actor::Principal(routed) = actor {
                assert_eq!(*routed, principal);
            }
        }
    }
}

fn assert_seeded_principal_inputs_reach_room_authority(
    facts: &journal::FactSnapshot,
    reader: &journal::JournalReader,
) {
    let horizon = facts
        .facts()
        .last()
        .expect("the seeded journal is not empty")
        .seq;
    let authority = reader
        .read_with(|read| {
            read.room_authority_page(
                journal::FactPageStart::After(None),
                horizon,
                journal::FactReadBudget::RETAINED,
            )
        })
        .expect("demo room authority reads");
    let sent = authority
        .rows
        .iter()
        .filter_map(|envelope| envelope.fact.read::<body::SteerSent>())
        .map(|body| body.key)
        .collect::<Vec<_>>();
    assert!(sent.iter().any(|key| key == DEMO_FIRST_PRINCIPAL_KEY));
    assert!(sent.iter().any(|key| key == DEMO_SECOND_PRINCIPAL_KEY));
    assert!(authority.rows.iter().any(|envelope| {
        envelope
            .fact
            .read::<body::SteerLapsed>()
            .is_some_and(|body| body.key == DEMO_SECOND_PRINCIPAL_KEY)
    }));
}

#[test]
fn seed_is_deterministic_quoted_and_reopenable_without_touching_parent_state() {
    let scratch = Scratch::new("seed");
    let root = scratch.child("demo root's");
    let executable = Path::new("/opt/spice bin/spice's");
    let environment = seed_test_environment(Some(&root), executable).expect("demo seed succeeds");

    assert_eq!(environment.root, fs::canonicalize(&root).unwrap());
    assert_eq!(
        environment.transcript,
        environment.root.join(TRANSCRIPT_NAME)
    );
    assert!(environment.state.journal().is_file());
    assert_eq!(
        seeded_head(&environment.root).unwrap(),
        format!("{SEED_BRANCH} at {SEED_COMMIT}")
    );
    assert_eq!(
        fs::read(&environment.transcript).unwrap(),
        transcript_bytes().unwrap()
    );
    assert!(environment.launch_command.starts_with("cd '"));
    assert!(environment.launch_command.contains("'\"'\"'"));
    assert!(environment.launch_command.contains(" demo --reuse "));
    assert_eq!(environment.remove_command.matches("rm -rf --").count(), 1);

    let reader = journal::JournalReader::open(environment.state.journal()).unwrap();
    let facts = reader.fact_snapshot().unwrap();
    let kinds = facts
        .facts()
        .iter()
        .map(|envelope| envelope.fact.kind())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == wire::FactKind::RoomPublished)
            .count(),
        CANNED_TRANSCRIPT.len()
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == wire::FactKind::LaneSeated)
            .count(),
        DEMO_LANES.len() + DEMO_TRANSCRIPT_SEATS.len()
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == wire::FactKind::LaneStarted)
            .count(),
        DEMO_LANES.len() + DEMO_TRANSCRIPT_SEATS.len()
    );
    for seed in DEMO_LANES {
        assert!(facts.facts().iter().any(|envelope| {
            envelope
                .fact
                .read::<body::LaneStarted>()
                .is_some_and(|started| {
                    started.lane == uuid(seed.lane)
                        && started.occupant == uuid(seed.occupant)
                        && started.epoch == uuid(DEMO_EPOCH)
                })
        }));
    }
    assert!(facts.facts().iter().any(|envelope| {
        envelope
            .fact
            .read::<body::LaneExited>()
            .is_some_and(|exited| {
                exited.lane == uuid(DEMO_MESSAGE_LANE)
                    && exited.occupant == uuid(DEMO_MESSAGE_OCCUPANT)
            })
    }));
    let scan = listen::scan(
        environment
            .state
            .lane_tape(uuid(DEMO_MESSAGE_LANE))
            .join("omp"),
        listen::Dialect::Omp,
        None,
    )
    .expect("demo message tape scans");
    assert!(scan.records.iter().any(|record| {
        let listen::RecordPayload::Events(events) = &record.payload else {
            return false;
        };
        events.iter().any(|event| {
            matches!(
                &event.event,
                listen::Event::Prose(prose) | listen::Event::StreamedProse(prose)
                    if prose.text.contains(DEMO_FIRST_CONCERN_KEY)
            )
        })
    }));
    let claim = facts
        .facts()
        .iter()
        .find_map(|envelope| envelope.fact.read::<body::ClaimWon>())
        .expect("Ava's demo claim is retained");
    assert_eq!(claim.task, DEMO_STAMP.parse().expect("demo task stamp"));
    assert_eq!(claim.actor, Actor::Lane(uuid(DEMO_LANE)));
    assert_eq!(claim.lane, uuid(DEMO_LANE));
    assert_eq!(claim.occupant, uuid(DEMO_OCCUPANT));
    assert_eq!(claim.epoch, uuid(DEMO_EPOCH));
    assert_seeded_design_history(&facts);
    assert_seeded_principal_identity(&facts);
    assert_seeded_principal_inputs_reach_room_authority(&facts, &reader);
    assert_seeded_peer_lines(&facts, &reader);

    let reopened = reopen_environment(&root, executable).expect("complete seed reopens");
    assert_eq!(reopened.root, environment.root);
    assert_eq!(reopened.launch_command, environment.launch_command);
}

/// Iris and Theo hold one line opened, continued and closed, one declined,
/// and one opened by a packet Juno's departed lane heard nothing of, and Iris
/// recorded once in the task's room without reaching anyone.
fn assert_seeded_peer_lines(facts: &journal::FactSnapshot, reader: &journal::JournalReader) {
    use wire::body::{PeerBodyKind, PeerLineAction};
    let name = |lane: CanonicalUuid| {
        DEMO_TRANSCRIPT_SEATS
            .into_iter()
            .find(|seat| uuid(seat.lane) == lane)
            .map_or("unknown", |seat| seat.name)
    };
    let messaged = facts
        .facts()
        .iter()
        .filter_map(|envelope| envelope.fact.read::<body::SteerMessaged>())
        .collect::<Vec<_>>();
    assert_eq!(
        messaged
            .iter()
            .map(|message| (name(message.from), name(message.lane), message.peer_body))
            .collect::<Vec<_>>(),
        [
            ("Iris", "Theo", Some(PeerBodyKind::Open)),
            ("Theo", "Iris", Some(PeerBodyKind::Continue)),
            ("Iris", "Theo", Some(PeerBodyKind::Final)),
            ("Theo", "Iris", Some(PeerBodyKind::Open)),
            ("Iris", "Theo", Some(PeerBodyKind::Decline)),
            ("Iris", "Theo", Some(PeerBodyKind::Open)),
            ("Theo", "Iris", Some(PeerBodyKind::Final)),
        ]
    );
    assert!(
        !facts
            .facts()
            .iter()
            .any(|envelope| envelope.fact.kind() == wire::FactKind::SteerQueued),
        "the departed lane's address queues nothing"
    );
    let roots = messaged
        .iter()
        .filter(|message| message.peer_body == Some(PeerBodyKind::Open))
        .map(|message| message.directive)
        .collect::<Vec<_>>();
    let closed = reader
        .read(move |read| {
            roots
                .into_iter()
                .map(|root| {
                    Ok(read
                        .peer_line_head(root)?
                        .and_then(|line| line.terminal().map(|terminal| terminal.action)))
                })
                .collect::<Result<Vec<_>, journal::GuardError>>()
        })
        .unwrap();
    assert_eq!(
        closed,
        [
            Some(PeerLineAction::Finish),
            Some(PeerLineAction::Decline),
            Some(PeerLineAction::Finish),
        ]
    );
    let room_messages = facts
        .facts()
        .iter()
        .filter_map(|envelope| envelope.fact.read::<wire::rooms::RoomMessaged>())
        .collect::<Vec<_>>();
    let [room] = room_messages
        .iter()
        .filter(|room| room.author == uuid(DEMO_IRIS_SEAT.occupant))
        .collect::<Vec<_>>()
        .try_into()
        .expect("Iris records once in the task's room");
    assert!(room.directed.is_empty(), "a room record reaches nobody");
    let nora = room_messages
        .iter()
        .filter(|room| room.author == uuid(DEMO_MESSAGE_OCCUPANT))
        .collect::<Vec<_>>();
    assert_eq!(nora.len(), 2, "Nora's inbox conversations reach the room");
    assert!(
        nora.iter().all(|room| room.directed.is_empty()),
        "principal inbox routes open no lane directive"
    );
}

#[test]
fn seeded_roots_are_born_on_one_commit_wherever_they_are_created() {
    let scratch = Scratch::new("born");
    let first = scratch.child("first root");
    let second = scratch.child("a second root under a much longer name");
    for root in [&first, &second] {
        seed_test_environment(Some(root), Path::new("/opt/spice"))
            .expect("born demo seed succeeds");
    }

    let born = "refs/heads/main at a53ad58b1356a5f7abea7437975321850e04b217";
    assert_eq!(
        seeded_head(&first).unwrap(),
        born,
        "the first seeded root is not born on the deterministic commit"
    );
    assert_eq!(
        seeded_head(&second).unwrap(),
        born,
        "a longer root path moved the seeded commit"
    );
    assert_eq!(
        format!("{SEED_BRANCH} at {SEED_COMMIT}"),
        born,
        "reuse requires descent from a commit the seed never writes"
    );
}

/// Advance a seeded root the way working lanes do: a commit on its branch, an
/// edited repository policy, and a residency that seats one more lane.
fn advance_seeded_root(environment: &Environment) {
    let repository = gix::open(&environment.root).unwrap();
    let tree = repository.write_object(gix::objs::Tree::default()).unwrap();
    let identity = gix::actor::SignatureRef {
        name: "lane".into(),
        email: "lane@spice.invalid".into(),
        time: "1783746000 +0000",
    };
    let seed = gix::ObjectId::from_hex(SEED_COMMIT.as_bytes()).unwrap();
    repository
        .commit_as(identity, identity, SEED_BRANCH, "lane work\n", tree, [seed])
        .unwrap();
    fs::write(
        environment.root.join(wire::TRACKED_POLICY_FILE),
        b"[judge]\nenabled = true\n",
    )
    .unwrap();

    let reader = journal::JournalReader::open(environment.state.journal()).unwrap();
    let mut head = reader
        .read(|read| read.residency_head())
        .unwrap()
        .expect("the seed seats a resident")
        .body()
        .clone();
    drop(reader);
    let probe = uuid("018f82d4-6f59-7d06-92e3-bce913116418");
    let capability = uuid("018f82d4-6f59-7d06-92e3-bce913116419");
    let mut lane = head.lanes[0].clone();
    lane.lane = probe;
    lane.name = Some("Probe".to_owned());
    lane.capabilities = vec![capability];
    lane.occupant = None;
    lane.lineage.clear();
    head.topology_revision += 1;
    head.lanes.push(lane);

    let mut journal = journal::Journal::open(environment.state.journal()).unwrap();
    let mut write = JournalWrite::new(
        Actor::Spice,
        format!("resident:{DEMO_EPOCH}").parse().unwrap(),
        instant("2026-07-11T06:00:00.000000Z"),
        Producer::ResidentRecognition,
    );
    let seated = write
        .fact(
            body::LaneSeated {
                lane: probe,
                team: uuid(DEMO_TEAM),
                capability,
                name: Some("Probe".to_owned()),
                worktree: environment.root.display().to_string(),
                life: "drive".to_owned(),
                vendor: crate::resident::SIMULATED_VENDOR.parse().unwrap(),
                model: None,
                effort: None,
            },
            FactCause::Root,
        )
        .unwrap();
    write.residency_head(seated, move |_| head);
    journal.apply(write).unwrap();
}

#[test]
fn reuse_admits_a_root_its_lanes_advanced_but_not_one_grown_from_another_seed() {
    let scratch = Scratch::new("advanced");
    let root = scratch.child("advanced-root");
    let executable = Path::new("/opt/spice");
    let environment =
        seed_test_environment(Some(&root), executable).expect("advanced-root seed succeeds");
    advance_seeded_root(&environment);
    let reopened = reopen_environment(&root, executable).expect("an advanced demo root reopens");
    assert_eq!(reopened.root, environment.root);

    let head = environment.root.join(".git/HEAD");
    let branch = environment.root.join(".git/refs/heads/main");
    let advanced = fs::read(&branch).unwrap();
    let refuses = |observed: &str| {
        let outcome = *reopen_environment(&root, executable).expect_err("a foreign head refuses");
        let Outcome::Refusal(refusal) = outcome else {
            panic!("a foreign head must refuse: {outcome:?}");
        };
        assert_eq!(refusal.reason(), "demo::commit-moved");
        let diagnostic = refusal.diagnostic().expect("demo refusal is diagnostic");
        assert_eq!(diagnostic.observed(), Some(observed));
        assert_eq!(
            diagnostic.detail(),
            Some("check out a commit descending from the seeded commit or seed a new one")
        );
    };

    let repository = gix::open(&environment.root).unwrap();
    let tree = repository.write_object(gix::objs::Tree::default()).unwrap();
    let identity = gix::actor::SignatureRef {
        name: "other".into(),
        email: "other@spice.invalid".into(),
        time: "1783746000 +0000",
    };
    let foreign = repository
        .commit_as(
            identity,
            identity,
            "refs/heads/foreign",
            "another root\n",
            tree,
            gix::commit::NO_PARENT_IDS,
        )
        .unwrap();
    fs::write(&head, "ref: refs/heads/foreign\n").unwrap();
    refuses(&format!("refs/heads/foreign at {foreign}"));

    fs::write(&head, format!("ref: {SEED_BRANCH}\n")).unwrap();
    fs::write(&branch, "9c3f4bd0bd0b1e2ee59a1e0e9d2b47a3f7c4a5d6\n").unwrap();
    refuses("refs/heads/main at 9c3f4bd0bd0b1e2ee59a1e0e9d2b47a3f7c4a5d6");

    fs::write(&head, "ref: refs/heads/operator\n").unwrap();
    refuses("refs/heads/operator at no commit");

    fs::write(&head, format!("ref: {SEED_BRANCH}\n")).unwrap();
    fs::write(&branch, &advanced).unwrap();
    assert!(
        reopen_environment(&root, executable).is_ok(),
        "returning to the seed's descendant restores a reusable demo root"
    );
}

#[test]
fn reuse_refuses_roots_whose_demo_identity_does_not_match() {
    let scratch = Scratch::new("identity");
    let executable = Path::new("/opt/spice");
    let refusal_of = |root: &Path| {
        let outcome = *reopen_environment(root, executable).expect_err("a foreign root refuses");
        let Outcome::Refusal(refusal) = outcome else {
            panic!("a foreign root must refuse: {outcome:?}");
        };
        assert!(
            refusal
                .diagnostic()
                .and_then(|diagnostic| diagnostic.detail())
                .is_some(),
            "{} names no repair",
            refusal.reason()
        );
        refusal.reason().to_owned()
    };

    let plain = scratch.child("plain repository");
    fs::create_dir(&plain).unwrap();
    born_repository(&plain).unwrap();
    assert_eq!(refusal_of(&plain), "demo::seed-unrecognized");

    let transcript = scratch.child("edited transcript");
    let environment = seed_test_environment(Some(&transcript), executable).unwrap();
    fs::write(&environment.transcript, b"{}\n").unwrap();
    assert_eq!(refusal_of(&transcript), "demo::transcript-moved");

    let reseeded = scratch.child("foreign journal");
    let environment = seed_test_environment(Some(&reseeded), executable).unwrap();
    let journal = environment.state.journal();
    for suffix in ["", "-wal", "-shm"] {
        let mut path = journal.as_os_str().to_owned();
        path.push(suffix);
        let _ = fs::remove_file(PathBuf::from(path));
    }
    drop(journal::Journal::open(journal).unwrap());
    assert_eq!(refusal_of(&reseeded), "demo::seed-moved");
}

#[test]
fn new_seed_refuses_nonempty_and_non_directory_roots_without_mutation() {
    let scratch = Scratch::new("root-refusal");
    let nonempty = scratch.child("operator repository");
    fs::create_dir(&nonempty).unwrap();
    let sentinel = nonempty.join("sentinel.txt");
    fs::write(&sentinel, b"operator bytes\n").unwrap();
    let file = scratch.child("operator-file");
    fs::write(&file, b"also operator bytes\n").unwrap();

    for root in [&nonempty, &file] {
        let outcome = *seed_test_environment(Some(root), Path::new("/opt/spice"))
            .expect_err("protected root refuses");
        let Outcome::Refusal(refusal) = outcome else {
            panic!("protected root must refuse: {outcome:?}");
        };
        assert!(matches!(
            refusal.reason(),
            "demo::root-not-empty" | "demo::root-refused"
        ));
    }
    assert_eq!(fs::read(&sentinel).unwrap(), b"operator bytes\n");
    assert_eq!(fs::read(&file).unwrap(), b"also operator bytes\n");
}

#[cfg(unix)]
#[test]
fn non_utf8_executable_refuses_before_the_demo_root_exists() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    let scratch = Scratch::new("executable-refusal");
    let root = scratch.child("uncreated-root");
    let executable = PathBuf::from(OsString::from_vec(b"/opt/spice-\xff".to_vec()));
    let outcome = *seed_test_environment(Some(&root), &executable)
        .expect_err("non-reproducible executable path refuses");

    let Outcome::Refusal(refusal) = outcome else {
        panic!("non-UTF-8 executable path must refuse: {outcome:?}");
    };
    assert_eq!(refusal.reason(), "demo::path-refused");
    assert!(!root.exists(), "path refusal must precede seed mutation");
}

#[test]
fn finish_app_preserves_failures_and_interruptions_and_scopes_cleanup() {
    let scratch = Scratch::new("finish-app");
    let root = scratch.child("automatic-root");
    fs::create_dir(&root).expect("the automatic root is created");
    let environment = seed_test_prepared_root(&root, Path::new("/opt/spice"), true)
        .expect("the automatic demo seed succeeds");
    let endpoint = "http://127.0.0.1:1234".to_owned();

    let failure = environment_fault("demo::fixture-failed", "fixture failure");
    let Outcome::Fault(fault) =
        finish_app(&environment, true, endpoint.clone(), Err(Box::new(failure)))
    else {
        panic!("a failed demo preserves its typed failure");
    };
    assert_eq!(fault.reason, "demo::fixture-failed");
    assert!(fault.detail.contains("demo root preserved"));
    assert!(root.exists(), "failure preserves the recovery root");

    let Outcome::Reply(interrupted) = finish_app(&environment, true, endpoint.clone(), Ok(true))
    else {
        panic!("an interrupted demo replies with recovery");
    };
    assert_eq!(interrupted.body.lifecycle, "interrupted-preserved");
    assert!(root.exists(), "interruption preserves the recovery root");

    let Outcome::Reply(clean) = finish_app(&environment, true, endpoint, Ok(false)) else {
        panic!("a clean automatic demo replies");
    };
    assert_eq!(clean.body.lifecycle, "removed");
    assert!(!root.exists(), "clean completion removes the demo root");
    assert!(scratch.0.exists(), "cleanup stays scoped to the demo root");
}

#[test]
fn disappearing_known_publication_files_are_a_benign_readiness_change() {
    let scratch = Scratch::new("transient-journal");
    let root = scratch.child("automatic-root");
    fs::create_dir(&root).expect("the automatic demo root is created");
    let environment = seed_test_prepared_root(&root, Path::new("/opt/spice"), true)
        .expect("the automatic demo root is seeded");
    assert!(environment.automatic);
    let journal = environment.state.journal();
    let sidecar = journal.with_extension("sqlite3-journal");
    fs::write(&sidecar, []).expect("the journal sidecar is discovered");
    let error =
        notify::Error::io(io::Error::from(io::ErrorKind::NotFound)).add_path(sidecar.clone());

    let destination = atomic::Destination::new(root.join("published")).expect("atomic destination");
    let staged = destination.stage().expect("atomic publication is staged");
    let temporary = staged.temporary_path().to_path_buf();

    fs::remove_file(&sidecar).expect("the journal sidecar disappears before inspection");
    drop(staged);

    assert_eq!(
        readiness_change(Err(error.add_path(temporary)), journal),
        Ok(())
    );
}

#[test]
fn persistent_and_unrelated_readiness_scan_failures_are_preserved() {
    let scratch = Scratch::new("readiness-errors");
    let journal = scratch.child("spicejournal.sqlite3");
    let sidecar = journal.with_extension("sqlite3-journal");
    fs::write(&sidecar, []).expect("the persistent journal sidecar exists");

    let persistent = readiness_change(
        Err(notify::Error::io(io::Error::from(io::ErrorKind::NotFound)).add_path(sidecar.clone())),
        &journal,
    )
    .expect_err("a persistent path is not dismissed as a disappearance race");
    assert!(persistent.contains(&sidecar.display().to_string()));

    let destination = atomic::Destination::new(scratch.child("published"))
        .expect("persistent atomic destination");
    let staged = destination.stage().expect("persistent atomic stage");
    let temporary = staged.temporary_path().to_path_buf();
    let persistent_atomic =
        readiness_change(
            Err(notify::Error::io(io::Error::from(io::ErrorKind::NotFound))
                .add_path(temporary.clone())),
            &journal,
        )
        .expect_err("a persistent atomic path is not dismissed as a disappearance race");
    assert!(persistent_atomic.contains(&temporary.display().to_string()));
    drop(staged);

    fs::remove_file(&sidecar).expect("the journal sidecar is absent for the I/O fixture");
    let unrelated_io = readiness_change(
        Err(
            notify::Error::io(io::Error::from(io::ErrorKind::PermissionDenied))
                .add_path(sidecar.clone()),
        ),
        &journal,
    )
    .expect_err("an unrelated I/O error remains a readiness failure");
    assert!(unrelated_io.contains(&sidecar.display().to_string()));

    let unrelated_path = scratch.child("unrelated.tmp");
    let unrelated_missing = readiness_change(
        Err(notify::Error::io(io::Error::from(io::ErrorKind::NotFound))
            .add_path(unrelated_path.clone())),
        &journal,
    )
    .expect_err("an unrelated missing path remains a readiness failure");
    assert!(unrelated_missing.contains(&unrelated_path.display().to_string()));

    let malformed_atomic = scratch.child(".spice-atomic-01-2.tmp");
    let malformed_missing = readiness_change(
        Err(notify::Error::io(io::Error::from(io::ErrorKind::NotFound))
            .add_path(malformed_atomic.clone())),
        &journal,
    )
    .expect_err("a malformed atomic lookalike remains a readiness failure");
    assert!(malformed_missing.contains(&malformed_atomic.display().to_string()));
}
