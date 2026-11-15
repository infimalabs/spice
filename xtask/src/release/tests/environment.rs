use super::*;

fn release_environment(cargo_target_dir: Option<&str>) -> ReleaseEnvironment {
    ReleaseEnvironment {
        path: Some(OsString::from("/tool/bin")),
        home: Some(OsString::from("/home/release")),
        cargo_home: Some(OsString::from("/cargo/home")),
        system_root: Some(OsString::from("/windows")),
        path_ext: Some(OsString::from(".EXE")),
        trunk_offline: Some(OsString::from("true")),
        temp: Some(OsString::from("/temp")),
        tmp: Some(OsString::from("/tmp")),
        cc: Some(OsString::from("/tool/bin/cl")),
        cxx: Some(OsString::from("/tool/bin/cl")),
        include: Some(OsString::from("/include")),
        lib: Some(OsString::from("/lib")),
        cargo_target_dir: cargo_target_dir.map(OsString::from),
        ..ReleaseEnvironment::default()
    }
}

/// The cleared release environment freezes compiler discovery and the admitted
/// toolchain inputs. `programs.rustc` is the workspace-root five-rank answer;
/// `RUSTC` outranks `CARGO_BUILD_RUSTC`, so only the selected answer is copied.
#[test]
fn system_executor_overrides_machine_local_cargo_build_state() {
    let fixture = Fixture::new();
    let release = release_environment(None);
    let programs = fixture.programs();

    let executor =
        SystemExecutor::new(&fixture.root, &release, &programs.rustc).expect("release executor");
    let entries = executor.environment.entries();
    let value = |name: &str| {
        entries
            .iter()
            .find_map(|(key, value)| (key == name).then_some(value.as_os_str()))
            .unwrap_or_else(|| panic!("missing release environment {name}"))
    };

    assert_eq!(
        value("CARGO_BUILD_BUILD_DIR"),
        fixture
            .root
            .join("target/release-appliance-build")
            .as_os_str()
    );
    assert_eq!(value("CARGO_INCREMENTAL"), "0");
    assert_eq!(value("RUSTC"), programs.rustc.as_os_str());
    assert_eq!(value("SystemRoot"), "/windows");
    // Without PATHEXT, Trunk's `which` lookup cannot resolve the admitted
    // `wasm-bindgen.exe` and falls back to downloading it.
    assert_eq!(value("PATHEXT"), ".EXE");
    assert_eq!(value("TRUNK_OFFLINE"), "true");
    assert_eq!([value("TEMP"), value("TMP")], ["/temp", "/tmp"]);
    assert_eq!([value("CC"), value("CXX")], ["/tool/bin/cl"; 2]);
    assert_eq!([value("INCLUDE"), value("LIB")], ["/include", "/lib"]);
    assert!(
        entries.iter().all(|(key, _)| key != "CARGO_BUILD_RUSTC"),
        "the resolved RUSTC already outranks CARGO_BUILD_RUSTC"
    );
    // Pinned to the resolved answer rather than left absent: absence would let
    // the sole app-build child, `xtask app`, pick a build root of its own.
    assert_eq!(
        value("CARGO_TARGET_DIR"),
        fixture.root.join("target").as_os_str()
    );
    assert_eq!(value("RUSTC_WRAPPER"), "");
    assert_eq!(value("RUSTC_WORKSPACE_WRAPPER"), "");
}

/// The workspace default and the acquired value are asserted against
/// different roots on purpose. A sealed image mounts the workspace
/// read-only, so a build root that silently stayed under it would be the
/// one failure this resolver exists to prevent, and two cases that agreed
/// on a path could not tell the two arms apart.
#[test]
fn the_build_root_is_the_acquired_scratch_or_the_workspace_and_never_both() {
    let root = Path::new("/workspace");

    assert_eq!(
        release_environment(None).build_root(root),
        root.join("target"),
        "an unset CARGO_TARGET_DIR must fall back to the workspace",
    );
    assert_eq!(
        release_environment(Some("/cargo-target")).build_root(root),
        Path::new("/cargo-target"),
        "an acquired CARGO_TARGET_DIR must displace the workspace entirely",
    );

    let executor = SystemExecutor::new(
        root,
        &release_environment(Some("/cargo-target")),
        Path::new("/tool/bin/rustc"),
    )
    .expect("release executor");
    let entries = executor.environment.entries();
    let value = |name: &str| {
        entries
            .iter()
            .find_map(|(key, value)| (key == name).then_some(value.as_os_str()))
            .unwrap_or_else(|| panic!("missing release environment {name}"))
    };
    assert_eq!(
        value("CARGO_BUILD_BUILD_DIR"),
        OsStr::new("/cargo-target/release-appliance-build"),
    );
    // The `app-build` step runs this same binary, which acquires its build
    // root from `CARGO_TARGET_DIR` at its own rim. Withholding it here
    // would send the app rebuild back under the workspace no matter what
    // the resolver above decided.
    assert_eq!(value("CARGO_TARGET_DIR"), OsStr::new("/cargo-target"));
}

/// `HOME` is Cargo's own rule for locating a Cargo home, so the fallback
/// has to reproduce it rather than invent a second answer, and an explicit
/// `CARGO_HOME` has to win over it.
#[test]
fn the_cargo_home_prefers_the_acquired_value_and_falls_back_to_cargos_own_rule() {
    assert_eq!(
        release_environment(None).cargo_home(),
        Some(OsString::from("/cargo/home")),
    );

    let inherited = ReleaseEnvironment {
        home: Some(OsString::from("/home/release")),
        ..ReleaseEnvironment::default()
    };
    assert_eq!(
        inherited.cargo_home(),
        Some(OsString::from("/home/release/.cargo")),
    );

    let bare = ReleaseEnvironment::default();
    assert_eq!(
        bare.cargo_home(),
        None,
        "with neither value there is no Cargo home to guess at",
    );
}
