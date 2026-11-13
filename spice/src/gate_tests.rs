use super::*;

#[test]
fn staged_renames_retain_both_owners_independent_of_git_rename_configuration() {
    let expected = [
        "README.fixture.md",
        "app/browser/renamed.mjs",
        "app/src/renamed.rs",
        "wire/src/renamed.rs",
    ];
    let changes = parse_selected_changes(
        b"wire/src/renamed.rs\0app/src/renamed.rs\0README.fixture.md\0app/browser/renamed.mjs\0",
    )
    .expect("parse both sides emitted by the no-renames Git boundary");
    assert_eq!(changes, expected);

    let rules = ::gate::repository_rule_policy(None).expect("packaged rule policy");
    let selection = ::gate::select_current_source_change_gates(
        Digest::compute(wire::Domain::GateTree, b"staged rename fixture"),
        &changes,
        &changes,
        &[],
        &rules,
    )
    .expect("classify both sides of staged renames");
    assert_eq!(selection.changed_sources(), expected);
    for gate_id in [
        GateId::RepoDocs,
        GateId::Taste,
        GateId::Formatters,
        GateId::AppWebTypecheck,
        GateId::JavascriptUnused,
    ] {
        assert!(selection.gate_ids().contains(&gate_id));
    }
}

#[test]
fn verifier_cargo_budget_binds_once_and_refuses_drift() {
    let mut environment = vec![
        (OsString::from("Z_FIXTURE"), OsString::from("last")),
        (OsString::from(CARGO_BUILD_JOBS), OsString::from("99")),
        (OsString::from("A_FIXTURE"), OsString::from("first")),
    ];
    let mut bound = None;

    bind_cargo_jobs(&mut environment, &mut bound, 0).expect("zero normalizes to one job");
    assert_eq!(bound, Some(1));
    assert_eq!(
        environment
            .iter()
            .filter(|(name, _)| name == CARGO_BUILD_JOBS)
            .count(),
        1
    );
    assert_eq!(
        environment
            .iter()
            .find(|(name, _)| name == CARGO_BUILD_JOBS)
            .map(|(_, value)| value),
        Some(&OsString::from("1"))
    );
    assert!(environment.windows(2).all(|pair| pair[0] < pair[1]));

    let exact = environment.clone();
    bind_cargo_jobs(&mut environment, &mut bound, 1).expect("same budget is idempotent");
    assert_eq!(environment, exact);

    let error = bind_cargo_jobs(&mut environment, &mut bound, 2)
        .expect_err("a later budget cannot change the evidence environment");
    assert_eq!(error.reason(), "gate::verifier-budget-refused");
    assert_eq!(environment, exact);
}

#[test]
fn compiler_environment_ignores_shell_scratch_and_retains_arbitrary_build_inputs() {
    let prefixed_name = |prefix: &[u8], suffix: &str| {
        let mut name = String::from_utf8_lossy(prefix).into_owned();
        name.push_str(suffix);
        OsString::from(name)
    };
    let producer = compiler_environment_from([
        (OsString::from("RUSTFLAGS"), OsString::from("--cfg=bound")),
        (
            OsString::from(SHELL_LAST_COMMAND),
            OsString::from("/fixture/producer-xtask"),
        ),
        (OsString::from("SHLVL"), OsString::from("1")),
        (
            OsString::from("SSH_AUTH_SOCK"),
            OsString::from("/fixture/producer-agent.sock"),
        ),
        (OsString::from("TERM_SESSION_ID"), OsString::from("w0t0p0")),
        (
            prefixed_name(SHELL_HOOK_PREFIX, "REPO_ROOT"),
            OsString::from("/fixture/repository"),
        ),
        (
            prefixed_name(SHELL_HOOK_PREFIX, "WRAPPERS"),
            OsString::from("producer wrapper functions"),
        ),
        (
            OsString::from("ARBITRARY_BUILD_INPUT"),
            OsString::from("retained"),
        ),
        (
            OsString::from(wire::SpiceVariable::Lane.as_str()),
            OsString::from("producer-lane"),
        ),
        (
            OsString::from(wire::SpiceVariable::NativeCargo.as_str()),
            OsString::from("/toolchain/cargo"),
        ),
        (
            OsString::from(crate::rtk::HISTORY_VARIABLE),
            OsString::from("/fixture/rtk/producer-lane/history.db"),
        ),
    ]);
    let consumer = compiler_environment_from([
        (
            OsString::from(SHELL_LAST_COMMAND),
            OsString::from("/fixture/consumer-spice"),
        ),
        (OsString::from("SHLVL"), OsString::from("2")),
        (OsString::from("TMUX"), OsString::from("/fixture/tmux,1,0")),
        (
            prefixed_name(SHELL_HOOK_PREFIX, "WRAPPERS"),
            OsString::from("consumer wrapper functions"),
        ),
        (
            OsString::from("ARBITRARY_BUILD_INPUT"),
            OsString::from("retained"),
        ),
        (
            OsString::from(wire::SpiceVariable::Lane.as_str()),
            OsString::from("consumer-lane"),
        ),
        (
            OsString::from(wire::SpiceVariable::NativeCargo.as_str()),
            OsString::from("/toolchain/cargo"),
        ),
        (OsString::from("RUSTFLAGS"), OsString::from("--cfg=bound")),
        (
            OsString::from(crate::rtk::HISTORY_VARIABLE),
            OsString::from("/fixture/rtk/consumer-lane/history.db"),
        ),
        (
            OsString::from(crate::agent_launch::OMP_DISABLE_LSPMUX),
            OsString::from("1"),
        ),
    ]);

    assert_eq!(producer, consumer);
    assert_eq!(
        producer,
        [
            (
                OsString::from("ARBITRARY_BUILD_INPUT"),
                OsString::from("retained"),
            ),
            (OsString::from("RUSTFLAGS"), OsString::from("--cfg=bound")),
            (
                OsString::from(wire::SpiceVariable::NativeCargo.as_str()),
                OsString::from("/toolchain/cargo"),
            ),
        ]
    );

    let drifted = compiler_environment_from([
        (
            OsString::from("ARBITRARY_BUILD_INPUT"),
            OsString::from("changed"),
        ),
        (OsString::from("RUSTFLAGS"), OsString::from("--cfg=bound")),
    ]);
    assert_ne!(producer, drifted);
}

