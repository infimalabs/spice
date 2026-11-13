//! Production execution of the compiled battery from one selected tree.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use wire::Digest;

use super::authorities::{BatterySourceCapture, InputMode};
use super::{
    ArmEvidence, ArmExecution, ArmFamily, BatteryContext, BatteryError, BatteryExecutor,
    BatteryInputs, BatteryStateRun, CompiledArm, inline_evidence,
};
use crate::core::{
    ActorCapture, CORE_GATE_IDS, ClaimCapture, CoreSource, acquire_core_arm, evaluate_core,
    evaluate_core_arm,
};
use crate::hygiene::{
    HYGIENE_ARMS, HygieneConfiguration, HygieneSource, acquire_hygiene, acquire_hygiene_arm,
    acquire_hygiene_arm_reached,
};
use crate::input::Contract;
use crate::metrics::{
    BaselineCapture, MetricSource, STRUCTURAL_ARMS, acquire_metric_arm_with_bounds,
    acquire_metrics_with_bounds,
};
use crate::state::{BoundDecision, JudgedLatches, LatchObservation};
use crate::toolchain::{
    RimCapture, TASTE_EXTENSIONS, TOOLCHAIN_ARMS, ToolchainSource, acquire_toolchain_arm,
    acquire_toolchain_arm_tools, acquire_toolchain_observed, environment_baseline_suffixes,
    measure_toolchain, measure_toolchain_arm, toolchain_executable_authority,
};
use crate::tree::{RefreshableSourceState, SelectedTree, Selector, StagedAuthority};
use crate::{ChangeReachMode, ChangeReachObject, ChangeSelectionReport, GateId};

const FAMILY_COUNT: usize = 4;

/// Observable executor boundary used by integration tests and callers that
/// need to account for orchestration without replacing family acquisition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RepositoryExecutionEvent {
    /// `run_battery` invoked one canonical arm position.
    ArmInvoked {
        position: usize,
        gate_id: GateId,
        family: ArmFamily,
    },
    /// The executor crossed one family acquisition boundary.
    FamilyInvoked { family: ArmFamily },
    /// One family began the named arm's actual analysis.
    AnalysisInvoked { gate_id: GateId, family: ArmFamily },
    /// The sequential toolchain acquisition finished one named analysis.
    ToolchainAnalysisCompleted {
        gate_id: GateId,
        reusable_workspace: Option<crate::RustAnalysisWorkspace>,
    },
}

enum FamilyExecution {
    Complete(Vec<ArmEvidence>),
    Refused(String),
}

/// Core evidence projected from one run-wide repository instant.
struct CapturedCore {
    source: Digest,
    evidence: Option<Result<Vec<ArmEvidence>, String>>,
    bounds: Vec<BoundDecision>,
    observations: Vec<LatchObservation>,
}

/// One immutable selected tree before run-start authorities are bound to it.
pub(super) struct RepositorySelection {
    tree: Arc<SelectedTree>,
    contract: Arc<Contract>,
}

impl RepositorySelection {
    pub(super) fn capture(
        repository: impl Into<PathBuf>,
        selector: &Selector,
        input_mode: InputMode,
    ) -> Result<Self, BatteryError> {
        let repository = repository.into();
        let tree = Arc::new(selector.materialize(&repository).map_err(|error| {
            BatteryError::new(format!("cannot capture battery source: {error}"))
        })?);
        let contract = Arc::new(
            match input_mode {
                InputMode::Strict => Contract::load(tree.root()),
                InputMode::Generic => Contract::load_generic(tree.root()),
            }
            .map_err(|error| {
                BatteryError::new(format!("cannot load captured battery contract: {error}"))
            })?,
        );
        Ok(Self { tree, contract })
    }

    pub(super) fn bind(
        mut self,
        claim: ClaimCapture,
        actor: ActorCapture,
        latches: &JudgedLatches,
        rim: &RimCapture,
    ) -> Result<RepositorySource, BatteryError> {
        let core = Arc::new(
            CoreSource::capture(&self.tree, &self.contract, claim, actor, latches, rim).map_err(
                |error| BatteryError::new(format!("cannot identify battery source: {error}")),
            )?,
        );
        Arc::get_mut(&mut self.tree)
            .expect("an unbound repository selection has one tree owner")
            .refresh_staged_seal()
            .map_err(|error| BatteryError::new(error.to_string()))?;
        let baselines = Arc::new(capture_baselines(
            self.tree.repository(),
            &self.contract,
            rim,
            core.clean_merge_tree(),
        ));
        core.verify_git_controls()
            .map_err(|error| BatteryError::new(error.to_string()))?;
        self.verify_source()?;
        Ok(RepositorySource {
            tree: self.tree,
            contract: self.contract,
            core,
            baselines,
        })
    }

