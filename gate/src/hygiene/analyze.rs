use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::sync::OnceLock;
use std::time::Instant;

use rustpython_ast::{Expr, ExprContext, Visitor};
use rustpython_parser::Parse;
use wire::{Digest, Domain};

use super::measure::evidence_rank;
use super::*;

/// Test builds never reach the analysis deadline, so a starved fixture host
/// cannot turn a fixture's analysis into a timeout.
#[cfg(not(test))]
const DEADLINE_MS: u64 = 30_000;
#[cfg(test)]
const DEADLINE_MS: u64 = 365 * 24 * 60 * 60 * 1_000;

/// Admitted tracked inputs for one complete hygiene run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HygieneConfiguration {
    production_roots: Vec<String>,
    test_roots: Vec<String>,
    suppressions: Vec<SuppressedPath>,
}

impl HygieneConfiguration {
    /// The compiled roots used by the current command-surface battery.
    ///
    /// This is one gate-owned declaration rather than a pair of literals at
    /// every entrypoint. Repositories with no selected Python beneath these
    /// roots are reported as not applicable by the family; callers do not
    /// substitute another policy or launch another analyzer.
    #[must_use]
    pub fn current() -> Self {
        Self::new(["src"], ["tests"]).expect("compiled hygiene roots are normalized")
    }

    /// Admit the roots that seed production and test graph traversal.
    ///
    /// Roots are selected-tree-relative files or directories. They are
    /// normalized, sorted, and deduplicated before any source is read.
    ///
    /// # Errors
    ///
    /// Refuses an empty side or a non-normalized root.
    pub fn new(
        production_roots: impl IntoIterator<Item = impl Into<String>>,
        test_roots: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, HygieneAnalysisError> {
        let mut production_roots = collect_paths(production_roots)?;
        let mut test_roots = collect_paths(test_roots)?;
        if production_roots.is_empty() || test_roots.is_empty() {
            return Err(HygieneAnalysisError::Configuration(
                "hygiene analysis requires at least one production root and test root".to_owned(),
            ));
        }
        production_roots.sort();
        production_roots.dedup();
        test_roots.sort();
        test_roots.dedup();
        Ok(Self {
            production_roots,
            test_roots,
            suppressions: Vec::new(),
        })
    }

    /// Retain the candidates a walk rule removed from the selected tree.
    ///
    /// These never reach an analyzer, so nothing downstream can rediscover
    /// them; carrying them here is what lets a measuring arm still name what it
    /// was not allowed to measure. Canonical order is imposed once, where every
    /// other evidence row gets it, so it also covers a report read back in.
    ///
    /// # Errors
    ///
    /// Refuses a suppression missing its path, shape, deciding rule, or any
    /// provenance component, because partial provenance cannot be audited.
    pub fn with_suppressions(
        mut self,
        suppressions: impl IntoIterator<Item = SuppressedPath>,
    ) -> Result<Self, HygieneAnalysisError> {
        self.suppressions = suppressions.into_iter().collect();
        if let Some(incomplete) = self
            .suppressions
            .iter()
            .find(|item| !item.is_complete() || invalid_identity(&item.path))
        {
            return Err(HygieneAnalysisError::Configuration(format!(
                "suppressed path {:?} requires a shape, a deciding rule, and complete provenance",
                incomplete.path
            )));
        }
        Ok(self)
    }
}

/// Why a source-driven hygiene run could not produce its canonical report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HygieneAnalysisError {
    /// The immutable selected source could not be captured whole.
    Source(String),
    /// Tracked inputs were not execution-grade.
    Configuration(String),
    /// Internally produced evidence violated the closed report contract.
    Measurement(HygieneError),
}

impl fmt::Display for HygieneAnalysisError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(evidence) => {
                write!(formatter, "hygiene source acquisition failed: {evidence}")
            }
            Self::Configuration(evidence) => formatter.write_str(evidence),
            Self::Measurement(source) => write!(formatter, "hygiene measurement failed: {source}"),
        }
    }
}

impl Error for HygieneAnalysisError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Source(_) | Self::Configuration(_) => None,
            Self::Measurement(source) => Some(source),
        }
    }
}

