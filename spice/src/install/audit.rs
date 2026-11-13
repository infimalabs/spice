use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use wire::Digest;

use super::record::{RefreshRecord, read_record_for_refresh};
use super::{
    Asset, BINARY_PATH, GIT_COMMIT_MESSAGE_PATH, GIT_HOOK_CONFIG_PATH, GIT_PRE_COMMIT_PATH,
    GIT_REFERENCE_TRANSACTION_PATH, InstallError, InstallStatus, Repository, current_build_differs,
    installed_binary, verify_progress,
};

/// Deterministic relationship between one repository and its installed appliance.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApplianceState {
    Current,
    MissingReceipt,
    StaleReceipt,
    RepairableReceipt,
    MissingBinary,
    BinaryTampered,
    HookTampered,
    AssetTampered,
    AnotherBuild,
}

impl ApplianceState {
    /// Stable diagnostic spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::MissingReceipt => "missing-receipt",
            Self::StaleReceipt => "stale-receipt",
            Self::RepairableReceipt => "repairable-receipt",
            Self::MissingBinary => "missing-binary",
            Self::BinaryTampered => "binary-tampered",
            Self::HookTampered => "hook-tampered",
            Self::AssetTampered => "asset-tampered",
            Self::AnotherBuild => "another-build",
        }
    }
}

/// Side-effect-free installed-appliance freshness evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApplianceAudit {
    state: ApplianceState,
    detail: String,
    repair: Option<String>,
    receipt: Option<Digest>,
    installed_binary: PathBuf,
    current_binary: PathBuf,
}

impl ApplianceAudit {
    /// Classified appliance state.
    #[must_use]
    pub const fn state(&self) -> ApplianceState {
        self.state
    }

    /// Whether the installed bytes and receipt match the current build exactly.
    #[must_use]
    pub const fn is_current(&self) -> bool {
        matches!(self.state, ApplianceState::Current)
    }

    /// Exact diagnostic evidence.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// Current-build command that previews the digest-bound repair.
    #[must_use]
    pub fn repair(&self) -> Option<&str> {
        self.repair.as_deref()
    }

    /// Installed plan identity, when one valid receipt supplied it.
    #[must_use]
    pub const fn receipt(&self) -> Option<&Digest> {
        self.receipt.as_ref()
    }

    /// Receipt-owned installed executable path.
    #[must_use]
    pub fn installed_binary(&self) -> &Path {
        &self.installed_binary
    }

    /// Freshly built executable used as the comparison reference.
    #[must_use]
    pub fn current_binary(&self) -> &Path {
        &self.current_binary
    }
}

/// Audit the receipt-owned appliance against one freshly built Spice executable.
///
/// This performs no mutation and launches no subprocess. Callers that need a
/// current-source answer must build `current_binary` before entering this audit;
/// the reference-transaction hook never calls it.
pub fn audit_appliance(
    repository: &Repository,
    current_binary: &Path,
) -> Result<ApplianceAudit, InstallError> {
    let installed = installed_binary(repository);
    let preview = preview_command(current_binary);
    let loaded = match load_audit_record(repository, current_binary, &installed, &preview)? {
        Ok(loaded) => loaded,
        Err(audit) => return Ok(audit),
    };
    let receipt = loaded.receipt;
    let repairable = loaded.refresh.is_repairable();
    let record = &loaded.refresh.loaded().record;
    if let Some(audit) = receipt_state_audit(current_binary, record, &installed, &preview, &receipt)
    {
        return Ok(audit);
    }
    if let Some(audit) = progress_audit(
        repository,
        current_binary,
        record,
        &installed,
        &preview,
        &receipt,
    )? {
        return Ok(audit);
    }
    if repairable {
        super::progress::prepare_over(repository, Some(current_binary), loaded.refresh, None)?;
        return Ok(report(
            ApplianceState::RepairableReceipt,
            format!(
                "installed-appliance receipt {receipt} can be forward repaired by an exact current-build install plan"
            ),
            Some(preview),
            Some(receipt),
            installed,
            current_binary,
        ));
    }
    let Some(binary) = record
        .plan
        .operations
        .iter()
        .find(|operation| operation.asset == Asset::Binary)
    else {
        return Ok(report(
            ApplianceState::MissingBinary,
            "installed-appliance receipt owns no Spice binary".to_owned(),
            Some(preview),
            Some(receipt),
            installed,
            current_binary,
        ));
    };
    if current_binary == installed {
        return Ok(report(
            ApplianceState::Current,
            format!("installed appliance matches receipt {receipt} and the current build"),
            None,
            Some(receipt),
            installed,
            current_binary,
        ));
    }
    if current_build_differs(repository, &record.plan, Some(current_binary))? {
        return Ok(report(
            ApplianceState::AnotherBuild,
            format!(
                "installed appliance generation differs from current build; installed binary digest {}",
                binary.generated.digest,
            ),
            Some(preview),
            Some(receipt),
            installed,
            current_binary,
        ));
    }
    Ok(report(
        ApplianceState::Current,
        format!("installed appliance matches receipt {receipt} and the current build"),
        None,
        Some(receipt),
        installed,
        current_binary,
    ))
}

