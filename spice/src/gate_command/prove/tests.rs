use std::cell::RefCell;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

#[test]
fn registered_gate_lookup_is_exact() {
    assert_eq!(gate_id("formatters"), Some(GateId::Formatters));
    assert_eq!(gate_id("Formatters"), None);
    assert_eq!(gate_id("formatters "), None);
}

#[test]
fn missing_filters_only_an_action_with_a_current_receipt() {
    assert!(action_needed(false, false));
    assert!(action_needed(false, true));
    assert!(action_needed(true, false));
    assert!(!action_needed(true, true));
}

#[test]
fn a_complete_fingerprint_resolves_without_entering_planning() {
    let authority = ActionFingerprintAuthority::new(ActionFingerprintAuthorityFields {
        candidate_tree: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        baseline: None,
        source: Digest::compute(Domain::GateTree, b"source"),
        policy: Digest::compute(Domain::GatePolicy, b"policy"),
        gate_set: Digest::compute(Domain::GateSet, b"gate set"),
        tools: Digest::compute(Domain::GateTool, b"tools"),
        environment: Digest::compute(Domain::GateConfiguration, b"environment"),
        cargo_source: None,
    })
    .expect("valid fingerprint authority");
    let planned = AtomicUsize::new(0);

    let Ok(resolution) = fingerprint::resolve(
        Some(&authority),
        |_| Ok(Some(Vec::new())),
        || {
            planned.fetch_add(1, Ordering::Relaxed);
            Ok((Vec::new(), Vec::new(), Vec::new()))
        },
    ) else {
        panic!("complete fingerprint must resolve");
    };

    assert!(
        matches!(resolution, fingerprint::ActionResolution::Fingerprint(actions) if actions.is_empty())
    );
    assert_eq!(planned.load(Ordering::Relaxed), 0);

    let Ok(resolution) = fingerprint::resolve(
        Some(&authority),
        |_| Ok(None),
        || {
            planned.fetch_add(1, Ordering::Relaxed);
            Ok((Vec::new(), Vec::new(), Vec::new()))
        },
    ) else {
        panic!("missing fingerprint planning failed");
    };
    let fingerprint::ActionResolution::Planned((present, pending, expected)) = resolution else {
        panic!("missing fingerprint did not enter planning");
    };
    assert!(present.is_empty());
    assert!(pending.is_empty());
    assert!(expected.is_empty());
    assert_eq!(planned.load(Ordering::Relaxed), 1);
}

#[test]
fn cargo_selectors_are_explicit_and_keep_canonical_report_order() {
    assert!(cargo_request(Request {
        action: Some("rust-typecheck"),
        missing: false,
    }));
    assert!(cargo_request(Request {
        action: Some("cargo:test:fixture::lib:fixture"),
        missing: false,
    }));
    assert!(!cargo_request(Request {
        action: Some("formatters"),
        missing: true,
    }));
    assert_eq!(
        proof_action_order("taste", "cargo:typecheck:fixture::lib:fixture"),
        std::cmp::Ordering::Less
    );
    assert_eq!(
        proof_action_order("cargo:typecheck:fixture::lib:fixture", "app-web-typecheck"),
        std::cmp::Ordering::Less
    );
    assert_eq!(
        proof_action_order("private-internals", "cargo:test:fixture::lib:fixture"),
        std::cmp::Ordering::Less
    );
}

#[test]
fn cargo_environment_omits_worktree_outputs_and_matches_forced_wrappers() {
    let normalized = normalized_action_environment(&[
        (OsString::from("PWD"), OsString::from("/worktree-a")),
        (OsString::from("=C:"), OsString::from("C:\\worktree-a")),
        (
            OsString::from("CARGO_TARGET_DIR"),
            OsString::from("/worktree-a/target"),
        ),
        (
            OsString::from("CARGO_BUILD_BUILD_DIR"),
            OsString::from("/worktree-a/build"),
        ),
        (OsString::from("RUSTC_WRAPPER"), OsString::from("sccache")),
        (
            OsString::from(wire::SpiceVariable::Lane.as_str()),
            OsString::from("lane-a"),
        ),
        (
            OsString::from(wire::SpiceVariable::PreCommitSource.as_str()),
            OsString::from("staged-source-a"),
        ),
        (
            OsString::from(wire::SpiceVariable::WrapperSpice.as_str()),
            OsString::from("/worktree-a/.spice-install/bin/spice"),
        ),
        (
            OsString::from("CARGO_HOME"),
            OsString::from("/worktree-a/private-cargo"),
        ),
        (OsString::from("RUSTFLAGS"), OsString::from("--cfg=proof")),
        (
            OsString::from(wire::SpiceVariable::NativeCargo.as_str()),
            OsString::from("/toolchain/cargo"),
        ),
    ]);
    assert_eq!(
        normalized,
        [
            (OsString::from("RUSTC_WORKSPACE_WRAPPER"), OsString::new()),
            (OsString::from("RUSTC_WRAPPER"), OsString::new()),
            (OsString::from("RUSTFLAGS"), OsString::from("--cfg=proof")),
            (
                OsString::from(wire::SpiceVariable::NativeCargo.as_str()),
                OsString::from("/toolchain/cargo"),
            ),
        ]
    );
    assert_eq!(
        normalized,
        normalized_action_environment(&[
            (OsString::from("=C:"), OsString::from("C:\\Git\\bin")),
            (
                OsString::from("=d:"),
                OsString::from("D:\\another-directory")
            ),
            (
                OsString::from("CARGO_HOME"),
                OsString::from("/worktree-b/another-private-cargo"),
            ),
            (
                OsString::from(wire::SpiceVariable::Lane.as_str()),
                OsString::from("lane-b"),
            ),
            (
                OsString::from(wire::SpiceVariable::PreCommitPaths.as_str()),
                OsString::from("[\"different.rs\"]"),
            ),
            (OsString::from("RUSTFLAGS"), OsString::from("--cfg=proof")),
            (
                OsString::from(wire::SpiceVariable::NativeCargo.as_str()),
                OsString::from("/toolchain/cargo"),
            ),
        ]),
        "private locations and Spice routing must not partition shared receipt identity",
    );

    let Ok((_, first)) = environment(&normalized) else {
        panic!("digest first Cargo environment");
    };
    let drifted = normalized_action_environment(&[
        (
            OsString::from("FIXTURE_BUILD_INPUT"),
            OsString::from("changed"),
        ),
        (OsString::from("RUSTFLAGS"), OsString::from("--cfg=proof")),
    ]);
    let Ok((_, second)) = environment(&drifted) else {
        panic!("digest drifted Cargo environment");
    };
    assert_ne!(first, second);
}

