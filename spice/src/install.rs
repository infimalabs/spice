//! Digest-bound, repository-local Spice installation.
//!
//! Plans are side-effect free; apply re-plans and reverses only exact images.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use wire::{Digest, Domain};

use crate::repository::Repository;

mod audit;
mod binding;
mod canvas;
mod error;
mod files;
mod fold;
mod git_hooks;
mod handle;
mod maintenance;
mod model;
mod path;
mod progress;
mod record;
mod ref_storage;
mod reversal;
mod symbol_sample;
mod symbol_verification;
mod symbols;
mod validation;

pub use audit::{ApplianceAudit, ApplianceState, audit_appliance};
use binding::{
    BoundDestination, bind_destination, validate_operation_binding, validate_record_binding,
};
pub use binding::{Generation, ParentObservation};
pub use error::InstallError;
use files::{
    cleanup_operation, matches_generated, materialize_generated, observe, observe_bound,
    prepare_retirement, publish_operation, restore_operation, restore_retirement, retire_path,
    withdraw_parent,
};
use path::validate_relative;
pub use progress::PreparedInstall;
use progress::{
    apply_expected, apply_prepared_record, retire_uninstalled_record, unapply_record,
    verify_progress,
};
use record::{read_record, read_record_for_refresh, read_record_for_transition};
use validation::{asset_contract, fixed_content_differs_from_current_build};

pub use fold::{
    InstallOperationSummary, InstallPlanSummary, InstallPreview, InstallReceipt,
    InstallRetirementSummary, InstallStageKind, InstallStatus,
};
pub use maintenance::{
    InstallTempInspection, inspect_temporary_records, reclaim_temporary_records,
};
pub use model::{
    Asset, AssetTag, FileImage, GeneratedImage, Location, Operation, Orphan, Retirement,
};
use model::{ObservedFile, ReceiptGeneration, RepositoryIdentity, images_match};
pub use symbol_sample::{
    DwarfLookup, DwarfdumpLookup, LookupError, MAX_SAMPLE_ADDRESSES, MAX_SAMPLE_BYTES, SampleError,
    SampleSymbolization, SampleTarget, SampledFrame, SourceLocation, SymbolizedFrame,
    symbolize_sample,
};
pub use symbol_verification::{
    SymbolImage, SymbolVerification, SymbolVerificationError, verify_symbols,
};
pub use symbols::SymbolProvenance;

// Both versions are selected at compile time from `dist/version/counters`, the
// one place the receipt schema is bumped; the product version's minor sums
// every record in that registry. Registered retained schemas cross forward
// during refresh. A receipt numbered above this build refuses as
// `InstallError::ForeignRecord`; unapply it with the appliance that owns it
// before installing this one.
const VERSION_COUNTERS: &str = ::std::include_str!("../../dist/version/counters");
const PLAN_VERSION: u16 = wire::counter::registry_counter_u16(VERSION_COUNTERS, "install");
const RECORD_VERSION: u16 = wire::counter::registry_counter_u16(VERSION_COUNTERS, "install");

/// The receipt, relative to the state root it lives in.
pub const RECORD_PATH: &str = "appliance/install/receipt.json";
/// Reversal payloads, relative to the state root.
const REVERSAL_PATH: &str = "appliance/install/reversals";
const RECORD_MODE: u32 = 0o600;
#[cfg(unix)]
const DIRECTORY_MODE: u32 = 0o700;
// Windows grants files to their owner alone: its protected owner-only DACL
// has no group or other principals to carry Unix's read-only grants.
#[cfg(not(windows))]
const DATA_MODE: u32 = 0o644;
#[cfg(windows)]
const DATA_MODE: u32 = 0o600;
#[cfg(not(windows))]
const EXECUTABLE_MODE: u32 = 0o755;
#[cfg(windows)]
const EXECUTABLE_MODE: u32 = 0o700;

// Appliance assets use a sibling namespace. The receipt binds from the
// existing Git-common directory so a fresh preview does not create its state
// root; apply creates every missing parent under the same admitted binding.
const GIT_PRE_COMMIT_PATH: &str = ".spice-install/githooks/pre-commit";
const GIT_COMMIT_MESSAGE_PATH: &str = ".spice-install/githooks/commit-msg";
const GIT_REFERENCE_TRANSACTION_PATH: &str = ".spice-install/githooks/reference-transaction";
const GIT_HOOK_CONFIG_PATH: &str = ".spice-install/githooks/config";
#[cfg(not(windows))]
const BINARY_PATH: &str = ".spice-install/bin/spice";
#[cfg(windows)]
const BINARY_PATH: &str = ".spice-install/bin/spice.exe";

const SKILL: &str = ::std::include_str!("../agent/SKILL.md");
/// Maximum bytes admitted for the authoritative skill.
pub const SKILL_BYTES_CEILING: usize = 8 * 1_024;
/// Maximum bytes admitted for the binding system-prompt bootstrap card.
pub const SKILL_BOOTSTRAP_BYTES_CEILING: usize = 1_024;
const _: () = assert!(
    SKILL.len() <= SKILL_BYTES_CEILING,
    "spice/agent/SKILL.md exceeds the 8 KiB authoritative resource ceiling"
);

