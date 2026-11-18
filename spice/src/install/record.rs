use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use wire::Digest;

use super::files::{observe_bound, publish_bound};
use super::{
    FileImage, GeneratedContentAuthority, Generation, InstallError, InstallPlan,
    InstallPlanSummary, InstallReceipt, InstallStatus, RECORD_MODE, RECORD_PATH, RECORD_VERSION,
    REVERSAL_PATH, ReceiptGeneration, Repository, RepositoryIdentity, bind_record_destination,
    record_location, validate_install_record_binding, validation,
};

mod migrate;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InstallRecord {
    pub(super) version: u16,
    repository: RepositoryIdentity,
    pub(super) plan: InstallPlan,
    pub(super) plan_digest: Digest,
    pub(super) destinations: Vec<(super::Location, String)>,
    pub(super) status: InstallStatus,
    pub(super) completed: usize,
    pub(super) installed: Vec<Option<Generation>>,
    pub(super) installed_parents: Vec<Option<Generation>>,
    pub(super) restored: Vec<Option<Generation>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) retired_parents: Vec<Option<Generation>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) restored_retirements: Vec<Option<Generation>>,
}

#[derive(Deserialize)]
struct InstallRecordHeader {
    version: u16,
}

pub(super) struct LoadedRecord {
    pub(super) record: InstallRecord,
    pub(super) generation: ReceiptGeneration,
}

pub(super) enum RefreshRecord {
    Current(LoadedRecord),
    /// A record whose current-schema image differs from the receipt on disk
    /// after a mount-device repair. Only a refresh plan may replace it.
    Repairable {
        loaded: LoadedRecord,
        original_digest: Digest,
    },
}

impl RefreshRecord {
    pub(super) const fn is_repairable(&self) -> bool {
        matches!(self, Self::Repairable { .. })
    }

    pub(super) const fn loaded(&self) -> &LoadedRecord {
        match self {
            Self::Current(loaded) | Self::Repairable { loaded, .. } => loaded,
        }
    }

    /// The digest naming the receipt on disk, before any repair.
    pub(super) const fn receipt_digest(&self) -> &Digest {
        match self {
            Self::Current(loaded) => &loaded.record.plan_digest,
            Self::Repairable {
                original_digest, ..
            } => original_digest,
        }
    }

    pub(super) fn into_loaded(self) -> LoadedRecord {
        match self {
            Self::Current(loaded) | Self::Repairable { loaded, .. } => loaded,
        }
    }
}

impl InstallRecord {
    pub(super) fn new(plan: InstallPlan) -> Result<Self, InstallError> {
        let plan_digest = plan.digest()?;
        let repository = plan.repository.clone();
        let destinations = plan
            .operations
            .iter()
            .map(|operation| (operation.location, operation.destination.clone()))
            .collect();
        let slots = plan.operations.len();
        let retirements = plan.retirements.len();
        Ok(Self {
            version: RECORD_VERSION,
            repository,
            plan,
            plan_digest,
            destinations,
            status: InstallStatus::Applying,
            completed: 0,
            installed: vec![None; slots],
            installed_parents: vec![None; slots],
            restored: vec![None; slots],
            retired_parents: vec![None; retirements],
            restored_retirements: vec![None; retirements],
        })
    }

    pub(super) fn validate(&self) -> Result<(), InstallError> {
        self.validate_with_content(self.status == InstallStatus::Uninstalled)
    }

    /// Validate state read from disk before deciding whether to refresh it.
    ///
    /// A settled receipt is the intact output of whichever release wrote it:
    /// evidence of what that release published, admitted only so a refresh
    /// can supersede it or a reversal restore what it recorded, after
    /// progress verification proves every destination still matches. Its
    /// generated digests are history, never bytes this build applies. An
    /// interrupted direction, and a published install still owed its
    /// lifecycle owner, remain owned by the build that started them.
    fn validate_loaded(&self) -> Result<(), InstallError> {
        self.validate_with_content(self.settled())
    }