    fn verify_source(&self) -> Result<(), BatteryError> {
        self.tree
            .verify_source()
            .map_err(|error| BatteryError::new(error.to_string()))
    }

    pub(super) fn tree(&self) -> &SelectedTree {
        &self.tree
    }
}

/// Run-wide selected bytes and live observations shared by focused executors.
#[derive(Clone)]
pub(super) struct RepositorySource {
    tree: Arc<SelectedTree>,
    contract: Arc<Contract>,
    core: Arc<CoreSource>,
    baselines: Arc<BaselineCapture>,
}

impl RepositorySource {
    pub(super) fn verify_source(&self) -> Result<(), BatteryError> {
        self.tree
            .verify_source()
            .map_err(|error| BatteryError::new(error.to_string()))?;
        self.core
            .verify_git_controls()
            .map_err(|error| BatteryError::new(error.to_string()))
    }
}

/// One capture-owning production implementation of [`BatteryExecutor`].
///
/// A family is projected when its first arm is requested. A complete capture
/// caches the family's complete result set or refusal; a selected executor
/// projects only the named result from its run-wide source authority. Either
/// way, no requested analysis runs twice.
///
/// The core and metrics families are the two exceptions, and are acquired at
/// capture. One run-wide core source retains the live repository observations;
/// each executor projects either the complete family or its named arm from
/// those observations. Two core arms inspect the repository the tree was
/// materialized from, so those observations can only be taken in the same
/// moment the source is captured: an acquisition deferred to first request
/// would read an index, a `MERGE_HEAD`, or a worktree that moved after the
/// capture the report names.
/// Metrics are acquired immediately after that observation because their
/// file-shape and routine-complexity rows decide elastic bounds which must be
/// present in the immutable run context before any arm executes. Each complete
/// result or refusal is retained and handed over unchanged when its first arm
/// is requested.
///
/// The committed comparison and merge-authorship baselines are read in that
/// same moment and for the same reason, even though some arms that read them
/// are not core arms. They are the live repository's `HEAD` and pending merge
/// parents, and those move: a read deferred to first request would compare part
/// of the selection against one ancestry and the rest against another, and the
/// report would name neither.
///
/// The acting actor and the latch inventory are handed directly from the core
/// capture to the structural acquisition. That keeps both families deciding
/// against one ledger reading; neither is retained for a later observation.
pub struct RepositoryExecutor<'trace> {
    tree: Arc<SelectedTree>,
    core_source: Arc<CoreSource>,
    contract: Arc<Contract>,
    source: Digest,
    core: Option<Result<Vec<ArmEvidence>, String>>,
    metrics: Option<Result<Vec<ArmEvidence>, String>>,
    baselines: Arc<BaselineCapture>,
    bounds: Vec<BoundDecision>,
    rim: RimCapture,
    hygiene: HygieneConfiguration,
    context: Option<BatteryContext>,
    families: [Option<FamilyExecution>; FAMILY_COUNT],
    compiler_environment: Vec<(OsString, OsString)>,
    selected: Option<GateId>,
    reached_paths: Option<Vec<String>>,
    execution_objects: Option<Vec<ChangeReachObject>>,
    reusable_rust_workspace: Option<crate::RustAnalysisWorkspace>,
    trace: Option<&'trace mut (dyn FnMut(RepositoryExecutionEvent) + Send)>,
}

