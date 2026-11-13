//! Pure external-release planning and one resumable execution path.
//!
//! The planner has no effect port. It seals every observation and the complete
//! operation inventory into one digest. The executor accepts only that digest,
//! re-observes the entire binding before the first effect, admits the already
//! converged prefix from durable receipts and authoritative external state, and
//! brackets every remaining effect with an accepted-operation row and a typed
//! receipt.

use std::fmt;

use serde::{Deserialize, Serialize};
use wire::body::PublishedTarget;
use wire::manifest::{ProductVersion, ReleaseTarget};
use wire::release::{Operation, RegistryEvidence, ReleaseObservations, ReleasePlan};
use wire::{Digest, Domain};

/// The sole ordered operation graph for fresh and resumed publication.
const OPERATIONS: &[Operation] = &[
    Operation::ResolvePreconditionsAndCredentials,
    Operation::RunCorrectnessGate,
    Operation::InstallAndProbeArtifacts,
    Operation::RehearseUpload,
    Operation::UploadPackage,
    Operation::AwaitRegistryVisibility,
    Operation::CreateImmutableTag,
    Operation::Announce,
];

/// Operations a package already visible at the release authority proves
/// crossed: once the exact package is published, nothing before visibility
/// may run again.
const PUBLICATION_PREFIX: [Operation; 5] = [
    Operation::RunCorrectnessGate,
    Operation::InstallAndProbeArtifacts,
    Operation::RehearseUpload,
    Operation::UploadPackage,
    Operation::AwaitRegistryVisibility,
];

/// All caller and repository input from which a release plan is derived.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanInput {
    pub repository: Digest,
    pub requested_version: ProductVersion,
    pub source_commit: String,
    pub source_archive: Digest,
    pub release_commit: String,
    pub notes_document: String,
    pub manifest: Digest,
    pub package: Digest,
    pub package_object: String,
    pub installed_artifact: Digest,
    pub targets: Vec<PublishedTarget>,
    pub observations: ReleaseObservations,
}

#[derive(Serialize)]
struct PlanBinding<'a> {
    repository: &'a Digest,
    requested_version: &'a ProductVersion,
    source_commit: &'a str,
    source_archive: &'a Digest,
    release_commit: &'a str,
    notes_document: &'a str,
    notes: &'a Digest,
    manifest: &'a Digest,
    package: &'a Digest,
    package_object: &'a str,
    installed_artifact: &'a Digest,
    targets: &'a [PublishedTarget],
    observations: &'a ReleaseObservations,
    operations: &'a [Operation],
}

/// Build a release plan without invoking an effect adapter.
///
/// # Errors
///
/// Refuses every local or remote observation that would make apply ambiguous,
/// unsafe, or capable of publishing a different release than the preview.
pub fn plan_release(input: PlanInput) -> Result<ReleasePlan, PlanError> {
    validate_object("source commit", &input.source_commit)?;
    validate_object("release commit", &input.release_commit)?;
    validate_object("package object", &input.package_object)?;
    validate_object("HEAD", &input.observations.head)?;
    validate_observations(
        &input.requested_version,
        &input.source_commit,
        &input.release_commit,
        &input.package_object,
        &input.observations,
    )?;
    validate_targets(&input.targets)?;
    let notes = Digest::compute(Domain::DistReleaseNotes, input.notes_document.as_bytes());
    let mut plan = ReleasePlan {
        key: Digest::compute(Domain::DistReleasePlan, b"unsealed"),
        repository: input.repository,
        requested_version: input.requested_version,
        source_commit: input.source_commit,
        source_archive: input.source_archive,
        release_commit: input.release_commit,
        manifest: input.manifest,
        notes,
        notes_document: input.notes_document,
        package: input.package,
        package_object: input.package_object,
        installed_artifact: input.installed_artifact,
        targets: input.targets,
        operations: OPERATIONS.to_vec(),
        observations: input.observations,
    };
    plan.key = plan_identity(&plan)?;
    Ok(plan)
}

/// Identity written only after this plan's full correctness gate succeeds.
#[must_use]
pub fn gate_receipt(plan: &ReleasePlan) -> Digest {
    Digest::compute(
        Domain::GateReceipt,
        format!("{}:{}", plan.key, Operation::RunCorrectnessGate.as_str()).as_bytes(),
    )
}

