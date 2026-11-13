//! Typed pre-execution test-selection evidence.
//!
//! Test selection retains its conservative source-to-target reach report.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};
use wire::Digest;

/// Why coverage cannot establish a clean result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "state")]
pub enum CoverageCompleteness {
    /// Every known suite test published all of its covered source points.
    Complete,
    /// The execution published some coverage but proved its denominator was incomplete.
    Incomplete { reason: String },
    /// The execution could not establish any authoritative coverage denominator.
    Unknown { reason: String },
}

/// One exact conservative build-source-to-test reach edge used for hook selection.
///
/// The edge says canonical Cargo metadata proves that the named test target
/// directly compiles the source through its declared target root. It is not
/// runtime line coverage; completeness applies only to this conservative
/// selection boundary.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestSelectionEdge {
    source: String,
    test: String,
}

impl TestSelectionEdge {
    /// Bind one changed build source to one selected package-qualified test target.
    pub fn new(
        source: impl Into<String>,
        test: impl Into<String>,
    ) -> Result<Self, TestEvidenceError> {
        let source = source.into();
        let test = test.into();
        require_path("selected build source", &source)?;
        require_text("selected test identity", &test)?;
        Ok(Self { source, test })
    }

    /// Exact repository-relative changed build source.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Exact package-qualified Cargo test-target identity.
    #[must_use]
    pub fn test(&self) -> &str {
        &self.test
    }
}

/// Version of the staged-source test-selection report.
pub const TEST_SELECTION_REPORT_VERSION: u64 = 1;

const CARGO_WORKSPACE_ENGINE: &str = "cargo-workspace";

/// Typed staged-source plan and its exact source-to-test provenance.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestSelectionReport {
    version: u64,
    source: Digest,
    changed_sources: Vec<String>,
    engines: Vec<String>,
    tests: Vec<String>,
    edges: Vec<TestSelectionEdge>,
    coverage: CoverageCompleteness,
}

impl TestSelectionReport {
    /// Publish one canonical hook selection.
    ///
    /// Complete selection requires every changed source and every selected
    /// test to participate in at least one exact edge. Unknown or incomplete
    /// selection remains encodable but never admits execution as sufficient.
    pub fn new(
        source: Digest,
        changed_sources: Vec<String>,
        tests: Vec<String>,
        edges: Vec<TestSelectionEdge>,
        coverage: CoverageCompleteness,
    ) -> Result<Self, TestEvidenceError> {
        Self::new_with_engines(
            source,
            changed_sources,
            vec![CARGO_WORKSPACE_ENGINE.to_owned()],
            tests,
            edges,
            coverage,
        )
    }

    fn new_with_engines(
        source: Digest,
        mut changed_sources: Vec<String>,
        mut engines: Vec<String>,
        mut tests: Vec<String>,
        mut edges: Vec<TestSelectionEdge>,
        coverage: CoverageCompleteness,
    ) -> Result<Self, TestEvidenceError> {
        for path in &changed_sources {
            require_path("changed build source", path)?;
        }
        changed_sources.sort();
        if let Some(path) = repeated(&changed_sources) {
            return Err(TestEvidenceError::new(format!(
                "changed build source {path:?} is repeated"
            )));
        }
        for engine in &engines {
            require_text("selected test engine", engine)?;
            if engine != CARGO_WORKSPACE_ENGINE {
                return Err(TestEvidenceError::new(format!(
                    "unsupported selected test engine {engine:?}"
                )));
            }
        }
        engines.sort();
        if let Some(engine) = repeated(&engines) {
            return Err(TestEvidenceError::new(format!(
                "selected test engine {engine:?} is repeated"
            )));
        }
        for test in &tests {
            require_text("selected test identity", test)?;
        }
        tests.sort();
        if let Some(test) = repeated(&tests) {
            return Err(TestEvidenceError::new(format!(
                "selected test identity {test:?} is repeated"
            )));
        }
        edges.sort();
        if let Some(pair) = edges.windows(2).find(|pair| pair[0] == pair[1]) {
            let edge = &pair[0];
            return Err(TestEvidenceError::new(format!(
                "source-to-test selection edge {:?} -> {:?} is repeated",
                edge.source, edge.test
            )));
        }
        let sources = changed_sources
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let selected = tests.iter().map(String::as_str).collect::<BTreeSet<_>>();
        for edge in &edges {
            if !sources.contains(edge.source.as_str()) {
                return Err(TestEvidenceError::new(format!(
                    "selection edge names unchanged source {:?}",
                    edge.source
                )));
            }
            if !selected.contains(edge.test.as_str()) {
                return Err(TestEvidenceError::new(format!(
                    "selection edge names unselected test {:?}",
                    edge.test
                )));
            }
        }
        if engines.is_empty() && (!tests.is_empty() || !edges.is_empty()) {
            return Err(TestEvidenceError::new(
                "zero-engine test selection cannot name tests or reach edges",
            ));
        }
        if matches!(coverage, CoverageCompleteness::Complete) && !engines.is_empty() {
            if let Some(source) = changed_sources
                .iter()
                .find(|source| !edges.iter().any(|edge| edge.source == **source))
            {
                return Err(TestEvidenceError::new(format!(
                    "complete test selection has no reaching test for changed source {source:?}"
                )));
            }
            if let Some(test) = tests
                .iter()
                .find(|test| !edges.iter().any(|edge| edge.test == **test))
            {
                return Err(TestEvidenceError::new(format!(
                    "complete test selection has no changed-source provenance for test {test:?}"
                )));
            }
        }
        Ok(Self {
            version: TEST_SELECTION_REPORT_VERSION,
            source,
            changed_sources,
            engines,
            tests,
            edges,
            coverage,
        })
    }

