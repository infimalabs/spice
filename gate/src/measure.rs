//! The quantities a gate measures, and what each one has to declare.
//!
//! A bare number cannot hold a ratchet up, because nobody can tell later
//! whether it fell by improvement or by the metric quietly covering less. So a
//! metric here declares its files, its language, its parser, its target, and
//! its exclusions, and the run records the exact file list it counted. A
//! shrinking number is then either an improvement or a visible change to one of
//! those declarations, and never an unexplained gift.

use std::fmt;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sill::{
    Argv, Cancellation, Completion, Deadlines, Environment, Output, ProcessGroup, Redaction,
    RestartDisposition, Spec, SpecError, StdinPolicy, StreamPolicy, WaitError, WaitPolicy,
};
use wire::{Digest, Domain};

use crate::jurisdiction::{Ignore, IgnoreError, Jurisdiction, Rule, Shape};

/// Launch a bounded source consumer while preserving its selected-tree lease.
pub(crate) fn spawn(spec: Spec) -> std::io::Result<sill::Running> {
    spawn_observed(spec, None)
}

pub(crate) fn spawn_observed(
    spec: Spec,
    observation: Option<sill::Observation>,
) -> std::io::Result<sill::Running> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        crate::tree::spawn_consumer(spec, observation)
    }
    #[cfg(target_arch = "wasm32")]
    {
        match observation {
            Some(observation) => sill::spawn_observed(spec, observation),
            None => sill::spawn(spec),
        }
    }
}

/// Whether the current process is an already-contained gate worker.
///
/// Ordinary library callers keep each bounded child in a new process group.
/// The commit gate instead runs in a fresh supervisor-owned group and enters
/// this scope before acquiring anything. Its children must stay in that same
/// group so one deadline signal reaches the worker, Cargo, rustc, Trunk, and
/// every parser together.
static INHERIT_PROCESS_GROUP: AtomicBool = AtomicBool::new(false);

/// Process-global containment authority for one isolated gate worker.
///
/// This scope is deliberately exclusive. It is intended only for a fresh
/// worker process whose complete lifetime belongs to an outer supervisor; an
/// ordinary in-process caller must retain the default per-child groups.
#[derive(Debug)]
pub struct InheritedProcessGroup {
    _private: (),
}

impl Drop for InheritedProcessGroup {
    fn drop(&mut self) {
        INHERIT_PROCESS_GROUP.store(false, Ordering::Release);
    }
}

/// Keep every bounded gate child in the caller's existing process group.
///
/// # Errors
///
/// Refuses a second simultaneous scope in the same process.
pub fn inherit_process_group() -> Result<InheritedProcessGroup, &'static str> {
    INHERIT_PROCESS_GROUP
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| InheritedProcessGroup { _private: () })
        .map_err(|_| "gate process-group containment is already active")
}

/// Process group one bounded gate child must enter.
///
/// Ordinary callers isolate each child and supervise its whole descendant
/// group. A commit-gate worker has a shorter outer deadline and deliberately
/// keeps every child in the worker's existing group so that deadline remains
/// the single containment boundary.
pub(crate) fn bounded_process_group() -> ProcessGroup {
    if INHERIT_PROCESS_GROUP.load(Ordering::Acquire) {
        ProcessGroup::Inherit
    } else {
        ProcessGroup::New
    }
}

/// Runtime deadline owned by a bounded child or its enclosing supervisor.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn bounded_runtime_deadline(runtime: Duration) -> Duration {
    if INHERIT_PROCESS_GROUP.load(Ordering::Acquire) {
        Duration::MAX
    } else {
        runtime
    }
}

/// The external program that turns a file list into one number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parser {
    program: String,
    arguments: Vec<String>,
}

impl Parser {
    /// The most files one invocation may name.
    ///
    /// Paths reach the parser on its argument vector, and every system caps how
    /// long that may be. The cap is declared here so a jurisdiction that
    /// outgrows it is refused by name rather than by a spawn failure whose
    /// message is about the kernel instead of about the metric.
    pub const FILE_LIMIT: usize = 4096;

