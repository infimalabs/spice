use std::fs;

use gate::{
    AnalysisEvidence, GateId, HYGIENE_ARMS, HYGIENE_SOURCE_FILE_BYTE_LIMIT, HygieneConfiguration,
    HygieneMeasurement, HygieneOutcome, HygieneProvenance, HygieneReport, HygieneSource,
    Jurisdiction, Selector, SuppressedPath, acquire_hygiene,
};

use super::support::{Tree, WORKSPACE_POLICY, git_commit, git_repository, selected_contract};

#[test]
fn one_captured_source_walks_the_same_jurisdiction_and_measures_every_arm() {
    let repository = hygiene_repository("hygiene-acquire-parity", false);
    // Two digests agree because both walks applied the same rules only when a
    // path is subject to one. This module is excluded by the tracked policy and
    // by nothing compiled, so a capture that dropped the repository's own rules
    // would admit it and digest differently.
    repository.write("third_party/copied.py", "def copied():\n    return 1\n");
    repository.write(
        "spice.toml",
        &format!("{WORKSPACE_POLICY}\n[gate.walk]\nskip = [\"third_party/copied.py\"]\n",),
    );
    git_commit(
        &repository,
        "hygiene parity fixture with a tracked exclusion",
    );
    let tree = Selector::staged()
        .materialize(&repository.root)
        .expect("materialize the captured hygiene source");
    let contract = selected_contract(tree.root());

    let prior_jurisdiction = Jurisdiction::walk(tree.root(), contract.policy().ignore())
        .expect("walk the existing hygiene fixture jurisdiction");
    let source = HygieneSource::capture(&tree, &contract).expect("capture one hygiene source");
    let repeated = HygieneSource::capture(&tree, &contract).expect("repeat hygiene capture");
    let acquired =
        acquire_hygiene(&source, &configuration()).expect("acquire the complete hygiene family");

    let excluded = prior_jurisdiction
        .entries()
        .iter()
        .find(|entry| entry.path() == "third_party/copied.py")
        .expect("the tracked exclusion stays in the total inventory");
    assert!(
        !excluded.admission().admits(),
        "the parity fixture proves nothing about rules unless the tracked skip excludes a path"
    );
    assert_eq!(source.root(), tree.root());
    assert_eq!(source.digest(), repeated.digest());
    assert_eq!(
        acquired
            .arms
            .iter()
            .map(|measurement| measurement.gate_id)
            .collect::<Vec<_>>(),
        HYGIENE_ARMS
            .iter()
            .map(|declaration| declaration.id())
            .collect::<Vec<_>>()
    );
    assert!(acquired.arms.iter().all(|measurement| {
        measurement.evidence.iter().any(|evidence| {
            matches!(
                evidence,
                AnalysisEvidence::Analyzer(_) | AnalysisEvidence::LanguageApplicability { .. }
            )
        })
    }));

    repository.remove();
}

#[test]
fn compiled_exclusions_are_visible_in_the_acquired_evidence() {
    let repository = hygiene_repository("hygiene-acquire-exclusions", true);
    let tree = Selector::staged()
        .materialize(&repository.root)
        .expect("materialize the captured hygiene source");
    let contract = selected_contract(tree.root());
    let source = HygieneSource::capture(&tree, &contract).expect("capture one hygiene source");

    let ignored = source
        .jurisdiction()
        .entries()
        .iter()
        .find(|entry| entry.path() == ".venv/broken.py")
        .expect("compiled exclusion remains in the total inventory");
    assert!(!ignored.admission().admits());

    let report = acquire_hygiene(&source, &configuration())
        .expect("ignored malformed source cannot fail acquisition");

    for (measurement, declaration) in report.arms.iter().zip(HYGIENE_ARMS) {
        assert_eq!(measurement.roots, declaration.jurisdiction().roots());
        assert_eq!(measurement.includes, declaration.jurisdiction().includes());
        assert_eq!(measurement.excludes, declaration.jurisdiction().excludes());
        assert_ne!(measurement.outcome, HygieneOutcome::ParseFailure);
    }
    assert_eq!(
        suppressed_paths(arm(&report, GateId::AssertionFreeTests)),
        Some(
            [SuppressedPath {
                path: ".venv/broken.py".to_owned(),
                shape: "file".to_owned(),
                rule: ".venv/**".to_owned(),
                provenance: HygieneProvenance {
                    release: "current".to_owned(),
                    layer: "compiled".to_owned(),
                    document: "<compiled-gate-battery>".to_owned(),
                    field: "gate::hygiene::HygieneJurisdiction::excludes".to_owned(),
                },
            }]
            .as_slice()
        ),
        "a compiled arm exclusion is retained by the layer that shipped it"
    );

    repository.remove();
}