/// Recompute and compare the complete plan identity.
///
/// # Errors
///
/// Returns an encoding fault or a moved-key refusal.
pub fn verify_plan(plan: &ReleasePlan) -> Result<(), PlanError> {
    validate_object("source commit", &plan.source_commit)?;
    validate_object("release commit", &plan.release_commit)?;
    validate_object("package object", &plan.package_object)?;
    validate_object("HEAD", &plan.observations.head)?;
    validate_observations(
        &plan.requested_version,
        &plan.source_commit,
        &plan.release_commit,
        &plan.package_object,
        &plan.observations,
    )?;
    validate_targets(&plan.targets)?;
    if plan.operations != OPERATIONS {
        return Err(PlanError::OperationList);
    }
    let notes = Digest::compute(Domain::DistReleaseNotes, plan.notes_document.as_bytes());
    if plan.notes != notes {
        return Err(PlanError::NotesIdentity);
    }
    let observed = plan_identity(plan)?;
    if observed == plan.key {
        Ok(())
    } else {
        Err(PlanError::KeyMoved {
            expected: plan.key.clone(),
            observed,
        })
    }
}

fn plan_identity(plan: &ReleasePlan) -> Result<Digest, PlanError> {
    let binding = PlanBinding {
        repository: &plan.repository,
        requested_version: &plan.requested_version,
        source_commit: &plan.source_commit,
        source_archive: &plan.source_archive,
        release_commit: &plan.release_commit,
        notes_document: &plan.notes_document,
        notes: &plan.notes,
        manifest: &plan.manifest,
        package: &plan.package,
        package_object: &plan.package_object,
        installed_artifact: &plan.installed_artifact,
        targets: &plan.targets,
        observations: &plan.observations,
        operations: &plan.operations,
    };
    let bytes = wire::encode(&binding).map_err(|error| PlanError::Encode(error.to_string()))?;
    Ok(Digest::compute(Domain::DistReleasePlan, &bytes))
}

fn validate_observations(
    requested_version: &ProductVersion,
    source_commit: &str,
    release_commit: &str,
    package_object: &str,
    observed: &ReleaseObservations,
) -> Result<(), PlanError> {
    if source_commit != release_commit {
        return Err(PlanError::CommitDisagreement {
            source_commit: source_commit.to_owned(),
            release_commit: release_commit.to_owned(),
        });
    }
    if !observed.clean {
        return Err(PlanError::Dirty);
    }
    if observed.unrecorded_local_commits != 0 {
        return Err(PlanError::UnrecordedLocalCommits(
            observed.unrecorded_local_commits,
        ));
    }
    if &observed.source_version != requested_version
        || &observed.release_version != requested_version
    {
        return Err(PlanError::VersionDisagreement {
            requested: requested_version.clone(),
            observed: observed.release_version.clone(),
        });
    }
    if observed.head != release_commit {
        return Err(PlanError::NonHeadPublication {
            head: observed.head.clone(),
            release: release_commit.to_owned(),
        });
    }
    if !observed.credentials_valid {
        return Err(PlanError::Credentials);
    }
    refuse_moved_ref("package", observed.package_ref.as_deref(), package_object)?;
    refuse_moved_ref("tag", observed.tag_ref.as_deref(), release_commit)?;
    refuse_moved_ref(
        "announcement",
        observed.announcement_ref.as_deref(),
        release_commit,
    )?;
    Ok(())
}

fn refuse_moved_ref(
    name: &'static str,
    observed: Option<&str>,
    release: &str,
) -> Result<(), PlanError> {
    match observed {
        Some(object) if object != release => Err(PlanError::RefMoved {
            name,
            expected: release.to_owned(),
            observed: object.to_owned(),
        }),
        _ => Ok(()),
    }
}