    /// Declare a parser.
    ///
    /// # Errors
    ///
    /// Returns [`MetricError::EmptyField`] when the program is not named.
    pub fn new(
        program: impl Into<String>,
        arguments: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, MetricError> {
        let program = program.into();
        if program.is_empty() {
            return Err(MetricError::EmptyField { field: "parser" });
        }
        unbroken("program", &program)?;

        let arguments: Vec<String> = arguments.into_iter().map(Into::into).collect();
        for argument in &arguments {
            unbroken("argument", argument)?;
        }

        Ok(Self { program, arguments })
    }

    /// The program invoked.
    #[must_use]
    pub fn program(&self) -> &str {
        &self.program
    }

    /// The fixed arguments placed before the file list.
    #[must_use]
    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }
}

/// The process bounds every parser run answers to.
///
/// One value covers every metric in a policy, so no single metric can grant
/// itself a longer deadline or a wider capture than its neighbours.
#[derive(Debug, Clone)]
pub struct Bounds {
    deadlines: Deadlines,
    wait: WaitPolicy,
    grace: Duration,
    capture: usize,
}

impl Bounds {
    /// Declare the bounds parsers run under.
    ///
    /// # Errors
    ///
    /// Returns the reason SILL refused these bounds.
    pub fn new(
        owner: impl Into<String>,
        deadlines: Deadlines,
        grace: Duration,
        capture: usize,
    ) -> Result<Self, SpecError> {
        Ok(Self {
            deadlines,
            wait: WaitPolicy::new(owner, Cancellation::Never, RestartDisposition::Never)?,
            grace,
            capture,
        })
    }

    /// The deadlines a parser run may not exceed.
    #[must_use]
    pub const fn deadlines(&self) -> Deadlines {
        self.deadlines
    }

    /// Grace between termination and forced termination.
    #[must_use]
    pub const fn grace(&self) -> Duration {
        self.grace
    }

    /// Bytes retained from each output stream.
    #[must_use]
    pub const fn capture(&self) -> usize {
        self.capture
    }

    /// Remove the child runtime deadline when an outer supervisor owns it.
    ///
    /// The inherited process-group scope is entered only by the isolated
    /// pre-commit worker. Its supervisor already bounds and terminates the
    /// worker's complete descendant group, so a second wall-clock decision in
    /// a long-running child can only misclassify machine delay as source
    /// evidence. Ordinary callers retain this value unchanged.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn without_runtime_deadline_under_outer_supervisor(&self) -> Self {
        let runtime = bounded_runtime_deadline(self.deadlines.runtime());
        if runtime == self.deadlines.runtime() {
            return self.clone();
        }
        let mut supervised = self.clone();
        supervised.deadlines = Deadlines::new(
            runtime,
            self.deadlines.output_drain(),
            self.deadlines.reap(),
        )
        .expect("existing nonzero drain and reap deadlines remain valid");
        supervised
    }

    /// Canonical declaration of every process bound.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "bounds 1\nowner {}\nruntime-ms {}\noutput-drain-ms {}\nreap-ms {}\n\
             cancellation never\nrestart never\ngrace-ms {}\ncapture {}\n",
            self.wait.owner(),
            self.deadlines.runtime().as_millis(),
            self.deadlines.output_drain().as_millis(),
            self.deadlines.reap().as_millis(),
            self.grace.as_millis(),
            self.capture,
        )
    }

    /// Identity of the complete process declaration.
    #[must_use]
    pub fn digest(&self) -> Digest {
        Digest::compute(Domain::GateBounds, self.render().as_bytes())
    }

    pub(crate) fn spec(&self, argv: Argv, cwd: &Path) -> Spec {
        Spec {
            argv,
            cwd: cwd.to_path_buf(),
            environment: Environment::empty(),
            group: bounded_process_group(),
            deadlines: self.deadlines,
            wait: self.wait.clone(),
            output: Output::new(
                StdinPolicy::Null,
                StreamPolicy::Capture {
                    limit: self.capture,
                },
                StreamPolicy::Capture {
                    limit: self.capture,
                },
            ),
            redaction: Redaction::None,
            grace: self.grace,
        }
    }
}

