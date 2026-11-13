//! Acquisition of one complete core-family input battery.
//!
//! [`super::evaluate_core`] decides; this module only gathers. Every compiled
//! declaration produces exactly one observation, in compiled order, whether or
//! not its provider could measure. An arm this build cannot observe crosses as
//! an explicit non-completion state carrying its own recovery, so a missing
//! provider is visible in the report instead of being an omitted row.
//!
//! Two arms declare exact `git` argument vectors. Acquisition avoids them only
//! when captured repository state proves the same clean result; otherwise they
//! run while the source is captured under the contract's process bounds, and
//! every process failure becomes typed evidence rather than an acquisition
//! error. The only failures that refuse outright are the ones that would leave
//! the battery incomplete: an unreadable selected tree and a malformed arm.
//!
//! Acquisition is handed no repository. Every arm reads either the immutable
//! selected tree or an observation the capture already took, so a repository
//! mutated or swapped after capture has nothing left to reach: there is no
//! later state that can cross into a report under the captured digest.

use crate::measure::spawn;
use crate::toolchain::RimCapture;
use serde::{Deserialize, Serialize};
use sill::{Argv, Completion, Environment};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use wire::{Digest, Phase, TaskHandle};

use super::admission::{validate_git_tree, validate_repo_path};
use super::config_keys::config_keys;
use super::configuration_inventory::ConfigurationInventorySource;
use super::declaration::CoreArmDeclaration;
use super::shape::repo_shape;
use super::{
    CoreArmInput, CoreEvidence, CoreInputError, CoreInputs, CoreObservation, DocMeasurement,
    MergeIntegrityEvidence, MergeTreeCompletion, PlanningPhaseEvidence, RepoDocsEvidence,
    StagingEvidence, core_declarations,
};
use crate::GateId;
use crate::input::ParsedContract;
use crate::jurisdiction::Jurisdiction;
use crate::measure::Bounds;
use crate::metrics::{
    BaselineCapture, MetricId, MetricKey, MetricStateBinding, ParserEvidence, ParserId,
    measure_repository_document, resolve_metric_rule,
};
use crate::policy::PolicyContext;
use crate::state::{
    ActorInputs, BoundDecision, BoundError, JudgedLatches, LatchObservation, PeerClaim,
    owning_claim,
};
use crate::tree::SelectedTree;

mod merge_control;

use merge_control::GitControlSeal;

static GIT_SUBPROCESSES: AtomicU64 = AtomicU64::new(0);

/// Number of Git subprocess launches attempted by this process.
///
/// This is diagnostic observation only. It does not participate in gate
/// identity or decisions.
#[must_use]
pub fn git_subprocess_count() -> u64 {
    GIT_SUBPROCESSES.load(Ordering::Relaxed)
}

/// The board head captured with the source, before acquisition begins.
///
/// The gate holds no journal, so the claim is captured by the caller that owns
/// one and handed over whole. Absence is stated two different ways because a
/// store that answered "nothing is claimed" and a repository with no store at
/// all are different observations, and only the first is a measurement.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ClaimCapture {
    /// One task is claimed in this worktree, at this workflow phase.
    Active {
        /// Claimed task.
        task: TaskHandle,
        /// Current workflow phase of that claim.
        phase: Phase,
    },
    /// The claim store answered and holds no active claim.
    Idle,
    /// No claim store answered for this repository.
    Absent {
        /// Why no store answered.
        reason: String,
    },
}

/// The actor acting in this worktree, captured with the source.
///
/// Claim arbitration needs an identity the selected tree cannot supply: the
/// acting actor lives in the live worktree the gate does not read, exactly
/// like the board head above. The caller that owns one hands it over whole,
/// and a run with no acting actor says so rather than borrowing another input
/// to stand in for one.
///
/// The active bound claims ride with the actor because a claim is peer-held
/// only relative to someone: with no actor there is no peer to be outranked by.
/// Production capture replaces this caller observation with the active claims
/// from its fixed-clock [`crate::BatteryStateRun`]; direct acquisition tests
/// may still provide the typed observation explicitly.
#[derive(Clone, Debug, PartialEq)]
pub enum ActorCapture {
    /// One actor is acting in this worktree.
    Acting {
        /// Identity used for shared flex-claim arbitration.
        actor: String,
        /// Bound claims the ledger held active when the caller read it.
        bound_claims: Vec<PeerClaim>,
    },
    /// No actor is acting in this worktree.
    Absent,
}

impl ActorCapture {
    /// The identity acting in this worktree, when one is.
    #[must_use]
    pub fn active_actor(&self) -> Option<&str> {
        match self {
            Self::Acting { actor, .. } => Some(actor),
            Self::Absent => None,
        }
    }

    /// Retain one candidate path with the actor used for claim arbitration.
    ///
    /// The complete input crosses the transaction boundary so a measurement
    /// above base can claim its exact path after the bound decision. The bound
    /// itself does not consult this provenance.
    pub(crate) fn actor_inputs(&self, path: &str) -> Result<ActorInputs, BoundError> {
        let inputs = ActorInputs::new(path)?;
        Ok(match self {
            Self::Acting { actor, .. } => inputs.with_active_actor(actor),
            Self::Absent => inputs,
        })
    }