impl<'trace> RepositoryExecutor<'trace> {
    /// Derive one battery executor from a retained repository selection.
    ///
    /// `rim` carries the directory, executable search path, and changed paths
    /// the caller observed at the binary rim. All three are supplied rather than
    /// derived here: an executor that read the repository back would measure a
    /// set the caller never saw, and an executor that defaulted to none would
    /// scan nothing and call every tree clean.
    ///
    /// The source capture's latches are the fixed-clock ledger already bound to
    /// the selected source: the durable breach inventory the core and
    /// structural families decide their elastic bounds against. A latched path
    /// is held to its base limit rather than the varied one, and its decision
    /// names the latch's recorded origin as inherited debt. The enclosing
    /// [`BatteryStateRun`] receives the resulting observations, so the bounds
    /// this executor decides and the state its report names are one read.
    ///
    /// Every arm and family invocation is emitted to `trace` when one is given,
    /// which observes execution without gaining any authority over it.
    ///
    /// Reached only through [`BatteryCapture`], which is what keeps an executor
    /// from being run against inputs no capture ever read.
    ///
    /// # Errors
    ///
    /// Refuses selection, tracked-contract, or source-identity failure before
    /// an executor can be used. A core family that refuses to acquire is not
    /// one of these: the capture succeeded, and the refusal it produced is the
    /// evidence every core position projects.
    ///
    /// [`BatteryCapture`]: super::BatteryCapture
    pub(super) fn capture_from_source(
        source_capture: &BatterySourceCapture,
        state_run: &BatteryStateRun,
        selected: Option<GateId>,
        selection: Option<&ChangeSelectionReport>,
        mut trace: Option<&'trace mut (dyn FnMut(RepositoryExecutionEvent) + Send)>,
    ) -> Result<Self, BatteryError> {
        let repository_source = &source_capture.repository;
        let actor = source_capture.actor.clone();
        let rim = source_capture.rim.clone();
        let hygiene = source_capture.hygiene.clone();
        let reached_paths = path_local_reach(selected, selection)?;
        let rim = if selected.is_some() {
            reached_paths
                .as_ref()
                .map_or(rim.clone(), |paths| rim.clone().with_changes(paths.clone()))
        } else {
            rim
        };
        let execution_objects =
            bound_execution_objects(selected, selection, reached_paths.as_deref(), rim.changes())?;
        let tree = Arc::clone(&repository_source.tree);
        let core_source = Arc::clone(&repository_source.core);
        let contract = Arc::clone(&repository_source.contract);
        let latches = &source_capture.latches;
        let CapturedCore {
            source: digest,
            evidence: core,
            mut bounds,
            mut observations,
        } = capture_core(
            &repository_source.core,
            &contract,
            &repository_source.baselines,
            selected,
            reached_paths.as_deref(),
            selection.map(ChangeSelectionReport::source),
            &mut trace,
        )?;
        let (metrics, metric_bounds, metric_observations) = match selected {
            None => {
                record_capture_analyses(
                    &mut trace,
                    ArmFamily::Metrics,
                    STRUCTURAL_ARMS.map(|arm| arm.id()),
                );
                let (evidence, bounds, observations) = metrics_family(
                    &tree,
                    &contract,
                    &repository_source.baselines,
                    &actor,
                    latches,
                    reached_paths.as_deref(),
                );
                (Some(evidence), bounds, observations)
            }
            Some(gate_id) if STRUCTURAL_ARMS.iter().any(|arm| arm.id() == gate_id) => {
                record_capture_analyses(&mut trace, ArmFamily::Metrics, [gate_id]);
                let (evidence, bounds, observations) = metrics_arm(
                    &tree,
                    &contract,
                    &repository_source.baselines,
                    &actor,
                    latches,
                    gate_id,
                    reached_paths.as_deref(),
                );
                (Some(evidence), bounds, observations)
            }
            Some(_) => (None, Vec::new(), Vec::new()),
        };
        bounds.extend(metric_bounds);
        observations.extend(metric_observations);
        state_run.observe(&observations, &bounds)?;
        Ok(Self {
            tree,
            core_source,
            contract,
            source: digest,
            core,
            metrics,
            baselines: Arc::clone(&repository_source.baselines),
            bounds,
            rim,
            hygiene,
            context: None,
            families: std::array::from_fn(|_| None),
            compiler_environment: Vec::new(),
            selected,
            reached_paths,
            execution_objects,
            reusable_rust_workspace: None,
            trace,
        })
    }

    /// Identity of the selected bytes this executor owns.
    #[must_use]
    pub const fn source(&self) -> &Digest {
        &self.source
    }

