//! Policy-selected test-engine dispatch over one immutable selected source.

use std::io::Write;
use std::path::Path;

use gate::{TestSelectionReport, WorkspaceTestBudget, WorkspaceTestRoots};
use sill::Environment;

use super::{CargoBudget, ChangeGateError};

pub(super) enum TestExecution {
    Packaged {
        engines: Vec<crate::config::TestEngine>,
    },
    CurrentSourceCargo,
}

impl TestExecution {
    pub(super) fn cargo_selected(&self, root: &Path, changes: &[String]) -> bool {
        match self {
            Self::CurrentSourceCargo => gate::cargo_workspace_tests_applicable(root, changes),
            Self::Packaged { engines } => {
                engines.contains(&crate::config::TestEngine::CargoWorkspace)
                    && gate::cargo_workspace_tests_applicable(root, changes)
            }
        }
    }
}

pub(super) struct TestRun<'a> {
    selected_root: &'a Path,
    roots: WorkspaceTestRoots<'a>,
    source: wire::Digest,
    changes: &'a [String],
    budget: CargoBudget,
    environment: Environment,
}

impl<'a> TestRun<'a> {
    pub(super) fn new(
        selected_root: &'a Path,
        writable_root: &'a Path,
        source: wire::Digest,
        changes: &'a [String],
        budget: CargoBudget,
        environment: Environment,
    ) -> Self {
        Self {
            selected_root,
            roots: WorkspaceTestRoots::new(selected_root, writable_root),
            source,
            changes,
            budget,
            environment,
        }
    }

    pub(super) fn with_reusable(mut self, workspace: &'a gate::RustAnalysisWorkspace) -> Self {
        self.roots = self.roots.with_reusable(workspace);
        self
    }

    pub(super) fn with_base(mut self, base: &'a Path) -> Self {
        self.roots = self.roots.with_base(base);
        self
    }
}

pub(super) fn run_selected_tests(
    run: TestRun<'_>,
    execution: TestExecution,
    progress: &mut impl Write,
) -> Result<TestSelectionReport, ChangeGateError> {
    let cargo_selected = execution.cargo_selected(run.selected_root, run.changes);
    if !cargo_selected {
        return TestSelectionReport::complete_without_engines(run.source, run.changes.to_vec())
            .map_err(|error| ChangeGateError::protocol("test-selection-invalid", error));
    }

    let environment = run.budget.bind_test_environment(run.environment)?;
    let budget = WorkspaceTestBudget::new(
        run.budget.test_concurrency(),
        run.budget.shard_jobs(),
        run.budget.test_threads(),
    )
    .map_err(|error| ChangeGateError::refusal("change-gate-tests-refused", error))?;
    gate::run_selected_workspace_tests_with_budget_at(
        run.roots,
        run.source,
        run.changes,
        environment,
        budget,
        progress,
    )
    .map_err(|error| ChangeGateError::refusal("change-gate-tests-refused", error))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    #[test]
    fn generic_repository_completes_without_resolving_cargo() {
        let root = fixture_root("generic-zero-engine");
        fs::create_dir_all(&root).expect("create generic repository fixture");
        let source = wire::Digest::compute(wire::Domain::GateTree, b"generic source");
        let changes = vec!["README.md".to_owned(), "assets/icon.svg".to_owned()];
        let environment =
            Environment::new([(OsString::from("PATH"), root.clone().into_os_string())])
                .expect("minimal environment");
        let mut progress = Vec::new();

        let report = run_selected_tests(
            TestRun::new(
                &root,
                &root,
                source.clone(),
                &changes,
                budget(),
                environment,
            ),
            TestExecution::Packaged {
                engines: Vec::new(),
            },
            &mut progress,
        )
        .expect("zero-engine repository needs no Cargo executable");

        assert_eq!(report.source(), &source);
        assert_eq!(report.changed_sources(), changes);
        assert!(report.engines().is_empty());
        assert!(report.tests().is_empty());
        assert!(report.edges().is_empty());
        assert!(report.admits());
        assert!(progress.is_empty());
        fs::remove_dir_all(root).expect("remove generic repository fixture");
    }

    #[test]
    fn declared_cargo_engine_is_skipped_for_an_irrelevant_scope() {
        let root = fixture_root("cargo-irrelevant-scope");
        fs::create_dir_all(&root).expect("create Cargo repository fixture");
        fs::write(root.join("Cargo.toml"), "[workspace]\nresolver = \"2\"\n")
            .expect("write Cargo workspace authority");
        let environment =
            Environment::new([(OsString::from("PATH"), root.clone().into_os_string())])
                .expect("minimal environment");
        let mut progress = Vec::new();

        let report = run_selected_tests(
            TestRun::new(
                &root,
                &root,
                wire::Digest::compute(wire::Domain::GateTree, b"documentation-only source"),
                &["README.md".to_owned()],
                budget(),
                environment,
            ),
            TestExecution::Packaged {
                engines: vec![crate::config::TestEngine::CargoWorkspace],
            },
            &mut progress,
        )
        .expect("irrelevant Cargo scope needs no Cargo executable");

        assert!(report.engines().is_empty());
        assert!(report.admits());
        assert!(progress.is_empty());
        fs::remove_dir_all(root).expect("remove Cargo repository fixture");
    }

    #[test]
    fn current_source_surface_skips_cargo_for_an_irrelevant_slice() {
        let root = fixture_root("current-source-cargo");
        fs::create_dir_all(&root).expect("create current-source fixture");
        let environment =
            Environment::new([(OsString::from("PATH"), root.clone().into_os_string())])
                .expect("minimal environment");

        let report = run_selected_tests(
            TestRun::new(
                &root,
                &root,
                wire::Digest::compute(wire::Domain::GateTree, b"current-source documentation"),
                &["README.md".to_owned()],
                budget(),
                environment,
            ),
            TestExecution::CurrentSourceCargo,
            &mut Vec::new(),
        )
        .expect("an irrelevant current-source slice needs no Cargo executable");

        assert!(report.engines().is_empty());
        assert!(report.tests().is_empty());
        assert!(report.admits());
        fs::remove_dir_all(root).expect("remove current-source fixture");
    }

    fn fixture_root(name: &str) -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "spice-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ))
    }

    fn budget() -> CargoBudget {
        CargoBudget::from_capacity(4, 4)
    }
}
