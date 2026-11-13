use std::fs;
use std::time::Duration;

use super::test_support::{InertProbes, LaunchFixture};
use super::*;

#[test]
fn occupant_deadline_and_absence_use_their_independent_configured_horizons() {
    let fixture = LaunchFixture::new();
    fs::write(
        fixture.root.join("spice.toml"),
        "[lane]\nlease = \"1s\"\nruntime = \"5s\"\n",
    )
    .expect("independent lease and runtime policy writes");

    let launch = occupant_launch(
        &fixture.paths,
        fixture.request(vendor::Resume::Fresh),
        fixture.reporter(),
        &InertProbes::within(&fixture.paths),
    )
    .expect("occupant launch composes");
    assert_eq!(launch.spec.deadlines.runtime(), Duration::from_secs(5));
    assert_eq!(launch.lease, Duration::from_secs(1));
    let Cancellation::Token(spec_cancellation) = launch.spec.wait.cancellation() else {
        panic!("occupant wait policy has no supervisor cancellation");
    };
    assert!(!spec_cancellation.is_cancelled());
    launch.cancellation.cancel();
    assert!(spec_cancellation.is_cancelled());
}

#[test]
fn absent_cargo_lock_launches_without_rust_environment_or_vendor_shell() {
    let fixture = LaunchFixture::new();
    fs::remove_file(fixture.root.join("Cargo.lock")).expect("remove fixture Cargo lock");
    let repository = Repository::discover(&fixture.root).expect("fixture repository discovers");

    let (environment, _, _) = launch_environment(
        &repository,
        &fixture.root.join("omp"),
        "rtk",
        fixture.lane,
        &InertProbes::within(&fixture.paths),
    )
    .expect("non-Rust launch environment composes");
    for name in [
        "SHELL",
        spice::cargo_isolation::CARGO_HOME_VARIABLE,
        spice::cargo_isolation::BOOTSTRAP_HOME_VARIABLE,
        spice::cargo_isolation::NATIVE_CARGO_VARIABLE,
        spice::cargo_isolation::NATIVE_RUSTC_VARIABLE,
        spice::cargo_isolation::NATIVE_RUSTDOC_VARIABLE,
        spice::cargo_isolation::RUSTC_VARIABLE,
        spice::cargo_isolation::RUSTDOC_VARIABLE,
    ] {
        assert!(
            environment.entries().iter().all(|(key, _)| key != name),
            "non-Rust launch retained {name}"
        );
    }
}

#[test]
fn zero_config_repository_without_a_lock_composes_default_omp_and_refuses_other_vendors() {
    let fixture = LaunchFixture::new();
    for path in ["Cargo.lock", "Cargo.toml", "spice.toml"] {
        fs::remove_file(fixture.root.join(path)).expect("remove Rust fixture input");
    }
    let worktree = fixture.root.to_str().expect("fixture worktree is UTF-8");
    let omp: wire::Name = "omp".parse().expect("default vendor name");
    let other: wire::Name = "other".parse().expect("unsupported vendor name");
    let request = |vendor| OccupantLaunchRequest {
        worktree,
        vendor,
        epoch: fixture.epoch,
        lane: fixture.lane,
        occupant: fixture.occupant,
        resume: vendor::Resume::Fresh,
        fast_mode: false,
        model: None,
        effort: None,
    };

    occupant_launch(
        &fixture.paths,
        request(&omp),
        fixture.reporter(),
        &InertProbes::within(&fixture.paths),
    )
    .expect("zero-config non-Rust repository composes default omp");

    let Err(Outcome::Refusal(refusal)) = occupant_launch(
        &fixture.paths,
        request(&other),
        fixture.reporter(),
        &InertProbes::within(&fixture.paths),
    ) else {
        panic!("a lane recorded under an unsupported vendor must refuse to wake");
    };
    let diagnostic = refusal.diagnostic().expect("the refusal is diagnosed");
    assert_eq!(diagnostic.reason(), "lane::vendor-unsupported");
    assert_eq!(diagnostic.expected(), Some("omp or scripted"));
    assert_eq!(diagnostic.observed(), Some("other"));
}