    /// Whether a peer holds the bound claim that governs `path`.
    ///
    /// Ownership is resolved by the ledger's own rule, so this acquisition and
    /// the state transaction that redirects a claimant name the same owner for
    /// one path. The actor is trimmed for the same reason the transaction
    /// trims its claimant: a claim is held by an identity, not by a spelling
    /// with surrounding space.
    fn peer_holds(&self, path: &str) -> bool {
        match self {
            Self::Acting {
                actor,
                bound_claims,
            } => {
                owning_claim(bound_claims, path).is_some_and(|claim| claim.actor() != actor.trim())
            }
            Self::Absent => false,
        }
    }
}

/// One captured Git source and everything acquisition is permitted to read.
///
/// Capture computes the immutable tree identity once and, in the same moment,
/// takes every Git-declared observation the run can project. Complete and
/// selected-arm acquisitions therefore bind to the same digest and live
/// repository instant, and [`CoreInputs`] cannot be assembled from a moving
/// tree or repository.
///
/// The durable latches are copied in for the same reason: a bounded arm decides
/// against the ledger this run publishes as its state authority, and reading
/// that ledger again mid-acquisition could answer from a version the report
/// never names.
#[derive(Debug)]
pub struct CoreSource {
    root: PathBuf,
    jurisdiction: Jurisdiction,
    digest: Digest,
    claim: ClaimCapture,
    actor: ActorCapture,
    git_controls: GitControlSeal,
    git: GitObservations,
    latches: JudgedLatches,
}

impl CoreSource {
    /// Capture one immutable source for the core family.
    ///
    /// The repository is taken from the tree rather than accepted beside it, so
    /// no caller can name one source for the identity and another for the Git
    /// evidence.
    ///
    /// `latches` are the durable breaches the caller read from the ledger it
    /// publishes as this run's state authority, handed over for the same reason
    /// its bound claims are: the enclosing battery state transaction supplies
    /// the one run-start reading the report names, rather than allowing this
    /// family to take a second one of its own.
    ///
    /// # Errors
    ///
    /// Returns the walk or read failure that prevented one complete tree
    /// identity. A partially digested tree is never returned.
    pub fn capture(
        tree: &SelectedTree,
        contract: &ParsedContract,
        claim: ClaimCapture,
        actor: ActorCapture,
        latches: &JudgedLatches,
        rim: &RimCapture,
    ) -> Result<Self, CoreInputError> {
        Self::capture_inner(tree, contract, claim, actor, latches, rim)
    }

    fn capture_inner(
        tree: &SelectedTree,
        contract: &ParsedContract,
        claim: ClaimCapture,
        actor: ActorCapture,
        latches: &JudgedLatches,
        rim: &RimCapture,
    ) -> Result<Self, CoreInputError> {
        // Taken before the walk because these are the only subjects here that
        // can still move: the selected tree was sealed at materialization,
        // while the repository and its allocator-owned merge controls stay
        // live and writable underneath the run.
        let git_controls = GitControlSeal::capture(tree.repository())?;
        let git_program = crate::git::program(rim.directory(), rim.search_path())
            .map_err(|error| CoreInputError::new(format!("resolve Git executable: {error}")))?;
        git_program.announce_secondary();
        let git = GitObservations::capture(tree, contract.bounds(), git_program.path());
        git_controls.verify()?;
        let jurisdiction = Jurisdiction::walk(tree.root(), contract.policy().ignore())
            .map_err(|error| CoreInputError::new(format!("core source jurisdiction: {error}")))?;
        let digest = jurisdiction
            .tree_digest()
            .map_err(|error| CoreInputError::new(format!("core source tree identity: {error}")))?;
        Ok(Self {
            root: tree.root().to_path_buf(),
            jurisdiction,
            digest,
            claim,
            actor,
            git_controls,
            git,
            latches: latches.clone(),
        })
    }

    /// Immutable selected-tree root every compiled provider reads.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Admitted inventory that produced this source identity.
    pub(super) const fn jurisdiction(&self) -> &Jurisdiction {
        &self.jurisdiction
    }

    /// Identity every acquired arm is bound to.
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }

    /// Board head captured with this source.
    #[must_use]
    pub const fn claim(&self) -> &ClaimCapture {
        &self.claim
    }

    /// Acting actor captured with this source.
    #[must_use]
    pub const fn actor(&self) -> &ActorCapture {
        &self.actor
    }

    /// Prove allocator-owned merge controls still name the captured Git instant.
    pub(crate) fn verify_git_controls(&self) -> Result<(), CoreInputError> {
        self.git_controls.verify()
    }

    /// Git's exact tree for a clean two-parent synthesis, when one was proven.
    ///
    /// This private capture evidence lets structural acquisition distinguish a
    /// file Git synthesized from a file the merge receiver edited. Conflicted,
    /// octopus, absent, and unmeasured merges return no tree and therefore grant
    /// no authorship exemption.
    #[must_use]
    pub(crate) fn clean_merge_tree(&self) -> Option<(&str, &str, &str, &Path)> {
        self.git.clean_merge_tree.as_ref().and_then(|capture| {
            let isolation = self.git.isolation.as_ref()?;
            Some((
                capture.head.as_str(),
                capture.merging.as_str(),
                capture.tree.as_str(),
                isolation.root.as_path(),
            ))
        })
    }
}

