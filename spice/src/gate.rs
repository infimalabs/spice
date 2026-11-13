//! The one native command rim for a canonical gate battery.
//!
//! Every public gate entrypoint calls this module; no caller assembles a
//! contract or invokes another command.
//! Ambient process state is captured once here and handed to `gate` as typed
//! observations; acquisition and decisions remain owned by the gate crate.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::Path;
use std::process::Command;

use crate::ambient::Ambient;
use crate::cargo_isolation;
use crate::repository::{Repository, StateError};
use ::gate::{
    ActorCapture, BatteryCapture, BatteryReport, CaptureAuthorities, ChangeSelectionReport,
    ClaimCapture, FocusedArmReport, GateId, HygieneConfiguration, InputMode,
    RepositoryExecutionEvent, RimCapture, Selector, StateRunScope,
};
use gix::bstr::ByteSlice as _;
use wire::Digest;

const PROCESS_PATH: &str = concat!("PA", "TH");
const CARGO_BUILD_JOBS: &str = "CARGO_BUILD_JOBS";

pub mod arms;
mod candidate_tree;
mod capture;
mod claim;
mod environment;
pub mod sweep;
pub use claim::{AmbientClaimError, ambient_claim};
pub use environment::operational_routing;

use environment::compiler_environment;
pub use environment::git_local_environment;
#[cfg(test)]
use environment::{
    SHELL_HOOK_PREFIX, SHELL_LAST_COMMAND, compiler_environment_from,
    compiler_environment_from_with_cargo_home,
};

pub use candidate_tree::CandidateTree;

/// Resolve the tree identity represented by the exact staged-index authority.
///
/// The ambient `GIT_INDEX_FILE`, including its absence, is captured before Git
/// source acquisition. Relative alternate indexes are anchored at the selected
/// ordinary or linked worktree, matching Git's `-C` interpretation.
///
/// # Errors
///
/// Returns a typed gate-rim failure when the index cannot be sealed, parsed, or
/// represented as one canonical SHA-1 or SHA-256 tree identity.
pub fn capture_candidate_tree(
    repository: &Repository,
    ambient: &Ambient,
) -> Result<CandidateTree, RunError> {
    candidate_tree::capture_candidate_tree(repository, ambient.get("GIT_INDEX_FILE"))
}

/// One staged battery capture retained across explicit diagnostic actions.
pub struct PreparedBattery<'trace> {
    capture: BatteryCapture<'trace>,
    compiler_environment: Vec<(OsString, OsString)>,
    cargo_jobs: Option<usize>,
}

/// One retained staged capture selected for one canonical arm.
///
/// The ordinary change gate uses the always-on staging arm as its one source
/// capture. That gives test selection, policy selection, and the first focused
/// result one immutable root and digest without acquiring sibling families.
pub struct PreparedArm<'trace> {
    capture: BatteryCapture<'trace>,
    gate_id: GateId,
    compiler_environment: Vec<(OsString, OsString)>,
    cargo_jobs: Option<usize>,
}

