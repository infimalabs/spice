//! What one current gate run decided, and every identity that pins its receipt.
//!
//! A verdict that only says pass or fail is a rumour. A publishable receipt is
//! minted only by [`Contract::adjudicate`]: it binds the selected contents, the
//! compiled gate set, every loaded declaration, every probed tool, and the
//! ordered outcome of the completed run. Decoded receipts remain evidence;
//! there is no path from their retained bytes back into executable inputs.

use std::fmt;
use std::fmt::Write as _;
use std::io::Write as _;
use std::marker::PhantomData;
use std::path::Path;

use serde::{Deserialize, Serialize};
use wire::{Digest, Domain};

use crate::input::{Contract, ToolIdentity};
use crate::jurisdiction::{Jurisdiction, WalkError};
#[cfg(not(target_arch = "wasm32"))]
use crate::long_lines::scan_jurisdiction;
use crate::long_lines::{LongLineFinding, validate as validate_long_lines};
use crate::measure::{
    Bounds, JurisdictionMode, MeasureError, Measurement, Policy, measurement_digest,
    render_measurements,
};
#[cfg(not(target_arch = "wasm32"))]
use crate::metrics::CommittedTexts;
use crate::ratchet::{Finding, Ratchet};
#[cfg(not(target_arch = "wasm32"))]
use crate::tree::{TreeError, tracked_paths};

/// The one receipt schema this build emits and admits.
pub const GATE_RECEIPT_VERSION: u64 = 1;

/// Whether a run passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Standing {
    /// Every metric held, shrank, or is newly declared.
    Clean,
    /// At least one metric regressed or lost the declaration behind its
    /// baseline.
    Refused,
}

impl Standing {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Refused => "refused",
        }
    }
}

/// One compiled tool identity retained in a receipt without executable data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptTool {
    name: String,
    identity: Digest,
}

impl ReceiptTool {
    fn current(tool: &ToolIdentity) -> Self {
        Self {
            name: tool.name().to_owned(),
            identity: tool.digest().clone(),
        }
    }

    /// Stable compiled tool name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Identity of the declaration and executable bytes observed at preflight.
    #[must_use]
    pub const fn identity(&self) -> &Digest {
        &self.identity
    }
}

/// One completed current run's canonical, versioned receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verdict {
    version: u64,
    /// Digest of the admitted files' contents.
    tree: Digest,
    /// Digest of the complete compiled gate set.
    gate_set: Digest,
    /// Digest of the enforcement declaration.
    policy: Digest,
    /// Digest of every path reached, admitted or not.
    inventory: Digest,
    /// Digest of the measured quantities and the files behind them.
    measurement: Digest,
    /// Digest of the complete current process bounds.
    bounds: Digest,
    /// Digest of the compiled lower-is-better state.
    baseline: Digest,
    /// Exact committed `HEAD` the per-file source comparison captured.
    head: String,
    /// Every current compiled tool identity, in declaration order.
    tools: Vec<ReceiptTool>,
    /// Whether the run passed.
    standing: Standing,
    /// Where every metric stands, in metric order.
    findings: Vec<Finding>,
    /// Newly introduced over-limit Rust lines, in path and locus order.
    long_lines: Vec<LongLineFinding>,
}

impl Verdict {
    fn current(contract: &Contract, run: &Run) -> Self {
        Self {
            version: GATE_RECEIPT_VERSION,
            tree: run.tree.clone(),
            gate_set: contract.gate_set().digest(),
            policy: run.policy.clone(),
            inventory: run.inventory.clone(),
            measurement: run.measurement.clone(),
            bounds: contract.bounds().digest(),
            baseline: contract.floor_state_digest(),
            head: run.head.clone(),
            tools: contract.tools().iter().map(ReceiptTool::current).collect(),
            standing: run.standing,
            findings: run.findings.clone(),
            long_lines: run.long_lines.clone(),
        }
    }

    /// Receipt schema admitted by this build.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Digest of admitted file contents.
    #[must_use]
    pub const fn tree(&self) -> &Digest {
        &self.tree
    }

    /// Digest of the compiled executable gate set.
    #[must_use]
    pub const fn gate_set(&self) -> &Digest {
        &self.gate_set
    }

    /// Digest of effective policy.
    #[must_use]
    pub const fn policy(&self) -> &Digest {
        &self.policy
    }

    /// Digest of every reached path and its admission reason.
    #[must_use]
    pub const fn inventory(&self) -> &Digest {
        &self.inventory
    }

    /// Digest of all completed measurements.
    #[must_use]
    pub const fn measurement(&self) -> &Digest {
        &self.measurement
    }

    /// Digest of current process bounds.
    #[must_use]
    pub const fn bounds(&self) -> &Digest {
        &self.bounds
    }

    /// Digest of the complete versioned baseline.
    #[must_use]
    pub const fn baseline(&self) -> &Digest {
        &self.baseline
    }

