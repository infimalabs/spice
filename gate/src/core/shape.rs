//! Repository-shape acquisition over one captured selected-tree inventory.
//!
//! Three policy inputs decide what this arm looks at, and each of them has a
//! derivation the repository never has to restate. Package roots fall back to
//! the tree's own packaging metadata, test roots to pytest's own configuration,
//! and the module-name shape to the packaged pattern. A repository that skips
//! this arm therefore skips it because it is not a Python package, not because
//! it declined to say so twice.

mod packaging;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use regex::Regex;
use toml::Value;

use super::acquire::CoreSource;
use super::admission::validate_repo_path;
use super::{CoreEvidence, CoreFinding, CoreInputError, CoreObservation, RepoShapeEvidence};
use crate::input::{ParsedContract, ShapePolicy};
use crate::jurisdiction::{Entry, Shape};
use crate::policy::{PathPattern, compile_stem_expression};
use packaging::derived_package_roots;

const MIN_NAME_CLUSTER_THRESHOLD: usize = 3;
const ALLOWED_FILES: &[&str] = &["__main__.py", "conftest.py", "py.typed"];

/// The module-name shape a repository that declares none is held to.
const PACKAGED_BOUNDARY_PATTERN: &str = "^_*[0-9a-z]+_*$";

/// The test root a repository that configures none is scanned at.
const DEFAULT_TEST_ROOT: &str = "tests";

/// Packaging metadata this arm reads when tracked policy declares no roots.
const PYPROJECT: &str = "pyproject.toml";

pub(super) fn repo_shape(
    source: &CoreSource,
    contract: &ParsedContract,
) -> Result<CoreObservation, CoreInputError> {
    let policy = contract.shape();
    if policy.name_cluster_threshold() < MIN_NAME_CLUSTER_THRESHOLD {
        return Err(CoreInputError::new(format!(
            "policy.name_cluster_threshold must be at least {MIN_NAME_CLUSTER_THRESHOLD}"
        )));
    }

    let entries = source.jurisdiction().entries();
    let tree = TreeIndex::new(entries);
    let pyproject = pyproject(source.root())?;
    let boundary = boundary_pattern(policy)?;
    let generated = compile_patterns("policy.generated_paths", policy.generated_paths())?;
    let tests = test_patterns(policy, &pyproject)?;
    let declared = declared_roots(policy, &pyproject, &tree)?;
    if declared.is_empty() {
        return Ok(CoreObservation::Skipped {
            reason: format!(
                "no Python package roots are declared in tracked policy \
                 or derivable from {PYPROJECT}"
            ),
        });
    }
    let roots = active_roots(&declared, &tree);
    if roots.is_empty() {
        return Ok(CoreObservation::Skipped {
            reason: "no declared Python package root exists in the selected tree".to_owned(),
        });
    }
    if !entries.iter().any(|entry| {
        entry.measurable()
            && entry.path().ends_with(".py")
            && roots.iter().any(|root| beneath(entry.path(), root))
            && !residue(entry.path())
            && !generated_path(entry.path(), &generated)
    }) {
        return Ok(CoreObservation::Skipped {
            reason: "declared Python package roots contain no admitted modules".to_owned(),
        });
    }

    let mut findings = Vec::new();
    namespace_findings(entries, &roots, &generated, &mut findings)?;
    path_shape_findings(
        entries,
        &roots,
        &tests,
        &generated,
        &boundary,
        &mut findings,
    )?;
    name_cluster_findings(
        entries,
        &roots,
        &generated,
        &boundary,
        policy.name_cluster_threshold(),
        &mut findings,
    )?;
    Ok(CoreObservation::Measured {
        evidence: CoreEvidence::RepoShape(RepoShapeEvidence::new(findings)),
    })
}

/// The whole packaging metadata of the captured tree, empty when it has none.
///
/// A file that is present and undecodable refuses. Nothing is what an absent
/// file says, and answering both with a skip would let a broken
/// `pyproject.toml` retire this arm quietly.
///
/// # Errors
///
/// Returns the read or decode failure of a present `pyproject.toml`.
fn pyproject(root: &Path) -> Result<Value, CoreInputError> {
    let path = root.join(PYPROJECT);
    if !path.is_file() {
        return Ok(Value::Table(toml::Table::new()));
    }
    let text = fs::read_to_string(&path)
        .map_err(|error| CoreInputError::new(format!("{PYPROJECT} could not be read: {error}")))?;
    toml::from_str(&text)
        .map_err(|error| CoreInputError::new(format!("{PYPROJECT} is not valid TOML: {error}")))
}