/// Execute all five compiled hygiene arms over one admitted captured tree.
///
/// Only measurable jurisdiction entries are read. Parsing, graph construction,
/// symbol extraction, test classification, private-boundary resolution, policy
/// decisions, and elapsed time are derived inside this function; callers cannot
/// provide observations or measurement counts.
///
/// A bare [`Jurisdiction`] cannot say which layer asked for each of its walk
/// skips, so a report analyzed straight from one is silently missing its
/// suppression evidence rather than refusing to exist. Only [`HygieneSource`]
/// holds that attribution, which is why [`acquire_hygiene`] is the sole way in.
///
/// [`HygieneSource`]: super::HygieneSource
/// [`acquire_hygiene`]: super::acquire_hygiene
///
/// # Errors
///
/// Returns an internal contract error if source-derived observations cannot be
/// represented by the closed report schema.
pub(crate) fn analyze_hygiene(
    documents: &[HygieneDocument],
    project_markers: &[String],
    configuration: &HygieneConfiguration,
) -> Result<HygieneReport, HygieneAnalysisError> {
    let started = Instant::now();
    let report = AnalyzerRun::execute_at(documents, project_markers, configuration, started)
        .map_err(HygieneAnalysisError::Measurement)?;
    if elapsed_ms(started) > DEADLINE_MS {
        timeout_report(elapsed_ms(started)).map_err(HygieneAnalysisError::Measurement)
    } else {
        Ok(report)
    }
}

/// Execute exactly one compiled hygiene arm over one admitted captured tree.
pub(crate) fn analyze_hygiene_arm(
    documents: &[HygieneDocument],
    project_markers: &[String],
    configuration: &HygieneConfiguration,
    gate_id: GateId,
    reached_paths: Option<&BTreeSet<String>>,
) -> Result<HygieneMeasurement, HygieneAnalysisError> {
    if !HYGIENE_ARMS.iter().any(|arm| arm.id == gate_id) {
        return Err(HygieneAnalysisError::Measurement(
            HygieneError::ForeignGate(gate_id),
        ));
    }
    let started = Instant::now();
    let measurement = AnalyzerRun::execute_arm_at(
        documents,
        project_markers,
        configuration,
        started,
        gate_id,
        reached_paths,
    )
    .map_err(HygieneAnalysisError::Measurement)?;
    if elapsed_ms(started) > DEADLINE_MS {
        timeout_measurement(gate_id, elapsed_ms(started)).map_err(HygieneAnalysisError::Measurement)
    } else {
        Ok(measurement)
    }
}

fn timeout_report(elapsed_ms: u64) -> Result<HygieneReport, HygieneError> {
    measure_hygiene(HYGIENE_ARMS.map(|declaration| HygieneObservation {
        gate_id: declaration.id,
        outcome: HygieneOutcome::Timeout,
        reason: "native hygiene deadline expired".to_owned(),
        evidence: vec![AnalysisEvidence::Deadline {
            limit_ms: DEADLINE_MS,
            elapsed_ms,
            state: EvidenceState::TimedOut,
        }],
        findings: Vec::new(),
    }))
}

fn timeout_measurement(
    gate_id: GateId,
    elapsed_ms: u64,
) -> Result<HygieneMeasurement, HygieneError> {
    measure_hygiene_arm(HygieneObservation {
        gate_id,
        outcome: HygieneOutcome::Timeout,
        reason: "native hygiene deadline expired".to_owned(),
        evidence: vec![AnalysisEvidence::Deadline {
            limit_ms: DEADLINE_MS,
            elapsed_ms,
            state: EvidenceState::TimedOut,
        }],
        findings: Vec::new(),
    })
}

fn invalid_identity(value: &str) -> bool {
    value.is_empty() || value.contains(['\n', '\r'])
}

fn collect_paths(
    values: impl IntoIterator<Item = impl Into<String>>,
) -> Result<Vec<String>, HygieneAnalysisError> {
    let mut output = Vec::new();
    for value in values {
        let value = value.into();
        if invalid_selected_path(&value) || value.ends_with('/') || value.contains('\\') {
            return Err(HygieneAnalysisError::Configuration(format!(
                "hygiene root is not a normalized selected-tree path: {value:?}"
            )));
        }
        output.push(value);
    }
    Ok(output)
}

fn invalid_selected_path(path: &str) -> bool {
    path.is_empty()
        || path.starts_with('/')
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
}

