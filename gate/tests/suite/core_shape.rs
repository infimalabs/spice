use gate::{
    ActorCapture, ClaimCapture, CoreEvidence, CoreFinding, CoreObservation, CoreSource,
    CoreStanding, GateId, JudgedLatches, RepoShapeEvidence, Selector, acquire_core,
};

use super::support::{
    Tree, acquired, acquired_arm, captured_baselines, contract_repository, git_commit,
    git_repository, measured, policy_document, rim, selected_contract,
};

fn shape_contract_repository(label: &str) -> Tree {
    let repository = git_repository(label);
    repository.write(
        "spice.toml",
        &policy_document(concat!(
            "[policy]\npackage_roots = [\"src/sample\"]\n",
            "name_cluster_threshold = 3\n"
        )),
    );
    repository
}

/// The refusal one unusable shape input produces instead of a battery.
fn shape_refusal(repository: &Tree) -> String {
    let tree = Selector::staged()
        .materialize(&repository.root)
        .expect("materialize the captured index");
    let contract = selected_contract(tree.root());
    let baselines = captured_baselines(repository, &contract);
    let rim = rim(repository);
    let source = CoreSource::capture(
        &tree,
        &contract,
        ClaimCapture::Idle,
        ActorCapture::Absent,
        &JudgedLatches::default(),
        &rim,
    )
    .expect("capture one immutable core source");
    acquire_core(&source, &contract, &baselines)
        .expect_err("an unusable shape input must refuse rather than acquire")
        .to_string()
}

#[test]
fn repository_shape_skips_only_when_no_package_root_is_declared_or_derivable() {
    let repository = contract_repository("core-acquire-shape-skipped");
    let report = acquired(&repository, ClaimCapture::Idle, ActorCapture::Absent);
    let shape = acquired_arm(&report, GateId::RepoShape);

    assert_eq!(shape.standing(), CoreStanding::Skipped);
    let CoreObservation::Skipped { reason } = shape.observation() else {
        panic!("repository shape without package roots must skip");
    };
    assert!(
        reason.contains("no Python package roots") && reason.contains("derivable from"),
        "a skip must name both doors it found shut: {reason}"
    );

    repository.remove();
}

#[test]
fn repository_shape_skips_when_declared_roots_contain_no_modules() {
    let repository = shape_contract_repository("core-acquire-shape-no-modules");
    repository.write("src/sample/README.md", "# No Python here\n");
    git_commit(&repository, "non-applicable shape fixture");

    let report = acquired(&repository, ClaimCapture::Idle, ActorCapture::Absent);
    let shape = acquired_arm(&report, GateId::RepoShape);
    assert_eq!(shape.standing(), CoreStanding::Skipped);
    let CoreObservation::Skipped { .. } = shape.observation() else {
        panic!("a package root without admitted modules must skip");
    };

    repository.remove();
}

#[test]
fn repository_shape_measures_a_clean_declared_package() {
    let repository = shape_contract_repository("core-acquire-shape-clean");
    repository.write("src/sample/feature.py", "VALUE = 1\n");
    repository.write("tests/test_feature.py", "def test_feature():\n    pass\n");
    git_commit(&repository, "clean shape fixture");

    let report = acquired(&repository, ClaimCapture::Idle, ActorCapture::Absent);
    let shape = acquired_arm(&report, GateId::RepoShape);
    assert_eq!(
        shape.observation(),
        &measured(CoreEvidence::RepoShape(RepoShapeEvidence::new(Vec::new())))
    );
    assert_eq!(shape.standing(), CoreStanding::Passed);

    repository.remove();
}

#[test]
fn repository_shape_exempts_the_declared_generated_subtree() {
    let repository = git_repository("core-acquire-shape-generated");
    repository.write(
        "spice.toml",
        &policy_document(concat!(
            "[policy]\n",
            "package_roots = [\"src/sample\"]\n",
            "generated_paths = [\"src/sample/generated\"]\n",
        )),
    );
    repository.write("src/sample/feature.py", "VALUE = 1\n");
    repository.write("src/sample/generated/BadName.py", "GENERATED = True\n");
    git_commit(&repository, "generated shape fixture");

    let report = acquired(&repository, ClaimCapture::Idle, ActorCapture::Absent);
    let shape = acquired_arm(&report, GateId::RepoShape);
    assert_eq!(
        shape.observation(),
        &measured(CoreEvidence::RepoShape(RepoShapeEvidence::new(Vec::new())))
    );
    assert_eq!(shape.standing(), CoreStanding::Passed);

    repository.remove();
}

