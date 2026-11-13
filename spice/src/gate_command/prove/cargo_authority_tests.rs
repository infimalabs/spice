use std::ffi::{OsStr, OsString};
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

use super::cargo_authority;

const TOOLCHAIN: &str = "[toolchain]\nchannel = \"fixture-1.0\"\ntargets = []\n";

#[test]
fn staged_lockfile_uses_the_live_worktrees_private_home() {
    use spice::cargo_isolation::{CARGO_HOME_VARIABLE, Host, prepare_lane, prepare_manual};

    let root = std::env::temp_dir().join(format!("spice-prove-source-{}", uuid::Uuid::new_v4()));
    let worktree = root.join("worktree");
    let selected = root.join("selected");
    let bootstrap = root.join("bootstrap");
    let tools = root.join("tools");
    for directory in [&worktree, &selected, &bootstrap, &tools] {
        fs::create_dir_all(directory).unwrap();
    }
    gix::init(&worktree).unwrap();
    let repository = spice::repository::Repository::discover(&worktree).unwrap();
    let lockfile = "version = 4\npackage = []\n";
    fs::write(worktree.join("Cargo.lock"), lockfile).unwrap();
    fs::write(selected.join("Cargo.lock"), lockfile).unwrap();
    for root in [&worktree, &selected] {
        fs::write(root.join("rust-toolchain.toml"), TOOLCHAIN).unwrap();
    }
    for name in ["cargo", "rustc", "rustdoc"] {
        let path = tools.join(name);
        fs::write(&path, "#!/bin/sh\nexit 91\n").unwrap();
        sill::authority::set_owner_mode(&path, 0o700).unwrap();
    }
    let host = Host::new(
        bootstrap,
        tools.join("cargo"),
        tools.join("rustc"),
        tools.join("rustdoc"),
    )
    .unwrap();
    let lane = "018f82d4-6f59-7d06-92e3-bce913116411".parse().unwrap();
    let home = prepare_lane(&repository, lane, &host).unwrap();
    let mut environment = host.compiler_environment().unwrap().entries().to_vec();
    for (name, value) in &mut environment {
        if name == OsStr::new(CARGO_HOME_VARIABLE) {
            *value = home.home().as_os_str().to_owned();
        }
    }
    let accepted = super::cargo_source_authority(&worktree, &selected, &environment);
    assert!(
        accepted.is_ok(),
        "the selected copy retains its owner's admitted home"
    );
    let Err(error) = super::cargo_source_authority(&selected, &selected, &environment) else {
        panic!("the selected copy must not impersonate the home owner");
    };
    assert!(error.detail().contains("home-owner-mismatch"));

    // The working copy can diverge without changing the exact staged source.
    fs::write(
        worktree.join("Cargo.lock"),
        format!("{lockfile}# unstaged\n"),
    )
    .unwrap();
    assert!(super::cargo_source_authority(&worktree, &selected, &environment).is_ok());
    fs::write(
        selected.join("Cargo.lock"),
        format!("{lockfile}# staged drift\n"),
    )
    .unwrap();
    let Err(_) = super::cargo_source_authority(&worktree, &selected, &environment) else {
        panic!("a different selected lockfile needs its own admitted generation");
    };

    // Task synchronization moves both trees, while the supervised lane keeps
    // its original private home. Only the supported launcher admits the new
    // immutable source generation.
    let next_lock = format!("{lockfile}# staged drift\n");
    fs::write(worktree.join("Cargo.lock"), &next_lock).unwrap();
    assert!(
        super::cargo_source_authority(&worktree, &selected, &environment).is_err(),
        "synchronizing alone cannot admit new external sources",
    );
    spice::cargo_isolation::source_generation_authority(home.home(), lockfile.as_bytes())
        .expect("refused readmission leaves the prior generation usable");
    let admitted = prepare_manual(&repository, &host).unwrap();
    let (_, next) = super::cargo_source_authority(&worktree, &selected, &environment)
        .unwrap_or_else(|error| {
            panic!(
                "admitted synchronized generation refused: {}",
                error.detail()
            )
        });
    assert_ne!(
        accepted
            .as_ref()
            .unwrap_or_else(|error| panic!("{}", error.detail()))
            .1,
        next
    );
    spice::cargo_isolation::source_generation_authority(home.home(), lockfile.as_bytes())
        .expect("selecting the development home leaves the occupied lane generation unchanged");
    drop(admitted);
    drop(accepted);
    drop(home);
    writable_directories(&root);
    fs::remove_dir_all(root).unwrap();
}

