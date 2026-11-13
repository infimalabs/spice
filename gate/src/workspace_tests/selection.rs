//! Pure staged build-source reach over one canonical Cargo inventory.

use super::*;

/// Ordinary source changes stay comfortably below the historical
/// repository-wide selected-test inventory and run each target once. A change
/// that reaches further is an authority change, however it is spelled: its
/// effect cannot be narrowed, so it runs under the exhaustive verifier.
pub(super) const ROUTINE_TEST_TARGET_LIMIT: usize = 32;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DeclaredTestTarget {
    package: &'static str,
    kind: &'static str,
    name: &'static str,
}

impl DeclaredTestTarget {
    const fn new(package: &'static str, kind: &'static str, name: &'static str) -> Self {
        Self {
            package,
            kind,
            name,
        }
    }

    fn display(self) -> String {
        format!("{}::{}:{}", self.package, self.kind, self.name)
    }
}

#[derive(Clone, Copy, Debug)]
struct DurableSourceSelection {
    source: &'static str,
    owner: DeclaredTestTarget,
    supplemental: &'static [DeclaredTestTarget],
}

const WIRE_PROOFS: &[DeclaredTestTarget] = &[DeclaredTestTarget::new("wire", "test", "suite")];
const STEER_PROOFS: &[DeclaredTestTarget] = &[DeclaredTestTarget::new("steer", "test", "suite")];

/// Durable source declarations add semantic proof targets to ordinary Cargo
/// reach. Their owner and every named target are checked against the selected
/// tree before any selection is published, so this table cannot go stale in
/// silence. Downstream contracts remain derived from public module provenance;
/// declarations may name only local proof targets, and a declaration must
/// name at least one.
const DURABLE_SOURCE_SELECTIONS: &[DurableSourceSelection] = &[
    DurableSourceSelection {
        source: "steer/src/ack/keys.rs",
        owner: DeclaredTestTarget::new("steer", "lib", "steer"),
        supplemental: STEER_PROOFS,
    },
    DurableSourceSelection {
        source: "wire/src/fact.rs",
        owner: DeclaredTestTarget::new("wire", "lib", "wire"),
        supplemental: WIRE_PROOFS,
    },
    DurableSourceSelection {
        source: "wire/src/environment.rs",
        owner: DeclaredTestTarget::new("wire", "lib", "wire"),
        supplemental: WIRE_PROOFS,
    },
    DurableSourceSelection {
        source: "wire/src/witness.rs",
        owner: DeclaredTestTarget::new("wire", "lib", "wire"),
        supplemental: WIRE_PROOFS,
    },
];

#[derive(Debug)]
pub(super) struct SelectionPlan {
    report: crate::TestSelectionReport,
}

impl SelectionPlan {
    pub(super) fn into_report(self) -> crate::TestSelectionReport {
        self.report
    }
}

