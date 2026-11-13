//! One capture of everything a battery run consumes and everything it runs.
//!
//! [`RepositorySelection`] materializes one selection and loads its tracked
//! contract before the state, source identity, and repository observations are
//! bound. The authorities the battery reads alongside its arms come from that
//! same source capture, so they are retained here rather than assembled beside
//! a second materialization of the selection.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use wire::{Digest, Domain, TaskHandle};

use super::repository_executor::{RepositoryExecutor, RepositorySelection, RepositorySource};
use super::{
    BatteryArmResult, BatteryError, BatteryInputs, BatteryReport, BatteryScope, BatteryStateRun,
    BatteryToolEvidence, ConfigurationEvidence, RepositoryExecutionEvent, StateRunScope,
    run_battery,
};
use crate::core::{ActorCapture, ClaimCapture};
use crate::hygiene::HygieneConfiguration;
use crate::input::Contract;
use crate::manifest::OriginLayer;
use crate::policy::Policy as RulePolicy;
use crate::state::{
    ActorInputs, ActorResolution, BoundDecision, JudgedLatches, LatchEntry, StateLedger,
    StateReceipt,
};
use crate::toolchain::RimCapture;
use crate::tree::{Selector, StagedAuthority};

/// Which tracked-document grammar a capture loads its contract under.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputMode {
    /// The strict self-hosting contract: every tracked document must parse.
    Strict,
    /// The installed generic policy: missing repository policy and
    /// lower-is-better state remain typed absences.
    Generic,
}

/// The caller's authorities for one battery capture: the repository and the
/// selection within it, and the claim, actor, command rim, hygiene
/// configuration, and sweep covers observed on the live worktree.
pub struct CaptureAuthorities<'a> {
    pub repository: PathBuf,
    pub selector: &'a Selector,
    pub claim: ClaimCapture,
    pub actor: ActorCapture,
    pub rim: RimCapture,
    pub hygiene: HygieneConfiguration,
    /// Latches a pending sweep task holds for someone other than this run's
    /// actor: an over-limit finding on one informs and passes.
    pub covers: BTreeMap<LatchEntry, TaskHandle>,
}

/// Release whose grammar understood the tracked documents read here.
const CURRENT_RELEASE: &str = "current";

/// Every authority one battery run reads, owned by the capture that read them.
///
/// Held apart from the executor that runs against them because
/// [`run_battery`] borrows the authorities for as long as it borrows the
/// executor mutably. Keeping them in two fields of one capture is what lets a
/// caller hand over both without ever holding one without the other.
///
/// Nothing outside this module reads an authority back: what a run resolved is
/// published by the report's own context, digested and complete, rather than
/// handed out twice in two shapes.
struct BatteryAuthorities {
    source: Digest,
    actor: ActorResolution,
    scope: BatteryScope,
    configuration: Vec<ConfigurationEvidence>,
    policy: RulePolicy,
    state: StateLedger,
    bounds: Vec<BoundDecision>,
    tools: Vec<BatteryToolEvidence>,
}

/// Run-wide authority that every focused arm derives from without recapturing bytes.
#[derive(Clone)]
pub(super) struct BatterySourceCapture {
    pub(super) repository: RepositorySource,
    pub(super) actor: ActorCapture,
    resolved_actor: ActorResolution,
    scope: BatteryScope,
    pub(super) state: StateLedger,
    pub(super) latches: JudgedLatches,
    pub(super) rim: RimCapture,
    pub(super) hygiene: HygieneConfiguration,
}

impl BatteryAuthorities {
    /// Borrow the complete authority set as the inputs every arm reads.
    fn inputs(&self) -> BatteryInputs<'_> {
        BatteryInputs {
            source: &self.source,
            actor: &self.actor,
            scope: &self.scope,
            configuration: &self.configuration,
            policy: &self.policy,
            state: &self.state,
            bounds: &self.bounds,
            tools: &self.tools,
        }
    }
}

/// One selection turned into both the authorities and the executor.
///
/// The two halves are produced from a single materialization: a capture that
/// took the authorities from one selection and ran the arms against another
/// would publish a report whose context describes a tree its results never saw.
pub struct BatteryCapture<'trace> {
    source_capture: BatterySourceCapture,
    authorities: BatteryAuthorities,
    executor: RepositoryExecutor<'trace>,
    selected: Option<crate::GateId>,
    state_run: BatteryStateRun,
}

