use super::*;

#[test]
fn a_shard_pins_incremental_off_over_an_inherited_setting() {
    let inherited = Environment::new([
        (CARGO_INCREMENTAL, "1"),
        (CARGO_TARGET_DIR, "/ambient/target"),
    ])
    .expect("fixture environment");
    let environment =
        shard_environment(Path::new("/writable"), inherited, &[]).expect("shard environment");
    let entries = environment
        .entries()
        .iter()
        .cloned()
        .collect::<BTreeMap<OsString, OsString>>();
    assert_eq!(
        entries.get(OsStr::new(CARGO_INCREMENTAL)),
        Some(&OsString::from("0"))
    );
    assert_eq!(
        entries.get(OsStr::new(CARGO_BUILD_BUILD_DIR)),
        Some(&OsString::from("/writable/target/cargo-build"))
    );
}

/// `(CARGO_ENCODED_RUSTFLAGS, RUSTFLAGS)` in the environment of a shard
/// appending `appended` under the `inherited` compiler environment.
fn shard_rustflags(
    inherited: &[(&str, &str)],
    appended: &[String],
) -> (Option<OsString>, Option<OsString>) {
    let inherited = Environment::new(inherited.iter().copied()).expect("fixture environment");
    let environment =
        shard_environment(Path::new("/writable"), inherited, appended).expect("shard environment");
    let entries = environment
        .entries()
        .iter()
        .cloned()
        .collect::<BTreeMap<OsString, OsString>>();
    (
        entries.get(OsStr::new(CARGO_ENCODED_RUSTFLAGS)).cloned(),
        entries.get(OsStr::new(RUSTFLAGS)).cloned(),
    )
}

#[test]
fn a_windows_shard_pins_the_warning_deny_over_inherited_rustflags() {
    let deny = WINDOWS_DENY_WARNINGS.map(str::to_owned);
    assert_eq!(
        shard_rustflags(&[], &deny),
        (Some(OsString::from("-D\u{1f}warnings")), None)
    );
    assert_eq!(
        shard_rustflags(
            &[
                (CARGO_ENCODED_RUSTFLAGS, "-C\u{1f}debuginfo=0"),
                (RUSTFLAGS, "-C opt-level=1"),
            ],
            &deny,
        ),
        (
            Some(OsString::from("-C\u{1f}debuginfo=0\u{1f}-D\u{1f}warnings")),
            Some(OsString::from("-C opt-level=1")),
        )
    );
    assert_eq!(
        shard_rustflags(&[(RUSTFLAGS, " -C  debuginfo=0 ")], &deny),
        (
            Some(OsString::from("-C\u{1f}debuginfo=0\u{1f}-D\u{1f}warnings")),
            Some(OsString::from(" -C  debuginfo=0 ")),
        )
    );
}

#[test]
fn a_host_shard_keeps_inherited_rustflags_as_retained() {
    let inherited = [
        (CARGO_ENCODED_RUSTFLAGS, "-C\u{1f}debuginfo=0"),
        (RUSTFLAGS, "-C opt-level=1"),
    ];
    assert_eq!(
        shard_rustflags(&inherited, &[]),
        (
            Some(OsString::from("-C\u{1f}debuginfo=0")),
            Some(OsString::from("-C opt-level=1")),
        )
    );
    assert_eq!(shard_rustflags(&[], &[]), (None, None));
}

fn generated_outputs(plan: &CargoGateShardPlan) -> BTreeSet<String> {
    plan.typechecks()
        .iter()
        .flat_map(|shard| &shard.compile_time_authorities)
        .flat_map(|authority| &authority.generated_outputs)
        .cloned()
        .collect()
}

/// The `cargo metadata` document Cargo reports for the planning fixture.
fn fixture_metadata(root: &Path) -> serde_json::Value {
    let path = |relative: &str| root.join(relative);
    let id = format!("path+file://{}#plan-source-analysis@0.1.0", root.display());
    serde_json::json!({
        "packages": [{
            "id": id,
            "name": "plan-source-analysis",
            "version": "0.1.0",
            "edition": "2024",
            "source": null,
            "dependencies": [],
            "targets": [
                {"kind": ["lib"], "name": "plan_source_analysis", "test": true,
                 "src_path": path("src/lib.rs")},
                {"kind": ["bin"], "name": "plan-source-analysis", "test": true,
                 "src_path": path("src/main.rs")},
                {"kind": ["custom-build"], "name": "build-script-build", "test": false,
                 "src_path": path("build.rs")},
            ],
            "features": {},
            "manifest_path": path("Cargo.toml"),
            "metadata": {"spice": {"cargo-proof": {"version": 1, "complete": true}}},
        }],
        "workspace_members": [id],
        "resolve": {"nodes": [{"id": id, "dependencies": []}]},
        "target_directory": path("target"),
        "build_directory": path("target"),
    })
}