fn receipt_state_audit(
    current_binary: &Path,
    record: &super::record::InstallRecord,
    installed: &Path,
    preview: &str,
    receipt: &Digest,
) -> Option<ApplianceAudit> {
    let detail = if record.status != InstallStatus::Complete {
        format!("installed-appliance receipt is {:?}", record.status)
    } else {
        return None;
    };
    Some(report(
        ApplianceState::StaleReceipt,
        detail,
        Some(preview.to_owned()),
        Some(receipt.clone()),
        installed.to_path_buf(),
        current_binary,
    ))
}

fn load_audit_record(
    repository: &Repository,
    current_binary: &Path,
    installed: &Path,
    preview: &str,
) -> Result<Result<AuditLoadedRecord, ApplianceAudit>, InstallError> {
    match read_record_for_refresh(repository) {
        Ok(Some(refresh)) => {
            let receipt = refresh.receipt_digest().clone();
            Ok(Ok(AuditLoadedRecord { refresh, receipt }))
        }
        Ok(None) => Ok(Err(report(
            ApplianceState::MissingReceipt,
            "no installed-appliance receipt exists".to_owned(),
            Some(preview.to_owned()),
            None,
            installed.to_path_buf(),
            current_binary,
        ))),
        Err(error) if stale_receipt(&error) => Ok(Err(report(
            ApplianceState::StaleReceipt,
            error.to_string(),
            Some(preview.to_owned()),
            None,
            installed.to_path_buf(),
            current_binary,
        ))),
        Err(error) => Err(error),
    }
}

struct AuditLoadedRecord {
    refresh: RefreshRecord,
    receipt: Digest,
}

fn progress_audit(
    repository: &Repository,
    current_binary: &Path,
    record: &super::record::InstallRecord,
    installed: &Path,
    preview: &str,
    receipt: &Digest,
) -> Result<Option<ApplianceAudit>, InstallError> {
    let Err(error) = verify_progress(repository, record) else {
        return Ok(None);
    };
    let state = match &error {
        InstallError::Drift { path, .. } => drift_state(repository, path)?,
        _ if stale_receipt(&error) => ApplianceState::StaleReceipt,
        _ => return Err(error),
    };
    Ok(Some(report(
        state,
        error.to_string(),
        Some(preview.to_owned()),
        Some(receipt.clone()),
        installed.to_path_buf(),
        current_binary,
    )))
}

fn stale_receipt(error: &InstallError) -> bool {
    matches!(
        error,
        InstallError::Decode(_)
            | InstallError::InvalidRecord(_)
            | InstallError::PathShape(_)
            | InstallError::PathOutsideRoot(_)
            | InstallError::ParentChanged(_)
    )
}

fn drift_state(repository: &Repository, path: &str) -> Result<ApplianceState, InstallError> {
    if path == BINARY_PATH {
        return match fs::symlink_metadata(installed_binary(repository)) {
            Ok(_) => Ok(ApplianceState::BinaryTampered),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Ok(ApplianceState::MissingBinary)
            }
            Err(source) => Err(InstallError::io(
                "inspect installed Spice executable",
                installed_binary(repository),
                source,
            )),
        };
    }
    if matches!(
        path,
        GIT_PRE_COMMIT_PATH
            | GIT_COMMIT_MESSAGE_PATH
            | GIT_REFERENCE_TRANSACTION_PATH
            | GIT_HOOK_CONFIG_PATH
    ) {
        Ok(ApplianceState::HookTampered)
    } else {
        Ok(ApplianceState::AssetTampered)
    }
}

fn preview_command(current_binary: &Path) -> String {
    current_binary.to_str().map_or_else(
        || format!("run {current_binary:?} with the `install` argument"),
        |binary| shell_words::join([binary, "install"]),
    )
}

fn report(
    state: ApplianceState,
    detail: String,
    repair: Option<String>,
    receipt: Option<Digest>,
    installed_binary: PathBuf,
    current_binary: &Path,
) -> ApplianceAudit {
    ApplianceAudit {
        state,
        detail,
        repair,
        receipt,
        installed_binary,
        current_binary: current_binary.to_path_buf(),
    }
}
