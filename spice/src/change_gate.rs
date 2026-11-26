//! One fail-closed ordinary gate over exact staged-change evidence.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gate::{
    ArmDisposition, ArmFamily, BatteryArmResult, BatteryManifest, BatteryState, ChangeReachObject,
    ChangeSelectionReport, GateId, Selector, StateRunScope, TestSelectionReport,
};
use serde::{Deserialize, Serialize};
use sill::Environment;

use crate::ambient::Ambient;
use crate::repository::Repository;

mod cargo_budget;
mod policy_schedule;
mod progress;
mod selection_policy;
mod test_execution;
mod worker_scheduler;

use cargo_budget::CargoBudget;
use progress::{SharedProgress, publish_test_selection};
#[cfg(test)]
use selection_policy::packaged_selection_policy;
use selection_policy::{current_source_policy, packaged_policy};
use test_execution::{TestExecution, TestRun, run_selected_tests};
use worker_scheduler::WorkerScheduler;

/// Current combined ordinary-change-gate report schema.
pub const CHANGE_GATE_REPORT_VERSION: u64 = 1;

/// One initial compare-and-swap plus bounded fresh-authority retries. The
/// enclosing pre-commit supervisor remains the wall-clock authority; this cap
/// prevents a continuously moving ledger from creating an unbounded inner
/// retry loop.
const STATE_PUBLICATION_ATTEMPTS: usize = 4;

/// Policy evidence shaped to the selector's exact authority scope.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "coverage")]
pub enum ChangePolicyReport {
    /// Minimal source-bound summaries for a locally understood change.
    Focused {
        source: wire::Digest,
        results: Vec<PolicyArmSummary>,
    },
}

impl ChangePolicyReport {
    fn focused_summaries(source: &wire::Digest, results: Vec<PolicyArmSummary>) -> Self {
        Self::Focused {
            source: source.clone(),
            results,
        }
    }

    /// Number of canonical policy arms retained by this report.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Focused { results, .. } => results.len(),
        }
    }

    /// Whether this report retains no policy arms.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Canonical policy results in selected manifest order.
    pub fn iter(&self) -> std::slice::Iter<'_, PolicyArmSummary> {
        match self {
            Self::Focused { results, .. } => results.iter(),
        }
    }

    fn admits(&self) -> bool {
        self.iter().all(|result| result.state.admits())
    }

    fn validate(&self, gate_ids: &[GateId], source: &wire::Digest) -> Result<(), ChangeGateError> {
        match self {
            Self::Focused {
                source: observed,
                results,
            } => {
                validate_policy_results(results, gate_ids)?;
                validate_policy_source(observed, source, "focused")?;
            }
        }
        Ok(())
    }
}

fn validate_policy_source(
    observed: &wire::Digest,
    expected: &wire::Digest,
    coverage: &str,
) -> Result<(), ChangeGateError> {
    if observed != expected {
        return Err(ChangeGateError::protocol(
            "change-gate-source-moved",
            format!("{coverage} policy summary observed a different staged source"),
        ));
    }
    Ok(())
}

fn validate_policy_results(
    results: &[PolicyArmSummary],
    gate_ids: &[GateId],
) -> Result<(), ChangeGateError> {
    if results.len() != gate_ids.len() {
        return Err(ChangeGateError::protocol(
            "change-gate-policy-incomplete",
            "policy summary does not exactly cover the selected arm inventory",
        ));
    }
    let manifest = BatteryManifest::canonical()
        .map_err(|error| ChangeGateError::protocol("change-gate-policy-manifest-invalid", error))?;
    for (result, gate_id) in results.iter().zip(gate_ids) {
        let Some(arm) = manifest.arms().get(result.position) else {
            return Err(ChangeGateError::protocol(
                "change-gate-policy-incomplete",
                format!(
                    "policy summary position {} is not canonical",
                    result.position
                ),
            ));
        };
        let ArmDisposition::Compiled {
            gate_id: expected_gate,
            family: expected_family,
            declaration: expected_declaration,
        } = arm.disposition();
        if result.gate_id != *gate_id
            || result.gate_id != *expected_gate
            || result.family != *expected_family
            || result.declaration != *expected_declaration
        {
            return Err(ChangeGateError::protocol(
                "change-gate-policy-incomplete",
                format!(
                    "policy summary position {} disagrees with the canonical manifest",
                    result.position,
                ),
            ));
        }
        if result.reason.trim().is_empty() || result.reason.contains(['\0', '\r']) {
            return Err(ChangeGateError::protocol(
                "change-gate-policy-invalid",
                format!("policy summary {} has an invalid reason", result.gate_id),
            ));
        }
    }
    Ok(())
}

/// Compact terminal identity for one fully validated policy result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyArmSummary {
    position: usize,
    gate_id: GateId,
    family: ArmFamily,
    declaration: String,
    state: BatteryState,
    reason: String,
    authority: wire::Digest,
}

impl PolicyArmSummary {
    fn from_result(result: &BatteryArmResult, authority: wire::Digest) -> Self {
        Self {
            position: result.position(),
            gate_id: result.gate_id(),
            family: result.family(),
            declaration: result.declaration().to_owned(),
            state: result.state(),
            reason: result.reason().to_owned(),
            authority,
        }
    }

    /// Canonical gate identity.
    #[must_use]
    pub const fn gate_id(&self) -> GateId {
        self.gate_id
    }

    /// Terminal state from the validated policy result.
    #[must_use]
    pub const fn state(&self) -> BatteryState {
        self.state
    }

    /// Exact bounded terminal reason.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Canonical selected tests and scope-shaped policy evidence for one staged source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeGateReport {
    version: u64,
    selection: ChangeSelectionReport,
    tests: TestSelectionReport,
    policy: ChangePolicyReport,
}