#[derive(Clone, Debug)]
enum Terminal {
    Parsed,
    TimedOut {
        elapsed_ms: u64,
    },
    ParseFailure {
        documents: u32,
        path: String,
        diagnostic: String,
    },
    WorkExhausted {
        documents: u32,
        path: String,
        diagnostic: String,
    },
}

struct AnalyzerRun<'a> {
    configuration: &'a HygieneConfiguration,
    boundary: &'a dyn ExecutionBoundary,
    started: Instant,
    project_markers: Vec<String>,
    selected_files: u32,
    corpus: Corpus,
    terminal: Terminal,
}

impl<'a> AnalyzerRun<'a> {
    fn execute_at(
        documents: &'a [HygieneDocument],
        project_markers: &[String],
        configuration: &'a HygieneConfiguration,
        started: Instant,
    ) -> Result<HygieneReport, HygieneError> {
        Self::execute_with_at(
            documents,
            project_markers,
            configuration,
            &NativeExecution,
            started,
        )
    }

    fn execute_arm_at(
        documents: &'a [HygieneDocument],
        project_markers: &[String],
        configuration: &'a HygieneConfiguration,
        started: Instant,
        gate_id: GateId,
        reached_paths: Option<&BTreeSet<String>>,
    ) -> Result<HygieneMeasurement, HygieneError> {
        let run = Self::prepare(
            documents,
            project_markers,
            configuration,
            &NativeExecution,
            started,
            reached_paths,
        );
        let declaration = HYGIENE_ARMS
            .into_iter()
            .find(|declaration| declaration.id == gate_id)
            .ok_or(HygieneError::ForeignGate(gate_id))?;
        measure_hygiene_arm(run.observe(declaration))
    }

    fn execute_with_at(
        documents: &'a [HygieneDocument],
        project_markers: &[String],
        configuration: &'a HygieneConfiguration,
        boundary: &'a dyn ExecutionBoundary,
        started: Instant,
    ) -> Result<HygieneReport, HygieneError> {
        let run = Self::prepare(
            documents,
            project_markers,
            configuration,
            boundary,
            started,
            None,
        );
        let observations = HYGIENE_ARMS.map(|arm| run.observe(arm));
        measure_hygiene(observations)
    }

    fn prepare(
        documents: &'a [HygieneDocument],
        project_markers: &[String],
        configuration: &'a HygieneConfiguration,
        boundary: &'a dyn ExecutionBoundary,
        started: Instant,
        reached_paths: Option<&BTreeSet<String>>,
    ) -> Self {
        let (selected_files, corpus, terminal) =
            Corpus::load(documents, configuration, started, reached_paths);
        Self {
            configuration,
            boundary,
            started,
            project_markers: project_markers.to_vec(),
            selected_files,
            corpus,
            terminal,
        }
    }

    fn observe(&self, declaration: HygieneDeclaration) -> HygieneObservation {
        let analyzer = match self.discover(declaration) {
            Ok(analyzer) => analyzer,
            Err(observation) => return observation,
        };
        if let Some(observation) = self.terminal_state_observation(declaration.id) {
            return observation;
        }
        self.measure_observation(declaration, analyzer)
    }

    fn discover(
        &self,
        declaration: HygieneDeclaration,
    ) -> Result<AnalyzerEvidence, HygieneObservation> {
        if !self.boundary.scheduled(declaration.id) {
            return Err(self.terminal_observation(
                declaration.id,
                HygieneOutcome::Unmeasured,
                "hygiene scheduler did not dispatch the selected arm",
                AnalysisEvidence::Measurement {
                    state: EvidenceState::Unmeasured,
                    stage: "scheduler-dispatch".to_owned(),
                },
            ));
        }
        let analyzer = self.boundary.analyzer(declaration);
        let failure = if analyzer.is_unavailable() {
            Some((
                HygieneOutcome::Unavailable,
                "compiled hygiene analyzer is unavailable",
            ))
        } else if analyzer.is_unsupported() {
            Some((
                HygieneOutcome::Unsupported,
                "compiled hygiene analyzer violates its exact version contract",
            ))
        } else {
            None
        };
        if let Some((outcome, reason)) = failure {
            return Err(self.terminal_observation(
                declaration.id,
                outcome,
                reason,
                AnalysisEvidence::Analyzer(analyzer),
            ));
        }
        Ok(analyzer)
    }

