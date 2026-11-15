use std::collections::BTreeMap;
use std::convert::Infallible;

use super::*;

const SOURCE: &str = "1111111111111111111111111111111111111111";
const RELEASE: &str = "2222222222222222222222222222222222222222";
const PACKAGE_OBJECT: &str = "3333333333333333333333333333333333333333";

fn digest(domain: Domain, value: &[u8]) -> Digest {
    Digest::compute(domain, value)
}

fn input() -> PlanInput {
    let version = ProductVersion::parse("1.2.3").expect("fixture version");
    let package = digest(Domain::DistReleasePackage, b"package");
    PlanInput {
        repository: digest(Domain::DistSourceCommit, b"repository"),
        requested_version: version.clone(),
        source_commit: SOURCE.to_owned(),
        source_archive: digest(Domain::DistSourceArchive, b"archive"),
        release_commit: SOURCE.to_owned(),
        notes_document: "## Highlights\n\n- Exact curated prose.\n".to_owned(),
        manifest: digest(Domain::DistReleaseManifest, b"manifest"),
        package,
        package_object: PACKAGE_OBJECT.to_owned(),
        installed_artifact: digest(Domain::DistArtifact, b"native"),
        targets: vec![
            PublishedTarget {
                target: "aarch64-apple-darwin".to_owned(),
                artifact: digest(Domain::DistArtifact, b"mac native"),
                receipt: digest(Domain::ProofReceipt, b"mac proof"),
            },
            PublishedTarget {
                target: "x86_64-unknown-linux-musl".to_owned(),
                artifact: digest(Domain::DistArtifact, b"linux native"),
                receipt: digest(Domain::ProofReceipt, b"linux proof"),
            },
            PublishedTarget {
                target: "x86_64-pc-windows-msvc".to_owned(),
                artifact: digest(Domain::DistArtifact, b"windows native"),
                receipt: digest(Domain::ProofReceipt, b"windows proof"),
            },
        ],
        observations: ReleaseObservations {
            head: SOURCE.to_owned(),
            clean: true,
            unrecorded_local_commits: 0,
            source_version: version.clone(),
            release_version: version,
            credentials_valid: true,
            package_ref: None,
            tag_ref: None,
            announcement_ref: None,
        },
    }
}

#[derive(Default)]
struct MemoryJournal {
    accepted: BTreeMap<Operation, AcceptedOperation>,
    receipts: BTreeMap<Operation, OperationReceipt>,
}

impl ReleaseJournal for MemoryJournal {
    type Error = Infallible;

    fn accepted(
        &mut self,
        _: &Digest,
        operation: Operation,
    ) -> Result<Option<AcceptedOperation>, Self::Error> {
        Ok(self.accepted.get(&operation).cloned())
    }

    fn receipt(
        &mut self,
        _: &Digest,
        operation: Operation,
    ) -> Result<Option<OperationReceipt>, Self::Error> {
        Ok(self.receipts.get(&operation).cloned())
    }

    fn accept(&mut self, accepted: &AcceptedOperation) -> Result<(), Self::Error> {
        self.accepted.insert(accepted.operation, accepted.clone());
        Ok(())
    }

    fn complete(&mut self, receipt: &OperationReceipt) -> Result<(), Self::Error> {
        self.receipts.insert(receipt.operation, receipt.clone());
        Ok(())
    }
}

struct FakeEffects {
    observations: ReleaseObservations,
    converged: BTreeMap<Operation, OperationState>,
    calls: Vec<Operation>,
    interrupt_before: Option<Operation>,
    interrupt_after: Option<Operation>,
}

impl FakeEffects {
    fn new(plan: &ReleasePlan) -> Self {
        Self {
            observations: plan.observations.clone(),
            converged: BTreeMap::new(),
            calls: Vec::new(),
            interrupt_before: None,
            interrupt_after: None,
        }
    }