    /// Captured committed `HEAD`, or `unborn` when the repository has no commit.
    #[must_use]
    pub fn head(&self) -> &str {
        &self.head
    }

    /// Current compiled tools and the identities observed at preflight.
    #[must_use]
    pub fn tools(&self) -> &[ReceiptTool] {
        &self.tools
    }

    /// Whether the run passed.
    #[must_use]
    pub const fn standing(&self) -> Standing {
        self.standing
    }

    /// Every finding, in metric order.
    #[must_use]
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// Newly introduced over-limit Rust source lines.
    #[must_use]
    pub fn long_lines(&self) -> &[LongLineFinding] {
        &self.long_lines
    }

    /// Human-readable canonical text carrying every receipt field in order.
    #[must_use]
    pub fn render(&self) -> String {
        let mut rendered = format!(
            "gate-receipt {}\ntree {}\ngate-set {}\npolicy {}\ninventory {}\n\
             measurement {}\nbounds {}\nbaseline {}\nhead {}\n",
            self.version,
            self.tree,
            self.gate_set,
            self.policy,
            self.inventory,
            self.measurement,
            self.bounds,
            self.baseline,
            self.head,
        );
        for tool in &self.tools {
            let _ = writeln!(rendered, "tool {} {}", tool.name, tool.identity);
        }
        let _ = writeln!(rendered, "standing {}", self.standing.as_str());
        for finding in &self.findings {
            let _ = writeln!(rendered, "finding {} {}", finding.metric, finding.movement);
        }
        render_long_line_findings(&mut rendered, &self.long_lines);
        rendered
    }

    /// Identity of the entire frozen receipt schema and its current values.
    #[must_use]
    pub fn digest(&self) -> Digest {
        let bytes = self
            .encode()
            .expect("the receipt schema contains only infallible JSON values");
        Digest::compute(Domain::GateReceipt, &bytes)
    }

    /// The canonical bytes of this receipt.
    ///
    /// # Errors
    ///
    /// Returns the encoder error when the receipt cannot be rendered.
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        wire::encode(self)
    }

    /// Decode only the current receipt schema and only its canonical JSON.
    ///
    /// # Errors
    ///
    /// Returns malformed fields, an unsupported version with the observed
    /// number retained as evidence, or the canonical bytes for a second JSON
    /// spelling of an otherwise valid receipt.
    pub fn decode(bytes: &[u8]) -> Result<Self, ReceiptDecodeError> {
        let evidence: VersionEvidence = wire::decode(bytes).map_err(ReceiptDecodeError::Json)?;
        if evidence.version != GATE_RECEIPT_VERSION {
            return Err(ReceiptDecodeError::UnsupportedVersion {
                observed: evidence.version,
            });
        }
        let receipt: Self = wire::decode(bytes).map_err(ReceiptDecodeError::Json)?;
        let canonical = receipt.encode().map_err(ReceiptDecodeError::Json)?;
        if bytes != canonical {
            return Err(ReceiptDecodeError::NonCanonical { canonical });
        }
        Ok(receipt)
    }

    fn validate(&self) -> Result<(), &'static str> {
        if self.head.is_empty() || self.head.contains(['\n', '\r']) {
            return Err("captured HEAD is empty or contains a line break");
        }
        if self
            .findings
            .iter()
            .any(|finding| finding.metric == "rust.long-lines")
        {
            return Err("rust.long-lines survived as a scalar ratchet finding");
        }
        validate_long_lines(&self.long_lines)?;
        let derived = if self.long_lines.is_empty()
            && self
                .findings
                .iter()
                .all(|finding| finding.movement.passes())
        {
            Standing::Clean
        } else {
            Standing::Refused
        };
        if self.standing != derived {
            return Err("standing is not derived from scalar and rust.long-lines findings");
        }
        Ok(())
    }

    /// Publish this receipt so no reader ever sees a partial one.
    ///
    /// The bytes are staged beside the destination and swapped in one step. A
    /// run interrupted before the swap leaves the previous receipt exactly as
    /// it was, which is the only honest thing an unfinished run can leave.
    ///
    /// # Errors
    ///
    /// Returns the encoder or publication failure.
    pub fn publish(&self, destination: &Path) -> Result<(), PublishError> {
        let bytes = self.encode().map_err(PublishError::Encode)?;
        atomic::Destination::new(destination)
            .map_err(PublishError::Publish)?
            .publish(|staged| staged.write_all(&bytes))
            .map_err(PublishError::Publish)
    }
}

pub(crate) fn run_evidence_digest(
    measurements: &[Measurement],
    head: &str,
    long_lines: &[LongLineFinding],
) -> Digest {
    let mut rendered = render_measurements(measurements);
    let _ = writeln!(rendered, "head {head}");
    render_long_line_findings(&mut rendered, long_lines);
    Digest::compute(Domain::GateMeasurement, rendered.as_bytes())
}

