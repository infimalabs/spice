//! Resident startup, access, path, and lock regressions split from the root suite.

use super::*;

fn listen_for(argv: &[&str]) -> CommandResult<SocketAddr> {
    let matches = spice::cli::command()
        .try_get_matches_from(argv)
        .expect("declared up operands parse");
    requested_listen(spice::cli::selected_matches(&matches))
}

fn listen_refusal_for(argv: &[&str]) -> String {
    match listen_for(argv) {
        Ok(listen) => panic!("{argv:?} must not bind {listen}"),
        Err(Outcome::Refusal(refusal)) => refusal.reason().to_owned(),
        Err(other) => panic!("{argv:?} must refuse rather than fault: {other:?}"),
    }
}

fn access_for(argv: &[&str]) -> CommandResult<()> {
    let matches = spice::cli::command()
        .try_get_matches_from(argv)
        .expect("declared up operands parse");
    let leaf = spice::cli::selected_matches(&matches);
    let listen = requested_listen(leaf)?;
    requested_access(leaf, listen, None).map(|_| ())
}

fn access_refusal(argv: &[&str]) -> Refusal {
    match access_for(argv) {
        Ok(()) => panic!("{argv:?} must not admit access"),
        Err(Outcome::Refusal(refusal)) => refusal,
        Err(other) => panic!("{argv:?} must refuse rather than fault: {other:?}"),
    }
}

fn access_refusal_for(argv: &[&str]) -> String {
    access_refusal(argv).reason().to_owned()
}

pub(super) struct NoopCaptureReporter;

/// A lane runtime whose captures nobody reports.
fn silent_runtime<'a>(
    capabilities: &'a serve::axum::Capabilities,
    discovery: &'a serve::Discovery,
    reads: &'a read_materializations::CurrentReads,
) -> lane_surface::Runtime<'a> {
    lane_surface::Runtime::new(
        capabilities,
        discovery,
        Arc::new(NoopCaptureReporter),
        reads,
    )
}

impl lane::capture::CaptureReporter for NoopCaptureReporter {
    fn record(
        &self,
        _record: lane::capture::CaptureRecord,
    ) -> Result<(), lane::capture::CaptureReportError> {
        Ok(())
    }

    fn failed(&self, _failure: lane::capture::CaptureFailure) {}

    fn exited(&self, _exit: lane::capture::CaptureExit) {}
}

fn admitted_lane_seat(
    epoch: CanonicalUuid,
    body: &wire::intent::LaneSeat,
) -> wire::AdmittedCommand {
    Command {
        v: wire::WireVersion::CURRENT,
        id: test_uuid(70),
        epoch,
        kind: CommandKind::LaneSeat,
        expected: Vec::new(),
        body: serde_json::to_value(body).expect("lane seat command body encodes"),
    }
    .admit(
        &wire::Accepted {
            wire: wire::WireVersion::CURRENT,
            actor: Actor::TEST_PRINCIPAL,
            epoch,
            release: "1.2.3".parse().expect("fixture release"),
        },
        &wire::principal::PrincipalRoles::default(),
    )
    .expect("operator lane seat command admits")
}

/// The surface listens exactly where the operator requested, each half
/// defaulting on its own to loopback and a kernel-picked port.
///
/// An operator reaching this surface from another machine has to be able to say
/// which address it answers on, and naming only one half must not discard the
/// other half's default.
#[test]
fn the_listen_address_is_what_was_asked_for_or_an_ephemeral_loopback() {
    assert_eq!(listen_for(&["spice", "up"]), Ok(EPHEMERAL_LOOPBACK));
    assert_eq!(EPHEMERAL_LOOPBACK.port(), 0);
    assert!(EPHEMERAL_LOOPBACK.ip().is_loopback());

    assert_eq!(
        listen_for(&["spice", "up", "--host", "0.0.0.0"]),
        Ok(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            EPHEMERAL_LOOPBACK.port()
        )),
        "a named host keeps the default port"
    );
    assert_eq!(
        listen_for(&["spice", "up", "--port", "9000"]),
        Ok(SocketAddr::new(EPHEMERAL_LOOPBACK.ip(), 9000)),
        "a named port keeps the default host"
    );
    assert_eq!(
        listen_for(&["spice", "up", "--host", "10.0.0.5", "--port", "9000"]),
        Ok("10.0.0.5:9000".parse().expect("fixture address")),
        "both named is bound verbatim"
    );
}

