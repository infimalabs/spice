use std::io::{self, Read as _, Write as _};
use std::net::{Shutdown, TcpListener};
use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::SystemTime;

use gate::test_support::gix;

use super::*;
mod multi_worktree;
mod restart_binding;

#[test]
fn executable_rollback_requires_the_journal_schema_to_be_proven_unchanged() {
    const CURRENT: u32 = spice::journal_schema::CURRENT;
    assert!(schema_rollback_safe(None, None));
    assert!(schema_rollback_safe(Some(CURRENT), Some(CURRENT)));
    assert!(!schema_rollback_safe(None, Some(CURRENT)));
    assert!(!schema_rollback_safe(Some(CURRENT - 1), Some(CURRENT)));
    assert!(!schema_rollback_safe(Some(CURRENT), None));
}

#[test]
fn no_resident_refresh_does_not_inspect_journal_schema() {
    let fixture = Fixture::new("no-resident-schema");
    let repository = fixture.repository();
    let journal = repository
        .admit_state()
        .expect("repository state")
        .journal()
        .to_path_buf();
    fs::create_dir_all(journal.parent().expect("journal parent")).expect("journal parent exists");
    fs::write(&journal, b"not a SQLite journal").expect("invalid journal writes");

    assert_eq!(
        schema_recovery_boundary(&repository, false).expect("no-resident boundary skips"),
        None,
    );
    assert!(schema_recovery_boundary(&repository, true).is_err());
}

#[test]
fn a_committed_schema_advance_preserves_the_successor_executable() {
    let fixture = Fixture::new("schema-advance");
    let repository = fixture.repository();
    let installed = install::installed_binary(&repository);
    place(&repository, b"predecessor\n");
    let parked = park(&repository, &installed)
        .expect("predecessor parks")
        .expect("predecessor exists");
    place(&repository, b"successor\n");
    let rollback = Rollback {
        installed: &installed,
        executable: Some(&parked),
        resident_executable: None,
        published: None,
        restart: false,
        binding: RestartBinding::default(),
        resident: Default::default(),
    };
    let mut host = Scripted::new(unexpected);
    let mut output = Vec::new();

    const SUCCESSOR_SCHEMA: u32 = spice::journal_schema::CURRENT;
    let error = recover_start_failure_after(
        &mut host,
        &repository,
        &rollback,
        Some(SUCCESSOR_SCHEMA - 1),
        Ok(Some(SUCCESSOR_SCHEMA)),
        &mut output,
        RefreshError::ResidentUnready,
    );

    assert!(matches!(error, RefreshError::SchemaAdvanced { .. }));
    assert!(
        host.installs.is_empty(),
        "an older schema writer was reinstalled"
    );
    assert_eq!(
        fs::read(installed).expect("installed reads"),
        b"successor\n"
    );
    assert!(String::from_utf8_lossy(&output).contains("preserving new executable"));
}

#[test]
fn uncertain_schema_recovery_preserves_the_successor_and_names_both_causes() {
    let fixture = Fixture::new("schema-uncertain");
    let repository = fixture.repository();
    let installed = install::installed_binary(&repository);
    place(&repository, b"predecessor\n");
    let parked = park(&repository, &installed)
        .expect("predecessor parks")
        .expect("predecessor exists");
    place(&repository, b"successor\n");
    let invalid = fixture.0.join("uncertain-journal.sqlite3");
    fs::write(&invalid, b"not a SQLite authority").expect("uncertain journal writes");
    let inspection = spice::journal_schema::inspect_stored(&invalid)
        .expect_err("invalid journal makes recovery uncertain");
    let mut host = Scripted::new(unexpected);
    let mut output = Vec::new();

    let error = recover_start_failure_after(
        &mut host,
        &repository,
        &Rollback {
            installed: &installed,
            executable: Some(&parked),
            resident_executable: None,
            published: None,
            restart: false,
            binding: RestartBinding::default(),
            resident: Default::default(),
        },
        Some(spice::journal_schema::CURRENT - 1),
        Err(inspection),
        &mut output,
        RefreshError::ResidentUnready,
    );

    assert!(matches!(
        error,
        RefreshError::SchemaRecoveryUncertain {
            before: Some(before),
            ref cause,
            ..
        } if before == spice::journal_schema::CURRENT - 1
            && cause.contains("resident never became ready")
    ));
    assert!(
        host.installs.is_empty(),
        "uncertain recovery never republishes an older schema writer",
    );
    assert_eq!(
        fs::read(installed).expect("installed successor reads"),
        b"successor\n",
    );
    assert!(
        output.is_empty(),
        "uncertain recovery performs no rollback action"
    );
}