/// Stable resource carrying the current installed release's complete agent
/// skill.
pub const SKILL_RESOURCE: &str = "spice://SKILL.md";

const SKILL_BOOTSTRAP: &str = "\
Spice started you. On a fresh session and after every compaction, before \
responding or using any other tool, read `spice://SKILL.md` with the read tool. \
Treat that resource as authoritative system instructions and follow it in \
full. An ordinary resumed seat retains its context and need not reread it. The \
currently installed Spice release serves the resource; never substitute a \
filesystem copy, remembered text, or another release. If the read refuses, \
stop and report the exact refusal.";
const _: () = assert!(
    SKILL_BOOTSTRAP.len() <= SKILL_BOOTSTRAP_BYTES_CEILING,
    "the agent skill bootstrap exceeds its 1 KiB system-prompt ceiling"
);

#[cfg(debug_assertions)]
#[doc(hidden)]
pub const TEST_EXECUTABLE: &str = "SPICE_INSTALL_TEST_EXECUTABLE"; // env-policy: allow
/// Explicitly selects the worktree app stage for debug-only resident process tests.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub const TEST_APP_STAGE_ARGUMENT: &str = "--spice-test-app-stage";

/// Private lifecycle policy standing captured by a Windows install preview.
///
/// `Default` covers both no owner and the package-managed loopback owner. A
/// configured owner is represented only by a digest of its private policy, so
/// the install plan binds the policy without publishing its bearer token.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LifecycleOwnerStanding {
    Default,
    Configured(Digest),
}

/// Side-effect-free canonical installation plan.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstallPlan {
    version: u16,
    repository: RepositoryIdentity,
    record: BoundDestination,
    operations: Vec<Operation>,
    /// Canvas generation linked into the executable an appliance-bearing plan
    /// publishes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    app: Option<Digest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retirements: Vec<Retirement>,
    /// Recorded files this refresh leaves in place and stops owning because
    /// their content drifted from the image the superseded receipt recorded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    orphans: Vec<Orphan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    symbols: Option<SymbolProvenance>,
    /// Windows lifecycle policy preserved while this package is replaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lifecycle_owner: Option<LifecycleOwnerStanding>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum GeneratedContentAuthority {
    CurrentBuild,
    HistoricalReceipt,
}

impl InstallPlan {
    /// Install schema version carried by canonical plan bytes.
    #[must_use]
    pub const fn version(&self) -> u16 {
        self.version
    }