    fn result(plan: &ReleasePlan, operation: Operation) -> OperationState {
        match operation {
            Operation::ResolvePreconditionsAndCredentials => OperationState::Binding {
                identity: plan.repository.clone(),
            },
            Operation::RunCorrectnessGate => OperationState::Gate {
                receipt: Some(gate_receipt(plan)),
            },
            Operation::RehearseUpload => OperationState::Package {
                identity: Some(plan.package.clone()),
                object: Some(plan.package_object.clone()),
            },
            Operation::InstallAndProbeArtifacts => OperationState::Artifact {
                identity: Some(plan.installed_artifact.clone()),
            },
            Operation::UploadPackage => OperationState::RemoteRef {
                reference: format!("refs/spice/packages/v{}", plan.requested_version),
                object: Some(plan.package_object.clone()),
            },
            Operation::AwaitRegistryVisibility => OperationState::Registry {
                evidence: Some(RegistryEvidence {
                    version: plan.requested_version.clone(),
                    release_commit: plan.release_commit.clone(),
                    package: plan.package.clone(),
                }),
            },
            Operation::CreateImmutableTag => OperationState::RemoteRef {
                reference: format!("refs/tags/v{}", plan.requested_version),
                object: Some(plan.release_commit.clone()),
            },
            Operation::PublishGithubReleaseAssets => OperationState::GithubRelease {
                tag: format!("v{}", plan.requested_version),
                title: Some(format!("v{}", plan.requested_version)),
                notes: Some(plan.notes.clone()),
                assets: Some(github_release_assets(plan)),
            },
            Operation::Announce => OperationState::Announcement {
                object: Some(plan.release_commit.clone()),
            },
        }
    }
}

impl EffectPort for FakeEffects {
    fn preflight(&mut self, _: &ReleasePlan) -> Result<ReleaseObservations, EffectError> {
        Ok(self.observations.clone())
    }

    fn prior(
        &mut self,
        plan: &ReleasePlan,
        operation: Operation,
    ) -> Result<OperationState, EffectError> {
        Ok(self
            .converged
            .get(&operation)
            .cloned()
            .unwrap_or_else(|| match operation {
                Operation::CreateImmutableTag => OperationState::RemoteRef {
                    reference: format!("refs/tags/v{}", plan.requested_version),
                    object: None,
                },
                Operation::AwaitRegistryVisibility => OperationState::Registry { evidence: None },
                Operation::PublishGithubReleaseAssets => OperationState::GithubRelease {
                    tag: format!("v{}", plan.requested_version),
                    title: None,
                    notes: None,
                    assets: None,
                },
                Operation::Announce => OperationState::Announcement { object: None },
                _ => OperationState::Artifact { identity: None },
            }))
    }

    fn converge(
        &mut self,
        plan: &ReleasePlan,
        operation: Operation,
        _: &OperationState,
    ) -> Result<(OperationState, Convergence), EffectError> {
        if self.interrupt_before == Some(operation) {
            self.interrupt_before = None;
            return Err(EffectError::Interrupted);
        }
        let expected = Self::result(plan, operation);
        let existed = self.converged.contains_key(&operation);
        let partial_provider = operation == Operation::PublishGithubReleaseAssets
            && self.converged.get(&operation) != Some(&expected);
        let result = if partial_provider {
            expected
        } else {
            self.converged.get(&operation).cloned().unwrap_or(expected)
        };
        if !existed || partial_provider {
            self.calls.push(operation);
            self.converged.insert(operation, result.clone());
        }
        if self.interrupt_after == Some(operation) {
            self.interrupt_after = None;
            return Err(EffectError::Interrupted);
        }
        Ok((
            result,
            if existed && !partial_provider {
                Convergence::AlreadyConverged
            } else {
                Convergence::Applied
            },
        ))
    }
}

#[test]
fn preview_is_pure_and_spells_the_complete_order() {
    let plan = plan_release(input()).expect("plan");
    assert_eq!(plan.operations, OPERATIONS);
    assert_eq!(
        plan.notes_document,
        "## Highlights\n\n- Exact curated prose.\n"
    );
    assert!(
        plan.operations
            .iter()
            .all(|operation| !operation.as_str().contains("source")),
        "publication must not observe or mutate a moving source branch"
    );
    assert_eq!(
        plan.operations
            .iter()
            .map(|operation| operation.as_str())
            .collect::<Vec<_>>(),
        [
            "resolve-preconditions-and-credentials",
            "run-correctness-gate",
            "install-and-probe-artifacts",
            "rehearse-upload",
            "upload-package",
            "await-registry-visibility",
            "create-immutable-tag",
            "publish-github-release-assets",
            "announce",
        ]
    );
    verify_plan(&plan).expect("identity replays");
}