/// A resident start refused over an unchanged schema rolls back through the
/// parked executable's own installer, plan then apply by digest, and restarts
/// the resident it stopped; the in-process installer never runs.
#[test]
fn a_refused_resident_start_restores_through_the_parked_executables_installer() {
    let fixture = Fixture::new("start-refused-restore");
    let repository = fixture.repository();
    let installed = install::installed_binary(&repository);
    let endpoint = "http://127.0.0.1:8767";
    place(&repository, b"predecessor\n");
    let parked = park(&repository, &installed)
        .expect("predecessor parks")
        .expect("predecessor exists");
    place(&repository, b"successor\n");
    let mut host = Scripted::new(parked_installer(
        parked.clone(),
        &repository,
        |_: &Issued| ready(endpoint),
    ));
    let mut output = Vec::new();

    let error = recover_start_failure_after(
        &mut host,
        &repository,
        &Rollback {
            installed: &installed,
            executable: Some(&parked),
            resident_executable: None,
            published: Some(endpoint),
            restart: true,
            binding: restart_binding(Some("127.0.0.1"), Some(8767)),
            resident: Default::default(),
        },
        Some(spice::journal_schema::CURRENT),
        Ok(Some(spice::journal_schema::CURRENT)),
        &mut output,
        RefreshError::ResidentStartRefused("journal::schema-refused".to_owned()),
    );

    assert!(
        matches!(&error, RefreshError::ResidentStartRefused(refusal) if refusal == "journal::schema-refused"),
        "the start refusal was not reported after the rollback: {error}"
    );
    assert_eq!(
        host.issued,
        [
            Issued::install(&parked, None),
            Issued::install(&parked, Some(&parked_plan_digest())),
            Issued::up(&installed, &["--host", "127.0.0.1", "--port", "8767"]),
        ],
        "the rollback did not run the parked executable's installer: {}",
        String::from_utf8_lossy(&output)
    );
    assert!(
        host.installs.is_empty(),
        "the in-process installer ran for a rollback"
    );
    assert_eq!(
        fs::read(&installed).expect("installed reads"),
        b"predecessor\n"
    );
}

/// How many names one fixture tries before giving up on an exclusive root.
const SCRATCH_ATTEMPTS: usize = 100;

/// A scratch repository whose appliance state a refresh may rewrite.
struct Fixture(PathBuf);

