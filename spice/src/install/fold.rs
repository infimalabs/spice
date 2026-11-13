//! Bounded public folds of private installation authority.

use serde::{Deserialize, Serialize};
use std::fmt;

use wire::Digest;

use super::{
    AssetTag, GeneratedImage, InstallError, InstallPlan, LifecycleOwnerStanding, Location, Orphan,
    PreparedInstall, SymbolProvenance,
};

/// Kind of durable action represented by one ordered public stage.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallStageKind {
    FilePublication,
    Retirement,
}

/// One byte-free file-publication stage in canonical execution order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstallOperationSummary {
    index: usize,
    kind: InstallStageKind,
    asset: AssetTag,
    location: Location,
    path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous: Option<GeneratedImage>,
    generated: GeneratedImage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    superseded: Option<GeneratedImage>,
    introduced_parents: Vec<String>,
}

impl InstallOperationSummary {
    /// Zero-based position among all durable stages.
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// Stable action discriminator for this stage.
    #[must_use]
    pub const fn kind(&self) -> InstallStageKind {
        self.kind
    }

    /// Label of the asset this stage publishes.
    #[must_use]
    pub const fn asset(&self) -> &AssetTag {
        &self.asset
    }

    /// Root against which [`Self::path`] resolves.
    #[must_use]
    pub const fn location(&self) -> Location {
        self.location
    }

    /// Repository-relative destination spelling.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Mode and digest restored by unapply, absent for an introduced file.
    #[must_use]
    pub const fn previous(&self) -> Option<&GeneratedImage> {
        self.previous.as_ref()
    }

    /// Mode and digest published by this stage.
    #[must_use]
    pub const fn generated(&self) -> &GeneratedImage {
        &self.generated
    }

    /// Mode and digest replaced by a refresh.
    #[must_use]
    pub const fn superseded(&self) -> Option<&GeneratedImage> {
        self.superseded.as_ref()
    }

    /// Parent directories introduced by this stage, outermost first.
    #[must_use]
    pub fn introduced_parents(&self) -> &[String] {
        &self.introduced_parents
    }

    fn changes_file(&self) -> bool {
        self.superseded.as_ref().or(self.previous.as_ref()) != Some(&self.generated)
    }
}

/// One byte-free retirement stage in canonical execution order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstallRetirementSummary {
    index: usize,
    kind: InstallStageKind,
    path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous: Option<GeneratedImage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed: Option<GeneratedImage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    superseded: Option<GeneratedImage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    introduced_parents: Vec<String>,
}

impl InstallRetirementSummary {
    /// Zero-based position among all durable stages.
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// Stable action discriminator for this stage.
    #[must_use]
    pub const fn kind(&self) -> InstallStageKind {
        self.kind
    }

    /// Repository-common relative path removed by this stage.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Mode and digest restored by unapply, when one existed.
    #[must_use]
    pub const fn previous(&self) -> Option<&GeneratedImage> {
        self.previous.as_ref()
    }

    /// Mode and digest observed before retirement.
    #[must_use]
    pub const fn observed(&self) -> Option<&GeneratedImage> {
        self.observed.as_ref()
    }

    /// Mode and digest replaced by a refresh.
    #[must_use]
    pub const fn superseded(&self) -> Option<&GeneratedImage> {
        self.superseded.as_ref()
    }

    /// Parent directories withdrawn after this reversal, outermost first.
    #[must_use]
    pub fn introduced_parents(&self) -> &[String] {
        &self.introduced_parents
    }

    fn removes_file(&self) -> bool {
        self.superseded.is_some() || self.observed.is_some()
    }
}

/// Byte-free public description of one canonical private install plan.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstallPlanSummary {
    version: u16,
    operation_count: usize,
    operations: Vec<InstallOperationSummary>,
    retirements: Vec<InstallRetirementSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    orphans: Vec<Orphan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    symbols: Option<SymbolProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lifecycle_owner: Option<LifecycleOwnerStanding>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    lifecycle_change: bool,
}