#[test]
fn repository_shape_reports_the_exact_canonical_violation_set() {
    let repository = shape_contract_repository("core-acquire-shape-violations");
    repository.write("src/sample/__init__.py", "");
    repository.write("src/sample/BadName.py", "");
    repository.write("src/sample/sharedalpha.py", "");
    repository.write("src/sample/sharedbeta.py", "");
    repository.write("src/sample/sharedgamma.py", "");
    repository.write("tests/BadName.py", "");
    git_commit(&repository, "violating shape fixture");

    let report = acquired(&repository, ClaimCapture::Idle, ActorCapture::Absent);
    let shape = acquired_arm(&report, GateId::RepoShape);
    let expected = RepoShapeEvidence::new(vec![
        CoreFinding::new(
            "namespace-package",
            Some("src/sample/__init__.py"),
            "__init__.py is forbidden under a declared namespace-package root",
        )
        .unwrap(),
        CoreFinding::new(
            "path-shape",
            Some("src/sample/BadName.py"),
            "file name violates the boundary-underscore module shape",
        )
        .unwrap(),
        CoreFinding::new(
            "path-shape",
            Some("tests/BadName.py"),
            "file name violates the boundary-underscore module shape",
        )
        .unwrap(),
        CoreFinding::new(
            "name-cluster-prefix",
            Some("src/sample"),
            concat!(
                "3 sibling modules share the prefix \"shared\" ",
                "(sharedalpha.py, sharedbeta.py, sharedgamma.py); ",
                "split the shared concept into a namespace subpackage"
            ),
        )
        .unwrap(),
    ]);
    assert_eq!(
        shape.observation(),
        &measured(CoreEvidence::RepoShape(expected))
    );
    assert_eq!(shape.standing(), CoreStanding::Failed);

    repository.remove();
}

#[test]
fn repository_shape_measures_a_package_root_only_pyproject_metadata_names() {
    let repository = git_repository("core-acquire-shape-derived");
    repository.write(
        "spice.toml",
        &policy_document("[policy]\nname_cluster_threshold = 3\n"),
    );
    repository.write("pyproject.toml", "[project]\nname = \"sample-app\"\n");
    repository.write("sample_app/feature.py", "VALUE = 1\n");
    repository.write("sample_app/BadName.py", "");
    git_commit(&repository, "pyproject-derived shape fixture");

    let report = acquired(&repository, ClaimCapture::Idle, ActorCapture::Absent);
    let shape = acquired_arm(&report, GateId::RepoShape);
    let expected = RepoShapeEvidence::new(vec![
        CoreFinding::new(
            "path-shape",
            Some("sample_app/BadName.py"),
            "file name violates the boundary-underscore module shape",
        )
        .unwrap(),
    ]);
    assert_eq!(
        shape.observation(),
        &measured(CoreEvidence::RepoShape(expected)),
        "a distribution name is a package-root declaration this arm must honour"
    );
    assert_eq!(shape.standing(), CoreStanding::Failed);

    repository.remove();
}