#[test]
fn a_repeated_search_path_entry_does_not_partition_environment_identity() {
    let normalized =
        |path| normalized_action_environment(&[(OsString::from("PATH"), search_path(path))]);
    let once = normalized(&["/install/bin", "/toolchain/bin", "/usr/bin"]);
    let twice = normalized(&["/install/bin", "/toolchain/bin", "/install/bin", "/usr/bin"]);

    assert_eq!(once, twice);
    assert_eq!(
        once[0].1,
        search_path(&["/install/bin", "/toolchain/bin", "/usr/bin"])
    );
    assert_ne!(
        once,
        normalized(&["/toolchain/bin", "/install/bin", "/usr/bin"])
    );
}

#[test]
fn windows_git_hook_routing_reconstructs_the_producers_search_path() {
    let root = Path::new("drive/Program Files/Git");
    let custom = PathBuf::from("drive/tools");
    let public = root.join("cmd");
    let system = PathBuf::from("drive/Windows/System32");
    let user_bin = ["drive", "Users", "spice", "bin"]
        .iter()
        .collect::<PathBuf>();
    let direct = vec![custom.clone(), public.clone(), system.clone()];
    let hook = vec![
        root.join("mingw64/bin"),
        root.join("usr/bin"),
        user_bin.clone(),
        custom.clone(),
        public.clone(),
        system.clone(),
    ];
    let public_first_hook = vec![
        public.clone(),
        root.join("mingw64/bin"),
        root.join("usr/bin"),
        user_bin.clone(),
        custom.clone(),
        system.clone(),
    ];
    let fallback_hook = vec![
        root.join("mingw64/bin"),
        root.join("usr/bin"),
        user_bin,
        system.clone(),
    ];
    let normalize = |paths| {
        normalized_windows_git_search_path_with(paths, |path| windows_path_eq(path, &public))
    };

    assert_eq!(normalize(direct), vec![custom.clone(), system.clone()]);
    assert_eq!(normalize(hook), vec![custom.clone(), system.clone()]);
    assert_eq!(normalize(public_first_hook), vec![custom, system.clone()]);
    assert_eq!(normalize(fallback_hook), vec![system]);
}

fn search_path(paths: &[&str]) -> OsString {
    std::env::join_paths(paths.iter().map(Path::new)).expect("fixture PATH composes")
}

#[test]
fn interrupted_execution_retains_prior_publication_and_cleans_up() {
    let events = RefCell::new(Vec::new());
    let result: Result<Vec<u8>, &str> = complete_actions(
        vec![1_u8, 2, 3],
        |action| {
            events.borrow_mut().push(format!("execute-{action}"));
            if action == 2 {
                Err("interrupted")
            } else {
                Ok(action)
            }
        },
        || {
            events.borrow_mut().push("validate".to_owned());
            Ok(())
        },
        || {
            events.borrow_mut().push("cleanup".to_owned());
            Ok(())
        },
        |receipt| {
            events.borrow_mut().push(format!("publish-{receipt}"));
            Ok(receipt)
        },
    );

    assert_eq!(result, Err("interrupted"));
    assert_eq!(
        events.into_inner(),
        ["execute-1", "validate", "publish-1", "execute-2", "cleanup",]
    );
}

#[test]
fn source_validation_precedes_each_opportunistic_publication() {
    let events = RefCell::new(Vec::new());
    let result = complete_actions(
        vec![1_u8, 2],
        |action| {
            events.borrow_mut().push(format!("execute-{action}"));
            Ok::<_, &str>(action)
        },
        || {
            events.borrow_mut().push("validate".to_owned());
            Ok(())
        },
        || {
            events.borrow_mut().push("cleanup".to_owned());
            Ok(())
        },
        |receipt| {
            events.borrow_mut().push(format!("publish-{receipt}"));
            Ok(receipt)
        },
    );

    assert_eq!(result, Ok(vec![1, 2]));
    assert_eq!(
        events.into_inner(),
        [
            "execute-1",
            "validate",
            "publish-1",
            "execute-2",
            "validate",
            "publish-2",
            "cleanup",
        ]
    );
}

#[test]
fn later_publication_failure_retains_the_prior_receipt() {
    let published = RefCell::new(Vec::new());
    let result: Result<Vec<u8>, &str> = complete_actions(
        vec![1_u8, 2],
        Ok::<_, &str>,
        || Ok(()),
        || Ok(()),
        |receipt| {
            if receipt == 2 {
                return Err("later publication failed");
            }
            published.borrow_mut().push(receipt);
            Ok(receipt)
        },
    );

    assert_eq!(result, Err("later publication failed"));
    assert_eq!(published.into_inner(), [1]);
}