impl PreparedArm<'_> {
    /// Identity of the immutable selected source retained by this preparation.
    #[must_use]
    pub const fn source(&self) -> &Digest {
        self.capture.source()
    }

    /// Private selected-tree root retained for this preparation's lifetime.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.capture.root()
    }

    /// Exact changed paths captured at the command rim for this source.
    #[must_use]
    pub fn changes(&self) -> &[String] {
        self.capture.changes()
    }

    /// Objects the validated ordinary reach plan bound to this recapture.
    #[must_use]
    pub fn execution_objects(&self) -> Option<&[::gate::ChangeReachObject]> {
        self.capture.execution_objects()
    }

    /// Stable source and outputs completed by this focused native Rust arm.
    #[must_use]
    pub fn reusable_rust_workspace(&self) -> Option<&::gate::RustAnalysisWorkspace> {
        self.capture.reusable_rust_workspace()
    }

    /// Every regular path retained by the immutable selected tree.
    #[must_use]
    pub fn paths(&self) -> &[String] {
        self.capture.paths()
    }

    /// Tracked generated-path selectors captured from this selected tree.
    #[must_use]
    pub fn generated_paths(&self) -> &[String] {
        self.capture.generated_paths()
    }

    /// Complete immutable authority context captured for this focused arm.
    pub fn context(&self) -> Result<::gate::BatteryContext, RunError> {
        self.capture
            .context()
            .map_err(|error| RunError::refusal("gate::context-refused", error))
    }

    /// Exact external executable evidence for this focused action.
    ///
    /// The probe captures identity only and performs none of the arm's
    /// analysis. Proof publication compares it with the executed report.
    pub fn action_tools(&self) -> Result<Vec<::gate::ToolEvidence>, RunError> {
        self.capture
            .action_tools(self.gate_id, &self.compiler_environment)
            .map_err(|error| RunError::refusal("gate::tool-authority-refused", error))
    }

    /// Complete executable-file authority used by proof fingerprint lookup.
    pub fn proof_tool_authority(&self) -> Result<Digest, RunError> {
        self.capture
            .proof_tool_authority(&self.compiler_environment)
            .map_err(|error| RunError::refusal("gate::tool-authority-refused", error))
    }

    /// Prove the live selected source has not moved since this preparation.
    ///
    /// # Errors
    ///
    /// Returns the exact staged index, `HEAD`, or repository movement.
    pub fn verify_source(&self) -> Result<(), RunError> {
        self.capture
            .verify_source()
            .map_err(|error| RunError::refusal("gate::source-moved", error))
    }

    /// Check a staged source while preserving index movement for a semantic recapture.
    ///
    /// This is narrower than [`Self::verify_source`]: revision and rename-base
    /// movement still refuse, while an index seal failure is returned as a
    /// typed state for the enclosing pre-commit reservation to reacquire.
    pub fn verify_refreshable_source(
        &self,
    ) -> Result<::gate::tree::RefreshableSourceState, RunError> {
        self.capture
            .verify_refreshable_source()
            .map_err(|error| RunError::refusal("gate::source-moved", error))
    }

    /// Canonical staged entries and exact rename lineage retained by this arm.
    #[must_use]
    pub fn staged_authority(&self) -> Option<&::gate::tree::StagedAuthority> {
        self.capture.staged_authority()
    }

    /// Which worktree paths this arm's private snapshot left out.
    ///
    /// # Errors
    ///
    /// Refuses when the worktree cannot be compared with the snapshot.
    pub fn worktree_divergence(&self) -> Result<::gate::tree::WorktreeDivergence, RunError> {
        self.capture
            .worktree_divergence()
            .map_err(|error| RunError::refusal("gate::snapshot-divergence-refused", error))
    }

    /// Compile one proof input closure from this preparation's retained tree.
    pub fn compile_action_input(
        &self,
        selection: &ChangeSelectionReport,
        gate_id: GateId,
        inventory: ::gate::ActionInputInventory,
    ) -> Result<::gate::ActionInputClosure, RunError> {
        self.capture
            .compile_action_input(selection, gate_id, inventory)
            .map_err(|error| RunError::refusal("gate::action-input-refused", error))
    }

    /// Compile one explicit proof sub-action closure from the retained tree.
    pub fn compile_explicit_action_input(
        &self,
        paths: Vec<String>,
        non_files: Vec<::gate::ActionNonFileInput>,
    ) -> Result<::gate::ActionInputManifest, RunError> {
        self.capture
            .compile_explicit_action_input(paths, non_files)
            .map_err(|error| RunError::refusal("gate::action-input-refused", error))
    }

    /// Compiler environment captured by the command rim for this action.
    #[must_use]
    pub fn compiler_environment(&self) -> &[(OsString, OsString)] {
        &self.compiler_environment
    }

    /// Materialize the exact committed base retained beside this staged source.
    ///
    /// # Errors
    ///
    /// Refuses when the retained repository object or private materialization
    /// cannot be read exactly.
    pub fn materialize_staged_base(&self) -> Result<Option<::gate::SelectedTree>, RunError> {
        self.capture
            .materialize_staged_base()
            .map_err(|error| RunError::refusal("gate::base-tree-refused", error))
    }

    /// Execute the one canonical focused arm this capture selected.
    ///
    /// # Errors
    ///
    /// Returns the canonical focused-arm refusal without acquiring siblings.
    pub fn execute(&mut self) -> Result<FocusedArmReport, RunError> {
        self.capture
            .execute_arm_report_with_environment(self.gate_id, self.compiler_environment.clone())
            .map_err(|error| RunError::refusal("gate::battery-refused", error))
    }

    /// Cap Cargo work performed by this focused arm without changing compiler semantics.
    ///
    /// # Errors
    ///
    /// Refuses a different second budget after this retained capture was bound.
    pub fn bind_cargo_jobs(&mut self, cargo_jobs: usize) -> Result<(), RunError> {
        bind_cargo_jobs(
            &mut self.compiler_environment,
            &mut self.cargo_jobs,
            cargo_jobs,
        )
    }

    /// Point this arm's Cargo at `home` instead of the command rim's own.
    ///
    /// A seated lane's home stays pinned to its seat generation; an arm that
    /// resolves the staged `Cargo.lock` needs the home admitted for that lock.
    pub fn bind_cargo_home(&mut self, home: &Path) {
        self.compiler_environment
            .retain(|(name, _)| name != OsStr::new(cargo_isolation::CARGO_HOME_VARIABLE));
        self.compiler_environment.push((
            OsString::from(cargo_isolation::CARGO_HOME_VARIABLE),
            home.as_os_str().to_owned(),
        ));
        self.compiler_environment.sort();
    }

    /// Derive one reached focused arm from this preparation's immutable source.
    ///
    /// The derived arm shares the selected tree, contract, command-rim
    /// environment, and state transaction while retaining an independent
    /// executor and exact authored reach.
    pub fn derive_reached_arm(
        &self,
        selection: &ChangeSelectionReport,
        gate_id: GateId,
    ) -> Result<PreparedArm<'static>, RunError> {
        self.capture
            .derive_arm_in_state_reached(selection, gate_id)
            .map(|capture| PreparedArm {
                capture,
                gate_id,
                compiler_environment: self.compiler_environment.clone(),
                cargo_jobs: self.cargo_jobs,
            })
            .map_err(|error| RunError::refusal("gate::capture-refused", error))
    }

    /// Derive one live-only arm from this preparation without packaged selection.
    pub fn derive_live_arm(&self, gate_id: GateId) -> Result<PreparedArm<'static>, RunError> {
        self.capture
            .derive_live_arm_in_state(gate_id)
            .map(|capture| PreparedArm {
                capture,
                gate_id,
                compiler_environment: self.compiler_environment.clone(),
                cargo_jobs: self.cargo_jobs,
            })
            .map_err(|error| RunError::refusal("gate::capture-refused", error))
    }

    /// Publish all focused observations after the enclosing run accepts.
    pub fn accept_state(&self, scope: StateRunScope) -> Result<(), RunError> {
        self.capture
            .accept_state(scope)
            .map(|_| ())
            .map_err(RunError::state_publication)
    }

    /// Discard all focused observations after the enclosing run rejects.
    pub fn reject_state(&self) -> Result<(), RunError> {
        self.capture
            .reject_state()
            .map_err(|error| RunError::fault("gate::state-rejection-unavailable", error))
    }
}

