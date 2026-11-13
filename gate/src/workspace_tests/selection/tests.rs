use super::*;

fn package_and_dependent_fixture(
    fixture: &crate::test_repository::Fixture,
    data: &[&str],
) -> (Inventory, PackageInventory) {
    let root = fs::canonicalize(fixture.path()).expect("canonical fixture root");
    let target = |package: &str, kind: &str, name: &str, source: &str| TargetIdentity {
        package: package.to_owned(),
        kind: kind.to_owned(),
        name: name.to_owned(),
        required_features: Vec::new(),
        source: root.join(source),
        declared_path_sources: Vec::new(),
        source_bytes: 1,
    };
    let targets = vec![
        target("wire", "lib", "wire", "wire/src/lib.rs"),
        target("wire", "test", "absence", "wire/tests/absence.rs"),
    ];
    let package = PackageInventory {
        id: "wire 0.0.0".to_owned(),
        name: "wire".to_owned(),
        root: root.join("wire"),
        dependencies: Vec::new(),
        namespace_context: FileMacroCrateContext::from_metadata("wire", "2024", &[])
            .expect("fixture namespace"),
        compile_time_authority: None,
        declared_targets: targets.clone(),
        removed_targets: Vec::new(),
        targets,
        data_roots: data.iter().map(|data| root.join(data)).collect(),
    };
    let dependent_targets = vec![target("consumer", "lib", "consumer", "consumer/src/lib.rs")];
    let dependent = PackageInventory {
        id: "consumer 0.0.0".to_owned(),
        name: "consumer".to_owned(),
        root: root.join("consumer"),
        dependencies: vec![package.root.clone()],
        namespace_context: FileMacroCrateContext::from_metadata("consumer", "2024", &[])
            .expect("dependent namespace"),
        declared_targets: dependent_targets.clone(),
        targets: dependent_targets,
        ..package.clone()
    };
    let inventory = Inventory {
        packages: vec![package.clone(), dependent],
        moved_package_roots: BTreeMap::new(),
        resolution: CargoResolution::default(),
        binary_outputs: Vec::new(),
        target_directory: root.join("target"),
        build_directory: root.join("target/cargo-build"),
    };
    (inventory, package)
}