#[test]
fn a_worktree_in_another_repository_refuses_the_lane_instead_of_faulting_the_resident() {
    let resident = LaunchFixture::new();
    let foreign = LaunchFixture::new();

    let Err(Outcome::Refusal(refusal)) = occupant_launch(
        &resident.paths,
        foreign.request(vendor::Resume::Fresh),
        resident.reporter(),
        &InertProbes::within(&resident.paths),
    ) else {
        panic!("a copied journal naming another repository's worktree must park only that lane");
    };
    let diagnostic = refusal.diagnostic().expect("the refusal is diagnosed");
    assert_eq!(diagnostic.reason(), "lane::foreign-worktree");
    assert_eq!(
        diagnostic.observed(),
        Some(
            foreign
                .paths
                .common
                .to_str()
                .expect("fixture path is UTF-8")
        )
    );
}

#[cfg(unix)]
#[test]
fn isolation_binds_the_private_home_and_the_admitted_native_toolchain() {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = LaunchFixture::new();
    let bootstrap = fixture.root.join("bootstrap");
    fs::create_dir(&bootstrap).expect("fixture bootstrap home creates");
    let tool = |name: &str| {
        let path = fixture.root.join(name);
        fs::write(&path, "#!/bin/sh\nexit 0\n").expect("fixture tool writes");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .expect("fixture tool becomes executable");
        path
    };
    let (cargo, rustc, rustdoc) = (tool("cargo"), tool("rustc"), tool("rustdoc"));
    let host = spice::cargo_isolation::Host::new(bootstrap, cargo, rustc, rustdoc)
        .expect("fixture host admits");
    let home = fixture.root.join("lane-home");

    let environment = isolated_cargo_environment(&host, &home);
    let path = |path: &Path| path.as_os_str().to_owned();
    assert_eq!(
        environment,
        [
            (spice::cargo_isolation::CARGO_HOME_VARIABLE, path(&home)),
            (
                spice::cargo_isolation::BOOTSTRAP_HOME_VARIABLE,
                path(host.bootstrap_home())
            ),
            (
                spice::cargo_isolation::NATIVE_CARGO_VARIABLE,
                path(host.cargo())
            ),
            (
                spice::cargo_isolation::NATIVE_RUSTC_VARIABLE,
                path(host.rustc())
            ),
            (
                spice::cargo_isolation::NATIVE_RUSTDOC_VARIABLE,
                path(host.rustdoc())
            ),
            (spice::cargo_isolation::RUSTC_VARIABLE, path(host.rustc())),
            (
                spice::cargo_isolation::RUSTDOC_VARIABLE,
                path(host.rustdoc())
            ),
        ]
        .map(|(name, value)| (OsString::from(name), value))
    );
}

#[test]
fn non_regular_cargo_lock_refuses_before_rust_tool_probe() {
    let fixture = LaunchFixture::new();
    let lock = fixture.root.join("Cargo.lock");
    fs::remove_file(&lock).expect("remove fixture Cargo lock");
    fs::create_dir(&lock).expect("replace Cargo lock with directory");
    let repository = Repository::discover(&fixture.root).expect("fixture repository discovers");

    let error = cargo_environment(
        &repository,
        fixture.lane,
        &InertProbes::within(&fixture.paths),
    )
    .expect_err("non-regular Cargo lock refuses");
    assert!(matches!(
        error,
        Outcome::Fault(ref fault)
            if fault.reason == "lane::cargo-isolation-unavailable"
    ));
}

#[test]
fn the_session_stamp_is_omp_s_millisecond_iso_spelling() {
    let now: jiff::Timestamp = "2026-09-16T06:43:21.805123Z".parse().expect("an instant");
    assert_eq!(session_stamp(now), "2026-09-16T06:43:21.805Z");
    let whole: jiff::Timestamp = "2026-09-16T06:43:21Z".parse().expect("an instant");
    assert_eq!(session_stamp(whole), "2026-09-16T06:43:21.000Z");
}

/// Probes for a launch that must not read the lane's session authority.
struct UnreadAuthority(InertProbes);

impl LaunchProbes for UnreadAuthority {
    fn advance(&self, repository: &Repository) -> String {
        self.0.advance(repository)
    }

    fn shadow(&self, repository: &Repository, base: Environment) -> CommandResult<Environment> {
        self.0.shadow(repository, base)
    }