impl PreparedBattery<'_> {
    /// Cap standalone Cargo analyses owned by this retained battery capture.
    ///
    /// # Errors
    ///
    /// Refuses a different second budget after the capture was bound.
    pub fn bind_cargo_jobs(&mut self, cargo_jobs: usize) -> Result<(), RunError> {
        bind_cargo_jobs(
            &mut self.compiler_environment,
            &mut self.cargo_jobs,
            cargo_jobs,
        )
    }

    /// Identity of the immutable selected source retained by this preparation.
    #[must_use]
    pub const fn source(&self) -> &Digest {
        self.capture.source()
    }

    /// Private selected-tree root retained for this preparation's lifetime.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.capture.root()
    }

    /// Exact changed paths captured at the command rim for this source.
    #[must_use]
    pub fn changes(&self) -> &[String] {
        self.capture.changes()
    }

    /// Every regular path retained by the immutable selected tree.
    #[must_use]
    pub fn paths(&self) -> &[String] {
        self.capture.paths()
    }

    /// Which worktree paths this battery's private snapshot left out.
    ///
    /// # Errors
    ///
    /// Refuses when the worktree cannot be compared with the snapshot.
    pub fn worktree_divergence(&self) -> Result<::gate::tree::WorktreeDivergence, RunError> {
        self.capture
            .worktree_divergence()
            .map_err(|error| RunError::refusal("gate::snapshot-divergence-refused", error))
    }

    /// Tracked generated-path selectors captured from this selected tree.
    #[must_use]
    pub fn generated_paths(&self) -> &[String] {
        self.capture.generated_paths()
    }

    /// Complete immutable authority context captured for this battery.
    pub fn context(&self) -> Result<::gate::BatteryContext, RunError> {
        self.capture
            .context()
            .map_err(|error| RunError::refusal("gate::context-refused", error))
    }

    /// Prove the live selected source still names this full-battery capture.
    ///
    /// # Errors
    ///
    /// Returns the exact staged index, `HEAD`, or repository movement.
    pub fn verify_source(&self) -> Result<(), RunError> {
        self.capture
            .verify_source()
            .map_err(|error| RunError::refusal("gate::source-moved", error))
    }

    /// Project the selected file-shape and routine-complexity arms early.
    ///
    /// The complete battery later reuses the same capture and cached metrics;
    /// this seam exists only to refuse structural defects before expensive
    /// external verifier suites begin.
    pub fn execute_structural_preflight(&mut self) -> Result<Vec<FocusedArmReport>, RunError> {
        self.capture
            .execute_structural_preflight()
            .map_err(|error| RunError::refusal("gate::structural-preflight-refused", error))
    }

    /// Execute this retained capture directly.
    ///
    /// # Errors
    ///
    /// Returns the canonical battery refusal.
    pub fn execute(&mut self) -> Result<BatteryReport, RunError> {
        self.capture
            .execute_with_environment(self.compiler_environment.clone())
            .map_err(|error| RunError::refusal("gate::battery-refused", error))
    }

    /// Publish the exhaustive state transaction after the enclosing verifier accepts.
    pub fn accept_state(&self) -> Result<::gate::StateReceipt, RunError> {
        self.capture
            .accept_state(StateRunScope::Exhaustive)
            .map_err(RunError::state_publication)
    }

    /// Discard the exhaustive state transaction after refusal or interruption.
    pub fn reject_state(&self) -> Result<(), RunError> {
        self.capture
            .reject_state()
            .map_err(|error| RunError::fault("gate::state-rejection-unavailable", error))
    }
}