#[test]
fn a_tracked_walk_skip_suppresses_its_candidate_while_retaining_where_it_came_from() {
    let measured = suppression_repository("hygiene-acquire-suppression-measured", &[]);
    let suppressed = suppression_repository(
        "hygiene-acquire-suppression",
        &[
            "tests/test_ignored.py",
            "app/_ignored.py",
            "notes.txt",
            "app/generated.py",
        ],
    );

    let measured_report = acquired_report(&measured);
    let measured_arm = arm(&measured_report, GateId::AssertionFreeTests);
    assert!(
        measured_arm
            .findings
            .iter()
            .any(|finding| finding.path == "tests/test_ignored.py"),
        "the candidate must be measurable before a skip can be shown to suppress it: {:?}",
        measured_arm.findings
    );
    assert_eq!(suppressed_paths(measured_arm), None);

    let report = acquired_report(&suppressed);
    let measurement = arm(&report, GateId::AssertionFreeTests);
    assert_eq!(measurement.outcome, HygieneOutcome::Findings);
    assert!(
        !measurement
            .findings
            .iter()
            .any(|finding| finding.path == "tests/test_ignored.py"),
        "the skip must remove the candidate: {:?}",
        measurement.findings
    );
    assert_eq!(
        suppressed_paths(measurement),
        Some(
            vec![
                SuppressedPath {
                    path: "app/_ignored.py".to_owned(),
                    shape: "file".to_owned(),
                    rule: "app/_ignored.py".to_owned(),
                    provenance: tracked_walk_provenance(1),
                },
                SuppressedPath {
                    path: "app/generated.py".to_owned(),
                    shape: "directory".to_owned(),
                    rule: "app/generated.py".to_owned(),
                    provenance: tracked_walk_provenance(3),
                },
                SuppressedPath {
                    path: "tests/test_ignored.py".to_owned(),
                    shape: "file".to_owned(),
                    rule: "tests/test_ignored.py".to_owned(),
                    provenance: tracked_walk_provenance(0),
                },
            ]
            .as_slice()
        ),
        "the skipped non-Python file is absent and the skipped directory is \
         present under the shape that says so"
    );

    let decoded: HygieneReport = serde_json::from_slice(&report.canonical_json())
        .expect("a report carrying suppressions decodes");
    assert_eq!(decoded, report);
    assert!(
        !measurement
            .evidence
            .iter()
            .any(|evidence| matches!(evidence, AnalysisEvidence::Exclusion(_))),
        "a suppressed path must not retire the arm that still measured the rest"
    );

    suppressed.remove();
    measured.remove();
}