impl Fixture {
    /// One scratch repository under a root nothing else already holds.
    ///
    /// `create_dir` refuses a directory that exists rather than opening it, so
    /// retrying under a fresh name is what makes the root exclusive.
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..SCRATCH_ATTEMPTS {
            let stamp = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("the clock reads after the epoch")
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "spice-xtask-develop-{label}-{stamp}-{}",
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&root) {
                Ok(()) => return Self::initialized(root),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("exclusive scratch root is created: {error}"),
            }
        }
        panic!("no exclusive scratch root was available in {SCRATCH_ATTEMPTS} attempts");
    }

    /// Canonicalize the freshly created `root` and make it a repository.
    fn initialized(root: PathBuf) -> Self {
        let fixture = Self(fs::canonicalize(&root).expect("scratch root canonicalizes"));
        let repository = gix::ThreadSafeRepository::init_opts(
            &fixture.0,
            gix::create::Kind::WithWorktree,
            gix::create::Options::default(),
            gix::open::Options::isolated(),
        )
        .expect("initialize in-process scratch repository")
        .to_thread_local();
        fs::write(repository.git_dir().join("HEAD"), "ref: refs/heads/main\n")
            .expect("select deterministic scratch branch");
        fixture
    }

    fn repository(&self) -> Repository {
        Repository::discover(&self.0).expect("scratch repository resolves")
    }

    /// Write one distinct executable image and answer where it landed.
    fn binary(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).expect("scratch executable is written");
        path
    }

    fn publish(&self, endpoint: &str) -> spice::serve::Discovery {
        let identity = "00000000-0000-4000-8000-000000000000"
            .parse::<wire::CanonicalUuid>()
            .expect("fixture identity is canonical");
        let discovery = spice::serve::Discovery {
            endpoint: endpoint.to_owned(),
            capability: identity,
            epoch: identity,
        };
        let state = self
            .repository()
            .admit_state()
            .expect("scratch repository state is admitted");
        if let Some(parent) = state.serve().parent() {
            fs::create_dir_all(parent).expect("scratch discovery parent is created");
        }
        discovery
            .publish(state.serve())
            .expect("scratch discovery publishes");
        fs::write(
            state.resident(),
            spice::resident_lock::holder_record(std::process::id(), Some(identity)),
        )
        .expect("scratch holder record publishes");
        discovery
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Removal is allowed to fail: reporting that from a drop would replace
        // the failure the test is already carrying.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Put `bytes` in place as `repository`'s installed executable.
fn place(repository: &Repository, bytes: &[u8]) -> Digest {
    let installed = install::installed_binary(repository);
    fs::create_dir_all(
        installed
            .parent()
            .expect("installed executable has a parent"),
    )
    .expect("installed executable parent is created");
    fs::write(&installed, bytes).expect("installed executable is written");
    executable_digest(&installed).expect("installed executable hashes")
}

/// One appliance command as the refresh issued it.
#[derive(Debug, Eq, PartialEq)]
struct Issued {
    program: PathBuf,
    arguments: Vec<String>,
}

impl Issued {
    fn down(program: &Path) -> Self {
        Self {
            program: program.to_path_buf(),
            arguments: vec!["down".to_owned()],
        }
    }

    fn up(program: &Path, arguments: &[&str]) -> Self {
        Self {
            program: program.to_path_buf(),
            arguments: ["--json", "up"]
                .iter()
                .chain(arguments)
                .map(|argument| (*argument).to_owned())
                .collect(),
        }
    }

    fn install(program: &Path, apply: Option<&Digest>) -> Self {
        let mut arguments = vec!["--json".to_owned(), "install".to_owned()];
        if let Some(digest) = apply {
            arguments.push(format!("--apply={digest}"));
        }
        Self {
            program: program.to_path_buf(),
            arguments,
        }
    }

    fn is_down(&self) -> bool {
        self.arguments == ["down"]
    }

    /// The plan digest this is an `install --apply` of, if it is one.
    fn applies(&self) -> Option<Digest> {
        match self.arguments.as_slice() {
            [json, install, apply] if json == "--json" && install == "install" => apply
                .strip_prefix("--apply=")
                .and_then(|digest| digest.parse().ok()),
            _ => None,
        }
    }

    fn is_install_plan(&self) -> bool {
        self.arguments == ["--json", "install"]
    }
}

/// In-process appliance host: an install copies the executable image into
/// place, and `answer` replies to each appliance command.
struct Scripted<F> {
    answer: F,
    installs: Vec<PathBuf>,
    issued: Vec<Issued>,
}

impl<F: FnMut(&Issued) -> Output> Scripted<F> {
    fn new(answer: F) -> Self {
        Self {
            answer,
            installs: Vec::new(),
            issued: Vec::new(),
        }
    }
}

impl<F: FnMut(&Issued) -> Output> Host for Scripted<F> {
    fn install(&mut self, repository: &Repository, binary: &Path) -> Result<Digest, InstallError> {
        self.installs.push(binary.to_path_buf());
        Ok(place(
            repository,
            &fs::read(binary).expect("installed image reads"),
        ))
    }

    fn run(&mut self, command: &mut Command) -> io::Result<Output> {
        let issued = Issued {
            program: PathBuf::from(command.get_program()),
            arguments: command
                .get_args()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect(),
        };
        let answer = (self.answer)(&issued);
        self.issued.push(issued);
        Ok(answer)
    }
}

fn unexpected(issued: &Issued) -> Output {
    panic!("no appliance command was expected: {issued:?}")
}

fn exited(code: i32, stdout: Vec<u8>, stderr: &str) -> Output {
    Output {
        status: ExitStatus::from_raw(code << 8),
        stdout,
        stderr: stderr.as_bytes().to_vec(),
    }
}

fn stopped() -> Output {
    exited(0, Vec::new(), "")
}

fn already_up() -> Output {
    exited(2, Vec::new(), "spice: resident::already-up; pid=42\n")
}

/// The launcher's typed readiness reply for `endpoint`.
fn ready(endpoint: &str) -> Output {
    let readiness = wire::Outcome::Reply(wire::Reply {
        position: wire::ReplyPosition::Direct {},
        body: wire::AppOpen {
            endpoint: endpoint.to_owned(),
            epoch: "00000000-0000-4000-8000-000000000004"
                .parse()
                .expect("fixture epoch is canonical"),
        },
    });
    let mut stdout = wire::encode(&readiness).expect("readiness encodes");
    stdout.push(b'\n');
    exited(0, stdout, "")
}

/// The plan digest a parked executable's own `install` preview names.
fn parked_plan_digest() -> Digest {
    Digest::compute(wire::Domain::InstallPlan, b"parked executable plan")
}

/// One typed reply as a successful `--json` answer on stdout.
fn replied<T: serde::Serialize>(body: T) -> Output {
    let reply = wire::Outcome::Reply(wire::Reply {
        position: wire::ReplyPosition::Direct {},
        body,
    });
    exited(0, wire::encode(&reply).expect("a reply encodes"), "")
}

/// A parked executable's `--json install` preview naming `digest`.
fn install_preview(digest: &Digest) -> Output {
    let preview: install::InstallPreview = wire::decode(
        serde_json::json!({
            "digest": digest,
            "plan": {"version": 1, "operation_count": 0, "operations": [], "retirements": []},
        })
        .to_string()
        .as_bytes(),
    )
    .expect("a minimal preview decodes");
    replied(preview)
}

/// A parked executable's `--json install --apply` receipt for `digest`.
fn install_receipt(digest: &Digest) -> Output {
    let receipt: install::InstallReceipt = wire::decode(
        serde_json::json!({
            "plan": {"version": 1, "operation_count": 0, "operations": [], "retirements": []},
            "digest": digest,
            "status": "complete",
            "completed": 0,
        })
        .to_string()
        .as_bytes(),
    )
    .expect("a minimal receipt decodes");
    replied(receipt)
}

/// A parked executable's `--json install` refusal: the outcome on stderr with
/// a failing status, as the appliance answers.
fn install_refused(reason: &str, detail: &str) -> Output {
    let refusal: wire::Outcome<install::InstallPreview> =
        wire::Outcome::Refusal(wire::Refusal::bare(
            wire::RefusalDiagnostic::new(reason.to_owned(), None, None, None).with_detail(detail),
        ));
    let encoded = wire::encode(&refusal).expect("a refusal encodes");
    exited(2, Vec::new(), &String::from_utf8(encoded).expect("utf-8"))
}

/// Answer the parked executable's own install stages, publishing its bytes
/// into place on apply exactly as that executable's installer would, and hand
/// every other command to `rest`.
fn parked_installer(
    parked: PathBuf,
    repository: &Repository,
    mut rest: impl FnMut(&Issued) -> Output,
) -> impl FnMut(&Issued) -> Output {
    let installed = install::installed_binary(repository);
    move |issued: &Issued| {
        if issued.program != parked {
            return rest(issued);
        }
        if issued.is_install_plan() {
            return install_preview(&parked_plan_digest());
        }
        if let Some(digest) = issued.applies() {
            assert_eq!(digest, parked_plan_digest(), "apply named another plan");
            fs::copy(&parked, &installed).expect("the parked executable publishes itself");
            return install_receipt(&digest);
        }
        rest(issued)
    }
}

/// A port nothing is bound to, taken by binding one and letting it go.
fn unbound_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a scratch port binds");
    let port = listener
        .local_addr()
        .expect("the scratch port is named")
        .port();
    drop(listener);
    port
}