impl ChangeGateReport {
    fn new(
        selection: ChangeSelectionReport,
        tests: TestSelectionReport,
        policy: ChangePolicyReport,
    ) -> Result<Self, ChangeGateError> {
        let report = Self {
            version: CHANGE_GATE_REPORT_VERSION,
            selection,
            tests,
            policy,
        };
        report.validate()?;
        Ok(report)
    }

    /// Current combined report schema.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Exact staged-path-to-policy selection evidence.
    #[must_use]
    pub const fn selection(&self) -> &ChangeSelectionReport {
        &self.selection
    }

    /// Exact staged-source-to-test-engine execution evidence.
    #[must_use]
    pub const fn tests(&self) -> &TestSelectionReport {
        &self.tests
    }

    /// Canonical policy evidence in selected manifest order.
    #[must_use]
    pub const fn policy(&self) -> &ChangePolicyReport {
        &self.policy
    }

    /// Whether every selected test and policy arm admitted.
    #[must_use]
    pub fn admits(&self) -> bool {
        self.tests.admits() && self.policy.admits()
    }

    /// Compact deterministic evidence for selected policy refusals.
    #[must_use]
    pub fn refusal_fold(&self) -> String {
        let mut refusal = String::new();
        for result in self.policy.iter().filter(|result| !result.state().admits()) {
            let _ = writeln!(
                refusal,
                "gate={} state={} reason={}",
                result.gate_id(),
                result.state().as_str(),
                result.reason(),
            );
        }
        refusal
    }

    /// Canonical JSON bytes for this ordinary-gate report.
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        wire::encode(self)
    }

    /// Decode only canonical, source-consistent selected evidence.
    pub fn decode(bytes: &[u8]) -> Result<Self, ChangeGateError> {
        let report: Self = wire::decode(bytes)
            .map_err(|error| ChangeGateError::protocol("change-gate-report-invalid", error))?;
        report.validate()?;
        let canonical = report
            .encode()
            .map_err(|error| ChangeGateError::protocol("change-gate-report-unencodable", error))?;
        if canonical != bytes {
            return Err(ChangeGateError::protocol(
                "change-gate-report-noncanonical",
                "ordinary change-gate report bytes are not canonical",
            ));
        }
        Ok(report)
    }

    fn validate(&self) -> Result<(), ChangeGateError> {
        if self.version != CHANGE_GATE_REPORT_VERSION {
            return Err(ChangeGateError::protocol(
                "change-gate-report-version",
                format!(
                    "unsupported ordinary change-gate version {}; expected {CHANGE_GATE_REPORT_VERSION}",
                    self.version,
                ),
            ));
        }
        let source = self.selection.source();
        if self.tests.source() != source {
            return Err(ChangeGateError::protocol(
                "change-gate-source-mismatch",
                "test selection and policy selection name different staged sources",
            ));
        }
        if self
            .tests
            .changed_sources()
            .iter()
            .any(|changed| !self.selection.changed_sources().contains(changed))
        {
            return Err(ChangeGateError::protocol(
                "change-gate-change-mismatch",
                "test selection names a source absent from policy selection",
            ));
        }
        self.policy.validate(self.selection.gate_ids(), source)?;
        Ok(())
    }
}

struct ChangeGateExecution {
    selected_root: PathBuf,
    writable_root: PathBuf,
    base_tree: Option<gate::SelectedTree>,
    test_source: wire::Digest,
    test_changes: Vec<String>,
    test_execution: TestExecution,
    budget: CargoBudget,
    environment: Environment,
}

struct PartialStateContext<'a> {
    ambient: &'a Ambient,
    selection: &'a ChangeSelectionReport,
    source: &'a wire::Digest,
    environment: &'a Environment,
    surface: SelectionSurface,
    claim: gate::ClaimCapture,
    budget: CargoBudget,
}

/// Execute the ordinary staged-change gate without an ambient repository-wide selector.
///
/// The staging arm owns the first immutable capture. Its exact changes feed the
/// pure policy selector and every explicitly enabled packaged test engine.
/// Every other selected arm is a focused canonical run whose source digest
/// must match that capture.
///
/// # Errors
///
/// Returns repository/environment faults, selected-source movement, incomplete
/// test reach, or canonical focused-arm acquisition failures.
pub fn run(
    progress: &mut (impl Write + Send),
    ambient: &Ambient,
) -> Result<ChangeGateReport, ChangeGateError> {
    run_with_claim(progress, ambient, gate::ClaimCapture::Idle)
}

/// Execute the staged-change gate with the board claim captured by its caller.
///
/// # Errors
///
/// Returns the same staged selection, test, policy, and capture failures as [`run`].
pub fn run_with_claim(
    progress: &mut (impl Write + Send),
    ambient: &Ambient,
    claim: gate::ClaimCapture,
) -> Result<ChangeGateReport, ChangeGateError> {
    run_with_selection(progress, ambient, SelectionSurface::Packaged, claim)
}

/// Execute the richer current-source selector from the explicit xtask rim.
///
/// Every capture uses the strict self-hosting inputs: both tracked contract
/// documents and the complete compiled tool preflight are required.
///
/// # Errors
///
/// Returns the same staged selection, test, or policy failures as [`run`].
pub fn run_current_source(
    progress: &mut (impl Write + Send),
    ambient: &Ambient,
) -> Result<ChangeGateReport, ChangeGateError> {
    run_with_selection(
        progress,
        ambient,
        SelectionSurface::CurrentSource,
        gate::ClaimCapture::Idle,
    )
}