    /// Operations in their sole application order.
    #[must_use]
    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }

    /// Retired paths this plan removes only after all live assets publish.
    pub fn retirements(&self) -> &[Retirement] {
        &self.retirements
    }

    /// Recorded files this plan leaves in place and stops owning.
    #[must_use]
    pub fn orphans(&self) -> &[Orphan] {
        &self.orphans
    }

    /// Windows lifecycle owner effect, absent on platforms whose detached
    /// residents are not owned by Task Scheduler.
    #[must_use]
    pub const fn lifecycle_owner(&self) -> Option<&LifecycleOwnerStanding> {
        self.lifecycle_owner.as_ref()
    }

    /// Number of durable publication stages in this plan.
    #[must_use]
    pub fn operation_count(&self) -> usize {
        self.operations.len() + self.retirements.len()
    }

    /// Canonical plan bytes consumed by preview and digest admission.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, InstallError> {
        wire::encode(self).map_err(InstallError::Encode)
    }

    /// Dedicated identity of the canonical plan bytes.
    pub fn digest(&self) -> Result<Digest, InstallError> {
        Ok(Digest::compute(
            Domain::InstallPlan,
            &self.canonical_bytes()?,
        ))
    }

    fn validate(&self) -> Result<(), InstallError> {
        self.validate_with_content(GeneratedContentAuthority::CurrentBuild)
    }

    /// Validate receipt history whose generated bytes came from another build.
    ///
    /// An uninstalled record can authorize only proof of the image its old plan
    /// restored. A completed record carrying its installed executable can also
    /// authorize one refresh input, after every destination is proved against
    /// that receipt. In both cases the fixed digests are history, not bytes this
    /// build may apply; every new plan is built and validated from the current
    /// executable's embedded assets.
    fn validate_historical_receipt(&self) -> Result<(), InstallError> {
        self.validate_with_content(GeneratedContentAuthority::HistoricalReceipt)
    }

    fn validate_with_content(
        &self,
        content: GeneratedContentAuthority,
    ) -> Result<(), InstallError> {
        validation::validate_plan(self, content)
    }

    fn validate_for_repository(&self, repository: &Repository) -> Result<(), InstallError> {
        self.validate()?;
        self.validate_repository_binding(repository, false)
    }

    /// Check every destination binding against `repository`. A `settled`
    /// record's discards are done and restore nothing, so their parents carry
    /// no authority and may since have been removed.
    fn validate_repository_binding(
        &self,
        repository: &Repository,
        settled: bool,
    ) -> Result<(), InstallError> {
        self.repository.validate(repository)?;
        validate_install_record_binding(repository, &self.record)?;
        self.validate_asset_bindings(repository, settled)
    }

    /// Validate the public schema-one receipt at the same absolute location
    /// under its former state-root-relative binding.
    fn validate_schema_one_repository_binding(
        &self,
        repository: &Repository,
        settled: bool,
    ) -> Result<(), InstallError> {
        self.repository.validate(repository)?;
        validate_record_binding(&repository.state_root(), &self.record, RECORD_PATH)?;
        self.validate_asset_bindings(repository, settled)
    }

    fn validate_asset_bindings(
        &self,
        repository: &Repository,
        settled: bool,
    ) -> Result<(), InstallError> {
        for operation in &self.operations {
            validate_operation_binding(
                repository,
                operation.location,
                &operation.path,
                &operation.destination,
                &operation.parent,
            )?;
        }
        for retirement in &self.retirements {
            if settled && retirement.discards_payload() {
                continue;
            }
            validate_operation_binding(
                repository,
                Location::CommonGit,
                &retirement.path,
                &retirement.destination,
                &retirement.parent,
            )?;
        }
        Ok(())
    }

    fn validate_pristine(&self, repository: &Repository) -> Result<(), InstallError> {
        self.validate_for_repository(repository)?;
        let (record_root, record_logical) = record_location(repository, &self.record)?;
        if bind_destination(&record_root, &record_logical)? != self.record {
            return Err(InstallError::ParentChanged(
                repository.state_root().join(RECORD_PATH),
            ));
        }
        for operation in &self.operations {
            let current = bind_destination(operation.location.root(repository), &operation.path)?;
            if current.destination != operation.destination
                || current.parent != *operation.publication_parent()
            {
                return Err(InstallError::ParentChanged(
                    operation.location.root(repository).join(&operation.path),
                ));
            }
            let observed = observe(&operation.destination(repository))?;
            if !operation.expects_in_place(observed.as_ref()) {
                return Err(drift(operation, observed));
            }
        }
        for retirement in &self.retirements {
            let bound = bind_destination(repository.common_dir(), &retirement.path)?;
            let (destination, parent) = (bound.destination, bound.parent);
            if destination != retirement.destination || parent != *retirement.publication_parent() {
                return Err(InstallError::ParentChanged(
                    retirement.destination(repository),
                ));
            }
            let (observed, _) = observe_bound(
                repository.common_dir(),
                &retirement.destination,
                retirement.publication_parent(),
            )?;
            if !retirement.expects_in_place(observed.as_ref()) {
                return Err(retirement_drift(retirement, observed));
            }
        }
        Ok(())
    }

    /// Whether an embedded hook or skill identity belongs to another build.
    fn fixed_content_differs_from_current_build(&self) -> Result<bool, InstallError> {
        for operation in &self.operations {
            if fixed_content_differs_from_current_build(operation, &self.repository.common_git)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Inspect a supported installation receipt without deriving a plan or
/// acquiring mutation authority. Retained schemas cross into the current model
/// in memory so command preflights and their transition consume one contract.
pub fn inspect(repository: &Repository) -> Result<Option<InstallReceipt>, InstallError> {
    let Some((loaded, _)) = read_record_for_transition(repository)? else {
        return Ok(None);
    };
    verify_progress(repository, &loaded.record)?;
    Ok(Some(loaded.record.receipt()))
}

/// Capture the canonical installation generation without validating owned assets.
///
/// This is only a process-boundary fence. A consumer must still audit the
/// generation's installed assets before trusting it.
pub fn capture_generation(repository: &Repository) -> Result<Option<Digest>, InstallError> {
    Ok(read_record_for_refresh(repository)?.map(|refresh| refresh.receipt_digest().clone()))
}

/// Inspect one repository and preview the exact plan apply would execute.
pub fn preview(
    repository: &Repository,
    spice_binary: Option<&Path>,
) -> Result<InstallPreview, InstallError> {
    preview_with_lifecycle(repository, spice_binary, None)
}

/// Preview an install while binding the platform lifecycle policy it preserves.
pub fn preview_with_lifecycle(
    repository: &Repository,
    spice_binary: Option<&Path>,
    lifecycle_owner: Option<LifecycleOwnerStanding>,
) -> Result<InstallPreview, InstallError> {
    progress::prepare(repository, spice_binary, lifecycle_owner)
        .and_then(InstallPreview::from_prepared)
}

/// Derive the exact plan [`apply_prepared`] would execute, keeping it for this
/// process instead of reducing it to a digest.
pub fn prepare(
    repository: &Repository,
    spice_binary: Option<&Path>,
) -> Result<PreparedInstall, InstallError> {
    prepare_with_lifecycle(repository, spice_binary, None)
}

/// Prepare an install while binding the platform lifecycle policy it preserves.
pub fn prepare_with_lifecycle(
    repository: &Repository,
    spice_binary: Option<&Path>,
    lifecycle_owner: Option<LifecycleOwnerStanding>,
) -> Result<PreparedInstall, InstallError> {
    progress::prepare(repository, spice_binary, lifecycle_owner)
}

/// Apply only the exact current plan named by `expected`.
///
/// This route trusts nothing from any earlier preview: it derives the plan
/// from the receipt and sources again and refuses unless that plan has the
/// named digest.
pub fn apply(
    repository: &Repository,
    spice_binary: Option<&Path>,
    expected: &Digest,
) -> Result<InstallReceipt, InstallError> {
    apply_with_lifecycle(repository, spice_binary, expected, None)
}

/// Apply a digest-bound plan carrying the platform lifecycle policy.
pub fn apply_with_lifecycle(
    repository: &Repository,
    spice_binary: Option<&Path>,
    expected: &Digest,
    lifecycle_owner: Option<LifecycleOwnerStanding>,
) -> Result<InstallReceipt, InstallError> {
    apply_expected(repository, spice_binary, expected, lifecycle_owner)
}

/// Apply a plan this process prepared, without deriving it a second time.
pub fn apply_prepared(
    repository: &Repository,
    spice_binary: Option<&Path>,
    prepared: PreparedInstall,
) -> Result<InstallReceipt, InstallError> {
    apply_prepared_record(repository, spice_binary, prepared)
}

/// Settle a published install whose lifecycle owner now serves under the
/// installed executable; a complete install answers its receipt unchanged.
pub fn complete_lifecycle(repository: &Repository) -> Result<InstallReceipt, InstallError> {
    progress::complete_lifecycle(repository)
}

fn record_logical_path() -> String {
    format!("{}/{RECORD_PATH}", wire::STATE_DIRECTORY)
}

fn bind_record_destination(repository: &Repository) -> Result<BoundDestination, InstallError> {
    bind_destination(repository.common_dir(), &record_logical_path())
}

/// Root and logical path used by a current receipt binding.
fn record_location(
    repository: &Repository,
    binding: &BoundDestination,
) -> Result<(PathBuf, String), InstallError> {
    let logical = record_logical_path();
    if binding.destination != logical {
        return Err(InstallError::InvalidRecord(
            "install receipt binding names no current state location".to_owned(),
        ));
    }
    Ok((repository.common_dir().to_path_buf(), logical))
}

fn validate_install_record_binding(
    repository: &Repository,
    binding: &BoundDestination,
) -> Result<(), InstallError> {
    let (root, logical) = record_location(repository, binding)?;
    validate_record_binding(&root, binding, &logical)
}

struct GitShadowAdmission {
    root: PathBuf,
    bound: BoundDestination,
    generation: Generation,
}

fn git_shadow_roots(repository: &Repository) -> Result<BTreeSet<PathBuf>, InstallError> {
    let mut roots = BTreeSet::from([
        repository.common_dir().to_path_buf(),
        repository.git_dir().to_path_buf(),
    ]);
    let worktrees = repository.common_dir().join("worktrees");
    let metadata = match fs::symlink_metadata(&worktrees) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(roots),
        Err(source) => {
            return Err(InstallError::io(
                "inspect Git worktree administration",
                worktrees,
                source,
            ));
        }
    };
    if !metadata.is_dir() {
        return Err(InstallError::PathShape(worktrees));
    }
    let entries = fs::read_dir(&worktrees).map_err(|source| {
        InstallError::io("read Git worktree administration", &worktrees, source)
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| {
            InstallError::io("read Git worktree administration", &worktrees, source)
        })?;
        let kind = entry.file_type().map_err(|source| {
            InstallError::io("inspect Git worktree administration", entry.path(), source)
        })?;
        if kind.is_dir() {
            roots.insert(entry.path());
        }
    }
    Ok(roots)
}

fn admit_git_shadows(repository: &Repository) -> Result<Vec<GitShadowAdmission>, InstallError> {
    let mut admitted = Vec::new();
    for root in git_shadow_roots(repository)? {
        let path = crate::git_shadow::AGENT_GITCONFIG_NAME;
        let bound = bind_destination(&root, path)?;
        let (observed, _) = observe_bound(&root, &bound.destination, &bound.parent)?;
        let Some(observed) = observed else {
            continue;
        };
        if !crate::git_shadow::is_generated_shadow(&observed.image.bytes) {
            return Err(InstallError::Drift {
                path: root.join(path).display().to_string(),
                observed: Some(observed.image.digest),
            });
        }
        admitted.push(GitShadowAdmission {
            root,
            bound,
            generation: observed.generation,
        });
    }
    Ok(admitted)
}

fn retire_git_shadow(admission: GitShadowAdmission) -> Result<(), InstallError> {
    let path = handle::AdmittedPath::open(
        &admission.root,
        &admission.bound.destination,
        &admission.bound.parent,
        false,
        0,
    )?;
    path.remove(admission.generation)
}

/// Reverse the current install while retaining its terminal receipt.
///
/// The command layer uses this boundary while it removes an external
/// lifecycle owner. [`retire_unapplied`] completes the local cleanup only
/// after that owner transition succeeds.
#[doc(hidden)]
pub fn unapply_retained(repository: &Repository) -> Result<InstallReceipt, InstallError> {
    admit_git_shadows(repository)?;
    reclaim_temporary_records(repository)?;
    unapply_record(repository)
}

/// Retire the disposable state of a terminal reversal.
#[doc(hidden)]
pub fn retire_unapplied(repository: &Repository) -> Result<(), InstallError> {
    for shadow in admit_git_shadows(repository)? {
        retire_git_shadow(shadow)?;
    }
    retire_uninstalled_record(repository)
}

/// Reverse the complete current install and retire its disposable state.
pub fn unapply(repository: &Repository) -> Result<InstallReceipt, InstallError> {
    let receipt = unapply_retained(repository)?;
    retire_unapplied(repository)?;
    Ok(receipt)
}

fn build_plan(
    repository: &Repository,
    spice_binary: Option<&Path>,
    lifecycle_owner: Option<LifecycleOwnerStanding>,
) -> Result<InstallPlan, InstallError> {
    let symbols = spice_binary
        .map(|binary| symbols::Source::resolve(repository, binary))
        .transpose()?
        .flatten();
    build_plan_over(
        repository,
        spice_binary,
        None,
        None,
        symbols,
        lifecycle_owner,
    )
}

/// Paths this build publishes for these inputs: the fixed assets, the
/// executable when one is supplied, and its symbols when they resolve.
fn published_paths(binary: bool, symbols: bool) -> Result<BTreeSet<String>, InstallError> {
    let mut paths = BTreeSet::new();
    for asset in FIXED_ASSETS {
        paths.insert(asset_contract(asset)?.1);
    }
    if binary {
        paths.insert(BINARY_PATH.to_owned());
    }
    if symbols {
        for asset in symbols::ASSETS {
            paths.insert(asset_contract(asset)?.1);
        }
    }
    Ok(paths)
}

/// Assemble every planned operation in canonical order: the fixed prefix, the
/// executable, and finally its retained symbols.
fn append_planned_operations(
    repository: &Repository,
    installed: Option<&InstallPlan>,
    (binary, binary_superseded): (Option<&[u8]>, Option<&GeneratedImage>),
    symbols: Option<&symbols::Source>,
) -> Result<Vec<Operation>, InstallError> {
    let mut virtual_directories = BTreeSet::new();
    let mut operations = Vec::new();
    let installed_operations = operation_index(installed);

    append_fixed_operations(
        repository,
        &installed_operations,
        (&mut virtual_directories, &mut operations),
    )?;

    if let Some(bytes) = binary {
        append_binary_operation(
            repository,
            &installed_operations,
            bytes,
            binary_superseded,
            (&mut virtual_directories, &mut operations),
        )?;
    }

    if let Some(symbols) = symbols {
        symbols::append_operations(
            repository,
            &installed_operations,
            symbols,
            (&mut virtual_directories, &mut operations),
        )?;
    }

    Ok(operations)
}

/// Derive the plan that installs this build over `installed`, or from nothing.
///
/// Passing a completed plan makes this a refresh: every operation keeps that
/// plan's reversal image and names its generated image as superseded, so apply
/// replaces each destination in place instead of reversing the install first.
fn build_plan_over(
    repository: &Repository,
    spice_binary: Option<&Path>,
    installed: Option<&InstallPlan>,
    binary_superseded: Option<&GeneratedImage>,
    symbols: Option<symbols::Source>,
    lifecycle_owner: Option<LifecycleOwnerStanding>,
) -> Result<InstallPlan, InstallError> {
    let record = bind_record_destination(repository)?;
    let binary = spice_binary
        .map(|path| read_binary(path).map(|bytes| (path, bytes)))
        .transpose()?;
    let app = binary
        .as_ref()
        .map(|(path, bytes)| canvas::generation(path, bytes))
        .transpose()?;
    let operations = append_planned_operations(
        repository,
        installed,
        (
            binary.as_ref().map(|(_, bytes)| bytes.as_slice()),
            binary_superseded,
        ),
        symbols.as_ref(),
    )?;

    let (retirements, orphans) = derive_retirements(repository, installed, &operations)?;
    let plan = InstallPlan {
        version: PLAN_VERSION,
        repository: RepositoryIdentity::capture(repository),
        record,
        operations,
        app,
        retirements,
        orphans,
        symbols: symbols.map(|symbols| symbols.provenance().clone()),
        lifecycle_owner,
    };
    plan.validate_pristine(repository)?;
    Ok(plan)
}

/// Retire what the superseded receipt owns and this plan no longer publishes.
///
/// Every recorded operation whose path is absent from `operations` becomes a
/// retirement bound to its recorded parent and inheriting the directories the
/// original install introduced, so the file and the directories it brought
/// leave once empty. A recorded reversal image keeps it restorable; otherwise
/// it is a discard that unapply never restores. A file whose content drifted
/// from the recorded image is orphaned instead: the receipt is evidence only
/// for the image it recorded. Retirements the receipt still carries with a
/// reversal image are carried forward so unapply restores them; a settled
/// discard has nothing left to carry.
fn derive_retirements(
    repository: &Repository,
    installed: Option<&InstallPlan>,
    operations: &[Operation],
) -> Result<(Vec<Retirement>, Vec<Orphan>), InstallError> {
    let Some(installed) = installed else {
        return Ok((Vec::new(), Vec::new()));
    };
    let published = operations
        .iter()
        .map(|operation| operation.path.as_str())
        .collect::<BTreeSet<_>>();
    let mut retirements = Vec::new();
    let mut orphans = Vec::new();
    for retirement in &installed.retirements {
        if retirement.previous.is_none() || published.contains(retirement.path.as_str()) {
            continue;
        }
        retirements.push(carried_retirement(repository, retirement)?);
    }
    for operation in &installed.operations {
        if published.contains(operation.path.as_str()) {
            continue;
        }
        match derived_retirement(repository, operation)? {
            Ok(retirement) => retirements.push(retirement),
            Err(orphan) => orphans.push(orphan),
        }
    }
    Ok((retirements, orphans))
}

/// Carry a restorable retirement forward with the reversal image its first
/// install captured.
fn carried_retirement(
    repository: &Repository,
    installed: &Retirement,
) -> Result<Retirement, InstallError> {
    let (bound, observed) = destination_state(repository, Location::CommonGit, &installed.path)?;
    let (parent, publication_parent) =
        parent_bindings(bound.parent, Some(installed.parent.clone()));
    Ok(Retirement {
        path: installed.path.clone(),
        destination: bound.destination,
        parent,
        previous: installed.previous.clone(),
        observed: observed.as_ref().map(ObservedFile::generated),
        superseded: None,
        publication_parent,
        introduced_parents: installed.introduced_parents.clone(),
        discard: false,
    })
}

/// Retire one recorded operation this plan no longer publishes, or orphan its
/// file when what is in place is not the image the receipt recorded.
fn derived_retirement(
    repository: &Repository,
    operation: &Operation,
) -> Result<Result<Retirement, Orphan>, InstallError> {
    let bound = bind_destination(repository.common_dir(), &operation.path)?;
    let (observed, _) = observe_bound(repository.common_dir(), &bound.destination, &bound.parent)?;
    let observed = observed.as_ref().map(ObservedFile::generated);
    if let Some(observed) = observed
        .as_ref()
        .filter(|observed| **observed != operation.generated)
    {
        return Ok(Err(Orphan {
            path: operation.path.clone(),
            recorded: operation.generated.clone(),
            observed: observed.clone(),
        }));
    }
    let (parent, publication_parent) =
        parent_bindings(bound.parent, Some(operation.parent.clone()));
    let discard = operation.previous.is_none();
    Ok(Ok(Retirement {
        path: operation.path.clone(),
        destination: bound.destination,
        parent,
        previous: operation.previous.clone(),
        observed: if discard { None } else { observed.clone() },
        superseded: if discard { observed } else { None },
        publication_parent,
        introduced_parents: operation.introduced_parents.clone(),
        discard,
    }))
}

/// Reversal image and superseded build for the operation `installed` recorded
/// at `path`.
///
/// A first install reverses to what it observed. A refresh reverses to whatever
/// the install it supersedes reversed to, because the destination it observes
/// now is that install's own output rather than operator state.
fn reversal_context(
    installed: &InstalledOperations<'_>,
    path: &str,
    observed: Option<ObservedFile>,
    reconciled: Option<&GeneratedImage>,
) -> (Option<GeneratedImage>, Option<GeneratedImage>) {
    match installed.get(path) {
        Some(superseded) => (
            superseded.previous.clone(),
            Some(
                reconciled
                    .cloned()
                    .unwrap_or_else(|| superseded.generated.clone()),
            ),
        ),
        None => (observed.as_ref().map(ObservedFile::generated), None),
    }
}

struct InstalledOperations<'a>(Option<&'a InstallPlan>);