pub(super) fn package_inventory(
    package: MetadataPackage,
    root: &Path,
) -> Result<PackageInventory, WorkspaceTestError> {
    let selected_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if !package.manifest_path.is_absolute() {
        return Err(WorkspaceTestError::new(format!(
            "workspace package {:?} metadata named relative manifest {}",
            package.name,
            package.manifest_path.display(),
        )));
    }
    let manifest = package.manifest_path.clone();
    let manifest = fs::canonicalize(&manifest).unwrap_or(manifest);
    let package_root = manifest.parent().ok_or_else(|| {
        WorkspaceTestError::new(format!(
            "workspace package {:?} has no manifest parent",
            package.name
        ))
    })?;
    if !package_root.starts_with(&selected_root) {
        return Err(WorkspaceTestError::new(format!(
            "workspace package {:?} manifest {} escapes selected root {}",
            package.name,
            manifest.display(),
            selected_root.display(),
        )));
    }
    let (compile_time_authority, namespace_context) = package_authorities(&package)?;
    let mut dependencies = package
        .dependencies
        .into_iter()
        .filter_map(|dependency| dependency.path)
        .map(|path| {
            if !path.is_absolute() {
                return Err(WorkspaceTestError::new(format!(
                    "workspace package {:?} metadata named relative dependency {}",
                    package.name,
                    path.display(),
                )));
            }
            Ok(fs::canonicalize(&path).unwrap_or(path))
        })
        .collect::<Result<Vec<_>, _>>()?;
    dependencies.sort();
    dependencies.dedup();
    let mut declared_targets = package
        .targets
        .into_iter()
        .map(|target| {
            target_identity(
                TargetOwner {
                    id: &package.id,
                    name: &package.name,
                    features: &package.features,
                    namespace_context: &namespace_context,
                },
                &selected_root,
                target,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    declared_targets.sort_by(|left, right| left.1.cmp(&right.1));
    let targets = declared_targets
        .iter()
        .filter(|(test, _)| *test)
        .map(|(_, target)| target.clone())
        .collect();
    let declared_targets: Vec<TargetIdentity> = declared_targets
        .into_iter()
        .map(|(_, target)| target)
        .collect();
    let data_roots = declared_data_roots(&package.name, &package.metadata, package_root)?;
    validate_data_roots(&package.name, &data_roots, &declared_targets)?;
    Ok(PackageInventory {
        id: package.id,
        name: package.name,
        root: package_root.to_path_buf(),
        dependencies,
        namespace_context,
        compile_time_authority,
        declared_targets,
        removed_targets: Vec::new(),
        targets,
        data_roots,
    })
}

/// What a target needs to know about the package declaring it.
struct TargetOwner<'a> {
    id: &'a str,
    name: &'a str,
    features: &'a BTreeMap<String, Vec<String>>,
    namespace_context: &'a FileMacroCrateContext,
}

/// One declared Cargo target resolved to its canonical source reach, paired
/// with whether Cargo builds it for tests.
fn target_identity(
    owner: TargetOwner<'_>,
    selected_root: &Path,
    target: MetadataTarget,
) -> Result<(bool, TargetIdentity), WorkspaceTestError> {
    let test = target.test;
    let mut required_features = target.required_features;
    required_features.sort();
    required_features.dedup();
    if let Some(feature) = required_features
        .iter()
        .find(|feature| !owner.features.contains_key(feature.as_str()))
    {
        return Err(WorkspaceTestError::new(format!(
            "workspace package {:?} target {:?} requires undeclared feature {:?}",
            owner.name, target.name, feature,
        )));
    }
    if !target.src_path.is_absolute() {
        return Err(WorkspaceTestError::new(format!(
            "workspace package {:?} target {:?} metadata named relative source {}",
            owner.name,
            target.name,
            target.src_path.display(),
        )));
    }
    let source = target.src_path;
    let source = fs::canonicalize(&source).unwrap_or(source);
    if !source.starts_with(selected_root) {
        return Err(WorkspaceTestError::new(format!(
            "workspace package {:?} target source {} escapes selected root {}",
            owner.name,
            source.display(),
            selected_root.display(),
        )));
    }
    let declared_path_sources =
        declared_path_sources(selected_root, &source, owner.namespace_context.clone())?;
    let source_bytes = target_source_bytes(&source, &declared_path_sources)?;
    Ok((
        test,
        TargetIdentity {
            package: owner.id.to_owned(),
            kind: target.kind.join("+"),
            name: target.name,
            required_features,
            source,
            declared_path_sources,
            source_bytes,
        },
    ))
}

/// Directories a package states hold bytes no Cargo target compiles.
///
/// Runtime-read data -- parser input, golden samples, audit corpora -- is Rust
/// text a test opens as a file, and no target's module graph reaches it. Reach
/// analysis can prove a path is compiled but never that it is not: a module
/// introduced by macro expansion is exactly what an unread path looks like. So
/// the tree states it, in the manifest, beside the compile-time authority that
/// already lives there:
///
/// ```toml
/// [package.metadata.spice]
/// data = ["tests/fixture"]
/// ```
fn declared_data_roots(
    package: &str,
    metadata: &serde_json::Value,
    package_root: &Path,
) -> Result<Vec<PathBuf>, WorkspaceTestError> {
    let Some(declared) = metadata.get("spice").and_then(|spice| spice.get("data")) else {
        return Ok(Vec::new());
    };
    let declared: Vec<String> = serde_json::from_value(declared.clone()).map_err(|error| {
        WorkspaceTestError::new(format!(
            "Cargo package {package:?} has invalid package.metadata.spice.data: {error}"
        ))
    })?;
    let mut roots = Vec::new();
    for entry in declared {
        let relative = Path::new(&entry);
        if entry.is_empty()
            || relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(WorkspaceTestError::new(format!(
                "Cargo package {package:?} declares data directory {entry:?}, which is not a plain relative path inside the package",
            )));
        }
        roots.push(package_root.join(relative));
    }
    roots.sort();
    roots.dedup();
    Ok(roots)
}

/// A declared data directory may not hold source a Cargo target compiles,
/// which is what keeps the declaration from becoming a way to hide source
/// from the gate rather than a statement about data.
fn validate_data_roots(
    package: &str,
    roots: &[PathBuf],
    targets: &[TargetIdentity],
) -> Result<(), WorkspaceTestError> {
    for root in roots {
        let compiled = targets.iter().find_map(|target| {
            std::iter::once(&target.source)
                .chain(
                    target
                        .declared_path_sources
                        .iter()
                        .map(|source| &source.path),
                )
                .find(|source| source.starts_with(root))
        });
        if let Some(compiled) = compiled {
            return Err(WorkspaceTestError::new(format!(
                "Cargo package {package:?} declares data directory {} holding compiled source {}",
                root.display(),
                compiled.display(),
            )));
        }
    }
    Ok(())
}

fn package_authorities(
    package: &MetadataPackage,
) -> Result<
    (
        Option<CompileTimeAuthorityDeclaration>,
        FileMacroCrateContext,
    ),
    WorkspaceTestError,
> {
    Ok((
        compile_time::authority_declaration(&package.name, &package.metadata)?,
        FileMacroCrateContext::from_metadata(
            &package.name,
            &package.edition,
            &package.dependencies,
        )?,
    ))
}

pub(super) fn selection_plan(
    inventory: &Inventory,
    root: &Path,
    source: wire::Digest,
    changed_sources: &[String],
) -> Result<SelectionPlan, WorkspaceTestError> {
    let selected_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    validate_durable_source_registry(inventory, &selected_root, DURABLE_SOURCE_SELECTIONS)?;
    let mut build_sources = changed_sources
        .iter()
        .filter(|path| test_relevant_source(path))
        .cloned()
        .collect::<Vec<_>>();
    build_sources.sort();
    build_sources.dedup();
    let mut tests = BTreeSet::new();
    let mut edges = BTreeSet::new();
    for changed in &build_sources {
        let absolute = selected_root.join(changed);
        if workspace_wide_source(changed) {
            for package in &inventory.packages {
                select_package_targets(package, changed, &mut tests, &mut edges)?;
            }
            continue;
        }
        let claims = source_claims(inventory, changed, &absolute);
        if claims.is_empty() && deleted_package_source(inventory, &absolute) {
            continue;
        }
        if claims.is_empty() {
            let package = uncompiled_data(inventory, &absolute)
                .ok_or_else(|| unclassified_source(inventory, changed, &absolute))?;
            select_package_targets(package, changed, &mut tests, &mut edges)?;
        }
        let direct_test_only = exact_test_only_claim(&claims, &absolute);
        for claim in claims {
            if claim.package_wide {
                for package in reverse_dependency_closure(inventory, claim.package) {
                    select_package_targets(package, changed, &mut tests, &mut edges)?;
                }
                continue;
            }
            if claim_has_removed_target(&claim)? {
                select_targets(
                    claim.package,
                    deletion_validation_targets(claim.package)?,
                    changed,
                    &mut tests,
                    &mut edges,
                )?;
                continue;
            }
            if !direct_test_only
                && claim
                    .targets
                    .iter()
                    .any(|target| target_reaches_dependent_tests(target, &absolute))
            {
                select_direct_targets(&claim, changed, &mut tests, &mut edges)?;
                for package in reverse_dependency_closure(inventory, claim.package)
                    .into_iter()
                    .filter(|package| package.id != claim.package.id)
                {
                    select_targets(
                        package,
                        contract_test_targets(package),
                        changed,
                        &mut tests,
                        &mut edges,
                    )?;
                }
                continue;
            }
            let direct_tests = claim
                .targets
                .into_iter()
                .filter(|target| claim.package.targets.contains(target))
                .collect::<Vec<_>>();
            if !direct_tests.is_empty() {
                select_targets(claim.package, direct_tests, changed, &mut tests, &mut edges)?;
            }
        }
        select_durable_source_proofs(inventory, changed, &mut tests, &mut edges)?;
        if !edges.iter().any(|edge| edge.source() == changed) {
            return Err(WorkspaceTestError::new(format!(
                "changed Rust source {changed:?} has no proven test-enabled Cargo target reach"
            )));
        }
    }
    let report = crate::TestSelectionReport::new(
        source,
        build_sources,
        tests.into_iter().collect(),
        edges.into_iter().collect(),
        crate::CoverageCompleteness::Complete,
    )
    .map_err(|error| WorkspaceTestError::new(error.to_string()))?;
    Ok(SelectionPlan { report })
}

pub(super) fn cargo_check_selections(
    inventory: &Inventory,
    root: &Path,
    changed_sources: &[String],
) -> Result<Vec<CargoCheckSelection>, WorkspaceTestError> {
    let selected_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut reached = BTreeMap::<&str, BTreeSet<CargoTargetSelection>>::new();
    if changed_sources.is_empty() {
        select_package_checks(inventory.packages.iter(), &mut reached)?;
    }
    for changed in changed_sources
        .iter()
        .filter(|path| test_relevant_source(path))
    {
        if workspace_wide_source(changed) {
            select_package_checks(inventory.packages.iter(), &mut reached)?;
            continue;
        }
        let absolute = selected_root.join(changed);
        let claims = source_claims(inventory, changed, &absolute);
        if claims.is_empty() && deleted_package_source(inventory, &absolute) {
            continue;
        }
        if claims.is_empty() {
            let package = uncompiled_data(inventory, &absolute)
                .ok_or_else(|| unclassified_source(inventory, changed, &absolute))?;
            select_package_checks([package], &mut reached)?;
        }
        let direct_test_only = exact_test_only_claim(&claims, &absolute);
        for claim in claims {
            if claim.package_wide {
                select_package_checks([claim.package], &mut reached)?;
                select_reverse_package_checks(
                    reverse_dependency_closure(inventory, claim.package)
                        .into_iter()
                        .filter(|package| package.id != claim.package.id),
                    &mut reached,
                )?;
                continue;
            }
            if claim_has_removed_target(&claim)? {
                select_target_checks(
                    claim.package,
                    deletion_validation_targets(claim.package)?,
                    &mut reached,
                )?;
                continue;
            }
            if !direct_test_only
                && claim
                    .targets
                    .iter()
                    .any(|target| target_reaches_dependents(target))
            {
                select_target_checks(claim.package, claim.targets.iter().copied(), &mut reached)?;
                for package in reverse_dependency_closure(inventory, claim.package)
                    .into_iter()
                    .filter(|package| package.id != claim.package.id)
                {
                    select_target_checks(package, contract_test_targets(package), &mut reached)?;
                }
                continue;
            }
            let targets = reached.entry(claim.package.id.as_str()).or_default();
            for target in claim.targets {
                if let Some(target) =
                    cargo_target_selection(target, claim.package.targets.contains(target))?
                {
                    targets.insert(target);
                }
            }
        }
    }
    Ok(inventory
        .packages
        .iter()
        .filter_map(|package| {
            reached
                .get(package.id.as_str())
                .map(|targets| CargoCheckSelection {
                    name: package.name.clone(),
                    root: package.root.clone(),
                    targets: targets.iter().cloned().collect(),
                })
        })
        .collect())
}

struct SourceClaim<'a> {
    package: &'a PackageInventory,
    targets: Vec<&'a TargetIdentity>,
    package_wide: bool,
    test_only: bool,
}