/// Immutable staged anchor and the exact packaged policy selection derived from it.
pub struct StagedActionSelection {
    anchor: crate::gate::PreparedArm<'static>,
    selection: ChangeSelectionReport,
    cargo_tests: bool,
}

impl StagedActionSelection {
    /// Retained staged capture from which selected actions can be derived.
    #[must_use]
    pub fn anchor(&self) -> &crate::gate::PreparedArm<'static> {
        &self.anchor
    }

    /// Exact packaged selection compiled from the retained staged tree.
    #[must_use]
    pub const fn selection(&self) -> &ChangeSelectionReport {
        &self.selection
    }

    /// Whether packaged policy selected canonical Cargo workspace tests.
    #[must_use]
    pub const fn cargo_tests(&self) -> bool {
        self.cargo_tests
    }

    /// Discard any focused state observations accumulated by derived actions.
    pub fn reject_state(&self) -> Result<(), crate::gate::RunError> {
        self.anchor.reject_state()
    }

    /// Return the retained staged anchor after an enclosing admission consumes the selection.
    #[must_use]
    pub fn into_anchor(self) -> crate::gate::PreparedArm<'static> {
        self.anchor
    }
}

/// Capture staged content for a fingerprint lookup without constructing packaged selection.
///
/// # Errors
///
/// Returns the same staged capture failures as [`run`].
pub fn prepare_staged_anchor(
    ambient: &Ambient,
) -> Result<crate::gate::PreparedArm<'static>, ChangeGateError> {
    SelectionSurface::Packaged
        .prepare_anchor(ambient, gate::ClaimCapture::Idle)
        .map_err(ChangeGateError::gate)
}

/// Apply packaged action selection to an already captured staged anchor.
pub fn prepare_staged_actions_from_anchor(
    ambient: &Ambient,
    anchor: crate::gate::PreparedArm<'static>,
) -> Result<StagedActionSelection, ChangeGateError> {
    let environment = child_environment(ambient)?;
    let (selection, tests) = select_changes(&anchor, SelectionSurface::Packaged, &environment)?;
    let cargo_tests = tests.cargo_selected(anchor.root(), anchor.changes());
    Ok(StagedActionSelection {
        anchor,
        selection,
        cargo_tests,
    })
}

/// Restore a previously validated packaged selection over the exact retained staged anchor.
///
/// # Errors
///
/// Refuses when the persisted selection does not name the anchor's source and changed paths.
pub fn restore_staged_actions(
    anchor: crate::gate::PreparedArm<'static>,
    selection: ChangeSelectionReport,
    cargo_tests: bool,
) -> Result<StagedActionSelection, ChangeGateError> {
    let encoded = selection.encode().map_err(|error| {
        ChangeGateError::protocol("change-gate-selection-invalid", error.to_string())
    })?;
    let selection = ChangeSelectionReport::decode(&encoded).map_err(|error| {
        ChangeGateError::protocol("change-gate-selection-invalid", error.to_string())
    })?;
    if selection.source() != anchor.source() {
        return Err(ChangeGateError::protocol(
            "change-gate-selection-source-mismatch",
            "persisted selection names a different staged source",
        ));
    }
    if selection.changed_sources() != anchor.changes() {
        return Err(ChangeGateError::protocol(
            "change-gate-selection-path-mismatch",
            "persisted selection names different changed sources",
        ));
    }
    if cargo_tests && !selection.gate_ids().contains(&GateId::RustTypecheck) {
        return Err(ChangeGateError::protocol(
            "change-gate-selection-cargo-mismatch",
            "persisted Cargo tests require the selected Rust typecheck arm",
        ));
    }
    Ok(StagedActionSelection {
        anchor,
        selection,
        cargo_tests,
    })
}

#[derive(Clone, Copy)]
enum SelectionSurface {
    Packaged,
    CurrentSource,
}

impl SelectionSurface {
    fn prepare_anchor(
        self,
        ambient: &Ambient,
        claim: gate::ClaimCapture,
    ) -> Result<crate::gate::PreparedArm<'static>, crate::gate::RunError> {
        match self {
            Self::Packaged => crate::gate::prepare_arm_with_claim(
                ambient,
                Selector::staged(),
                GateId::Staging,
                claim,
            ),
            Self::CurrentSource => {
                crate::gate::prepare_arm_strict(ambient, Selector::staged(), GateId::Staging)
            }
        }
    }
}