/// The manifest and lock file that metadata describes.
fn write_manifest(fixture: &crate::test_repository::Fixture) {
    fixture.write(
        "Cargo.toml",
        concat!(
            "[package]\n",
            "name = \"plan-source-analysis\"\n",
            "version = \"0.1.0\"\n",
            "edition = \"2024\"\n",
            "build = \"build.rs\"\n\n",
            "[[bin]]\n",
            "name = \"plan-source-analysis\"\n",
            "path = \"src/main.rs\"\n\n",
            "[package.metadata.spice.cargo-proof]\n",
            "version = 1\n",
            "complete = true\n",
        ),
    );
    fixture.write(
        "Cargo.lock",
        concat!(
            "# This file is automatically @generated by Cargo.\n",
            "version = 4\n\n",
            "[[package]]\n",
            "name = \"plan-source-analysis\"\n",
            "version = \"0.1.0\"\n",
        ),
    );
}

fn windows_fixture_metadata(root: &Path) -> serde_json::Value {
    let path = |relative: &str| root.join(relative);
    let spice = format!("path+file://{}/spice#spice@0.0.0", root.display());
    let sill = format!("path+file://{}/sill#sill@0.0.0", root.display());
    let app = format!("path+file://{}/app#app@0.0.0", root.display());
    serde_json::json!({
        "packages": [
            {
                "id": spice,
                "name": "spice",
                "version": "0.0.0",
                "edition": "2024",
                "source": null,
                "dependencies": [],
                "targets": [
                    {"kind": ["lib"], "name": "spice", "test": true,
                     "src_path": path("spice/src/lib.rs")},
                    {"kind": ["bin"], "name": "spice", "test": true,
                     "src_path": path("spice/src/main.rs")},
                ],
                "features": {},
                "manifest_path": path("spice/Cargo.toml"),
                "metadata": {"spice": {"cargo-proof": {"version": 1, "complete": true}}},
            },
            {
                "id": sill,
                "name": "sill",
                "version": "0.0.0",
                "edition": "2024",
                "source": null,
                "dependencies": [],
                "targets": [
                    {"kind": ["lib"], "name": "sill", "test": true,
                     "src_path": path("sill/src/lib.rs")},
                ],
                "features": {},
                "manifest_path": path("sill/Cargo.toml"),
                "metadata": {"spice": {"cargo-proof": {"version": 1, "complete": true}}},
            },
            {
                "id": app,
                "name": "app",
                "version": "0.0.0",
                "edition": "2024",
                "source": null,
                "dependencies": [],
                "targets": [
                    {"kind": ["lib"], "name": "app", "test": true,
                     "src_path": path("app/src/lib.rs")},
                ],
                "features": {},
                "manifest_path": path("app/Cargo.toml"),
                "metadata": {"spice": {"cargo-proof": {"version": 1, "complete": true}}},
            },
        ],
        "workspace_members": [spice, sill, app],
        "resolve": {"nodes": [
            {"id": spice, "dependencies": []},
            {"id": sill, "dependencies": []},
            {"id": app, "dependencies": []},
        ]},
        "target_directory": path("target"),
        "build_directory": path("target"),
    })
}