/// Every package root this repository names, tracked or derived.
fn declared_roots(
    policy: &ShapePolicy,
    pyproject: &Value,
    tree: &TreeIndex<'_>,
) -> Result<Vec<String>, CoreInputError> {
    let declared = policy.package_roots();
    let roots = if declared.is_empty() {
        derived_package_roots(pyproject, tree)?
    } else {
        declared.to_vec()
    };
    for root in &roots {
        validate_repo_path(root)?;
    }
    Ok(roots)
}

/// The declared roots that exist as admitted directories in this tree.
fn active_roots(declared: &[String], tree: &TreeIndex<'_>) -> Vec<String> {
    declared
        .iter()
        .filter(|root| tree.is_directory(root))
        .cloned()
        .collect()
}

/// The module-name shape every scanned name is held to.
///
/// Compiled as a whole-name match, which an unanchored tracked pattern would
/// otherwise lose.
fn boundary_pattern(policy: &ShapePolicy) -> Result<Regex, CoreInputError> {
    let field = "policy.package.boundary_underscore_pattern";
    let pattern = policy
        .boundary_underscore_pattern()
        .unwrap_or(PACKAGED_BOUNDARY_PATTERN);
    if pattern.is_empty() {
        return Err(CoreInputError::new(format!("{field} must not be empty")));
    }
    compile_stem_expression(pattern)
        .map_err(|reason| CoreInputError::new(format!("{field}: {reason}")))
}

/// Test-root patterns in fixed derivation precedence.
///
/// Tracked policy wins; pytest's own configuration answers next, because a
/// repository that already told pytest where its tests live has told this arm
/// too; `tests` is the last resort rather than the only answer.
fn test_patterns(
    policy: &ShapePolicy,
    pyproject: &Value,
) -> Result<Vec<PathPattern>, CoreInputError> {
    if !policy.test_paths().is_empty() {
        return compile_patterns("policy.test_paths", policy.test_paths());
    }
    let configured = pytest_testpaths(pyproject);
    if !configured.is_empty() {
        return compile_patterns(
            "pyproject.toml:tool.pytest.ini_options.testpaths",
            &configured,
        );
    }
    compile_patterns("the default test root", &[DEFAULT_TEST_ROOT.to_owned()])
}

/// Test roots pytest is configured with, as a list or a whitespace-split string.
fn pytest_testpaths(pyproject: &Value) -> Vec<String> {
    let Some(configured) = pyproject
        .get("tool")
        .and_then(|tool| tool.get("pytest"))
        .and_then(|pytest| pytest.get("ini_options"))
        .and_then(|options| options.get("testpaths"))
    else {
        return Vec::new();
    };
    match configured {
        Value::String(raw) => raw.split_whitespace().map(str::to_owned).collect(),
        Value::Array(entries) => entries
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// Compile one tracked path list against the shared repository-path contract.
fn compile_patterns(field: &str, values: &[String]) -> Result<Vec<PathPattern>, CoreInputError> {
    values
        .iter()
        .map(|value| {
            PathPattern::tracked(value.trim().trim_end_matches('/'))
                .map_err(|reason| CoreInputError::new(format!("{field} {reason}")))
        })
        .collect()
}

/// Whether one path is exempt as a declared generated path.
///
/// The exemption matches the path itself, exactly as the tracked contract
/// spells it: a literal pattern carries its whole subtree, and a glob pattern
/// carries only what it matches.
fn generated_path(path: &str, generated: &[PathPattern]) -> bool {
    generated.iter().any(|pattern| pattern.matches(path))
}

/// One selected-tree inventory, asked the questions packaging metadata asks.
pub(super) struct TreeIndex<'a> {
    entries: &'a [Entry],
}

impl<'a> TreeIndex<'a> {
    fn new(entries: &'a [Entry]) -> Self {
        Self { entries }
    }

    /// Whether this path is an admitted directory of the selected tree.
    pub(super) fn is_directory(&self, path: &str) -> bool {
        self.entries.iter().any(|entry| {
            entry.path() == path && entry.admission().admits() && entry.shape() == Shape::Directory
        })
    }

    /// Admitted directories directly inside one directory, in sorted order.
    ///
    /// An empty parent, or `.`, names the tree root itself.
    pub(super) fn child_directories(&self, parent: &str) -> Vec<&'a str> {
        let parent = parent.trim_end_matches('/');
        let parent = if parent == "." { "" } else { parent };
        let mut children: Vec<&str> = self
            .entries
            .iter()
            .filter(|entry| entry.admission().admits() && entry.shape() == Shape::Directory)
            .map(Entry::path)
            .filter(|path| self::parent(path) == parent)
            .collect();
        children.sort_unstable();
        children
    }

    /// Whether this path is a directory holding at least one admitted module.
    pub(super) fn holds_module(&self, path: &str) -> bool {
        self.is_directory(path)
            && self.entries.iter().any(|entry| {
                entry.measurable() && entry.path().ends_with(".py") && beneath(entry.path(), path)
            })
    }
}