fn validate_targets(targets: &[PublishedTarget]) -> Result<(), PlanError> {
    const EXPECTED: [ReleaseTarget; 3] = [
        ReleaseTarget::Aarch64AppleDarwin,
        ReleaseTarget::X86_64UnknownLinuxMusl,
        ReleaseTarget::X86_64PcWindowsMsvc,
    ];
    if targets.len() != EXPECTED.len() {
        return Err(PlanError::TargetCount(targets.len()));
    }
    for (index, (target, expected)) in targets.iter().zip(EXPECTED).enumerate() {
        if target.target != expected.as_str() {
            return Err(PlanError::TargetOrder {
                index,
                expected,
                observed: target.target.clone(),
            });
        }
    }
    Ok(())
}

fn validate_object(label: &'static str, object: &str) -> Result<(), PlanError> {
    let valid = matches!(object.len(), 40 | 64)
        && object
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if valid {
        Ok(())
    } else {
        Err(PlanError::Object { label })
    }
}

/// Why consequence-free release planning refused.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("release plan encoding failed: {0}")]
    Encode(String),
    #[error("{label} is not one canonical Git object")]
    Object { label: &'static str },
    #[error("the release worktree is dirty")]
    Dirty,
    #[error(
        "release source commit {source_commit} disagrees with versioned commit {release_commit}"
    )]
    CommitDisagreement {
        source_commit: String,
        release_commit: String,
    },
    #[error("{0} local commits have not been recorded on the publication baseline")]
    UnrecordedLocalCommits(u64),
    #[error("expected version {requested} disagrees with observed release version {observed}")]
    VersionDisagreement {
        requested: ProductVersion,
        observed: ProductVersion,
    },
    #[error("publication baseline {release} is not HEAD {head}")]
    NonHeadPublication { head: String, release: String },
    #[error("release publication credentials did not validate")]
    Credentials,
    #[error("the {name} ref names {observed}, not {expected}")]
    RefMoved {
        name: &'static str,
        expected: String,
        observed: String,
    },
    #[error("the release has {0} sealed targets instead of three")]
    TargetCount(usize),
    #[error("sealed target {index} is {observed}, expected {expected}")]
    TargetOrder {
        index: usize,
        expected: ReleaseTarget,
        observed: String,
    },
    #[error("release plan operation inventory is not the canonical graph")]
    OperationList,
    #[error("release note bytes disagree with their identity")]
    NotesIdentity,
    #[error("release plan key moved from {expected} to {observed}")]
    KeyMoved { expected: Digest, observed: Digest },
}

/// Complete state observed immediately before one operation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum OperationState {
    Binding {
        identity: Digest,
    },
    Gate {
        receipt: Option<Digest>,
    },
    Artifact {
        identity: Option<Digest>,
    },
    Package {
        identity: Option<Digest>,
        object: Option<String>,
    },
    RemoteRef {
        reference: String,
        object: Option<String>,
    },
    Registry {
        evidence: Option<RegistryEvidence>,
    },
    Announcement {
        object: Option<String>,
    },
}

/// Durable intent written before crossing one operation boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedOperation {
    pub plan: Digest,
    pub operation: Operation,
    pub prior: OperationState,
}

/// Whether an operation caused its effect or found it already complete.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Convergence {
    Applied,
    AlreadyConverged,
}

/// Durable typed evidence written after one operation converges.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperationReceipt {
    pub plan: Digest,
    pub operation: Operation,
    pub prior: OperationState,
    pub result: OperationState,
    pub convergence: Convergence,
}

/// Journal boundary used by the sole release executor.
pub trait ReleaseJournal {
    type Error: fmt::Display;

    fn accepted(
        &mut self,
        plan: &Digest,
        operation: Operation,
    ) -> Result<Option<AcceptedOperation>, Self::Error>;
    fn receipt(
        &mut self,
        plan: &Digest,
        operation: Operation,
    ) -> Result<Option<OperationReceipt>, Self::Error>;
    fn accept(&mut self, accepted: &AcceptedOperation) -> Result<(), Self::Error>;
    fn complete(&mut self, receipt: &OperationReceipt) -> Result<(), Self::Error>;
}

/// One typed effect port. Implementations must make `converge` idempotent.
pub trait EffectPort {
    /// Re-observe all plan-bound values without mutation before any effect.
    fn preflight(&mut self, plan: &ReleasePlan) -> Result<ReleaseObservations, EffectError>;
    /// Observe the complete prior state immediately before an operation.
    fn prior(
        &mut self,
        plan: &ReleasePlan,
        operation: Operation,
    ) -> Result<OperationState, EffectError>;
    /// Converge one operation from its durably recorded prior state.
    fn converge(
        &mut self,
        plan: &ReleasePlan,
        operation: Operation,
        prior: &OperationState,
    ) -> Result<(OperationState, Convergence), EffectError>;
}