fn run_with_selection(
    progress: &mut (impl Write + Send),
    ambient: &Ambient,
    surface: SelectionSurface,
    claim: gate::ClaimCapture,
) -> Result<ChangeGateReport, ChangeGateError> {
    let total_started = Instant::now();
    publish_phase(progress, "capture", None)?;
    let capture_started = Instant::now();
    let budget = CargoBudget::detect(gate::WORKSPACE_TEST_CONCURRENCY_CAP);
    publish_budget(progress, budget, "prioritized-bounded-policy-then-tests")?;
    let environment = child_environment(ambient)?;
    let retry_claim = claim.clone();
    let mut anchor = surface
        .prepare_anchor(ambient, claim)
        .map_err(ChangeGateError::gate)?;
    publish_phase_terminal(progress, "capture", capture_started, "success")?;
    publish_phase(progress, "selection", Some(anchor.changes().len()))?;
    let selection_started = Instant::now();
    let (selection, test_execution) = select_changes(&anchor, surface, &environment)?;
    publish_selection(progress, &selection)?;
    publish_phase_terminal(progress, "selection", selection_started, "success")?;

    let repository = Repository::discover(Path::new(".")).map_err(|error| ChangeGateError {
        reason: crate::repository::ResolveError::REASON.to_owned(),
        detail: error.to_string(),
        fault: true,
    })?;
    let base_tree =
        if gate::cargo_workspace_base_inventory_required(anchor.root(), anchor.changes())
            .map_err(|error| ChangeGateError::refusal("base-tree-ownership-refused", error))?
        {
            Some(
                anchor
                    .materialize_staged_base()
                    .map_err(ChangeGateError::gate)?
                    .ok_or_else(|| {
                        ChangeGateError::refusal(
                            "base-tree-ownership-refused",
                            "removed Rust source requires a committed staged base tree",
                        )
                    })?,
            )
        } else {
            None
        };
    let execution = ChangeGateExecution {
        selected_root: anchor.root().to_path_buf(),
        writable_root: repository.worktree_root().to_path_buf(),
        base_tree,
        test_source: anchor.source().clone(),
        test_changes: anchor.changes().to_vec(),
        test_execution,
        budget,
        environment: environment.clone(),
    };
    let source = selection.source().clone();
    let mut shared_progress = SharedProgress::new(progress);
    let execution_started = Instant::now();
    let (tests, policy) = run_policy_and_tests(
        &mut anchor,
        &selection,
        &source,
        execution,
        &mut shared_progress,
    )?;
    publish_phase_terminal(
        &mut shared_progress,
        "policy-tests",
        execution_started,
        "success",
    )?;
    let publication_started = Instant::now();
    publish_test_selection(&mut shared_progress, &tests)?;
    if !tests.admits() {
        let evidence = String::from_utf8(
            tests
                .encode()
                .map_err(|error| ChangeGateError::protocol("test-selection-unencodable", error))?,
        )
        .map_err(|error| ChangeGateError::protocol("test-selection-non-utf8", error))?;
        return Err(ChangeGateError::refusal(
            "change-gate-test-coverage-unknown",
            evidence,
        ));
    }

    let policy = settle_partial_state(
        &mut anchor,
        PartialStateContext {
            ambient,
            selection: &selection,
            source: &source,
            environment: &environment,
            surface,
            claim: retry_claim,
            budget,
        },
        policy,
        &mut shared_progress,
    )?;
    let report = ChangeGateReport::new(selection, tests, policy)?;
    publish_phase_terminal(
        &mut shared_progress,
        "publication",
        publication_started,
        "success",
    )?;
    publish_phase_terminal(&mut shared_progress, "complete", total_started, "success")?;
    Ok(report)
}

fn settle_partial_state<W: Write + Send + ?Sized>(
    anchor: &mut crate::gate::PreparedArm<'static>,
    context: PartialStateContext<'_>,
    mut policy: ChangePolicyReport,
    progress: &mut SharedProgress<'_, W>,
) -> Result<ChangePolicyReport, ChangeGateError> {
    for attempt in 1..=STATE_PUBLICATION_ATTEMPTS {
        if !policy.admits() {
            anchor.reject_state().map_err(ChangeGateError::gate)?;
            return Ok(policy);
        }
        match anchor.accept_state(StateRunScope::Partial) {
            Ok(()) => return Ok(policy),
            Err(error)
                if error.is_stale_state_publication() && attempt < STATE_PUBLICATION_ATTEMPTS =>
            {
                writeln!(
                    progress,
                    "[change-gate-state] disposition=stale-retry publication_attempt={attempt}",
                )
                .map_err(|error| {
                    ChangeGateError::environment("change-gate-progress-unavailable", error)
                })?;
                progress.flush().map_err(|error| {
                    ChangeGateError::environment("change-gate-progress-unavailable", error)
                })?;

                let mut retry = context
                    .surface
                    .prepare_anchor(context.ambient, context.claim.clone())
                    .map_err(ChangeGateError::gate)?;
                let (retry_selection, _) =
                    select_changes(&retry, context.surface, context.environment)?;
                if &retry_selection != context.selection || retry.source() != context.source {
                    return Err(ChangeGateError::refusal(
                        "change-gate-source-moved",
                        "staged source or selected policy changed while retrying state publication",
                    ));
                }
                let scheduler = WorkerScheduler::new(context.budget.worker_processes());
                let focused = run_focused_policy(
                    &mut retry,
                    context.selection,
                    context.source,
                    context.budget,
                    &scheduler,
                    BTreeMap::new(),
                    progress,
                )?;
                policy = ChangePolicyReport::focused_summaries(context.source, focused.summaries);
                policy.validate(context.selection.gate_ids(), context.source)?;
                *anchor = retry;
            }
            Err(error) => return Err(ChangeGateError::gate(error)),
        }
    }
    unreachable!("the fixed state publication attempt loop always returns")
}

fn select_changes(
    anchor: &crate::gate::PreparedArm<'_>,
    surface: SelectionSurface,
    environment: &Environment,
) -> Result<(ChangeSelectionReport, TestExecution), ChangeGateError> {
    let (selection, test_execution) = match surface {
        SelectionSurface::Packaged => {
            let policy = packaged_policy(anchor.root(), anchor.paths(), environment)?;
            let selection = gate::select_change_gates(
                anchor.source().clone(),
                anchor.changes(),
                &policy.selection,
            )
            .and_then(|selection| {
                gate::compile_language_reach(
                    anchor.root(),
                    anchor.paths(),
                    policy.selection.package_roots(),
                    policy.selection.test_paths(),
                    selection,
                )
            });
            (
                selection,
                TestExecution::Packaged {
                    engines: policy.test_engines,
                },
            )
        }
        SelectionSurface::CurrentSource => {
            let policy = current_source_policy(anchor.root())?;
            let selection = gate::select_current_source_change_gates(
                anchor.source().clone(),
                anchor.paths(),
                anchor.changes(),
                &policy.generated_paths,
                &policy.rules,
            )
            .and_then(|selection| {
                gate::compile_language_reach(anchor.root(), anchor.paths(), &[], &[], selection)
            });
            (selection, TestExecution::CurrentSourceCargo)
        }
    };
    let selection =
        selection.map_err(|error| ChangeGateError::refusal("change-selection-refused", error))?;
    Ok((selection, test_execution))
}