fn namespace_findings(
    entries: &[Entry],
    roots: &[String],
    generated: &[PathPattern],
    findings: &mut Vec<CoreFinding>,
) -> Result<(), CoreInputError> {
    for entry in entries.iter().filter(|entry| {
        entry.measurable()
            && file_name(entry.path()) == "__init__.py"
            && roots.iter().any(|root| beneath(entry.path(), root))
            && !generated_path(entry.path(), generated)
    }) {
        findings.push(CoreFinding::new(
            "namespace-package",
            Some(entry.path()),
            "__init__.py is forbidden under a declared namespace-package root",
        )?);
    }
    Ok(())
}

fn path_shape_findings(
    entries: &[Entry],
    roots: &[String],
    tests: &[PathPattern],
    generated: &[PathPattern],
    boundary: &Regex,
    findings: &mut Vec<CoreFinding>,
) -> Result<(), CoreInputError> {
    let files: BTreeSet<&str> = entries
        .iter()
        .filter(|entry| entry.measurable())
        .map(Entry::path)
        .collect();
    for entry in entries.iter().filter(|entry| {
        entry.admission().admits()
            && scanned(entry.path(), roots, tests)
            && !residue(entry.path())
            && !generated_path(entry.path(), generated)
    }) {
        let name = file_name(entry.path());
        if entry.shape() == Shape::Directory {
            if !boundary.is_match(name) {
                findings.push(CoreFinding::new(
                    "path-shape",
                    Some(entry.path()),
                    "directory name violates the boundary-underscore module shape",
                )?);
            }
            continue;
        }
        if entry.shape() != Shape::File || !name.ends_with(".py") || ALLOWED_FILES.contains(&name) {
            continue;
        }

        let stem = name.trim_end_matches(".py");
        let module = if test_path(entry.path(), tests) {
            stem.strip_prefix("test_")
        } else {
            Some(stem)
        };
        if !module.is_some_and(|module| boundary.is_match(module)) {
            findings.push(CoreFinding::new(
                "path-shape",
                Some(entry.path()),
                "file name violates the boundary-underscore module shape",
            )?);
        } else if stem.as_bytes().last().is_some_and(u8::is_ascii_digit) {
            findings.push(CoreFinding::new(
                "generic-split",
                Some(entry.path()),
                "generic numbered split; name the seam instead",
            )?);
        } else if sibling_shard(entry.path(), &files) {
            findings.push(CoreFinding::new(
                "continuation-shard",
                Some(entry.path()),
                "continuation shard of a sibling; name the seam instead",
            )?);
        }
    }
    Ok(())
}

fn name_cluster_findings(
    entries: &[Entry],
    roots: &[String],
    generated: &[PathPattern],
    boundary: &Regex,
    threshold: usize,
    findings: &mut Vec<CoreFinding>,
) -> Result<(), CoreInputError> {
    let mut directories: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
    for entry in entries.iter().filter(|entry| {
        entry.measurable()
            && entry.path().ends_with(".py")
            && roots.iter().any(|root| beneath(entry.path(), root))
            && !residue(entry.path())
            && !generated_path(entry.path(), generated)
            && !ALLOWED_FILES.contains(&file_name(entry.path()))
    }) {
        let stem = file_name(entry.path()).trim_end_matches(".py");
        if boundary.is_match(stem) {
            directories
                .entry(parent(entry.path()))
                .or_default()
                .push((stem.trim_matches('_'), entry.path()));
        }
    }

    for (directory, modules) in directories {
        for suffix in [false, true] {
            for (affix, members) in clusters(&modules, suffix, threshold) {
                let names = members
                    .iter()
                    .map(|path| file_name(path))
                    .collect::<Vec<_>>()
                    .join(", ");
                let kind = if suffix { "suffix" } else { "prefix" };
                findings.push(CoreFinding::new(
                    format!("name-cluster-{kind}"),
                    Some(directory),
                    format!(
                        concat!(
                            "{} sibling modules share the {} {:?} ({}); ",
                            "split the shared concept into a namespace subpackage",
                        ),
                        members.len(),
                        kind,
                        affix,
                        names,
                    ),
                )?);
            }
        }
    }
    Ok(())
}