#[test]
fn every_bound_value_moves_the_plan_digest() {
    let baseline = plan_release(input()).expect("baseline");
    let mut changed = input();
    changed.notes_document.push('!');
    assert_ne!(
        baseline.key,
        plan_release(changed).expect("notes variant").key
    );
    let moved = |plan: ReleasePlan| {
        let result = verify_plan(&plan);
        assert!(
            matches!(result, Err(PlanError::KeyMoved { .. })),
            "unexpected verification result: {result:?}"
        );
    };
    let mut plan = baseline.clone();
    plan.repository = digest(Domain::DistSourceCommit, b"other repository");
    moved(plan);
    let mut plan = baseline.clone();
    plan.requested_version = ProductVersion::parse("1.2.4").expect("moved version");
    plan.observations.source_version = plan.requested_version.clone();
    plan.observations.release_version = plan.requested_version.clone();
    moved(plan);
    let mut plan = baseline.clone();
    plan.source_commit = RELEASE.to_owned();
    plan.release_commit = RELEASE.to_owned();
    plan.observations.head = RELEASE.to_owned();
    moved(plan);
    let mut plan = baseline.clone();
    plan.source_archive = digest(Domain::DistSourceArchive, b"other source archive");
    moved(plan);
    let mut plan = baseline.clone();
    plan.manifest = digest(Domain::DistReleaseManifest, b"other manifest");
    moved(plan);
    let mut plan = baseline.clone();
    plan.package = digest(Domain::DistReleasePackage, b"other package");
    moved(plan);
    let mut plan = baseline.clone();
    plan.package_object = "4444444444444444444444444444444444444444".to_owned();
    moved(plan);
    let mut plan = baseline.clone();
    plan.installed_artifact = digest(Domain::DistArtifact, b"other installed artifact");
    moved(plan);
    let mut plan = baseline.clone();
    plan.targets[0].receipt = digest(Domain::ProofReceipt, b"other receipt");
    moved(plan);
    let mut plan = baseline.clone();
    plan.requested_version = ProductVersion::parse("1.2.1").expect("moved requested version");
    plan.observations.source_version = plan.requested_version.clone();
    plan.observations.release_version = plan.observations.source_version.clone();
    moved(plan);
    let mut plan = baseline;
    plan.operations.swap(0, 1);
    assert!(matches!(verify_plan(&plan), Err(PlanError::OperationList)));
}