#[test]
fn compiler_child_receives_semantic_inputs_but_no_operational_routing() {
    let routing = [
        wire::SpiceVariable::AgentVendor,
        wire::SpiceVariable::Lane,
        wire::SpiceVariable::PreCommitPaths,
        wire::SpiceVariable::PreCommitSource,
        wire::SpiceVariable::RtkExecutable,
        wire::SpiceVariable::Session,
        wire::SpiceVariable::WrapperSpice,
    ];
    let mut input = routing
        .map(|variable| {
            (
                OsString::from(variable.as_str()),
                OsString::from("routing-only"),
            )
        })
        .to_vec();
    input.extend([
        (
            OsString::from("ARBITRARY_BUILD_INPUT"),
            OsString::from("retained"),
        ),
        (
            OsString::from(wire::SpiceVariable::NativeCargo.as_str()),
            OsString::from("/toolchain/cargo"),
        ),
    ]);
    let captured = compiler_environment_from(input);
    for variable in routing {
        assert!(
            captured.iter().all(|(name, _)| name != variable.as_str()),
            "{} reached the compiler child",
            variable.as_str()
        );
    }
    assert!(captured.contains(&(
        OsString::from("ARBITRARY_BUILD_INPUT"),
        OsString::from("retained")
    )));
    assert!(captured.contains(&(
        OsString::from(wire::SpiceVariable::NativeCargo.as_str()),
        OsString::from("/toolchain/cargo")
    )));
}

#[test]
fn cargo_launcher_metadata_normalizes_to_the_installed_compiler_environment() {
    let producer = compiler_environment_from([
        (OsString::from("HOME"), OsString::from("/fixture/home")),
        (
            OsString::from("CARGO_HOME"),
            OsString::from("/fixture/home/.cargo"),
        ),
        (
            OsString::from("RUSTUP_HOME"),
            OsString::from("/fixture/home/.rustup"),
        ),
        (OsString::from("CARGO"), OsString::from("/toolchain/cargo")),
        (
            OsString::from("CARGO_MANIFEST_DIR"),
            OsString::from("/fixture/xtask"),
        ),
        (OsString::from("CARGO_PKG_NAME"), OsString::from("xtask")),
        (
            OsString::from("DYLD_FALLBACK_LIBRARY_PATH"),
            OsString::from("/fixture/target/debug/deps"),
        ),
        (
            OsString::from("RUSTUP_TOOLCHAIN"),
            OsString::from("fixture-toolchain"),
        ),
        (OsString::from("RUST_RECURSION_COUNT"), OsString::from("1")),
        (
            OsString::from("ARBITRARY_BUILD_INPUT"),
            OsString::from("retained"),
        ),
    ]);
    let consumer = compiler_environment_from([
        (OsString::from("HOME"), OsString::from("/fixture/home")),
        (
            OsString::from("ARBITRARY_BUILD_INPUT"),
            OsString::from("retained"),
        ),
    ]);

    assert_eq!(producer, consumer);
    assert!(producer.contains(&(
        OsString::from("CARGO_HOME"),
        OsString::from("/fixture/home/.cargo"),
    )));
    assert!(producer.contains(&(
        OsString::from("RUSTUP_HOME"),
        OsString::from("/fixture/home/.rustup"),
    )));

    let custom = compiler_environment_from([
        (OsString::from("HOME"), OsString::from("/fixture/home")),
        (
            OsString::from("CARGO_HOME"),
            OsString::from("/fixture/custom-cargo"),
        ),
        (
            OsString::from("ARBITRARY_BUILD_INPUT"),
            OsString::from("retained"),
        ),
    ]);
    assert_ne!(
        consumer, custom,
        "a custom Cargo home remains a distinct compiler input"
    );
}