fn render_long_line_findings(rendered: &mut String, findings: &[LongLineFinding]) {
    for finding in findings {
        let path = serde_json::to_string(finding.path())
            .expect("a repository path always has one JSON string spelling");
        let _ = writeln!(
            rendered,
            "finding rust.long-lines {path}:{} bytes {} file-debt {}->{}",
            finding.line(),
            finding.bytes(),
            finding.baseline_debt(),
            finding.current_debt(),
        );
    }
}

#[derive(Debug, Deserialize)]
struct VersionEvidence {
    version: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawVerdict {
    version: u64,
    tree: Digest,
    gate_set: Digest,
    policy: Digest,
    inventory: Digest,
    measurement: Digest,
    bounds: Digest,
    baseline: Digest,
    head: String,
    tools: Vec<ReceiptTool>,
    standing: Standing,
    findings: Vec<Finding>,
    long_lines: Vec<LongLineFinding>,
}

impl From<RawVerdict> for Verdict {
    fn from(raw: RawVerdict) -> Self {
        Self {
            version: raw.version,
            tree: raw.tree,
            gate_set: raw.gate_set,
            policy: raw.policy,
            inventory: raw.inventory,
            measurement: raw.measurement,
            bounds: raw.bounds,
            baseline: raw.baseline,
            head: raw.head,
            tools: raw.tools,
            standing: raw.standing,
            findings: raw.findings,
            long_lines: raw.long_lines,
        }
    }
}

impl<'de> Deserialize<'de> for Verdict {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;

        let raw = RawVerdict::deserialize(deserializer)?;
        if raw.version != GATE_RECEIPT_VERSION {
            return Err(D::Error::custom(format!(
                "receipt version {} is unsupported; current version is {}",
                raw.version, GATE_RECEIPT_VERSION
            )));
        }
        let receipt = Self::from(raw);
        receipt.validate().map_err(D::Error::custom)?;
        Ok(receipt)
    }
}

/// Everything one gate run produced.
#[derive(Debug, Clone)]
pub struct Run {
    jurisdiction: Jurisdiction,
    measurements: Vec<Measurement>,
    tree: Digest,
    policy: Digest,
    inventory: Digest,
    measurement: Digest,
    head: String,
    standing: Standing,
    findings: Vec<Finding>,
    long_lines: Vec<LongLineFinding>,
}

/// A completed run bound to the exact current contract that executed it.
#[derive(Debug, Clone)]
pub struct CurrentRun {
    run: Run,
    receipt: Verdict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotIntegrity {
    KernelChangeIdentity,
    Unsupported,
}

impl SnapshotIntegrity {
    const fn native() -> Self {
        if cfg!(target_family = "unix") {
            Self::KernelChangeIdentity
        } else {
            Self::Unsupported
        }
    }
}

#[derive(Clone, Copy)]
enum LongLineBaselines<'a> {
    None(PhantomData<&'a ()>),
    #[cfg(not(target_arch = "wasm32"))]
    Captured(&'a CommittedTexts),
}

impl LongLineBaselines<'_> {
    const fn none() -> Self {
        Self::None(PhantomData)
    }

    const fn captured(self) -> bool {
        match self {
            Self::None(_) => false,
            #[cfg(not(target_arch = "wasm32"))]
            Self::Captured(_) => true,
        }
    }
}

/// A run's progress sink: the path being measured, and how many of the total
/// are done.
type AdjudicationProgress<'a> = dyn FnMut(Option<&str>, usize, usize) + 'a;

impl Run {
    /// Walk a foreign repository, measure it, and judge it against a ratchet.
    ///
    /// # Errors
    ///
    /// Returns the failure that stopped the run. No verdict is produced from a
    /// partial run, because a receipt naming fewer metrics than the policy
    /// declares reads exactly like a run in which those metrics were fine.
    pub fn adjudicate(
        policy: &Policy,
        root: &Path,
        ratchet: &Ratchet,
        bounds: &Bounds,
    ) -> Result<Self, RunError> {
        Self::adjudicate_with_integrity(policy, root, ratchet, bounds, SnapshotIntegrity::native())
    }

    pub(crate) fn adjudicate_with_progress(
        policy: &Policy,
        root: &Path,
        ratchet: &Ratchet,
        bounds: &Bounds,
        progress: &mut AdjudicationProgress<'_>,
    ) -> Result<Self, RunError> {
        Self::adjudicate_with_inputs(
            policy,
            root,
            ratchet,
            bounds,
            SnapshotIntegrity::native(),
            LongLineBaselines::none(),
            Some(progress),
        )
    }

    fn adjudicate_with_integrity(
        policy: &Policy,
        root: &Path,
        ratchet: &Ratchet,
        bounds: &Bounds,
        snapshot_integrity: SnapshotIntegrity,
    ) -> Result<Self, RunError> {
        Self::adjudicate_with_inputs(
            policy,
            root,
            ratchet,
            bounds,
            snapshot_integrity,
            LongLineBaselines::none(),
            None,
        )
    }