/// One quantity the gate measures, declaring everything doctrine requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metric {
    name: String,
    language: String,
    target: String,
    parser: Parser,
    files: Vec<Rule>,
    exclusions: Vec<Rule>,
}

impl Metric {
    /// Declare a metric.
    ///
    /// `files` and `exclusions` are the two directions of scope stated
    /// separately, so a negated rule in either list would be a second way to
    /// say what the other list already says. Those are refused.
    ///
    /// # Errors
    ///
    /// Returns the reason this declaration is incomplete or ambiguous.
    pub fn new(
        name: impl Into<String>,
        language: impl Into<String>,
        target: impl Into<String>,
        parser: Parser,
        files: &str,
        exclusions: &str,
    ) -> Result<Self, MetricError> {
        let (name, language, target) = (name.into(), language.into(), target.into());
        for (field, value) in [
            ("name", &name),
            ("language", &language),
            ("target", &target),
        ] {
            if value.is_empty() {
                return Err(MetricError::EmptyField { field });
            }
            unbroken(field, value)?;
        }
        if name.contains(|character: char| character.is_whitespace()) {
            return Err(MetricError::SpacedName { name });
        }

        let files = selectors(files)?;
        if files.is_empty() {
            return Err(MetricError::EmptyField { field: "files" });
        }

        Ok(Self {
            name,
            language,
            target,
            parser,
            files,
            exclusions: selectors(exclusions)?,
        })
    }

    /// The metric's stable name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The language the parser reads.
    #[must_use]
    pub fn language(&self) -> &str {
        &self.language
    }

    /// What the number stands for.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    /// The declared parser.
    #[must_use]
    pub const fn parser(&self) -> &Parser {
        &self.parser
    }

    /// Whether this metric covers one admitted path.
    #[must_use]
    pub fn covers(&self, path: &str, shape: Shape) -> bool {
        self.files.iter().any(|rule| rule.matches(path, shape))
            && !self.exclusions.iter().any(|rule| rule.matches(path, shape))
    }

    /// The files inside a jurisdiction this metric counts, in path order.
    #[must_use]
    pub fn selection(&self, jurisdiction: &Jurisdiction) -> Vec<String> {
        jurisdiction
            .measurable()
            .filter(|entry| self.covers(entry.path(), entry.shape()))
            .map(|entry| entry.path().to_owned())
            .collect()
    }

    /// Run the declared parser over this metric's selection.
    ///
    /// # Errors
    ///
    /// Returns the reason no trustworthy number came back. A parser that fails,
    /// is killed by a deadline, or prints something other than one count
    /// produces no measurement at all rather than a zero.
    pub fn measure(
        &self,
        jurisdiction: &Jurisdiction,
        bounds: &Bounds,
    ) -> Result<Measurement, MeasureError> {
        let files = self.selection(jurisdiction);
        if files.len() > Parser::FILE_LIMIT {
            return Err(MeasureError::TooManyFiles {
                metric: self.name.clone(),
                files: files.len(),
            });
        }

        let outcome = spawn(self.spec(jurisdiction, bounds, &files)?)
            .map_err(|source| MeasureError::Spawn {
                metric: self.name.clone(),
                source,
            })?
            .wait()
            .map_err(|source| MeasureError::Wait {
                metric: self.name.clone(),
                source,
            })?;

        let refuse = |evidence: String| MeasureError::Unreadable {
            metric: self.name.clone(),
            evidence,
        };
        let Completion::Exited(status) = outcome.completion() else {
            return Err(refuse(format!("{:?}", outcome.completion())));
        };
        if !status.success() {
            return Err(refuse(format!("parser exited {status}")));
        }

        let captured = outcome
            .stdout()
            .ok_or_else(|| refuse("no stdout".to_owned()))?;
        let printed = String::from_utf8_lossy(captured.bytes()).trim().to_owned();
        let capture_failure = match (captured.truncated(), captured.complete()) {
            (true, false) => Some("truncated and incomplete stdout capture"),
            (true, true) => Some("truncated stdout capture"),
            (false, false) => Some("incomplete stdout capture"),
            (false, true) => None,
        };
        if let Some(evidence) = capture_failure {
            return Err(refuse(format!("{evidence}: {printed}")));
        }
        let value = printed.parse().map_err(|_| refuse(printed))?;

        Ok(Measurement {
            metric: self.name.clone(),
            value,
            files,
        })
    }