    /// Private selected-tree root retained for the executor's lifetime.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.tree.root()
    }

    pub(super) fn tree(&self) -> &SelectedTree {
        &self.tree
    }

    /// Exact changed paths captured at the command rim for this selected source.
    #[must_use]
    pub(super) fn changes(&self) -> &[String] {
        self.rim.changes()
    }

    /// Objects the validated ordinary reach plan actually bound to this capture.
    pub(super) fn execution_objects(&self) -> Option<&[ChangeReachObject]> {
        self.execution_objects.as_deref()
    }

    /// Stable source and outputs retained after a successful focused native check.
    pub(super) fn reusable_rust_workspace(&self) -> Option<&crate::RustAnalysisWorkspace> {
        self.reusable_rust_workspace.as_ref()
    }

    /// Prove the live selector source still names this retained tree.
    pub(super) fn verify_source(&self) -> Result<(), BatteryError> {
        self.tree
            .verify_source()
            .map_err(|error| BatteryError::new(error.to_string()))?;
        self.core_source
            .verify_git_controls()
            .map_err(|error| BatteryError::new(error.to_string()))
    }

    /// Preserve index movement as a signal for a reservation-scoped semantic recapture.
    pub(super) fn verify_refreshable_source(&self) -> Result<RefreshableSourceState, BatteryError> {
        self.core_source
            .verify_git_controls()
            .map_err(|error| BatteryError::new(error.to_string()))?;
        self.tree
            .verify_refreshable_source()
            .map_err(|error| BatteryError::new(error.to_string()))
    }

    /// Every regular path retained by this selected tree.
    pub(super) fn paths(&self) -> &[String] {
        self.tree.paths()
    }

    /// Canonical staged entries and their exact rename lineage.
    pub(super) fn staged_authority(&self) -> Option<&StagedAuthority> {
        self.tree.staged_authority()
    }

    pub(super) fn materialize_staged_base(&self) -> Result<Option<SelectedTree>, BatteryError> {
        self.tree
            .materialize_staged_base()
            .map_err(|error| BatteryError::new(error.to_string()))
    }

    pub(super) fn set_compiler_environment(
        &mut self,
        environment: Vec<(OsString, OsString)>,
    ) -> Result<(), BatteryError> {
        if self.families[family_index(ArmFamily::Toolchain)].is_some() {
            return Err(BatteryError::new(
                "compiler environment must be supplied before toolchain-family execution",
            ));
        }
        self.compiler_environment = environment;
        Ok(())
    }

    pub(super) fn compiler_environment(&self) -> &[(OsString, OsString)] {
        &self.compiler_environment
    }

    pub(super) fn action_tools(
        &self,
        gate_id: GateId,
        compiler_environment: &[(OsString, OsString)],
    ) -> Result<Vec<crate::ToolEvidence>, BatteryError> {
        let source = ToolchainSource::capture_with_baselines(
            &self.tree,
            self.rim.clone(),
            &self.baselines,
            compiler_environment,
            self.reached_paths.as_deref(),
        );
        acquire_toolchain_arm_tools(&source, &self.contract, gate_id)
            .map_err(|error| BatteryError::new(error.to_string()))
    }

    pub(super) fn proof_tool_authority(
        &self,
        compiler_environment: &[(OsString, OsString)],
    ) -> Result<Digest, BatteryError> {
        let source = ToolchainSource::capture_with_baselines(
            &self.tree,
            self.rim.clone(),
            &self.baselines,
            compiler_environment,
            self.reached_paths.as_deref(),
        );
        toolchain_executable_authority(&source).map_err(BatteryError::new)
    }

    /// The tracked contract this capture loaded from the selected tree.
    pub(super) fn contract(&self) -> &Contract {
        &self.contract
    }

    /// Every elastic bound the capture's core and structural acquisitions decided.
    ///
    /// These are the decisions the arms were already judged against, not a
    /// second resolution taken for the report. A family or row that could not
    /// decide a bound reports none for that position.
    pub(super) fn bounds(&self) -> &[BoundDecision] {
        &self.bounds
    }

    fn bind_inputs(&mut self, inputs: &BatteryInputs<'_>) -> Result<(), BatteryError> {
        if inputs.source != &self.source {
            return Err(BatteryError::new(format!(
                "battery inputs name source {}, but the executor owns {}",
                inputs.source, self.source
            )));
        }
        let context = inputs.snapshot()?;
        match &self.context {
            Some(bound) if bound != &context => Err(BatteryError::new(
                "repository executor cannot cross battery input contexts",
            )),
            Some(_) => Ok(()),
            None => {
                self.context = Some(context);
                Ok(())
            }
        }
    }

    fn record(&mut self, event: RepositoryExecutionEvent) {
        if let Some(trace) = &mut self.trace {
            trace(event);
        }
    }

    fn ensure_family(&mut self, family: ArmFamily) {
        let index = family_index(family);
        if self.families[index].is_some() {
            return;
        }
        self.record(RepositoryExecutionEvent::FamilyInvoked { family });
        let execution = self.acquire_family(family);
        self.families[index] = Some(execution);
    }

    fn acquire_family(&mut self, family: ArmFamily) -> FamilyExecution {
        let result = match family {
            // Not acquired here: this hands over the result the capture already
            // took, so the first core arm cannot start a second observation.
            ArmFamily::Core => self.core.clone().unwrap_or_else(|| {
                Err("selected capture did not acquire the core family".to_owned())
            }),
            ArmFamily::Toolchain => self.acquire_toolchain_family(),
            // Like core, this hands over the one result captured before the run
            // context froze its exact bound decisions.
            ArmFamily::Metrics => self.metrics.clone().unwrap_or_else(|| {
                Err("selected capture did not acquire the metrics family".to_owned())
            }),
            ArmFamily::Hygiene => self.acquire_hygiene_family(),
        };
        match result {
            Ok(evidence) => FamilyExecution::Complete(evidence),
            Err(reason) => FamilyExecution::Refused(reason),
        }
    }

    fn acquire_toolchain_family(&mut self) -> Result<Vec<ArmEvidence>, String> {
        let analyses = self.selected.map_or_else(
            || {
                TOOLCHAIN_ARMS
                    .iter()
                    .map(|arm| arm.id())
                    .collect::<Vec<_>>()
            },
            |gate_id| vec![gate_id],
        );
        self.record_analyses(ArmFamily::Toolchain, analyses);
        let source = ToolchainSource::capture_with_baselines(
            &self.tree,
            self.rim.clone(),
            &self.baselines,
            &self.compiler_environment,
            self.reached_paths.as_deref(),
        );
        if let Some(gate_id) = self.selected {
            let observation = acquire_toolchain_arm(&source, &self.contract, gate_id)
                .map_err(|error| error.to_string())?;
            let report = measure_toolchain_arm(observation).map_err(|error| error.to_string())?;
            if gate_id == GateId::RustTypecheck
                && report.outcome == crate::toolchain::ArmOutcome::Passed
            {
                self.reusable_rust_workspace = Some(
                    crate::toolchain::completed_rust_analysis_workspace(self.tree.repository())?,
                );
            }
            return Ok(vec![ArmEvidence::Toolchain { report }]);
        }
        let trace = &mut self.trace;
        let observations =
            acquire_toolchain_observed(&source, &self.contract, |gate_id, reusable_workspace| {
                if let Some(trace) = trace {
                    trace(RepositoryExecutionEvent::ToolchainAnalysisCompleted {
                        gate_id,
                        reusable_workspace,
                    });
                }
            });
        let report = measure_toolchain(observations).map_err(|error| error.to_string())?;
        Ok(report
            .arms
            .into_iter()
            .map(|report| ArmEvidence::Toolchain { report })
            .collect())
    }

    fn acquire_hygiene_family(&mut self) -> Result<Vec<ArmEvidence>, String> {
        let analyses = self.selected.map_or_else(
            || HYGIENE_ARMS.iter().map(|arm| arm.id()).collect::<Vec<_>>(),
            |gate_id| vec![gate_id],
        );
        self.record_analyses(ArmFamily::Hygiene, analyses);
        let source = if let Some(reached) = &self.reached_paths {
            HygieneSource::capture_focused(&self.tree, &self.contract, reached)
        } else {
            HygieneSource::capture(&self.tree, &self.contract)
        }
        .map_err(|error| error.to_string())?;
        if let Some(gate_id) = self.selected {
            let report = match &self.reached_paths {
                Some(reached) => {
                    acquire_hygiene_arm_reached(&source, &self.hygiene, gate_id, reached)
                }
                None => acquire_hygiene_arm(&source, &self.hygiene, gate_id),
            }
            .map_err(|error| error.to_string())?;
            return Ok(vec![ArmEvidence::Hygiene { report }]);
        }
        let report = acquire_hygiene(&source, &self.hygiene).map_err(|error| error.to_string())?;
        Ok(report
            .arms
            .into_iter()
            .map(|report| ArmEvidence::Hygiene { report })
            .collect())
    }

    fn record_analyses(&mut self, family: ArmFamily, gate_ids: impl IntoIterator<Item = GateId>) {
        for gate_id in gate_ids {
            self.record(RepositoryExecutionEvent::AnalysisInvoked { gate_id, family });
        }
    }

    fn project(&self, arm: &CompiledArm) -> Result<ArmEvidence, BatteryError> {
        let family = self.families[family_index(arm.family())]
            .as_ref()
            .expect("family is acquired before fold");
        match family {
            FamilyExecution::Refused(reason) => Err(BatteryError::new(format!(
                "{:?} family refused: {}",
                arm.family(),
                inline_evidence(reason)
            ))),
            FamilyExecution::Complete(evidence) => evidence
                .iter()
                .find(|item| item.gate_id() == arm.gate_id())
                .cloned()
                .ok_or_else(|| {
                    BatteryError::new(format!(
                        "{:?} family omitted compiled gate {}",
                        arm.family(),
                        arm.gate_id()
                    ))
                }),
        }
    }
}