    fn adjudicate_with_inputs(
        policy: &Policy,
        root: &Path,
        ratchet: &Ratchet,
        bounds: &Bounds,
        snapshot_integrity: SnapshotIntegrity,
        long_line_baselines: LongLineBaselines<'_>,
        mut progress: Option<&mut AdjudicationProgress<'_>>,
    ) -> Result<Self, RunError> {
        if snapshot_integrity == SnapshotIntegrity::Unsupported {
            return Err(RunError::SnapshotIntegrityUnsupported);
        }

        let selection = CapturedJurisdiction::capture(policy.jurisdiction(), root)?;
        let jurisdiction = selection.walk(root, policy)?;
        let inventory = jurisdiction.inventory_digest();
        let snapshot = jurisdiction.snapshot().map_err(RunError::Walk)?;
        let tree = snapshot.tree();
        let total = policy.metrics().len();
        let mut measurements = Vec::with_capacity(total);
        for (index, metric) in policy.metrics().iter().enumerate() {
            if let Some(progress) = progress.as_deref_mut() {
                progress(Some(metric.name()), index, total);
            }
            let measurement = metric.measure(snapshot.jurisdiction(), bounds);
            if let Some(changed) = snapshot.verify().map_err(RunError::Walk)? {
                return Err(RunError::SnapshotChanged {
                    metric: metric.name().to_owned(),
                    measured: measurement.as_ref().ok().map(Measurement::value),
                    path: changed.path,
                    tree: changed.tree,
                    observed_tree: changed.observed_tree,
                });
            }
            measurements.push(measurement.map_err(RunError::Measure)?);
            if let Some(progress) = progress.as_deref_mut() {
                progress(Some(metric.name()), index + 1, total);
            }
        }

        let (head, long_lines) = match long_line_baselines {
            #[cfg(not(target_arch = "wasm32"))]
            LongLineBaselines::Captured(baselines) => {
                let findings =
                    scan_jurisdiction(snapshot.jurisdiction(), baselines).map_err(|error| {
                        RunError::LongLineRead {
                            path: error.path,
                            source: error.source,
                        }
                    })?;
                if let Some(changed) = snapshot.verify().map_err(RunError::Walk)? {
                    return Err(RunError::SnapshotChanged {
                        metric: "rust.long-lines".to_owned(),
                        measured: None,
                        path: changed.path,
                        tree: changed.tree,
                        observed_tree: changed.observed_tree,
                    });
                }
                (baselines.commit().unwrap_or("unborn").to_owned(), findings)
            }
            LongLineBaselines::None(_) => ("not-captured".to_owned(), Vec::new()),
        };

        let observed = selection.walk(root, policy)?;
        let observed_inventory = observed.inventory_digest();
        let observed_snapshot = observed.snapshot().map_err(RunError::Walk)?;
        let observed_tree = observed_snapshot.tree();
        let mut paths = snapshot
            .jurisdiction()
            .changed_paths(observed_snapshot.jurisdiction())
            .map_err(RunError::Walk)?;
        paths.extend(selection.changed_paths(root)?);
        paths.sort();
        paths.dedup();
        if inventory != observed_inventory || tree != observed_tree || !paths.is_empty() {
            return Err(RunError::JurisdictionChanged {
                paths,
                inventory,
                observed_inventory,
                tree,
                observed_tree,
            });
        }

        let findings = ratchet.compare(&measurements);
        let standing =
            if long_lines.is_empty() && findings.iter().all(|finding| finding.movement.passes()) {
                Standing::Clean
            } else {
                Standing::Refused
            };
        let measurement = if long_line_baselines.captured() {
            run_evidence_digest(&measurements, &head, &long_lines)
        } else {
            measurement_digest(&measurements)
        };

        Ok(Self {
            jurisdiction,
            measurements,
            tree,
            policy: policy.digest(),
            inventory,
            measurement,
            head,
            standing,
            findings,
            long_lines,
        })
    }

    /// The jurisdiction this run walked.
    #[must_use]
    pub const fn jurisdiction(&self) -> &Jurisdiction {
        &self.jurisdiction
    }

    /// What this run measured.
    #[must_use]
    pub fn measurements(&self) -> &[Measurement] {
        &self.measurements
    }

    /// Digest of admitted file contents measured by this run.
    #[must_use]
    pub const fn tree(&self) -> &Digest {
        &self.tree
    }

    /// Digest of effective policy used by this run.
    #[must_use]
    pub const fn policy(&self) -> &Digest {
        &self.policy
    }

    /// Digest of every path reached by this run.
    #[must_use]
    pub const fn inventory(&self) -> &Digest {
        &self.inventory
    }

    /// Digest of the completed measurements.
    #[must_use]
    pub const fn measurement(&self) -> &Digest {
        &self.measurement
    }

    /// Whether the completed low-level run passed its ratchet.
    #[must_use]
    pub const fn standing(&self) -> Standing {
        self.standing
    }

    /// Ordered findings produced by the ratchet comparison.
    #[must_use]
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// Newly introduced over-limit Rust source lines attached by a current contract.
    #[must_use]
    pub fn long_lines(&self) -> &[LongLineFinding] {
        &self.long_lines
    }