impl InstallPlanSummary {
    pub(super) fn from_plan(plan: &InstallPlan) -> Self {
        let operations = plan
            .operations
            .iter()
            .enumerate()
            .map(|(index, operation)| InstallOperationSummary {
                index,
                kind: InstallStageKind::FilePublication,
                asset: operation.asset.clone(),
                location: operation.location,
                path: operation.path.clone(),
                previous: operation.previous.clone(),
                generated: operation.generated.clone(),
                superseded: operation.superseded.clone(),
                introduced_parents: operation.introduced_parents.clone(),
            })
            .collect();
        let retirement_offset = plan.operations.len();
        let retirements = plan
            .retirements
            .iter()
            .enumerate()
            .map(|(offset, retirement)| InstallRetirementSummary {
                index: retirement_offset + offset,
                kind: InstallStageKind::Retirement,
                path: retirement.path.clone(),
                previous: retirement.previous.clone(),
                observed: retirement.observed.clone(),
                superseded: retirement.superseded.clone(),
                introduced_parents: retirement.introduced_parents.clone(),
            })
            .collect();
        let lifecycle_owner = plan.lifecycle_owner.clone();
        Self {
            version: plan.version,
            operation_count: plan.operation_count() + usize::from(lifecycle_owner.is_some()),
            operations,
            retirements,
            orphans: plan.orphans.clone(),
            symbols: plan.symbols.clone(),
            lifecycle_change: lifecycle_owner.is_some(),
            lifecycle_owner,
        }
    }

    /// Private canonical plan schema version represented by this summary.
    #[must_use]
    pub const fn version(&self) -> u16 {
        self.version
    }

    /// Number of durable publication and retirement stages.
    #[must_use]
    pub const fn operation_count(&self) -> usize {
        self.operation_count
    }

    /// File publications in their canonical execution order.
    #[must_use]
    pub fn operations(&self) -> &[InstallOperationSummary] {
        &self.operations
    }

    /// Retirements immediately following file publication.
    #[must_use]
    pub fn retirements(&self) -> &[InstallRetirementSummary] {
        &self.retirements
    }

    /// Recorded files the plan leaves in place and stops owning because their
    /// content drifted from the image the superseded receipt recorded.
    #[must_use]
    pub fn orphans(&self) -> &[Orphan] {
        &self.orphans
    }

    /// Windows lifecycle policy this package preserves.
    #[must_use]
    pub const fn lifecycle_owner(&self) -> Option<&LifecycleOwnerStanding> {
        self.lifecycle_owner.as_ref()
    }

    /// Provenance of the executable and retained dSYM, when symbols publish.
    #[must_use]
    pub const fn symbols(&self) -> Option<&SymbolProvenance> {
        self.symbols.as_ref()
    }

    /// Restate a complete install as the plan apply would execute over it:
    /// every file already holds what it would publish and nothing remains to
    /// retire.
    fn settle(&mut self) {
        for operation in &mut self.operations {
            operation.superseded = Some(operation.generated.clone());
        }
        for retirement in &mut self.retirements {
            retirement.observed = None;
            retirement.superseded = None;
        }
        self.lifecycle_change = false;
    }
}

/// One side-effect-free install preview and the exact identity admitted by apply.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstallPreview {
    digest: Digest,
    plan: InstallPlanSummary,
}

impl InstallPreview {
    /// Preview the plan apply would execute now, bound to the identity apply
    /// admits. Over a complete install of this same build apply publishes
    /// nothing, so its stages read as already in place.
    pub fn from_prepared(prepared: PreparedInstall) -> Result<Self, InstallError> {
        let converged = prepared.converged();
        let plan = prepared.into_plan();
        plan.validate()?;
        let digest = plan.digest()?;
        let mut plan = InstallPlanSummary::from_plan(&plan);
        if converged {
            plan.settle();
        }
        Ok(Self { digest, plan })
    }