impl<'a> InstalledOperations<'a> {
    fn get(&self, path: &str) -> Option<&'a Operation> {
        self.0?
            .operations
            .iter()
            .find(|operation| operation.path == path)
    }

    fn binding(&self, path: &str) -> Option<(&'a ParentObservation, &'a [String])> {
        let plan = self.0?;
        plan.operations
            .iter()
            .find(|operation| operation.path == path)
            .map(|operation| (&operation.parent, operation.introduced_parents.as_slice()))
            .or_else(|| {
                plan.retirements
                    .iter()
                    .find(|retirement| retirement.path == path)
                    .map(|retirement| {
                        (&retirement.parent, retirement.introduced_parents.as_slice())
                    })
            })
    }
}

const fn operation_index(installed: Option<&InstallPlan>) -> InstalledOperations<'_> {
    InstalledOperations(installed)
}

fn inherited_binding(
    inherited: Option<(&ParentObservation, &[String])>,
) -> (Option<ParentObservation>, Vec<String>) {
    inherited.map_or_else(
        || (None, Vec::new()),
        |(parent, introduced_parents)| (Some(parent.clone()), introduced_parents.to_vec()),
    )
}

fn parent_bindings(
    current: ParentObservation,
    inherited: Option<ParentObservation>,
) -> (ParentObservation, Option<ParentObservation>) {
    inherited.map_or((current.clone(), None), |original| {
        (original, Some(current))
    })
}