/// An address this machine cannot listen on is named rather than approximated.
///
/// Resolving a host name here would bind whatever the resolver answered with,
/// which is not what the operator asked for and not something they could check
/// afterwards, so a name is refused instead of guessed at.
#[test]
fn an_unbindable_listen_request_is_refused_by_name() {
    assert_eq!(
        listen_refusal_for(&["spice", "up", "--host", "app.example"]),
        "resident::host-invalid"
    );
    assert_eq!(
        listen_refusal_for(&["spice", "up", "--port", "70000"]),
        "resident::port-invalid"
    );
    assert_eq!(
        listen_refusal_for(&["spice", "up", "--port", "http"]),
        "resident::port-invalid"
    );
}

#[test]
fn remote_listening_requires_explicit_consent_and_validates_an_optional_token() {
    assert_eq!(
        access_refusal_for(&["spice", "up", "--host", "0.0.0.0"]),
        "resident::insecure-bind-consent-required"
    );
    assert!(
        access_for(&["spice", "up", "--host", "0.0.0.0", "--allow-insecure-bind"]).is_ok(),
        "explicit insecure consent admits a network-protected remote bind"
    );
    let invalid = access_refusal(&[
        "spice",
        "up",
        "--host",
        "0.0.0.0",
        "--allow-insecure-bind",
        "--auth-token",
        "too-short-token",
    ]);
    assert_eq!(invalid.reason(), "resident::auth-token-invalid");
    assert_eq!(
        invalid
            .diagnostic()
            .and_then(wire::RefusalDiagnostic::observed),
        None,
        "a refused credential is never echoed"
    );
    assert!(
        access_for(&[
            "spice",
            "up",
            "--host",
            "0.0.0.0",
            "--allow-insecure-bind",
            "--auth-token",
            "correct-horse-battery-staple",
        ])
        .is_ok(),
        "explicit consent and a caller-held token admit the remote bind"
    );
    assert!(
        access_for(&["spice", "up"]).is_ok(),
        "the stable loopback default remains frictionless"
    );
}

/// `::1` is loopback here and at the listener, because one predicate says so.
///
/// This gate and the app-server one both ask [`std::net::IpAddr::is_loopback`]. A
/// narrowing on either side would split one decision in two: this side would
/// admit `--host ::1` with no token, and the listener would refuse the address
/// it had just admitted, telling the operator a machine-local bind is remote.
///
/// The wildcard cases sit beside the loopback one deliberately. An `is_ok` on
/// `::1` alone passes just as well against a gate that admits everything, and
/// `::` is the address that a rule reading only `Ipv4Addr` would wave through.
#[test]
fn ipv6_loopback_needs_no_consent_while_its_wildcard_needs_explicit_consent() {
    assert!(
        access_for(&["spice", "up", "--host", "::1"]).is_ok(),
        "IPv6 loopback is reachable from this machine alone"
    );

    assert_eq!(
        access_refusal_for(&["spice", "up", "--host", "::"]),
        "resident::insecure-bind-consent-required"
    );
    assert!(
        access_for(&["spice", "up", "--host", "::", "--allow-insecure-bind"]).is_ok(),
        "explicit insecure consent admits the IPv6 wildcard"
    );
}

#[test]
fn runtime_paths_follow_the_repository_common_directory() {
    let root = fixture("main");
    let nested = root.join("a/b");
    fs::create_dir_all(root.join(".git")).expect("git directory");
    fs::create_dir_all(&nested).expect("nested directory");

    let paths = RuntimePaths::discover(&nested).expect("paths resolve");
    let common = fs::canonicalize(root.join(".git")).expect("canonical common directory");

    assert_eq!(paths.runtime, common.join(".spice"));
    assert_eq!(
        paths.journal,
        common.join(".spice/authority/journal/journal.sqlite3")
    );
    fs::remove_dir_all(root).expect("fixture cleanup");
}