#[test]
fn a_directory_skip_names_the_subtree_it_closed_and_the_layer_that_closed_it() {
    let measured = directory_skip_repository("hygiene-acquire-subtree-measured", false);
    let closed = directory_skip_repository("hygiene-acquire-subtree", true);

    let measured_report = acquired_report(&measured);
    let measured_arm = arm(&measured_report, GateId::AssertionFreeTests);
    assert!(
        measured_arm
            .findings
            .iter()
            .any(|finding| finding.path == "tests/support/test_hidden.py"),
        "the subtree must hold a measured finding before a skip can be shown to close it: {:?}",
        measured_arm.findings
    );
    assert_eq!(
        suppressed_paths(measured_arm),
        Some([packaged_target_suppression()].as_slice()),
        "the packaged default closes the tracked target/ subtree with no tracked rule in play"
    );

    let report = acquired_report(&closed);
    let measurement = arm(&report, GateId::AssertionFreeTests);
    assert_eq!(measurement.outcome, HygieneOutcome::Findings);
    assert!(
        !measurement
            .findings
            .iter()
            .any(|finding| finding.path.starts_with("tests/support/")),
        "the directory skip must remove every candidate beneath it: {:?}",
        measurement.findings
    );
    assert_eq!(
        suppressed_paths(measurement),
        Some(
            [
                packaged_target_suppression(),
                SuppressedPath {
                    path: "tests/support".to_owned(),
                    shape: "directory".to_owned(),
                    rule: "tests/support".to_owned(),
                    provenance: tracked_walk_provenance(0),
                },
            ]
            .as_slice()
        ),
        "a pruned directory is evidence naming itself, its rule, and its layer"
    );
    assert!(
        !measurement
            .evidence
            .iter()
            .any(|evidence| matches!(evidence, AnalysisEvidence::Exclusion(_))),
        "closing a subtree must not retire the arm that still measured the rest"
    );

    closed.remove();
    measured.remove();
}

#[test]
fn source_metadata_refuses_oversized_python_before_allocation() {
    let repository = hygiene_repository("hygiene-source-bound", false);
    fs::File::create(repository.path("app/oversized.py"))
        .and_then(|file| file.set_len(HYGIENE_SOURCE_FILE_BYTE_LIMIT + 1))
        .expect("create sparse oversized Python source");
    git_commit(&repository, "oversized Python source");
    let tree = Selector::staged()
        .materialize(&repository.root)
        .expect("materialize source");
    let contract = selected_contract(tree.root());
    HygieneSource::capture(&tree, &contract).expect_err("oversized source refuses");
    repository.remove();
}

/// Acquire one complete family from a fixture repository, as production does.
fn acquired_report(repository: &Tree) -> HygieneReport {
    let tree = Selector::staged()
        .materialize(&repository.root)
        .expect("materialize the captured hygiene source");
    let contract = selected_contract(tree.root());
    let source = HygieneSource::capture(&tree, &contract).expect("capture one hygiene source");
    acquire_hygiene(&source, &configuration()).expect("acquire the complete hygiene family")
}

fn arm(report: &HygieneReport, gate_id: GateId) -> &HygieneMeasurement {
    report
        .arms
        .iter()
        .find(|measurement| measurement.gate_id == gate_id)
        .unwrap_or_else(|| panic!("{} is one of the five arms", gate_id.as_str()))
}

fn suppressed_paths(measurement: &HygieneMeasurement) -> Option<&[SuppressedPath]> {
    measurement
        .evidence
        .iter()
        .find_map(|evidence| match evidence {
            AnalysisEvidence::PathSuppression { suppressed } => Some(suppressed.as_slice()),
            _ => None,
        })
}

fn tracked_walk_provenance(index: usize) -> HygieneProvenance {
    HygieneProvenance {
        release: "current".to_owned(),
        layer: "repository".to_owned(),
        document: "spice.toml".to_owned(),
        field: format!("gate.walk.skip[{index}]"),
    }
}