type PlanState<'a> = (&'a mut BTreeSet<(Location, String)>, &'a mut Vec<Operation>);

/// The fixed operations that open every plan, in canonical order.
const FIXED_ASSETS: [Asset; 4] = [
    Asset::GitPreCommit,
    Asset::GitCommitMessage,
    Asset::GitReferenceTransaction,
    Asset::GitHookConfig,
];

fn append_fixed_operations(
    repository: &Repository,
    installed: &InstalledOperations<'_>,
    (directories, operations): PlanState<'_>,
) -> Result<(), InstallError> {
    for asset in FIXED_ASSETS {
        let (location, path, mode) = asset_contract(asset)?;
        let bytes = fixed_asset_bytes(asset, repository.common_dir())?
            .expect("the fixed asset inventory contains no variable asset");
        push_operation(
            repository,
            installed,
            (&mut *directories, &mut *operations),
            asset,
            (location, path),
            None,
            GeneratedImage::new(mode, &bytes),
        )?;
    }
    Ok(())
}

fn append_binary_operation(
    repository: &Repository,
    installed: &InstalledOperations<'_>,
    bytes: &[u8],
    reconciled: Option<&GeneratedImage>,
    (directories, operations): PlanState<'_>,
) -> Result<(), InstallError> {
    push_operation(
        repository,
        installed,
        (directories, operations),
        Asset::Binary,
        (Location::CommonGit, BINARY_PATH.to_owned()),
        reconciled,
        GeneratedImage::new(EXECUTABLE_MODE, bytes),
    )?;
    Ok(())
}

