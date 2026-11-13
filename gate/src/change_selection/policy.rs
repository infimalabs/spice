//! Repository-neutral applicability derived from one typed tracked policy.

use std::collections::BTreeSet;
use std::path::Path;

use super::{ALWAYS, ChangeSelectionError, valid_repository_path};
use crate::GateId;

const CONFIGURATION_GATES: [GateId; 3] = [
    GateId::ConfigKeyValidity,
    GateId::ConfigFalseDisable,
    GateId::ConfigTrackedTrust,
];
const PYTHON_HYGIENE: [GateId; 4] = [
    GateId::Reachability,
    GateId::SymbolReachability,
    GateId::PythonUnused,
    GateId::PrivateInternals,
];

/// Borrowed typed declarations used to compile packaged gate applicability.
///
/// An absent tracked configuration has no authored file-class selections. If
/// `configured` is true, all three file-class lists are required and nonempty,
/// matching the Gate policy compiler. Optional package roots broaden Python
/// selection when absent, optional generated paths exclude nothing, and
/// optional test paths conservatively classify every Python path as possibly a
/// test.
pub struct ChangeSelectionPolicyInputs<'a> {
    /// Every regular path in the retained staged tree.
    pub tracked_sources: &'a [String],
    /// Exact repository-relative files whose bytes define selection policy.
    pub policy_authorities: &'a [String],
    /// Whether the staged tree contains a tracked policy document.
    pub configured: bool,
    /// Compiled rule policy whose repository-document predicate the repo-docs
    /// and file-shape arms measure by; selection asks the same predicate.
    pub rules: &'a crate::policy::Policy,
    /// Required file-shape suffixes when configured.
    pub file_shape_suffixes: Option<&'a [String]>,
    /// Required complexity suffixes when configured.
    pub complexity_suffixes: Option<&'a [String]>,
    /// Required magic-number suffixes when configured.
    pub magic_suffixes: Option<&'a [String]>,
    /// Optional repository package roots.
    pub package_roots: &'a [String],
    /// Exact Rust modules reached by Cargo compile-time targets.
    pub declared_rust_sources: &'a [String],
    /// Optional generated-path selectors.
    pub generated_paths: &'a [String],
    /// Optional test-path selectors.
    pub test_paths: &'a [String],
}

/// Validated repository-neutral policy used by packaged change selection.
///
/// Applicability bounds which local arms may be selected; it never authorizes
/// an unclassified staged path to expand into the complete applicable set.
#[derive(Clone, Debug)]
pub struct ChangeSelectionPolicy {
    tracked_sources: Vec<String>,
    policy_authorities: Vec<String>,
    configured: bool,
    repository_documents: Vec<String>,
    file_shape_suffixes: Vec<String>,
    complexity_suffixes: Vec<String>,
    magic_suffixes: Vec<String>,
    package_roots: Vec<String>,
    declared_rust_sources: Vec<String>,
    generated_paths: Vec<String>,
    test_paths: Vec<String>,
}

impl ChangeSelectionPolicy {
    /// Compile exact staged paths and typed declarations into one selector policy.
    ///
    /// # Errors
    ///
    /// Returns an invalid path selector, authority path, tracked path, or
    /// missing/malformed required file-class declaration.
    pub fn new(inputs: ChangeSelectionPolicyInputs<'_>) -> Result<Self, ChangeSelectionError> {
        let tracked_sources = exact_paths("tracked source", inputs.tracked_sources)?;
        let policy_authorities = exact_paths("policy authority", inputs.policy_authorities)?;
        let repository_documents = repository_documents(inputs.rules, &tracked_sources)?;
        let file_shape_suffixes = required_suffixes(
            inputs.configured,
            "policy.file_shape.source_suffixes",
            inputs.file_shape_suffixes,
        )?;
        let complexity_suffixes = required_suffixes(
            inputs.configured,
            "policy.languages.complexity",
            inputs.complexity_suffixes,
        )?;
        let magic_suffixes = required_suffixes(
            inputs.configured,
            "policy.languages.magic",
            inputs.magic_suffixes,
        )?;
        Ok(Self {
            tracked_sources,
            policy_authorities,
            configured: inputs.configured,
            repository_documents,
            file_shape_suffixes,
            complexity_suffixes,
            magic_suffixes,
            package_roots: selectors("policy.package_roots", inputs.package_roots)?,
            declared_rust_sources: exact_paths(
                "declared Cargo Rust source",
                inputs.declared_rust_sources,
            )?,
            generated_paths: selectors("policy.generated_paths", inputs.generated_paths)?,
            test_paths: selectors("policy.test_paths", inputs.test_paths)?,
        })
    }

