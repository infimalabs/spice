use super::*;

/// Exact workspace packages reached through canonical retained Cargo metadata.
pub(crate) struct WorkspacePackageReach {
    pub(super) checks: Vec<CargoCheckSelection>,
    pub(super) roots: Vec<PathBuf>,
}

/// One package-and-target-addressed native Rust compilation unit.
pub(crate) struct RustCheckUnit {
    identity: String,
    arguments: Vec<String>,
}

impl RustCheckUnit {
    /// Stable package-qualified target identity selected from Cargo metadata.
    pub(crate) fn identity(&self) -> &str {
        &self.identity
    }

    /// Direct Cargo arguments that execute this unit without repository selection.
    pub(crate) fn arguments(&self) -> &[String] {
        &self.arguments
    }
}

impl WorkspacePackageReach {
    pub(crate) fn cargo_check_units(&self) -> Vec<RustCheckUnit> {
        let mut units = Vec::new();
        for check in &self.checks {
            for target in &check.targets {
                units.push(RustCheckUnit {
                    identity: format!("{}::{}", check.name, target.identity),
                    arguments: target.clippy_arguments(&check.name),
                });
            }
        }
        units
    }

    pub(crate) fn reaches_root(&self, root: &Path) -> bool {
        let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        self.roots.contains(&root)
    }
}

pub(crate) fn workspace_package_reach_at(
    root: &Path,
    base: Option<&Path>,
    changed_sources: &[String],
    environment: &Environment,
    cargo: &OsStr,
) -> Result<WorkspacePackageReach, WorkspaceTestError> {
    let mut inventory = acquire_inventory(root, environment, cargo)?;
    if super::cargo_workspace_base_inventory_required(root, changed_sources)? {
        let base = base.ok_or_else(|| {
            WorkspaceTestError::new(
                "removed Rust source requires exact base-tree Cargo ownership inventory",
            )
        })?;
        let base_inventory = acquire_inventory(base, environment, cargo)?;
        inventory.retain_removed_target_ownership(root, &base_inventory, base, changed_sources)?;
    }
    let checks = selection::cargo_check_selections(&inventory, root, changed_sources)?;
    Ok(WorkspacePackageReach {
        roots: checks.iter().map(|check| check.root.clone()).collect(),
        checks,
    })
}

