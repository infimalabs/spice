//! Explicit current-source selector retained for the xtask development rim.

use std::collections::BTreeSet;
use std::path::Path;

use super::policy::{matches_any, selected_suffix, selectors};
use super::{
    ChangeSelectionError, ChangeSelectionReport, select_with, with_exhaustive_authority_reach,
};
use crate::GateId;
use wire::Digest;

const RUST: [GateId; 7] = [
    GateId::Formatters,
    GateId::RustTypecheck,
    GateId::AppWebTypecheck,
    GateId::EnvPolicy,
    GateId::Complexity,
    GateId::MagicNumbers,
    GateId::MarkdownLinks,
];
const JAVASCRIPT: [GateId; 5] = [
    GateId::JavascriptUnused,
    GateId::EnvPolicy,
    GateId::EnvNameLedger,
    GateId::Complexity,
    GateId::MagicNumbers,
];
const DOCUMENTATION: [GateId; 2] = [GateId::Taste, GateId::MarkdownLinks];
const PYTHON: [GateId; 10] = [
    GateId::PythonTypecheck,
    GateId::EnvPolicy,
    GateId::EnvNameLedger,
    GateId::Complexity,
    GateId::MagicNumbers,
    GateId::Reachability,
    GateId::SymbolReachability,
    GateId::PythonUnused,
    GateId::AssertionFreeTests,
    GateId::PrivateInternals,
];
const CSHARP: [GateId; 3] = [
    GateId::CsharpUnusedCandidates,
    GateId::Complexity,
    GateId::MagicNumbers,
];
const SHELL: [GateId; 4] = [
    GateId::EnvPolicy,
    GateId::EnvNameLedger,
    GateId::Complexity,
    GateId::MagicNumbers,
];
const RUST_COMPILER_INPUT: [GateId; 2] = [GateId::RustTypecheck, GateId::AppWebTypecheck];

/// Select the richer Spice-source battery from its explicit xtask override.
///
/// This is intentionally separate from [`super::select_change_gates`]: these
/// path names describe this source repository and must never become packaged
/// applicability for an installed appliance.
///
/// # Errors
///
/// Returns the exact staged path and classification failure for any unknown
/// class or source outside the compiled current-source roots. A changed path
/// absent from `tracked_sources` that no class names is a removal and owes the
/// compile arms instead. Known selector authorities conservatively select
/// direct exhaustive execution.
pub fn select_current_source_change_gates(
    source: Digest,
    tracked_sources: &[String],
    changed_sources: &[String],
    generated_paths: &[String],
    rules: &crate::RulePolicy,
) -> Result<ChangeSelectionReport, ChangeSelectionError> {
    let generated_paths = selectors("policy.generated_paths", generated_paths)?;
    let authority_source = changed_sources.iter().find(|path| selector_authority(path));
    let exhaustive = authority_source.is_some();
    let tracked = tracked_sources
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut report = select_with(source, changed_sources, GateId::BATTERY.to_vec(), |path| {
        if exhaustive {
            Ok(GateId::BATTERY.to_vec())
        } else {
            gates_for_path(path, tracked.contains(path), &generated_paths, rules)
        }
    })?;
    if let Some(authority_source) = authority_source {
        report = with_exhaustive_authority_reach(
            report,
            "compiled current-source selection authority changed",
            authority_source,
        )?;
    }
    Ok(report)
}

pub(crate) fn gates_for_path(
    path: &str,
    tracked: bool,
    generated_paths: &[String],
    rules: &crate::RulePolicy,
) -> Result<Vec<GateId>, ChangeSelectionError> {
    if selector_authority(path) {
        return Err(ChangeSelectionError::new(format!(
            "{path:?} changes gate or toolchain selection authority"
        )));
    }

    let repository_document = tracked
        && rules.selects_repository_document(path).map_err(|error| {
            ChangeSelectionError::new(format!(
                "repository-document selection for {path:?}: {error}"
            ))
        })?;
    let mut gates = BTreeSet::from([GateId::LocalPaths]);
    if matches_any(generated_paths, path) {
        if repository_document {
            gates.insert(GateId::RepoDocs);
        }
        return Ok(gates.into_iter().collect());
    }
    if repository_document {
        gates.insert(GateId::RepoDocs);
    } else if selected_suffix(path, rules.document().file_classes().file_shape()) {
        gates.insert(GateId::FileShape);
    }
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    gates.extend(class_gates(path, extension.as_str(), tracked)?);
    Ok(gates.into_iter().collect())
}