impl BatteryExecutor for RepositoryExecutor<'_> {
    fn execute(
        &mut self,
        arm: &CompiledArm,
        inputs: &BatteryInputs<'_>,
    ) -> Result<ArmExecution, BatteryError> {
        self.record(RepositoryExecutionEvent::ArmInvoked {
            position: arm.position(),
            gate_id: arm.gate_id(),
            family: arm.family(),
        });
        self.bind_inputs(inputs)?;
        self.ensure_family(arm.family());
        let evidence = self.project(arm)?;
        ArmExecution::new(
            evidence.state()?,
            evidence.reason()?,
            evidence.provenance()?,
            evidence,
        )
    }
}

/// Project one arm's core evidence from run-wide source observations.
fn capture_core(
    source: &CoreSource,
    contract: &Contract,
    baselines: &BaselineCapture,
    selected: Option<GateId>,
    reached: Option<&[String]>,
    expected_source: Option<&Digest>,
    trace: &mut Option<&mut (dyn FnMut(RepositoryExecutionEvent) + Send)>,
) -> Result<CapturedCore, BatteryError> {
    if expected_source.is_some_and(|expected| expected != source.digest()) {
        return Err(BatteryError::new(
            "selected-arm reach authority names a different immutable source",
        ));
    }
    let (evidence, bounds, observations) = match selected {
        None => {
            record_capture_analyses(trace, ArmFamily::Core, CORE_GATE_IDS);
            let (evidence, bounds, observations) = core_family(source, contract, baselines);
            (Some(evidence), bounds, observations)
        }
        Some(gate_id) if CORE_GATE_IDS.contains(&gate_id) => {
            record_capture_analyses(trace, ArmFamily::Core, [gate_id]);
            let (evidence, bounds, observations) =
                core_arm(source, contract, baselines, gate_id, reached);
            (Some(evidence), bounds, observations)
        }
        Some(_) => (None, Vec::new(), Vec::new()),
    };
    Ok(CapturedCore {
        source: source.digest().clone(),
        evidence,
        bounds,
        observations,
    })
}