#[test]
fn all_no_effect_preconditions_refuse_during_planning() {
    let mut dirty = input();
    dirty.observations.clean = false;
    assert!(matches!(plan_release(dirty), Err(PlanError::Dirty)));

    let mut local = input();
    local.observations.unrecorded_local_commits = 1;
    assert!(matches!(
        plan_release(local),
        Err(PlanError::UnrecordedLocalCommits(1))
    ));

    let mut non_head = input();
    non_head.observations.head = RELEASE.to_owned();
    assert!(matches!(
        plan_release(non_head),
        Err(PlanError::NonHeadPublication { .. })
    ));

    let mut version = input();
    version.observations.release_version =
        ProductVersion::parse("1.2.1").expect("different release version");
    assert!(matches!(
        plan_release(version),
        Err(PlanError::VersionDisagreement { .. })
    ));

    let mut compiled = input();
    compiled.observations.source_version =
        ProductVersion::parse("1.2.2").expect("different compiled version");
    assert!(matches!(
        plan_release(compiled),
        Err(PlanError::VersionDisagreement { .. })
    ));

    let mut split_commit = input();
    split_commit.release_commit = RELEASE.to_owned();
    assert!(matches!(
        plan_release(split_commit),
        Err(PlanError::CommitDisagreement { .. })
    ));

    let mut package = input();
    package.observations.package_ref = Some(RELEASE.to_owned());
    assert!(matches!(
        plan_release(package),
        Err(PlanError::RefMoved {
            name: "package",
            ..
        })
    ));

    let mut tag = input();
    tag.observations.tag_ref = Some(RELEASE.to_owned());
    assert!(matches!(plan_release(tag), Err(PlanError::RefMoved { .. })));

    let mut credentials = input();
    credentials.observations.credentials_valid = false;
    assert!(matches!(
        plan_release(credentials),
        Err(PlanError::Credentials)
    ));

    let mut object = input();
    object.package_object = "not-a-tree".to_owned();
    assert!(matches!(
        plan_release(object),
        Err(PlanError::Object {
            label: "package object"
        })
    ));

    let mut short_object = input();
    short_object.package_object = "111111".to_owned();
    assert!(matches!(
        plan_release(short_object),
        Err(PlanError::Object {
            label: "package object"
        })
    ));

    let mut converged = input();
    converged.observations.package_ref = Some(PACKAGE_OBJECT.to_owned());
    converged.observations.tag_ref = Some(SOURCE.to_owned());
    converged.observations.announcement_ref = Some(SOURCE.to_owned());
    plan_release(converged).expect("refs already naming the release converge");
}

#[test]
fn moved_digest_and_observations_refuse_before_any_operation() {
    let plan = plan_release(input()).expect("plan");
    let mut journal = MemoryJournal::default();
    let mut effects = FakeEffects::new(&plan);
    let moved = digest(Domain::DistReleasePlan, b"moved");
    assert!(matches!(
        ReleaseExecutor::execute(&plan, &moved, &mut journal, &mut effects),
        Err(ExecuteError::AppliedKeyMoved { .. })
    ));
    assert!(effects.calls.is_empty());
    assert!(journal.accepted.is_empty());

    effects.observations.tag_ref = Some(RELEASE.to_owned());
    assert!(matches!(
        ReleaseExecutor::execute(&plan, &plan.key, &mut journal, &mut effects),
        Err(ExecuteError::ObservationsMoved)
    ));
    assert!(effects.calls.is_empty());
    assert!(journal.accepted.is_empty());
}

#[test]
fn one_executor_orders_remote_publication_after_local_proof() {
    let plan = plan_release(input()).expect("plan");
    let mut journal = MemoryJournal::default();
    let mut effects = FakeEffects::new(&plan);
    let execution = ReleaseExecutor::execute(&plan, &plan.key, &mut journal, &mut effects)
        .expect("execution converges");
    assert_eq!(effects.calls, plan.operations);
    assert_eq!(execution.completed.len(), plan.operations.len());
    ReleaseExecutor::verify_completion(&plan, &execution.completed)
        .expect("complete receipts independently verify");
    assert!(matches!(
        ReleaseExecutor::verify_completion(
            &plan,
            &execution.completed[..execution.completed.len() - 1]
        ),
        Err(ExecuteError::ReceiptCount { .. })
    ));
    let position = |operation| {
        effects
            .calls
            .iter()
            .position(|candidate| *candidate == operation)
            .expect("operation exists")
    };
    assert!(position(Operation::UploadPackage) < position(Operation::AwaitRegistryVisibility));
    assert!(position(Operation::AwaitRegistryVisibility) < position(Operation::CreateImmutableTag));
    assert!(position(Operation::CreateImmutableTag) < position(Operation::Announce));
}