    /// The bounded process this metric's parser runs as.
    fn spec(
        &self,
        jurisdiction: &Jurisdiction,
        bounds: &Bounds,
        files: &[String],
    ) -> Result<Spec, MeasureError> {
        let arguments: Vec<String> = self.parser.arguments.iter().chain(files).cloned().collect();

        let argv = Argv::new(self.parser.program.clone(), arguments).map_err(|source| {
            MeasureError::Spec {
                metric: self.name.clone(),
                source,
            }
        })?;
        Ok(bounds.spec(argv, jurisdiction.root()))
    }

    /// This metric's lines in a policy rendering.
    ///
    /// Every declared value is the last field on its line, and no declared
    /// value may contain a line break. Together those two facts are what make
    /// the rendering injective: a value can hold spaces without swallowing the
    /// field that follows it, and cannot open a line the policy never declared.
    /// Arguments therefore get one line each rather than being joined by
    /// spaces, which would have made `["-c", "a b"]` and `["-c", "a", "b"]` one
    /// policy under two declarations.
    pub(crate) fn render(&self, into: &mut String) {
        let _ = writeln!(into, "metric {} language {}", self.name, self.language);
        let _ = writeln!(into, "metric {} target {}", self.name, self.target);
        let _ = writeln!(into, "metric {} program {}", self.name, self.parser.program);
        for argument in &self.parser.arguments {
            let _ = writeln!(into, "metric {} argument {argument}", self.name);
        }
        for rule in &self.files {
            let _ = writeln!(into, "metric {} files {rule}", self.name);
        }
        for rule in &self.exclusions {
            let _ = writeln!(into, "metric {} exclude {rule}", self.name);
        }
    }
}

/// Refuse a declared value that could open a line of its own.
///
/// The policy digest is taken over the rendering, so a value carrying a line
/// break would let one declaration spell a policy that a different, honest
/// declaration also spells. Injectivity is the whole reason the digest is worth
/// binding into a receipt, and this is where it is bought.
fn unbroken(field: &'static str, value: &str) -> Result<(), MetricError> {
    if value.contains(['\n', '\r']) {
        return Err(MetricError::LineBreak {
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}

/// Parse a selector list, refusing the negations the two lists make redundant.
fn selectors(text: &str) -> Result<Vec<Rule>, MetricError> {
    let parsed = Ignore::parse(text).map_err(MetricError::Selector)?;
    if let Some(rule) = parsed.rules().iter().find(|rule| rule.negated()) {
        return Err(MetricError::NegatedSelector {
            rule: rule.to_string(),
        });
    }
    Ok(parsed.rules().to_vec())
}

/// One measured quantity and the exact files behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Measurement {
    metric: String,
    value: u64,
    files: Vec<String>,
}

impl Measurement {
    /// State a measured quantity and the files behind it.
    pub fn new(
        metric: impl Into<String>,
        value: u64,
        files: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            metric: metric.into(),
            value,
            files: files.into_iter().map(Into::into).collect(),
        }
    }

    /// The metric that produced this number.
    #[must_use]
    pub fn metric(&self) -> &str {
        &self.metric
    }

    /// The number.
    #[must_use]
    pub const fn value(&self) -> u64 {
        self.value
    }

    /// Exactly what was counted.
    #[must_use]
    pub fn files(&self) -> &[String] {
        &self.files
    }
}

/// The canonical rendering of a measurement set, files included.
///
/// The file list is part of the rendering because it is the part that explains
/// a number's movement. Two runs whose counts agree but whose file lists differ
/// did not measure the same thing, and their digests say so.
#[must_use]
pub fn render_measurements(measurements: &[Measurement]) -> String {
    let mut rendered = String::new();
    for measurement in measurements {
        let _ = writeln!(
            rendered,
            "count {} {}",
            measurement.metric, measurement.value
        );
        for file in &measurement.files {
            let _ = writeln!(rendered, "file {} {file}", measurement.metric);
        }
    }
    rendered
}

/// Digest of a measurement set.
#[must_use]
pub fn measurement_digest(measurements: &[Measurement]) -> Digest {
    Digest::compute(
        Domain::GateMeasurement,
        render_measurements(measurements).as_bytes(),
    )
}

/// Which path set one policy admits before its ignore rules decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JurisdictionMode {
    /// Walk every path reached below the named root.
    WholeWalk,
    /// Walk only paths captured from the repository's current Git index.
    TrackedIndex,
}