/// The two exact parent objects and clean tree one Git invocation joined.
#[derive(Debug)]
struct CleanMergeTree {
    head: String,
    merging: String,
    tree: String,
}

/// Git observations taken once when the source is captured.
///
/// Acquisition projects these rather than running the declared vectors again,
/// so the repository is read exactly once, in the same capture that fixes the
/// identity the arms are labelled with. The two declared observations cross as
/// typed evidence; the private clean-merge tree is optional and can only grant
/// a narrow authorship exemption when Git proved it.
#[derive(Debug)]
struct GitObservations {
    merge_integrity: Option<CoreObservation>,
    staging: Option<CoreObservation>,
    clean_merge_tree: Option<CleanMergeTree>,
    isolation: Option<GitIsolation>,
}

impl GitObservations {
    fn capture(tree: &SelectedTree, bounds: &Bounds, git_program: &Path) -> Self {
        let repository = tree.repository();
        let isolation = GitIsolation::create(tree);
        let ((merge_integrity, staging), clean_merge_tree) = thread::scope(|scope| {
            let merge_integrity =
                scope.spawn(|| merge_integrity(tree, bounds, isolation.as_ref(), git_program));
            let staging = scope.spawn(|| staging(tree, bounds, git_program));
            let clean_merge_tree =
                clean_merge_tree(repository, bounds, isolation.as_ref().ok(), git_program);
            (
                (
                    merge_integrity
                        .join()
                        .expect("scoped merge-integrity observation completes"),
                    staging
                        .join()
                        .expect("scoped staging observation completes"),
                ),
                clean_merge_tree,
            )
        });
        Self {
            merge_integrity: Some(merge_integrity),
            staging: Some(staging),
            clean_merge_tree,
            isolation: isolation.ok(),
        }
    }
}

/// One gate-owned object view for Git plumbing that synthesizes trees.
///
/// `merge-tree --write-tree` has no worktree or index mutation, but it normally
/// appends its result to the caller's object database. This private bare object
/// view makes the original database read-only through an alternate and gives
/// every synthesized object a destination below the selected tree's possessed
/// workspace. The object view stays alive through baseline capture because the
/// clean-merge authorship reader needs to traverse the synthesized tree.
#[derive(Debug)]
struct GitIsolation {
    root: PathBuf,
    objects: PathBuf,
    index: PathBuf,
    alternates: OsString,
}

impl GitIsolation {
    fn create(tree: &SelectedTree) -> Result<Self, String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let repository = crate::tree::open_repository(tree.repository())
            .map_err(|error| format!("open repository for isolated Git observation: {error}"))?;
        let original_objects = repository.common_dir().join("objects");
        let alternates = std::env::join_paths([&original_objects]).map_err(|error| {
            format!(
                "encode caller object database {} as a read-only alternate: {error}",
                original_objects.display()
            )
        })?;