    pub(super) fn applicable_gates(&self, changed_sources: &[String]) -> Vec<GateId> {
        let mut gates = ALWAYS.into_iter().collect::<BTreeSet<_>>();
        if !self.tracked_sources.is_empty() || !changed_sources.is_empty() {
            gates.insert(GateId::LocalPaths);
        }
        if self.configured
            || changed_sources
                .iter()
                .any(|path| self.policy_authority(path))
        {
            gates.extend(CONFIGURATION_GATES);
        }
        for path in &self.tracked_sources {
            gates.extend(self.gates_for_known_path(path));
        }
        for path in changed_sources {
            gates.extend(self.gates_for_known_path(path));
        }
        gates.into_iter().collect()
    }

    /// Compiled package-root selectors used by language dependency reach.
    #[must_use]
    pub fn package_roots(&self) -> &[String] {
        &self.package_roots
    }

    /// Compiled test-root selectors used by language dependency reach.
    #[must_use]
    pub fn test_paths(&self) -> &[String] {
        &self.test_paths
    }

    pub(super) fn gates_for_path(&self, path: &str) -> Result<Vec<GateId>, ChangeSelectionError> {
        if self.policy_authority(path) {
            return Err(ChangeSelectionError::new(format!(
                "{path:?} changes packaged selection policy"
            )));
        }
        if repository_selection_authority(path) {
            return Err(ChangeSelectionError::new(format!(
                "{path:?} changes repository build or test selection authority"
            )));
        }
        if self.generated(path) {
            let mut gates = BTreeSet::from([GateId::LocalPaths]);
            if self.repository_document(path) {
                gates.insert(GateId::RepoDocs);
            }
            return Ok(gates.into_iter().collect());
        }
        if extension(path) == "rs" && self.rust_workspace() && !self.compiled_rust_source(path) {
            return Err(ChangeSelectionError::new(format!(
                "{path:?} is Rust source outside every compiled workspace root"
            )));
        }
        if cargo_manifest(path) && self.rust_workspace() && !self.workspace_manifest(path) {
            return Err(ChangeSelectionError::new(format!(
                "{path:?} is a package manifest outside every compiled workspace root"
            )));
        }
        let gates = self.gates_for_known_path(path);
        if !gates.iter().any(|gate_id| *gate_id != GateId::LocalPaths) {
            if !contains_exact(&self.tracked_sources, path) {
                return Ok(self.removed_path_gates());
            }
            if !self.configured {
                return Ok(vec![GateId::LocalPaths]);
            }
            return Err(ChangeSelectionError::new(format!(
                "{path:?} has no repository-declared packaged policy class"
            )));
        }
        Ok(gates)
    }

    /// Gates owed by a path the staged tree no longer holds and no class names.
    ///
    /// Removed bytes introduce nothing to measure; what remains owed is proof
    /// that no surviving source still compiles them in, so a Rust workspace
    /// selects both compile arms.
    fn removed_path_gates(&self) -> Vec<GateId> {
        if self.rust_workspace() {
            vec![
                GateId::LocalPaths,
                GateId::RustTypecheck,
                GateId::AppWebTypecheck,
            ]
        } else {
            vec![GateId::LocalPaths]
        }
    }

    fn gates_for_known_path(&self, path: &str) -> Vec<GateId> {
        let mut gates = BTreeSet::new();
        if contains_exact(&self.tracked_sources, path) {
            gates.insert(GateId::LocalPaths);
        }
        if self.generated(path) {
            if self.repository_document(path) {
                gates.insert(GateId::RepoDocs);
            }
            return gates.into_iter().collect();
        }
        self.extend_declared_class_gates(path, &mut gates);
        self.extend_language_authority_gates(path, &mut gates);
        self.extend_extension_gates(path, &mut gates);
        gates.into_iter().collect()
    }

    /// Gates the repository's typed declarations select for `path`, before
    /// its extension is consulted.
    fn extend_declared_class_gates(&self, path: &str, gates: &mut BTreeSet<GateId>) {
        if contains_exact(&self.declared_rust_sources, path)
            || self.extensionless_compiler_input(path)
        {
            gates.extend([GateId::RustTypecheck, GateId::AppWebTypecheck]);
        }
        let repository_document = self.repository_document(path);
        if repository_document {
            gates.insert(GateId::RepoDocs);
        }
        if !repository_document && selected_suffix(path, &self.file_shape_suffixes) {
            gates.insert(GateId::FileShape);
        }
        if selected_suffix(path, &self.complexity_suffixes) {
            gates.insert(GateId::Complexity);
        }
        if selected_suffix(path, &self.magic_suffixes) {
            gates.insert(GateId::MagicNumbers);
        }
    }