/// Derive every target claim before selecting a package. Exact target roots,
/// `#[path]`, and `include!` reach may cross package roots; the conservative
/// library-directory fallback belongs only to the deepest containing package
/// and applies only when that package has no explicit claim for the source.
fn source_claims<'a>(
    inventory: &'a Inventory,
    changed: &str,
    absolute: &Path,
) -> Vec<SourceClaim<'a>> {
    let containing = containing_package(inventory, absolute);
    inventory
        .packages
        .iter()
        .filter_map(|package| {
            let explicit = package
                .declared_targets
                .iter()
                .chain(package.removed_targets.iter())
                .filter_map(|target| {
                    target_explicit_source_reach(target, absolute)
                        .map(|test_only| (target, test_only))
                })
                .collect::<Vec<_>>();
            let test_only =
                !explicit.is_empty() && explicit.iter().all(|(_, test_only)| *test_only);
            let targets: Vec<&TargetIdentity> =
                if explicit.is_empty() && containing.is_some_and(|owner| owner.id == package.id) {
                    package
                        .declared_targets
                        .iter()
                        .filter(|target| library_fallback_reaches_source(target, absolute))
                        .collect()
                } else {
                    explicit.into_iter().map(|(target, _)| target).collect()
                };
            let package_wide = package_wide_source(changed, package, absolute);
            (!targets.is_empty() || package_wide).then_some(SourceClaim {
                package,
                targets,
                package_wide,
                test_only,
            })
        })
        .collect()
}