    fn rebase_repository(&mut self, repository: &Repository) -> Result<(), InstallError> {
        if !matches!(
            self.status,
            InstallStatus::Establishing | InstallStatus::Complete | InstallStatus::Uninstalled
        ) {
            return Err(InstallError::InvalidRecord(
                "an interrupted install cannot be relocated".to_owned(),
            ));
        }
        let prior = self.repository.common_git.clone();
        if prior == repository.common_dir() {
            return Ok(());
        }
        if prior.exists() {
            return Err(InstallError::InvalidRecord(
                "install record was copied while its owning repository still exists".to_owned(),
            ));
        }
        self.repository = RepositoryIdentity::capture(repository);
        self.plan.repository = self.repository.clone();
        self.plan_digest = self.plan.digest()?;
        Ok(())
    }

    /// Rebind one internally coherent receipt after macOS renumbers its mount.
    ///
    /// The persisted device number is an immediate race witness, not a stable
    /// APFS volume identity. A reboot may change it while every inode and path
    /// remains intact. Only a single old device shared by every recorded parent
    /// is eligible; strict repository validation below still proves every inode
    /// before the repaired receipt can drive a refresh.
    fn repair_mount_device(
        &mut self,
        repository: &Repository,
    ) -> Result<Option<Digest>, InstallError> {
        #[cfg(not(target_vendor = "apple"))]
        {
            let _ = repository;
            Ok(None)
        }
        #[cfg(target_vendor = "apple")]
        {
            let metadata =
                std::fs::symlink_metadata(repository.common_dir()).map_err(|source| {
                    InstallError::io(
                        "inspect repository mount generation",
                        repository.common_dir(),
                        source,
                    )
                })?;
            let current = Generation::observe(&metadata).device;
            let prior = self.plan.record.parent.generation.device;
            if current == prior {
                return Ok(None);
            }
            let coherent = self
                .plan
                .operations
                .iter()
                .map(|operation| operation.parent.generation.device)
                .chain(
                    self.plan
                        .operations
                        .iter()
                        .filter_map(|operation| operation.publication_parent.as_ref())
                        .map(|parent| parent.generation.device),
                )
                .chain(
                    self.plan
                        .retirements
                        .iter()
                        .map(|retirement| retirement.parent.generation.device),
                )
                .chain(
                    self.plan
                        .retirements
                        .iter()
                        .filter_map(|retirement| retirement.publication_parent.as_ref())
                        .map(|parent| parent.generation.device),
                )
                .chain(
                    self.installed_parents
                        .iter()
                        .flatten()
                        .map(|parent| parent.device),
                )
                .chain(
                    self.retired_parents
                        .iter()
                        .flatten()
                        .map(|parent| parent.device),
                )
                .all(|device| device == prior);
            if !coherent {
                return Ok(None);
            }

            let original_digest = self.plan_digest.clone();
            self.plan.record.parent.generation.device = current;
            for operation in &mut self.plan.operations {
                operation.parent.generation.device = current;
                if let Some(parent) = &mut operation.publication_parent {
                    parent.generation.device = current;
                }
            }
            for retirement in &mut self.plan.retirements {
                retirement.parent.generation.device = current;
                if let Some(parent) = &mut retirement.publication_parent {
                    parent.generation.device = current;
                }
            }
            for parent in self.installed_parents.iter_mut().flatten() {
                parent.device = current;
            }
            for parent in self.retired_parents.iter_mut().flatten() {
                parent.device = current;
            }
            self.plan_digest = self.plan.digest()?;
            Ok(Some(original_digest))
        }
    }

    fn validate_with_content(&self, historical: bool) -> Result<(), InstallError> {
        self.validate_with_content_at_version(historical, RECORD_VERSION)
    }