fn class_gates(
    path: &str,
    extension: &str,
    tracked: bool,
) -> Result<Vec<GateId>, ChangeSelectionError> {
    match extension {
        "rs" => rust_gates(path),
        "js" | "mjs" | "cjs" => Ok(with_frontend(path, &JAVASCRIPT)),
        "html" | "css" => Ok(with_frontend(path, &[])),
        "md" | "txt" | "rst" => Ok(DOCUMENTATION.to_vec()),
        "py" | "pyi" => Ok(PYTHON.to_vec()),
        "cs" => Ok(CSHARP.to_vec()),
        "sh" | "bash" | "zsh" => Ok(SHELL.to_vec()),
        "toml" | "json" | "yaml" | "yml" | "lock" => configuration_gates(path),
        "" if tracked && extensionless_workspace_input(path) => Ok(RUST_COMPILER_INPUT.to_vec()),
        "" if known_extensionless_shell(path) => Ok(SHELL.to_vec()),
        "" if known_extensionless(path) => Ok(vec![GateId::RepoShape]),
        _ if frontend_path(path) => Ok(vec![GateId::AppWebTypecheck]),
        _ if !tracked => Ok(RUST_COMPILER_INPUT.to_vec()),
        _ => Err(ChangeSelectionError::new(format!(
            "{path:?} has no compiled change-local policy class"
        ))),
    }
}

fn rust_gates(path: &str) -> Result<Vec<GateId>, ChangeSelectionError> {
    if !known_workspace_rust_source(path) {
        return Err(ChangeSelectionError::new(format!(
            "{path:?} is Rust source outside every compiled workspace root"
        )));
    }
    Ok(RUST.to_vec())
}

fn configuration_gates(path: &str) -> Result<Vec<GateId>, ChangeSelectionError> {
    if Path::new(path).file_name().and_then(|name| name.to_str()) == Some("Cargo.toml")
        && !known_workspace_manifest(path)
    {
        return Err(ChangeSelectionError::new(format!(
            "{path:?} is a package manifest outside every compiled workspace root"
        )));
    }
    let mut gates = vec![GateId::RepoShape];
    if path == "pyproject.toml" {
        gates.extend([
            GateId::PythonTypecheck,
            GateId::Reachability,
            GateId::SymbolReachability,
            GateId::PythonUnused,
            GateId::AssertionFreeTests,
            GateId::PrivateInternals,
        ]);
    }
    if path == "pyrightconfig.json" {
        gates.extend([
            GateId::PythonTypecheck,
            GateId::Reachability,
            GateId::SymbolReachability,
            GateId::PythonUnused,
            GateId::AssertionFreeTests,
            GateId::PrivateInternals,
        ]);
    }
    if matches!(
        path,
        "package.json" | "package-lock.json" | "jsconfig.json" | "tsconfig.json"
    ) {
        gates.push(GateId::JavascriptUnused);
    }
    if frontend_path(path) || matches!(path, "package.json" | "package-lock.json") {
        gates.push(GateId::AppWebTypecheck);
    }
    Ok(gates)
}

fn with_frontend(path: &str, base: &[GateId]) -> Vec<GateId> {
    let mut gates = base.to_vec();
    if frontend_path(path) {
        gates.push(GateId::AppWebTypecheck);
    }
    gates
}

pub(crate) fn known_workspace_rust_source(path: &str) -> bool {
    let mut components = path.split('/');
    let Some(root) = components.next() else {
        return false;
    };
    if matches!(root, "src" | "tests" | "examples" | "benches") {
        return true;
    }
    if !known_workspace_package_root(root) {
        return false;
    }
    match components.next() {
        Some("src" | "tests" | "examples" | "benches") => true,
        Some("build.rs") => components.next().is_none(),
        Some("build") => root == "fold",
        _ => false,
    }
}

fn extensionless_workspace_input(path: &str) -> bool {
    let mut components = path.split('/');
    let Some(root) = components.next() else {
        return false;
    };
    components.next().is_some() && known_workspace_package_root(root)
}

fn known_workspace_manifest(path: &str) -> bool {
    let mut components = path.split('/');
    let Some(root) = components.next() else {
        return false;
    };
    components.next() == Some("Cargo.toml")
        && components.next().is_none()
        && known_workspace_package_root(root)
}

fn known_workspace_package_root(root: &str) -> bool {
    matches!(
        root,
        "app"
            | "artifact"
            | "atomic"
            | "board"
            | "bus"
            | "serve"
            | "consent"
            | "dialect"
            | "dist"
            | "fold"
            | "gate"
            | "journal"
            | "lane"
            | "listen"
            | "proof"
            | "sill"
            | "spice"
            | "steer"
            | "tape"
            | "taste"
            | "topic"
            | "vendor"
            | "wire"
            | "windows"
            | "xtask"
    )
}