impl<'trace> BatteryCapture<'trace> {
    /// Snapshot the complete current authority context without executing an arm.
    pub fn context(&self) -> Result<super::BatteryContext, BatteryError> {
        self.authorities.inputs().snapshot()
    }

    /// Probe the external executables declared by this selected focused arm.
    ///
    /// This captures identity only; it does not execute the arm analysis.
    pub fn action_tools(
        &self,
        gate_id: crate::GateId,
        compiler_environment: &[(OsString, OsString)],
    ) -> Result<Vec<crate::ToolEvidence>, BatteryError> {
        if self.selected != Some(gate_id) {
            return Err(BatteryError::new(format!(
                "tool authority for {gate_id} requires a capture selected for that arm"
            )));
        }
        self.executor.action_tools(gate_id, compiler_environment)
    }

    /// Cheap complete executable-file authority for reusable action lookup.
    pub fn proof_tool_authority(
        &self,
        compiler_environment: &[(OsString, OsString)],
    ) -> Result<Digest, BatteryError> {
        self.executor.proof_tool_authority(compiler_environment)
    }

    /// Prove the live selector source still names this capture.
    ///
    /// # Errors
    ///
    /// Returns the exact selected-tree movement diagnostic.
    pub fn verify_source(&self) -> Result<(), BatteryError> {
        self.executor.verify_source()
    }

    /// Preserve index movement as a signal for a reservation-scoped semantic recapture.
    pub fn verify_refreshable_source(
        &self,
    ) -> Result<crate::tree::RefreshableSourceState, BatteryError> {
        self.executor.verify_refreshable_source()
    }

    /// Every regular path retained by this exact selected source.
    #[must_use]
    pub fn paths(&self) -> &[String] {
        self.executor.paths()
    }

    /// Canonical staged entries and their exact rename lineage, when staged.
    #[must_use]
    pub fn staged_authority(&self) -> Option<&crate::tree::StagedAuthority> {
        self.executor.staged_authority()
    }

    /// Which worktree paths this capture's private snapshot left out.
    ///
    /// # Errors
    ///
    /// Returns the repository or filesystem failure that kept the comparison
    /// from completing.
    pub fn worktree_divergence(&self) -> Result<crate::tree::WorktreeDivergence, BatteryError> {
        self.executor
            .tree()
            .worktree_divergence()
            .map_err(|error| BatteryError::new(error.to_string()))
    }

    /// Compile one action closure from this capture's retained tree.
    pub fn compile_action_input(
        &self,
        selection: &crate::ChangeSelectionReport,
        gate_id: crate::GateId,
        inventory: crate::ActionInputInventory,
    ) -> Result<crate::ActionInputClosure, BatteryError> {
        crate::compile_action_input(self.executor.tree(), selection, gate_id, inventory)
            .map_err(|error| BatteryError::new(error.to_string()))
    }

    /// Compile one explicit sub-action closure from this capture's retained tree.
    pub fn compile_explicit_action_input(
        &self,
        paths: Vec<String>,
        non_files: Vec<crate::ActionNonFileInput>,
    ) -> Result<crate::ActionInputManifest, BatteryError> {
        crate::compile_explicit_action_input(self.executor.tree(), paths, non_files)
            .map_err(|error| BatteryError::new(error.to_string()))
    }

    /// Command-rim compiler environment bound before focused execution.
    #[must_use]
    pub fn compiler_environment(&self) -> &[(OsString, OsString)] {
        self.executor.compiler_environment()
    }

    /// Materialize the exact committed base retained by this staged capture.
    ///
    /// # Errors
    ///
    /// Returns repository, object, path, or filesystem failures from the
    /// retained selected-tree authority.
    pub fn materialize_staged_base(
        &self,
    ) -> Result<Option<crate::tree::SelectedTree>, BatteryError> {
        self.executor.materialize_staged_base()
    }

    /// Tracked generated-path selectors captured from this selected tree.
    #[must_use]
    pub fn generated_paths(&self) -> &[String] {
        self.executor.contract().shape().generated_paths()
    }