    fn cargo(
        &self,
        repository: &Repository,
        lane: CanonicalUuid,
    ) -> CommandResult<Vec<(OsString, OsString)>> {
        self.0.cargo(repository, lane)
    }

    fn rtk(
        &self,
        executable: &std::ffi::OsStr,
        working: &Path,
        environment: &Environment,
    ) -> wire::body::RtkHealth {
        self.0.rtk(executable, working, environment)
    }

    fn session(&self, _worktree: PathBuf) -> crate::session_command::Environment {
        panic!("a resumed launch plan read the lane's authority")
    }

    fn variable(&self, name: &str) -> Option<OsString> {
        self.0.variable(name)
    }
}

#[test]
fn a_resumed_launch_defers_its_authority_to_delivery() {
    let fixture = LaunchFixture::new();
    let resume = vendor::Resume::Session(
        vendor::SessionId::new(super::test_support::SESSION.to_owned()).expect("fixture identity"),
    );
    let launch = occupant_launch(
        &fixture.paths,
        fixture.request(resume),
        fixture.reporter(),
        &UnreadAuthority(InertProbes::within(&fixture.paths)),
    )
    .expect("resumed launch composes without reading authority");
    let Some(crate::resident::supervisor::command::FirstTurn::Resumed(_)) = launch.first_prompt
    else {
        panic!("a resumed launch's first turn is not marked resumed");
    };

    let launch = occupant_launch(
        &fixture.paths,
        fixture.request(vendor::Resume::Fresh),
        fixture.reporter(),
        &InertProbes::within(&fixture.paths),
    )
    .expect("fresh launch composes");
    let Some(crate::resident::supervisor::command::FirstTurn::Fresh(_)) = launch.first_prompt
    else {
        panic!("a fresh launch's first turn is not marked fresh");
    };
}

#[test]
fn a_fresh_launch_pins_the_session_it_resumes_where_omp_files_the_worktree_under_home() {
    let fixture = LaunchFixture::new();
    let probes = InertProbes::within(&fixture.paths);
    let launch = occupant_launch(
        &fixture.paths,
        fixture.request(vendor::Resume::Fresh),
        fixture.reporter(),
        &probes,
    )
    .expect("fresh launch composes");
    let identity = launch
        .spec
        .argv
        .arguments()
        .windows(2)
        .find(|pair| pair[0] == "--resume")
        .and_then(|pair| pair[1].to_str())
        .expect("a fresh launch resumes a pinned session");

    let home = fixture.paths.runtime.join("home");
    let layout = vendor::OMP.transcript_layout();
    let sessions = home
        .join(layout.default_home)
        .join(layout.sessions_directory);
    let folder = vendor::omp::session_paths::session_directory(
        Some(&canonical(&home)),
        Some(&canonical(&std::env::temp_dir())),
        &canonical(&fixture.root),
    );
    let mut folders = fs::read_dir(&sessions)
        .expect("the pinned session lies beneath the probed home")
        .map(|entry| entry.expect("session folder entry").file_name());
    assert_eq!(folders.next(), Some(folder.clone().into()));
    assert_eq!(folders.next(), None);
    let pinned: Vec<_> = fs::read_dir(sessions.join(&folder))
        .expect("the worktree's session folder lists")
        .map(|entry| entry.expect("pinned session entry").file_name())
        .collect();
    let [file] = pinned.as_slice() else {
        panic!("a fresh launch pins exactly one session, found {pinned:?}");
    };
    assert!(
        file.to_str()
            .is_some_and(|name| name.ends_with(&format!("_{identity}.jsonl"))),
        "the pinned file {file:?} is not the resumed session {identity}"
    );
}

#[test]
fn an_inert_probe_without_a_fixture_home_refuses_to_pin_a_session() {
    let fixture = LaunchFixture::new();
    let repository = Repository::discover(&fixture.root).expect("fixture repository discovers");
    let Err(Outcome::Fault(fault)) = pin_session(&repository, &InertProbes::default()) else {
        panic!("a fixture probe without a home must refuse session pinning");
    };
    assert_eq!(fault.reason, "lane::session-home-unavailable");
    assert!(!fixture.paths.runtime.join("home").exists());
}