pub(crate) fn selector_authority(path: &str) -> bool {
    path == wire::TRACKED_POLICY_FILE
        || matches!(
            path,
            ".cargo/config"
                | ".cargo/config.toml"
                | "rust-toolchain"
                | "rust-toolchain.toml"
                | "Cargo.lock"
                | "Cargo.toml"
                | "dist/proof-image/Dockerfile"
                | "gate/src/battery/manifest.toml"
                | "gate/src/change_selection.rs"
                | "gate/src/lib.rs"
                | "gate/src/manifest.rs"
                | "gate/src/test_evidence.rs"
                | "gate/src/workspace_tests.rs"
                | "spice/src/change_gate.rs"
                | "spice/src/cli.rs"
                | "spice/src/cli/contracts.rs"
                | "spice/src/cli/mounts.rs"
                | "spice/src/cli/syntax.rs"
                | "spice/src/gate.rs"
                | "spice/src/gate_command.rs"
                | "spice/src/install/git-hooks/pre-commit"
                | "spice/src/install/git_hooks.rs"
                | "spice/src/lib.rs"
                | "spice/src/main.rs"
                | "xtask/src/gate_command.rs"
                | "xtask/src/lib.rs"
                | "xtask/src/main.rs"
        )
        || path.starts_with("gate/src/change_selection/")
        || path.starts_with("gate/src/test_evidence/")
        || path.starts_with("gate/src/workspace_tests/")
        || path.starts_with("spice/src/gate_command/pre_commit")
}

fn frontend_path(path: &str) -> bool {
    path.starts_with("app/") || path.starts_with("serve/") || path.starts_with("package")
}

fn known_extensionless(path: &str) -> bool {
    matches!(path, ".gitignore" | ".gitattributes" | "LICENSE")
        || path.starts_with("spice/src/install/git-hooks/")
}

fn known_extensionless_shell(path: &str) -> bool {
    path.starts_with("steer/hooks/shell/") || path.starts_with("steer/hooks/wrappers/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensionless_files_inside_workspace_crates_select_rust_consumers() {
        let rules = rules();
        for path in ["dist/version/base", "dist/version/counters"] {
            let sources = [path.to_owned()];
            let selected = select_current_source_change_gates(
                Digest::compute(wire::Domain::GateTree, path.as_bytes()),
                &sources,
                &sources,
                &[],
                &rules,
            )
            .expect("crate build input");
            assert!(
                selected.gate_ids().contains(&GateId::RustTypecheck),
                "{path}"
            );
            assert!(
                selected.gate_ids().contains(&GateId::AppWebTypecheck),
                "{path}"
            );
        }
        assert!(gates_for_path("VERSION", true, &[], &rules).is_err());
        assert!(gates_for_path("docs/NOTICE", true, &[], &rules).is_err());
    }

    #[test]
    fn removed_paths_without_a_class_owe_compile_proof_while_present_ones_refuse() {
        let rules = rules();
        let removed = ["retired/NOTICE".to_owned()];
        let report = select_current_source_change_gates(
            Digest::compute(wire::Domain::GateTree, b"removed input"),
            &[],
            &removed,
            &[],
            &rules,
        )
        .expect("a removed unclassified path is selectable");
        for gate_id in [
            GateId::LocalPaths,
            GateId::RustTypecheck,
            GateId::AppWebTypecheck,
        ] {
            assert!(report.gate_ids().contains(&gate_id));
        }

        assert!(
            select_current_source_change_gates(
                Digest::compute(wire::Domain::GateTree, b"present input"),
                &removed,
                &removed,
                &[],
                &rules,
            )
            .is_err()
        );
    }

    #[test]
    fn authority_changes_select_exhaustively_before_removed_paths_are_classified() {
        let sources = [
            "gate/src/change_selection/current_source.rs".to_owned(),
            "retired/Cargo.toml".to_owned(),
        ];
        let report = select_current_source_change_gates(
            Digest::compute(wire::Domain::GateTree, b"fixture source"),
            &[],
            &sources,
            &[],
            &rules(),
        )
        .expect("authority changes admit every path into exhaustive selection");

        assert_eq!(report.changed_sources(), sources);
        assert_eq!(report.gate_ids(), GateId::BATTERY);
    }

    fn rules() -> crate::RulePolicy {
        crate::repository_rule_policy(None).expect("packaged rule policy")
    }
}