#[test]
fn interruption_at_every_boundary_resumes_without_duplicate_effects() {
    let plan = plan_release(input()).expect("plan");
    for interrupted in &plan.operations {
        let mut journal = MemoryJournal::default();
        let mut effects = FakeEffects::new(&plan);
        effects.interrupt_before = Some(*interrupted);
        assert!(matches!(
            ReleaseExecutor::execute(&plan, &plan.key, &mut journal, &mut effects),
            Err(ExecuteError::Effect(EffectError::Interrupted))
        ));
        ReleaseExecutor::execute(&plan, &plan.key, &mut journal, &mut effects)
            .expect("interrupted execution resumes");
        let first_calls = effects.calls.clone();
        ReleaseExecutor::execute(&plan, &plan.key, &mut journal, &mut effects)
            .expect("completed execution replays from receipts");
        assert_eq!(effects.calls, first_calls);
        assert_eq!(effects.calls, plan.operations);
    }
    for interrupted in &plan.operations {
        let mut journal = MemoryJournal::default();
        let mut effects = FakeEffects::new(&plan);
        effects.interrupt_after = Some(*interrupted);
        assert!(matches!(
            ReleaseExecutor::execute(&plan, &plan.key, &mut journal, &mut effects),
            Err(ExecuteError::Effect(EffectError::Interrupted))
        ));
        assert!(journal.accepted.contains_key(interrupted));
        assert!(!journal.receipts.contains_key(interrupted));
        ReleaseExecutor::execute(&plan, &plan.key, &mut journal, &mut effects)
            .expect("post-effect interruption converges from accepted prior state");
        assert_eq!(effects.calls, plan.operations);
    }
}

#[test]
fn installed_and_registry_receipts_are_exact_not_recognition_only() {
    let plan = plan_release(input()).expect("plan");
    let mut journal = MemoryJournal::default();
    let mut effects = FakeEffects::new(&plan);
    effects.converged.insert(
        Operation::InstallAndProbeArtifacts,
        OperationState::Artifact {
            identity: Some(digest(Domain::DistArtifact, b"another binary")),
        },
    );
    let error = ReleaseExecutor::execute(&plan, &plan.key, &mut journal, &mut effects)
        .expect_err("wrong installed artifact refuses");
    assert!(matches!(error, ExecuteError::InstalledArtifactMoved));
    assert!(!journal.receipts.contains_key(&Operation::Announce));
}

#[test]
fn completed_receipts_remain_bound_to_the_exact_plan_and_result() {
    let plan = plan_release(input()).expect("plan");
    let mut journal = MemoryJournal::default();
    let mut effects = FakeEffects::new(&plan);
    let execution = ReleaseExecutor::execute(&plan, &plan.key, &mut journal, &mut effects)
        .expect("fixture execution converges");

    let mut moved_plan = execution.completed.clone();
    moved_plan[0].plan = digest(Domain::DistReleasePlan, b"another plan");
    assert!(matches!(
        ReleaseExecutor::verify_completion(&plan, &moved_plan),
        Err(ExecuteError::ReceiptMoved(
            Operation::ResolvePreconditionsAndCredentials
        ))
    ));

    let package_index = plan
        .operations
        .iter()
        .position(|operation| *operation == Operation::RehearseUpload)
        .expect("package operation");
    let mut moved_package = execution.completed.clone();
    moved_package[package_index].result = OperationState::Package {
        identity: Some(digest(Domain::DistReleasePackage, b"another package")),
        object: Some(plan.package_object.clone()),
    };
    assert!(matches!(
        ReleaseExecutor::verify_completion(&plan, &moved_package),
        Err(ExecuteError::ReceiptMoved(Operation::RehearseUpload))
    ));

    let registry_index = plan
        .operations
        .iter()
        .position(|operation| *operation == Operation::AwaitRegistryVisibility)
        .expect("registry operation");
    let mut moved_registry = execution.completed;
    moved_registry[registry_index].result = OperationState::Registry { evidence: None };
    assert!(matches!(
        ReleaseExecutor::verify_completion(&plan, &moved_registry),
        Err(ExecuteError::RegistryEvidenceMoved)
    ));
}

fn observed(plan: &ReleasePlan, operations: &[Operation]) -> FakeEffects {
    let mut effects = FakeEffects::new(plan);
    for operation in operations {
        effects
            .converged
            .insert(*operation, FakeEffects::result(plan, *operation));
    }
    effects
}