/// Typed failure categories preserved by every effect implementation.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum EffectError {
    #[error("a plan-bound observation moved: {0}")]
    Moved(String),
    #[error("operator interrupted release execution")]
    Interrupted,
    #[error("correctness gate refused: {0}")]
    GateRefused(String),
    #[error("remote release operation refused: {0}")]
    RemoteRefused(String),
    #[error("release environment failed: {0}")]
    Environment(String),
}

/// Progress returned after the sole ordered execution path converges.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Execution {
    pub completed: Vec<OperationReceipt>,
}

/// Stateless vendor for one journaled, resumable release transaction.
pub struct ReleaseExecutor;

impl ReleaseExecutor {
    /// Verify a complete ordered receipt chain without invoking an effect port.
    ///
    /// # Errors
    ///
    /// Refuses an invalid plan, missing, reordered, duplicated, or moved
    /// receipt, and any receipt result that does not prove its exact operation.
    pub fn verify_completion(
        plan: &ReleasePlan,
        receipts: &[OperationReceipt],
    ) -> Result<(), ExecuteError> {
        verify_plan(plan).map_err(ExecuteError::Plan)?;
        if receipts.len() != plan.operations.len() {
            return Err(ExecuteError::ReceiptCount {
                expected: plan.operations.len(),
                observed: receipts.len(),
            });
        }
        for (operation, receipt) in plan.operations.iter().zip(receipts) {
            if receipt.operation != *operation {
                return Err(ExecuteError::ReceiptMoved(receipt.operation));
            }
            validate_receipt(plan, receipt)?;
        }
        Ok(())
    }

    /// Execute or resume exactly the plan whose digest was applied.
    ///
    /// Admission proves the converged prefix before the first effect: an
    /// operation is met by its durable receipt or by authoritative external
    /// state, and met operations must precede every unmet one. A witnessed
    /// operation is receipted without crossing its effect again, so a visible
    /// package never reruns the gate or republishes, and an observed tag or
    /// announcement is never repeated.
    ///
    /// # Errors
    ///
    /// Refuses key or observation drift and contradictory or non-prefix
    /// external state before any operation, preserves effect outcome
    /// categories, and faults on journal durability failures.
    pub fn execute<J: ReleaseJournal, E: EffectPort>(
        plan: &ReleasePlan,
        applied: &Digest,
        journal: &mut J,
        effects: &mut E,
    ) -> Result<Execution, ExecuteError> {
        verify_plan(plan).map_err(ExecuteError::Plan)?;
        if applied != &plan.key {
            return Err(ExecuteError::AppliedKeyMoved {
                expected: plan.key.clone(),
                observed: applied.clone(),
            });
        }
        let observed = effects.preflight(plan).map_err(ExecuteError::Effect)?;
        if observed != plan.observations {
            return Err(ExecuteError::ObservationsMoved);
        }
        let witnesses = Witnesses::observe(plan, effects)?;
        admit_prefix(plan, journal, &witnesses)?;
        let mut completed = Vec::with_capacity(plan.operations.len());
        for operation in &plan.operations {
            if let Some(receipt) = journal
                .receipt(&plan.key, *operation)
                .map_err(journal_error)?
            {
                completed.push(receipt);
                continue;
            }
            let witness = witnesses.proof(*operation);
            let accepted = match journal
                .accepted(&plan.key, *operation)
                .map_err(journal_error)?
            {
                Some(accepted) => accepted,
                None => {
                    let prior = match witness {
                        Some(state) => state.clone(),
                        None => effects
                            .prior(plan, *operation)
                            .map_err(ExecuteError::Effect)?,
                    };
                    let accepted = AcceptedOperation {
                        plan: plan.key.clone(),
                        operation: *operation,
                        prior,
                    };
                    journal.accept(&accepted).map_err(journal_error)?;
                    accepted
                }
            };
            let (result, convergence) = match witness {
                Some(state) => (state.clone(), Convergence::AlreadyConverged),
                None => effects
                    .converge(plan, *operation, &accepted.prior)
                    .map_err(ExecuteError::Effect)?,
            };
            let receipt = OperationReceipt {
                plan: plan.key.clone(),
                operation: *operation,
                prior: accepted.prior,
                result,
                convergence,
            };
            validate_receipt(plan, &receipt)?;
            journal.complete(&receipt).map_err(journal_error)?;
            completed.push(receipt);
        }
        Self::verify_completion(plan, &completed)?;
        Ok(Execution { completed })
    }
}