fn claim_has_removed_target(claim: &SourceClaim<'_>) -> Result<bool, WorkspaceTestError> {
    let removed = claim
        .targets
        .iter()
        .filter(|target| claim.package.removed_targets.contains(target))
        .count();
    if removed == 0 {
        return Ok(false);
    }
    if removed != claim.targets.len() {
        return Err(WorkspaceTestError::new(format!(
            "Cargo package {:?} has mixed current and removed target ownership for one changed source",
            claim.package.name,
        )));
    }
    Ok(true)
}

/// Current unit target that proves a package remains valid after one target disappears.
fn deletion_validation_targets(
    package: &PackageInventory,
) -> Result<Vec<&TargetIdentity>, WorkspaceTestError> {
    let libraries = package
        .targets
        .iter()
        .filter(|target| target_is_library(target))
        .collect::<Vec<_>>();
    if !libraries.is_empty() {
        return Ok(libraries);
    }
    let binaries = package
        .targets
        .iter()
        .filter(|target| target_has_kind(target, "bin"))
        .collect::<Vec<_>>();
    if !binaries.is_empty() {
        return Ok(binaries);
    }
    Err(WorkspaceTestError::new(format!(
        "Cargo package {:?} has no current library or binary target for deleted-target validation",
        package.name,
    )))
}