fn publish_selection(
    progress: &mut (impl Write + Send),
    selection: &ChangeSelectionReport,
) -> Result<(), ChangeGateError> {
    writeln!(
        progress,
        "[change-gate-selection] mode={:?} changes={} policy_arms={}",
        selection.mode(),
        selection.changed_sources().len(),
        selection.gate_ids().len(),
    )
    .map_err(|error| ChangeGateError::environment("change-gate-progress-unavailable", error))?;
    writeln!(
        progress,
        "[change-gate-selection-report] detail=canonical-report source={} changes={} arms={} edges={}",
        selection.source(),
        selection.changed_sources().len(),
        selection.gate_ids().len(),
        selection.edges().len(),
    )
    .map_err(|error| ChangeGateError::environment("change-gate-progress-unavailable", error))?;
    progress
        .flush()
        .map_err(|error| ChangeGateError::environment("change-gate-progress-unavailable", error))
}

fn publish_phase(
    progress: &mut (impl Write + Send),
    stage: &str,
    changes: Option<usize>,
) -> Result<(), ChangeGateError> {
    match changes {
        Some(changes) => writeln!(
            progress,
            "[change-gate-phase] stage={stage} changes={changes}"
        ),
        None => writeln!(progress, "[change-gate-phase] stage={stage}"),
    }
    .map_err(|error| ChangeGateError::environment("change-gate-progress-unavailable", error))?;
    progress
        .flush()
        .map_err(|error| ChangeGateError::environment("change-gate-progress-unavailable", error))
}

fn publish_phase_terminal(
    progress: &mut (impl Write + Send + ?Sized),
    stage: &str,
    started: Instant,
    disposition: &str,
) -> Result<(), ChangeGateError> {
    writeln!(
        progress,
        "[change-gate-phase] stage={stage}-terminal elapsed_ms={} disposition={disposition}",
        started.elapsed().as_millis(),
    )
    .map_err(|error| ChangeGateError::environment("change-gate-progress-unavailable", error))?;
    progress
        .flush()
        .map_err(|error| ChangeGateError::environment("change-gate-progress-unavailable", error))
}

fn publish_budget(
    progress: &mut (impl Write + Send),
    budget: CargoBudget,
    scheduling: &str,
) -> Result<(), ChangeGateError> {
    writeln!(
        progress,
        concat!(
            "[change-gate-budget] cpus={} ordinary_lanes={} advisory_share={} worker_process_cap={} ",
            "policy_jobs={} native_policy_jobs={} policy_peak={} test_concurrency={} shard_jobs={} test_threads={} ",
            "worker_scope=phase-peaks policy_and_tests=nonoverlapping scheduling={}"
        ),
        budget.cpus(),
        budget.ordinary_lanes(),
        budget.advisory_lane_share(),
        budget.worker_processes(),
        budget.policy_jobs(),
        budget.native_policy_jobs(),
        budget.policy_slots(),
        budget.test_concurrency(),
        budget.shard_jobs(),
        budget.test_threads(),
        scheduling,
    )
    .map_err(|error| ChangeGateError::environment("change-gate-progress-unavailable", error))?;
    if budget.exceeds_advisory_lane_share() {
        writeln!(
            progress,
            concat!(
                "[change-gate-budget-warning] reason=advisory-lane-share-exceeded ",
                "host_capacity={} ordinary_lanes={} advisory_share={} requested_slots={} ",
                "allocations=policy_peak:{} test_peak:{}x{}={} ",
                "action=continue-unchanged"
            ),
            budget.cpus(),
            budget.ordinary_lanes(),
            budget.advisory_lane_share(),
            budget.requested_slots(),
            budget.policy_slots(),
            budget.test_concurrency(),
            budget.shard_jobs(),
            budget.test_slots(),
        )
        .map_err(|error| ChangeGateError::environment("change-gate-progress-unavailable", error))?;
    }
    progress
        .flush()
        .map_err(|error| ChangeGateError::environment("change-gate-progress-unavailable", error))
}

fn run_policy_and_tests<W: Write + Send + ?Sized>(
    anchor: &mut crate::gate::PreparedArm<'_>,
    selection: &ChangeSelectionReport,
    source: &wire::Digest,
    execution: ChangeGateExecution,
    progress: &mut SharedProgress<'_, W>,
) -> Result<(TestSelectionReport, ChangePolicyReport), ChangeGateError> {
    let ChangeGateExecution {
        selected_root,
        writable_root,
        base_tree,
        test_source,
        test_changes,
        test_execution,
        budget,
        environment,
    } = execution;
    let scheduler = WorkerScheduler::new(budget.worker_processes());
    let preexecuted =
        preexecute_policy_screen(anchor, selection, source, budget, &scheduler, progress)?;
    let test_run = TestRun::new(
        &selected_root,
        &writable_root,
        test_source,
        &test_changes,
        budget,
        environment,
    );
    let test_run = match base_tree.as_ref() {
        Some(base) => test_run.with_base(base.root()),
        None => test_run,
    };
    let test_run = match preexecuted.rust_workspace.as_ref() {
        Some(workspace) => test_run.with_reusable(workspace),
        None => test_run,
    };
    let policy_started = Instant::now();
    let focused = run_focused_policy(
        anchor,
        selection,
        source,
        budget,
        &scheduler,
        preexecuted.reports,
        progress,
    );
    let policy_disposition = if focused.is_ok() { "success" } else { "failed" };
    publish_phase_terminal(
        progress,
        "policy-remainder",
        policy_started,
        policy_disposition,
    )?;
    let focused = focused?;
    let test_started = Instant::now();
    let tests = run_selected_tests(test_run, test_execution, progress);
    let test_disposition = if tests.is_ok() { "success" } else { "failed" };
    publish_phase_terminal(progress, "tests", test_started, test_disposition)?;
    let tests = tests?;
    let policy = ChangePolicyReport::focused_summaries(source, focused.summaries);
    Ok((tests, policy))
}