fn capture_baselines(
    repository: &Path,
    contract: &Contract,
    rim: &RimCapture,
    clean_merge_tree: Option<(&str, &str, &str, &Path)>,
) -> BaselineCapture {
    let classes = contract.rule_policy().document().file_classes();
    let mut suffixes = classes.magic().to_vec();
    suffixes.extend(classes.file_shape().iter().cloned());
    suffixes.extend(classes.complexity().iter().cloned());
    suffixes.extend(environment_baseline_suffixes());
    suffixes.extend(
        TASTE_EXTENSIONS
            .iter()
            .map(|extension| format!(".{extension}")),
    );
    suffixes.sort();
    suffixes.dedup();
    BaselineCapture::capture_with_paths(repository, &suffixes, rim.changes(), clean_merge_tree)
}

/// Acquire the complete structural family and retain every bound it decided.
///
/// This happens before [`BatteryInputs`] freezes the run context. The evidence
/// is still projected only when the canonical metrics positions execute, but
/// both that evidence and these decisions come from this single acquisition.
fn metrics_family(
    tree: &SelectedTree,
    contract: &Contract,
    baselines: &BaselineCapture,
    actor: &ActorCapture,
    latches: &JudgedLatches,
    reached: Option<&[String]>,
) -> (
    Result<Vec<ArmEvidence>, String>,
    Vec<BoundDecision>,
    Vec<LatchObservation>,
) {
    let source = match reached.map_or_else(
        || MetricSource::capture(tree, contract, baselines, actor, latches),
        |paths| MetricSource::capture_reached(tree, contract, baselines, actor, latches, paths),
    ) {
        Ok(source) => source,
        Err(error) => return (Err(error.to_string()), Vec::new(), Vec::new()),
    };
    let (evidence, bounds, observations) = acquire_metrics_with_bounds(&source).into_parts();
    let arms = evidence.map_err(|error| error.to_string()).map(|reports| {
        reports
            .into_iter()
            .map(|report| ArmEvidence::Metrics { report })
            .collect()
    });
    (arms, bounds, observations)
}