impl JurisdictionMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::WholeWalk => "whole-walk",
            Self::TrackedIndex => "tracked-index",
        }
    }
}

/// The full enforcement declaration one gate run reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    jurisdiction: JurisdictionMode,
    ignore: Ignore,
    metrics: Vec<Metric>,
}

impl Policy {
    /// Declare a policy.
    ///
    /// Metrics are held in name order so the rendering, and therefore the
    /// policy digest, does not depend on the order they were written in.
    ///
    /// # Errors
    ///
    /// Returns [`MetricError::DuplicateName`] when two metrics share a name,
    /// which would make a ratchet line ambiguous.
    pub fn new(
        ignore: Ignore,
        metrics: impl IntoIterator<Item = Metric>,
    ) -> Result<Self, MetricError> {
        Self::with_jurisdiction(JurisdictionMode::WholeWalk, ignore, metrics)
    }

    /// Declare a policy over the current Git index path set.
    ///
    /// # Errors
    ///
    /// Returns [`MetricError::DuplicateName`] when two metrics share a name.
    pub fn tracked_index(
        ignore: Ignore,
        metrics: impl IntoIterator<Item = Metric>,
    ) -> Result<Self, MetricError> {
        Self::with_jurisdiction(JurisdictionMode::TrackedIndex, ignore, metrics)
    }

    fn with_jurisdiction(
        jurisdiction: JurisdictionMode,
        ignore: Ignore,
        metrics: impl IntoIterator<Item = Metric>,
    ) -> Result<Self, MetricError> {
        let mut metrics: Vec<Metric> = metrics.into_iter().collect();
        metrics.sort_by(|left, right| left.name.cmp(&right.name));

        if let Some(pair) = metrics.windows(2).find(|pair| pair[0].name == pair[1].name) {
            return Err(MetricError::DuplicateName {
                name: pair[0].name.clone(),
            });
        }

        Ok(Self {
            jurisdiction,
            ignore,
            metrics,
        })
    }

    /// The one source of paths this policy measures.
    #[must_use]
    pub const fn jurisdiction(&self) -> JurisdictionMode {
        self.jurisdiction
    }

    /// The jurisdiction rules.
    #[must_use]
    pub const fn ignore(&self) -> &Ignore {
        &self.ignore
    }

    /// The declared metrics, in name order.
    #[must_use]
    pub fn metrics(&self) -> &[Metric] {
        &self.metrics
    }

    /// The canonical rendering of everything this policy declares.
    #[must_use]
    pub fn render(&self) -> String {
        let mut rendered = String::new();
        if self.jurisdiction == JurisdictionMode::TrackedIndex {
            let _ = writeln!(rendered, "jurisdiction {}", self.jurisdiction.as_str());
        }
        for rule in self.ignore.rules() {
            let _ = writeln!(rendered, "ignore {rule}");
        }
        for metric in &self.metrics {
            metric.render(&mut rendered);
        }
        rendered
    }

    /// Digest of the declaration.
    #[must_use]
    pub fn digest(&self) -> Digest {
        Digest::compute(Domain::GatePolicy, self.render().as_bytes())
    }