    fn repository_document(&self, path: &str) -> bool {
        contains_exact(&self.repository_documents, path)
    }

    /// Gates `path`'s extension selects.
    fn extend_extension_gates(&self, path: &str, gates: &mut BTreeSet<GateId>) {
        let extension = extension(path);
        match extension.as_str() {
            "rs" => {
                if self.rust_workspace() {
                    gates.extend([
                        GateId::Formatters,
                        GateId::RustTypecheck,
                        GateId::AppWebTypecheck,
                    ]);
                }
                gates.insert(GateId::MarkdownLinks);
                gates.extend([GateId::EnvPolicy, GateId::EnvNameLedger]);
            }
            "js" | "mjs" | "cjs" => {
                gates.extend([
                    GateId::JavascriptUnused,
                    GateId::EnvPolicy,
                    GateId::EnvNameLedger,
                ]);
                if frontend_path(path) {
                    gates.insert(GateId::AppWebTypecheck);
                }
            }
            "html" | "css" if frontend_path(path) => {
                gates.insert(GateId::AppWebTypecheck);
            }
            "md" => {
                gates.extend([GateId::Taste, GateId::MarkdownLinks]);
            }
            "txt" | "rst" => {
                gates.insert(GateId::Taste);
            }
            "py" | "pyi" => {
                if self.python_project() {
                    gates.insert(GateId::PythonTypecheck);
                }
                if self.python_source(path) {
                    gates.extend(PYTHON_HYGIENE);
                    if self.python_project() {
                        gates.insert(GateId::RepoShape);
                    }
                }
                if self.test_source(path) {
                    gates.insert(GateId::AssertionFreeTests);
                }
                gates.extend([GateId::EnvPolicy, GateId::EnvNameLedger]);
            }
            "cs" => {
                gates.extend([
                    GateId::CsharpUnusedCandidates,
                    GateId::EnvPolicy,
                    GateId::EnvNameLedger,
                ]);
            }
            "sh" | "bash" | "zsh" | "ts" | "tsx" | "jsx" => {
                gates.extend([GateId::EnvPolicy, GateId::EnvNameLedger]);
            }
            "toml" if cargo_manifest(path) && self.rust_workspace() => {
                gates.extend([
                    GateId::Formatters,
                    GateId::RustTypecheck,
                    GateId::AppWebTypecheck,
                ]);
            }
            "toml" if path == "rust-toolchain.toml" && self.rust_workspace() => {
                gates.extend([
                    GateId::Formatters,
                    GateId::RustTypecheck,
                    GateId::AppWebTypecheck,
                ]);
            }
            "lock" if path == "Cargo.lock" && self.rust_workspace() => {
                gates.extend([GateId::RustTypecheck, GateId::AppWebTypecheck]);
            }
            "" if known_extensionless(path) => {
                gates.insert(GateId::RepoShape);
            }
            _ => {}
        }
    }

    fn policy_authority(&self, path: &str) -> bool {
        contains_exact(&self.policy_authorities, path)
    }

    pub(super) fn selection_authority(&self, path: &str) -> bool {
        self.policy_authority(path) || repository_selection_authority(path)
    }

    fn generated(&self, path: &str) -> bool {
        matches_any(&self.generated_paths, path)
    }

    fn rust_workspace(&self) -> bool {
        contains_exact(&self.tracked_sources, "Cargo.toml")
    }

    fn python_project(&self) -> bool {
        contains_exact(&self.tracked_sources, "pyproject.toml")
    }

    fn javascript_project(&self) -> bool {
        self.tracked_sources
            .iter()
            .any(|path| matches!(extension(path).as_str(), "js" | "mjs" | "cjs"))
    }

    fn extend_language_authority_gates(&self, path: &str, gates: &mut BTreeSet<GateId>) {
        match path {
            "pyproject.toml" | "pyrightconfig.json" if self.python_project() => {
                gates.extend([GateId::RepoShape, GateId::PythonTypecheck]);
                gates.extend(PYTHON_HYGIENE);
                gates.insert(GateId::AssertionFreeTests);
            }
            "package.json" | "package-lock.json" | "jsconfig.json" | "tsconfig.json"
                if self.javascript_project() =>
            {
                gates.insert(GateId::JavascriptUnused);
                if matches!(path, "package.json" | "package-lock.json") {
                    gates.insert(GateId::AppWebTypecheck);
                }
            }
            _ => {}
        }
    }