/// Serve one exact resident bootstrap from `listener`.
fn serve_bootstrap(
    listener: TcpListener,
    discovery: spice::serve::Discovery,
    binary: Digest,
) -> thread::JoinHandle<()> {
    serve_bootstraps(listener, discovery, vec![binary])
}

/// Serve successive resident generations from one stable endpoint.
fn serve_bootstraps(
    listener: TcpListener,
    discovery: spice::serve::Discovery,
    binaries: Vec<Digest>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for binary in binaries {
            let bootstrap = wire::AppBootstrap {
                endpoint: discovery.endpoint.clone(),
                capability: discovery.capability,
                epoch: discovery.epoch,
                version: wire::Version {
                    product: wire::manifest::ProductVersion::parse("0.2.0-dev.0")
                        .expect("fixture product version"),
                    binary,
                    wire: wire::WireVersion::CURRENT,
                    journal: spice::journal_schema::CURRENT,
                    app_protocol: wire::AppProtocolVersion::CURRENT,
                    app: Digest::compute(wire::Domain::DistInventory, b"fixture app"),
                },
            };
            let body = wire::encode(&bootstrap).expect("fixture bootstrap encodes");
            let (mut stream, _) = listener.accept().expect("bootstrap connection arrives");
            let mut request = Vec::new();
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut chunk = [0_u8; 256];
                let read = stream.read(&mut chunk).expect("bootstrap request reads");
                assert_ne!(read, 0, "bootstrap request terminates its headers");
                request.extend_from_slice(&chunk[..read]);
                assert!(request.len() <= 4 * 1024, "bootstrap request stays bounded");
            }
            assert!(
                request.starts_with(b"GET /bootstrap HTTP/1.1\r\n"),
                "unexpected bootstrap request: {}",
                String::from_utf8_lossy(&request)
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .and_then(|()| stream.write_all(&body))
            .and_then(|()| stream.flush())
            .and_then(|()| stream.shutdown(Shutdown::Write))
            .expect("bootstrap response writes");
        }
    })
}

/// A stable-rollback promotion fault occurs after `down`, so it must restore
/// service through the separately retained resident authority before returning.
#[test]
fn rollback_promotion_failure_restores_the_predecessor_resident() {
    let fixture = Fixture::new("rollback-promotion-failure");
    let repository = fixture.repository();
    let discovery_path = repository
        .admit_state()
        .expect("fixture state is admitted")
        .serve()
        .to_path_buf();
    let common = workspace(&repository).expect("common refresh workspace exists");
    let stable_rollback = common.join(ROLLBACK);
    let retained_resident = common.join(RESIDENT_ROLLBACK);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bootstrap listener binds");
    let address = listener.local_addr().expect("bootstrap address reads");
    let endpoint = format!("http://{address}");
    let port = address.port().to_string();
    let discovery = fixture.publish(&endpoint);
    let predecessor = place(&repository, b"predecessor executable\n");
    let successor = fixture.binary("spice-successor", b"successor executable\n");
    let server = serve_bootstraps(
        listener,
        discovery,
        vec![predecessor.clone(), predecessor.clone()],
    );
    let mut host = Scripted::new({
        let endpoint = endpoint.clone();
        let stable_rollback = stable_rollback.clone();
        move |issued: &Issued| {
            if !issued.is_down() {
                return ready(&endpoint);
            }
            fs::remove_file(&discovery_path).expect("the stopped resident withdraws");
            fs::remove_file(&stable_rollback).expect("the stable rollback exists");
            fs::create_dir(&stable_rollback).expect("the rollback slot is obstructed");
            stopped()
        }
    });
    let no_resident = RefreshRequest {
        restart: false,
        ..RefreshRequest::default()
    };
    let mut output = Vec::new();
    install_built(
        &mut host,
        &repository,
        successor.clone(),
        &no_resident,
        &mut output,
    )
    .expect("no-resident publication installs successor");
    let request = RefreshRequest {
        restart: true,
        ..RefreshRequest::default()
    };

    let result = install_built(
        &mut host,
        &repository,
        successor.clone(),
        &request,
        &mut output,
    );
    server.join().expect("bootstrap server joins");

    assert!(
        matches!(result, Err(RefreshError::Io(_))),
        "rollback promotion fault was not reported: {result:?}"
    );
    assert_eq!(
        host.issued,
        [
            Issued::down(&retained_resident),
            Issued::up(
                &retained_resident,
                &["--host", "127.0.0.1", "--port", &port]
            ),
        ],
        "the predecessor resident was not restored through its own authority: {}",
        String::from_utf8_lossy(&output)
    );
    assert_eq!(
        host.installs,
        [successor],
        "promotion failure changed the previously runnable executable"
    );
    assert_eq!(
        executable_digest(&retained_resident).expect("retained authority hashes"),
        predecessor,
        "promotion failure lost the complete predecessor authority"
    );
}