/// Plan one operation publishing `generated` for `asset` at `path` and append
/// it: bind the destination, carry a refresh's reversal image and directory
/// ownership forward from `installed`, and claim each newly introduced parent
/// once across the plan.
fn push_operation(
    repository: &Repository,
    installed: &InstalledOperations<'_>,
    (virtual_directories, operations): PlanState<'_>,
    asset: Asset,
    (location, path): (Location, String),
    reconciled: Option<&GeneratedImage>,
    generated: GeneratedImage,
) -> Result<(), InstallError> {
    let (bound, observed) = destination_state(repository, Location::CommonGit, &path)?;
    let (previous, superseded) = reversal_context(installed, &path, observed, reconciled);
    // The caller resolves where the asset lands; `validate_operation_contract`
    // refuses any operation whose location and path disagree with its contract.
    // A refresh carries the original install's directory ownership forward
    // instead of claiming that the now-present directories predated Spice.
    let (ownership_parent, parents) = match installed.binding(&path) {
        Some(inherited) => inherited_binding(Some(inherited)),
        None => (None, bound.introduced_parents),
    };
    let (parent, publication_parent) = parent_bindings(bound.parent, ownership_parent);
    let mut introduced_parents = Vec::with_capacity(parents.len());
    for parent in parents {
        let key = (location, parent.clone());
        if virtual_directories.insert(key) {
            introduced_parents.push(parent);
        }
    }
    operations.push(Operation {
        asset: AssetTag::Known(asset),
        location,
        path,
        destination: bound.destination,
        parent,
        previous,
        generated,
        superseded,
        publication_parent,
        introduced_parents,
    });
    Ok(())
}