struct FocusedPolicyExecution {
    summaries: Vec<PolicyArmSummary>,
}

#[derive(Clone, Copy)]
struct ParallelPolicyContext<'a, 'prepared> {
    anchor: &'a crate::gate::PreparedArm<'prepared>,
    selection: &'a ChangeSelectionReport,
    source: &'a wire::Digest,
    budget: CargoBudget,
    scheduler: &'a WorkerScheduler,
}

struct PreexecutedPolicy {
    reports: BTreeMap<GateId, gate::FocusedArmReport>,
    rust_workspace: Option<gate::RustAnalysisWorkspace>,
}

fn preexecute_policy_screen<W: Write + Send + ?Sized>(
    anchor: &mut crate::gate::PreparedArm<'_>,
    selection: &ChangeSelectionReport,
    source: &wire::Digest,
    budget: CargoBudget,
    scheduler: &WorkerScheduler,
    progress: &mut SharedProgress<'_, W>,
) -> Result<PreexecutedPolicy, ChangeGateError> {
    let mut prepared = Vec::new();
    for reach in selection
        .reach()
        .iter()
        .filter(|reach| pretest_screen_gate(reach.gate_id()))
    {
        let gate_id = reach.gate_id();
        publish_policy_start(progress, gate_id)?;
        let arm = prepare_policy_arm(anchor, selection, reach, budget, progress)?;
        prepared.push((gate_id, arm));
    }

    let completed = std::thread::scope(|scope| {
        let handles = prepared
            .into_iter()
            .map(|(gate_id, mut arm)| {
                let started = Instant::now();
                let handle = scope.spawn(move || {
                    let result = (|| {
                        let _permit = scheduler.acquire()?;
                        let report = arm.execute().map_err(ChangeGateError::gate)?;
                        let workspace = (gate_id == GateId::RustTypecheck)
                            .then(|| arm.reusable_rust_workspace().cloned())
                            .flatten();
                        Ok::<_, ChangeGateError>((report, workspace))
                    })();
                    (result, started.elapsed())
                });
                (gate_id, handle)
            })
            .collect::<Vec<_>>();
        let mut completed = Vec::with_capacity(handles.len());
        for (gate_id, handle) in handles {
            let (result, elapsed) = handle.join().map_err(|_| {
                ChangeGateError::protocol(
                    "change-gate-policy-panicked",
                    format!("focused {} policy arm panicked", gate_id.as_str()),
                )
            })?;
            completed.push((gate_id, result, elapsed));
        }
        Ok::<_, ChangeGateError>(completed)
    })?;

    let mut reports = BTreeMap::new();
    let mut rust_workspace = None;
    for (gate_id, result, elapsed) in completed {
        let disposition = match &result {
            Ok((report, _)) if report.admits() => "completed",
            Ok(_) => "refused",
            Err(_) => "failed",
        };
        publish_policy_terminal_elapsed(progress, gate_id, elapsed, disposition)?;
        let (report, workspace) = result?;
        validate_focused_policy_source(&report, source)?;
        if gate_id == GateId::RustTypecheck {
            rust_workspace = workspace;
        }
        reports.insert(gate_id, report);
    }
    if reports.values().any(|report| !report.admits()) {
        anchor.reject_state().map_err(ChangeGateError::gate)?;
        let reports = reports.into_values().collect::<Vec<_>>();
        return Err(ChangeGateError::refusal(
            "change-gate-policy-regressed",
            gate::BatteryRefusalFold::from_focused_reports(&reports),
        ));
    }
    Ok(PreexecutedPolicy {
        reports,
        rust_workspace,
    })
}

const fn pretest_screen_gate(gate_id: GateId) -> bool {
    matches!(
        gate_id,
        GateId::RustTypecheck | GateId::AppWebTypecheck | GateId::Complexity
    )
}

fn run_focused_policy(
    anchor: &mut crate::gate::PreparedArm<'_>,
    selection: &ChangeSelectionReport,
    source: &wire::Digest,
    budget: CargoBudget,
    scheduler: &WorkerScheduler,
    mut preexecuted: BTreeMap<GateId, gate::FocusedArmReport>,
    progress: &mut impl Write,
) -> Result<FocusedPolicyExecution, ChangeGateError> {
    let mut reports = BTreeMap::new();
    let mut parallel = Vec::new();
    for reach in selection.reach() {
        let gate_id = reach.gate_id();
        if let Some(report) = preexecuted.remove(&gate_id) {
            insert_policy_report(&mut reports, report)?;
            continue;
        }
        if gate_id == GateId::Staging {
            execute_parallel_policy_segment(
                ParallelPolicyContext {
                    anchor,
                    selection,
                    source,
                    budget,
                    scheduler,
                },
                &mut parallel,
                &mut reports,
                progress,
            )?;
            publish_policy_start(progress, gate_id)?;
            let started = Instant::now();
            let _permit = scheduler.acquire()?;
            anchor
                .bind_cargo_jobs(budget.policy_jobs())
                .map_err(ChangeGateError::gate)?;
            publish_execution_scope(
                progress,
                gate_id,
                reach.objects(),
                &[ChangeReachObject::Repository],
            )?;
            let report = anchor.execute().map_err(ChangeGateError::gate);
            let disposition = if report.is_ok() {
                "completed"
            } else {
                "failed"
            };
            publish_policy_terminal(progress, gate_id, started, disposition)?;
            insert_policy_report(&mut reports, report?)?;
            continue;
        }
        parallel.push(reach);
    }
    execute_parallel_policy_segment(
        ParallelPolicyContext {
            anchor,
            selection,
            source,
            budget,
            scheduler,
        },
        &mut parallel,
        &mut reports,
        progress,
    )?;
    let mut policy = Vec::with_capacity(selection.reach().len());
    for reach in selection.reach() {
        let gate_id = reach.gate_id();
        let report = reports.remove(&gate_id).ok_or_else(|| {
            ChangeGateError::protocol(
                "change-gate-policy-incomplete",
                format!(
                    "focused policy scheduler returned no {} report",
                    gate_id.as_str()
                ),
            )
        })?;
        record_policy_report(&mut policy, report, source)?;
    }
    Ok(FocusedPolicyExecution { summaries: policy })
}