/// Acquire one structural arm and retain only the bounds it decided.
fn metrics_arm(
    tree: &SelectedTree,
    contract: &Contract,
    baselines: &BaselineCapture,
    actor: &ActorCapture,
    latches: &JudgedLatches,
    gate_id: GateId,
    reached: Option<&[String]>,
) -> (
    Result<Vec<ArmEvidence>, String>,
    Vec<BoundDecision>,
    Vec<LatchObservation>,
) {
    let source = match reached.map_or_else(
        || MetricSource::capture(tree, contract, baselines, actor, latches),
        |paths| MetricSource::capture_reached(tree, contract, baselines, actor, latches, paths),
    ) {
        Ok(source) => source,
        Err(error) => return (Err(error.to_string()), Vec::new(), Vec::new()),
    };
    let (evidence, bounds, observations) =
        acquire_metric_arm_with_bounds(&source, gate_id).into_parts();
    let arm = evidence
        .map(|report| vec![ArmEvidence::Metrics { report }])
        .map_err(|error| error.to_string());
    (arm, bounds, observations)
}

/// Acquire and judge the complete core family from one captured source.
///
/// Projected while the shared source authority is in hand rather than from a
/// later repository read, because that authority is the only thing that holds
/// the Git observations. A refusal is carried as the family's result instead
/// of failing the capture: an incomplete family is evidence the battery
/// projects, not a reason no battery can run.
///
/// The bounds the acquisition decided are returned beside the arms because this
/// is the only moment they exist: judging consumes the inputs, and re-resolving
/// them from the report afterwards would be a second decision nothing was
/// judged against.
fn core_family(
    source: &CoreSource,
    contract: &Contract,
    baselines: &BaselineCapture,
) -> (
    Result<Vec<ArmEvidence>, String>,
    Vec<BoundDecision>,
    Vec<LatchObservation>,
) {
    let (inputs, observations) =
        match crate::core::acquire_core_with_observations(source, contract, baselines) {
            Ok(acquisition) => acquisition,
            Err(error) => return (Err(error.to_string()), Vec::new(), Vec::new()),
        };
    let bounds = inputs.bounds().to_vec();
    let arms = evaluate_core(&inputs)
        .arms()
        .iter()
        .cloned()
        .map(|report| ArmEvidence::Core { report })
        .collect();
    (Ok(arms), bounds, observations)
}

/// Acquire and judge exactly one core arm from the shared captured source.
fn core_arm(
    source: &CoreSource,
    contract: &Contract,
    baselines: &BaselineCapture,
    gate_id: GateId,
    reached: Option<&[String]>,
) -> (
    Result<Vec<ArmEvidence>, String>,
    Vec<BoundDecision>,
    Vec<LatchObservation>,
) {
    let (input, bounds, observations) =
        match acquire_core_arm(source, contract, baselines, gate_id, reached) {
            Ok(acquisition) => acquisition,
            Err(error) => return (Err(error.to_string()), Vec::new(), Vec::new()),
        };
    let report = evaluate_core_arm(&input);
    (Ok(vec![ArmEvidence::Core { report }]), bounds, observations)
}

fn record_capture_analyses(
    trace: &mut Option<&mut (dyn FnMut(RepositoryExecutionEvent) + Send)>,
    family: ArmFamily,
    gate_ids: impl IntoIterator<Item = GateId>,
) {
    if let Some(trace) = trace {
        for gate_id in gate_ids {
            trace(RepositoryExecutionEvent::AnalysisInvoked { gate_id, family });
        }
    }
}

const PATH_LOCAL_ARMS: [GateId; 20] = [
    GateId::RepoDocs,
    GateId::Formatters,
    GateId::LocalPaths,
    GateId::Taste,
    GateId::EnvPolicy,
    GateId::FileShape,
    GateId::Complexity,
    GateId::MagicNumbers,
    GateId::MarkdownLinks,
    GateId::AssertionFreeTests,
    GateId::RustTypecheck,
    GateId::AppWebTypecheck,
    GateId::JavascriptUnused,
    GateId::CsharpUnusedCandidates,
    GateId::PythonTypecheck,
    GateId::EnvNameLedger,
    GateId::Reachability,
    GateId::SymbolReachability,
    GateId::PythonUnused,
    GateId::PrivateInternals,
];

const DEPENDENCY_PATH_ARMS: [GateId; 8] = [
    GateId::JavascriptUnused,
    GateId::CsharpUnusedCandidates,
    GateId::PythonTypecheck,
    GateId::EnvNameLedger,
    GateId::Reachability,
    GateId::SymbolReachability,
    GateId::PythonUnused,
    GateId::PrivateInternals,
];