    /// The ratchet this run earns, absent when the run was refused.
    ///
    /// A refused run earns nothing. Letting one tighten would take the baseline
    /// of every metric that behaved and hand it to a tree that has already
    /// failed, which is how a regression buys itself a lower bar somewhere
    /// else.
    #[must_use]
    pub fn earned(&self, ratchet: &Ratchet) -> Option<Ratchet> {
        match self.standing {
            Standing::Clean => Some(ratchet.tighten(&self.measurements)),
            Standing::Refused => None,
        }
    }
}

enum CapturedJurisdiction {
    WholeWalk,
    #[cfg(not(target_arch = "wasm32"))]
    TrackedIndex(Vec<String>),
}

impl CapturedJurisdiction {
    fn capture(mode: JurisdictionMode, root: &Path) -> Result<Self, RunError> {
        #[cfg(target_arch = "wasm32")]
        let _ = root;
        match mode {
            JurisdictionMode::WholeWalk => Ok(Self::WholeWalk),
            #[cfg(not(target_arch = "wasm32"))]
            JurisdictionMode::TrackedIndex => tracked_paths(root)
                .map(Self::TrackedIndex)
                .map_err(RunError::TrackedIndex),
            #[cfg(target_arch = "wasm32")]
            JurisdictionMode::TrackedIndex => Err(RunError::TrackedIndexUnsupported),
        }
    }

    fn walk(&self, root: &Path, policy: &Policy) -> Result<Jurisdiction, RunError> {
        match self {
            Self::WholeWalk => Jurisdiction::walk(root, policy.ignore()),
            #[cfg(not(target_arch = "wasm32"))]
            Self::TrackedIndex(paths) => Jurisdiction::walk_tracked(root, policy.ignore(), paths),
        }
        .map_err(RunError::Walk)
    }