fn exact_test_only_claim(claims: &[SourceClaim<'_>], source: &Path) -> bool {
    // Narrow only one present source with one proven owner. Removed sources
    // and multiply-reached paths deliberately retain the package-level
    // fallback because the selected tree cannot prove an exclusive test edge.
    let [claim] = claims else {
        return false;
    };
    source.is_file() && claim.test_only && claim.targets.len() == 1
}

fn select_direct_targets(
    claim: &SourceClaim<'_>,
    changed: &str,
    tests: &mut BTreeSet<String>,
    edges: &mut BTreeSet<crate::TestSelectionEdge>,
) -> Result<(), WorkspaceTestError> {
    select_targets(
        claim.package,
        claim
            .targets
            .iter()
            .copied()
            .filter(|target| claim.package.targets.contains(target)),
        changed,
        tests,
        edges,
    )
}

/// Runtime contracts in reverse dependents enter through their library or
/// binary unit harness. Process-heavy integration targets remain in the full
/// verifier and are selected directly when their own source changes.
fn contract_test_targets(package: &PackageInventory) -> Vec<&TargetIdentity> {
    let contract = package
        .targets
        .iter()
        .filter(|target| target_is_library(target) || target_has_kind(target, "bin"))
        .collect::<Vec<_>>();
    if contract.is_empty() {
        package.targets.iter().collect()
    } else {
        contract
    }
}

fn validate_durable_source_registry(
    inventory: &Inventory,
    root: &Path,
    registry: &[DurableSourceSelection],
) -> Result<(), WorkspaceTestError> {
    let mut sources = BTreeSet::new();
    for declaration in registry {
        if !sources.insert(declaration.source) {
            return Err(WorkspaceTestError::new(format!(
                "durable source {:?} is declared more than once",
                declaration.source,
            )));
        }
        validate_durable_source_declaration(inventory, root, declaration)?;
    }
    Ok(())
}

fn validate_durable_source_declaration(
    inventory: &Inventory,
    root: &Path,
    declaration: &DurableSourceSelection,
) -> Result<(), WorkspaceTestError> {
    let source = Path::new(declaration.source);
    if source.is_absolute()
        || source
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(WorkspaceTestError::new(format!(
            "durable source {:?} is not a normalized repository-relative path",
            declaration.source,
        )));
    }
    if declaration.supplemental.is_empty() {
        return Err(WorkspaceTestError::new(format!(
            "durable source {:?} declares no supplemental proof target",
            declaration.source,
        )));
    }
    validate_durable_source_owner(inventory, root, declaration)?;
    validate_supplemental_targets(inventory, declaration)
}

fn validate_durable_source_owner(
    inventory: &Inventory,
    root: &Path,
    declaration: &DurableSourceSelection,
) -> Result<(), WorkspaceTestError> {
    let owner_package = declared_package(inventory, declaration.owner.package)?;
    let owner = declared_target(owner_package, declaration.owner)?;
    let absolute = root.join(declaration.source);
    validate_durable_owner_claim(inventory, declaration, owner_package, owner, &absolute)?;
    validate_durable_owner_provenance(declaration, owner_package, owner, &absolute)
}

fn validate_durable_owner_claim(
    inventory: &Inventory,
    declaration: &DurableSourceSelection,
    owner_package: &PackageInventory,
    owner: &TargetIdentity,
    absolute: &Path,
) -> Result<(), WorkspaceTestError> {
    let claims = source_claims(inventory, declaration.source, absolute);
    let [claim] = claims.as_slice() else {
        return Err(WorkspaceTestError::new(format!(
            "durable source {:?} must have exactly one Cargo source-provenance owner, found {}",
            declaration.source,
            claims.len(),
        )));
    };
    if claim.package.id == owner_package.id
        && !claim.package_wide
        && !claim.test_only
        && claim.targets.as_slice() == [owner]
    {
        return Ok(());
    }
    Err(WorkspaceTestError::new(format!(
        "durable source {:?} is not owned exclusively by declared target {}",
        declaration.source,
        declaration.owner.display(),
    )))
}