#[derive(Clone, Debug)]
pub(super) struct CargoCheckSelection {
    pub(super) name: String,
    pub(super) root: PathBuf,
    pub(super) targets: Vec<CargoTargetSelection>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct CargoTargetSelection {
    pub(super) identity: String,
    pub(super) flag: &'static str,
    pub(super) name: Option<String>,
    pub(super) required_features: Vec<String>,
    pub(super) test_harness: bool,
}

impl CargoTargetSelection {
    /// `cargo clippy` arguments that compile and lint exactly this target of
    /// `package` with warnings denied. A test-enabled target is linted under
    /// the test profile, so its `cfg(test)` code is compiled and linted too.
    pub(super) fn clippy_arguments(&self, package: &str) -> Vec<String> {
        let mut arguments = vec![
            "clippy".to_owned(),
            "--locked".to_owned(),
            "--package".to_owned(),
            package.to_owned(),
            self.flag.to_owned(),
        ];
        arguments.extend(self.name.clone());
        if let Some(features) = qualified_required_features(
            self.required_features
                .iter()
                .map(|feature| (package, feature.as_str())),
        ) {
            arguments.extend(["--features".to_owned(), features]);
        }
        if self.test_harness {
            arguments.extend(["--profile".to_owned(), "test".to_owned()]);
        }
        arguments.extend(
            [
                "--message-format=json-render-diagnostics",
                "--",
                "-D",
                "warnings",
            ]
            .map(str::to_owned),
        );
        arguments
    }
}

pub(super) fn qualified_required_features<'a>(
    features: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<String> {
    let features = features
        .into_iter()
        .map(|(package, feature)| format!("{package}/{feature}"))
        .collect::<BTreeSet<_>>();
    (!features.is_empty()).then(|| features.into_iter().collect::<Vec<_>>().join(","))
}

#[derive(Deserialize)]
pub(super) struct Metadata {
    pub(super) packages: Vec<MetadataPackage>,
    pub(super) workspace_members: Vec<String>,
    pub(super) resolve: Option<MetadataResolve>,
    #[serde(default)]
    pub(super) target_directory: PathBuf,
    #[serde(default)]
    pub(super) build_directory: PathBuf,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct MetadataPackage {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) edition: String,
    #[serde(default)]
    pub(super) version: String,
    pub(super) manifest_path: PathBuf,
    #[serde(default)]
    pub(super) source: Option<String>,
    #[serde(default)]
    pub(super) dependencies: Vec<MetadataDependency>,
    pub(super) targets: Vec<MetadataTarget>,
    #[serde(default)]
    pub(super) features: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub(super) metadata: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct MetadataResolve {
    pub(super) nodes: Vec<MetadataResolveNode>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct MetadataResolveNode {
    pub(super) id: String,
    #[serde(default)]
    pub(super) dependencies: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct MetadataDependency {
    pub(super) name: String,
    pub(super) rename: Option<String>,
    pub(super) path: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct MetadataTarget {
    pub(super) name: String,
    pub(super) kind: Vec<String>,
    pub(super) test: bool,
    pub(super) src_path: PathBuf,
    #[serde(rename = "required-features", default)]
    pub(super) required_features: Vec<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct BinaryOutput {
    pub(super) package_id: String,
    pub(super) package_name: String,
    pub(super) target_name: String,
}

pub(super) struct MissingBinaryOutput<'a> {
    pub(super) binary: &'a BinaryOutput,
    pub(super) path: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum DeclaredPathSourceOrigin {
    FileMacro,
    IncludedRust,
    Module,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct SourceMacroScope {
    pub(super) scope: super::source_reach::FileMacroScope,
    pub(super) crate_root: bool,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct DeclaredPathSource {
    pub(super) path: PathBuf,
    pub(super) test_only: bool,
    pub(super) dependent_tests: bool,
    pub(super) origins: BTreeSet<DeclaredPathSourceOrigin>,
    pub(super) scopes: BTreeSet<SourceMacroScope>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct TargetIdentity {
    pub(super) package: String,
    pub(super) kind: String,
    pub(super) name: String,
    pub(super) required_features: Vec<String>,
    pub(super) source: PathBuf,
    pub(super) declared_path_sources: Vec<DeclaredPathSource>,
    pub(super) source_bytes: u64,
}

impl TargetIdentity {
    pub(super) fn display(&self) -> String {
        format!("{}:{}", self.kind, self.name)
    }
}

#[derive(Clone, Debug)]
pub(super) struct PackageInventory {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) root: PathBuf,
    pub(super) dependencies: Vec<PathBuf>,
    pub(super) namespace_context: super::source_reach::FileMacroCrateContext,
    pub(super) compile_time_authority: Option<CompileTimeAuthorityDeclaration>,
    pub(super) declared_targets: Vec<TargetIdentity>,
    pub(super) removed_targets: Vec<TargetIdentity>,
    pub(super) targets: Vec<TargetIdentity>,
    /// Directories this package declares as bytes no Cargo target compiles.
    pub(super) data_roots: Vec<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CompileTimeAuthorityDeclaration {
    pub(super) files: Vec<String>,
    pub(super) environment: Vec<String>,
    pub(super) calls: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct CargoResolution {
    pub(super) packages: BTreeMap<String, ResolvedPackage>,
    pub(super) edges: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug)]
pub(super) struct ResolvedPackage {
    pub(super) id: String,
    pub(super) authority_id: String,
    pub(super) name: String,
    pub(super) external: bool,
    pub(super) compile_time_targets: Vec<ResolvedCompileTimeTarget>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct ResolvedCompileTimeTarget {
    pub(super) kind: String,
    pub(super) name: String,
}

#[derive(Clone, Debug)]
pub(super) struct Inventory {
    pub(super) packages: Vec<PackageInventory>,
    pub(super) moved_package_roots: BTreeMap<PathBuf, String>,
    pub(super) resolution: CargoResolution,
    pub(super) binary_outputs: Vec<BinaryOutput>,
    pub(super) target_directory: PathBuf,
    pub(super) build_directory: PathBuf,
}

fn target_shape_matches(
    base_package: &PackageInventory,
    base: &TargetIdentity,
    current_package: &PackageInventory,
    current: &TargetIdentity,
) -> bool {
    let names_match = base.name == current.name
        || (base.name == base_package.name && current.name == current_package.name);
    names_match
        && base.kind == current.kind
        && base.required_features == current.required_features
        && base.source.strip_prefix(&base_package.root).ok()
            == current.source.strip_prefix(&current_package.root).ok()
}

fn moved_dependency_matches(
    base_dependency: &Path,
    current_dependency: &Path,
    base_inventory: &Inventory,
    current_inventory: &Inventory,
    base_root: &Path,
    root: &Path,
    changed_sources: &[String],
) -> bool {
    if base_dependency.strip_prefix(base_root).ok() == current_dependency.strip_prefix(root).ok() {
        return true;
    }
    let Some(base_package) = base_inventory
        .packages
        .iter()
        .find(|package| package.root == base_dependency)
    else {
        return false;
    };
    let Some(current_package) = current_inventory
        .packages
        .iter()
        .find(|package| package.root == current_dependency)
    else {
        return false;
    };
    let Ok(base_relative) = base_package.root.strip_prefix(base_root) else {
        return false;
    };
    let Ok(current_relative) = current_package.root.strip_prefix(root) else {
        return false;
    };
    base_package.name == current_package.name
        && changed_sources
            .iter()
            .any(|path| Path::new(path) == base_relative.join("Cargo.toml"))
        && changed_sources
            .iter()
            .any(|path| Path::new(path) == current_relative.join("Cargo.toml"))
        && base_package.declared_targets.len() == current_package.declared_targets.len()
        && base_package.declared_targets.iter().all(|base_target| {
            current_package
                .declared_targets
                .iter()
                .filter(|current_target| {
                    target_shape_matches(base_package, base_target, current_package, current_target)
                })
                .count()
                == 1
        })
}

fn moved_package_matches(
    base: &PackageInventory,
    current: &PackageInventory,
    base_inventory: &Inventory,
    current_inventory: &Inventory,
    base_root: &Path,
    root: &Path,
    changed_sources: &[String],
) -> bool {
    let Ok(base_relative) = base.root.strip_prefix(base_root) else {
        return false;
    };
    let Ok(current_relative) = current.root.strip_prefix(root) else {
        return false;
    };
    if base.name != current.name
        || base_relative == current_relative
        || !changed_sources
            .iter()
            .any(|path| Path::new(path) == base_relative.join("Cargo.toml"))
        || !changed_sources
            .iter()
            .any(|path| Path::new(path) == current_relative.join("Cargo.toml"))
        || base.dependencies.len() != current.dependencies.len()
        || base.declared_targets.len() != current.declared_targets.len()
    {
        return false;
    }
    let dependencies_match = base.dependencies.iter().all(|base_dependency| {
        current.dependencies.iter().any(|current_dependency| {
            moved_dependency_matches(
                base_dependency,
                current_dependency,
                base_inventory,
                current_inventory,
                base_root,
                root,
                changed_sources,
            )
        })
    });
    dependencies_match
        && base.declared_targets.iter().all(|base_target| {
            current
                .declared_targets
                .iter()
                .filter(|current_target| {
                    target_shape_matches(base, base_target, current, current_target)
                })
                .count()
                == 1
        })
}

fn consolidated_package_matches(
    base: &PackageInventory,
    current: &PackageInventory,
    base_root: &Path,
    root: &Path,
) -> bool {
    let Ok(base_relative) = base.root.strip_prefix(base_root) else {
        return false;
    };
    let Ok(current_relative) = current.root.strip_prefix(root) else {
        return false;
    };
    base_relative != current_relative && base_relative.starts_with(current_relative)
}

impl Inventory {
    pub(super) fn retain_removed_target_ownership(
        &mut self,
        root: &Path,
        base: &Self,
        base_root: &Path,
        changed_sources: &[String],
    ) -> Result<(), WorkspaceTestError> {
        let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let base_root = fs::canonicalize(base_root).unwrap_or_else(|_| base_root.to_path_buf());
        for changed in changed_sources
            .iter()
            .filter(|path| Path::new(path).extension() == Some(OsStr::new("rs")))
        {
            let selected_source = root.join(changed);
            match fs::symlink_metadata(&selected_source) {
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(WorkspaceTestError::new(format!(
                        "changed Rust source {changed:?} is unavailable at {}: {error}",
                        selected_source.display(),
                    )));
                }
            }
            let base_source = base_root.join(changed);
            let mut owners = Vec::new();
            for package in &base.packages {
                for target in &package.targets {
                    if target.source == base_source {
                        owners.push((package, target));
                    }
                }
            }
            let [(base_package, base_target)] = owners.as_slice() else {
                if owners.is_empty() {
                    continue;
                }
                return Err(WorkspaceTestError::new(format!(
                    "removed Rust source {changed:?} has ambiguous base-tree Cargo target ownership",
                )));
            };
            let relative_package = base_package.root.strip_prefix(&base_root).map_err(|_| {
                WorkspaceTestError::new(format!(
                    "base-tree Cargo package {:?} root {} escapes base root {}",
                    base_package.name,
                    base_package.root.display(),
                    base_root.display(),
                ))
            })?;
            let current_index = self.packages.iter().position(|package| {
                package.name == base_package.name && package.root == root.join(relative_package)
            });
            let Some(current_index) = current_index else {
                let mut successors = self.packages.iter().filter_map(|package| {
                    let consolidated =
                        consolidated_package_matches(base_package, package, &base_root, &root);
                    (consolidated
                        || moved_package_matches(
                            base_package,
                            package,
                            base,
                            self,
                            &base_root,
                            &root,
                            changed_sources,
                        ))
                    .then_some((package, consolidated))
                });
                let Some((successor, consolidated)) = successors.next() else {
                    return Err(WorkspaceTestError::new(format!(
                        "removed Rust source {changed:?} belonged to base-tree Cargo package {:?}, which has no matching current package or moved successor",
                        base_package.name,
                    )));
                };
                if let Some((other, _)) = successors.next() {
                    return Err(WorkspaceTestError::new(format!(
                        "removed Rust source {changed:?} has ambiguous moved package successors {:?} and {:?}",
                        successor.name, other.name,
                    )));
                }
                if !consolidated
                    && !successor.declared_targets.iter().any(|target| {
                        target_shape_matches(base_package, base_target, successor, target)
                    })
                {
                    return Err(WorkspaceTestError::new(format!(
                        "removed Rust source {changed:?} has no target in moved package successor {:?}",
                        successor.name,
                    )));
                }
                let successor_id = successor.id.clone();
                let moved_root = root.join(relative_package);
                if let Some(owner) = self.moved_package_roots.get(&moved_root)
                    && owner != &successor_id
                {
                    return Err(WorkspaceTestError::new(format!(
                        "moved package root {} has conflicting successors {owner:?} and {successor_id:?}",
                        moved_root.display(),
                    )));
                }
                self.moved_package_roots.insert(moved_root, successor_id);
                continue;
            };
            let current_package = &mut self.packages[current_index];
            let mut target = (*base_target).clone();
            target.package.clone_from(&current_package.id);
            target.source = rebase_path(&target.source, &base_root, &root, changed)?;
            for declared in &mut target.declared_path_sources {
                declared.path = rebase_path(&declared.path, &base_root, &root, changed)?;
            }
            current_package.removed_targets.push(target);
            current_package.removed_targets.sort();
            current_package.removed_targets.dedup();
        }
        Ok(())
    }

    pub(super) fn execution_environment(
        &self,
        environment: Environment,
        selected_root: &Path,
        writable_root: &Path,
        allow_configured_external: bool,
    ) -> Result<Environment, WorkspaceTestError> {
        let selected_root =
            fs::canonicalize(selected_root).unwrap_or_else(|_| selected_root.to_path_buf());
        let writable_root =
            fs::canonicalize(writable_root).unwrap_or_else(|_| writable_root.to_path_buf());
        let target = writable_directory(
            &self.target_directory,
            &selected_root,
            &writable_root.join("target"),
            allow_configured_external,
        );
        let build = writable_directory(
            &self.build_directory,
            &selected_root,
            &writable_root.join("target/cargo-build"),
            allow_configured_external,
        );
        let mut entries = environment
            .entries()
            .iter()
            .cloned()
            .collect::<BTreeMap<OsString, OsString>>();
        entries.insert(OsString::from(CARGO_TARGET_DIR), target.into_os_string());
        entries.insert(
            OsString::from(CARGO_BUILD_BUILD_DIR),
            build.into_os_string(),
        );
        entries.insert(
            OsString::from(TEST_WRITABLE_ROOT),
            writable_root.into_os_string(),
        );
        Environment::new(entries).map_err(|error| WorkspaceTestError::new(error.to_string()))
    }

    pub(super) fn selected(&self, tests: &[String]) -> Result<Self, WorkspaceTestError> {
        let selected = tests.iter().cloned().collect::<BTreeSet<_>>();
        let mut packages = self
            .packages
            .iter()
            .filter_map(|package| {
                let targets = package
                    .targets
                    .iter()
                    .filter(|target| {
                        selected.contains(&format!("{}::{}", package.name, target.display()))
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                (!targets.is_empty()).then(|| PackageInventory {
                    id: package.id.clone(),
                    name: package.name.clone(),
                    root: package.root.clone(),
                    dependencies: package.dependencies.clone(),
                    namespace_context: package.namespace_context.clone(),
                    compile_time_authority: package.compile_time_authority.clone(),
                    declared_targets: package.declared_targets.clone(),
                    removed_targets: package.removed_targets.clone(),
                    targets,
                    data_roots: package.data_roots.clone(),
                })
            })
            .collect::<Vec<_>>();
        packages.sort_by(|left, right| (&left.name, &left.id).cmp(&(&right.name, &right.id)));
        let observed = packages
            .iter()
            .flat_map(|package| {
                package
                    .targets
                    .iter()
                    .map(|target| format!("{}::{}", package.name, target.display()))
            })
            .collect::<BTreeSet<_>>();
        if observed != selected {
            return Err(WorkspaceTestError::new(format!(
                "selected test inventory drifted: expected {selected:?}, observed {observed:?}"
            )));
        }
        let selected_packages = packages
            .iter()
            .map(|package| package.id.as_str())
            .collect::<BTreeSet<_>>();
        let binary_outputs = self
            .binary_outputs
            .iter()
            .filter(|binary| selected_packages.contains(binary.package_id.as_str()))
            .cloned()
            .collect();
        Ok(Self {
            packages,
            resolution: self.resolution.clone(),
            binary_outputs,
            moved_package_roots: self.moved_package_roots.clone(),
            target_directory: self.target_directory.clone(),
            build_directory: self.build_directory.clone(),
        })
    }
}

fn rebase_path(
    path: &Path,
    base_root: &Path,
    root: &Path,
    changed: &str,
) -> Result<PathBuf, WorkspaceTestError> {
    let relative = path.strip_prefix(base_root).map_err(|_| {
        WorkspaceTestError::new(format!(
            "base-tree Cargo ownership for {changed:?} names path {} outside base root {}",
            path.display(),
            base_root.display(),
        ))
    })?;
    Ok(root.join(relative))
}

fn writable_directory(
    configured: &Path,
    selected_root: &Path,
    local: &Path,
    allow_configured_external: bool,
) -> PathBuf {
    if configured.as_os_str().is_empty()
        || configured.starts_with(selected_root)
        || !allow_configured_external
    {
        local.to_path_buf()
    } else {
        configured.to_path_buf()
    }
}

pub(super) fn acquire_inventory(
    root: &Path,
    environment: &Environment,
    cargo: &OsStr,
) -> Result<Inventory, WorkspaceTestError> {
    acquire_inventory_with_resolution(root, environment, cargo, false)
}

/// Resolve regular compiler inputs declared by Cargo targets.
///
/// # Errors
///
/// Returns metadata, declared-source, escaping-path, non-Unicode path, or
/// ambiguous compile-time target ownership failures.
pub fn cargo_declared_compile_time_sources_at(
    root: &Path,
    environment: &Environment,
) -> Result<Vec<String>, WorkspaceTestError> {
    if !root.join("Cargo.toml").is_file() {
        return Ok(Vec::new());
    }
    let environment = without_pre_commit_dispatch(environment.clone())?;
    let inventory = acquire_inventory(root, &environment, OsStr::new("cargo"))?;
    declared_compile_time_sources(&inventory, root)
}

pub(super) fn declared_compile_time_sources(
    inventory: &Inventory,
    root: &Path,
) -> Result<Vec<String>, WorkspaceTestError> {
    let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut sources = BTreeSet::new();
    for package in &inventory.packages {
        for target in &package.declared_targets {
            let special_target = target
                .kind
                .split('+')
                .any(|kind| matches!(kind, "custom-build" | "proc-macro"));
            for declared in &target.declared_path_sources {
                let file_input = declared.origins.iter().any(|origin| {
                    matches!(
                        origin,
                        DeclaredPathSourceOrigin::FileMacro
                            | DeclaredPathSourceOrigin::IncludedRust
                    )
                });
                let special_module = special_target
                    && declared.origins.contains(&DeclaredPathSourceOrigin::Module)
                    && declared.path.extension() == Some(OsStr::new("rs"));
                if !file_input && !special_module {
                    continue;
                }
                match fs::symlink_metadata(&declared.path) {
                    Ok(metadata) if metadata.file_type().is_file() => {}
                    Ok(_) => continue,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        return Err(WorkspaceTestError::new(format!(
                            "inspect declared compile-time source {}: {error}",
                            declared.path.display(),
                        )));
                    }
                }
                let relative = declared.path.strip_prefix(&root).map_err(|_| {
                    WorkspaceTestError::new(format!(
                        "declared compile-time source {} escapes selected root {}",
                        declared.path.display(),
                        root.display(),
                    ))
                })?;
                let source = relative.to_str().ok_or_else(|| {
                    WorkspaceTestError::new(format!(
                        "declared compile-time source {} is not Unicode",
                        relative.display(),
                    ))
                })?;
                sources.insert(source.replace(std::path::MAIN_SEPARATOR, "/"));
            }
        }
    }
    Ok(sources.into_iter().collect())
}

pub(super) fn acquire_resolved_inventory(
    root: &Path,
    environment: &Environment,
    cargo: &OsStr,
) -> Result<Inventory, WorkspaceTestError> {
    acquire_inventory_with_resolution(root, environment, cargo, true)
}

fn acquire_inventory_with_resolution(
    root: &Path,
    environment: &Environment,
    cargo: &OsStr,
    require_resolution: bool,
) -> Result<Inventory, WorkspaceTestError> {
    let mut arguments = vec!["metadata", "--locked", "--format-version", "1"];
    if !require_resolution {
        arguments.push("--no-deps");
    }
    let spec = process_spec(
        cargo,
        arguments,
        root,
        environment.clone(),
        METADATA_DEADLINE,
        Cancellation::Never,
        "workspace-test metadata",
    )?;
    let outcome = crate::measure::spawn(spec)
        .map_err(|error| WorkspaceTestError::new(format!("metadata launch failed: {error}")))?
        .wait()
        .map_err(|error| WorkspaceTestError::new(format!("metadata wait failed: {error}")))?;
    metadata_from_outcome(outcome, root, require_resolution)
}

fn metadata_from_outcome(
    outcome: WaitOutcome,
    root: &Path,
    require_resolution: bool,
) -> Result<Inventory, WorkspaceTestError> {
    match outcome.completion() {
        Completion::Exited(status) if status.success() => {}
        completion => {
            return Err(WorkspaceTestError::new(format!(
                "cargo metadata ended as {}: {}",
                completion_name(completion),
                captured_excerpt(outcome.stderr())
            )));
        }
    }
    let stdout = outcome
        .stdout()
        .ok_or_else(|| WorkspaceTestError::new("cargo metadata stdout was not captured"))?;
    if stdout.truncated() || !stdout.complete() || !outcome.outputs_drained() {
        return Err(WorkspaceTestError::new(
            "cargo metadata output exceeded or escaped its capture bound",
        ));
    }
    let metadata: Metadata = serde_json::from_slice(stdout.bytes())
        .map_err(|error| WorkspaceTestError::new(format!("cargo metadata was invalid: {error}")))?;
    inventory_from_metadata(metadata, root, require_resolution)
}

pub(super) fn inventory_from_metadata(
    metadata: Metadata,
    root: &Path,
    require_resolution: bool,
) -> Result<Inventory, WorkspaceTestError> {
    let target_directory = metadata.target_directory.clone();
    let build_directory = metadata.build_directory.clone();
    let resolution = if require_resolution {
        resolved_inventory(&metadata, root)?
    } else {
        CargoResolution::default()
    };
    let members = metadata
        .workspace_members
        .into_iter()
        .collect::<BTreeSet<_>>();
    let mut binary_outputs = metadata
        .packages
        .iter()
        .filter(|package| members.contains(&package.id))
        .flat_map(package_binary_outputs)
        .collect::<Vec<_>>();
    binary_outputs.sort();
    let mut packages = metadata
        .packages
        .into_iter()
        .filter(|package| members.contains(&package.id))
        .map(|package| package_inventory(package, root))
        .collect::<Result<Vec<_>, _>>()?;
    packages.sort_by(|left, right| (&left.name, &left.id).cmp(&(&right.name, &right.id)));
    if packages.is_empty() {
        return Err(WorkspaceTestError::new(
            "cargo metadata named no workspace packages",
        ));
    }
    validate_unique_workspace_packages(&packages)?;
    validate_workspace_dependencies(&packages, root)?;
    Ok(Inventory {
        packages,
        resolution,
        binary_outputs,
        moved_package_roots: BTreeMap::new(),
        target_directory,
        build_directory,
    })
}

fn resolved_inventory(
    metadata: &Metadata,
    root: &Path,
) -> Result<CargoResolution, WorkspaceTestError> {
    let selected_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let resolve = metadata.resolve.as_ref().ok_or_else(|| {
        WorkspaceTestError::new("cargo metadata omitted the resolved dependency graph")
    })?;
    let mut packages = BTreeMap::new();
    for package in &metadata.packages {
        if package.version.is_empty() {
            return Err(WorkspaceTestError::new(format!(
                "Cargo package {:?} metadata omitted its version",
                package.name,
            )));
        }
        if !package.manifest_path.is_absolute() {
            return Err(WorkspaceTestError::new(format!(
                "Cargo package {:?} metadata named relative manifest {}",
                package.name,
                package.manifest_path.display(),
            )));
        }
        let manifest = fs::canonicalize(&package.manifest_path)
            .unwrap_or_else(|_| package.manifest_path.clone());
        let package_root = manifest.parent().ok_or_else(|| {
            WorkspaceTestError::new(format!(
                "Cargo package {:?} has no manifest parent",
                package.name,
            ))
        })?;
        let authority_id = if package_root.starts_with(&selected_root) {
            let relative = manifest.strip_prefix(&selected_root).map_err(|_| {
                WorkspaceTestError::new(format!(
                    "Cargo package {:?} manifest {} escapes selected root {}",
                    package.name,
                    manifest.display(),
                    selected_root.display(),
                ))
            })?;
            format!(
                "path:{}#{}@{}",
                relative
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/"),
                package.name,
                package.version,
            )
        } else if let Some(source) = &package.source {
            format!("{source}#{}@{}", package.name, package.version)
        } else {
            format!("external-path#{}@{}", package.name, package.version)
        };
        let mut compile_time_targets = package
            .targets
            .iter()
            .flat_map(|target| {
                target
                    .kind
                    .iter()
                    .filter(|kind| matches!(kind.as_str(), "custom-build" | "proc-macro"))
                    .map(|kind| ResolvedCompileTimeTarget {
                        kind: kind.clone(),
                        name: target.name.clone(),
                    })
            })
            .collect::<Vec<_>>();
        compile_time_targets.sort();
        compile_time_targets.dedup();
        let resolved = ResolvedPackage {
            id: package.id.clone(),
            authority_id,
            name: package.name.clone(),
            external: package.source.is_some() || !package_root.starts_with(&selected_root),
            compile_time_targets,
        };
        if packages.insert(package.id.clone(), resolved).is_some() {
            return Err(WorkspaceTestError::new(format!(
                "cargo metadata repeats resolved package id {:?}",
                package.id,
            )));
        }
    }
    let mut edges = BTreeMap::new();
    for node in &resolve.nodes {
        let mut dependencies = node.dependencies.clone();
        dependencies.sort();
        dependencies.dedup();
        if edges.insert(node.id.clone(), dependencies).is_some() {
            return Err(WorkspaceTestError::new(format!(
                "cargo metadata repeats resolve node {:?}",
                node.id,
            )));
        }
    }
    for (id, dependencies) in &edges {
        if !packages.contains_key(id) {
            return Err(WorkspaceTestError::new(format!(
                "cargo resolve node {id:?} has no package identity",
            )));
        }
        if let Some(missing) = dependencies
            .iter()
            .find(|dependency| !packages.contains_key(*dependency))
        {
            return Err(WorkspaceTestError::new(format!(
                "cargo resolve node {id:?} names missing dependency {missing:?}",
            )));
        }
    }
    Ok(CargoResolution { packages, edges })
}

fn package_binary_outputs(package: &MetadataPackage) -> Vec<BinaryOutput> {
    let enabled = default_feature_closure(&package.features);
    package
        .targets
        .iter()
        .filter(|target| target.kind.iter().any(|kind| kind == "bin"))
        .filter(|target| {
            target
                .required_features
                .iter()
                .all(|feature| enabled.contains(feature))
        })
        .map(|target| BinaryOutput {
            package_id: package.id.clone(),
            package_name: package.name.clone(),
            target_name: target.name.clone(),
        })
        .collect()
}

fn default_feature_closure(features: &BTreeMap<String, Vec<String>>) -> BTreeSet<String> {
    let mut enabled = BTreeSet::new();
    let mut pending = features
        .contains_key("default")
        .then(|| "default".to_owned())
        .into_iter()
        .collect::<VecDeque<_>>();
    while let Some(feature) = pending.pop_front() {
        if !enabled.insert(feature.clone()) {
            continue;
        }
        for member in features.get(&feature).into_iter().flatten() {
            if features.contains_key(member) && !enabled.contains(member) {
                pending.push_back(member.clone());
            }
        }
    }
    enabled
}

fn validate_unique_workspace_packages(
    packages: &[PackageInventory],
) -> Result<(), WorkspaceTestError> {
    let mut names = BTreeSet::new();
    let mut roots = BTreeMap::new();
    for package in packages {
        if !names.insert(package.name.as_str()) {
            return Err(WorkspaceTestError::new(format!(
                "workspace package name {:?} is ambiguous for Cargo --package",
                package.name
            )));
        }
        if let Some(previous) = roots.insert(package.root.as_path(), package.name.as_str()) {
            return Err(WorkspaceTestError::new(format!(
                "workspace packages {previous:?} and {:?} share root {}",
                package.name,
                package.root.display(),
            )));
        }
    }
    Ok(())
}

fn validate_workspace_dependencies(
    packages: &[PackageInventory],
    root: &Path,
) -> Result<(), WorkspaceTestError> {
    let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    for package in packages {
        for dependency in &package.dependencies {
            if dependency.starts_with(&root)
                && !packages
                    .iter()
                    .any(|candidate| candidate.root == *dependency)
            {
                return Err(WorkspaceTestError::new(format!(
                    "workspace package {:?} depends on unowned local path {}",
                    package.name,
                    dependency.display(),
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod declared_source_tests {
    use super::*;

    #[test]
    fn extensionless_file_macro_input_is_declared_without_a_rust_change_hint() {
        let root = crate::test_repository::Fixture::repository("declared-extensionless-input");
        root.write("dist/version/base", b"0.2.0-dev.0\n");
        let version = fs::canonicalize(root.join("dist/version/base")).expect("canonical version");
        let declared = DeclaredPathSource {
            path: version,
            test_only: false,
            dependent_tests: true,
            origins: BTreeSet::from([DeclaredPathSourceOrigin::FileMacro]),
            scopes: BTreeSet::new(),
        };
        let target = TargetIdentity {
            package: "dist".to_owned(),
            kind: "lib".to_owned(),
            name: "dist".to_owned(),
            required_features: Vec::new(),
            source: root.join("dist/src/lib.rs"),
            declared_path_sources: vec![declared],
            source_bytes: 1,
        };
        let inventory = Inventory {
            packages: vec![PackageInventory {
                id: "dist 0.0.0".to_owned(),
                name: "dist".to_owned(),
                root: root.join("dist"),
                dependencies: Vec::new(),
                namespace_context: FileMacroCrateContext::from_metadata("dist", "2024", &[])
                    .expect("fixture namespace"),
                compile_time_authority: None,
                declared_targets: vec![target.clone()],
                removed_targets: Vec::new(),
                targets: vec![target],
                data_roots: Vec::new(),
            }],
            moved_package_roots: BTreeMap::new(),
            resolution: CargoResolution::default(),
            binary_outputs: Vec::new(),
            target_directory: root.join("target"),
            build_directory: root.join("target/cargo-build"),
        };

        assert_eq!(
            declared_compile_time_sources(&inventory, root.path()).expect("declared sources"),
            ["dist/version/base"]
        );
        assert!(
            cargo_declared_compile_time_sources_at(root.path(), &Environment::empty())
                .expect("repository without Cargo")
                .is_empty()
        );
    }

    fn package(
        workspace: &Path,
        directory: &str,
        name: &str,
        dependency: &str,
    ) -> PackageInventory {
        let package_root = workspace.join(directory);
        let target = TargetIdentity {
            package: format!("{name} 0.0.0"),
            kind: "lib".to_owned(),
            name: name.to_owned(),
            required_features: Vec::new(),
            source: package_root.join("src/lib.rs"),
            declared_path_sources: Vec::new(),
            source_bytes: 1,
        };
        PackageInventory {
            id: format!("{name} 0.0.0"),
            name: name.to_owned(),
            root: package_root,
            dependencies: vec![workspace.join(dependency)],
            namespace_context: FileMacroCrateContext::from_metadata(name, "2024", &[])
                .expect("fixture namespace"),
            compile_time_authority: None,
            declared_targets: vec![target.clone()],
            removed_targets: Vec::new(),
            targets: vec![target],
            data_roots: Vec::new(),
        }
    }

    fn inventory(root: &Path, packages: Vec<PackageInventory>) -> Inventory {
        Inventory {
            packages,
            resolution: CargoResolution::default(),
            moved_package_roots: BTreeMap::new(),
            binary_outputs: Vec::new(),
            target_directory: root.join("target"),
            build_directory: root.join("target/cargo-build"),
        }
    }

    #[test]
    fn a_moved_package_with_a_moved_dependency_keeps_its_unique_current_successor() {
        let fixture = crate::test_repository::Fixture::repository("moved-package-successor");
        let base_root = fixture.join("base");
        let current_root = fixture.join("current");
        let base = inventory(
            &base_root,
            vec![
                package(&base_root, "legacy", "serve", "legacy-worker"),
                package(&base_root, "legacy-worker", "worker", "wire"),
            ],
        );
        let mut current = inventory(
            &current_root,
            vec![
                package(&current_root, "serve", "serve", "tools/worker"),
                package(&current_root, "tools/worker", "worker", "wire"),
            ],
        );
        let changes = [
            "legacy/Cargo.toml".to_owned(),
            "legacy/src/lib.rs".to_owned(),
            "serve/Cargo.toml".to_owned(),
            "serve/src/lib.rs".to_owned(),
            "legacy-worker/Cargo.toml".to_owned(),
            "legacy-worker/src/lib.rs".to_owned(),
            "tools/worker/Cargo.toml".to_owned(),
            "tools/worker/src/lib.rs".to_owned(),
        ];

        current
            .retain_removed_target_ownership(&current_root, &base, &base_root, &changes)
            .expect("shape-preserving package and dependency moves retain target ownership");

        assert!(current.packages[0].removed_targets.is_empty());
        assert_eq!(
            current
                .moved_package_roots
                .get(&current_root.join("legacy")),
            Some(&current.packages[0].id),
        );
        assert_eq!(
            current
                .moved_package_roots
                .get(&current_root.join("legacy-worker")),
            Some(&current.packages[1].id),
        );
    }

    #[test]
    fn a_nested_package_can_consolidate_into_its_changed_ancestor_package() {
        let fixture = crate::test_repository::Fixture::repository("consolidated-package");
        let base_root = fixture.join("base");
        let current_root = fixture.join("current");
        let base = inventory(
            &base_root,
            vec![package(
                &base_root,
                "windows/guest",
                "windows-guest",
                "wire",
            )],
        );
        let mut current = inventory(
            &current_root,
            vec![package(
                &current_root,
                "windows",
                "windows-execution",
                "wire",
            )],
        );
        let changes = [
            "windows/guest/Cargo.toml".to_owned(),
            "windows/guest/src/lib.rs".to_owned(),
            "windows/Cargo.toml".to_owned(),
            "windows/src/lib.rs".to_owned(),
        ];

        current
            .retain_removed_target_ownership(&current_root, &base, &base_root, &changes)
            .expect("a changed ancestor package owns a removed nested package");

        assert_eq!(
            current
                .moved_package_roots
                .get(&current_root.join("windows/guest")),
            Some(&current.packages[0].id),
        );
    }

    #[test]
    fn a_different_package_name_is_not_a_moved_successor() {
        let fixture = crate::test_repository::Fixture::repository("moved-package-name");
        let base_root = fixture.join("base");
        let current_root = fixture.join("current");
        let base = inventory(
            &base_root,
            vec![package(&base_root, "legacy", "legacy", "wire")],
        );
        let mut current = inventory(
            &current_root,
            vec![package(&current_root, "replacement", "serve", "wire")],
        );
        let changes = [
            "legacy/Cargo.toml".to_owned(),
            "legacy/src/lib.rs".to_owned(),
            "replacement/Cargo.toml".to_owned(),
            "replacement/src/lib.rs".to_owned(),
        ];

        assert!(
            current
                .retain_removed_target_ownership(&current_root, &base, &base_root, &changes)
                .is_err()
        );
        assert!(current.moved_package_roots.is_empty());
    }
}