fn clusters<'a>(
    modules: &[(&'a str, &'a str)],
    suffix: bool,
    threshold: usize,
) -> Vec<(String, Vec<&'a str>)> {
    let mut affixes = BTreeSet::new();
    for (index, (left, _)) in modules.iter().enumerate() {
        for (right, _) in &modules[index + 1..] {
            let affix = common_affix(left, right, suffix);
            if affix.len() >= 4 && affix.bytes().all(|byte| byte.is_ascii_alphabetic()) {
                affixes.insert(affix);
            }
        }
    }
    let mut candidates: Vec<_> = affixes
        .into_iter()
        .filter_map(|affix| {
            let members: Vec<_> = modules
                .iter()
                .filter(|(stem, _)| {
                    stem.len() > affix.len()
                        && if suffix {
                            stem.ends_with(&affix)
                        } else {
                            stem.starts_with(&affix)
                        }
                })
                .map(|(_, path)| *path)
                .collect();
            (members.len() >= threshold).then_some((affix, members))
        })
        .collect();
    candidates.sort_by(|left, right| {
        left.0
            .len()
            .cmp(&right.0.len())
            .then_with(|| left.0.cmp(&right.0))
    });

    let mut selected: Vec<(String, Vec<&str>)> = Vec::new();
    for candidate in candidates {
        let member_set: BTreeSet<_> = candidate.1.iter().copied().collect();
        if selected.iter().any(|(_, selected_members)| {
            let selected_set: BTreeSet<_> = selected_members.iter().copied().collect();
            member_set.is_subset(&selected_set)
        }) {
            continue;
        }
        selected.push(candidate);
    }
    selected
}

fn common_affix(left: &str, right: &str, suffix: bool) -> String {
    if suffix {
        let count = left
            .bytes()
            .rev()
            .zip(right.bytes().rev())
            .take_while(|(left, right)| left == right)
            .count();
        left[left.len().saturating_sub(count)..].to_owned()
    } else {
        let count = left
            .bytes()
            .zip(right.bytes())
            .take_while(|(left, right)| left == right)
            .count();
        left[..count].to_owned()
    }
}

fn sibling_shard(path: &str, files: &BTreeSet<&str>) -> bool {
    let name = file_name(path);
    let stem = name.trim_end_matches(".py");
    let Some((last, base)) = stem.as_bytes().split_last() else {
        return false;
    };
    if base.is_empty() || base.ends_with(b"_") {
        return false;
    }
    let base = String::from_utf8_lossy(base);
    let sibling = join(parent(path), &format!("{base}.py"));
    if !files.contains(sibling.as_str()) {
        return false;
    }
    if last.is_ascii_digit() {
        return true;
    }
    if !last.is_ascii_lowercase() {
        return false;
    }
    files
        .iter()
        .filter(|candidate| parent(candidate) == parent(path))
        .filter(|candidate| {
            let candidate = file_name(candidate).trim_end_matches(".py");
            candidate.len() == base.len() + 1
                && candidate.starts_with(base.as_ref())
                && candidate
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_lowercase)
        })
        .count()
        >= 2
}

/// Whether one path lies inside a scanned root.
///
/// Package roots and every configured test root are scanned alike; a root is
/// never a member of itself, so naming a root never indicts the root.
fn scanned(path: &str, roots: &[String], tests: &[PathPattern]) -> bool {
    roots.iter().any(|root| path != root && beneath(path, root))
        || tests.iter().any(|pattern| pattern.matches_ancestor(path))
}

/// Whether one path lies at or inside a configured test root.
fn test_path(path: &str, tests: &[PathPattern]) -> bool {
    tests
        .iter()
        .any(|pattern| pattern.matches_or_ancestor(path))
}

fn beneath(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|remainder| remainder.starts_with('/'))
}

fn residue(path: &str) -> bool {
    path.split('/').any(|part| part == "__pycache__") || path.ends_with(".pyc")
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}