/// An authenticated generation outside both transaction slots is not ours.
#[test]
fn a_foreign_resident_is_refused_before_stop_or_install() {
    let fixture = Fixture::new("foreign-resident");
    let repository = fixture.repository();
    place(&repository, b"installed executable\n");
    let successor = fixture.binary("spice-foreign-successor", b"successor executable\n");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bootstrap listener binds");
    let address = listener.local_addr().expect("bootstrap address reads");
    let discovery = fixture.publish(&format!("http://{address}"));
    let foreign = Digest::compute(wire::Domain::DistArtifact, b"foreign executable");
    let server = serve_bootstrap(listener, discovery, foreign);
    let request = RefreshRequest {
        restart: true,
        ..RefreshRequest::default()
    };
    let mut host = Scripted::new(unexpected);
    let mut output = Vec::new();

    let result = install_built(&mut host, &repository, successor, &request, &mut output);
    server.join().expect("bootstrap server joins");

    assert!(
        matches!(&result, Err(RefreshError::ResidentStopRefused(refusal))
            if refusal.contains("resident::foreign-generation")),
        "foreign resident refusal was not named: {result:?}"
    );
    assert!(
        host.installs.is_empty(),
        "foreign resident refusal replaced the installed executable"
    );
}

#[test]
fn restart_admits_the_launchers_typed_readiness() {
    let fixture = Fixture::new("restart-contract");
    let repository = fixture.repository();
    let endpoint = "http://127.0.0.1:43821";
    let request = RefreshRequest {
        restart: true,
        ..RefreshRequest::default()
    };
    let mut host = Scripted::new(|_: &Issued| ready(endpoint));
    let mut output = Vec::new();

    let restarted = restart(
        &mut host,
        &repository,
        Path::new("/installed/spice"),
        &restart_binding(Some("127.0.0.1"), Some(43821)),
        &request,
        &mut output,
    );

    assert_eq!(
        restarted.expect("typed launcher readiness is sufficient"),
        Some(endpoint.to_owned())
    );
    assert_eq!(
        host.issued,
        [Issued::up(
            Path::new("/installed/spice"),
            &["--host", "127.0.0.1", "--port", "43821"]
        )]
    );
    assert!(
        String::from_utf8_lossy(&output).contains("resident ready at http://127.0.0.1:43821"),
        "refresh did not surface the admitted endpoint"
    );
}

#[test]
fn restart_surfaces_the_daemon_launchers_exact_refusal() {
    let fixture = Fixture::new("restart-refusal");
    let repository = fixture.repository();
    let request = RefreshRequest {
        restart: true,
        ..RefreshRequest::default()
    };
    let mut host = Scripted::new(|_: &Issued| already_up());
    let mut output = Vec::new();

    let restarted = restart(
        &mut host,
        &repository,
        Path::new("/installed/spice"),
        &RestartBinding::default(),
        &request,
        &mut output,
    );

    assert!(
        matches!(restarted, Err(RefreshError::ResidentStartRefused(ref refusal))
            if refusal.contains("resident::already-up; pid=42")),
        "refresh replaced the launcher's exact refusal: {restarted:?}"
    );
}

/// A concurrent launcher can finish the install's job before its own `up`.
///
/// Treating the resulting `already-up` as a start failure enters rollback even
/// though the installed executable is serving. The bootstrap binary identity
/// makes that state exact rather than accepting an arbitrary lock holder.
#[test]
fn a_concurrent_installed_resident_completes_the_restart() {
    let fixture = Fixture::new("restart-current-race");
    let repository = fixture.repository();
    let installed = fixture.binary("spice-installed", b"installed executable\n");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bootstrap listener binds");
    let address = listener.local_addr().expect("bootstrap address reads");
    let endpoint = format!("http://{address}");
    let discovery = fixture.publish(&endpoint);
    let binary = executable_digest(&installed).expect("installed executable hashes");
    let server = serve_bootstrap(listener, discovery, binary);
    let request = RefreshRequest {
        restart: true,
        ..RefreshRequest::default()
    };
    let mut host = Scripted::new(|_: &Issued| already_up());
    let mut output = Vec::new();

    let restarted = restart_admitting_reappearance(
        &mut host,
        &repository,
        &installed,
        &restart_binding(Some("127.0.0.1"), Some(address.port())),
        &request,
        &mut output,
    );
    server.join().expect("bootstrap server joins");

    assert_eq!(
        restarted.expect("the installed resident is already ready"),
        Some(endpoint)
    );
    assert_eq!(host.issued.len(), 1, "the ready resident was stopped");
}

