use super::*;

pub(super) fn transcript_bytes() -> DemoResult<Vec<u8>> {
    let mut bytes = Vec::new();
    for (timestamp, text) in CANNED_TRANSCRIPT {
        bytes.extend(
            wire::encode(&DemoTurn {
                timestamp,
                role: "assistant",
                text,
            })
            .map_err(|error| protocol_fault("demo::transcript-invalid", error.to_string()))?,
        );
        bytes.push(b'\n');
    }
    Ok(bytes)
}

pub(super) fn expected_manifest() -> DemoManifest {
    DemoManifest {
        schema: 1,
        task: DEMO_TASK.to_owned(),
        lane: uuid(DEMO_LANE),
        transcript: TRANSCRIPT_NAME.to_owned(),
    }
}

pub(super) fn environment(
    root: PathBuf,
    transcript: PathBuf,
    state: spice::repository::StatePaths,
    executable: &Path,
    automatic: bool,
) -> DemoResult<Environment> {
    let root_text = require_utf8(&root, "demo root")?;
    let executable = require_utf8(executable, "current spice executable")?;
    let quoted_root = shell_quote(root_text);
    let quoted_executable = shell_quote(executable);
    Ok(Environment {
        launch_command: format!(
            "cd {quoted_root} && {quoted_executable} demo --reuse {quoted_root}"
        ),
        remove_command: format!("rm -rf -- {quoted_root}"),
        root,
        transcript,
        state,
        automatic,
    })
}

pub(super) fn app_environment(
    environment: Environment,
    cleanup: bool,
    ambient: &Ambient,
    open: bool,
) -> Outcome<DemoAnswer> {
    let (events, receiver) = mpsc::channel();
    let _watcher = match watch_environment(&environment, events.clone()) {
        Ok(watcher) => watcher,
        Err(failure) => return preserve_environment_failure(&environment, *failure),
    };
    let relay = match signal_relay(events.clone()) {
        Ok(relay) => relay,
        Err(failure) => return preserve_environment_failure(&environment, *failure),
    };
    let resident = match start_resident(&environment, events.clone(), ambient.clone()) {
        Ok(resident) => resident,
        Err(failure) => return preserve_environment_failure(&environment, *failure),
    };

    let ready = wait_for_discovery(&environment, &receiver);
    let interrupted_before_launch = match ready {
        Ok(false) => false,
        Ok(true) => true,
        Err(failure) => {
            drop(relay);
            drop(resident);
            return preserve_environment_failure(&environment, *failure);
        }
    };
    if interrupted_before_launch {
        let outcome = stop_resident(&environment, &receiver, ambient);
        drop(relay);
        return match outcome {
            Ok(()) => {
                let _ = resident.join();
                reply(&environment, "interrupted", "preserved", None)
            }
            Err(failure) => preserve_environment_failure(&environment, *failure),
        };
    }

    // The endpoint is admitted on every path; only the browser is optional.
    // Resolving it here rather than inside the launcher is what lets a plain
    // run still print an endpoint an operator or agent can open deliberately,
    // and it keeps demo on the one admission path `up` already uses.
    let opened = crate::command_client::open_app_with(
        environment.state.serve(),
        || {},
        |endpoint| {
            if open {
                crate::command_client::system_browser(endpoint)
            } else {
                Ok(())
            }
        },
    );
    let endpoint = match opened {
        Outcome::Reply(answer) => answer.body.endpoint,
        Outcome::Refusal(refusal) => {
            let _ = stop_resident(&environment, &receiver, ambient);
            drop(relay);
            let _ = resident.join();
            return preserve_environment_failure(&environment, Outcome::Refusal(refusal));
        }
        Outcome::Fault(fault) => {
            let _ = stop_resident(&environment, &receiver, ambient);
            drop(relay);
            let _ = resident.join();
            return preserve_environment_failure(&environment, Outcome::Fault(fault));
        }
    };
    let completion = wait_for_resident(&environment, &receiver, ambient);
    drop(relay);
    let _ = resident.join();
    finish_app(&environment, cleanup, endpoint, completion)
}