    /// Capture one repository into a complete, runnable battery.
    ///
    /// `authorities` carries the caller's observations of the live worktree,
    /// which no selected tree can supply, beside the repository and selection
    /// the capture materializes. Everything else is read here: the policy,
    /// configuration, and tool evidence come from the contract this capture
    /// loaded, the scope from the selector it materialized, and the ledger from
    /// the repository it was taken from. `selected` narrows the battery to one
    /// canonical arm, and `trace` receives every arm and family invocation
    /// without gaining authority over execution.
    ///
    /// # Errors
    ///
    /// Refuses selection, contract, source-identity, ledger, or authority
    /// failure before any arm can run. An absent actor is one of these: a
    /// battery report names the identity it was run by, and no other input can
    /// stand in for one.
    pub fn capture(
        authorities: CaptureAuthorities<'_>,
        input_mode: InputMode,
        selected: Option<crate::GateId>,
        trace: Option<&'trace mut (dyn FnMut(RepositoryExecutionEvent) + Send)>,
    ) -> Result<Self, BatteryError> {
        let CaptureAuthorities {
            repository,
            selector,
            claim,
            actor,
            rim,
            hygiene,
            covers,
        } = authorities;
        // Read before the capture consumes the observation, and both before the
        // selection is materialized: an authority that cannot be produced is a
        // refusal, and refusing it here costs no private tree.
        let resolved_actor = run_actor(&actor)?;
        let scope = selected_scope(selector, &repository)?;
        let state_run = BatteryStateRun::capture(&repository)?;
        let repository_source = RepositorySelection::capture(&repository, selector, input_mode)?;
        let rim = match (
            rim.derives_staged_changes(),
            repository_source.tree().staged_changes(),
        ) {
            (true, Some(changes)) => rim.with_changes(changes.to_vec()),
            _ => rim,
        };
        let tree = repository_source.tree();
        let (state, latches, bound_actor) = state_run.bind(
            tree.paths(),
            tree.renames(),
            tree.staged_authority()
                .and_then(StagedAuthority::baseline_identity),
            tree.publishes_state(),
            &actor,
        )?;
        let latches = latches.with_covers(covers);
        let repository_source =
            repository_source.bind(claim, bound_actor.clone(), &latches, &rim)?;
        let source_capture = BatterySourceCapture {
            repository: repository_source,
            actor: bound_actor,
            state,
            latches,
            resolved_actor,
            scope,
            rim,
            hygiene,
        };
        Self::capture_from_source(source_capture, selected, trace, state_run, None)
    }

    fn capture_from_source(
        source_capture: BatterySourceCapture,
        selected: Option<crate::GateId>,
        trace: Option<&'trace mut (dyn FnMut(RepositoryExecutionEvent) + Send)>,
        state_run: BatteryStateRun,
        selection: Option<&crate::ChangeSelectionReport>,
    ) -> Result<Self, BatteryError> {
        source_capture.repository.verify_source()?;
        let executor = RepositoryExecutor::capture_from_source(
            &source_capture,
            &state_run,
            selected,
            selection,
            trace,
        )?;
        let contract = executor.contract();
        let authorities = BatteryAuthorities {
            source: executor.source().clone(),
            actor: source_capture.resolved_actor.clone(),
            scope: source_capture.scope.clone(),
            configuration: tracked_evidence(contract)?,
            policy: contract.rule_policy().clone(),
            // The repository-document, structural file-shape, and structural
            // routine-complexity arms measured every selected path or routine
            // against a per-actor elastic limit. These are the exact decisions
            // those capture-time acquisitions judged their rows against, all
            // decided from the same ledger published above; nothing is
            // re-resolved merely to describe the run.
            bounds: executor.bounds().to_vec(),
            state: source_capture.state.clone(),
            tools: tool_evidence(contract)?,
        };
        Ok(Self {
            source_capture,
            authorities,
            executor,
            selected,
            state_run,
        })
    }

    /// Run the canonical battery against this capture.
    ///
    /// # Errors
    ///
    /// Returns the manifest, context, or completeness refusal that prevented
    /// one total result vector. An arm that fails is a result, not an error.
    pub fn execute(&mut self) -> Result<BatteryReport, BatteryError> {
        if let Some(gate_id) = self.selected {
            return Err(BatteryError::new(format!(
                "selected capture for {gate_id} cannot execute a complete battery"
            )));
        }
        let inputs = self.authorities.inputs();
        run_battery(&inputs, &mut self.executor)
    }

    /// Run the canonical battery with the compiler environment captured at the command rim.
    pub fn execute_with_environment(
        &mut self,
        environment: Vec<(OsString, OsString)>,
    ) -> Result<BatteryReport, BatteryError> {
        if self.selected.is_some() {
            return Err(BatteryError::new(
                "selected capture cannot execute a complete battery",
            ));
        }
        self.executor.set_compiler_environment(environment)?;
        let inputs = self.authorities.inputs();
        run_battery(&inputs, &mut self.executor)
    }