/// A predecessor that wins the dark interval is stopped before one bounded retry.
#[test]
fn a_concurrent_predecessor_is_replaced_by_the_installed_resident() {
    let fixture = Fixture::new("restart-predecessor-race");
    let repository = fixture.repository();
    let discovery_path = repository
        .admit_state()
        .expect("fixture state is admitted")
        .serve()
        .to_path_buf();
    let endpoint = "http://127.0.0.1:43822";
    let installed = fixture.binary("spice-installed", b"installed executable\n");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bootstrap listener binds");
    let address = listener.local_addr().expect("bootstrap address reads");
    let discovery = fixture.publish(&format!("http://{address}"));
    let predecessor = Digest::compute(wire::Domain::DistArtifact, b"predecessor executable");
    let server = serve_bootstrap(listener, discovery, predecessor);
    let request = RefreshRequest {
        restart: true,
        ..RefreshRequest::default()
    };
    let mut withdrawn = false;
    let mut host = Scripted::new(|issued: &Issued| {
        if issued.is_down() {
            fs::remove_file(&discovery_path).expect("the predecessor withdraws");
            withdrawn = true;
            stopped()
        } else if withdrawn {
            ready(endpoint)
        } else {
            already_up()
        }
    });
    let mut output = Vec::new();

    let restarted = restart_admitting_reappearance(
        &mut host,
        &repository,
        &installed,
        &restart_binding(Some("127.0.0.1"), Some(43822)),
        &request,
        &mut output,
    );
    server.join().expect("bootstrap server joins");

    assert_eq!(
        restarted.expect("the predecessor is replaced"),
        Some(endpoint.to_owned())
    );
    assert_eq!(
        host.issued,
        [
            Issued::up(&installed, &["--host", "127.0.0.1", "--port", "43822"]),
            Issued::down(&installed),
            Issued::up(&installed, &["--host", "127.0.0.1", "--port", "43822"]),
        ],
        "the predecessor was not stopped and retried once"
    );
}

/// A clean stop is not reported as a refusal.
#[test]
fn a_clean_stop_withdraws_without_reporting_a_refusal() {
    let fixture = Fixture::new("stop-clean");
    let repository = fixture.repository();
    fixture.publish(&format!("http://127.0.0.1:{}", unbound_port()));
    let mut host = Scripted::new(|_: &Issued| stopped());

    let mut output = Vec::new();
    let result = stop(
        &mut host,
        &repository,
        Path::new("/installed/spice"),
        &mut output,
    );
    let output = String::from_utf8_lossy(&output).into_owned();

    assert!(result.is_ok(), "a clean stop was refused: {result:?}");
    assert_eq!(host.issued, [Issued::down(Path::new("/installed/spice"))]);
    assert!(
        !output.contains("down refused"),
        "a clean stop was reported as a refusal: {output}"
    );
}

/// A resident already gone refuses the stop, and the refresh proceeds anyway,
/// republishing the refusal on its own stream.
#[test]
fn a_refused_stop_with_a_dead_endpoint_proceeds_and_republishes_the_refusal() {
    let fixture = Fixture::new("stop-refused-dead");
    let repository = fixture.repository();
    fixture.publish(&format!("http://127.0.0.1:{}", unbound_port()));
    let mut host = Scripted::new(|_: &Issued| {
        exited(7, Vec::new(), "resident::not-running nothing to stop\n")
    });

    let mut output = Vec::new();
    let result = stop(
        &mut host,
        &repository,
        Path::new("/installed/spice"),
        &mut output,
    );
    let output = String::from_utf8_lossy(&output).into_owned();

    assert!(
        result.is_ok(),
        "a resident that was already gone failed the refresh: {result:?}"
    );
    assert!(
        output.contains("resident::not-running nothing to stop"),
        "the refusal text never reached the refresh's own stream: {output}"
    );
    assert!(
        output.contains("nothing answers the endpoint; continuing"),
        "proceeding past a dead endpoint was never reported: {output}"
    );
}

/// A refused stop with the endpoint still live is named as the refusal.
#[test]
fn a_refused_stop_with_a_live_endpoint_is_named() {
    let fixture = Fixture::new("stop-refused-live");
    let repository = fixture.repository();
    let listener = TcpListener::bind("127.0.0.1:0").expect("a scratch port binds");
    let bound = listener.local_addr().expect("the scratch port is named");
    fixture.publish(&format!("http://127.0.0.1:{}", bound.port()));
    let mut host =
        Scripted::new(|_: &Issued| exited(7, Vec::new(), "resident::down-refused still serving\n"));

    let mut output = Vec::new();
    let result = stop(
        &mut host,
        &repository,
        Path::new("/installed/spice"),
        &mut output,
    );
    drop(listener);

    assert!(
        matches!(&result, Err(RefreshError::ResidentStopRefused(refusal))
            if refusal.contains("resident::down-refused still serving")),
        "a refused stop over a live endpoint was not named: {result:?}"
    );
}