#[test]
fn a_visible_package_supersedes_the_publication_prefix() {
    let plan = plan_release(input()).expect("plan");
    let mut journal = MemoryJournal::default();
    let mut effects = observed(&plan, &[Operation::AwaitRegistryVisibility]);
    let execution = ReleaseExecutor::execute(&plan, &plan.key, &mut journal, &mut effects)
        .expect("a visible package converges");
    assert_eq!(
        effects.calls,
        [
            Operation::ResolvePreconditionsAndCredentials,
            Operation::CreateImmutableTag,
            Operation::PublishGithubReleaseAssets,
            Operation::Announce,
        ],
        "visibility must not rerun the gate or republish"
    );
    let visible = FakeEffects::result(&plan, Operation::AwaitRegistryVisibility);
    for receipt in &execution.completed {
        if PUBLICATION_PREFIX.contains(&receipt.operation) {
            assert_eq!(receipt.result, visible);
            assert_eq!(receipt.convergence, Convergence::AlreadyConverged);
        }
    }
    ReleaseExecutor::verify_completion(&plan, &execution.completed)
        .expect("superseded receipts verify");

    let mut applied = execution.completed;
    applied[1].convergence = Convergence::Applied;
    assert!(matches!(
        ReleaseExecutor::verify_completion(&plan, &applied),
        Err(ExecuteError::ReceiptMoved(Operation::RunCorrectnessGate))
    ));
}

#[test]
fn an_observed_tag_and_announcement_are_never_repeated() {
    let plan = plan_release(input()).expect("plan");
    let mut journal = MemoryJournal::default();
    let mut effects = observed(
        &plan,
        &[
            Operation::AwaitRegistryVisibility,
            Operation::CreateImmutableTag,
            Operation::PublishGithubReleaseAssets,
            Operation::Announce,
        ],
    );
    ReleaseExecutor::execute(&plan, &plan.key, &mut journal, &mut effects)
        .expect("a converged release resumes");
    assert_eq!(
        effects.calls,
        [Operation::ResolvePreconditionsAndCredentials]
    );
}

#[test]
fn a_partial_provider_release_resumes_missing_assets_before_announcement() {
    let plan = plan_release(input()).expect("plan");
    let mut effects = observed(
        &plan,
        &[
            Operation::AwaitRegistryVisibility,
            Operation::CreateImmutableTag,
        ],
    );
    effects.converged.insert(
        Operation::PublishGithubReleaseAssets,
        OperationState::GithubRelease {
            tag: format!("v{}", plan.requested_version),
            title: Some(format!("v{}", plan.requested_version)),
            notes: Some(plan.notes.clone()),
            assets: Some(vec![github_release_assets(&plan)[0].clone()]),
        },
    );
    ReleaseExecutor::execute(
        &plan,
        &plan.key,
        &mut MemoryJournal::default(),
        &mut effects,
    )
    .expect("partial provider release resumes");
    assert_eq!(
        effects.calls,
        [
            Operation::ResolvePreconditionsAndCredentials,
            Operation::PublishGithubReleaseAssets,
            Operation::Announce,
        ]
    );
}

#[test]
fn provider_asset_names_and_identities_derive_from_the_sealed_targets() {
    let plan = plan_release(input()).expect("plan");
    let assets = github_release_assets(&plan);
    assert_eq!(
        assets
            .iter()
            .map(|asset| asset.name.as_str())
            .collect::<Vec<_>>(),
        [
            "spice-aarch64-apple-darwin",
            "spice-x86_64-unknown-linux-musl",
            "spice-x86_64-pc-windows-msvc.exe",
        ]
    );
    assert_eq!(
        assets
            .iter()
            .map(|asset| &asset.identity)
            .collect::<Vec<_>>(),
        plan.targets
            .iter()
            .map(|target| &target.artifact)
            .collect::<Vec<_>>()
    );
}