    /// Publish explicit complete evidence that policy selected no test engine.
    pub fn complete_without_engines(
        source: Digest,
        changed_sources: Vec<String>,
    ) -> Result<Self, TestEvidenceError> {
        Self::new_with_engines(
            source,
            changed_sources,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            CoverageCompleteness::Complete,
        )
    }

    /// Current report schema.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Exact selected-tree identity.
    #[must_use]
    pub const fn source(&self) -> &Digest {
        &self.source
    }

    /// Sorted changed sources considered by the selected engines.
    #[must_use]
    pub fn changed_sources(&self) -> &[String] {
        &self.changed_sources
    }

    /// Sorted packaged test engines selected for this source.
    #[must_use]
    pub fn engines(&self) -> &[String] {
        &self.engines
    }

    /// Sorted package-qualified test targets selected for execution.
    #[must_use]
    pub fn tests(&self) -> &[String] {
        &self.tests
    }

    /// Exact conservative source-to-test reach edges.
    #[must_use]
    pub fn edges(&self) -> &[TestSelectionEdge] {
        &self.edges
    }

    /// Whether the complete source-to-test denominator was proven.
    #[must_use]
    pub const fn coverage(&self) -> &CoverageCompleteness {
        &self.coverage
    }

    /// Only complete selection evidence admits the narrowed test execution.
    #[must_use]
    pub const fn admits(&self) -> bool {
        matches!(self.coverage, CoverageCompleteness::Complete)
    }

    /// Encode the stable machine-readable hook report.
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Decode and validate one hook report from this schema.
    pub fn decode(bytes: &[u8]) -> Result<Self, TestEvidenceError> {
        let report: Self = serde_json::from_slice(bytes).map_err(|error| {
            TestEvidenceError::new(format!("invalid test selection report: {error}"))
        })?;
        if report.version != TEST_SELECTION_REPORT_VERSION {
            return Err(TestEvidenceError::new(format!(
                "unsupported test selection report version {}",
                report.version
            )));
        }
        Self::new_with_engines(
            report.source,
            report.changed_sources,
            report.engines,
            report.tests,
            report.edges,
            report.coverage,
        )
    }
}

impl CoverageCompleteness {
    /// Declare incomplete coverage with one exact reason.
    pub fn incomplete(reason: impl Into<String>) -> Result<Self, TestEvidenceError> {
        let reason = reason.into();
        require_text("incomplete coverage reason", &reason)?;
        Ok(Self::Incomplete { reason })
    }

    /// Declare unknown coverage with one exact reason.
    pub fn unknown(reason: impl Into<String>) -> Result<Self, TestEvidenceError> {
        let reason = reason.into();
        require_text("unknown coverage reason", &reason)?;
        Ok(Self::Unknown { reason })
    }
}

/// A malformed test-selection authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TestEvidenceError {
    detail: String,
}

impl TestEvidenceError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }

    /// Exact refusal detail.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for TestEvidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl Error for TestEvidenceError {}

fn repeated(values: &[String]) -> Option<&str> {
    values
        .windows(2)
        .find_map(|pair| (pair[0] == pair[1]).then_some(pair[0].as_str()))
}

fn require_text(field: &str, value: &str) -> Result<(), TestEvidenceError> {
    if value.trim().is_empty() || value.contains(['\0', '\r', '\n']) {
        Err(TestEvidenceError::new(format!(
            "{field} must be nonempty single-line text"
        )))
    } else {
        Ok(())
    }
}

fn require_path(field: &str, value: &str) -> Result<(), TestEvidenceError> {
    require_text(field, value)?;
    let invalid = value.starts_with('/')
        || value.contains('\\')
        || value
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."));
    if invalid {
        Err(TestEvidenceError::new(format!(
            "{field} must be a normalized repository-relative path: {value:?}"
        )))
    } else {
        Ok(())
    }
}