    fn changed_paths(&self, root: &Path) -> Result<Vec<String>, RunError> {
        #[cfg(target_arch = "wasm32")]
        let _ = root;
        match self {
            Self::WholeWalk => Ok(Vec::new()),
            #[cfg(not(target_arch = "wasm32"))]
            Self::TrackedIndex(captured) => {
                let observed = tracked_paths(root).map_err(RunError::TrackedIndex)?;
                Ok(changed_tracked_paths(captured, &observed))
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn changed_tracked_paths(captured: &[String], observed: &[String]) -> Vec<String> {
    captured
        .iter()
        .filter(|path| observed.binary_search(path).is_err())
        .chain(
            observed
                .iter()
                .filter(|path| captured.binary_search(path).is_err()),
        )
        .cloned()
        .collect()
}

impl Contract {
    /// Execute this current contract and bind its complete identity to the
    /// completed run in one receipt.
    ///
    /// # Errors
    ///
    /// Returns the run failure. A partial run never reaches receipt
    /// construction.
    pub fn adjudicate(&self, root: &Path) -> Result<CurrentRun, RunError> {
        #[cfg(not(target_arch = "wasm32"))]
        let run = {
            let baselines = CommittedTexts::read(root, &[".rs".to_owned()], &[], None)
                .map_err(RunError::CommittedBaseline)?;
            Run::adjudicate_with_inputs(
                self.policy(),
                root,
                self.ratchet(),
                self.bounds(),
                SnapshotIntegrity::native(),
                LongLineBaselines::Captured(&baselines),
                None,
            )?
        };
        #[cfg(target_arch = "wasm32")]
        let run = Run::adjudicate(self.policy(), root, self.ratchet(), self.bounds())?;
        let receipt = Verdict::current(self, &run);
        Ok(CurrentRun { run, receipt })
    }
}

impl CurrentRun {
    /// Completed low-level run evidence.
    #[must_use]
    pub const fn run(&self) -> &Run {
        &self.run
    }

    /// Complete current receipt, constructed only after the run completed.
    #[must_use]
    pub const fn receipt(&self) -> &Verdict {
        &self.receipt
    }

    /// The jurisdiction this current run walked.
    #[must_use]
    pub const fn jurisdiction(&self) -> &Jurisdiction {
        self.run.jurisdiction()
    }

    /// What this current run measured.
    #[must_use]
    pub fn measurements(&self) -> &[Measurement] {
        self.run.measurements()
    }

    /// The ratchet this current run earns, absent when it was refused.
    #[must_use]
    pub fn earned(&self, ratchet: &Ratchet) -> Option<Ratchet> {
        self.run.earned(ratchet)
    }
}

/// What stopped a run.
#[derive(Debug)]
pub enum RunError {
    /// This target cannot prove that a parser left its snapshot unchanged.
    SnapshotIntegrityUnsupported,
    /// The jurisdiction could not be established.
    Walk(WalkError),
    /// A metric could not be measured.
    Measure(MeasureError),
    /// The committed `HEAD` source baseline could not be captured.
    #[cfg(not(target_arch = "wasm32"))]
    CommittedBaseline(TreeError),
    /// The current Git index path set could not be captured.
    #[cfg(not(target_arch = "wasm32"))]
    TrackedIndex(TreeError),
    /// This target cannot capture the current Git index path set.
    #[cfg(target_arch = "wasm32")]
    TrackedIndexUnsupported,
    /// One current Rust source selected for attribution could not be read.
    #[cfg(not(target_arch = "wasm32"))]
    LongLineRead {
        /// Repository-relative selected source path.
        path: String,
        /// Filesystem refusal.
        source: std::io::Error,
    },
    /// The inventory or admitted contents moved during measurement.
    JurisdictionChanged {
        /// Sorted repository-relative paths whose inventory or bytes moved.
        paths: Vec<String>,
        /// Inventory established before measurement.
        inventory: Digest,
        /// Inventory observed after measurement.
        observed_inventory: Digest,
        /// Admitted contents established before measurement.
        tree: Digest,
        /// Admitted contents observed after measurement.
        observed_tree: Digest,
    },
    /// A parser changed the private jurisdiction snapshot it was measuring.
    SnapshotChanged {
        /// The metric whose parser changed the snapshot.
        metric: String,
        /// The number it printed, when it produced one trustworthy count.
        measured: Option<u64>,
        /// The first snapshot path whose kernel state moved.
        path: String,
        /// Admitted contents captured before any parser ran.
        tree: Digest,
        /// Admitted contents after the parser, when they remained readable.
        observed_tree: Option<Digest>,
    },
}

impl fmt::Display for RunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SnapshotIntegrityUnsupported => write!(
                formatter,
                "snapshot integrity is unsupported on this target; no parser was launched"
            ),
            Self::Walk(source) => write!(formatter, "{source}"),
            Self::Measure(source) => write!(formatter, "{source}"),
            #[cfg(not(target_arch = "wasm32"))]
            Self::CommittedBaseline(source) => {
                write!(
                    formatter,
                    "capture committed HEAD for rust.long-lines: {source}"
                )
            }
            #[cfg(not(target_arch = "wasm32"))]
            Self::TrackedIndex(source) => {
                write!(formatter, "capture tracked-index jurisdiction: {source}")
            }
            #[cfg(target_arch = "wasm32")]
            Self::TrackedIndexUnsupported => {
                formatter.write_str("tracked-index jurisdiction is unsupported on this target")
            }
            #[cfg(not(target_arch = "wasm32"))]
            Self::LongLineRead { path, source } => {
                write!(formatter, "read rust.long-lines source {path}: {source}")
            }
            Self::JurisdictionChanged {
                paths,
                inventory,
                observed_inventory,
                tree,
                observed_tree,
            } => write!(
                formatter,
                "jurisdiction changed during measurement at {}: inventory {inventory} became \
                 {observed_inventory}; tree {tree} became {observed_tree}",
                paths.join(", ")
            ),
            Self::SnapshotChanged {
                metric,
                measured,
                path,
                tree,
                observed_tree,
            } => write!(
                formatter,
                "{metric} parser changed snapshot path {path} after measuring {measured:?}: \
                 tree {tree} became {observed_tree:?}"
            ),
        }
    }
}

impl std::error::Error for RunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Walk(source) => Some(source),
            Self::Measure(source) => Some(source),
            #[cfg(not(target_arch = "wasm32"))]
            Self::CommittedBaseline(source) => Some(source),
            #[cfg(not(target_arch = "wasm32"))]
            Self::TrackedIndex(source) => Some(source),
            #[cfg(not(target_arch = "wasm32"))]
            Self::LongLineRead { source, .. } => Some(source),
            #[cfg(target_arch = "wasm32")]
            Self::TrackedIndexUnsupported => None,
            Self::SnapshotIntegrityUnsupported
            | Self::JurisdictionChanged { .. }
            | Self::SnapshotChanged { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use sill::Deadlines;
    use wire::{Outcome, Reply, ReplyPosition};

    use super::*;
    use crate::jurisdiction::Ignore;
    use crate::measure::{Metric, Parser};
    use crate::ratchet::Movement;

    fn receipt_digest(label: &str) -> Digest {
        Digest::compute(Domain::GateFile, label.as_bytes())
    }

    fn sample_receipt() -> Verdict {
        Verdict {
            version: GATE_RECEIPT_VERSION,
            tree: receipt_digest("tree"),
            gate_set: receipt_digest("gate-set"),
            policy: receipt_digest("policy"),
            inventory: receipt_digest("inventory"),
            measurement: receipt_digest("measurement"),
            bounds: receipt_digest("bounds"),
            baseline: receipt_digest("baseline"),
            head: "0123456789abcdef".to_owned(),
            tools: vec![ReceiptTool {
                name: "awk".to_owned(),
                identity: receipt_digest("awk"),
            }],
            standing: Standing::Clean,
            findings: vec![Finding {
                metric: "rust.lines".to_owned(),
                movement: Movement::Held { baseline: 70 },
            }],
            long_lines: Vec::new(),
        }
    }

    #[test]
    fn canonical_text_and_json_round_trip_without_losing_ordered_evidence() {
        let receipt = sample_receipt();
        let expected_text = format!(
            "gate-receipt 1\ntree {}\ngate-set {}\npolicy {}\ninventory {}\n\
             measurement {}\nbounds {}\nbaseline {}\nhead 0123456789abcdef\ntool awk {}\n\
             standing clean\nfinding rust.lines held at 70\n",
            receipt.tree,
            receipt.gate_set,
            receipt.policy,
            receipt.inventory,
            receipt.measurement,
            receipt.bounds,
            receipt.baseline,
            receipt.tools[0].identity,
        );
        assert_eq!(receipt.render(), expected_text);

        let encoded = receipt.encode().expect("encode canonical receipt");
        assert_eq!(receipt.encode().expect("encode again"), encoded);
        let decoded = Verdict::decode(&encoded).expect("decode canonical receipt");
        assert_eq!(decoded, receipt);
        assert_eq!(decoded.render(), expected_text);
        let outcome = Outcome::Reply(Reply {
            position: ReplyPosition::Direct {},
            body: receipt,
        });
        assert_eq!(
            wire::encode(&outcome).expect("encode direct verdict body"),
            wire::encode(&outcome).expect("encode direct verdict body again")
        );
    }

    #[test]
    fn receipt_json_refuses_unknown_missing_and_second_spellings() {
        let receipt = sample_receipt();
        let encoded = receipt.encode().expect("encode canonical receipt");

        let mut unknown: serde_json::Value =
            serde_json::from_slice(&encoded).expect("decode fixture value");
        unknown
            .as_object_mut()
            .expect("receipt is an object")
            .insert("argv".to_owned(), serde_json::json!(["untrusted"]));
        assert!(matches!(
            Verdict::decode(&serde_json::to_vec(&unknown).expect("encode unknown field")),
            Err(ReceiptDecodeError::Json(_))
        ));

        let mut nested_unknown: serde_json::Value =
            serde_json::from_slice(&encoded).expect("decode nested fixture value");
        nested_unknown["findings"][0]["movement"]
            .as_object_mut()
            .expect("movement is an object")
            .insert("unknown".to_owned(), serde_json::json!(true));
        assert!(matches!(
            Verdict::decode(
                &serde_json::to_vec(&nested_unknown).expect("encode nested unknown field")
            ),
            Err(ReceiptDecodeError::Json(_))
        ));

        let mut missing: serde_json::Value =
            serde_json::from_slice(&encoded).expect("decode fixture value");
        missing
            .as_object_mut()
            .expect("receipt is an object")
            .remove("bounds");
        assert!(matches!(
            Verdict::decode(&serde_json::to_vec(&missing).expect("encode missing field")),
            Err(ReceiptDecodeError::Json(_))
        ));

        let mut padded = Vec::with_capacity(encoded.len() + 1);
        padded.push(b' ');
        padded.extend_from_slice(&encoded);
        let refused = Verdict::decode(&padded).expect_err("a second JSON spelling is refused");
        assert_eq!(refused.canonical(), Some(encoded.as_slice()));
    }

    #[test]
    fn a_foreign_receipt_version_is_named_but_never_admitted() {
        let mut foreign: serde_json::Value =
            serde_json::from_slice(&sample_receipt().encode().expect("encode canonical receipt"))
                .expect("decode fixture value");
        foreign
            .as_object_mut()
            .expect("receipt is an object")
            .insert("version".to_owned(), serde_json::json!(77));
        foreign
            .as_object_mut()
            .expect("receipt is an object")
            .insert("future-executable".to_owned(), serde_json::json!("never"));
        foreign
            .as_object_mut()
            .expect("receipt is an object")
            .insert("standing".to_owned(), serde_json::json!("future-standing"));

        let refused = Verdict::decode(
            &serde_json::to_vec(&foreign).expect("encode foreign receipt evidence"),
        )
        .expect_err("foreign receipt stays evidence");
        assert!(matches!(
            refused,
            ReceiptDecodeError::UnsupportedVersion { observed: 77 }
        ));
    }

    #[test]
    fn every_bound_value_moves_the_complete_receipt_identity() {
        let receipt = sample_receipt();
        let identity = receipt.digest();
        let assert_moved = |changed: Verdict, field: &str| {
            assert_ne!(
                changed.digest(),
                identity,
                "changing {field} did not move the receipt identity"
            );
        };

        let mut changed = receipt.clone();
        changed.version += 1;
        assert_moved(changed, "version");
        let mut changed = receipt.clone();
        changed.tree = receipt_digest("other-tree");
        assert_moved(changed, "tree");
        let mut changed = receipt.clone();
        changed.gate_set = receipt_digest("other-gate-set");
        assert_moved(changed, "gate set");
        let mut changed = receipt.clone();
        changed.policy = receipt_digest("other-policy");
        assert_moved(changed, "policy");
        let mut changed = receipt.clone();
        changed.inventory = receipt_digest("other-inventory");
        assert_moved(changed, "inventory");
        let mut changed = receipt.clone();
        changed.measurement = receipt_digest("other-measurement");
        assert_moved(changed, "measurement");
        let mut changed = receipt.clone();
        changed.bounds = receipt_digest("other-bounds");
        assert_moved(changed, "bounds");
        let mut changed = receipt.clone();
        changed.baseline = receipt_digest("other-baseline");
        assert_moved(changed, "baseline");
        let mut changed = receipt.clone();
        changed.tools[0].identity = receipt_digest("other-awk");
        assert_moved(changed, "tool identity");
        let mut changed = receipt.clone();
        changed.tools[0].name = "other-tool".to_owned();
        assert_moved(changed, "tool name");
        let mut changed = receipt.clone();
        changed.standing = Standing::Refused;
        assert_moved(changed, "standing");
        let mut changed = receipt;
        changed.findings[0].movement = Movement::Regressed {
            baseline: 70,
            measured: 71,
        };
        assert_moved(changed, "ordered findings");
    }

    #[cfg(not(target_family = "unix"))]
    #[test]
    fn a_non_unix_target_selects_the_fail_closed_snapshot_path() {
        assert_eq!(SnapshotIntegrity::native(), SnapshotIntegrity::Unsupported);
    }

    #[test]
    fn unsupported_snapshot_integrity_refuses_before_a_restoring_parser_can_run() {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "gate-unsupported-snapshot-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("src")).expect("create fixture tree");
        fs::write(root.join("src/lib.rs"), "one\n").expect("write fixture source");

        let metric = Metric::new(
            "rust.lines",
            "rust",
            "transient lines restored with their metadata",
            Parser::new(
                "same-length-restore-parser-must-not-launch",
                std::iter::empty::<&str>(),
            )
            .expect("declare parser"),
            "*.rs\n",
            "",
        )
        .expect("declare metric");
        let policy = Policy::new(Ignore::parse("").expect("parse ignore"), [metric])
            .expect("declare policy");
        let bounds = Bounds::new(
            "unsupported-snapshot-fixture",
            Deadlines::new(
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
            )
            .expect("declare deadlines"),
            Duration::from_millis(10),
            1024,
        )
        .expect("declare bounds");

        let refused = Run::adjudicate_with_integrity(
            &policy,
            &root,
            &Ratchet::parse("rust.lines 1\n").expect("parse ratchet"),
            &bounds,
            SnapshotIntegrity::Unsupported,
        )
        .expect_err("unsupported snapshot integrity must fail before parser launch");

        assert!(matches!(refused, RunError::SnapshotIntegrityUnsupported));
        fs::remove_dir_all(root).expect("remove fixture tree");
    }
}

/// Why retained receipt bytes were not admitted as current canonical evidence.
#[derive(Debug)]
pub enum ReceiptDecodeError {
    /// The JSON shape or one typed field was malformed.
    Json(serde_json::Error),
    /// The bytes name a schema this build does not interpret.
    UnsupportedVersion {
        /// Exact foreign version retained as evidence.
        observed: u64,
    },
    /// A valid value used a second JSON spelling.
    NonCanonical {
        /// The one JSON spelling this build admits.
        canonical: Vec<u8>,
    },
}

impl ReceiptDecodeError {
    /// Canonical replacement bytes, when only the JSON spelling differed.
    #[must_use]
    pub fn canonical(&self) -> Option<&[u8]> {
        match self {
            Self::NonCanonical { canonical } => Some(canonical),
            Self::Json(_) | Self::UnsupportedVersion { .. } => None,
        }
    }
}

impl fmt::Display for ReceiptDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(source) => write!(formatter, "decoding the receipt: {source}"),
            Self::UnsupportedVersion { observed } => write!(
                formatter,
                "receipt version {observed} is unsupported; current version is \
                 {GATE_RECEIPT_VERSION}"
            ),
            Self::NonCanonical { .. } => {
                formatter.write_str("receipt JSON is not the canonical byte spelling")
            }
        }
    }
}

impl std::error::Error for ReceiptDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(source) => Some(source),
            Self::UnsupportedVersion { .. } | Self::NonCanonical { .. } => None,
        }
    }
}

/// Why a receipt did not reach its destination.
#[derive(Debug)]
pub enum PublishError {
    /// The receipt could not be rendered.
    Encode(serde_json::Error),
    /// The rendered receipt could not be published.
    Publish(atomic::Error),
}

impl fmt::Display for PublishError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(source) => write!(formatter, "rendering the receipt: {source}"),
            Self::Publish(source) => write!(formatter, "publishing the receipt: {source}"),
        }
    }
}

impl std::error::Error for PublishError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Encode(source) => Some(source),
            Self::Publish(source) => Some(source),
        }
    }
}