/// An install refusal that left the receipt and executable untouched is
/// returned as itself without reinstalling anything, while the resident this
/// refresh stopped is still started again.
#[test]
fn an_untouched_install_refusal_restarts_the_resident_it_stopped() {
    let fixture = Fixture::new("untouched-resident-refusal");
    let repository = fixture.repository();
    let installed = install::installed_binary(&repository);
    place(&repository, b"first build\n");
    let parked = park(&repository, &installed)
        .expect("the installed executable is parked")
        .expect("an installed executable exists to park");
    let before = snapshot_install(&repository, &installed).expect("install state snapshots");
    let refusal = InstallError::Drift {
        path: ".spice-install/githooks/pre-commit".to_owned(),
        observed: None,
    };
    let mut host = Scripted::new(|_: &Issued| ready("http://127.0.0.1:8767"));

    let mut output = Vec::new();
    let reported = restore_if_changed(
        &mut host,
        &repository,
        &Rollback {
            installed: &installed,
            executable: Some(&parked),
            resident_executable: None,
            published: Some("http://127.0.0.1:8767"),
            restart: true,
            binding: restart_binding(Some("127.0.0.1"), Some(8767)),
            resident: Default::default(),
        },
        &before,
        &mut output,
        refusal,
    );

    assert!(matches!(
        &reported,
        RefreshError::Install(InstallError::Drift { path, .. })
            if path == ".spice-install/githooks/pre-commit"
    ));
    assert!(
        host.installs.is_empty(),
        "an unchanged executable was reinstalled"
    );
    assert_eq!(
        host.issued,
        [Issued::up(
            &installed,
            &["--host", "127.0.0.1", "--port", "8767"]
        )],
        "the stopped resident was not restored"
    );
}

/// A refresh that fails after the executable moved puts the previous one back
/// through that executable's own installer, plan then apply by digest, and
/// still reports the failure that forced it.
#[test]
fn a_failed_refresh_restores_the_parked_executable_and_reports_its_cause() {
    let fixture = Fixture::new("restore");
    let repository = fixture.repository();
    let installed = install::installed_binary(&repository);
    place(&repository, b"first build\n");
    let parked = park(&repository, &installed)
        .expect("the installed executable is parked")
        .expect("an installed executable exists to park");
    place(&repository, b"second build\n");
    let mut host = Scripted::new(parked_installer(parked.clone(), &repository, unexpected));

    let mut output = Vec::new();
    let reported = restore(
        &mut host,
        &repository,
        &Rollback {
            installed: &installed,
            executable: Some(&parked),
            resident_executable: None,
            published: None,
            restart: false,
            binding: RestartBinding::default(),
            resident: Default::default(),
        },
        &mut output,
        RefreshError::ResidentUnready,
    );

    assert!(
        matches!(reported, RefreshError::ResidentUnready),
        "a successful restore replaced the reported cause: {reported}"
    );
    assert_eq!(
        host.issued,
        [
            Issued::install(&parked, None),
            Issued::install(&parked, Some(&parked_plan_digest())),
        ],
        "the parked executable did not publish itself: {}",
        String::from_utf8_lossy(&output)
    );
    assert!(
        host.installs.is_empty(),
        "the in-process installer ran for a restore"
    );
    assert_eq!(
        fs::read(&installed).expect("installed executable reads"),
        b"first build\n"
    );
}

/// A parked executable that refuses its own install leaves the restore
/// reported as such, carrying the failure that forced it, with no in-process
/// installer standing in and no resident restarted over a mixed tree.
#[test]
fn a_parked_executable_refusing_its_install_is_a_failed_restore() {
    let fixture = Fixture::new("restore-refused");
    let repository = fixture.repository();
    let installed = install::installed_binary(&repository);
    place(&repository, b"first build\n");
    let parked = park(&repository, &installed)
        .expect("the installed executable is parked")
        .expect("an installed executable exists to park");
    place(&repository, b"second build\n");
    let mut host = Scripted::new({
        let parked = parked.clone();
        move |issued: &Issued| {
            assert_eq!(issued, &Issued::install(&parked, None));
            install_refused(
                "install::record-refused",
                "the install record is a later plan version",
            )
        }
    });

    let mut output = Vec::new();
    let reported = restore(
        &mut host,
        &repository,
        &Rollback {
            installed: &installed,
            executable: Some(&parked),
            resident_executable: None,
            published: Some("http://127.0.0.1:8767"),
            restart: true,
            binding: restart_binding(Some("127.0.0.1"), Some(8767)),
            resident: Default::default(),
        },
        &mut output,
        RefreshError::ResidentStartRefused("journal::schema-refused".to_owned()),
    );

    match &reported {
        RefreshError::RestoreFailed { cause, error } => {
            assert!(
                cause.contains("journal::schema-refused"),
                "the start refusal was not carried: {cause}"
            );
            assert!(
                matches!(
                    error,
                    RestoreError::Refused { stage: RestoreStage::Plan, reason, .. }
                        if reason == "install::record-refused"
                ),
                "the parked executable's refusal was not reported: {error}"
            );
        }
        other => panic!("a refused restore was not reported: {other}"),
    }
    assert_eq!(host.issued, [Issued::install(&parked, None)]);
    assert!(
        host.installs.is_empty(),
        "the in-process installer stood in for the parked executable"
    );
    assert_eq!(
        fs::read(&installed).expect("installed executable reads"),
        b"second build\n",
        "a refused restore changed the installed executable"
    );
}