fn validate_durable_owner_provenance(
    declaration: &DurableSourceSelection,
    owner_package: &PackageInventory,
    owner: &TargetIdentity,
    absolute: &Path,
) -> Result<(), WorkspaceTestError> {
    let Some(provenance) = owner
        .declared_path_sources
        .iter()
        .find(|declared| declared.path == absolute)
    else {
        return Err(WorkspaceTestError::new(format!(
            "durable source {:?} has no declared module provenance in {}",
            declaration.source,
            declaration.owner.display(),
        )));
    };
    let public_module = !provenance.test_only
        && provenance.dependent_tests
        && provenance
            .origins
            .contains(&DeclaredPathSourceOrigin::Module);
    if target_is_library(owner) && owner_package.targets.contains(owner) && public_module {
        return Ok(());
    }
    Err(WorkspaceTestError::new(format!(
        "durable source {:?} must be a non-test-only public module of test-enabled library target {}",
        declaration.source,
        declaration.owner.display(),
    )))
}

fn validate_supplemental_targets(
    inventory: &Inventory,
    declaration: &DurableSourceSelection,
) -> Result<(), WorkspaceTestError> {
    let mut supplemental = BTreeSet::new();
    for target in declaration.supplemental {
        if target.package != declaration.owner.package {
            return Err(WorkspaceTestError::new(format!(
                "durable source {:?} supplemental target {} is outside its owner package; downstream contracts must come from public source provenance",
                declaration.source,
                target.display(),
            )));
        }
        if !supplemental.insert(*target) {
            return Err(WorkspaceTestError::new(format!(
                "durable source {:?} repeats supplemental target {}",
                declaration.source,
                target.display(),
            )));
        }
        let package = declared_package(inventory, target.package)?;
        let resolved = declared_target(package, *target)?;
        if !package.targets.contains(resolved) {
            return Err(WorkspaceTestError::new(format!(
                "durable source {:?} supplemental target {} is not test-enabled",
                declaration.source,
                target.display(),
            )));
        }
    }
    Ok(())
}

fn declared_package<'a>(
    inventory: &'a Inventory,
    name: &str,
) -> Result<&'a PackageInventory, WorkspaceTestError> {
    let mut matches = inventory
        .packages
        .iter()
        .filter(|package| package.name == name);
    let package = matches.next().ok_or_else(|| {
        WorkspaceTestError::new(format!(
            "durable source registry names unknown Cargo package {name:?}"
        ))
    })?;
    if matches.next().is_some() {
        return Err(WorkspaceTestError::new(format!(
            "durable source registry package name {name:?} is ambiguous"
        )));
    }
    Ok(package)
}

fn declared_target(
    package: &PackageInventory,
    declaration: DeclaredTestTarget,
) -> Result<&TargetIdentity, WorkspaceTestError> {
    package
        .declared_targets
        .iter()
        .find(|target| target.kind == declaration.kind && target.name == declaration.name)
        .ok_or_else(|| {
            WorkspaceTestError::new(format!(
                "durable source registry names unknown Cargo target {}",
                declaration.display(),
            ))
        })
}

fn select_durable_source_proofs(
    inventory: &Inventory,
    changed: &str,
    tests: &mut BTreeSet<String>,
    edges: &mut BTreeSet<crate::TestSelectionEdge>,
) -> Result<(), WorkspaceTestError> {
    let Some(declaration) = DURABLE_SOURCE_SELECTIONS
        .iter()
        .find(|declaration| declaration.source == changed)
    else {
        return Ok(());
    };
    let package = declared_package(inventory, declaration.owner.package)?;
    let targets = declaration
        .supplemental
        .iter()
        .map(|target| declared_target(package, *target))
        .collect::<Result<Vec<_>, _>>()?;
    select_targets(package, targets, changed, tests, edges)
}

pub(super) fn routine_selection(changed_sources: &[String], targets: usize) -> bool {
    targets <= ROUTINE_TEST_TARGET_LIMIT
        && changed_sources
            .iter()
            .filter(|path| test_relevant_source(path))
            .all(|path| !selection_authority_source(path))
}

fn selection_authority_source(path: &str) -> bool {
    workspace_wide_source(path)
        || matches!(
            Path::new(path).file_name().and_then(OsStr::to_str),
            Some("Cargo.toml") | Some("build.rs")
        )
}

fn containing_package<'a>(
    inventory: &'a Inventory,
    absolute: &Path,
) -> Option<&'a PackageInventory> {
    inventory
        .packages
        .iter()
        .filter(|package| absolute.starts_with(&package.root))
        .max_by_key(|package| package.root.components().count())
        .or_else(|| {
            inventory
                .moved_package_roots
                .iter()
                .filter(|(root, _)| absolute.starts_with(root))
                .max_by_key(|(root, _)| root.components().count())
                .and_then(|(_, owner)| {
                    inventory
                        .packages
                        .iter()
                        .find(|package| package.id == *owner)
                })
        })
}