        let mut root = None;
        for _ in 0..128 {
            let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
            let candidate = tree
                .private_workspace()
                .join(format!("git-observation-{}-{nonce}", std::process::id()));
            match fs::create_dir(&candidate) {
                Ok(()) => {
                    root = Some(candidate);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(format!(
                        "create isolated Git observation {}: {error}",
                        candidate.display()
                    ));
                }
            }
        }
        let root = root.ok_or_else(|| {
            "isolated Git observation name attempts exhausted in the gate workspace".to_owned()
        })?;
        let objects = root.join("objects");
        let result = (|| {
            fs::create_dir(&objects).map_err(|error| {
                format!(
                    "create isolated Git object database {}: {error}",
                    objects.display()
                )
            })?;
            fs::create_dir(objects.join("info"))
                .map_err(|error| format!("create isolated Git alternates directory: {error}"))?;
            fs::create_dir(root.join("refs"))
                .map_err(|error| format!("create isolated Git refs directory: {error}"))?;
            fs::write(root.join("HEAD"), "ref: refs/heads/main\n")
                .map_err(|error| format!("write isolated Git HEAD: {error}"))?;
            fs::write(
                root.join("config"),
                "[core]\n\tbare = true\n\trepositoryformatversion = 0\n",
            )
            .map_err(|error| format!("write isolated Git configuration: {error}"))?;
            fs::write(
                objects.join("info/alternates"),
                format!("{}\n", original_objects.display()),
            )
            .map_err(|error| format!("write isolated Git alternate: {error}"))?;
            Ok::<_, String>(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_dir_all(&root);
            return Err(error);
        }
        Ok(Self {
            index: root.join("index"),
            root,
            objects,
            alternates,
        })
    }

    fn environment(&self) -> Result<Environment, GitRefusal> {
        Environment::new([
            (OsString::from("GIT_OPTIONAL_LOCKS"), OsString::from("0")),
            (OsString::from("GIT_NO_LAZY_FETCH"), OsString::from("1")),
            (OsString::from("GIT_CONFIG_NOSYSTEM"), OsString::from("1")),
            (
                OsString::from("GIT_OBJECT_DIRECTORY"),
                self.objects.as_os_str().to_owned(),
            ),
            (
                OsString::from("GIT_ALTERNATE_OBJECT_DIRECTORIES"),
                self.alternates.clone(),
            ),
            (
                OsString::from("GIT_INDEX_FILE"),
                self.index.as_os_str().to_owned(),
            ),
        ])
        .map_err(|error| GitRefusal::Unmeasured(format!("isolated Git environment: {error}")))
    }
}

impl Drop for GitIsolation {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Acquire one complete core-family battery from one captured source.
///
/// Every declaration in [`core_declarations`] contributes exactly one arm, in
/// compiled order. No arm is omitted, none is invented, and no standing is
/// computed here. `baselines` must be the committed capture taken beside
/// `source`; repository-document debt is a comparison between those two
/// immutable inputs, not a later read of the live repository.
///
/// # Errors
///
/// Returns the first refusal that would leave the battery incomplete: a
/// provenance the declaration cannot supply, evidence that fails admission, or
/// an arm vector the family will not admit.
pub fn acquire_core(
    source: &CoreSource,
    contract: &ParsedContract,
    baselines: &BaselineCapture,
) -> Result<CoreInputs, CoreInputError> {
    acquire_core_with_observations(source, contract, baselines).map(|(inputs, _)| inputs)
}

/// Acquire the complete core family and retain its exact state observations.
pub(crate) fn acquire_core_with_observations(
    source: &CoreSource,
    contract: &ParsedContract,
    baselines: &BaselineCapture,
) -> Result<(CoreInputs, Vec<LatchObservation>), CoreInputError> {
    let configuration = ConfigurationInventorySource::capture(source)?;
    let mut arms = Vec::with_capacity(core_declarations().len());
    let mut decided = StateDecisions::default();
    for declaration in core_declarations() {
        arms.push(CoreArmInput::new(
            declaration.id(),
            source.digest.clone(),
            observe(
                declaration,
                source,
                contract,
                baselines,
                Some(&configuration),
                None,
                &mut decided,
            )?,
        )?);
    }
    let StateDecisions {
        bounds,
        observations,
    } = decided;
    CoreInputs::new(arms).map(|inputs| (inputs.with_bounds(bounds), observations))
}

/// Acquire exactly one core arm and the elastic bounds that arm decided.
pub(crate) fn acquire_core_arm(
    source: &CoreSource,
    contract: &ParsedContract,
    baselines: &BaselineCapture,
    gate_id: GateId,
    reached: Option<&[String]>,
) -> Result<(CoreArmInput, Vec<BoundDecision>, Vec<LatchObservation>), CoreInputError> {
    let declaration = core_declarations()
        .iter()
        .find(|declaration| declaration.id() == gate_id)
        .ok_or_else(|| CoreInputError::new(format!("{gate_id} is not a compiled core arm")))?;
    let configuration = matches!(
        gate_id,
        GateId::ConfigFalseDisable | GateId::ConfigTrackedTrust
    )
    .then(|| ConfigurationInventorySource::capture(source))
    .transpose()?;
    let mut decided = StateDecisions::default();
    let observation = observe(
        declaration,
        source,
        contract,
        baselines,
        configuration.as_ref(),
        reached,
        &mut decided,
    )?;
    let StateDecisions {
        bounds,
        observations,
    } = decided;
    CoreArmInput::new(gate_id, source.digest.clone(), observation)
        .map(|input| (input, bounds, observations))
}

/// The state effects core acquisition decides while observing: the elastic
/// bounds it settled and the latch observations it recorded.
#[derive(Default)]
struct StateDecisions {
    bounds: Vec<BoundDecision>,
    observations: Vec<LatchObservation>,
}

fn observe(
    declaration: &CoreArmDeclaration,
    source: &CoreSource,
    contract: &ParsedContract,
    baselines: &BaselineCapture,
    configuration: Option<&ConfigurationInventorySource>,
    reached: Option<&[String]>,
    decided: &mut StateDecisions,
) -> Result<CoreObservation, CoreInputError> {
    Ok(match declaration.id() {
        GateId::MergeIntegrity => source.git.merge_integrity.clone().ok_or_else(|| {
            CoreInputError::new("merge-integrity acquisition omitted its Git observation")
        })?,
        GateId::PlanningPhase => planning_phase(&source.claim),
        GateId::RepoShape => return repo_shape(source, contract),
        GateId::Staging => source.git.staging.clone().ok_or_else(|| {
            CoreInputError::new("staging acquisition omitted its Git observation")
        })?,
        GateId::ConfigKeyValidity => return config_keys(contract),
        GateId::RepoDocs => {
            return repo_docs(source, contract, baselines, reached, decided);
        }
        GateId::ConfigFalseDisable => {
            return configuration
                .ok_or_else(|| {
                    CoreInputError::new("false-disable acquisition omitted its configuration")
                })?
                .false_disable();
        }
        GateId::ConfigTrackedTrust => {
            return configuration
                .ok_or_else(|| {
                    CoreInputError::new("tracked-trust acquisition omitted its configuration")
                })?
                .tracked_trust();
        }
        id => unreachable!("{id} is not a compiled core declaration"),
    })
}

fn repo_docs(
    source: &CoreSource,
    contract: &ParsedContract,
    baselines: &BaselineCapture,
    reached: Option<&[String]>,
    decided: &mut StateDecisions,
) -> Result<CoreObservation, CoreInputError> {
    if let BaselineCapture::Unreachable(reason) = baselines {
        return Err(CoreInputError::new(format!(
            "committed repository-document parents could not be read: {reason}"
        )));
    }
    let mut measurements = Vec::new();
    for entry in source.jurisdiction.measurable() {
        let path = entry.path();
        if reached.is_some_and(|reached| !reached.iter().any(|candidate| candidate == path)) {
            continue;
        }
        let selected = contract
            .rule_policy()
            .selects_repository_document(path)
            .map_err(|error| {
                CoreInputError::new(format!(
                    "repository-document selection for {path:?}: {error}"
                ))
            })?;
        if !selected {
            continue;
        }
        let bytes = fs::read(source.root.join(path)).map_err(|error| {
            CoreInputError::new(format!(
                "read selected repository document {path:?}: {error}"
            ))
        })?;
        let document = measure_repository_document(path, &bytes).map_err(|error| {
            CoreInputError::new(format!(
                "measure selected repository document {path:?}: {error}"
            ))
        })?;
        let committed = baselines
            .baseline_bytes(path, &bytes)
            .filter(|committed| *committed != bytes)
            .and_then(|committed| measure_repository_document(path, committed).ok())
            .map(|document| document.characters());
        let (measurement, decision, observation) = document_measurement(
            &source.actor,
            contract,
            &source.latches,
            path,
            document.characters(),
            committed,
        )?;
        measurements.push(measurement);
        decided.bounds.push(decision);
        decided.observations.push(observation);
    }
    RepoDocsEvidence::new(measurements)
        .map(CoreEvidence::RepoDocs)
        .map(measured)
}

/// Decide one selected document's bound and retain its claim actor.
///
/// The decision is taken through [`MetricStateBinding`], which is the same
/// route [`crate::state::CurrentState`] reconciles a durable latch through, so
/// the limit this arm is judged against and the limit the ledger would latch
/// are one computation rather than two spellings of it. That binding resolves
/// the rule and `repo_doc_chars` latch key from the tracked contract, so
/// nothing here restates a policy field. Actor provenance remains on the
/// transaction observation only so an over-base document can enter shared
/// claim arbitration; it does not change the compiled ceiling.
///
/// A latched document is held to its base limit, because a breach already
/// spent the elastic headroom the ratchet lends. The latch is read from the
/// ledger this run published as its state authority; changed committed bytes
/// already above base infers the same state before a first durable observation
/// can exist. The arm and the report therefore answer to one debt rule whether
/// the prior breach is visible in the ledger or in the committed document.
///
/// A bound claim is resolved only for a document that breached its cap, which
/// is the only set worth asking the ledger about: a document inside its
/// bound claims nothing and can be held by nobody. An unlimited document is
/// never claimed for the same reason, since it has no cap left to breach.
fn document_measurement(
    actor: &ActorCapture,
    contract: &ParsedContract,
    latches: &JudgedLatches,
    path: &str,
    characters: u64,
    committed: Option<u64>,
) -> Result<(DocMeasurement, BoundDecision, LatchObservation), CoreInputError> {
    let rule = resolve_metric_rule(
        contract.rule_policy(),
        MetricId::RepositoryDocumentCharacters,
        &PolicyContext::new().with_path(path),
    )
    .map_err(|error| {
        CoreInputError::new(format!(
            "resolve repository-document bound for {path:?}: {error}"
        ))
    })?;
    let committed_over_base = rule.changed_committed_base_breach(committed);
    let key = MetricKey::path(path)
        .map_err(|error| CoreInputError::new(format!("repository-document key: {error}")))?;
    let binding = MetricStateBinding::new(
        MetricId::RepositoryDocumentCharacters,
        key,
        characters,
        ParserEvidence::Native {
            parser: ParserId::Utf8DocumentCharactersV1,
        },
        rule,
    )
    .map_err(|error| CoreInputError::new(format!("bind repository-document {path:?}: {error}")))?;
    let actor_inputs = actor.actor_inputs(path).map_err(|error| {
        CoreInputError::new(format!(
            "actor provenance for repository document {path:?}: {error}"
        ))
    })?;
    let (decision, observation) = binding
        .observe_decision(latches, committed_over_base, &actor_inputs)
        .map_err(|error| {
            CoreInputError::new(format!(
                "resolve active repository-document bound for {path:?}: {error}"
            ))
        })?;
    let measurement = if decision.unlimited() {
        DocMeasurement::unlimited(path, characters)?
    } else {
        let peer_claim = decision.over_limit() && actor.peer_holds(path);
        DocMeasurement::new(path, characters, decision.active_limit(), peer_claim)?
            .with_inheritance(decision.inherited().cloned())
            .with_cover(latches.cover(binding.entry(), &decision).cloned())
    };
    Ok((measurement, decision, observation))
}

fn planning_phase(claim: &ClaimCapture) -> CoreObservation {
    match claim {
        ClaimCapture::Active { task, phase } => {
            measured(CoreEvidence::PlanningPhase(PlanningPhaseEvidence::Active {
                task: task.clone(),
                phase: *phase,
            }))
        }
        ClaimCapture::Idle => measured(CoreEvidence::PlanningPhase(
            PlanningPhaseEvidence::NoActiveClaim,
        )),
        ClaimCapture::Absent { reason } => CoreObservation::Skipped {
            reason: reason.clone(),
        },
    }
}

fn merge_integrity(
    tree: &SelectedTree,
    bounds: &Bounds,
    isolation: Result<&GitIsolation, &String>,
    git_program: &Path,
) -> CoreObservation {
    let repository = tree.repository();
    match merge_head_path(repository) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return measured(CoreEvidence::MergeIntegrity(
                MergeIntegrityEvidence::NoMerge,
            ));
        }
        Err(refusal) => return refusal.into_observation(),
    }