    fn python_source(&self, path: &str) -> bool {
        self.package_roots.is_empty() || matches_any(&self.package_roots, path)
    }

    fn test_source(&self, path: &str) -> bool {
        self.test_paths.is_empty() || matches_any(&self.test_paths, path)
    }

    fn compiled_rust_source(&self, path: &str) -> bool {
        if contains_exact(&self.declared_rust_sources, path) {
            return true;
        }
        let Some(root) = compiled_package_root(path) else {
            return false;
        };
        let manifest = if root.is_empty() {
            "Cargo.toml".to_owned()
        } else {
            format!("{root}/Cargo.toml")
        };
        contains_exact(&self.tracked_sources, &manifest)
    }

    fn extensionless_compiler_input(&self, path: &str) -> bool {
        if !contains_exact(&self.tracked_sources, path) || !extension(path).is_empty() {
            return false;
        }
        let mut directory = Path::new(path).parent();
        while let Some(candidate) = directory {
            let root = candidate.to_string_lossy();
            if self
                .tracked_sources
                .iter()
                .filter_map(|source| compiled_package_root(source))
                .any(|source_root| source_root == root)
            {
                return true;
            }
            directory = candidate.parent();
        }
        false
    }

    fn workspace_manifest(&self, path: &str) -> bool {
        let Some(root) = path.strip_suffix("/Cargo.toml") else {
            return path == "Cargo.toml";
        };
        let prefix = format!("{root}/");
        self.tracked_sources.iter().any(|source| {
            source
                .strip_prefix(&prefix)
                .is_some_and(compiled_package_source)
        })
    }
}

fn compiled_package_source(path: &str) -> bool {
    compiled_package_root(path).is_some()
}

fn frontend_path(path: &str) -> bool {
    path.starts_with("app/") || path.starts_with("serve/")
}

fn compiled_package_root(path: &str) -> Option<&str> {
    for directory in ["src", "tests", "examples", "benches"] {
        let root_prefix = format!("{directory}/");
        if path.starts_with(&root_prefix) {
            return Some("");
        }
        let nested = format!("/{directory}/");
        if let Some(position) = path.find(&nested) {
            return Some(&path[..position]);
        }
    }
    if path == "build.rs" {
        return Some("");
    }
    path.strip_suffix("/build.rs")
}

fn repository_selection_authority(path: &str) -> bool {
    matches!(
        path,
        ".cargo/config"
            | ".cargo/config.toml"
            | "Cargo.lock"
            | "Cargo.toml"
            | "dist/proof-image/Dockerfile"
            | "rust-toolchain"
            | "rust-toolchain.toml"
    )
}