fn bind_cargo_jobs(
    environment: &mut Vec<(OsString, OsString)>,
    bound: &mut Option<usize>,
    requested: usize,
) -> Result<(), RunError> {
    let requested = requested.max(1);
    match *bound {
        Some(current) if current != requested => Err(RunError::refusal(
            "gate::verifier-budget-refused",
            format!("Cargo job cap changed from {current} to {requested} after capture"),
        )),
        Some(_) => Ok(()),
        None => {
            environment.retain(|(name, _)| name != CARGO_BUILD_JOBS);
            environment.push((
                OsString::from(CARGO_BUILD_JOBS),
                OsString::from(requested.to_string()),
            ));
            environment.sort();
            *bound = Some(requested);
            Ok(())
        }
    }
}

/// Capture the selected source once for an explicit diagnostic run.
///
/// # Errors
///
/// Returns the same rim and capture failures as [`run`].
pub fn prepare(
    ambient: &Ambient,
    selector: Selector,
) -> Result<PreparedBattery<'static>, RunError> {
    prepare_with_mode(ambient, selector, InputMode::Generic)
}

fn prepare_with_mode(
    ambient: &Ambient,
    selector: Selector,
    input_mode: InputMode,
) -> Result<PreparedBattery<'static>, RunError> {
    let compiler_environment = compiler_environment(ambient);
    capture(ambient, selector, input_mode, None, None).map(|capture| PreparedBattery {
        capture,
        compiler_environment,
        cargo_jobs: None,
    })
}

/// Capture one selected arm with the board claim observed by the command rim.
///
/// # Errors
///
/// Returns the same rim and selected-arm capture failures as [`run_arm_report`].
pub fn prepare_arm_with_claim(
    ambient: &Ambient,
    selector: Selector,
    gate_id: GateId,
    claim: ClaimCapture,
) -> Result<PreparedArm<'static>, RunError> {
    let compiler_environment = compiler_environment(ambient);
    capture::capture_inner(
        ambient,
        selector,
        InputMode::Generic,
        Some(gate_id),
        None,
        claim,
    )
    .map(|capture| PreparedArm {
        capture,
        gate_id,
        compiler_environment,
        cargo_jobs: None,
    })
}

/// Capture one selected arm at an admitted directory with an exact board claim.
pub fn prepare_arm_at_with_claim(
    ambient: &Ambient,
    directory: &Path,
    selector: Selector,
    gate_id: GateId,
    claim: ClaimCapture,
) -> Result<PreparedArm<'static>, RunError> {
    let compiler_environment = compiler_environment(ambient);
    capture::capture_at_inner(
        ambient,
        directory,
        selector,
        InputMode::Generic,
        Some(gate_id),
        None,
        claim,
    )
    .map(|capture| PreparedArm {
        capture,
        gate_id,
        compiler_environment,
        cargo_jobs: None,
    })
}