fn writable_directories(path: &std::path::Path) {
    let metadata = fs::symlink_metadata(path).unwrap();
    if metadata.is_dir() {
        sill::authority::set_owner_mode(path, 0o700).unwrap();
        for entry in fs::read_dir(path).unwrap() {
            writable_directories(&entry.unwrap().path());
        }
    }
}

#[test]
fn cargo_authority_binds_absolute_tools_before_planning() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "spice-prove-cargo-authority-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    let bootstrap = root.join("bootstrap");
    let private_home = root.join("private-home");
    let tools = root.join("tools");
    fs::create_dir_all(&bootstrap).expect("create bootstrap Cargo home");
    fs::create_dir(&private_home).expect("create private Cargo home");
    fs::create_dir(&tools).expect("create native tool directory");
    fs::write(root.join("rust-toolchain.toml"), TOOLCHAIN).expect("write toolchain declaration");
    let cargo = tools.join("cargo");
    let rustc = tools.join("rustc");
    let rustdoc = tools.join("rustdoc");
    for tool in [&cargo, &rustc, &rustdoc] {
        fs::write(tool, "fixture\n").expect("write native tool fixture");
        sill::authority::set_owner_mode(tool, 0o700).expect("make native tool fixture executable");
    }
    let environment = vec![
        (
            OsString::from(spice::cargo_isolation::BOOTSTRAP_HOME_VARIABLE),
            bootstrap.into_os_string(),
        ),
        (
            OsString::from(spice::cargo_isolation::CARGO_HOME_VARIABLE),
            private_home.clone().into_os_string(),
        ),
        (
            OsString::from(spice::cargo_isolation::NATIVE_CARGO_VARIABLE),
            cargo.clone().into_os_string(),
        ),
        (
            OsString::from(spice::cargo_isolation::NATIVE_RUSTC_VARIABLE),
            rustc.into_os_string(),
        ),
        (
            OsString::from(spice::cargo_isolation::NATIVE_RUSTDOC_VARIABLE),
            rustdoc.into_os_string(),
        ),
        (OsString::from("PATH"), OsString::new()),
    ];

    let Ok(authority) = cargo_authority(&root, &environment) else {
        panic!("resolve Cargo authority");
    };
    assert_eq!(authority.cargo, fs::canonicalize(&cargo).unwrap());
    let entries = authority
        .execution_environment
        .iter()
        .cloned()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        entries[OsStr::new(spice::cargo_isolation::CARGO_HOME_VARIABLE)],
        private_home
    );
    assert_eq!(
        entries[OsStr::new(spice::cargo_isolation::NATIVE_CARGO_VARIABLE)],
        authority.cargo
    );
    assert_eq!(entries[OsStr::new("PATH")], OsString::new());
    assert!(
        authority
            .action_environment
            .iter()
            .all(|(name, _)| name != OsStr::new(spice::cargo_isolation::CARGO_HOME_VARIABLE))
    );

    fs::remove_file(&cargo).expect("remove admitted Cargo fixture");
    let error = cargo_authority(&root, &environment)
        .expect_err("stale Cargo authority must refuse before planning");
    assert_eq!(
        error.reason(),
        "cargo-isolation::native-toolchain-unavailable"
    );
    fs::remove_dir_all(&root).expect("remove Cargo authority fixture");
}
