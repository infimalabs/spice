use std::fs;

use lane::occupant::DetachedWait;

use super::*;

fn uuid(last: u8) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-bce9131165{last:02}")
        .parse()
        .expect("fixture UUID")
}

fn paths(root: &Path) -> RuntimePaths {
    RuntimePaths {
        worktree: root.to_path_buf(),
        common: root.join("common"),
        runtime: root.join("runtime"),
        live: root.join("runtime").join("runtime"),
        data: root.join("data"),
        journal: root.join("journal"),
        resident: root.join("resident"),
        serve: root.join("serve"),
        lane_serve: root.join("lane-serve"),
        degraded: root.join("degraded"),
        tape: root.join("lanes"),
        supervisors: root.join("lanes"),
        steering_media: root.join("lanes"),
        stage: root.join("stage"),
    }
}

fn terminal_state(
    root: &Path,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    session: wire::LaneSession,
    completion: PersistedCompletion,
) -> State {
    State {
        version: STATE_VERSION,
        supervisor: std::process::id(),
        worktree: root.to_string_lossy().into_owned(),
        vendor: "omp".to_owned(),
        lane,
        occupant,
        phase: Phase::Exited,
        group: Some(412),
        session: Some(session),
        claim: None,
        git: Some("fixture-head".to_owned()),
        started_millis: Some(now_millis().expect("fixture time")),
        runtime_millis: Some(4 * 60 * 60 * 1_000),
        grace_millis: Some(20_000),
        reap_millis: Some(20_000),
        completion: CompletionField::some(completion),
        failure: None,
        capture_drained: false,
    }
}