/// A package whose test targets, feature-gated test, and check-only bin
/// each compile as their own unit, beside a dependent package.
fn narrowing_fixture(fixture: &crate::test_repository::Fixture) -> Inventory {
    let root = fs::canonicalize(fixture.path()).expect("canonical fixture root");
    let target = |package: &str, kind: &str, name: &str, source: &str, features: &[&str]| {
        fixture.write(source, b"fn fixture() {}\n");
        TargetIdentity {
            package: package.to_owned(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            required_features: features.iter().map(|&feature| feature.to_owned()).collect(),
            source: root.join(source),
            declared_path_sources: Vec::new(),
            source_bytes: 1,
        }
    };
    let tested = vec![
        target("planted", "lib", "planted", "planted/src/lib.rs", &[]),
        target(
            "planted",
            "test",
            "selected",
            "planted/tests/selected.rs",
            &[],
        ),
        target(
            "planted",
            "test",
            "unrelated",
            "planted/tests/unrelated.rs",
            &["unrelated"],
        ),
        target(
            "planted",
            "test",
            "guarded",
            "planted/tests/guarded.rs",
            &["guarded"],
        ),
    ];
    let mut declared = tested.clone();
    declared.push(target(
        "planted",
        "bin",
        "tool",
        "planted/src/bin/tool.rs",
        &[],
    ));
    let planted = PackageInventory {
        id: "planted 0.0.0".to_owned(),
        name: "planted".to_owned(),
        root: root.join("planted"),
        dependencies: Vec::new(),
        namespace_context: FileMacroCrateContext::from_metadata("planted", "2024", &[])
            .expect("fixture namespace"),
        compile_time_authority: None,
        declared_targets: declared,
        removed_targets: Vec::new(),
        targets: tested,
        data_roots: Vec::new(),
    };
    let consumer_targets = vec![target(
        "consumer",
        "lib",
        "consumer",
        "consumer/src/lib.rs",
        &[],
    )];
    let consumer = PackageInventory {
        id: "consumer 0.0.0".to_owned(),
        name: "consumer".to_owned(),
        root: root.join("consumer"),
        dependencies: vec![planted.root.clone()],
        namespace_context: FileMacroCrateContext::from_metadata("consumer", "2024", &[])
            .expect("dependent namespace"),
        declared_targets: consumer_targets.clone(),
        targets: consumer_targets,
        ..planted.clone()
    };
    Inventory {
        packages: vec![planted, consumer],
        moved_package_roots: BTreeMap::new(),
        resolution: CargoResolution::default(),
        binary_outputs: Vec::new(),
        target_directory: root.join("target"),
        build_directory: root.join("target/cargo-build"),
    }
}

/// The Cargo units one changed source compiles, as argument lines.
fn units(
    inventory: &Inventory,
    fixture: &crate::test_repository::Fixture,
    changed: &str,
) -> Vec<String> {
    let checks = cargo_check_selections(inventory, fixture.path(), &[changed.to_owned()])
        .expect("a present owned source selects its checks");
    let reach = WorkspacePackageReach {
        roots: checks.iter().map(|check| check.root.clone()).collect(),
        checks,
    };
    reach
        .cargo_check_units()
        .iter()
        .map(|unit| unit.arguments().join(" "))
        .collect()
}

/// A change lints only the target it reaches: no sibling target, no
/// dependent package, the exact package-qualified features a gated target
/// requires so Cargo does not skip it, and the test profile only for a
/// target whose tests run.
#[test]
fn a_changed_target_source_compiles_only_its_own_target() {
    let fixture = crate::test_repository::Fixture::repository("reach-narrowing");
    let inventory = narrowing_fixture(&fixture);
    let tail = "--message-format=json-render-diagnostics -- -D warnings";

    assert_eq!(
        units(&inventory, &fixture, "planted/tests/selected.rs"),
        [format!(
            "clippy --locked --package planted --test selected --profile test {tail}"
        )],
    );
    assert_eq!(
        units(&inventory, &fixture, "planted/tests/guarded.rs"),
        [format!(
            "clippy --locked --package planted --test guarded --features planted/guarded \
                 --profile test {tail}"
        )],
    );
    assert_eq!(
        units(&inventory, &fixture, "planted/src/bin/tool.rs"),
        [format!(
            "clippy --locked --package planted --bin tool {tail}"
        )],
    );
}

/// Fixture data a test reads at run time is owned by no Cargo target, so
/// deleting it must select the coverage that can actually observe the
/// deletion rather than demand an owner that never existed.
#[test]
fn removing_rust_data_no_target_compiled_selects_its_package() {
    let fixture = crate::test_repository::Fixture::repository("removed-rust-data");
    let (inventory, package) = package_and_dependent_fixture(&fixture, &[]);
    let data = ["wire/tests/fixture/absence/bare.rs".to_owned()];

    let selected = cargo_check_selections(&inventory, fixture.path(), &data)
        .expect("removed uncompiled data selects its package");

    let [selection] = selected.as_slice() else {
        panic!("one package answers for its own data, found {selected:?}");
    };
    assert_eq!(selection.name, package.name);
    assert_eq!(selection.targets.len(), package.targets.len());
}

/// The same path still present is a source whose owner this scanner failed
/// to find, and that stays fail-closed.
#[test]
fn an_unowned_rust_file_still_on_disk_is_refused() {
    let fixture = crate::test_repository::Fixture::repository("unowned-rust-source");
    let (inventory, _) = package_and_dependent_fixture(&fixture, &[]);
    fixture.write("wire/tests/fixture/absence/bare.rs", b"pub struct Draft;\n");
    let present = ["wire/tests/fixture/absence/bare.rs".to_owned()];

    cargo_check_selections(&inventory, fixture.path(), &present)
        .expect_err("a present unowned Rust source is refused");

    let inaccessible = ["wire/tests/fixture/absence/bare.rs/child.rs".to_owned()];
    cargo_check_selections(&inventory, fixture.path(), &inaccessible)
        .expect_err("a filesystem error other than absence must remain a refusal");
}

/// A deleted package's paths are gone and outside every current package,
/// so they select nothing; a stray still on disk there keeps refusing.
#[test]
fn a_deleted_package_selects_nothing_but_a_present_stray_is_refused() {
    let fixture = crate::test_repository::Fixture::repository("deleted-package");
    let (inventory, _) = package_and_dependent_fixture(&fixture, &[]);
    let deleted = ["retired/Cargo.toml", "retired/src/lib.rs"].map(str::to_owned);

    let selected = cargo_check_selections(&inventory, fixture.path(), &deleted)
        .expect("a deleted package's paths are admitted");
    assert!(selected.is_empty(), "{selected:?}");

    fixture.write("retired/src/lib.rs", b"pub struct Stray;\n");
    cargo_check_selections(&inventory, fixture.path(), &deleted)
        .expect_err("a present source outside every package is refused");
}

/// Data that arrives, moves, or changes is present on disk, so absence
/// cannot classify it and the package's own declaration must.
#[test]
fn declared_data_on_disk_selects_the_package_that_reads_it() {
    let fixture = crate::test_repository::Fixture::repository("declared-rust-data");
    let (inventory, package) = package_and_dependent_fixture(&fixture, &["wire/tests/fixture"]);
    fixture.write("wire/tests/fixture/absence/bare.rs", b"pub struct Draft;\n");
    let added = ["wire/tests/fixture/absence/bare.rs".to_owned()];

    let selected = cargo_check_selections(&inventory, fixture.path(), &added)
        .expect("declared data selects its package");

    let [selection] = selected.as_slice() else {
        panic!("one package answers for its own data, found {selected:?}");
    };
    assert_eq!(selection.name, package.name);
    assert_eq!(selection.targets.len(), package.targets.len());
}

/// The declaration states what is not compiled, so it cannot be used to
/// hide source that is.
#[test]
fn a_data_declaration_covering_compiled_source_is_refused() {
    let fixture = crate::test_repository::Fixture::repository("data-over-source");
    fixture.write("wire/Cargo.toml", b"[package]\nname = \"wire\"\n");
    fixture.write("wire/src/lib.rs", b"pub mod absence;\n");
    fixture.write("wire/src/absence.rs", b"pub struct Draft;\n");
    let manifest = fixture.join("wire/Cargo.toml");

    package_inventory(
        serde_json::from_value(serde_json::json!({
            "id": "wire 0.0.0",
            "name": "wire",
            "edition": "2024",
            "manifest_path": manifest,
            "targets": [{
                "name": "wire",
                "kind": ["lib"],
                "test": true,
                "src_path": fixture.join("wire/src/lib.rs"),
            }],
            "metadata": {"spice": {"data": ["src"]}},
        }))
        .expect("fixture package metadata"),
        fixture.path(),
    )
    .expect_err("a data declaration over compiled source is refused");
}

/// The fact registry owns durable wire vocabulary, so changing it selects
/// the existing vocabulary suite without widening unrelated wire sources.
#[test]
fn the_fact_registry_selects_the_wire_vocabulary_proof_only() {
    let fixture = crate::test_repository::Fixture::repository("wire-fact-proof");
    let (mut inventory, _) = package_and_dependent_fixture(&fixture, &[]);
    let root = fs::canonicalize(fixture.path()).expect("canonical fixture root");
    let suite = TargetIdentity {
        package: "wire".to_owned(),
        kind: "test".to_owned(),
        name: "suite".to_owned(),
        required_features: Vec::new(),
        source: root.join("wire/tests/suite.rs"),
        declared_path_sources: Vec::new(),
        source_bytes: 1,
    };
    let wire = inventory
        .packages
        .iter_mut()
        .find(|package| package.name == "wire")
        .expect("wire package");
    wire.declared_targets.push(suite.clone());
    wire.targets.push(suite);

    let mut tests = BTreeSet::new();
    let mut edges = BTreeSet::new();
    select_durable_source_proofs(&inventory, "wire/src/fact.rs", &mut tests, &mut edges)
        .expect("the fact registry selects its durable proof");
    assert_eq!(tests, BTreeSet::from(["wire::test:suite".to_owned()]));
    assert_eq!(edges.len(), 1);

    let mut unrelated_tests = BTreeSet::new();
    let mut unrelated_edges = BTreeSet::new();
    select_durable_source_proofs(
        &inventory,
        "wire/src/lib.rs",
        &mut unrelated_tests,
        &mut unrelated_edges,
    )
    .expect("an unrelated source needs no supplemental proof");
    assert!(unrelated_tests.is_empty());
    assert!(unrelated_edges.is_empty());
}

/// A durable declaration names its proof targets by Cargo identity, so a
/// target the tree no longer declares refuses rather than selecting nothing.
#[test]
fn a_supplemental_proof_target_must_be_declared_by_the_tree() {
    const DECLARED: &[DeclaredTestTarget] = &[DeclaredTestTarget::new("wire", "test", "absence")];
    let fixture = crate::test_repository::Fixture::repository("supplemental-target");
    let (inventory, _) = package_and_dependent_fixture(&fixture, &[]);
    let declaration = |supplemental| DurableSourceSelection {
        source: "wire/src/environment.rs",
        owner: DeclaredTestTarget::new("wire", "lib", "wire"),
        supplemental,
    };

    validate_supplemental_targets(&inventory, &declaration(DECLARED))
        .expect("a declared test target is admitted");
    let error = validate_supplemental_targets(&inventory, &declaration(WIRE_PROOFS))
        .expect_err("an undeclared test target refuses");
    assert!(
        error.to_string().contains("wire::test:suite"),
        "the refusal names the missing target: {error}"
    );
}