fn journal_error(error: impl fmt::Display) -> ExecuteError {
    ExecuteError::Journal(error.to_string())
}

/// Authoritative external state proving an operation already converged.
struct Witnesses {
    registry: Option<OperationState>,
    tag: Option<OperationState>,
    announcement: Option<OperationState>,
}

impl Witnesses {
    fn observe<E: EffectPort>(plan: &ReleasePlan, effects: &mut E) -> Result<Self, ExecuteError> {
        let registry = effects
            .prior(plan, Operation::AwaitRegistryVisibility)
            .map_err(ExecuteError::Effect)?;
        let registry = match &registry {
            OperationState::Registry { evidence: None } => None,
            OperationState::Registry {
                evidence: Some(evidence),
            } if *evidence == registry_evidence(plan) => Some(registry),
            _ => return Err(ExecuteError::RegistryEvidenceMoved),
        };
        Ok(Self {
            registry,
            tag: Self::release_object(plan, effects, Operation::CreateImmutableTag)?,
            announcement: Self::release_object(plan, effects, Operation::Announce)?,
        })
    }

    fn release_object<E: EffectPort>(
        plan: &ReleasePlan,
        effects: &mut E,
        operation: Operation,
    ) -> Result<Option<OperationState>, ExecuteError> {
        let state = effects
            .prior(plan, operation)
            .map_err(ExecuteError::Effect)?;
        let object = match &state {
            OperationState::RemoteRef { object, .. } | OperationState::Announcement { object } => {
                object.as_deref()
            }
            _ => return Err(ExecuteError::ExternalMoved(operation)),
        };
        match object {
            None => Ok(None),
            Some(object) if object == plan.release_commit => Ok(Some(state)),
            Some(_) => Err(ExecuteError::ExternalMoved(operation)),
        }
    }

    fn proof(&self, operation: Operation) -> Option<&OperationState> {
        match operation {
            Operation::CreateImmutableTag => self.tag.as_ref(),
            Operation::Announce => self.announcement.as_ref(),
            operation if PUBLICATION_PREFIX.contains(&operation) => self.registry.as_ref(),
            _ => None,
        }
    }

    /// Operations whose own effect is directly observable, so a receipt
    /// without that observation contradicts authority. Upload is excluded:
    /// registry visibility may lag its receipt.
    const fn observes(operation: Operation) -> bool {
        matches!(
            operation,
            Operation::AwaitRegistryVisibility
                | Operation::CreateImmutableTag
                | Operation::Announce
        )
    }
}

/// Refuse unless every stored receipt is exact for its operation and receipts
/// and external witnesses describe one converged prefix, all before the first
/// effect. Credential resolution re-binds every run, so it is witnessed only by
/// its own receipt and never orders external state.
fn admit_prefix<J: ReleaseJournal>(
    plan: &ReleasePlan,
    journal: &mut J,
    witnesses: &Witnesses,
) -> Result<(), ExecuteError> {
    let mut unmet = None;
    for operation in &plan.operations {
        let receipt = journal
            .receipt(&plan.key, *operation)
            .map_err(journal_error)?;
        if let Some(receipt) = &receipt {
            if receipt.operation != *operation {
                return Err(ExecuteError::ReceiptMoved(receipt.operation));
            }
            validate_receipt(plan, receipt)?;
        }
        if *operation == Operation::ResolvePreconditionsAndCredentials {
            continue;
        }
        let witnessed = witnesses.proof(*operation).is_some();
        let receipted = receipt.is_some();
        if receipted && !witnessed && Witnesses::observes(*operation) {
            return Err(ExecuteError::Contradicted(*operation));
        }
        let met = witnessed || receipted;
        match (met, unmet) {
            (false, None) => unmet = Some(*operation),
            (true, Some(unmet)) => {
                return Err(ExecuteError::NotPrefix {
                    converged: *operation,
                    unmet,
                });
            }
            _ => {}
        }
    }
    Ok(())
}