/// A refresh that stopped a resident restores both the prior installed bytes
/// and the distinct command authority that had actually been serving them.
#[test]
fn a_failed_refresh_restarts_the_resident_it_stopped() {
    let fixture = Fixture::new("resident-restore");
    let repository = fixture.repository();
    let installed = install::installed_binary(&repository);
    let endpoint = "http://127.0.0.1:8767";
    let resident = fixture.0.join("spice-serving-predecessor");
    place(&repository, b"first build\n");
    let parked = park(&repository, &installed)
        .expect("the installed executable is parked")
        .expect("an installed executable exists to park");
    place(&repository, b"second build\n");
    let mut host = Scripted::new(parked_installer(
        parked.clone(),
        &repository,
        |_: &Issued| ready(endpoint),
    ));

    let mut output = Vec::new();
    let reported = restore(
        &mut host,
        &repository,
        &Rollback {
            installed: &installed,
            executable: Some(&parked),
            resident_executable: Some(&resident),
            published: Some(endpoint),
            restart: true,
            binding: restart_binding(Some("127.0.0.1"), Some(8767)),
            resident: Default::default(),
        },
        &mut output,
        RefreshError::ResidentUnready,
    );

    assert!(
        matches!(reported, RefreshError::ResidentUnready),
        "a successful restore replaced the reported cause: {reported}"
    );
    assert_eq!(
        host.issued,
        [
            Issued::install(&parked, None),
            Issued::install(&parked, Some(&parked_plan_digest())),
            Issued::up(&resident, &["--host", "127.0.0.1", "--port", "8767"]),
        ],
        "the parked executable was not republished before its resident restarted"
    );
    assert_eq!(
        fs::read(&installed).expect("installed executable reads"),
        b"first build\n"
    );
}

/// Nothing is parked when the appliance has no executable installed yet.
#[test]
fn a_first_install_parks_no_previous_executable() {
    let fixture = Fixture::new("unparked");
    let repository = fixture.repository();
    let installed = install::installed_binary(&repository);
    assert_eq!(
        park(&repository, &installed).expect("parking answers"),
        None,
        "an absent executable was reported as parked"
    );
}

/// Published origins split into the host and port `up` accepts.
#[test]
fn a_published_origin_splits_into_a_host_and_port() {
    assert_eq!(
        endpoint_parts("http://127.0.0.1:8767"),
        Some(("127.0.0.1".to_owned(), 8767))
    );
    assert_eq!(
        endpoint_parts("http://127.0.0.1:8767/app"),
        Some(("127.0.0.1".to_owned(), 8767))
    );
    assert_eq!(
        endpoint_parts("127.0.0.1:8767"),
        Some(("127.0.0.1".to_owned(), 8767))
    );
    assert_eq!(endpoint_parts("http://127.0.0.1"), None);
    assert_eq!(endpoint_parts("http://127.0.0.1:port"), None);
}

/// An explicit host or port overrides the published one, and either alone still
/// resolves the other from what the resident had published.
#[test]
fn an_explicit_endpoint_overrides_the_published_one() {
    let published = Some("http://127.0.0.1:8767");
    let default = RefreshRequest::default();
    assert_eq!(
        requested_endpoint(&default, published, None),
        (Some("127.0.0.1".to_owned()), Some(8767))
    );

    let ported = RefreshRequest {
        port: Some(8768),
        ..RefreshRequest::default()
    };
    assert_eq!(
        requested_endpoint(&ported, published, None),
        (Some("127.0.0.1".to_owned()), Some(8768))
    );

    let hosted = RefreshRequest {
        host: Some("0.0.0.0".to_owned()),
        ..RefreshRequest::default()
    };
    assert_eq!(
        requested_endpoint(&hosted, published, None),
        (Some("0.0.0.0".to_owned()), Some(8767))
    );
}

/// With nothing published and nothing asked for, the resident is left to pick
/// its own endpoint rather than being pinned to a guess.
#[test]
fn an_unpublished_endpoint_with_no_override_is_left_unbound() {
    assert_eq!(
        requested_endpoint(&RefreshRequest::default(), None, None),
        (None, None)
    );
}

/// An explicitly named port survives having no published endpoint to pair with.
///
/// The first refresh of a repository is exactly when nothing is published yet,
/// so the explicit half must survive on its own.
#[test]
fn an_explicit_endpoint_survives_an_unpublished_counterpart() {
    let ported = RefreshRequest {
        port: Some(8768),
        ..RefreshRequest::default()
    };
    assert_eq!(
        requested_endpoint(&ported, None, None),
        (None, Some(8768)),
        "an explicitly named port was dropped for want of a host"
    );

    let hosted = RefreshRequest {
        host: Some("127.0.0.1".to_owned()),
        ..RefreshRequest::default()
    };
    assert_eq!(
        requested_endpoint(&hosted, None, None),
        (Some("127.0.0.1".to_owned()), None),
        "an explicitly named host was dropped for want of a port"
    );
}

fn restart_binding(host: Option<&str>, port: Option<u16>) -> RestartBinding {
    RestartBinding {
        host: host.map(str::to_owned),
        port,
        ..RestartBinding::default()
    }
}