    /// Run exactly one canonical arm against this capture.
    ///
    /// # Errors
    ///
    /// Returns the same manifest, input, and executor refusals as a complete
    /// run, scoped to the selected arm.
    pub fn execute_arm(
        &mut self,
        gate_id: crate::GateId,
    ) -> Result<BatteryArmResult, BatteryError> {
        self.execute_arm_report(gate_id)
            .map(super::FocusedArmReport::into_result)
    }

    /// Run exactly one canonical arm and retain its verifiable input context.
    ///
    /// # Errors
    ///
    /// Returns the same manifest, input, and executor refusals as
    /// [`Self::execute_arm`].
    pub fn execute_arm_report(
        &mut self,
        gate_id: crate::GateId,
    ) -> Result<super::FocusedArmReport, BatteryError> {
        if self.selected != Some(gate_id) {
            return Err(BatteryError::new(format!(
                "arm execution for {gate_id} requires a capture selected for that arm"
            )));
        }
        let inputs = self.authorities.inputs();
        super::run_focused_arm(&inputs, &mut self.executor, gate_id)
    }

    /// Run one canonical arm with its caller-captured operational compiler environment.
    ///
    /// # Errors
    ///
    /// Returns the same selected-arm and execution failures as [`Self::execute_arm_report`],
    /// plus replacement after the toolchain family has completed.
    pub fn execute_arm_report_with_environment(
        &mut self,
        gate_id: crate::GateId,
        environment: Vec<(OsString, OsString)>,
    ) -> Result<super::FocusedArmReport, BatteryError> {
        self.executor.set_compiler_environment(environment)?;
        self.execute_arm_report(gate_id)
    }

    /// Project the two structural arms captured for this complete battery.
    ///
    /// Their metrics were acquired when the immutable selected source was
    /// materialized. Projecting them here therefore performs no second source
    /// capture or analysis, and a later complete execution reuses the same
    /// retained family evidence.
    ///
    /// # Errors
    ///
    /// Refuses selected-arm captures or invalid canonical inputs.
    pub fn execute_structural_preflight(
        &mut self,
    ) -> Result<Vec<super::FocusedArmReport>, BatteryError> {
        if self.selected.is_some() {
            return Err(BatteryError::new(
                "structural preflight requires a complete battery capture",
            ));
        }
        let inputs = self.authorities.inputs();
        [crate::GateId::FileShape, crate::GateId::Complexity]
            .into_iter()
            .map(|gate_id| super::run_focused_arm(&inputs, &mut self.executor, gate_id))
            .collect()
    }

    /// Derive one reached focused arm from this capture's immutable source authority.
    ///
    /// The live selector seal is verified before derivation. The returned arm
    /// receives a distinct executor and arm-specific evidence while sharing the
    /// selected tree, loaded contract, core observations, ancestry baselines,
    /// actor, command rim, hygiene configuration, and enclosing state ledger.
    pub fn derive_arm_in_state_reached(
        &self,
        selection: &crate::ChangeSelectionReport,
        gate_id: crate::GateId,
    ) -> Result<BatteryCapture<'static>, BatteryError> {
        BatteryCapture::<'static>::capture_from_source(
            self.source_capture.clone(),
            Some(gate_id),
            None,
            self.state_run.clone(),
            Some(selection),
        )
    }

    /// Derive one live-only arm without constructing packaged selection.
    ///
    /// # Errors
    ///
    /// Refuses every arm whose result can be reused or whose reach requires selection.
    pub fn derive_live_arm_in_state(
        &self,
        gate_id: crate::GateId,
    ) -> Result<BatteryCapture<'static>, BatteryError> {
        if crate::gate_action_reuse_class(gate_id) != Some(crate::ActionReuseClass::LiveOnly) {
            return Err(BatteryError::new(
                format!("arm {gate_id} is not live-only",),
            ));
        }
        BatteryCapture::<'static>::capture_from_source(
            self.source_capture.clone(),
            Some(gate_id),
            None,
            self.state_run.clone(),
            None,
        )
    }

    /// Identity of the selected bytes this capture owns.
    #[must_use]
    pub const fn source(&self) -> &Digest {
        &self.authorities.source
    }

    /// Private selected-tree root retained for the capture's lifetime.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.executor.root()
    }

    /// Exact changed paths observed before this selected source was captured.
    #[must_use]
    pub fn changes(&self) -> &[String] {
        self.executor.changes()
    }

    /// Objects retained from the validated reach plan for a focused recapture.
    ///
    /// Standalone selected arms and complete batteries have no authored reach
    /// plan and therefore return `None`.
    #[must_use]
    pub fn execution_objects(&self) -> Option<&[crate::ChangeReachObject]> {
        self.executor.execution_objects()
    }

    /// Stable source and outputs completed by a successful focused native Rust arm.
    #[must_use]
    pub fn reusable_rust_workspace(&self) -> Option<&crate::RustAnalysisWorkspace> {
        self.executor.reusable_rust_workspace()
    }

    /// Publish this enclosing run's accumulated state after total acceptance.
    pub fn accept_state(&self, scope: StateRunScope) -> Result<StateReceipt, BatteryError> {
        self.state_run.accept(scope)
    }

    /// Reject this enclosing run without publishing any accumulated state.
    pub fn reject_state(&self) -> Result<(), BatteryError> {
        self.state_run.reject()
    }
}