    /// Measure every metric against one jurisdiction.
    ///
    /// # Errors
    ///
    /// Returns the first metric that could not be measured. A partial
    /// measurement set is never returned, because a ratchet compared against
    /// one would read a missing metric as an absent regression.
    pub fn measure(
        &self,
        jurisdiction: &Jurisdiction,
        bounds: &Bounds,
    ) -> Result<Vec<Measurement>, MeasureError> {
        self.metrics
            .iter()
            .map(|metric| metric.measure(jurisdiction, bounds))
            .collect()
    }
}

/// What is wrong with a metric or policy declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetricError {
    /// A field doctrine requires was left unnamed.
    EmptyField {
        /// Which declaration is missing.
        field: &'static str,
    },
    /// A metric name carries whitespace, so a rendered line cannot be split.
    SpacedName {
        /// The refused name.
        name: String,
    },
    /// Two metrics share one name.
    DuplicateName {
        /// The shared name.
        name: String,
    },
    /// A selector line is not an admitted spelling.
    Selector(IgnoreError),
    /// A selector negates, which the two-list form already expresses.
    NegatedSelector {
        /// The refused rule.
        rule: String,
    },
    /// A declared value carries a line break and could forge a policy line.
    LineBreak {
        /// Which declaration carries it.
        field: &'static str,
        /// The refused value.
        value: String,
    },
}

impl fmt::Display for MetricError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyField { field } => write!(formatter, "a metric must declare its {field}"),
            Self::SpacedName { name } => {
                write!(formatter, "metric name {name:?} carries whitespace")
            }
            Self::DuplicateName { name } => write!(formatter, "two metrics are named {name:?}"),
            Self::Selector(source) => write!(formatter, "{source}"),
            Self::NegatedSelector { rule } => write!(
                formatter,
                "selector {rule:?} negates; state scope with the files and exclusions lists"
            ),
            Self::LineBreak { field, value } => write!(
                formatter,
                "{field} {value:?} carries a line break, which would forge a policy line"
            ),
        }
    }
}

impl std::error::Error for MetricError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Selector(source) => Some(source),
            _ => None,
        }
    }
}

/// Why no trustworthy number came back.
#[derive(Debug)]
pub enum MeasureError {
    /// The selection exceeds what one argument vector may name.
    TooManyFiles {
        /// The metric that overflowed.
        metric: String,
        /// How many files it selected.
        files: usize,
    },
    /// The parser process could not be described.
    Spec {
        /// The metric being measured.
        metric: String,
        /// What SILL said.
        source: SpecError,
    },
    /// The parser process could not be started.
    Spawn {
        /// The metric being measured.
        metric: String,
        /// What the system said.
        source: std::io::Error,
    },
    /// The parser process could not be waited on.
    Wait {
        /// The metric being measured.
        metric: String,
        /// What SILL said.
        source: WaitError,
    },
    /// The parser produced something other than one count.
    Unreadable {
        /// The metric being measured.
        metric: String,
        /// Exactly what came back.
        evidence: String,
    },
}

impl fmt::Display for MeasureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyFiles { metric, files } => write!(
                formatter,
                "{metric} selected {files} files, past the {} one invocation may name",
                Parser::FILE_LIMIT
            ),
            Self::Spec { metric, source } => write!(formatter, "{metric} parser spec: {source}"),
            Self::Spawn { metric, source } => write!(formatter, "{metric} parser spawn: {source}"),
            Self::Wait { metric, source } => write!(formatter, "{metric} parser wait: {source}"),
            Self::Unreadable { metric, evidence } => {
                write!(formatter, "{metric} parser produced no count: {evidence}")
            }
        }
    }
}

impl std::error::Error for MeasureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spec { source, .. } => Some(source),
            Self::Spawn { source, .. } => Some(source),
            Self::Wait { source, .. } => Some(source),
            Self::TooManyFiles { .. } | Self::Unreadable { .. } => None,
        }
    }
}
