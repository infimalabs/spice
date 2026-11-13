//! Configured pre-commit admission and policy checks.

use super::*;

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::sync::atomic::{AtomicU64, Ordering};

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = env::temp_dir().join(format!(
            "spice-configured-pre-commit-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&root).expect("create configured-command scratch");
        Self { root }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn parsed_commands(rows: &[String]) -> Vec<PreCommitStep> {
    let document = format!("[policy]\npre_commit = [\n{},\n]\n", rows.join(",\n"));
    toml::from_str::<spice::config::Root>(&document)
        .unwrap_or_else(|error| panic!("parse configured command fixture {document:?}: {error}"))
        .policy()
        .pre_commit()
        .to_vec()
}

fn row(label: &str, argv: &[&str], scopes: &[&str]) -> String {
    let scopes = if scopes.is_empty() {
        String::new()
    } else {
        format!(", scopes = {{ paths = {} }}", json(&scopes))
    };
    format!(
        "{{ label = {}, run = {}{scopes} }}",
        json(&label),
        json(&argv),
    )
}

fn initialize_repository(root: &Path) -> Repository {
    fs::create_dir(root.join(".git")).expect("create fixture Git directory");
    Repository::discover(root).expect("discover configured-command fixture")
}

fn append_granted_trust(
    journal_path: &Path,
    digest: Digest,
    worktree: Option<String>,
    origin: &str,
    instant: &str,
) {
    let mut authority = journal::Journal::open(journal_path).expect("open fixture authority");
    let mut write = journal::JournalWrite::new(
        wire::Actor::Spice,
        origin.parse().expect("fixture origin"),
        instant.parse().expect("fixture instant"),
        wire::Producer::CommandDoor,
    );
    let pended = write
        .fact(
            wire::body::TrustPended {
                surface: crate::trust_command::SURFACE_NAME.to_owned(),
                digest: digest.clone(),
                worktree: worktree.clone(),
                previous: None,
            },
            journal::FactCause::Root,
        )
        .expect("build fixture trust fact");
    write.trust_head(pended, move |_| wire::TrustHead {
        digest,
        state: TrustState::Granted,
        worktree,
    });
    authority.apply(write).expect("commit fixture trust grant");
}

#[cfg(unix)]
fn publish_executable(path: &Path, body: &[u8]) {
    fs::create_dir_all(path.parent().expect("executable has a parent"))
        .expect("create executable parent");
    fs::write(path, body).expect("write executable fixture");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .expect("make executable fixture executable");
}

#[test]
fn authored_spice_program_requires_and_uses_one_exact_executable_path() {
    let authored = vec![
        "spice".to_owned(),
        "dev".to_owned(),
        "cargo".to_owned(),
        "--".to_owned(),
        "run".to_owned(),
    ];
    let executable = Path::new("/git common/.spice-install/bin/spice");
    let resolved = process_argv(&authored, Some(executable)).expect("resolve exact appliance");
    assert_eq!(resolved[0], executable.as_os_str());
    assert_eq!(
        resolved[1..],
        authored[1..].iter().map(OsString::from).collect::<Vec<_>>()
    );

    let refusal = process_argv(&authored, None).expect_err("missing wrapper authority refuses");
    assert_eq!(refusal.reason(), "gate::pre-commit-wrapper-missing");
    assert!(refusal.detail().contains("wrappers.spice"));
}

#[test]
fn configured_spice_route_refuses_missing_installed_authority() {
    let scratch = Scratch::new("missing-spice-route");
    let repository = initialize_repository(&scratch.root);
    fs::write(
        scratch.root.join("spice.toml"),
        concat!(
            "[wrappers]\nspice = 'appliance'\n",
            "[policy]\npre_commit = [{ label = 'source', run = ['spice', 'dev', 'cargo', '--', 'check'] }]\n",
        ),
    )
    .expect("write missing-wrapper policy");

    let refusal = DispatchPolicy::load(&repository)
        .err()
        .expect("missing installed authority must refuse");
    assert_eq!(refusal.reason(), "gate::pre-commit-wrapper-refused");
    assert!(
        refusal
            .detail()
            .contains("lane::wrapper-command-unavailable"),
        "{}",
        refusal.detail(),
    );
}

#[test]
fn unrelated_wrapper_declaration_does_not_change_non_spice_commands() {
    let scratch = Scratch::new("unused-spice-route");
    let repository = initialize_repository(&scratch.root);
    fs::write(
        scratch.root.join("spice.toml"),
        concat!(
            "[wrappers]\nspice = 'appliance'\n",
            "[policy]\npre_commit = [{ label = 'other', run = ['/bin/true'] }]\n",
        ),
    )
    .expect("write unrelated-wrapper policy");

    let policy = DispatchPolicy::load(&repository)
        .expect("an unused wrapper does not enter configured execution");
    assert!(policy.spice.is_none());
}

#[cfg(unix)]
#[test]
fn configured_spice_route_refuses_redirected_authority_after_capture() {
    let scratch = Scratch::new("redirected-spice-route");
    let repository = initialize_repository(&scratch.root);
    fs::write(
        scratch.root.join("spice.toml"),
        concat!(
            "[wrappers]\nspice = 'appliance'\n",
            "[policy]\npre_commit = [{ label = 'source', run = ['spice', 'dev', 'cargo', '--', 'check'] }]\n",
        ),
    )
    .expect("write admitted-wrapper policy");
    let installed = spice::install::installed_binary(&repository);
    publish_executable(&installed, b"#!/bin/sh\nexit 0\n");
    let policy = DispatchPolicy::load(&repository).expect("capture admitted exact route");

    fs::remove_file(&installed).expect("remove captured appliance");
    let redirect = scratch.root.join("redirect-target");
    publish_executable(&redirect, b"#!/bin/sh\nexit 0\n");
    symlink(&redirect, &installed).expect("redirect captured appliance");

    let refusal = policy
        .verify_current(&repository)
        .expect_err("redirected authority must refuse");
    assert_eq!(refusal.reason(), "gate::pre-commit-wrapper-refused");
    assert!(
        refusal
            .detail()
            .contains("lane::wrapper-command-redirected"),
        "{}",
        refusal.detail(),
    );
}

#[test]
fn live_progress_selects_only_the_exact_unscoped_source_gate_contract() {
    let commands = parsed_commands(&[
        row(
            "label is not authority",
            &[
                "spice",
                "dev",
                "cargo",
                "--",
                "run",
                "--quiet",
                "--locked",
                "--package",
                "spice",
                "--bin",
                "spice",
                "--",
                "gate",
                "selection",
            ],
            &[],
        ),
        row(
            "different selector",
            &[
                "spice",
                "dev",
                "cargo",
                "--",
                "run",
                "--quiet",
                "--locked",
                "--package",
                "spice",
                "--bin",
                "spice",
                "--",
                "gate",
                "staging",
            ],
            &[],
        ),
        row(
            "scoped source gate",
            &[
                "spice",
                "dev",
                "cargo",
                "--",
                "run",
                "--quiet",
                "--locked",
                "--package",
                "spice",
                "--bin",
                "spice",
                "--",
                "gate",
                "selection",
            ],
            &["spice/**"],
        ),
        row("unrelated configured command", &["/bin/true"], &[]),
    ]);

    assert!(is_source_gate_contract(&commands[0]));
    assert!(!is_source_gate_contract(&commands[1]));
    assert!(!is_source_gate_contract(&commands[2]));
    assert!(!is_source_gate_contract(&commands[3]));
}

#[test]
fn exact_source_gate_alone_raises_the_configured_capture_floor() {
    let bounds = ExecutionBounds::production(Duration::from_secs(1));

    assert_eq!(bounds.stderr_capture_limit(false), OUTPUT_LIMIT);
    assert_eq!(bounds.stderr_capture_limit(true), 1024 * 1024);
}

#[test]
fn trust_refusal_names_exact_digest_provenance_path_and_command_words() {
    let scratch = Scratch::new("trust-refusal");
    let repository = initialize_repository(&scratch.root);
    fs::write(
        scratch.root.join("spice.toml"),
        concat!(
            "[policy]\n",
            "pre_commit = [{ label = 'exact', run = ['/bin/echo', 'one', 'two'] }]\n",
        ),
    )
    .expect("write executable policy fixture");
    let policy = DispatchPolicy::load(&repository).expect("load executable policy");

    let absent = policy.trust_refusal_detail(None, None);
    assert!(absent.contains("standing=absent"), "{absent}");
    assert!(absent.contains("producer_worktree=absent"), "{absent}");
    assert!(
        absent.contains(&scratch.root.display().to_string()),
        "{absent}"
    );

    let detail = policy.trust_refusal_detail(Some(TrustState::Pended), Some("/worktrees/producer"));
    assert!(
        detail.contains(&policy.surface.digest().to_string()),
        "{detail}"
    );
    assert!(detail.contains("standing=pended"), "{detail}");
    assert!(
        detail.contains("producer_worktree=\"/worktrees/producer\""),
        "{detail}"
    );
    assert!(detail.contains("provenance=tracked"), "{detail}");
    assert!(
        detail.contains(&scratch.root.display().to_string()),
        "{detail}"
    );
    assert!(detail.contains("spice.toml"), "{detail}");
    for word in ["exact", "/bin/echo", "one", "two"] {
        assert!(detail.contains(word), "{detail}");
    }
}

#[test]
fn durable_trust_is_read_only_and_required_before_execution() {
    let scratch = Scratch::new("durable-trust");
    let repository = initialize_repository(&scratch.root);
    fs::write(
        scratch.root.join("spice.toml"),
        "[policy]\npre_commit = [{ label = 'exact', run = ['/bin/true'] }]\n",
    )
    .expect("write trusted policy fixture");
    let policy = DispatchPolicy::load(&repository).expect("load executable policy");
    let state = repository.admit_state().expect("admit fixture state paths");
    fs::create_dir_all(state.data()).expect("create fixture authority parent");
    drop(journal::Journal::open(state.journal()).expect("baseline fixture authority"));
    let before = fs::read(state.journal()).expect("read unruled authority");

    let refusal = policy
        .require_approved(&repository)
        .expect_err("an absent trust head must refuse execution");
    assert_eq!(refusal.reason(), "gate::pre-commit-configuration-untrusted");
    assert!(refusal.detail().contains("standing=absent"));
    assert_eq!(
        fs::read(state.journal()).expect("reread unruled authority"),
        before,
        "trust admission must not auto-pend or auto-grant the digest",
    );

    append_granted_trust(
        state.journal(),
        policy.surface.digest().clone(),
        Some("/worktrees/producer".to_owned()),
        "recovery:configured-bound-trust-fixture",
        "2030-04-12T18:44:02.271Z",
    );
    let granted = fs::read(state.journal()).expect("read bound granted authority");

    policy
        .require_approved(&repository)
        .expect("the exact durable grant admits a sibling worktree");
    assert_eq!(
        fs::read(state.journal()).expect("reread granted authority"),
        granted,
        "approval checks are read-only",
    );
}

#[test]
fn a_bound_grant_does_not_admit_changed_command_words() {
    let scratch = Scratch::new("changed-digest-trust");
    let repository = initialize_repository(&scratch.root);
    let policy_path = scratch.root.join("spice.toml");
    fs::write(
        &policy_path,
        "[policy]\npre_commit = [{ label = 'exact', run = ['/bin/true'] }]\n",
    )
    .expect("write trusted policy fixture");
    let trusted = DispatchPolicy::load(&repository).expect("load trusted executable policy");
    let state = repository.admit_state().expect("admit fixture state paths");
    fs::create_dir_all(state.data()).expect("create fixture authority parent");
    append_granted_trust(
        state.journal(),
        trusted.surface.digest().clone(),
        Some("/worktrees/producer".to_owned()),
        "recovery:configured-changed-trust-fixture",
        "2030-04-12T18:44:03.271Z",
    );

    fs::write(
        policy_path,
        "[policy]\npre_commit = [{ label = 'changed', run = ['/bin/false'] }]\n",
    )
    .expect("change executable policy command words");
    let changed = DispatchPolicy::load(&repository).expect("load changed executable policy");
    assert_ne!(changed.surface.digest(), trusted.surface.digest());
    let refusal = changed
        .require_approved(&repository)
        .expect_err("a sibling's different digest remains untrusted");
    assert_eq!(refusal.reason(), "gate::pre-commit-configuration-untrusted");
    assert!(refusal.detail().contains("standing=absent"));
    assert!(refusal.detail().contains("/bin/false"));
}

#[test]
fn missing_trust_authority_refuses_with_the_exact_executable_capability() {
    let scratch = Scratch::new("missing-trust-authority");
    let repository = initialize_repository(&scratch.root);
    fs::write(
        scratch.root.join("spice.toml"),
        "[policy]\npre_commit = [{ label = 'exact', run = ['/bin/true'] }]\n",
    )
    .expect("write executable policy fixture");
    let policy = DispatchPolicy::load(&repository).expect("load executable policy");

    let refusal = policy
        .require_approved(&repository)
        .expect_err("a missing authority cannot silently admit execution");

    assert_eq!(refusal.reason(), "gate::pre-commit-configuration-untrusted");
    for evidence in [
        "standing=absent",
        "provenance=tracked",
        "spice.toml",
        "/bin/true",
        "trust_authority=",
    ] {
        assert!(refusal.detail().contains(evidence), "{}", refusal.detail());
    }
    assert!(
        refusal
            .detail()
            .contains(&policy.surface.digest().to_string()),
        "{}",
        refusal.detail(),
    );
}

#[test]
fn tracked_policy_movement_after_capture_refuses_even_when_bytes_are_restored() {
    let scratch = Scratch::new("policy-movement");
    let repository = initialize_repository(&scratch.root);
    let configuration = scratch.root.join("spice.toml");
    let original = "[policy]\npre_commit = [{ label = 'exact', run = ['/bin/true'] }]\n";
    fs::write(&configuration, original).expect("write executable policy fixture");
    let policy = DispatchPolicy::load(&repository).expect("load executable policy");
    fs::write(
        &configuration,
        "[policy]\npre_commit = [{ label = 'moved', run = ['/bin/false'] }]\n",
    )
    .expect("move policy bytes");
    fs::write(&configuration, original).expect("restore policy bytes");

    let refusal = policy
        .verify_current(&repository)
        .expect_err("capture generation must retain transient policy movement");

    assert_eq!(refusal.reason(), "gate::pre-commit-policy-moved");
    assert!(refusal.detail().contains("spice.toml"));
    assert!(
        refusal
            .detail()
            .contains(&policy.surface.digest().to_string())
    );
}

#[test]
fn recursive_dispatch_is_a_typed_refusal_before_supervision() {
    let outcome: Outcome<()> = recursion_refusal(OsStr::new("fixture-source"));
    let Outcome::Refusal(refusal) = outcome else {
        panic!("recursive dispatch must refuse");
    };
    assert_eq!(refusal.reason(), "gate::pre-commit-recursion");
    assert!(
        refusal
            .diagnostic()
            .and_then(RefusalDiagnostic::observed)
            .is_some_and(|observed| observed.contains("fixture-source")),
    );
}

#[test]
fn dispatch_possession_blocks_recursion_even_without_the_environment_marker() {
    let scratch = Scratch::new("dispatch-possession");
    let repository = initialize_repository(&scratch.root);
    let _outer = super::super::DispatchGuard::acquire_in::<()>(&repository)
        .expect("outer commit gate owns dispatch");

    let Outcome::Refusal(refusal) = super::super::DispatchGuard::acquire_in::<()>(&repository)
        .expect_err("nested commit gate must contend")
    else {
        panic!("dispatch contention must be a typed refusal");
    };

    assert_eq!(refusal.reason(), "gate::pre-commit-dispatch-active");
}