fn bound_execution_objects(
    selected: Option<GateId>,
    selection: Option<&ChangeSelectionReport>,
    reached_paths: Option<&[String]>,
    rim_changes: &[String],
) -> Result<Option<Vec<ChangeReachObject>>, BatteryError> {
    let Some(selection) = selection else {
        return Ok(None);
    };
    let Some(gate_id) = selected else {
        return Ok(None);
    };
    let reach = selection.reach_for(gate_id).ok_or_else(|| {
        BatteryError::new(format!(
            "selected arm {gate_id} has no validated object reach authority"
        ))
    })?;
    let mut objects = match reach.mode() {
        ChangeReachMode::Exhaustive { .. } => vec![ChangeReachObject::Repository],
        ChangeReachMode::Focused => reached_paths.map_or_else(
            || {
                if reach.objects() == [ChangeReachObject::Repository] {
                    vec![ChangeReachObject::Repository]
                } else {
                    rim_changes
                        .iter()
                        .map(|path| ChangeReachObject::Path { path: path.clone() })
                        .collect()
                }
            },
            |paths| {
                paths
                    .iter()
                    .map(|path| ChangeReachObject::Path { path: path.clone() })
                    .collect()
            },
        ),
    };
    objects.sort();
    objects.dedup();
    if objects != reach.objects() {
        return Err(BatteryError::new(format!(
            "focused execution scope for {gate_id} does not equal its validated reach plan"
        )));
    }
    Ok(Some(objects))
}

fn path_local_reach(
    selected: Option<GateId>,
    selection: Option<&ChangeSelectionReport>,
) -> Result<Option<Vec<String>>, BatteryError> {
    let Some(selection) = selection else {
        return Ok(None);
    };
    let Some(gate_id) = selected else {
        let mut paths = std::collections::BTreeSet::new();
        for gate_id in selection
            .gate_ids()
            .iter()
            .copied()
            .filter(|gate_id| PATH_LOCAL_ARMS.contains(gate_id))
        {
            let Some(reached) = path_local_arm_reach(gate_id, selection)? else {
                return Ok(None);
            };
            paths.extend(reached);
        }
        return Ok(Some(paths.into_iter().collect()));
    };
    path_local_arm_reach(gate_id, selection)
}

fn path_local_arm_reach(
    gate_id: GateId,
    selection: &ChangeSelectionReport,
) -> Result<Option<Vec<String>>, BatteryError> {
    let reach = selection.reach_for(gate_id).ok_or_else(|| {
        BatteryError::new(format!(
            "selected arm {gate_id} has no validated object reach authority"
        ))
    })?;
    if !PATH_LOCAL_ARMS.contains(&gate_id)
        || matches!(reach.mode(), ChangeReachMode::Exhaustive { .. })
    {
        return Ok(None);
    }
    let mut paths = Vec::new();
    for object in reach.objects() {
        let ChangeReachObject::Path { path } = object else {
            return Err(BatteryError::new(format!(
                "focused path-local arm {gate_id} received non-path reach object {object:?}"
            )));
        };
        paths.push(path.clone());
    }
    if paths.is_empty() {
        return Err(BatteryError::new(format!(
            "focused path-local arm {gate_id} received empty authored reach"
        )));
    }
    let direct = selection
        .edges()
        .iter()
        .filter(|edge| edge.gate_id() == gate_id)
        .map(|edge| edge.source().to_owned())
        .collect::<Vec<_>>();
    if !DEPENDENCY_PATH_ARMS.contains(&gate_id) && paths != direct {
        return Err(BatteryError::new(format!(
            "focused path-local arm {gate_id} reach does not equal its direct authored path selection"
        )));
    }
    Ok(Some(paths))
}

const fn family_index(family: ArmFamily) -> usize {
    match family {
        ArmFamily::Core => 0,
        ArmFamily::Toolchain => 1,
        ArmFamily::Metrics => 2,
        ArmFamily::Hygiene => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_source_validation_includes_merge_control_authority() {
        let fixture = crate::test_repository::Fixture::repository("repository-control-source");
        fixture.write("tracked.txt", b"base\n");
        crate::test_repository::stage_all(fixture.path());
        crate::test_repository::commit(fixture.path(), "base");
        let selection =
            RepositorySelection::capture(fixture.path(), &Selector::staged(), InputMode::Generic)
                .expect("capture generic repository selection");
        let rim = RimCapture::new(fixture.path(), None, Vec::<String>::new())
            .expect("capture repository rim");
        let source = selection
            .bind(
                ClaimCapture::Idle,
                ActorCapture::Absent,
                &JudgedLatches::default(),
                &rim,
            )
            .expect("bind repository source");
        source
            .verify_source()
            .expect("unchanged repository source verifies");

        std::fs::write(fixture.join(".git/MERGE_MSG"), "arrived after capture\n")
            .expect("create merge control after capture");

        source
            .verify_source()
            .expect_err("merge-control creation must move repository source");
    }
}