    fn terminal_state_observation(&self, gate_id: GateId) -> Option<HygieneObservation> {
        let (outcome, reason, evidence) = match &self.terminal {
            Terminal::TimedOut { elapsed_ms } => (
                HygieneOutcome::Timeout,
                "native hygiene deadline expired",
                AnalysisEvidence::Deadline {
                    limit_ms: DEADLINE_MS,
                    elapsed_ms: *elapsed_ms,
                    state: EvidenceState::TimedOut,
                },
            ),
            Terminal::ParseFailure {
                documents,
                path,
                diagnostic,
            } => (
                HygieneOutcome::ParseFailure,
                "selected Python source did not parse",
                AnalysisEvidence::Parse {
                    documents: *documents,
                    state: EvidenceState::ParseFailed,
                    failed_path: Some(path.clone()),
                    diagnostic: Some(diagnostic.clone()),
                },
            ),
            Terminal::WorkExhausted {
                documents,
                path,
                diagnostic,
            } => (
                HygieneOutcome::Failed,
                "native hygiene analysis work bound exhausted",
                AnalysisEvidence::Measurement {
                    state: EvidenceState::Failed,
                    stage: format!(
                        "analysis-work path={path:?} parsed_documents={documents} {diagnostic}"
                    ),
                },
            ),
            Terminal::Parsed => return None,
        };
        Some(self.terminal_observation(gate_id, outcome, reason, evidence))
    }

    fn measure_observation(
        &self,
        declaration: HygieneDeclaration,
        analyzer: AnalyzerEvidence,
    ) -> HygieneObservation {
        if self.elapsed_ms() > DEADLINE_MS {
            return self.timeout_observation(declaration.id);
        }
        if !self.corpus.applicable(declaration.id) {
            return HygieneObservation {
                gate_id: declaration.id,
                outcome: HygieneOutcome::NotApplicable,
                reason: "compiled hygiene applicability predicate is false".to_owned(),
                evidence: vec![self.language_evidence(false)],
                findings: Vec::new(),
            };
        }
        let mut graph_work = GraphWork::new(self.started, DEADLINE_MS);
        if !graph_work.preflight(&self.corpus) {
            return self.graph_failure_observation(declaration.id, &graph_work);
        }
        let Some(findings) = self.corpus.findings(declaration.id, &mut graph_work) else {
            return self.graph_failure_observation(declaration.id, &graph_work);
        };
        if self.elapsed_ms() > DEADLINE_MS {
            return self.timeout_observation(declaration.id);
        }
        let outcome = if findings.is_empty() {
            HygieneOutcome::Passed
        } else {
            HygieneOutcome::Findings
        };
        let Some(evidence) =
            self.completed_evidence(declaration, analyzer, findings.len(), &mut graph_work)
        else {
            return self.graph_failure_observation(declaration.id, &graph_work);
        };
        HygieneObservation {
            gate_id: declaration.id,
            outcome,
            reason: if outcome == HygieneOutcome::Passed {
                "source-derived findings are within the admitted policy limit".to_owned()
            } else {
                "source-derived findings exceed the admitted policy limit".to_owned()
            },
            evidence,
            findings,
        }
    }

    fn graph_failure_observation(&self, gate_id: GateId, work: &GraphWork) -> HygieneObservation {
        if matches!(work.failure(), Some(GraphFailure::Deadline)) {
            return self.timeout_observation(gate_id);
        }
        self.terminal_observation(
            gate_id,
            HygieneOutcome::Failed,
            "native hygiene graph work bound exhausted",
            AnalysisEvidence::Measurement {
                state: EvidenceState::Failed,
                stage: work.diagnostic(),
            },
        )
    }