    let staged_tree = match tree.staged_authority() {
        Some(authority) => authority.candidate_tree_identity().to_owned(),
        None => {
            return GitRefusal::Unmeasured(
                "merge-integrity cannot identify a live index from a revision selection".to_owned(),
            )
            .into_observation();
        }
    };
    let head_tree = match git_tree(
        git_program,
        repository,
        bounds,
        &["rev-parse", "HEAD^{tree}"],
    ) {
        Ok(tree) => tree,
        Err(refusal) => return refusal.into_observation(),
    };
    if staged_tree != head_tree {
        return measured(CoreEvidence::MergeIntegrity(
            MergeIntegrityEvidence::StagedContent {
                head_tree,
                staged_tree,
            },
        ));
    }

    let arguments = &[
        "merge-tree",
        "--write-tree",
        "-z",
        "--name-only",
        "HEAD",
        "MERGE_HEAD",
    ];
    let isolation = match isolation {
        Ok(isolation) => isolation,
        Err(reason) => {
            return GitRefusal::Unmeasured(reason.clone()).into_observation();
        }
    };
    let (code, output) = match isolated_git(
        git_program,
        repository,
        bounds,
        arguments,
        &[0, 1],
        isolation,
    ) {
        Ok(pair) => pair,
        Err(refusal) => return refusal.into_observation(),
    };
    let completion = if code == 0 {
        MergeTreeCompletion::Clean
    } else {
        MergeTreeCompletion::Conflicted
    };
    match parse_merge_tree(&output) {
        Ok((expected_tree, conflicted_paths)) => measured(CoreEvidence::MergeIntegrity(
            MergeIntegrityEvidence::Compared {
                head_tree,
                expected_tree,
                completion,
                conflicted_paths,
            },
        )),
        Err(refusal) => refusal.into_observation(),
    }
}