/// Capture one current-source arm under the strict self-hosting contract.
pub fn prepare_arm_strict(
    ambient: &Ambient,
    selector: Selector,
    gate_id: GateId,
) -> Result<PreparedArm<'static>, RunError> {
    prepare_arm_with_mode(ambient, selector, gate_id, InputMode::Strict)
}

fn prepare_arm_with_mode(
    ambient: &Ambient,
    selector: Selector,
    gate_id: GateId,
    input_mode: InputMode,
) -> Result<PreparedArm<'static>, RunError> {
    let compiler_environment = compiler_environment(ambient);
    capture(ambient, selector, input_mode, Some(gate_id), None).map(|capture| PreparedArm {
        capture,
        gate_id,
        compiler_environment,
        cargo_jobs: None,
    })
}

/// Capture the selected source once while exposing execution progress.
///
/// The returned preparation retains the trace for its lifetime, so test
/// selection and the later battery execution can share one immutable source
/// without losing the supervisor's exact family/arm cursor.
///
/// # Errors
///
/// Returns the same rim and capture failures as [`prepare`].
pub fn prepare_traced<'trace>(
    ambient: &Ambient,
    selector: Selector,
    trace: &'trace mut (dyn FnMut(RepositoryExecutionEvent) + Send),
) -> Result<PreparedBattery<'trace>, RunError> {
    let compiler_environment = compiler_environment(ambient);
    capture(ambient, selector, InputMode::Generic, None, Some(trace)).map(|capture| {
        PreparedBattery {
            capture,
            compiler_environment,
            cargo_jobs: None,
        }
    })
}

/// Execute the canonical battery over one selected source.
///
/// # Errors
///
/// Returns the exact rim acquisition or battery refusal unchanged.
pub fn run(ambient: &Ambient, selector: Selector) -> Result<BatteryReport, RunError> {
    run_prepared(prepare(ambient, selector)?)
}

/// Execute the strict self-hosting battery from one explicitly admitted
/// repository directory.
///
/// # Errors
///
/// Returns the exact rim acquisition or battery refusal. The caller-supplied
/// directory, rather than ambient process location, owns repository discovery.
pub fn run_strict_at(
    ambient: &Ambient,
    directory: &Path,
    selector: Selector,
) -> Result<BatteryReport, RunError> {
    let compiler_environment = compiler_environment(ambient);
    let capture = capture::capture_at_inner(
        ambient,
        directory,
        selector,
        InputMode::Strict,
        None,
        None,
        ClaimCapture::Idle,
    )?;
    run_prepared(PreparedBattery {
        capture,
        compiler_environment,
        cargo_jobs: None,
    })
}

/// Execute one prepared battery and publish or discard its state by verdict.
///
/// # Errors
///
/// Returns the battery refusal or the state publication failure.
pub fn run_prepared(mut prepared: PreparedBattery<'_>) -> Result<BatteryReport, RunError> {
    let report = prepared.execute()?;
    if report.admits() {
        prepared.accept_state()?;
    } else {
        prepared.reject_state()?;
    }
    Ok(report)
}

/// Execute one current-source arm under the strict self-hosting contract.
///
/// The report comes with which worktree paths the arm's private snapshot
/// left out, so the surface running it can say so.
///
/// # Errors
///
/// Returns the same acquisition and arm refusal as [`run`], without
/// executing sibling battery arms.
pub fn run_arm_report_strict(
    ambient: &Ambient,
    selector: Selector,
    gate_id: GateId,
) -> Result<(FocusedArmReport, ::gate::tree::WorktreeDivergence), RunError> {
    let mut prepared = prepare_arm_with_mode(ambient, selector, gate_id, InputMode::Strict)?;
    let snapshot = prepared.worktree_divergence()?;
    let report = prepared.execute()?;
    if report.admits() {
        prepared.accept_state(StateRunScope::Partial)?;
    } else {
        prepared.reject_state()?;
    }
    Ok((report, snapshot))
}

fn capture<'trace>(
    ambient: &Ambient,
    selector: Selector,
    input_mode: InputMode,
    selected: Option<GateId>,
    trace: Option<&'trace mut (dyn FnMut(RepositoryExecutionEvent) + Send)>,
) -> Result<BatteryCapture<'trace>, RunError> {
    capture::capture_inner(
        ambient,
        selector,
        input_mode,
        selected,
        trace,
        ClaimCapture::Idle,
    )
}