#[test]
fn replacement_adopts_current_running_and_terminal_supervisors() {
    let root = std::env::temp_dir().join(format!(
        "spice-terminal-supervisor-{}-{}",
        std::process::id(),
        now_millis().expect("fixture time")
    ));
    let paths = paths(&root);
    let lane = uuid(1);
    let occupant = uuid(2);
    let directory = supervisor_directory(&paths, lane, occupant);
    fs::create_dir_all(&directory).expect("supervisor directory");
    let session = wire::LaneSession {
        lane,
        occupant,
        worktree: root.to_string_lossy().into_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        launch: wire::body::LaneLaunchIdentity::new("sonnet", "high"),
        epoch: uuid(3),
        stream: "stdout".to_owned(),
        identity: "terminal-session".to_owned(),
        locus: wire::TapeLocus {
            member: 0,
            offset: 0,
            length: 1,
        },
    };
    let row = wire::LaneResidency {
        lane,
        team: lane,
        name: None,
        worktree: root.to_string_lossy().into_owned(),
        life: "drive".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        model: None,
        effort: None,
        fast_mode: false,
        occupant: Some(occupant),
        lineage: vec![occupant],
        capabilities: vec![uuid(4)],
        session: Some(session),
        disposition: "occupied".to_owned(),
    };

    for expected in [
        PersistedCompletion::Exited { status_raw: 0 },
        PersistedCompletion::TimedOut { status_raw: 0 },
        PersistedCompletion::Cancelled { status_raw: 7 << 8 },
    ] {
        let terminal = terminal_state(
            &root,
            lane,
            occupant,
            row.session.clone().expect("fixture session"),
            expected.clone(),
        );
        let mut running = terminal.clone();
        running.phase = Phase::Running;
        running.completion = CompletionField::none();
        let path = directory.join("state.json");
        publish_json(&path, &running).expect("running state publishes");
        let adopted = adopt(&paths, &row)
            .expect("running state reads")
            .expect("running supervisor is adoptable");
        let Adoption::Running(adopted) = adopted else {
            panic!("live supervisor was not adopted");
        };
        assert_eq!(adopted.occupant, occupant);
        assert_eq!(adopted.group, 412);
        assert_eq!(
            adopted.session,
            row.session.clone().expect("fixture session")
        );
        let DetachedWait::Running(process) = adopted
            .process
            .wait_for(Duration::ZERO)
            .expect("adopted supervisor remains running")
        else {
            panic!("live supervisor was mistaken for a terminal state");
        };

        publish_json(&path, &terminal).expect("terminal state publishes");
        let outcome = process.wait().expect("adopted supervisor completion reads");
        assert_eq!(PersistedCompletion::capture(outcome.completion()), expected);
        let adopted = adopt(&paths, &row)
            .expect("terminal state reads")
            .expect("terminal supervisor is adoptable");
        let Adoption::Exited(adopted) = adopted else {
            panic!("terminal supervisor was mistaken for a live launch");
        };
        assert_eq!(adopted.occupant, occupant);
        assert_eq!(adopted.group, 412);
        let outcome = adopted.process.wait().expect("terminal status reads");
        assert_eq!(PersistedCompletion::capture(outcome.completion()), expected);
    }
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn every_typed_completion_round_trips_without_conflating_code_zero() {
    let root = std::env::temp_dir();
    let lane = uuid(5);
    let occupant = uuid(6);
    let session = wire::LaneSession {
        lane,
        occupant,
        worktree: root.to_string_lossy().into_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        launch: wire::body::LaneLaunchIdentity::new("sonnet", "high"),
        epoch: uuid(7),
        stream: "stdout".to_owned(),
        identity: "round-trip-session".to_owned(),
        locus: wire::TapeLocus {
            member: 0,
            offset: 0,
            length: 1,
        },
    };
    let completions = [
        PersistedCompletion::Exited { status_raw: 0 },
        PersistedCompletion::TimedOut { status_raw: 0 },
        PersistedCompletion::Cancelled { status_raw: 7 << 8 },
    ];

    for expected in completions {
        let state = terminal_state(&root, lane, occupant, session.clone(), expected.clone());
        let bytes = serde_json::to_vec(&state).expect("state encodes");
        let decoded: State = serde_json::from_slice(&bytes).expect("state decodes");
        let decoded = decoded.validate().expect("state validates");
        assert_eq!(decoded.completion.as_ref(), Some(&expected));
    }
    for phase in [Phase::Starting, Phase::Running, Phase::Failed] {
        let mut state = terminal_state(
            &root,
            lane,
            occupant,
            session.clone(),
            PersistedCompletion::Exited { status_raw: 0 },
        );
        state.phase = phase;
        state.completion = CompletionField::none();
        let value = serde_json::to_value(&state).expect("nonterminal state encodes");
        assert!(
            value
                .get("completion")
                .is_some_and(serde_json::Value::is_null),
            "{phase:?} state omitted its required null completion"
        );
        serde_json::from_value::<State>(value)
            .expect("required null completion decodes")
            .validate()
            .expect("nonterminal null completion validates");
    }
}

#[test]
fn current_state_refuses_missing_unknown_and_phase_mismatched_completion() {
    let root = std::env::temp_dir().join(format!(
        "spice-invalid-supervisor-{}-{}",
        std::process::id(),
        now_millis().expect("fixture time")
    ));
    fs::create_dir_all(&root).expect("fixture directory creates");
    let lane = uuid(8);
    let occupant = uuid(9);
    let session = wire::LaneSession {
        lane,
        occupant,
        worktree: root.to_string_lossy().into_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        launch: wire::body::LaneLaunchIdentity::new("sonnet", "high"),
        epoch: uuid(10),
        stream: "stdout".to_owned(),
        identity: "invalid-session".to_owned(),
        locus: wire::TapeLocus {
            member: 0,
            offset: 0,
            length: 1,
        },
    };
    let valid = serde_json::to_value(terminal_state(
        &root,
        lane,
        occupant,
        session,
        PersistedCompletion::Exited { status_raw: 0 },
    ))
    .expect("valid state encodes");
    let cases = [
        ("missing", {
            let mut value = valid.clone();
            value
                .as_object_mut()
                .expect("state is an object")
                .remove("completion");
            value
        }),
        ("exited-null", {
            let mut value = valid.clone();
            value["completion"] = serde_json::Value::Null;
            value
        }),
        ("running-terminal", {
            let mut value = valid.clone();
            value["phase"] = serde_json::Value::String("running".to_owned());
            value
        }),
        ("unknown-kind", {
            let mut value = valid.clone();
            value["completion"]["kind"] = serde_json::Value::String("status_only".to_owned());
            value
        }),
        ("status-only", {
            let mut value = valid.clone();
            value
                .as_object_mut()
                .expect("state is an object")
                .remove("completion");
            value["status_raw"] = serde_json::Value::from(0);
            value
        }),
    ];
    let path = root.join("state.json");

    for (label, value) in &cases {
        let mut value = value.clone();
        value["version"] = serde_json::Value::from(STATE_VERSION);
        fs::write(
            &path,
            serde_json::to_vec(&value).expect("invalid fixture encodes"),
        )
        .expect("invalid fixture writes");
        let error = read_state(&path)
            .err()
            .unwrap_or_else(|| panic!("{label} state reached adoption"));
        assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{label}");
    }
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn newer_state_versions_are_refused_without_an_execution_path() {
    let root = std::env::temp_dir().join(format!(
        "spice-newer-supervisor-{}-{}",
        std::process::id(),
        now_millis().expect("fixture time")
    ));
    fs::create_dir_all(&root).expect("fixture directory creates");
    let state = root.join("state.json");
    let current = serde_json::to_value(State {
        version: STATE_VERSION,
        supervisor: std::process::id(),
        worktree: root.to_string_lossy().into_owned(),
        vendor: "omp".to_owned(),
        lane: uuid(1),
        occupant: uuid(2),
        phase: Phase::Starting,
        group: None,
        session: None,
        claim: None,
        git: None,
        started_millis: None,
        runtime_millis: None,
        grace_millis: None,
        reap_millis: None,
        completion: CompletionField::none(),
        failure: None,
        capture_drained: false,
    })
    .expect("current state encodes");
    for version in [STATE_VERSION + 1, STATE_VERSION + 2] {
        let mut retained = current.clone();
        retained["version"] = serde_json::Value::from(version);
        fs::write(
            &state,
            serde_json::to_vec(&retained).expect("unsupported fixture encodes"),
        )
        .expect("unsupported fixture writes");

        let error = match read_state(&state) {
            Ok(_) => panic!("v{version} state has no current execution path"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let diagnostic = error.to_string();
        assert!(
            diagnostic.contains(&format!("contract v{version}")),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains(&format!(
                "current Spice {} contract v{STATE_VERSION}",
                env!("CARGO_PKG_VERSION"),
            )),
            "{diagnostic}"
        );
    }
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn launch_outcomes_publish_their_cli_text_not_debug_syntax() {
    assert_eq!(
        outcome_error(store_fault(
            "lane::native-shell-unavailable",
            "the supervised environment has no native SHELL",
        ))
        .to_string(),
        "lane::native-shell-unavailable; the supervised environment has no native SHELL"
    );
    let refusal = wire::Outcome::Refusal(wire::Refusal::bare(
        wire::RefusalDiagnostic::new("config::value-invalid", None, None, None)
            .with_detail("vendor.omp.binary must be a string"),
    ));
    assert_eq!(
        outcome_error(refusal).to_string(),
        "config::value-invalid; vendor.omp.binary must be a string"
    );
}

fn presence_fixture(label: &str) -> (PathBuf, RuntimePaths, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "spice-supervisor-presence-{label}-{}-{}",
        std::process::id(),
        now_millis().expect("fixture time")
    ));
    let paths = paths(&root);
    let state = supervisor_directory(&paths, uuid(11), uuid(12)).join("state.json");
    fs::create_dir_all(&paths.common).expect("repository metadata directory creates");
    fs::create_dir_all(state.parent().expect("state has a parent"))
        .expect("supervisor state directory creates");
    (root, paths, state)
}

fn possess_resident_lock(paths: &RuntimePaths) -> sill::Possession {
    let resident = sill::LockFile::new(
        sill::LockName::new("resident").expect("fixture lock name"),
        paths.resident.clone(),
    );
    let sill::Acquisition::Acquired(possession) =
        resident.try_acquire().expect("resident lock probes")
    else {
        panic!("fixture resident lock was unexpectedly held");
    };
    possession
}

#[test]
fn resident_presence_resets_one_continuous_absence_window() {
    let (root, paths, state) = presence_fixture("window");
    let presence = ResidentPresence::new(&paths, &state);
    let lease = Duration::from_secs(10);
    let mut absence = AbsenceWindow::default();

    assert!(!absence.expired(Duration::ZERO, lease, &presence));
    assert!(!absence.expired(Duration::from_secs(9), lease, &presence));

    let resident = possess_resident_lock(&paths);
    assert!(!absence.expired(Duration::from_secs(9), lease, &presence));

    drop(resident);
    assert!(!absence.expired(Duration::from_secs(12), lease, &presence));
    assert!(!absence.expired(Duration::from_secs(21), lease, &presence));
    assert!(absence.expired(Duration::from_secs(22), lease, &presence));

    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn deleted_repository_or_state_is_absent_even_while_the_resident_lock_is_held() {
    let (root, paths, state) = presence_fixture("deleted");
    let resident = possess_resident_lock(&paths);
    let presence = ResidentPresence::new(&paths, &state);
    assert!(presence.present());

    let state_directory = state.parent().expect("state has a parent");
    fs::remove_dir_all(state_directory).expect("state directory deletes");
    assert!(!presence.present());
    fs::create_dir_all(state_directory).expect("state directory restores");
    assert!(presence.present());

    fs::remove_dir_all(&paths.common).expect("repository metadata deletes");
    assert!(!presence.present());
    fs::create_dir_all(&paths.common).expect("repository metadata restores");
    assert!(presence.present());

    fs::remove_dir_all(&root).expect("repository deletes");
    assert!(!presence.present());
    drop(resident);
}