    /// Exact canonical plan identity accepted by `install --apply`.
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }

    /// Bounded public metadata for the private canonical installation plan.
    #[must_use]
    pub const fn plan(&self) -> &InstallPlanSummary {
        &self.plan
    }

    /// Write the person-facing preview, ending with the exact command that
    /// applies this digest from where the preview came from.
    pub fn write_text(
        &self,
        formatter: &mut fmt::Formatter<'_>,
        apply: &dyn fmt::Display,
    ) -> fmt::Result {
        let files = self
            .plan
            .operations
            .iter()
            .filter(|operation| operation.changes_file())
            .count();
        let retirements = self
            .plan
            .retirements
            .iter()
            .filter(|retirement| retirement.removes_file())
            .count();
        let lifecycle = usize::from(self.plan.lifecycle_change);

        writeln!(formatter, "install plan {}", self.digest)?;
        if files == 0 && retirements == 0 && lifecycle == 0 {
            writeln!(formatter, "changes: none")?;
        } else {
            writeln!(
                formatter,
                "changes: {files} {}, {retirements} {}, {lifecycle} {}",
                plural(files, "file", "files"),
                plural(retirements, "retirement", "retirements"),
                plural(lifecycle, "lifecycle owner", "lifecycle owners"),
            )?;
        }
        for operation in self
            .plan
            .operations
            .iter()
            .filter(|operation| operation.changes_file())
        {
            let action = if operation.superseded.is_some() || operation.previous.is_some() {
                "update"
            } else {
                "create"
            };
            writeln!(formatter, "  {action} {}", operation.path)?;
        }
        for retirement in self
            .plan
            .retirements
            .iter()
            .filter(|retirement| retirement.removes_file())
        {
            writeln!(formatter, "  remove {}", retirement.path)?;
        }
        if self.plan.lifecycle_change {
            writeln!(
                formatter,
                "  ensure Windows lifecycle owner while preserving its private policy",
            )?;
        }
        write_orphans(formatter, &self.plan.orphans)?;
        write!(formatter, "apply: {apply}")
    }
}

/// Name each file left in place with the image the receipt recorded and the
/// one found, so the person who changed it can decide what becomes of it.
fn write_orphans(formatter: &mut fmt::Formatter<'_>, orphans: &[Orphan]) -> fmt::Result {
    for orphan in orphans {
        writeln!(
            formatter,
            "  leave {}: content differs from the recorded image {} (observed {}); \
             it stays in place and is no longer owned by the install",
            orphan.path,
            orphan.recorded.digest(),
            orphan.observed.digest(),
        )?;
    }
    Ok(())
}

const fn plural<'a>(count: usize, singular: &'a str, plural: &'a str) -> &'a str {
    if count == 1 { singular } else { plural }
}

/// Durable progress state for apply and reversal.
///
/// `Establishing` is the Windows boundary between published files and a
/// serving lifecycle owner: every file, retirement, and hook stage is
/// complete, and only the owner the plan names remains to be registered and
/// started under the installed executable.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallStatus {
    Applying,
    Establishing,
    Complete,
    Unapplying,
    Uninstalled,
}

/// Current strict installation receipt projected without private reversal bytes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstallReceipt {
    pub(super) plan: InstallPlanSummary,
    pub(super) digest: Digest,
    pub(super) status: InstallStatus,
    pub(super) completed: usize,
}

impl InstallReceipt {
    /// Bounded public metadata for the private plan retained by the record.
    #[must_use]
    pub const fn plan(&self) -> &InstallPlanSummary {
        &self.plan
    }

    /// Plan identity admitted by apply.
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }

    /// Durable progress state.
    #[must_use]
    pub const fn status(&self) -> InstallStatus {
        self.status
    }

    /// Operations durably acknowledged in the current direction.
    #[must_use]
    pub const fn completed(&self) -> usize {
        self.completed
    }
}

impl fmt::Display for InstallReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match self.status {
            InstallStatus::Applying => "installation applying",
            InstallStatus::Establishing => "installation published; lifecycle owner pending",
            InstallStatus::Complete => "installation complete",
            InstallStatus::Unapplying => "installation removal in progress",
            InstallStatus::Uninstalled => "installation removed",
        };
        writeln!(formatter, "{state}")?;
        writeln!(formatter, "plan: {}", self.digest)?;
        write_orphans(formatter, &self.plan.orphans)?;
        write!(
            formatter,
            "progress: {}/{} stages",
            self.completed, self.plan.operation_count
        )?;
        match self.status {
            InstallStatus::Establishing => {
                write!(formatter, "\napply: spice install --apply={}", self.digest)
            }
            InstallStatus::Complete => write!(formatter, "\nunapply: spice install --unapply"),
            InstallStatus::Uninstalled => write!(formatter, "\npreview: spice install"),
            InstallStatus::Applying | InstallStatus::Unapplying => Ok(()),
        }
    }
}