fn exact_paths(label: &str, paths: &[String]) -> Result<Vec<String>, ChangeSelectionError> {
    let mut normalized = paths.to_vec();
    for path in &normalized {
        if !valid_repository_path(path) {
            return Err(ChangeSelectionError::new(format!(
                "{label} {path:?} is not a normalized repository-relative path"
            )));
        }
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

/// Every staged path the measuring arms treat as a repository document, in
/// the sorted order of `tracked_sources`.
///
/// A removed path stays outside: its bytes are gone, so no cap is owed.
fn repository_documents(
    rules: &crate::policy::Policy,
    tracked_sources: &[String],
) -> Result<Vec<String>, ChangeSelectionError> {
    let mut documents = Vec::new();
    for path in tracked_sources {
        let selected = rules.selects_repository_document(path).map_err(|error| {
            ChangeSelectionError::new(format!(
                "repository-document selection for {path:?}: {error}"
            ))
        })?;
        if selected {
            documents.push(path.clone());
        }
    }
    Ok(documents)
}

pub(super) fn selectors(
    field: &str,
    values: &[String],
) -> Result<Vec<String>, ChangeSelectionError> {
    let mut selectors = values
        .iter()
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    for selector in &selectors {
        crate::validate_tracked_path_selector(selector)
            .map_err(|reason| ChangeSelectionError::new(format!("{field} {reason}")))?;
    }
    selectors.sort();
    selectors.dedup();
    Ok(selectors)
}

fn required_suffixes(
    configured: bool,
    field: &str,
    values: Option<&[String]>,
) -> Result<Vec<String>, ChangeSelectionError> {
    if !configured {
        return Ok(Vec::new());
    }
    let Some(values) = values else {
        return Err(ChangeSelectionError::new(format!(
            "{field} must be declared by the tracked policy document"
        )));
    };
    if values.is_empty() {
        return Err(ChangeSelectionError::new(format!(
            "{field} must contain at least one file suffix"
        )));
    }
    let mut suffixes = values.to_vec();
    for suffix in &suffixes {
        if !suffix.starts_with('.') || suffix.len() == 1 || suffix.contains('/') {
            return Err(ChangeSelectionError::new(format!(
                "{field} contains invalid suffix {suffix:?}"
            )));
        }
    }
    suffixes.sort_by_key(|suffix| suffix.to_ascii_lowercase());
    suffixes.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    Ok(suffixes)
}

pub(super) fn matches_any(selectors: &[String], path: &str) -> bool {
    selectors.iter().any(|selector| {
        crate::tracked_path_selector_matches(selector, path)
            .expect("selection policy retains only compiled path selectors")
    })
}

pub(super) fn selected_suffix(path: &str, suffixes: &[String]) -> bool {
    let Some(extension) = Path::new(path).extension() else {
        return false;
    };
    let extension = format!(".{}", extension.to_string_lossy().to_ascii_lowercase());
    suffixes
        .iter()
        .any(|suffix| suffix.eq_ignore_ascii_case(&extension))
}

fn extension(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn cargo_manifest(path: &str) -> bool {
    Path::new(path).file_name().and_then(|name| name.to_str()) == Some("Cargo.toml")
}

/// Extensionless repository files whose shape the repository gate measures.
fn known_extensionless(path: &str) -> bool {
    matches!(path, ".gitignore" | ".gitattributes" | "LICENSE")
}

fn contains_exact(values: &[String], expected: &str) -> bool {
    values
        .binary_search_by(|candidate| candidate.as_str().cmp(expected))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_compiler_inputs_select_rust_consumers_without_suffix_inference() {
        let tracked = [
            "Cargo.toml".to_owned(),
            "dist/version/base".to_owned(),
            "OTHER".to_owned(),
        ];
        let declared = ["dist/version/base".to_owned()];
        let suffixes = [".rs".to_owned()];
        let policy = ChangeSelectionPolicy::new(ChangeSelectionPolicyInputs {
            tracked_sources: &tracked,
            policy_authorities: &[],
            configured: true,
            rules: &rules(),
            file_shape_suffixes: Some(&suffixes),
            complexity_suffixes: Some(&suffixes),
            magic_suffixes: Some(&suffixes),
            package_roots: &[],
            declared_rust_sources: &declared,
            generated_paths: &[],
            test_paths: &[],
        })
        .expect("selection policy");

        let selected = policy
            .gates_for_path("dist/version/base")
            .expect("declared compiler input");
        assert_eq!(selected.len(), 3);
        for expected in [
            GateId::RustTypecheck,
            GateId::AppWebTypecheck,
            GateId::LocalPaths,
        ] {
            assert!(selected.contains(&expected));
        }
        assert!(policy.gates_for_path("OTHER").is_err());
        assert_eq!(
            policy.gates_for_path(".gitignore").expect("ignore rules"),
            [GateId::RepoShape]
        );
    }

    #[test]
    fn extensionless_files_inside_crate_directories_are_compiler_inputs() {
        let tracked = [
            "Cargo.toml".to_owned(),
            ".gitignore".to_owned(),
            "dist/Cargo.toml".to_owned(),
            "dist/src/lib.rs".to_owned(),
            "dist/version/base".to_owned(),
            "dist/version/counters".to_owned(),
            "docs/NOTICE".to_owned(),
        ];
        let suffixes = [".rs".to_owned()];
        let policy = ChangeSelectionPolicy::new(ChangeSelectionPolicyInputs {
            tracked_sources: &tracked,
            policy_authorities: &[],
            configured: true,
            rules: &rules(),
            file_shape_suffixes: Some(&suffixes),
            complexity_suffixes: Some(&suffixes),
            magic_suffixes: Some(&suffixes),
            package_roots: &[],
            declared_rust_sources: &[],
            generated_paths: &[],
            test_paths: &[],
        })
        .expect("selection policy");

        for path in ["dist/version/base", "dist/version/counters"] {
            let gates = policy.gates_for_path(path).expect("crate build input");
            assert!(gates.contains(&GateId::RustTypecheck), "{path}");
            assert!(gates.contains(&GateId::AppWebTypecheck), "{path}");
        }
        let root = policy.gates_for_path(".gitignore").expect("root ignore");
        assert!(root.contains(&GateId::RepoShape));
        assert!(!root.contains(&GateId::RustTypecheck));
        assert!(!root.contains(&GateId::AppWebTypecheck));
        assert!(policy.gates_for_path("docs/NOTICE").is_err());
    }

    #[test]
    fn removed_paths_without_a_class_owe_compile_proof() {
        let tracked = ["Cargo.toml".to_owned(), "src/lib.rs".to_owned()];
        let suffixes = [".rs".to_owned()];
        let policy = ChangeSelectionPolicy::new(ChangeSelectionPolicyInputs {
            tracked_sources: &tracked,
            policy_authorities: &[],
            configured: true,
            rules: &rules(),
            file_shape_suffixes: Some(&suffixes),
            complexity_suffixes: Some(&suffixes),
            magic_suffixes: Some(&suffixes),
            package_roots: &[],
            declared_rust_sources: &[],
            generated_paths: &[],
            test_paths: &[],
        })
        .expect("selection policy");

        let report = crate::select_change_gates(
            wire::Digest::compute(wire::Domain::GateTree, b"removed input"),
            &["dist/version/base".to_owned()],
            &policy,
        )
        .expect("a removed unclassified path is selectable");

        let mut expected = ALWAYS
            .into_iter()
            .chain([
                GateId::LocalPaths,
                GateId::RustTypecheck,
                GateId::AppWebTypecheck,
            ])
            .collect::<Vec<_>>();
        expected.sort();
        expected.dedup();
        assert_eq!(report.gate_ids(), expected);
    }

    #[test]
    fn generated_markdown_documents_select_repo_docs_instead_of_file_shape() {
        let tracked = [
            "docs/generated.md".to_owned(),
            "docs/guide.md".to_owned(),
            "docs/notes.txt".to_owned(),
            "generated/output.txt".to_owned(),
            "notes.md".to_owned(),
        ];
        let suffixes = [".md".to_owned(), ".txt".to_owned()];
        let generated = [
            "docs/generated.md".to_owned(),
            "generated/output.txt".to_owned(),
            "generated/removed.txt".to_owned(),
        ];
        let rules = rules();
        let policy = ChangeSelectionPolicy::new(ChangeSelectionPolicyInputs {
            tracked_sources: &tracked,
            policy_authorities: &[],
            configured: true,
            rules: &rules,
            file_shape_suffixes: Some(&suffixes),
            complexity_suffixes: Some(&suffixes),
            magic_suffixes: Some(&suffixes),
            package_roots: &[],
            declared_rust_sources: &[],
            generated_paths: &generated,
            test_paths: &[],
        })
        .expect("selection policy");

        for document in ["docs/guide.md", "notes.md"] {
            assert!(
                rules
                    .selects_repository_document(document)
                    .expect("document predicate"),
                "{document}"
            );
            let selected = policy.gates_for_path(document).expect("document");
            assert!(selected.contains(&GateId::RepoDocs), "{document}");
            assert!(!selected.contains(&GateId::FileShape), "{document}");
        }

        let generated_document = "docs/generated.md";
        assert!(
            rules
                .selects_repository_document(generated_document)
                .expect("generated document predicate")
        );
        let selected = policy
            .gates_for_path(generated_document)
            .expect("generated document");
        assert_eq!(selected.len(), 2);
        assert!(selected.contains(&GateId::LocalPaths));
        assert!(selected.contains(&GateId::RepoDocs));

        let text = "docs/notes.txt";
        assert!(!rules.selects_repository_document(text).expect("predicate"));
        let selected = policy.gates_for_path(text).expect("shaped text");
        assert!(selected.contains(&GateId::FileShape));
        assert!(!selected.contains(&GateId::RepoDocs));

        let generated_text = policy
            .gates_for_path("generated/output.txt")
            .expect("generated non-document");
        assert_eq!(generated_text, [GateId::LocalPaths]);
        assert_eq!(
            policy
                .gates_for_path("generated/removed.txt")
                .expect("removed generated path"),
            [GateId::LocalPaths]
        );
    }

    fn rules() -> crate::policy::Policy {
        crate::policy::parse_tracked_policy(
            crate::policy::PACKAGED_POLICY_SOURCE,
            crate::policy::packaged_policy_text(),
        )
        .expect("packaged rule policy")
    }
}