/// A holder replacement never creates an empty observation for a contender.
///
/// The callback is the exact instant after the writer has prepared its one
/// landing write and before that write occurs. The second acquisition therefore
/// exercises the resident's real contended-refusal path while the replacement
/// is in flight. Restoring a truncation before that callback makes this fixture
/// observe `holder metadata unavailable` instead of the seeded holder.
#[test]
fn contended_resident_names_the_holder_while_its_record_is_rewritten() {
    let root = fixture("holder-rewrite");
    fs::create_dir_all(root.join(".git")).expect("git directory");
    let paths = RuntimePaths::discover(&root).expect("runtime paths resolve");
    fs::create_dir_all(paths.resident.parent().expect("resident lock has a parent"))
        .expect("runtime directory");
    let lock = LockFile::new(
        LockName::new("resident").expect("resident lock name"),
        &paths.resident,
    );
    let Acquisition::Acquired(possession) = lock.try_acquire().expect("holder acquires") else {
        panic!("fresh resident lock unexpectedly contended");
    };
    let seeded = "pid=4242;epoch=seeded-holder";
    possession
        .rewrite_record(seeded.as_bytes())
        .expect("seeded holder lands");

    let mut observed = None;
    possession
        .rewrite_record_with(b"pid=4242;epoch=replacement", || {
            let Err(Outcome::Refusal(refusal)) = ResidentLock::acquire(&paths) else {
                panic!("the resident contender did not refuse");
            };
            assert_eq!(refusal.reason(), "resident::already-up");
            assert!(matches!(refusal, wire::Refusal::Bare { .. }));
            observed = refusal
                .diagnostic()
                .and_then(wire::RefusalDiagnostic::observed)
                .map(str::to_owned);
            Ok(())
        })
        .expect("replacement holder lands");

    assert_eq!(observed.as_deref(), Some(seeded));
    assert_eq!(
        fs::read_to_string(&paths.resident)
            .expect("replacement holder reads")
            .trim(),
        "pid=4242;epoch=replacement"
    );
    drop(possession);
    fs::remove_dir_all(root).expect("fixture cleanup");
}
/// Contention does not discard an explicit browser side effect. The current
/// resident's admitted discovery is opened, while the caller still receives
/// the truthful already-up refusal. A plain contender never launches.
#[test]
fn open_request_dials_the_current_resident_despite_contention() {
    const TOKEN: &str = "correct-horse-battery-staple";

    let (root, paths) = resident_fixture("open-contended-resident");
    let holder = ResidentLock::acquire(&paths).expect("resident holder acquires");
    let discovery = serve::Discovery {
        endpoint: "http://127.0.0.1:48239".to_owned(),
        capability: test_uuid(1),
        epoch: test_uuid(2),
    };
    holder
        .set_epoch(discovery.epoch)
        .expect("holder epoch writes");
    discovery
        .publish(&paths.serve)
        .expect("resident discovery publishes");

    let mut plain_launched = false;
    let Err(Outcome::Refusal(plain_refusal)) = acquire_for_up(
        &paths,
        Some(TOKEN),
        false,
        &mut io::sink(),
        &mut io::sink(),
        |_| {
            plain_launched = true;
            Ok(())
        },
    ) else {
        panic!("a plain contender must receive the already-up refusal");
    };
    assert_eq!(plain_refusal.reason(), "resident::already-up");
    assert!(!plain_launched);

    let mut launched = None;
    let Err(Outcome::Refusal(open_refusal)) = acquire_for_up(
        &paths,
        Some(TOKEN),
        true,
        &mut io::sink(),
        &mut io::sink(),
        |endpoint| {
            launched = Some(endpoint.to_owned());
            Ok(())
        },
    ) else {
        panic!("an opening contender must preserve the already-up refusal");
    };
    assert_eq!(open_refusal.reason(), "resident::already-up");
    assert_eq!(
        launched.as_deref(),
        Some("http://127.0.0.1:48239#correct-horse-battery-staple")
    );

    drop(holder);
    fs::remove_dir_all(root).expect("fixture cleanup");
}