    fn completed_evidence(
        &self,
        declaration: HygieneDeclaration,
        analyzer: AnalyzerEvidence,
        finding_count: usize,
        work: &mut GraphWork,
    ) -> Option<Vec<AnalysisEvidence>> {
        let mut evidence = vec![
            self.language_evidence(true),
            AnalysisEvidence::Analyzer(analyzer),
            AnalysisEvidence::PolicyDecision {
                field: decision_field(declaration.id).to_owned(),
                limit: 0,
                observed: u32::try_from(finding_count).unwrap_or(u32::MAX),
            },
            AnalysisEvidence::Deadline {
                limit_ms: DEADLINE_MS,
                elapsed_ms: self.elapsed_ms().min(DEADLINE_MS),
                state: EvidenceState::Completed,
            },
            AnalysisEvidence::Parse {
                documents: self.selected_files,
                state: EvidenceState::Completed,
                failed_path: None,
                diagnostic: None,
            },
            AnalysisEvidence::Measurement {
                state: EvidenceState::Completed,
                stage: "policy-decision".to_owned(),
            },
        ];
        if !self.configuration.suppressions.is_empty() {
            evidence.push(AnalysisEvidence::PathSuppression {
                suppressed: self.configuration.suppressions.clone(),
            });
        }
        for traversal in declaration.traversals {
            let item = self.corpus.traversal_evidence(*traversal, work)?;
            if !evidence
                .iter()
                .any(|existing| evidence_rank(existing) == evidence_rank(&item))
            {
                evidence.push(item);
            }
        }
        work.check().then_some(evidence)
    }

    fn terminal_observation(
        &self,
        gate_id: GateId,
        outcome: HygieneOutcome,
        reason: &str,
        evidence: AnalysisEvidence,
    ) -> HygieneObservation {
        HygieneObservation {
            gate_id,
            outcome,
            reason: reason.to_owned(),
            evidence: vec![evidence],
            findings: Vec::new(),
        }
    }

    fn timeout_observation(&self, gate_id: GateId) -> HygieneObservation {
        self.terminal_observation(
            gate_id,
            HygieneOutcome::Timeout,
            "native hygiene deadline expired",
            AnalysisEvidence::Deadline {
                limit_ms: DEADLINE_MS,
                elapsed_ms: self.elapsed_ms(),
                state: EvidenceState::TimedOut,
            },
        )
    }

    fn language_evidence(&self, applicable: bool) -> AnalysisEvidence {
        AnalysisEvidence::LanguageApplicability {
            language: "python".to_owned(),
            selected_files: self.selected_files,
            project_markers: self.project_markers.clone(),
            applicable,
            provenance: HygieneProvenance {
                release: CURRENT_RELEASE.to_owned(),
                layer: COMPILED_LAYER.to_owned(),
                document: COMPILED_DOCUMENT.to_owned(),
                field: "gate::hygiene::HygieneJurisdiction::applicability".to_owned(),
            },
        }
    }

    fn elapsed_ms(&self) -> u64 {
        elapsed_ms(self.started)
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn decision_field(gate: GateId) -> &'static str {
    match gate {
        GateId::Reachability => "policy.debt.reachability_test_only",
        GateId::AssertionFreeTests => "policy.debt.assertion_free_tests",
        _ => "compiled:zero-findings",
    }
}

trait ExecutionBoundary {
    fn scheduled(&self, gate: GateId) -> bool;
    fn analyzer(&self, declaration: HygieneDeclaration) -> AnalyzerEvidence;
}

struct NativeExecution;

impl ExecutionBoundary for NativeExecution {
    fn scheduled(&self, _gate: GateId) -> bool {
        true
    }

    fn analyzer(&self, declaration: HygieneDeclaration) -> AnalyzerEvidence {
        match native_executable_digest() {
            Some(digest) => AnalyzerEvidence::Identified {
                name: declaration.analyzer.evidence_name().to_owned(),
                requested: declaration.analyzer.component().to_owned(),
                resolved: declaration.analyzer.component().to_owned(),
                declared_version: NATIVE_ANALYZER_VERSION.to_owned(),
                observed_version: NATIVE_ANALYZER_VERSION.to_owned(),
                digest: digest.to_owned(),
            },
            None => AnalyzerEvidence::Unavailable {
                name: declaration.analyzer.evidence_name().to_owned(),
                requested: declaration.analyzer.component().to_owned(),
            },
        }
    }
}

fn native_executable_digest() -> Option<&'static str> {
    static DIGEST: OnceLock<Option<String>> = OnceLock::new();
    DIGEST
        .get_or_init(|| {
            let executable = std::env::current_exe().ok()?;
            let bytes = fs::read(executable).ok()?;
            Some(Digest::compute(Domain::GateTool, &bytes).to_string())
        })
        .as_deref()
}

fn saturating_sum(lengths: impl IntoIterator<Item = usize>) -> usize {
    lengths
        .into_iter()
        .fold(0, |total, length| total.saturating_add(length))
}

mod syntax;
pub(in crate::hygiene) use syntax::*;
mod corpus;
use corpus::*;