fn assert_windows_typecheck_contract(shards: &[CargoGateShard]) {
    assert_eq!(shards.len(), 3);
    assert_eq!(
        shards[0].action_id(),
        "cargo:typecheck:spice::windows-x86_64-pc-windows-msvc-bin:spice"
    );
    assert_eq!(
        shards[0].argv(),
        [
            "check",
            "--locked",
            "--package",
            "spice",
            "--bin",
            "spice",
            "--target",
            "x86_64-pc-windows-msvc",
            "--message-format=json-render-diagnostics",
        ]
    );
    assert_eq!(
        shards[1].action_id(),
        "cargo:typecheck:spice::windows-x86_64-pc-windows-msvc-tests"
    );
    assert_eq!(
        shards[1].argv(),
        [
            "check",
            "--locked",
            "--package",
            "spice",
            "--tests",
            "--target",
            "x86_64-pc-windows-msvc",
            "--message-format=json-render-diagnostics",
        ]
    );
    assert_eq!(
        shards[2].action_id(),
        "cargo:typecheck:sill::windows-x86_64-pc-windows-msvc-tests"
    );
    assert_eq!(
        shards[2].argv(),
        [
            "check",
            "--locked",
            "--package",
            "sill",
            "--tests",
            "--target",
            "x86_64-pc-windows-msvc",
            "--message-format=json-render-diagnostics",
        ]
    );
    assert!(shards.iter().all(|shard| {
        shard
            .input_paths()
            .iter()
            .any(|path| path == WINDOWS_TOOLCHAIN_PIN)
    }));
    assert!(shards.iter().all(|shard| {
        shard_rustflags(&[(RUSTFLAGS, "-C debuginfo=0")], &shard.appended_rustflags).0
            == Some(OsString::from("-C\u{1f}debuginfo=0\u{1f}-D\u{1f}warnings"))
    }));
}

#[test]
fn rust_reach_adds_complete_appliance_cross_checks_scoped_to_package_reach() {
    let fixture = crate::test_repository::Fixture::repository("cargo-windows-typecheck");
    fixture.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"app\", \"sill\", \"spice\"]\nresolver = \"3\"\n",
    );
    fixture.write("Cargo.lock", "version = 4\n");
    fixture.write(WINDOWS_TOOLCHAIN_PIN, "version = 1\n");
    for package in ["app", "sill", "spice"] {
        fixture.write(
            &format!("{package}/Cargo.toml"),
            format!("[package]\nname = \"{package}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n"),
        );
        fixture.write(&format!("{package}/src/lib.rs"), "pub fn library() {}\n");
    }
    fixture.write("spice/src/main.rs", "fn main() {}\n");
    let selected_paths = [
        "Cargo.lock",
        "Cargo.toml",
        WINDOWS_TOOLCHAIN_PIN,
        "app/Cargo.toml",
        "app/src/lib.rs",
        "sill/Cargo.toml",
        "sill/src/lib.rs",
        "spice/Cargo.toml",
        "spice/src/lib.rs",
        "spice/src/main.rs",
    ]
    .map(str::to_owned);
    let root = fs::canonicalize(fixture.path()).expect("canonical fixture root");
    let metadata = serde_json::from_value(windows_fixture_metadata(&root))
        .expect("fixture metadata has Cargo's shape");
    let inventory =
        inventory_from_metadata(metadata, &root, true).expect("resolve fixture inventory");
    let plan = |changed: &[&str], paths: &[String]| {
        let changed = changed
            .iter()
            .map(|path| (*path).to_owned())
            .collect::<Vec<_>>();
        let provenance =
            typecheck_provenance(&inventory, &root, &changed).expect("attribute changes");
        windows_typecheck_shards(
            &inventory,
            &root,
            &provenance,
            paths,
            &mut CompileTimeSourceAnalysis::default(),
            None,
        )
        .expect("plan Windows cross-target checks")
    };
    let carries = |shard: &CargoGateShard, wanted: &str| {
        shard.input_paths().iter().any(|path| path == wanted)
    };

    let without_pin = selected_paths
        .iter()
        .filter(|path| path.as_str() != WINDOWS_TOOLCHAIN_PIN)
        .cloned()
        .collect::<Vec<_>>();
    assert!(windows_typechecks_declared_at(&root));
    let shards = plan(&["spice/src/lib.rs"], &without_pin);
    assert_windows_typecheck_contract(&shards);
    assert_eq!(plan(&["spice/src/lib.rs"], &selected_paths), shards);

    let app_only = plan(&["app/src/lib.rs"], &selected_paths);
    assert!(
        app_only.iter().all(|shard| shard
            .input_paths()
            .iter()
            .all(|path| !path.starts_with("app/"))),
        "an edit no Windows-checked package compiles is no Windows input"
    );
    assert_eq!(
        app_only,
        plan(&[], &selected_paths),
        "an app-only edit leaves every Windows receipt key as it was"
    );

    let sill = plan(&["sill/src/lib.rs"], &selected_paths);
    assert!(carries(&sill[2], "sill/src/lib.rs"));
    assert!(!carries(&sill[0], "sill/src/lib.rs"));
    assert!(!carries(&sill[1], "sill/src/lib.rs"));

    for global in ["Cargo.lock", WINDOWS_TOOLCHAIN_PIN] {
        assert!(
            plan(&[global], &selected_paths)
                .iter()
                .all(|shard| carries(shard, global)),
            "{global} is every Windows shard's input"
        );
    }
}