/// The package whose own coverage answers for a Rust path no Cargo target
/// compiles, if that is what the change is.
///
/// A path is data on either of two grounds. The package may declare the
/// directory it lives in, which covers a data file arriving, moving, or
/// changing. Or the path may be gone from the selected tree while no target
/// claims it in the current or the base inventory, which is only true of
/// something nothing ever compiled: a deleted module still claims its target
/// through the library fallback or its base-tree removed target.
///
/// Absence is what makes the undeclared claim checkable, and only absence:
/// a path that is merely unreadable says nothing about what compiles it, so
/// every filesystem answer other than "not found" stays a refusal. A file
/// still present and undeclared keeps refusing too, because unread source
/// could reach it by a path this scanner failed to resolve, and a missing
/// owner there is a scanner gap rather than a statement about the file.
fn uncompiled_data<'a>(inventory: &'a Inventory, absolute: &Path) -> Option<&'a PackageInventory> {
    let package = containing_package(inventory, absolute)?;
    let declared = package
        .data_roots
        .iter()
        .any(|root| absolute.starts_with(root));
    (declared || absent(absolute)).then_some(package)
}

/// Whether a path no target claims belonged to a deleted package: it is gone
/// from the selected tree and no current package contains it. Nothing current
/// compiles it, and deleting a package changes the workspace manifest and
/// every former dependent's own manifest, which select what it reached.
fn deleted_package_source(inventory: &Inventory, absolute: &Path) -> bool {
    containing_package(inventory, absolute).is_none() && absent(absolute)
}

fn absent(path: &Path) -> bool {
    matches!(
        path.symlink_metadata(),
        Err(error) if error.kind() == io::ErrorKind::NotFound
    )
}

fn unclassified_source(
    inventory: &Inventory,
    changed: &str,
    absolute: &Path,
) -> WorkspaceTestError {
    containing_package(inventory, absolute).map_or_else(
        || {
            WorkspaceTestError::new(format!(
                "changed Rust source {changed:?} is outside every Cargo package"
            ))
        },
        |package| {
            WorkspaceTestError::new(format!(
                "changed Rust source {changed:?} is inside Cargo package {:?} but neither current nor base-tree Cargo target ownership establishes it",
                package.name,
            ))
        },
    )
}

fn select_package_checks<'a>(
    packages: impl IntoIterator<Item = &'a PackageInventory>,
    reached: &mut BTreeMap<&'a str, BTreeSet<CargoTargetSelection>>,
) -> Result<(), WorkspaceTestError> {
    for package in packages {
        let targets = reached.entry(package.id.as_str()).or_default();
        for target in &package.declared_targets {
            if let Some(target) = cargo_target_selection(target, package.targets.contains(target))?
            {
                targets.insert(target);
            }
        }
        if targets.is_empty() {
            return Err(WorkspaceTestError::new(format!(
                "Cargo package {:?} has no directly addressable build target",
                package.name,
            )));
        }
    }
    Ok(())
}

fn select_target_checks<'a>(
    package: &'a PackageInventory,
    selected: impl IntoIterator<Item = &'a TargetIdentity>,
    reached: &mut BTreeMap<&'a str, BTreeSet<CargoTargetSelection>>,
) -> Result<(), WorkspaceTestError> {
    let targets = reached.entry(package.id.as_str()).or_default();
    for target in selected {
        if let Some(target) = cargo_target_selection(target, package.targets.contains(target))? {
            targets.insert(target);
        }
    }
    Ok(())
}

/// Retain every addressable target in reverse-dependent packages.
///
/// Cargo metadata proves dependencies at package granularity, not at the
/// individual target that uses them. Naming only a library target would omit a
/// bin-only consumer or a dependent integration target, so the package's exact
/// target inventory is the narrowest complete downstream reach available.
fn select_reverse_package_checks<'a>(
    packages: impl IntoIterator<Item = &'a PackageInventory>,
    reached: &mut BTreeMap<&'a str, BTreeSet<CargoTargetSelection>>,
) -> Result<(), WorkspaceTestError> {
    select_package_checks(packages, reached)
}

fn cargo_target_selection(
    target: &TargetIdentity,
    test_harness: bool,
) -> Result<Option<CargoTargetSelection>, WorkspaceTestError> {
    if target.kind.split('+').all(|kind| kind == "custom-build") {
        return Ok(None);
    }
    let (flag, named) = target
        .kind
        .split('+')
        .find_map(|kind| match kind {
            "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro" => {
                Some(("--lib", false))
            }
            "bin" => Some(("--bin", true)),
            "test" => Some(("--test", true)),
            "example" => Some(("--example", true)),
            "bench" => Some(("--bench", true)),
            _ => None,
        })
        .ok_or_else(|| {
            WorkspaceTestError::new(format!(
                "workspace target {} has no exact Cargo check selector",
                target.display()
            ))
        })?;
    Ok(Some(CargoTargetSelection {
        identity: target.display(),
        flag,
        name: named.then(|| target.name.clone()),
        required_features: target.required_features.clone(),
        test_harness,
    }))
}