#[test]
fn isolated_execution_rebinds_the_lane_to_the_manual_cargo_home() {
    let bootstrap = crate::cargo_isolation::BOOTSTRAP_HOME_VARIABLE;
    let native = crate::cargo_isolation::NATIVE_CARGO_VARIABLE;
    let producer = compiler_environment_from([
        (OsString::from("HOME"), OsString::from("/fixture/home")),
        (
            OsString::from("CARGO_HOME"),
            OsString::from("/fixture/manual-cargo"),
        ),
        (
            OsString::from(bootstrap),
            OsString::from("/fixture/native-cargo"),
        ),
        (OsString::from(native), OsString::from("/fixture/bin/cargo")),
        (
            OsString::from("ARBITRARY_BUILD_INPUT"),
            OsString::from("retained"),
        ),
    ]);
    let consumer = compiler_environment_from_with_cargo_home(
        [
            (OsString::from("HOME"), OsString::from("/fixture/home")),
            (
                OsString::from("CARGO_HOME"),
                OsString::from("/fixture/lane-cargo"),
            ),
            (
                OsString::from(bootstrap),
                OsString::from("/fixture/native-cargo"),
            ),
            (OsString::from(native), OsString::from("/fixture/bin/cargo")),
            (
                OsString::from("ARBITRARY_BUILD_INPUT"),
                OsString::from("retained"),
            ),
        ],
        Path::new("/fixture/manual-cargo"),
    );

    assert_eq!(producer, consumer);
}

#[test]
fn git_commit_metadata_normalizes_to_the_compiler_environment() {
    let producer = compiler_environment_from([
        (OsString::from("HOME"), OsString::from("/fixture/home")),
        (OsString::from("PS1"), OsString::from("interactive prompt")),
        (
            OsString::from("ARBITRARY_BUILD_INPUT"),
            OsString::from("retained"),
        ),
    ]);
    let consumer = compiler_environment_from([
        (OsString::from("HOME"), OsString::from("/fixture/home")),
        (
            OsString::from("ARBITRARY_BUILD_INPUT"),
            OsString::from("retained"),
        ),
        (
            OsString::from("GIT_AUTHOR_DATE"),
            OsString::from("@1786792967 -0500"),
        ),
        (
            OsString::from("GIT_AUTHOR_EMAIL"),
            OsString::from("gate-report@example.invalid"),
        ),
        (
            OsString::from("GIT_AUTHOR_NAME"),
            OsString::from("Gate Report Fixture"),
        ),
        (OsString::from("GIT_DIR"), OsString::from(".git")),
        (OsString::from("GIT_EDITOR"), OsString::from(":")),
        (
            OsString::from("GIT_EXEC_PATH"),
            OsString::from("/fixture/git/libexec"),
        ),
        (OsString::from("GIT_OPTIONAL_LOCKS"), OsString::from("0")),
        (OsString::from("GIT_PAGER"), OsString::from("cat")),
        (OsString::from("PLINK_PROTOCOL"), OsString::from("ssh")),
        (
            OsString::from("GIT_INDEX_FILE"),
            OsString::from("/fixture/git/index"),
        ),
        (OsString::from("GIT_CONFIG_COUNT"), OsString::from("1")),
        (
            OsString::from("GIT_CONFIG_KEY_0"),
            OsString::from("core.hooksPath"),
        ),
        (
            OsString::from("GIT_CONFIG_VALUE_0"),
            OsString::from("/fixture/git/hooks"),
        ),
        (OsString::from("GIT_PREFIX"), OsString::new()),
    ]);

    assert_eq!(producer, consumer);
    assert!(
        consumer.contains(&(
            OsString::from("ARBITRARY_BUILD_INPUT"),
            OsString::from("retained")
        )),
        "Git routing exclusions must not drop semantic build inputs"
    );
}