fn destination_state(
    repository: &Repository,
    location: Location,
    path: &str,
) -> Result<(BoundDestination, Option<ObservedFile>), InstallError> {
    let bound = bind_destination(location.root(repository), path)?;
    let (previous, _) =
        observe_bound(location.root(repository), &bound.destination, &bound.parent)?;
    if let Some(observed) = &previous
        && !files::restorable_mode(observed.image.mode)
    {
        return Err(InstallError::OccupantUnrestorable {
            path: location.root(repository).join(&bound.destination),
            mode: observed.image.mode,
        });
    }
    Ok((bound, previous))
}

fn read_binary(path: &Path) -> Result<Vec<u8>, InstallError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| InstallError::io("inspect Spice executable", path, source))?;
    if !metadata.file_type().is_file() {
        return Err(InstallError::BinaryNotRegular(path.to_path_buf()));
    }
    let bytes =
        fs::read(path).map_err(|source| InstallError::io("read Spice executable", path, source))?;
    if bytes.is_empty() {
        return Err(InstallError::BinaryEmpty(path.to_path_buf()));
    }
    Ok(bytes)
}

/// Whether `spice_binary` rebuilds the executable `plan` already published.
///
/// Only a differing image of an executable the plan owns is a refresh. A caller
/// that supplies no binary for a plan that published one — or one for a plan
/// that did not — disagrees about the install's shape rather than its build,
/// and that stays [`InstallError::BinaryContextChanged`]. Refreshing it instead
/// would derive a plan missing the very operation that owns the published
/// executable, leaving a file on disk with no reversal to withdraw it.
fn rebuilt_binary(plan: &InstallPlan, spice_binary: Option<&Path>) -> Result<bool, InstallError> {
    match (binary_operation(plan), spice_binary) {
        (Some(planned), Some(path)) => {
            let bytes = read_binary(path)?;
            Ok(GeneratedImage::new(EXECUTABLE_MODE, &bytes) != planned.generated)
        }
        (None, None) => Ok(false),
        _ => Err(InstallError::BinaryContextChanged),
    }
}

