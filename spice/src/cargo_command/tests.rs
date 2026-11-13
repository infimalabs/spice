use std::ffi::OsStr;

use super::*;
#[test]
fn developer_children_begin_outside_supervised_vendor_state_and_may_opt_in() {
    let mut command = Command::new("cargo");
    command.env(vendor::OMP.thread_id_environment(), "fixture-omp");
    isolate_developer_environment(&mut command);

    for variable in spice::cargo_isolation::DEVELOPER_CHILD_EXCLUDED_VARIABLES {
        assert!(
            command
                .get_envs()
                .any(|(name, value)| name == variable && value.is_none()),
            "developer child does not inherit {variable}"
        );
    }

    command.env(vendor::OMP.thread_id_environment(), "owned-omp");
    assert!(command.get_envs().any(|(name, value)| {
        name == vendor::OMP.thread_id_environment() && value == Some(OsStr::new("owned-omp"))
    }));
}

#[test]
fn only_lock_writing_subcommands_leave_the_admitted_generation() {
    let arguments = |words: &[&str]| words.iter().map(OsString::from).collect::<Vec<_>>();
    for words in [
        &["update", "--package", "objc2-av-foundation"][..],
        &["generate-lockfile"],
        &["add", "objc2-av-foundation"],
        &["remove", "objc2-av-foundation"],
        &["rm", "objc2-av-foundation"],
        &["--verbose", "update"],
        &["+stable", "--color", "never", "update"],
        &["--config", "net.git-fetch-with-cli=true", "update"],
        &["-Z", "unstable-options", "update"],
        &["update", "--locked"],
        &["--frozen", "generate-lockfile"],
    ] {
        assert!(writes_lock(&arguments(words)), "{words:?}");
    }
    for words in [
        &["build", "--locked", "-p", "spice"][..],
        &["test", "--locked", "-p", "app", "--lib", "update"],
        &[
            "run",
            "--locked",
            "--package",
            "xtask",
            "--bin",
            "xtask",
            "--",
            "update",
        ],
        &["--color=never", "fetch"],
        &["--config", "update", "build"],
        &["vendor", "--locked"],
        &[],
        &["--verbose"],
    ] {
        assert!(!writes_lock(&arguments(words)), "{words:?}");
    }
}