#[test]
fn contradictory_or_non_prefix_state_refuses_before_effects() {
    let plan = plan_release(input()).expect("plan");
    let refuses = |mut effects: FakeEffects, journal: &mut MemoryJournal| {
        let accepted = journal.accepted.len();
        let error = ReleaseExecutor::execute(&plan, &plan.key, journal, &mut effects)
            .expect_err("admission refuses");
        assert!(effects.calls.is_empty(), "{error} crossed an effect");
        assert_eq!(journal.accepted.len(), accepted, "{error} journaled");
        error
    };

    let tag_first = refuses(
        observed(&plan, &[Operation::CreateImmutableTag]),
        &mut MemoryJournal::default(),
    );
    assert!(matches!(
        tag_first,
        ExecuteError::NotPrefix {
            converged: Operation::CreateImmutableTag,
            unmet: Operation::RunCorrectnessGate,
        }
    ));

    let untagged_announcement = refuses(
        observed(
            &plan,
            &[Operation::AwaitRegistryVisibility, Operation::Announce],
        ),
        &mut MemoryJournal::default(),
    );
    assert!(matches!(
        untagged_announcement,
        ExecuteError::NotPrefix {
            converged: Operation::Announce,
            unmet: Operation::CreateImmutableTag,
        }
    ));

    let mut other_package = FakeEffects::new(&plan);
    other_package.converged.insert(
        Operation::AwaitRegistryVisibility,
        OperationState::Registry {
            evidence: Some(RegistryEvidence {
                version: plan.requested_version.clone(),
                release_commit: plan.release_commit.clone(),
                package: digest(Domain::DistReleasePackage, b"another package"),
            }),
        },
    );
    assert!(matches!(
        refuses(other_package, &mut MemoryJournal::default()),
        ExecuteError::RegistryEvidenceMoved
    ));

    let mut moved_tag = FakeEffects::new(&plan);
    moved_tag.converged.insert(
        Operation::CreateImmutableTag,
        OperationState::RemoteRef {
            reference: format!("refs/tags/v{}", plan.requested_version),
            object: Some(RELEASE.to_owned()),
        },
    );
    assert!(matches!(
        refuses(moved_tag, &mut MemoryJournal::default()),
        ExecuteError::ExternalMoved(Operation::CreateImmutableTag)
    ));

    let mut journal = MemoryJournal::default();
    let mut effects = FakeEffects::new(&plan);
    ReleaseExecutor::execute(&plan, &plan.key, &mut journal, &mut effects)
        .expect("fixture release converges");
    let deleted_tag = observed(&plan, &[Operation::AwaitRegistryVisibility]);
    assert!(matches!(
        refuses(deleted_tag, &mut journal),
        ExecuteError::Contradicted(Operation::CreateImmutableTag)
    ));
}

#[test]
fn every_stored_receipt_is_validated_before_the_first_effect() {
    let plan = plan_release(input()).expect("plan");
    let forged = OperationReceipt {
        plan: plan.key.clone(),
        operation: Operation::RunCorrectnessGate,
        prior: OperationState::Gate { receipt: None },
        result: OperationState::Gate {
            receipt: Some(digest(Domain::GateReceipt, b"another gate")),
        },
        convergence: Convergence::Applied,
    };
    let mut journal = MemoryJournal::default();
    journal.receipts.insert(forged.operation, forged);
    let mut effects = observed(&plan, &[Operation::AwaitRegistryVisibility]);
    let error = ReleaseExecutor::execute(&plan, &plan.key, &mut journal, &mut effects)
        .expect_err("a forged later receipt refuses");
    assert!(matches!(
        error,
        ExecuteError::ReceiptMoved(Operation::RunCorrectnessGate)
    ));
    assert!(
        effects.calls.is_empty(),
        "an earlier operation crossed its effect"
    );
    assert!(journal.accepted.is_empty());

    let mut journal = MemoryJournal::default();
    let misfiled = OperationReceipt {
        plan: plan.key.clone(),
        operation: Operation::ResolvePreconditionsAndCredentials,
        prior: OperationState::Binding {
            identity: plan.repository.clone(),
        },
        result: OperationState::Binding {
            identity: plan.repository.clone(),
        },
        convergence: Convergence::Applied,
    };
    journal
        .receipts
        .insert(Operation::RunCorrectnessGate, misfiled);
    let mut effects = FakeEffects::new(&plan);
    assert!(matches!(
        ReleaseExecutor::execute(&plan, &plan.key, &mut journal, &mut effects),
        Err(ExecuteError::ReceiptMoved(_))
    ));
    assert!(effects.calls.is_empty());
}