fn execute_parallel_policy_segment(
    context: ParallelPolicyContext<'_, '_>,
    reaches: &mut Vec<&gate::ChangeArmReach>,
    reports: &mut BTreeMap<GateId, gate::FocusedArmReport>,
    progress: &mut impl Write,
) -> Result<(), ChangeGateError> {
    if reaches.is_empty() {
        return Ok(());
    }
    let reaches = std::mem::take(reaches);
    let mut scheduled = reaches.iter().copied().enumerate().collect::<Vec<_>>();
    scheduled.sort_by_key(|(index, reach)| (policy_schedule_rank(reach.gate_id()), *index));
    publish_policy_schedule(progress, context.budget.worker_processes(), &scheduled)?;
    let progress = RefCell::new(progress);
    let completions = policy_schedule::run(
        scheduled,
        context.budget.worker_processes(),
        |reach| {
            let mut progress = progress.borrow_mut();
            publish_policy_start(&mut **progress, reach.gate_id())?;
            prepare_policy_arm(
                context.anchor,
                context.selection,
                reach,
                context.budget,
                &mut **progress,
            )
        },
        |prepared| execute_prepared_policy_arm(prepared, context.scheduler),
        |index, elapsed, outcome| {
            let gate_id = reaches[index].gate_id();
            let disposition = if matches!(outcome, policy_schedule::Outcome::Returned(Ok(_))) {
                "completed"
            } else {
                "failed"
            };
            publish_policy_terminal_elapsed(
                &mut **progress.borrow_mut(),
                gate_id,
                elapsed,
                disposition,
            )
        },
    );

    let mut first_error = None;
    for completion in completions {
        let gate_id = reaches[completion.index].gate_id();
        let result = match completion.outcome {
            policy_schedule::Outcome::Returned(result) => result,
            policy_schedule::Outcome::Panicked => Err(ChangeGateError::protocol(
                "change-gate-policy-panicked",
                format!("focused {} policy arm panicked", gate_id.as_str()),
            )),
        };
        if first_error.is_none() {
            match result {
                Ok(report) => {
                    validate_focused_policy_source(&report, context.source)?;
                    insert_policy_report(reports, report)?;
                }
                Err(error) => first_error = Some(error),
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn insert_policy_report(
    reports: &mut BTreeMap<GateId, gate::FocusedArmReport>,
    report: gate::FocusedArmReport,
) -> Result<(), ChangeGateError> {
    let gate_id = report.gate_id();
    if reports.insert(gate_id, report).is_some() {
        return Err(ChangeGateError::protocol(
            "change-gate-policy-duplicate",
            format!("focused policy scheduler repeated {}", gate_id.as_str()),
        ));
    }
    Ok(())
}

fn publish_policy_schedule(
    progress: &mut impl Write,
    concurrency: usize,
    scheduled: &[(usize, &gate::ChangeArmReach)],
) -> Result<(), ChangeGateError> {
    let order = scheduled
        .iter()
        .map(|(_, reach)| reach.gate_id().as_str())
        .collect::<Vec<_>>()
        .join(",");
    writeln!(
        progress,
        "[change-gate-policy-schedule] concurrency={concurrency} order={order}",
    )
    .map_err(|error| ChangeGateError::environment("change-gate-progress-unavailable", error))?;
    progress
        .flush()
        .map_err(|error| ChangeGateError::environment("change-gate-progress-unavailable", error))
}

const fn policy_schedule_rank(gate_id: GateId) -> u8 {
    match gate_id {
        GateId::RustTypecheck | GateId::AppWebTypecheck | GateId::Complexity => 0,
        GateId::EnvNameLedger | GateId::MarkdownLinks | GateId::Formatters => 1,
        GateId::MagicNumbers | GateId::JavascriptUnused => 2,
        _ => 3,
    }
}

fn publish_policy_start(progress: &mut impl Write, gate_id: GateId) -> Result<(), ChangeGateError> {
    writeln!(
        progress,
        "[change-gate-policy] gate={} disposition=executed",
        gate_id.as_str(),
    )
    .map_err(|error| ChangeGateError::environment("change-gate-progress-unavailable", error))?;
    progress
        .flush()
        .map_err(|error| ChangeGateError::environment("change-gate-progress-unavailable", error))
}

fn publish_policy_terminal(
    progress: &mut impl Write,
    gate_id: GateId,
    started: Instant,
    disposition: &str,
) -> Result<(), ChangeGateError> {
    publish_policy_terminal_elapsed(progress, gate_id, started.elapsed(), disposition)
}

fn publish_policy_terminal_elapsed(
    progress: &mut impl Write,
    gate_id: GateId,
    elapsed: Duration,
    disposition: &str,
) -> Result<(), ChangeGateError> {
    writeln!(
        progress,
        "[change-gate-policy] gate={} stage=terminal elapsed_ms={} disposition={disposition}",
        gate_id.as_str(),
        elapsed.as_millis(),
    )
    .map_err(|error| ChangeGateError::environment("change-gate-progress-unavailable", error))?;
    progress
        .flush()
        .map_err(|error| ChangeGateError::environment("change-gate-progress-unavailable", error))
}

fn prepare_policy_arm(
    anchor: &crate::gate::PreparedArm<'_>,
    selection: &ChangeSelectionReport,
    reach: &gate::ChangeArmReach,
    budget: CargoBudget,
    progress: &mut impl Write,
) -> Result<crate::gate::PreparedArm<'static>, ChangeGateError> {
    let gate_id = reach.gate_id();
    let mut prepared = anchor
        .derive_reached_arm(selection, gate_id)
        .map_err(ChangeGateError::gate)?;
    let cargo_jobs = if gate_id == GateId::RustTypecheck {
        budget.native_policy_jobs()
    } else {
        budget.policy_jobs()
    };
    prepared
        .bind_cargo_jobs(cargo_jobs)
        .map_err(ChangeGateError::gate)?;
    let execution_objects = prepared.execution_objects().ok_or_else(|| {
        ChangeGateError::protocol(
            "change-gate-reach-unbound",
            format!(
                "focused arm {} retained no execution object inventory",
                gate_id.as_str()
            ),
        )
    })?;
    publish_execution_scope(progress, gate_id, reach.objects(), execution_objects)?;
    Ok(prepared)
}

fn execute_prepared_policy_arm(
    mut prepared: crate::gate::PreparedArm<'static>,
    scheduler: &WorkerScheduler,
) -> Result<gate::FocusedArmReport, ChangeGateError> {
    let _permit = scheduler.acquire()?;
    prepared.execute().map_err(ChangeGateError::gate)
}

fn record_policy_report(
    policy: &mut Vec<PolicyArmSummary>,
    report: gate::FocusedArmReport,
    source: &wire::Digest,
) -> Result<(), ChangeGateError> {
    validate_focused_policy_source(&report, source)?;
    let encoded = report.encode().map_err(|error| {
        ChangeGateError::protocol("change-gate-focused-policy-unencodable", error)
    })?;
    policy.push(PolicyArmSummary::from_result(
        report.result(),
        wire::Digest::compute(wire::Domain::GateFocusedReport, &encoded),
    ));
    Ok(())
}

fn validate_focused_policy_source(
    report: &gate::FocusedArmReport,
    source: &wire::Digest,
) -> Result<(), ChangeGateError> {
    if report.context().source() != source {
        return Err(ChangeGateError::refusal(
            "change-gate-source-moved",
            format!(
                "staged source changed while executing focused arm {}",
                report.gate_id().as_str(),
            ),
        ));
    }
    Ok(())
}

fn publish_execution_scope(
    progress: &mut impl Write,
    gate_id: GateId,
    planned: &[ChangeReachObject],
    executed: &[ChangeReachObject],
) -> Result<(), ChangeGateError> {
    if executed != planned {
        return Err(ChangeGateError::protocol(
            "change-gate-reach-mismatch",
            format!(
                "focused arm {} bound an execution inventory different from its reach plan",
                gate_id.as_str()
            ),
        ));
    }
    let objects = serde_json::to_string(executed)
        .map_err(|error| ChangeGateError::protocol("change-gate-reach-unencodable", error))?;
    writeln!(
        progress,
        "[change-gate-execution] gate={} objects={objects}",
        gate_id.as_str(),
    )
    .map_err(|error| ChangeGateError::environment("change-gate-progress-unavailable", error))?;
    progress
        .flush()
        .map_err(|error| ChangeGateError::environment("change-gate-progress-unavailable", error))
}

fn child_environment(ambient: &Ambient) -> Result<Environment, ChangeGateError> {
    ambient
        .environment()
        .map_err(|error| ChangeGateError::environment("change-gate-environment-unavailable", error))
}

/// Fail-closed ordinary-gate execution or report failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChangeGateError {
    reason: String,
    detail: String,
    fault: bool,
}

impl ChangeGateError {
    fn gate(error: crate::gate::RunError) -> Self {
        Self {
            reason: error.reason().to_owned(),
            detail: error.detail().to_owned(),
            fault: error.is_fault(),
        }
    }

    fn refusal(reason: &str, detail: impl fmt::Display) -> Self {
        Self {
            reason: format!("gate::{reason}"),
            detail: detail.to_string(),
            fault: false,
        }
    }

    fn protocol(reason: &str, detail: impl fmt::Display) -> Self {
        Self {
            reason: format!("gate::{reason}"),
            detail: detail.to_string(),
            fault: true,
        }
    }

    fn environment(reason: &str, detail: impl fmt::Display) -> Self {
        Self {
            reason: format!("gate::{reason}"),
            detail: detail.to_string(),
            fault: true,
        }
    }

    /// Stable refusal or fault identity.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// Exact retained evidence.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// Whether this is an environmental/protocol fault rather than a refusal.
    #[must_use]
    pub const fn is_fault(&self) -> bool {
        self.fault
    }
}

impl fmt::Display for ChangeGateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.reason, self.detail)
    }
}

impl Error for ChangeGateError {}

#[cfg(test)]
#[path = "change_gate_tests.rs"]
mod tests;