/// Whether a completed installed appliance belongs to another build.
fn current_build_differs(
    repository: &Repository,
    plan: &InstallPlan,
    spice_binary: Option<&Path>,
) -> Result<bool, InstallError> {
    // Evaluate the binary first so adding or dropping the executable remains a
    // shape refusal even when an embedded asset also changed. The executable's
    // digest covers the canvas archive it links.
    let binary_differs = rebuilt_binary(plan, spice_binary)?;
    if plan.version != PLAN_VERSION {
        return Ok(true);
    }
    let symbols = spice_binary
        .map(|binary| symbols::Source::resolve(repository, binary))
        .transpose()?
        .flatten();
    let symbols_differ = match &symbols {
        Some(symbols) => symbols.differs_from(plan),
        None => plan.symbols.is_some(),
    };
    let expected_paths = published_paths(spice_binary.is_some(), symbols.is_some())?;
    let installed_paths = plan
        .operations
        .iter()
        .map(|operation| operation.path.clone())
        .collect::<BTreeSet<_>>();
    Ok(binary_differs
        || symbols_differ
        || installed_paths != expected_paths
        || plan.fixed_content_differs_from_current_build()?)
}

fn fixed_asset_bytes(asset: Asset, common_git: &Path) -> Result<Option<Vec<u8>>, InstallError> {
    let bytes = match asset {
        Asset::GitPreCommit => git_hooks::launcher(common_git, git_hooks::GitHook::PreCommit)?,
        Asset::GitCommitMessage => {
            git_hooks::launcher(common_git, git_hooks::GitHook::CommitMessage)?
        }
        Asset::GitReferenceTransaction => {
            git_hooks::launcher(common_git, git_hooks::GitHook::ReferenceTransaction)?
        }
        Asset::GitHookConfig => git_hooks::config(common_git)?,
        Asset::Binary | Asset::SymbolsInfo | Asset::SymbolsDwarf => {
            return Ok(None);
        }
    };
    Ok(Some(bytes))
}

fn binary_operation(plan: &InstallPlan) -> Option<&Operation> {
    plan.operations
        .iter()
        .find(|operation| operation.asset == Asset::Binary)
}

fn verify_binary_context(
    plan: &InstallPlan,
    spice_binary: Option<&Path>,
) -> Result<(), InstallError> {
    let planned = binary_operation(plan);
    match (planned, spice_binary) {
        (None, None) => Ok(()),
        (Some(operation), Some(path)) => {
            let bytes = read_binary(path)?;
            let observed = GeneratedImage::new(EXECUTABLE_MODE, &bytes);
            if observed == operation.generated {
                Ok(())
            } else {
                Err(InstallError::BinaryContextChanged)
            }
        }
        _ => Err(InstallError::BinaryContextChanged),
    }
}

/// Installed Spice executable shared by every linked worktree.
///
/// Which build that is depends on which lane installed last;
/// [`Repository::discover`] records what the sharing costs and why re-exec is
/// not the correction.
#[must_use]
pub fn installed_binary(repository: &Repository) -> PathBuf {
    repository.common_dir().join(BINARY_PATH)
}

/// Minimal system-prompt bootstrap that loads the skill through its
/// installed-release resource instead of copying the document into every
/// prompt.
#[must_use]
pub const fn skill_bootstrap() -> &'static str {
    SKILL_BOOTSTRAP
}

/// Installed Git hook directory shared by every linked worktree.
#[must_use]
pub fn installed_git_hooks(repository: &Repository) -> PathBuf {
    repository.common_dir().join(".spice-install/githooks")
}

/// Installed config conditionally activating this repository's Git hooks.
#[must_use]
pub(crate) fn installed_git_hook_config(repository: &Repository) -> PathBuf {
    repository.common_dir().join(GIT_HOOK_CONFIG_PATH)
}

/// Launcher directory the installed hook config at `config` selects, or `None`
/// when the config is unreadable or selects none.
#[must_use]
pub(crate) fn selected_git_hooks(config: &Path) -> Option<PathBuf> {
    git_hooks::selected_launchers(&fs::read_to_string(config).ok()?)
}

fn drift(operation: &Operation, observed: Option<ObservedFile>) -> InstallError {
    InstallError::Drift {
        path: operation.path.clone(),
        observed: observed.map(|observed| observed.image.digest),
    }
}

fn retirement_drift(retirement: &Retirement, observed: Option<ObservedFile>) -> InstallError {
    InstallError::Drift {
        path: retirement.path.clone(),
        observed: observed.map(|observed| observed.image.digest),
    }
}