#[test]
fn plan_source_analysis_is_unique_equivalent_and_source_version_bound() {
    let fixture = crate::test_repository::Fixture::repository("cargo-plan-source-analysis");
    write_manifest(&fixture);
    fixture.write(
        "build.rs",
        "fn main() { println!(\"cargo::rerun-if-changed=build.rs\"); }\n",
    );
    fixture.write(
        "src/lib.rs",
        "include!(concat!(env!(\"OUT_DIR\"), \"/generated.rs\"));\n",
    );
    fixture.write("src/main.rs", "fn main() {}\n");
    let selected_paths = [
        "Cargo.lock",
        "Cargo.toml",
        "build.rs",
        "src/lib.rs",
        "src/main.rs",
    ]
    .map(str::to_owned);
    let changed_sources = ["src/lib.rs".to_owned(), "src/main.rs".to_owned()];
    let root = fs::canonicalize(fixture.path()).expect("canonical fixture root");
    let metadata = fixture_metadata(&root);
    let environment = Environment::new(std::iter::empty::<(OsString, OsString)>())
        .expect("empty environment is valid");
    let plan = || {
        plan_cargo_gate_shards(
            CargoGateShardPlanning {
                root: &root,
                base: None,
                source: wire::Digest::compute(wire::Domain::GateTree, b"source-a"),
                changed_sources: &changed_sources,
                selected_paths: &selected_paths,
                environment: environment.clone(),
                cargo: OsStr::new("cargo"),
                external_source: None,
                include_tests: false,
                include_windows_typechecks: false,
            },
            |selected| {
                let metadata = serde_json::from_value(metadata.clone())
                    .expect("fixture metadata has Cargo's shape");
                inventory_from_metadata(metadata, selected, true)
            },
        )
    };

    super::super::compile_time::reset_source_parse_count();
    let first = plan().expect("plan fixture Cargo shards");
    let first_parses = super::super::compile_time::source_parse_count();
    let unique_sources = selected_paths
        .iter()
        .filter(|path| path.ends_with(".rs"))
        .count();
    let repeated_traversal_parses = first.typechecks().len() * unique_sources;

    assert_eq!(first.typechecks().len(), 2);
    assert!(first.tests().is_empty());
    assert_eq!(first_parses, unique_sources);
    assert_eq!(repeated_traversal_parses, 6);

    super::super::compile_time::reset_source_parse_count();
    let equivalent = plan().expect("replan equivalent Cargo shards");
    assert_eq!(
        super::super::compile_time::source_parse_count(),
        unique_sources
    );
    assert_eq!(equivalent, first);

    fixture.write(
        "src/lib.rs",
        "include!(concat!(env!(\"OUT_DIR\"), \"/changed.rs\"));\n",
    );
    super::super::compile_time::reset_source_parse_count();
    let changed = plan().expect("replan changed Cargo source");
    assert_eq!(
        super::super::compile_time::source_parse_count(),
        unique_sources
    );
    assert_eq!(
        generated_outputs(&first),
        BTreeSet::from(["generated.rs".to_owned()])
    );
    assert_eq!(
        generated_outputs(&changed),
        BTreeSet::from(["changed.rs".to_owned()])
    );

    fixture.write("src/lib.rs", "pub fn malformed( {\n");
    let malformed = plan().expect_err("malformed changed source must be refused");
    let malformed = malformed.to_string();
    assert!(
        malformed.contains("parse") && malformed.contains("src/lib.rs"),
        "unexpected malformed-source refusal: {malformed}"
    );
}

#[test]
fn a_shards_executables_spend_one_execution_deadline() {
    let started = Instant::now();
    let deadline = started + SHARD_DEADLINE;
    assert_eq!(shard_remaining(deadline, started), Some(SHARD_DEADLINE));
    let first_used = Duration::from_secs(150);
    assert_eq!(
        shard_remaining(deadline, started + first_used),
        Some(SHARD_DEADLINE - first_used),
        "the next executable gets only what the first left"
    );
    assert_eq!(shard_remaining(deadline, deadline), None);
    assert_eq!(
        shard_remaining(deadline, deadline + Duration::from_secs(1)),
        None,
        "a spent deadline starts no further executable"
    );
}