/// Compute Git's clean two-parent result without granting it gate standing.
///
/// The merge-integrity arm only needs `merge-tree` when the staged tree still
/// equals `HEAD`; file authorship needs the clean result even when the pending
/// index correctly contains it. This observation therefore rides beside the
/// public core evidence. Only exit zero proves an automatic clean synthesis.
/// A conflict, multiple merge heads, unreadable state, or process failure is a
/// conservative absence: structural arms keep measuring the staged bytes.
fn clean_merge_tree(
    repository: &Path,
    bounds: &Bounds,
    isolation: Option<&GitIsolation>,
    git_program: &Path,
) -> Option<CleanMergeTree> {
    let merge_head_path = merge_head_path(repository).ok()??;
    let merge_heads = fs::read_to_string(merge_head_path).ok()?;
    let mut merge_heads = merge_heads
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let merging = merge_heads.next()?;
    if merge_heads.next().is_some() {
        return None;
    }
    let (_, head) = git(
        git_program,
        repository,
        bounds,
        &["rev-parse", "HEAD"],
        &[0],
    )
    .ok()?;
    let head = head.trim().to_owned();
    validate_git_tree(&head).ok()?;
    let (code, output) = isolated_git(
        git_program,
        repository,
        bounds,
        &["merge-tree", "--write-tree", &head, merging],
        &[0, 1],
        isolation?,
    )
    .ok()?;
    if code != 0 {
        return None;
    }
    let tree = output.lines().next()?.trim().to_owned();
    validate_git_tree(&tree).ok()?;
    Some(CleanMergeTree {
        head,
        merging: merging.to_owned(),
        tree,
    })
}

/// Split `--write-tree -z --name-only` output into its tree and conflict names.
///
/// The record separator is NUL and the conflicted-name section is terminated by
/// an empty record, after which the remainder is informational text this gate
/// does not interpret. Every name is put through the same admission the family
/// applies to any repository path, so an unusable name is unmeasured evidence
/// rather than an arm that refuses the whole battery.
fn parse_merge_tree(output: &str) -> Result<(String, Vec<String>), GitRefusal> {
    let mut records = output.split('\0');
    let tree = records.next().unwrap_or_default().trim().to_owned();
    validate_git_tree(&tree).map_err(|error| {
        GitRefusal::Unmeasured(format!("git merge-tree computed no tree: {error}"))
    })?;
    let mut conflicted: Vec<String> = records
        .take_while(|record| !record.is_empty())
        .map(|record| record.trim_matches('\n').to_owned())
        .filter(|record| !record.is_empty())
        .collect();
    conflicted.sort();
    conflicted.dedup();
    for path in &conflicted {
        validate_repo_path(path).map_err(|error| {
            GitRefusal::Unmeasured(format!("git merge-tree named an unusable path: {error}"))
        })?;
    }
    Ok((tree, conflicted))
}