pub(super) fn watch_environment(
    environment: &Environment,
    events: Sender<Event>,
) -> DemoResult<notify::PollWatcher> {
    let journal = environment.state.journal().to_path_buf();
    let mut watcher = notify::PollWatcher::new(
        move |event: notify::Result<notify::Event>| {
            let observed = readiness_change(event, &journal);
            let _ = events.send(Event::Changed(observed));
        },
        notify::Config::default().with_poll_interval(WATCH_SCAN_INTERVAL),
    )
    .map_err(|error| {
        environment_fault(
            "demo::readiness-watcher-unavailable",
            format!("the resident readiness watcher could not be created: {error}"),
        )
    })?;
    watcher
        .watch(&environment.root, RecursiveMode::Recursive)
        .map_err(|error| {
            environment_fault(
                "demo::readiness-watch-refused",
                format!(
                    "{} could not be watched for resident discovery: {error}",
                    environment.root.display()
                ),
            )
        })?;
    Ok(watcher)
}

pub(super) fn readiness_change(
    event: notify::Result<notify::Event>,
    journal: &Path,
) -> Result<(), String> {
    match event {
        Ok(_) => Ok(()),
        Err(error) if is_missing_readiness_transient(&error, journal) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

pub(super) fn is_missing_readiness_transient(error: &notify::Error, journal: &Path) -> bool {
    let notify::ErrorKind::Io(source) = &error.kind else {
        return false;
    };
    // PollWatcher enumerates each path before reading its metadata. SQLite and
    // atomic publication may remove a known transient between those steps, but
    // no other scan error should disappear into a generic change notification.
    source.kind() == io::ErrorKind::NotFound
        && !error.paths.is_empty()
        && error.paths.iter().all(|path| {
            (is_known_journal_sidecar(path, journal) || atomic::is_temporary_path(path))
                && matches!(
                    fs::symlink_metadata(path),
                    Err(source) if source.kind() == io::ErrorKind::NotFound
                )
        })
}

pub(super) fn is_known_journal_sidecar(path: &Path, journal: &Path) -> bool {
    ["-journal", "-wal", "-shm"].into_iter().any(|suffix| {
        let mut sidecar = journal.as_os_str().to_owned();
        sidecar.push(suffix);
        path.as_os_str() == sidecar
    })
}

fn signal_relay(events: Sender<Event>) -> DemoResult<ControlRelay> {
    ControlRelay::install(move || {
        let _ = events.send(Event::Signal);
    })
    .map_err(|error| environment_fault("demo::signals-unavailable", error.to_string()).into())
}

pub(super) fn start_resident(
    environment: &Environment,
    events: Sender<Event>,
    ambient: Ambient,
) -> DemoResult<JoinHandle<()>> {
    let root = environment.root.clone();
    std::thread::Builder::new()
        .name("spice-demo-resident".to_owned())
        .spawn(move || {
            let outcome = crate::resident::up_from(
                &root,
                crate::resident::AppBinding::local(crate::resident::EPHEMERAL_LOOPBACK),
                &ambient,
            );
            let _ = events.send(Event::Resident(outcome));
        })
        .map_err(|error| {
            environment_fault("demo::resident-launch-failed", error.to_string()).into()
        })
}

pub(super) fn finish_app(
    environment: &Environment,
    cleanup: bool,
    endpoint: String,
    completion: DemoResult<bool>,
) -> Outcome<DemoAnswer> {
    match completion {
        Ok(true) => reply(environment, "app", "interrupted-preserved", Some(endpoint)),
        Ok(false) if cleanup => match fs::remove_dir_all(&environment.root) {
            Ok(()) => reply(environment, "app", "removed", Some(endpoint)),
            Err(error) => preserve_environment_failure(
                environment,
                environment_fault(
                    "demo::cleanup-failed",
                    format!("{}: {error}", environment.root.display()),
                ),
            ),
        },
        Ok(false) => reply(environment, "app", "preserved", Some(endpoint)),
        Err(failure) => preserve_environment_failure(environment, *failure),
    }
}

pub(super) fn wait_for_discovery(
    environment: &Environment,
    receiver: &Receiver<Event>,
) -> DemoResult<bool> {
    let deadline = Monotonic::now() + READY_DEADLINE;
    loop {
        if environment.state.serve().is_file() {
            return Ok(false);
        }
        let remaining = deadline.saturating_duration_since(Monotonic::now());
        match receiver.recv_timeout(remaining) {
            Ok(Event::Changed(Ok(()))) => {}
            Ok(Event::Changed(Err(error))) => {
                return Err(environment_fault(
                    "demo::readiness-scan-failed",
                    format!(
                        "{} could not be scanned for resident discovery: {error}",
                        environment.root.display()
                    ),
                )
                .into());
            }
            Ok(Event::Resident(outcome)) => return Err(rebody(outcome).into()),
            Ok(Event::Signal) => return Ok(true),
            Err(RecvTimeoutError::Timeout) => {
                return Err(environment_fault(
                    "demo::resident-readiness-timeout",
                    format!("resident discovery was not published within {READY_DEADLINE:?}"),
                )
                .into());
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(environment_fault(
                    "demo::resident-events-closed",
                    "resident readiness channel disconnected",
                )
                .into());
            }
        }
    }
}

pub(super) fn wait_for_resident(
    environment: &Environment,
    receiver: &Receiver<Event>,
    ambient: &Ambient,
) -> DemoResult<bool> {
    loop {
        match receiver.recv() {
            Ok(Event::Changed(_)) => {}
            Ok(Event::Resident(Outcome::Reply(_))) => return Ok(false),
            Ok(Event::Resident(outcome)) => return Err(rebody(outcome).into()),
            Ok(Event::Signal) => {
                stop_resident(environment, receiver, ambient)?;
                return Ok(true);
            }
            Err(_) => {
                return Err(environment_fault(
                    "demo::resident-events-closed",
                    "resident lifecycle channel disconnected",
                )
                .into());
            }
        }
    }
}

pub(super) fn stop_resident(
    environment: &Environment,
    receiver: &Receiver<Event>,
    ambient: &Ambient,
) -> DemoResult<()> {
    let contract = crate::cli::contract(&["down"]).expect("down contract is registered");
    // The demo root seats no lanes, so the stop must ride the operator
    // connection: an ambient lane identity names a seat in the repository
    // that launched the agent, and honoring it here would dial a discovery
    // path the demo root cannot publish and strand the demo resident.
    match crate::resident::down_from_operator(&environment.root, contract, ambient) {
        Outcome::Reply(_) => {}
        outcome => return Err(rebody(outcome).into()),
    }
    let deadline = Monotonic::now() + STOP_DEADLINE;
    loop {
        let remaining = deadline.saturating_duration_since(Monotonic::now());
        match receiver.recv_timeout(remaining) {
            Ok(Event::Changed(_)) | Ok(Event::Signal) => {}
            Ok(Event::Resident(Outcome::Reply(_))) => return Ok(()),
            Ok(Event::Resident(outcome)) => return Err(rebody(outcome).into()),
            Err(RecvTimeoutError::Timeout) => {
                return Err(environment_fault(
                    "demo::resident-stop-timeout",
                    format!("resident did not stop within {STOP_DEADLINE:?}"),
                )
                .into());
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(environment_fault(
                    "demo::resident-events-closed",
                    "resident lifecycle channel disconnected",
                )
                .into());
            }
        }
    }
}

pub(super) fn reply(
    environment: &Environment,
    mode: &str,
    lifecycle: &str,
    endpoint: Option<String>,
) -> Outcome<DemoAnswer> {
    Outcome::Reply(Reply {
        position: ReplyPosition::Direct {},
        body: DemoAnswer {
            root: environment.root.display().to_string(),
            transcript: environment.transcript.display().to_string(),
            mode: mode.to_owned(),
            lifecycle: lifecycle.to_owned(),
            endpoint,
            launch_command: environment.launch_command.clone(),
            remove_command: environment.remove_command.clone(),
        },
    })
}

pub(super) fn uuid(value: &str) -> CanonicalUuid {
    value.parse().expect("demo UUID constants are canonical")
}

pub(super) fn instant(value: &str) -> wire::Instant {
    wire::Instant::new(value.parse().expect("demo timestamp constants parse"))
        .expect("demo timestamps fit the native instant range")
}

pub(super) fn build_failure(error: journal::BuildError) -> Outcome<DemoAnswer> {
    protocol_fault("demo::journal-proposal-invalid", error.to_string())
}

pub(super) fn commit_failure(error: journal::CommitError) -> Outcome<DemoAnswer> {
    environment_fault("demo::journal-write-failed", error.to_string())
}

pub(super) fn rebody<B>(outcome: Outcome<B>) -> Outcome<DemoAnswer> {
    match outcome {
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
        Outcome::Reply(_) => protocol_fault(
            "demo::resident-ended-before-ready",
            "the demo resident ended before its lifecycle wait completed",
        ),
    }
}

pub(super) fn refusal(
    reason: &str,
    expected: Option<&str>,
    observed: Option<&str>,
    detail: &str,
) -> Outcome<DemoAnswer> {
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason.to_owned(),
            Some("demo-root".to_owned()),
            expected.map(str::to_owned),
            observed.map(str::to_owned),
        )
        .with_detail(detail),
    ))
}