#[test]
fn repository_shape_derives_a_package_root_from_every_supported_backend() {
    // Every fixture holds a second `src/` package the backends never name, so a
    // backend this arm fails to read cannot pass by falling through to layout.
    let selected: &[&str] = &["src/sample_app/BadName.py"];
    let both: &[&str] = &["src/other_pkg/BadOther.py", "src/sample_app/BadName.py"];
    for (label, metadata, expected_paths) in [
        (
            "setuptools",
            concat!(
                "[tool.setuptools.packages.find]\n",
                "where = [\"src\"]\n",
                "include = [\"sample*\"]\n",
                "exclude = [\"sample_tests\"]\n",
            ),
            selected,
        ),
        (
            "poetry",
            concat!(
                "[tool.poetry]\n",
                "name = \"other-name\"\n",
                "[[tool.poetry.packages]]\n",
                "include = \"sample_app\"\n",
                "from = \"src\"\n",
            ),
            selected,
        ),
        (
            "hatch",
            "[tool.hatch.build.targets.wheel]\npackages = [\"src/sample_app\"]\n",
            selected,
        ),
        (
            "flit",
            "[tool.flit.module]\nname = \"sample-app\"\n",
            selected,
        ),
        (
            "pdm",
            "[tool.pdm.build]\nincludes = [\"src/sample_app\", \"README.md\"]\n",
            selected,
        ),
        (
            "src-layout",
            "[build-system]\nrequires = [\"hatchling\"]\n",
            both,
        ),
    ] {
        let repository = git_repository(&format!("core-acquire-shape-backend-{label}"));
        repository.write(
            "spice.toml",
            &policy_document("[policy]\nname_cluster_threshold = 3\n"),
        );
        repository.write("pyproject.toml", metadata);
        repository.write("src/sample_app/feature.py", "VALUE = 1\n");
        repository.write("src/sample_app/BadName.py", "");
        repository.write("src/other_pkg/BadOther.py", "");
        git_commit(&repository, "packaging backend shape fixture");

        let report = acquired(&repository, ClaimCapture::Idle, ActorCapture::Absent);
        let shape = acquired_arm(&report, GateId::RepoShape);
        let expected = RepoShapeEvidence::new(
            expected_paths
                .iter()
                .map(|path| {
                    CoreFinding::new(
                        "path-shape",
                        Some(*path),
                        "file name violates the boundary-underscore module shape",
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            shape.observation(),
            &measured(CoreEvidence::RepoShape(expected)),
            "the {label} backend names the package roots this arm must measure"
        );

        repository.remove();
    }
}

#[test]
fn repository_shape_refuses_packaging_metadata_it_cannot_read() {
    let repository = git_repository("core-acquire-shape-malformed");
    repository.write(
        "spice.toml",
        &policy_document("[policy]\nname_cluster_threshold = 3\n"),
    );
    repository.write("pyproject.toml", "[tool.setuptools]\npackages = 42\n");
    repository.write("sample_app/feature.py", "VALUE = 1\n");
    git_commit(&repository, "malformed packaging fixture");

    let refusal = shape_refusal(&repository);
    assert!(
        refusal.contains("[tool.setuptools].packages"),
        "a broken backend table must be named, not skipped past: {refusal}"
    );

    repository.remove();
}

#[test]
fn repository_shape_refuses_a_boundary_pattern_that_cannot_govern() {
    for (label, pattern) in [("blank", "''"), ("uncompilable", "'^[a-z'")] {
        let repository = git_repository(&format!("core-acquire-shape-pattern-{label}"));
        repository.write(
            "spice.toml",
            &policy_document(&format!(
                "[policy.package]\nboundary_underscore_pattern = {pattern}\n"
            )),
        );
        repository.write("pyproject.toml", "[project]\nname = \"sample-app\"\n");
        repository.write("sample_app/feature.py", "VALUE = 1\n");
        git_commit(&repository, "unusable boundary pattern fixture");

        let refusal = shape_refusal(&repository);
        assert!(
            refusal.contains("policy.package.boundary_underscore_pattern"),
            "a {label} pattern must refuse by name, not revert to the packaged one: {refusal}"
        );

        repository.remove();
    }
}

#[test]
fn repository_shape_scans_every_configured_test_root() {
    // Both configured roots carry an internal underscore, which the boundary
    // shape forbids, so a root scanned as a member of itself would indict its
    // own name. A test root is the thing being scanned, exactly as a package
    // root is, and neither is one of its own findings.
    let repository = git_repository("core-acquire-shape-test-roots");
    repository.write(
        "spice.toml",
        &policy_document(concat!(
            "[policy]\n",
            "package_roots = [\"src/sample\"]\n",
            "test_paths = [\"check_suite\", \"suites/*\"]\n",
        )),
    );
    repository.write("src/sample/feature.py", "VALUE = 1\n");
    repository.write(
        "check_suite/test_feature.py",
        "def test_feature():\n    pass\n",
    );
    repository.write("check_suite/BadName.py", "");
    repository.write(
        "suites/deep_unit/test_unit.py",
        "def test_unit():\n    pass\n",
    );
    repository.write("suites/deep_unit/BadName.py", "");
    repository.write("tests/BadName.py", "");
    git_commit(&repository, "configured test root fixture");

    let report = acquired(&repository, ClaimCapture::Idle, ActorCapture::Absent);
    let shape = acquired_arm(&report, GateId::RepoShape);
    let expected = RepoShapeEvidence::new(vec![
        CoreFinding::new(
            "path-shape",
            Some("check_suite/BadName.py"),
            "file name violates the boundary-underscore module shape",
        )
        .unwrap(),
        CoreFinding::new(
            "path-shape",
            Some("suites/deep_unit/BadName.py"),
            "file name violates the boundary-underscore module shape",
        )
        .unwrap(),
    ]);
    assert_eq!(
        shape.observation(),
        &measured(CoreEvidence::RepoShape(expected)),
        concat!(
            "configured test roots, including glob-selected ones, are scanned, ",
            "the literal tests/ this repository never configured is not, ",
            "and no configured root is indicted for its own name",
        )
    );
    assert_eq!(shape.standing(), CoreStanding::Failed);

    repository.remove();
}

#[test]
fn repository_shape_holds_names_to_the_tracked_boundary_pattern() {
    let repository = git_repository("core-acquire-shape-boundary");
    repository.write(
        "spice.toml",
        &policy_document(concat!(
            "[policy]\n",
            "package_roots = [\"src/sample\"]\n",
            "name_cluster_threshold = 3\n",
            "[policy.package]\n",
            "boundary_underscore_pattern = '^[a-z]+$'\n",
        )),
    );
    repository.write("src/sample/sharedalpha.py", "");
    repository.write("src/sample/sharedbeta.py", "");
    repository.write("src/sample/sharedgamma2.py", "");
    git_commit(&repository, "tracked boundary pattern fixture");

    let report = acquired(&repository, ClaimCapture::Idle, ActorCapture::Absent);
    let shape = acquired_arm(&report, GateId::RepoShape);
    let expected = RepoShapeEvidence::new(vec![
        CoreFinding::new(
            "path-shape",
            Some("src/sample/sharedgamma2.py"),
            "file name violates the boundary-underscore module shape",
        )
        .unwrap(),
    ]);
    assert_eq!(
        shape.observation(),
        &measured(CoreEvidence::RepoShape(expected)),
        concat!(
            "the packaged pattern admits digits, so it would have called this a ",
            "generic split inside a three-member prefix cluster; the tracked ",
            "pattern governs both scans instead",
        )
    );
    assert_eq!(shape.standing(), CoreStanding::Failed);

    repository.remove();
}

#[test]
fn repository_shape_exempts_nested_generated_globs_and_nothing_else() {
    let repository = git_repository("core-acquire-shape-generated-glob");
    repository.write(
        "spice.toml",
        &policy_document(concat!(
            "[policy]\n",
            "package_roots = [\"src/sample\"]\n",
            "generated_paths = [\"**/generated/*.py\", \"!src/sample/BadName.py\"]\n",
        )),
    );
    repository.write("src/sample/feature.py", "VALUE = 1\n");
    repository.write("src/sample/generated/BadName.py", "GENERATED = True\n");
    repository.write("src/sample/BadName.py", "");
    git_commit(&repository, "nested generated shape fixture");

    let report = acquired(&repository, ClaimCapture::Idle, ActorCapture::Absent);
    let shape = acquired_arm(&report, GateId::RepoShape);
    let expected = RepoShapeEvidence::new(vec![
        CoreFinding::new(
            "path-shape",
            Some("src/sample/BadName.py"),
            "file name violates the boundary-underscore module shape",
        )
        .unwrap(),
    ]);
    assert_eq!(
        shape.observation(),
        &measured(CoreEvidence::RepoShape(expected)),
        concat!(
            "a nested glob exempts what it matches, and a leading '!' is an ",
            "ordinary path character here rather than an exemption of its own",
        )
    );
    assert_eq!(shape.standing(), CoreStanding::Failed);

    repository.remove();
}