fn reverse_dependency_closure<'a>(
    inventory: &'a Inventory,
    package: &'a PackageInventory,
) -> Vec<&'a PackageInventory> {
    let mut reached = BTreeSet::from([package.id.as_str()]);
    let mut pending = VecDeque::from([package.root.as_path()]);
    while let Some(root) = pending.pop_front() {
        for dependent in &inventory.packages {
            if dependent
                .dependencies
                .iter()
                .any(|dependency| dependency == root)
                && reached.insert(dependent.id.as_str())
            {
                pending.push_back(&dependent.root);
            }
        }
    }
    inventory
        .packages
        .iter()
        .filter(|candidate| reached.contains(candidate.id.as_str()))
        .collect()
}

fn select_targets<'a>(
    package: &PackageInventory,
    targets: impl IntoIterator<Item = &'a TargetIdentity>,
    changed: &str,
    tests: &mut BTreeSet<String>,
    edges: &mut BTreeSet<crate::TestSelectionEdge>,
) -> Result<(), WorkspaceTestError> {
    for target in targets {
        let test = format!("{}::{}", package.name, target.display());
        tests.insert(test.clone());
        edges.insert(
            crate::TestSelectionEdge::new(changed.to_owned(), test)
                .map_err(|error| WorkspaceTestError::new(error.to_string()))?,
        );
    }
    Ok(())
}

pub(super) fn test_relevant_source(path: &str) -> bool {
    Path::new(path).extension() == Some(OsStr::new("rs"))
        || Path::new(path).file_name() == Some(OsStr::new("Cargo.toml"))
        || workspace_wide_source(path)
}

fn workspace_wide_source(path: &str) -> bool {
    path == wire::TRACKED_POLICY_FILE
        || matches!(
            path,
            ".cargo/config"
                | ".cargo/config.toml"
                | "Cargo.toml"
                | "Cargo.lock"
                | "rust-toolchain"
                | "rust-toolchain.toml"
                | "gate/src/battery/manifest.toml"
        )
}

fn package_wide_source(changed: &str, package: &PackageInventory, absolute: &Path) -> bool {
    absolute == package.root.join("Cargo.toml")
        || (Path::new(changed).file_name() == Some(OsStr::new("build.rs"))
            && absolute.parent() == Some(package.root.as_path()))
}

fn select_package_targets(
    package: &PackageInventory,
    changed: &str,
    tests: &mut BTreeSet<String>,
    edges: &mut BTreeSet<crate::TestSelectionEdge>,
) -> Result<(), WorkspaceTestError> {
    select_targets(package, &package.targets, changed, tests, edges)
}

/// Apply the conventional library-directory fallback after a package has no
/// exact target-root or declared module claim for the source.
fn library_fallback_reaches_source(target: &TargetIdentity, source: &Path) -> bool {
    let direct_root = target_is_library(target)
        .then(|| target.source.parent())
        .flatten();
    direct_root.is_some_and(|root| source.starts_with(root))
}

fn target_explicit_source_reach(target: &TargetIdentity, source: &Path) -> Option<bool> {
    if source == target.source {
        return Some(false);
    }
    target
        .declared_path_sources
        .iter()
        .find_map(|declared| (declared.path == source).then_some(declared.test_only))
}

fn target_is_library(target: &TargetIdentity) -> bool {
    target.kind.split('+').any(|kind| {
        matches!(
            kind,
            "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro"
        )
    })
}

fn target_has_kind(target: &TargetIdentity, expected: &str) -> bool {
    target.kind.split('+').any(|kind| kind == expected)
}

fn target_reaches_dependents(target: &TargetIdentity) -> bool {
    target_is_library(target) || target_has_kind(target, "custom-build")
}

/// Run reverse-dependent harnesses only for a source exposed by the target.
///
/// Cargo checks retain package-wide reverse reach for every library source, so
/// an internal module cannot silently break a downstream compile contract. Test
/// harnesses add behavioral evidence for the library root, public modules and
/// public re-exports. Missing source metadata stays conservative.
fn target_reaches_dependent_tests(target: &TargetIdentity, source: &Path) -> bool {
    target_has_kind(target, "custom-build")
        || (target_is_library(target)
            && (source == target.source
                || target
                    .declared_path_sources
                    .iter()
                    .find(|declared| declared.path == source)
                    .is_none_or(|declared| declared.dependent_tests)))
}

#[cfg(test)]
mod tests;