/// Resolve the one actor a battery run is attributed to.
///
/// [`ActorInputs::require_actor`] is also the claim-arbitration resolver, so
/// report attribution and claims admit the same actor-bearing provenance. The
/// candidate path is required by that shared input type but cannot become an
/// actor.
fn run_actor(actor: &ActorCapture) -> Result<ActorResolution, BatteryError> {
    let candidate = ActorInputs::new(wire::TRACKED_POLICY_FILE)
        .map_err(|error| BatteryError::new(format!("battery actor provenance: {error}")))?;
    let Some(active) = actor.active_actor() else {
        return Err(BatteryError::new(
            "battery capture requires an acting actor: a battery report names who ran it",
        ));
    };
    candidate
        .with_active_actor(active)
        .require_actor()
        .map_err(|error| BatteryError::new(format!("battery actor: {error}")))
}

/// Name the selection every arm reads and the repository it was taken from.
fn selected_scope(selector: &Selector, repository: &Path) -> Result<BatteryScope, BatteryError> {
    let (name, provenance) = match selector {
        Selector::Staged => (
            "staged-index",
            format!("the index of {}", repository.display()),
        ),
        Selector::Revision(revision) => (
            "revision",
            format!("{revision} in {}", repository.display()),
        ),
    };
    BatteryScope::new(name, provenance)
}

/// Digest every value the tracked configuration document resolved.
///
/// One evidence per leaf, keyed by its exact field path, so a document that
/// declares nothing produces nothing and a document that declares the same
/// field twice cannot produce two winners. Tables are descended rather than
/// digested: a table is the path to a value, not a value.
fn tracked_evidence(contract: &Contract) -> Result<Vec<ConfigurationEvidence>, BatteryError> {
    let mut evidence = Vec::new();
    let mut field = Vec::new();
    resolve_value(contract.tracked_configuration(), &mut field, &mut evidence)?;
    Ok(evidence)
}

fn resolve_value(
    value: &toml::Value,
    field: &mut Vec<String>,
    into: &mut Vec<ConfigurationEvidence>,
) -> Result<(), BatteryError> {
    if let toml::Value::Table(table) = value {
        for (key, nested) in table {
            field.push(key.clone());
            resolve_value(nested, field, into)?;
            field.pop();
        }
        return Ok(());
    }
    let encoded = wire::encode(value).map_err(|error| {
        BatteryError::new(format!(
            "cannot encode tracked configuration {}: {error}",
            field.join(".")
        ))
    })?;
    into.push(ConfigurationEvidence::new(
        field.join("."),
        Digest::compute(Domain::GateConfiguration, &encoded),
        CURRENT_RELEASE,
        OriginLayer::Repository,
        wire::TRACKED_POLICY_FILE,
        field.clone(),
    )?);
    Ok(())
}

/// Report every tool the captured contract found and probed.
///
/// The contract probes each declared executable once while it loads, so this
/// projects that one discovery rather than repeating it. A contract that failed
/// to identify a tool never loaded at all, which is why every entry here is an
/// identified one.
fn tool_evidence(contract: &Contract) -> Result<Vec<BatteryToolEvidence>, BatteryError> {
    contract
        .tools()
        .iter()
        .map(|tool| {
            BatteryToolEvidence::identified(
                tool.name(),
                tool.program().display().to_string(),
                tool.version(),
                tool.executable().clone(),
            )
        })
        .collect()
}