    fn validate_with_content_at_version(
        &self,
        historical: bool,
        version: u16,
    ) -> Result<(), InstallError> {
        if self.version != self.plan.version {
            return Err(InstallError::InvalidRecord(
                "install record and plan versions do not match".to_owned(),
            ));
        }
        if self.version != version {
            return Err(InstallError::InvalidRecord(format!(
                "install record version {} is not {version}",
                self.version
            )));
        }
        if version == RECORD_VERSION {
            if historical {
                self.plan.validate_historical_receipt()?;
            } else {
                self.plan.validate()?;
            }
        } else {
            let content = if historical {
                GeneratedContentAuthority::HistoricalReceipt
            } else {
                GeneratedContentAuthority::CurrentBuild
            };
            validation::validate_plan_at_version(&self.plan, content, version)?;
        }
        self.validate_plan_closure()?;
        self.validate_progress_content()
    }

    fn validate_plan_closure(&self) -> Result<(), InstallError> {
        if self.repository != self.plan.repository {
            return Err(InstallError::InvalidRecord(
                "install record repository does not close over its plan".to_owned(),
            ));
        }
        if self.plan_digest != self.plan.digest()? {
            return Err(InstallError::InvalidRecord(
                "install record plan digest does not match its canonical plan".to_owned(),
            ));
        }
        if !self
            .plan
            .operations
            .iter()
            .map(|operation| (operation.location, operation.destination.as_str()))
            .eq(self
                .destinations
                .iter()
                .map(|(location, path)| (*location, path.as_str())))
        {
            return Err(InstallError::InvalidRecord(
                "install destination inventory is not exact and ordered".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_progress_content(&self) -> Result<(), InstallError> {
        let slots = self.plan.operations.len();
        if self.installed.len() != slots
            || self.installed_parents.len() != slots
            || self.restored.len() != slots
        {
            return Err(InstallError::InvalidRecord(
                "install generation inventory has the wrong length".to_owned(),
            ));
        }
        let retirements = self.plan.retirements.len();
        if self.retired_parents.len() != retirements
            || self.restored_retirements.len() != retirements
        {
            return Err(InstallError::InvalidRecord(
                "install retirement progress has the wrong length".to_owned(),
            ));
        }
        let operation_count = self.plan.operation_count();
        if self.completed > operation_count {
            return Err(InstallError::InvalidRecord(
                "install progress is past the operation inventory".to_owned(),
            ));
        }
        if self
            .installed
            .iter()
            .zip(&self.installed_parents)
            .any(|(file, parent)| file.is_some() != parent.is_some())
        {
            return Err(InstallError::InvalidRecord(
                "installed file and parent generations are not paired".to_owned(),
            ));
        }
        if matches!(
            self.status,
            InstallStatus::Establishing | InstallStatus::Complete | InstallStatus::Uninstalled
        ) && self.completed != operation_count
        {
            return Err(InstallError::InvalidRecord(
                "terminal install record has incomplete progress".to_owned(),
            ));
        }
        if self.status == InstallStatus::Establishing && self.plan.lifecycle_owner.is_none() {
            return Err(InstallError::InvalidRecord(
                "establishing record names no lifecycle owner".to_owned(),
            ));
        }
        if self.status == InstallStatus::Applying && self.restored.iter().any(Option::is_some) {
            return Err(InstallError::InvalidRecord(
                "applying record carries reversal generations".to_owned(),
            ));
        }
        if self.status == InstallStatus::Applying
            && self.restored_retirements.iter().any(Option::is_some)
        {
            return Err(InstallError::InvalidRecord(
                "applying record carries retirement reversals".to_owned(),
            ));
        }
        if matches!(
            self.status,
            InstallStatus::Establishing | InstallStatus::Complete | InstallStatus::Unapplying
        ) && (self.installed.iter().any(Option::is_none)
            || self.installed_parents.iter().any(Option::is_none))
        {
            return Err(InstallError::InvalidRecord(
                "installed record omits a destination generation".to_owned(),
            ));
        }
        Ok(())
    }

    pub(super) fn validate_for_repository(
        &self,
        repository: &Repository,
    ) -> Result<(), InstallError> {
        self.validate()?;
        self.repository.validate(repository)?;
        self.plan
            .validate_repository_binding(repository, self.discards_done())
    }

    /// Whether this record's transaction is finished either way.
    pub(super) const fn settled(&self) -> bool {
        matches!(
            self.status,
            InstallStatus::Complete | InstallStatus::Uninstalled
        )
    }

    /// Whether every retirement has already discarded its payload: any state
    /// other than an in-flight direction.
    pub(super) const fn discards_done(&self) -> bool {
        !matches!(
            self.status,
            InstallStatus::Applying | InstallStatus::Unapplying
        )
    }

    /// A checkpoint mutates only the receipt. Each operation checks its own
    /// binding before mutation; transaction admission and terminal publication
    /// check the entire inventory. Rechecking every destination at every
    /// counter write makes an N-file installation perform O(N²) filesystem work.
    fn validate_for_write(&self, repository: &Repository) -> Result<(), InstallError> {
        // Progress arrays are admitted on construction/load and retain their
        // shape in this process. Check them in full at the terminal boundary,
        // not once per element updated.
        if self.completed > self.plan.operation_count() {
            return Err(InstallError::InvalidRecord(
                "install progress is past the operation inventory".to_owned(),
            ));
        }
        if self.repository != self.plan.repository {
            return Err(InstallError::InvalidRecord(
                "install record repository does not close over its plan".to_owned(),
            ));
        }
        self.repository.validate(repository)?;
        if self.settled() {
            self.validate_progress_content()?;
            self.plan.validate_repository_binding(repository, true)
        } else {
            validate_install_record_binding(repository, &self.plan.record)
        }
    }

    pub(super) fn validate_loaded_for_repository(
        &self,
        repository: &Repository,
    ) -> Result<(), InstallError> {
        self.validate_loaded()?;
        self.repository.validate(repository)?;
        self.plan
            .validate_repository_binding(repository, self.discards_done())
    }

    /// The public receipt. The lifecycle owner is the summary's last stage, so
    /// a settled record with one counts it as acknowledged.
    pub(super) fn receipt(&self) -> InstallReceipt {
        let lifecycle_settled = self.settled() && self.plan.lifecycle_owner.is_some();
        InstallReceipt {
            plan: InstallPlanSummary::from_plan(&self.plan),
            digest: self.plan_digest.clone(),
            status: self.status,
            completed: self.completed + usize::from(lifecycle_settled),
        }
    }
}

pub(super) fn read_record(repository: &Repository) -> Result<Option<LoadedRecord>, InstallError> {
    let Some((mut loaded, _)) = decode_record(repository, false)? else {
        return Ok(None);
    };
    finish_loaded_record(repository, &mut loaded)?;
    Ok(Some(loaded))
}

pub(super) fn read_record_for_refresh(
    repository: &Repository,
) -> Result<Option<RefreshRecord>, InstallError> {
    let Some((mut loaded, migrated)) = decode_record(repository, true)? else {
        return Ok(None);
    };
    let original_digest = migrated.or(finish_loaded_record(repository, &mut loaded)?);
    Ok(Some(match original_digest {
        Some(original_digest) => RefreshRecord::Repairable {
            loaded,
            original_digest,
        },
        None => RefreshRecord::Current(loaded),
    }))
}

pub(super) fn read_record_for_transition(
    repository: &Repository,
) -> Result<Option<(LoadedRecord, bool)>, InstallError> {
    let Some((mut loaded, migrated)) = decode_record(repository, true)? else {
        return Ok(None);
    };
    let requires_publish = migrated
        .or(finish_loaded_record(repository, &mut loaded)?)
        .is_some();
    Ok(Some((loaded, requires_publish)))
}

fn decode_record(
    repository: &Repository,
    migrate: bool,
) -> Result<Option<(LoadedRecord, Option<Digest>)>, InstallError> {
    record_path(repository)?;
    let state_root = repository.state_root();
    if fs::symlink_metadata(&state_root).is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
    {
        return Ok(None);
    }
    let binding = bind_record_destination(repository)?;
    let (root, _) = record_location(repository, &binding)?;
    let (observed, _) = observe_bound(&root, &binding.destination, &binding.parent)?;
    let Some(observed) = observed else {
        return Ok(None);
    };
    let generation = ReceiptGeneration::observed(&observed);
    let image = observed.image;
    let header: InstallRecordHeader = wire::decode(&image.bytes).map_err(InstallError::Decode)?;
    if header.version != RECORD_VERSION && (!migrate || !migrate::supports(header.version)) {
        return Err(migrate::foreign(repository, header.version));
    }
    let mut record: InstallRecord = wire::decode(&image.bytes).map_err(InstallError::Decode)?;
    let canonical = wire::encode(&record).map_err(InstallError::Encode)?;
    if canonical != image.bytes {
        return Err(InstallError::InvalidRecord(
            "install record is not canonical".to_owned(),
        ));
    }
    let migrated = migrate
        .then(|| migrate::forward(repository, &mut record))
        .transpose()?
        .flatten();
    Ok(Some((LoadedRecord { record, generation }, migrated)))
}

/// Validate a decoded record against `repository`, returning the on-disk
/// digest when a mount-device repair makes the record to refresh differ from
/// that receipt.
fn finish_loaded_record(
    repository: &Repository,
    loaded: &mut LoadedRecord,
) -> Result<Option<Digest>, InstallError> {
    loaded.record.validate_loaded()?;
    let record = &mut loaded.record;
    if record.repository.common_git != repository.common_dir() {
        record.rebase_repository(repository)?;
    }
    let repaired = record.repair_mount_device(repository)?;
    record.validate_loaded_for_repository(repository)?;
    Ok(repaired)
}

pub(super) fn write_initial_record(
    repository: &Repository,
    record: &InstallRecord,
) -> Result<ReceiptGeneration, InstallError> {
    record.validate_for_write(repository)?;
    let (root, _) = record_location(repository, &record.plan.record)?;
    let (observed, _) = observe_bound(
        &root,
        &record.plan.record.destination,
        &record.plan.record.parent,
    )?;
    if observed.is_some() {
        return Err(InstallError::ParentChanged(record_path(repository)?));
    }
    write_record_bytes(repository, record, None)
}

pub(super) fn write_record(
    repository: &Repository,
    record: &InstallRecord,
    expected: ReceiptGeneration,
) -> Result<ReceiptGeneration, InstallError> {
    record.validate_for_write(repository)?;
    write_record_bytes(repository, record, Some(expected))
}

fn write_record_bytes(
    repository: &Repository,
    record: &InstallRecord,
    expected: Option<ReceiptGeneration>,
) -> Result<ReceiptGeneration, InstallError> {
    let bytes = wire::encode(record).map_err(InstallError::Encode)?;
    let (root, _) = record_location(repository, &record.plan.record)?;
    publish_bound(
        &root,
        &record.plan.record.destination,
        &record.plan.record.parent,
        record.plan.record.introduced_parents.len(),
        &FileImage::new(RECORD_MODE, bytes),
        expected,
    )
}

pub(super) fn remove_record(
    repository: &Repository,
    record: &InstallRecord,
    generation: ReceiptGeneration,
) -> Result<(), InstallError> {
    let (root, _) = record_location(repository, &record.plan.record)?;
    let mut admitted = super::handle::AdmittedPath::open(
        &root,
        &record.plan.record.destination,
        &record.plan.record.parent,
        false,
        record.plan.record.introduced_parents.len(),
    )?;
    admitted.remove_receipt(generation)?;
    admitted.cleanup_introduced()?;
    drop(admitted);

    for path in [
        repository.state_root().join(REVERSAL_PATH),
        repository.state_root().join("appliance/install"),
        repository.state_root().join("appliance"),
        repository.state_root(),
    ] {
        remove_empty_directory(&path)?;
    }
    Ok(())
}

fn remove_empty_directory(path: &std::path::Path) -> Result<(), InstallError> {
    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(source)
            if matches!(
                source.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            Ok(())
        }
        Err(source) => Err(InstallError::io(
            "remove empty retired install state directory",
            path,
            source,
        )),
    }
}

pub(super) fn record_path(repository: &Repository) -> Result<PathBuf, InstallError> {
    Ok(repository
        .admit_state()
        .map_err(InstallError::State)?
        .runtime()
        .join(RECORD_PATH))
}