fn git_tree(
    git_program: &Path,
    repository: &Path,
    bounds: &Bounds,
    arguments: &[&str],
) -> Result<String, GitRefusal> {
    let (_, output) = git(git_program, repository, bounds, arguments, &[0])?;
    let tree = output.trim().to_owned();
    validate_git_tree(&tree).map_err(|error| {
        GitRefusal::Unmeasured(format!(
            "git {} named no tree: {error}",
            arguments.join(" ")
        ))
    })?;
    Ok(tree)
}

fn merge_head_path(repository: &Path) -> Result<Option<PathBuf>, GitRefusal> {
    let repository = crate::tree::open_repository(repository).map_err(|error| {
        GitRefusal::Unmeasured(format!("resolve Git directory for MERGE_HEAD: {error}"))
    })?;
    let path = repository.git_dir().join("MERGE_HEAD");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(Some(path)),
        Ok(_) => Err(GitRefusal::Unmeasured(format!(
            "Git MERGE_HEAD is not a regular file: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(GitRefusal::Unmeasured(format!(
            "inspect Git MERGE_HEAD {}: {error}",
            path.display()
        ))),
    }
}

fn staging(tree: &SelectedTree, bounds: &Bounds, git_program: &Path) -> CoreObservation {
    if staging_worktree_matches(tree) {
        return measured(CoreEvidence::Staging(
            StagingEvidence::new(std::iter::empty::<&str>())
                .expect("an empty staging intersection is valid"),
        ));
    }
    let repository = tree.repository();
    let arguments = &["diff", "--cached", "--name-only", "--diff-filter=ACMR"];
    let staged = match git(git_program, repository, bounds, arguments, &[0]) {
        Ok((_, output)) => output,
        Err(refusal) => return refusal.into_observation(),
    };
    let unstaged = match git(
        git_program,
        repository,
        bounds,
        &["diff", "--name-only"],
        &[0],
    ) {
        Ok((_, output)) => output,
        Err(refusal) => return refusal.into_observation(),
    };
    let unstaged: Vec<&str> = unstaged.lines().map(str::trim).collect();
    let partial = staged
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty() && unstaged.contains(path));
    match StagingEvidence::new(partial) {
        Ok(evidence) => measured(CoreEvidence::Staging(evidence)),
        Err(error) => CoreObservation::Unmeasured {
            reason: format!("git named an unusable path: {error}"),
        },
    }
}

/// Prove the staged paths cannot intersect Git's unstaged path set.
///
/// A selected path whose exact bytes and executable mode still match the
/// materialized index cannot be an unstaged edit. Deleted staged paths are not
/// members of the declared `ACMR` observation. Any uncertainty returns false
/// and leaves the existing Git-backed observation authoritative.
fn staging_worktree_matches(tree: &SelectedTree) -> bool {
    let Some(changes) = tree.staged_changes() else {
        return false;
    };
    if tree.verify_source().is_err() {
        return false;
    }
    changes.iter().all(|path| {
        let selected = tree.root().join(path);
        let Ok(selected_metadata) = fs::symlink_metadata(&selected) else {
            return true;
        };
        if !selected_metadata.file_type().is_file() {
            return false;
        }
        let worktree = tree.repository().join(path);
        let Ok(worktree_metadata) = fs::symlink_metadata(&worktree) else {
            return false;
        };
        if !worktree_metadata.file_type().is_file()
            || !worktree_mode_matches(tree, path, &worktree_metadata)
        {
            return false;
        }
        fs::read(&selected)
            .and_then(|selected| fs::read(&worktree).map(|worktree| selected == worktree))
            .unwrap_or(false)
    })
}

#[cfg(target_family = "unix")]
fn worktree_mode_matches(tree: &SelectedTree, path: &str, metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    let executable = metadata.permissions().mode() & 0o111 != 0;
    tree.mode(path)
        .is_some_and(|mode| executable == matches!(mode, crate::tree::SelectedFileMode::Executable))
}

#[cfg(not(target_family = "unix"))]
fn worktree_mode_matches(_tree: &SelectedTree, _path: &str, _metadata: &fs::Metadata) -> bool {
    false
}

const fn measured(evidence: CoreEvidence) -> CoreObservation {
    CoreObservation::Measured { evidence }
}

/// Why one declared Git observation did not complete.
enum GitRefusal {
    Unavailable(String),
    TimedOut(u64),
    Unmeasured(String),
}

impl GitRefusal {
    fn into_observation(self) -> CoreObservation {
        match self {
            Self::Unavailable(reason) => CoreObservation::Unavailable { reason },
            Self::TimedOut(milliseconds) => CoreObservation::TimedOut { milliseconds },
            Self::Unmeasured(reason) => CoreObservation::Unmeasured { reason },
        }
    }
}