fn registry_evidence(plan: &ReleasePlan) -> RegistryEvidence {
    RegistryEvidence {
        version: plan.requested_version.clone(),
        release_commit: plan.release_commit.clone(),
        package: plan.package.clone(),
    }
}

fn validate_receipt(plan: &ReleasePlan, receipt: &OperationReceipt) -> Result<(), ExecuteError> {
    if receipt.plan != plan.key || !plan.operations.contains(&receipt.operation) {
        return Err(ExecuteError::ReceiptMoved(receipt.operation));
    }
    use Operation as Op;
    let valid = match (&receipt.operation, &receipt.result) {
        (Op::ResolvePreconditionsAndCredentials, OperationState::Binding { identity }) => {
            identity == &plan.repository
        }
        (
            Op::RunCorrectnessGate,
            OperationState::Gate {
                receipt: Some(receipt),
            },
        ) => receipt == &gate_receipt(plan),
        (
            Op::RehearseUpload,
            OperationState::Package {
                identity: Some(identity),
                object: Some(object),
            },
        ) => identity == &plan.package && object == &plan.package_object,
        (
            Op::InstallAndProbeArtifacts,
            OperationState::Artifact {
                identity: Some(identity),
            },
        ) => identity == &plan.installed_artifact,
        (
            Op::CreateImmutableTag,
            OperationState::RemoteRef {
                object: Some(object),
                ..
            },
        ) => object == &plan.release_commit,
        (
            Op::UploadPackage,
            OperationState::RemoteRef {
                object: Some(object),
                ..
            },
        ) => object == &plan.package_object,
        (
            Op::AwaitRegistryVisibility,
            OperationState::Registry {
                evidence: Some(evidence),
            },
        ) => *evidence == registry_evidence(plan),
        (
            operation,
            OperationState::Registry {
                evidence: Some(evidence),
            },
        ) if PUBLICATION_PREFIX.contains(operation) => {
            receipt.convergence == Convergence::AlreadyConverged
                && *evidence == registry_evidence(plan)
        }
        (
            Op::Announce,
            OperationState::Announcement {
                object: Some(object),
            },
        ) => object == &plan.release_commit,
        _ => false,
    };
    if valid {
        Ok(())
    } else if receipt.operation == Op::InstallAndProbeArtifacts {
        Err(ExecuteError::InstalledArtifactMoved)
    } else if receipt.operation == Op::AwaitRegistryVisibility {
        Err(ExecuteError::RegistryEvidenceMoved)
    } else {
        Err(ExecuteError::ReceiptMoved(receipt.operation))
    }
}

/// Why a previewed transaction did not converge.
#[derive(Debug, thiserror::Error)]
pub enum ExecuteError {
    #[error(transparent)]
    Plan(PlanError),
    #[error("applied key {observed} differs from previewed key {expected}")]
    AppliedKeyMoved { expected: Digest, observed: Digest },
    #[error("a plan-bound observation moved before the first operation")]
    ObservationsMoved,
    #[error(transparent)]
    Effect(EffectError),
    #[error("release journal failed: {0}")]
    Journal(String),
    #[error("receipt for {0:?} does not belong to this plan")]
    ReceiptMoved(Operation),
    #[error("release has {observed} receipts, expected {expected}")]
    ReceiptCount { expected: usize, observed: usize },
    #[error("the independently installed artifact differs from the sealed artifact")]
    InstalledArtifactMoved,
    #[error("registry visibility does not name the exact package and release commit")]
    RegistryEvidenceMoved,
    #[error("authoritative {0} state names another release")]
    ExternalMoved(Operation),
    #[error("{converged} is already converged while earlier {unmet} is not")]
    NotPrefix {
        converged: Operation,
        unmet: Operation,
    },
    #[error("{0} has a durable receipt but its authoritative state is absent")]
    Contradicted(Operation),
}

#[cfg(test)]
mod tests;