/// A repository whose skippable paths are skipped or not.
///
/// Every path exists either way, so the only difference between the two
/// fixtures is the tracked `gate.walk.skip` list under test. Alongside the two
/// Python candidates it holds a non-Python file, which is no candidate and so
/// never a suppression, and a directory whose name ends in `.py`, which is the
/// case that makes the shape load-bearing: skipped, it is a subtree the walk
/// never entered rather than the Python file its name reads as, and only the
/// shape says which.
fn suppression_repository(label: &str, skip: &[&str]) -> Tree {
    let repository = hygiene_repository(label, false);
    repository.write("tests/test_ignored.py", "def test_ignored():\n    pass\n");
    repository.write("app/_ignored.py", "def _ignored():\n    return 1\n");
    repository.write("notes.txt", "not a hygiene candidate\n");
    repository.write("app/generated.py/emitted.txt", "generated output\n");
    if !skip.is_empty() {
        let entries = skip
            .iter()
            .map(|value| format!("\"{value}\""))
            .collect::<Vec<_>>()
            .join(", ");
        repository.write(
            "spice.toml",
            &format!("{WORKSPACE_POLICY}\n[gate.walk]\nskip = [{entries}]\n",),
        );
    }
    git_commit(&repository, "hygiene suppression fixture");
    repository
}

/// A repository whose candidate lives one directory below a skippable root.
///
/// The candidate is never named by a rule; only the directory holding it is, so
/// the skipped fixture can only account for the loss by reporting the subtree.
/// Both fixtures track a file under `target/`, which packaged walk policy
/// prunes. That is the only way a packaged default fires on a materialized
/// tree: the tree is a private copy of tracked content and holds nothing a
/// build or checkout leaves behind, and the other packaged rule names `.git/`,
/// which Git will not track. Tracking under `target/` is unusual, and that is
/// the point: it puts the packaged layer under a real acquisition instead of
/// under a hand-built rule list.
fn directory_skip_repository(label: &str, skip: bool) -> Tree {
    let repository = hygiene_repository(label, false);
    repository.write(
        "tests/support/test_hidden.py",
        "def test_hidden():\n    pass\n",
    );
    repository.write("target/staged.py", "def staged():\n    return 1\n");
    if skip {
        repository.write(
            "spice.toml",
            &format!("{WORKSPACE_POLICY}\n[gate.walk]\nskip = [\"tests/support\"]\n",),
        );
    }
    git_commit(&repository, "hygiene subtree suppression fixture");
    repository
}

/// The packaged default that closes a tracked `target/` subtree.
fn packaged_target_suppression() -> SuppressedPath {
    SuppressedPath {
        path: "target".to_owned(),
        shape: "directory".to_owned(),
        rule: "target/".to_owned(),
        provenance: HygieneProvenance {
            release: "current".to_owned(),
            layer: "packaged".to_owned(),
            document: gate::PACKAGED_POLICY_SOURCE.to_owned(),
            field: "gate.walk.skip[1]".to_owned(),
        },
    }
}

fn configuration() -> HygieneConfiguration {
    HygieneConfiguration::new(["app/main.py"], ["tests"]).expect("fixture roots")
}

fn hygiene_repository(label: &str, excluded_broken_source: bool) -> Tree {
    let repository = git_repository(label);
    repository.write("spice.toml", WORKSPACE_POLICY);
    repository.write("pyproject.toml", "[project]\nname = 'fixture'\n");
    repository.write("app/__init__.py", "from .main import entry\n");
    repository.write(
        "app/main.py",
        "from app.used import live\n\ndef entry():\n    return live()\n",
    );
    repository.write(
        "app/used.py",
        concat!(
            "def live():\n    return 1\n\n",
            "def test_only():\n    return 2\n\n",
            "def orphan():\n    return 3\n",
        ),
    );
    repository.write("app/_internal.py", "def _secret():\n    return 1\n");
    repository.write(
        "tests/test_main.py",
        concat!(
            "import app._internal as hidden\n",
            "from app.used import live, test_only\n\n",
            "def test_live():\n    assert live() == 1\n",
            "    assert test_only() == 2\n",
            "    assert hidden._secret() == 1\n\n",
            "def test_no_assert():\n    live()\n",
        ),
    );
    if excluded_broken_source {
        repository.write(".venv/broken.py", "def broken(:\n");
    }
    git_commit(&repository, "hygiene fixture");
    repository
}