/// A liveness observation cannot impersonate the resident it is checking.
///
/// The probe hook freezes the shared observation lock after it lands. Only
/// then does startup make its exclusive attempt; its contention hook releases
/// the probe, proving the two operations overlapped at the kernel lock rather
/// than merely running in nearby threads. Startup must retry that observation,
/// while the same availability path must still distinguish its resulting live
/// exclusive holder from the available lock after release.
#[cfg(unix)]
#[test]
fn resident_start_survives_an_overlapping_liveness_probe() {
    let root = fixture("startup-probe-overlap");
    fs::create_dir_all(root.join(".git")).expect("git directory");
    let paths = RuntimePaths::discover(&root).expect("runtime paths resolve");
    fs::create_dir_all(paths.resident.parent().expect("resident lock has a parent"))
        .expect("runtime directory");
    fs::write(&paths.resident, b"stale holder record\n").expect("existing resident lock file");
    let lock = LockFile::new(
        LockName::new("resident").expect("resident lock name"),
        &paths.resident,
    );
    let (probe_held, probe_landed) = std::sync::mpsc::sync_channel(0);
    let (release_probe, released) = std::sync::mpsc::sync_channel(0);

    std::thread::scope(|scope| {
        let observed_lock = lock.clone();
        let observer = scope.spawn(move || {
            observed_lock
                .availability_with(|| {
                    probe_held.send(()).expect("announce held probe");
                    released.recv().expect("startup releases probe");
                })
                .expect("probe answers")
        });
        probe_landed.recv().expect("probe lands before startup");

        let mut release_probe = Some(release_probe);
        let resident = ResidentLock::acquire_with(&paths, || {
            if let Some(release) = release_probe.take() {
                release.send(()).expect("release overlapping probe");
            }
        })
        .expect("probe-width contention cannot become already-up");

        assert_eq!(
            observer.join().expect("probe thread"),
            sill::Availability::Available
        );
        assert_eq!(
            lock.availability().expect("live resident probe"),
            sill::Availability::Held
        );
        drop(resident);
        assert_eq!(
            lock.availability().expect("released resident probe"),
            sill::Availability::Available
        );
    });

    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn stale_surface_cleanup_is_idempotent() {
    let root = fixture("stale-surfaces");
    fs::create_dir_all(root.join(".git")).expect("git directory");
    let paths = RuntimePaths::discover(&root).expect("paths resolve");
    fs::create_dir_all(&paths.lane_serve).expect("lane discovery directory");
    fs::write(&paths.serve, b"stale operator discovery").expect("operator discovery");
    fs::write(paths.lane_serve.join("lane"), b"stale lane discovery").expect("lane discovery");

    paths.clear_stale_surfaces().expect("stale surfaces clear");
    paths
        .clear_stale_surfaces()
        .expect("repeated stale surface cleanup is ordinary");

    assert!(!paths.serve.exists());
    assert!(!paths.lane_serve.exists());
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn linked_worktrees_share_the_git_common_directory() {
    let root = fixture("linked");
    let worktree = root.join("worktree");
    let common = root.join("common.git");
    let git_dir = common.join("worktrees/linked");
    fs::create_dir_all(&worktree).expect("worktree directory");
    fs::create_dir_all(&git_dir).expect("git directory");
    fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", git_dir.display()),
    )
    .expect("git marker");
    fs::write(git_dir.join("commondir"), "../..\n").expect("common marker");

    let paths = RuntimePaths::discover(&worktree).expect("paths resolve");
    let common = fs::canonicalize(&common).expect("canonical common directory");

    assert_eq!(paths.runtime, common.join(".spice"));
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn a_fresh_repository_startup_surface_stays_current_after_its_runtime_directory_appears() {
    let root = fixture("fresh-runtime");
    crate::test_repository::initialize(&root);
    let repository = Repository::discover(&root).expect("fixture repository resolves");
    let paths = RuntimePaths::discover(&root).expect("runtime paths resolve");
    assert!(!paths.runtime.exists(), "the repository has never started");

    let Ok(startup) = prepare_startup(&repository, &paths, &spice::ambient::capture()) else {
        panic!("a fresh repository prepares its startup surface");
    };
    assert!(paths.runtime.is_dir());
    assert!(
        require_current_surface(&repository, &startup.trust).is_ok(),
        "creating the runtime directory does not move the captured surface"
    );
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn lane_add_seats_a_vacant_lane_without_starting_an_occupant() {
    let (root, paths) = resident_fixture("lane-add-vacant");
    let repository = Repository::discover(&root).expect("fixture repository resolves");
    let surface =
        crate::trust_command::Surface::load(&repository).expect("fixture trust surface loads");
    let journal = journal::Journal::open(&paths.journal).expect("fixture journal opens");
    let bus = Bus::start(journal).expect("fixture bus starts");
    let epoch = test_uuid(71);
    commit_startup(&bus, &repository, &surface, epoch, None).expect("resident seats");
    let capabilities = serve::axum::Capabilities::default();
    let discovery = serve::Discovery {
        endpoint: "http://127.0.0.1:8767".to_owned(),
        capability: test_uuid(72),
        epoch,
    };
    let reads = read_materializations::CurrentReads::start_queueing(&bus, &paths);
    let runtime = silent_runtime(&capabilities, &discovery, &reads);
    let mut seats = Seats::new();
    let lane = test_uuid(73);
    let body = wire::intent::LaneSeat {
        topology_revision: 0,
        lane,
        team: lane,
        name: Some("new lane".to_owned()),
        worktree: repository.worktree_root().display().to_string(),
        life: "drive".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        model: Some("claude-sonnet-5".to_owned()),
        effort: Some("low".to_owned()),
        resume: None,
    };
    let command = admitted_lane_seat(epoch, &body);
    let unseated = bus.residency_head().unwrap().unwrap().position();
    for (model, effort, team, reason) in [
        (Some(" claude-sonnet-5"), None, lane, "lane::model-invalid"),
        (None, Some("ultra"), lane, "lane::effort-invalid"),
        (None, None, test_uuid(75), "team::target-missing"),
    ] {
        let refused = wire::intent::LaneSeat {
            model: model.map(str::to_owned),
            effort: effort.map(str::to_owned),
            team,
            ..body.clone()
        };
        let command = admitted_lane_seat(epoch, &refused);
        let answer = lane_surface::seat(
            &bus, &paths, epoch, &mut seats, &runtime, &command, &refused,
        );
        let Outcome::Refusal(refusal) = answer else {
            panic!("a selection outside the launch vocabulary must be refused: {answer:?}");
        };
        assert_eq!(refusal.reason(), reason);
        assert_eq!(bus.residency_head().unwrap().unwrap().position(), unseated);
    }

    let answer = lane_surface::seat(&bus, &paths, epoch, &mut seats, &runtime, &command, &body);

    let Outcome::Reply(reply) = answer else {
        panic!("lane add must commit: {answer:?}");
    };
    let CommandAnswer::LaneChange(change) = reply.body else {
        panic!("lane add returned a non-lane-change answer");
    };
    assert_eq!(
        (change.from.as_str(), change.to.as_str()),
        ("absent", "vacant")
    );
    assert!(seats.is_empty(), "lane add must not supervise a process");
    let before_join = bus.residency_head().unwrap().unwrap();
    let join = wire::intent::LaneSeat {
        topology_revision: change.topology_revision,
        lane: test_uuid(74),
        ..body.clone()
    };
    let command = admitted_lane_seat(epoch, &join);
    let joined = lane_surface::seat(&bus, &paths, epoch, &mut seats, &runtime, &command, &join);
    let Outcome::Refusal(refusal) = joined else {
        panic!("seating cannot grow a solo lane-ID team into a formation: {joined:?}");
    };
    assert_eq!(refusal.reason(), "team::seat-unfounded");
    let after_join = bus.residency_head().unwrap().unwrap();
    assert_eq!(after_join.position(), before_join.position());
    assert_eq!(after_join.body(), before_join.body());
    let snapshot = bus.task_snapshot().expect("facts read");
    assert!(
        !snapshot
            .facts()
            .iter()
            .any(|fact| fact.fact.kind() == wire::FactKind::LaneSessionDeclared),
        "lane add must not declare a vendor session"
    );
    assert!(
        !snapshot
            .facts()
            .iter()
            .any(|fact| fact.fact.kind() == wire::FactKind::LaneStarted),
        "lane add must not start an occupant"
    );
    let row = bus
        .residency_head()
        .expect("residency reads")
        .expect("residency exists")
        .into_body()
        .lanes
        .into_iter()
        .find(|row| row.lane == lane)
        .expect("lane add publishes one row");
    assert_eq!(row.occupant, None);
    assert_eq!(row.session, None);
    assert_eq!(row.lineage, Vec::<CanonicalUuid>::new());
    assert_eq!(row.disposition, "vacant");
    assert_eq!(row.capabilities.len(), 1);
    assert_eq!(row.model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(row.effort.as_deref(), Some("low"));
    bus.shutdown().expect("fixture bus stops");
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn a_linked_rack_is_admitted_once_and_resolved_by_its_own_archive() {
    let bus = Bus::start(journal::Journal::in_memory().expect("fixture journal opens"))
        .expect("fixture bus starts");
    let epoch = test_uuid(3);
    let first = wire::Digest::compute(wire::Domain::DistArtifact, b"rack one");
    let second = wire::Digest::compute(wire::Domain::DistArtifact, b"rack two");
    let generation = wire::Digest::compute(wire::Domain::DistArtifact, b"app");

    let admitted = runtime::admit_rack(&bus, epoch, &first, &generation).expect("rack admits");
    assert_eq!(
        runtime::admit_rack(&bus, epoch, &first, &generation).expect("rack re-admits"),
        admitted,
        "a second start appends no second fact for the same archive"
    );
    let newer = runtime::admit_rack(&bus, epoch, &second, &generation).expect("another admits");
    assert!(newer > admitted);

    let code = |archive: wire::Digest| {
        bus.read(move |read| spice::rack_admission::code(read, &archive))
            .expect("admissions read")
    };
    assert_eq!(
        code(first),
        Some(wire::SequenceCode::new(admitted.get())),
        "an older archive resolves its own fact, not the newest"
    );
    assert_eq!(code(second), Some(wire::SequenceCode::new(newer.get())));
    assert_eq!(
        code(wire::Digest::compute(
            wire::Domain::DistArtifact,
            b"unlinked"
        )),
        None
    );
    bus.shutdown().expect("fixture bus stops");
}