/// Run one declared Git observation and classify how it completed.
///
/// A non-accepted exit code, a lost or truncated capture, and a deadline are
/// each their own typed evidence. None of them is a measurement, and none of
/// them refuses the battery.
fn git(
    git_program: &Path,
    repository: &Path,
    bounds: &Bounds,
    arguments: &[&str],
    accepted: &[i32],
) -> Result<(i32, String), GitRefusal> {
    let environment = Environment::new([
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
    ])
    .map_err(|error| GitRefusal::Unmeasured(format!("git environment: {error}")))?;
    git_with_environment(
        git_program,
        repository,
        bounds,
        arguments,
        accepted,
        environment,
    )
}

fn isolated_git(
    git_program: &Path,
    repository: &Path,
    bounds: &Bounds,
    arguments: &[&str],
    accepted: &[i32],
    isolation: &GitIsolation,
) -> Result<(i32, String), GitRefusal> {
    git_with_environment(
        git_program,
        repository,
        bounds,
        arguments,
        accepted,
        isolation.environment()?,
    )
}

fn git_with_environment(
    git_program: &Path,
    repository: &Path,
    bounds: &Bounds,
    arguments: &[&str],
    accepted: &[i32],
    environment: Environment,
) -> Result<(i32, String), GitRefusal> {
    let argv = Argv::new(git_program.as_os_str(), arguments.iter().map(OsStr::new))
        .map_err(|error| GitRefusal::Unmeasured(format!("git argument vector: {error}")))?;
    let mut spec = bounds.spec(argv, repository);
    spec.environment = environment;
    GIT_SUBPROCESSES.fetch_add(1, Ordering::Relaxed);
    let outcome = spawn(spec)
        .map_err(|error| GitRefusal::Unavailable(format!("git did not start: {error}")))?
        .wait()
        .map_err(|error| GitRefusal::Unmeasured(format!("git was not reaped: {error}")))?;

    let named = arguments.join(" ");
    let status = match outcome.completion() {
        Completion::Exited(status) => *status,
        Completion::TimedOut(_) => {
            let milliseconds = bounds.deadlines().runtime().as_millis();
            let milliseconds = u64::try_from(milliseconds).unwrap_or(u64::MAX);
            return Err(GitRefusal::TimedOut(milliseconds.max(1)));
        }
        Completion::Cancelled(_) => {
            return Err(GitRefusal::Unmeasured(format!("git {named} was cancelled")));
        }
    };
    let code = status
        .code()
        .ok_or_else(|| GitRefusal::Unmeasured(format!("git {named} left no exit code")))?;
    if !accepted.contains(&code) {
        return Err(GitRefusal::Unmeasured(format!("git {named} exited {code}")));
    }

    let captured = outcome
        .stdout()
        .ok_or_else(|| GitRefusal::Unmeasured(format!("git {named} returned no stdout")))?;
    if captured.truncated() || !captured.complete() {
        return Err(GitRefusal::Unmeasured(format!(
            "git {named} stdout was truncated={} complete={}",
            captured.truncated(),
            captured.complete()
        )));
    }
    Ok((code, String::from_utf8_lossy(captured.bytes()).into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RepositoryFixture {
        root: PathBuf,
    }

    impl RepositoryFixture {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "gate-core-acquire-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).expect("create core acquisition fixture");
            crate::test_repository::initialize(&root);
            Self { root }
        }
    }

    impl Drop for RepositoryFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn index_movement_invalidates_an_empty_staging_shortcut() {
        let fixture = RepositoryFixture::new("moved-index");
        let root = &fixture.root;
        fs::write(root.join("tracked.txt"), "committed\n").expect("write committed file");
        crate::test_repository::stage_all(root);
        crate::test_repository::commit(root, "initial");

        let tree = crate::Selector::staged()
            .materialize(root)
            .expect("materialize clean staged source");
        assert!(staging_worktree_matches(&tree));

        fs::write(root.join("tracked.txt"), "staged later\n").expect("move worktree");
        crate::test_repository::stage(root, &["tracked.txt"]);
        assert!(!staging_worktree_matches(&tree));
    }

    #[test]
    fn staged_deletion_remains_eligible_for_the_staging_shortcut() {
        let fixture = RepositoryFixture::new("staged-deletion");
        let root = &fixture.root;
        fs::write(root.join("tracked.txt"), "committed\n").expect("write committed file");
        crate::test_repository::stage_all(root);
        crate::test_repository::commit(root, "initial");

        fs::remove_file(root.join("tracked.txt")).expect("delete tracked file");
        crate::test_repository::stage_all(root);
        let tree = crate::Selector::staged()
            .materialize(root)
            .expect("materialize staged deletion");
        assert!(staging_worktree_matches(&tree));
    }

    #[test]
    fn merge_head_resolves_from_the_repository_git_directory() {
        let fixture = RepositoryFixture::new("merge");
        let root = &fixture.root;
        let clean = merge_head_path(root).unwrap_or_else(|_| panic!("inspect clean repository"));
        assert_eq!(clean, None);

        let expected = root.join(".git/MERGE_HEAD");
        fs::write(&expected, "0000000000000000000000000000000000000000\n")
            .expect("write merge head");
        assert_eq!(
            merge_head_path(root).unwrap_or_else(|_| panic!("inspect active merge")),
            Some(expected)
        );
    }
}