fn acting_actor(repository: &Path) -> Result<String, RunError> {
    let repository =
        gix::open(repository).map_err(|error| RunError::refusal("gate::actor-refused", error))?;
    let actor = repository
        .config_snapshot()
        .string("user.email")
        .ok_or_else(|| {
            RunError::refusal("gate::actor-refused", "Git user.email is not configured")
        })?;
    let actor = actor
        .to_str()
        .map_err(|error| RunError::refusal("gate::actor-refused", error))?
        .trim();
    if actor.is_empty() || actor.contains(['\n', '\r']) {
        return Err(RunError::refusal(
            "gate::actor-refused",
            "Git user.email must name one nonempty single-line acting identity",
        ));
    }
    Ok(actor.to_owned())
}

fn selected_changes(
    repository: &Path,
    selector: &Selector,
    search_path: Option<&OsStr>,
) -> Result<Vec<String>, RunError> {
    if matches!(selector, Selector::Staged) {
        // Gate rebinds this placeholder from the exact sealed index materialization.
        return Ok(Vec::new());
    }
    let arguments: Vec<OsString> = match selector {
        Selector::Staged => unreachable!("staged changes come from the sealed index"),
        Selector::Revision(revision) => {
            if revision.starts_with('-') {
                return Err(RunError::refusal(
                    "gate::revision-refused",
                    format!("revision cannot be parsed as a Git option: {revision:?}"),
                ));
            }
            [
                "diff-tree",
                "--root",
                "--no-commit-id",
                "--name-only",
                "-r",
                "-z",
            ]
            .into_iter()
            .map(OsString::from)
            .chain(std::iter::once(OsString::from(revision)))
            .collect()
        }
    };
    let output = git_os(repository, search_path, &arguments).map_err(|error| match selector {
        Selector::Revision(_) if !error.is_fault() => {
            RunError::refusal("gate::selection-refused", error.detail())
        }
        _ => error,
    })?;
    parse_selected_changes(&output)
}

fn parse_selected_changes(output: &[u8]) -> Result<Vec<String>, RunError> {
    let mut changes = output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            std::str::from_utf8(path)
                .map(str::to_owned)
                .map_err(|error| RunError::refusal("gate::changes-refused", error))
        })
        .collect::<Result<Vec<_>, _>>()?;
    changes.sort();
    changes.dedup();
    Ok(changes)
}

fn git_os(
    repository: &Path,
    search_path: Option<&OsStr>,
    arguments: &[OsString],
) -> Result<Vec<u8>, RunError> {
    let selected = ::gate::git::program(repository, search_path)
        .map_err(|error| RunError::fault("gate::git-unavailable", error))?;
    selected.announce_secondary();
    let program = selected.path();
    let output = Command::new(program.as_os_str())
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .map_err(|error| RunError::fault("gate::git-unavailable", error))?;
    if !output.status.success() {
        return Err(RunError::refusal(
            "gate::git-refused",
            format!(
                "{} {:?} exited {:?}: {}",
                program.display(),
                arguments,
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    Ok(output.stdout)
}

/// One typed command-rim failure.
#[derive(Debug)]
pub struct RunError {
    reason: &'static str,
    detail: String,
    fault: bool,
    stale_state_publication: bool,
}

impl RunError {
    fn refusal(reason: &'static str, detail: impl fmt::Display) -> Self {
        Self {
            reason,
            detail: detail.to_string(),
            fault: false,
            stale_state_publication: false,
        }
    }

    fn fault(reason: &'static str, detail: impl fmt::Display) -> Self {
        Self {
            reason,
            detail: detail.to_string(),
            fault: true,
            stale_state_publication: false,
        }
    }

    fn state_publication(error: ::gate::BatteryError) -> Self {
        Self {
            reason: "gate::state-publication-refused",
            detail: error.to_string(),
            fault: false,
            stale_state_publication: error.is_stale_state_publication(),
        }
    }

    /// Stable command-surface refusal identity.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        self.reason
    }

    /// Complete underlying acquisition evidence.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// Whether this is an environment fault rather than an input refusal.
    #[must_use]
    pub const fn is_fault(&self) -> bool {
        self.fault
    }

    /// Whether an accepted run lost only the durable state compare-and-swap.
    #[must_use]
    pub const fn is_stale_state_publication(&self) -> bool {
        self.stale_state_publication
    }
}

impl fmt::Display for RunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.reason, self.detail)
    }
}

impl std::error::Error for RunError {}

#[cfg(test)]
#[path = "gate_tests.rs"]
mod tests;
