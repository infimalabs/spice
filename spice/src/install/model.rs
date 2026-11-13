use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use wire::{Digest, Domain};

use crate::repository::Repository;

use super::{Generation, InstallError};

/// One asset this build publishes, in canonical installation order.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Asset {
    GitPreCommit,
    GitCommitMessage,
    GitReferenceTransaction,
    GitHookConfig,
    Binary,
    /// Retained dSYM `Info.plist` published only with a symbol-bearing executable.
    SymbolsInfo,
    /// Retained dSYM DWARF image sharing the installed executable `LC_UUID`.
    SymbolsDwarf,
}

/// The asset label a receipt operation carries.
///
/// A receipt is ownership evidence for whatever release wrote it, so a label
/// this build does not publish still decodes and round-trips canonically: a
/// refresh retires its file by the recorded path, location, mode, and image
/// instead of needing a surviving enum variant for it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum AssetTag {
    Known(Asset),
    Unknown(String),
}

impl AssetTag {
    /// The asset this build publishes under the label, if any.
    #[must_use]
    pub const fn known(&self) -> Option<Asset> {
        match self {
            Self::Known(asset) => Some(*asset),
            Self::Unknown(_) => None,
        }
    }
}

impl From<Asset> for AssetTag {
    fn from(asset: Asset) -> Self {
        Self::Known(asset)
    }
}

impl PartialEq<Asset> for AssetTag {
    fn eq(&self, other: &Asset) -> bool {
        self.known() == Some(*other)
    }
}

/// Root against which an operation's relative path resolves.
///
/// An install identifies its repository by common directory alone: one record
/// serves every linked worktree, so appliance assets live exclusively below
/// [`Self::CommonGit`].
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Location {
    CommonGit,
}

impl Location {
    pub(super) fn root(self, repository: &Repository) -> &Path {
        match self {
            Self::CommonGit => repository.common_dir(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RepositoryIdentity {
    pub(super) common_git: PathBuf,
}

impl RepositoryIdentity {
    pub(super) fn capture(repository: &Repository) -> Self {
        Self {
            common_git: repository.common_dir().to_path_buf(),
        }
    }

    pub(super) fn validate(&self, repository: &Repository) -> Result<(), InstallError> {
        if self.common_git == repository.common_dir() {
            Ok(())
        } else {
            Err(InstallError::InvalidRecord(
                "install record belongs to another repository".to_owned(),
            ))
        }
    }
}

/// Complete bytes, mode, and content identity of one regular file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileImage {
    pub(super) mode: u32,
    pub(super) bytes: Vec<u8>,
    pub(super) digest: Digest,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ObservedFile {
    pub(super) image: FileImage,
    pub(super) generation: Generation,
}

impl ObservedFile {
    pub(super) fn generated(&self) -> GeneratedImage {
        self.image.generated()
    }
}

/// Native and content identity of one observed install receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ReceiptGeneration {
    generation: Generation,
    image: GeneratedImage,
}

impl ReceiptGeneration {
    pub(super) fn observed(observed: &ObservedFile) -> Self {
        Self::published(observed.generation, &observed.image)
    }

    pub(super) fn published(generation: Generation, image: &FileImage) -> Self {
        Self {
            generation,
            image: image.generated(),
        }
    }

    pub(super) const fn native(&self) -> Generation {
        self.generation
    }

    pub(super) fn matches(&self, observed: &ObservedFile) -> bool {
        self.generation == observed.generation && self.image == observed.generated()
    }
}

pub(super) fn images_match(
    observed: Option<&ObservedFile>,
    expected: Option<&GeneratedImage>,
) -> bool {
    // Durable drift belongs to the complete file image. A generation is only
    // an immediate compare-and-swap guard for the mutation that follows.
    match (observed, expected) {
        (None, None) => true,
        (Some(observed), Some(expected)) => observed.generated() == *expected,
        _ => false,
    }
}

impl FileImage {
    pub(super) fn new(mode: u32, bytes: Vec<u8>) -> Self {
        let digest = Digest::compute(Domain::InstallArtifact, &bytes);
        Self {
            mode,
            bytes,
            digest,
        }
    }

    /// Installed permission bits.
    #[must_use]
    pub const fn mode(&self) -> u32 {
        self.mode
    }

    /// Exact file bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Content identity of the file bytes.
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }

    pub(super) fn generated(&self) -> GeneratedImage {
        GeneratedImage {
            mode: self.mode,
            digest: self.digest.clone(),
        }
    }
}

/// Mode and content identity of bytes an install operation will publish.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedImage {
    pub(super) mode: u32,
    pub(super) digest: Digest,
}

impl GeneratedImage {
    pub(super) fn new(mode: u32, bytes: &[u8]) -> Self {
        Self {
            mode,
            digest: Digest::compute(Domain::InstallArtifact, bytes),
        }
    }

    /// Installed permission bits.
    #[must_use]
    pub const fn mode(&self) -> u32 {
        self.mode
    }

    /// Identity of the exact bytes selected by the plan.
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }

    pub(super) fn validate(&self) -> bool {
        self.mode <= 0o777
    }
}

/// One exact file replacement and the state it observed before installation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub(super) asset: AssetTag,
    pub(super) location: Location,
    pub(super) path: String,
    pub(super) destination: String,
    pub(super) parent: super::ParentObservation,
    /// Fixed-size identity of the pre-install image restored by reversal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) previous: Option<GeneratedImage>,
    pub(super) generated: GeneratedImage,
    /// Installed image this operation replaces, when it refreshes a completed
    /// install rather than introducing one.
    ///
    /// `previous` never moves: it stays the pre-install state a reversal
    /// restores, so refreshing repeatedly still uninstalls to the tree the
    /// first apply found. What a refresh needs beyond that is the image it
    /// expects to *find* in place, which is the superseded build's output
    /// rather than `previous`. Absent for a first install, and skipped from the
    /// canonical bytes then, so plans that introduce an install keep the digest
    /// they had before refreshing existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) superseded: Option<GeneratedImage>,
    /// Current binding used to publish a refresh after the originally absent
    /// parent hierarchy has been created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) publication_parent: Option<super::ParentObservation>,
    pub(super) introduced_parents: Vec<String>,
}

/// One receipt-owned path removed after every live Spice asset exists.
///
/// A refresh derives retirements from the receipt it supersedes: every
/// recorded operation whose path the new plan no longer publishes, and every
/// retirement that still carries a reversal image. They stay separate from
/// [`Operation`] because they publish nothing and never re-enter the
/// inventory. A reversible retirement keeps the exact prior image so reversal
/// restores the preceding installation; a discard is never restored.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Retirement {
    pub(super) path: String,
    pub(super) destination: String,
    pub(super) parent: super::ParentObservation,
    /// Fixed-size identity of the image restored by reversal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) previous: Option<GeneratedImage>,
    /// Fixed-size identity the initial retirement may replace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) observed: Option<GeneratedImage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) superseded: Option<GeneratedImage>,
    /// Current binding used to apply a refresh after the originally absent
    /// parent hierarchy has been created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) publication_parent: Option<super::ParentObservation>,
    /// Parent directories introduced by the original install and still owned
    /// by this reversal after its publishing operation was retired.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) introduced_parents: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(super) discard: bool,
}

const fn is_false(value: &bool) -> bool {
    !*value
}

/// A recorded file a refresh leaves in place because its content no longer
/// matches the image the receipt recorded for it.
///
/// The receipt is evidence only for the image it recorded, so the refresh
/// deletes nothing here: the file leaves receipt ownership and the plan names
/// both digests so the person who changed it can decide.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Orphan {
    pub(super) path: String,
    pub(super) recorded: GeneratedImage,
    pub(super) observed: GeneratedImage,
}

impl Orphan {
    /// Repository-relative path left in place.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Image the receipt recorded for the path.
    #[must_use]
    pub const fn recorded(&self) -> &GeneratedImage {
        &self.recorded
    }

    /// Image observed when the plan was derived.
    #[must_use]
    pub const fn observed(&self) -> &GeneratedImage {
        &self.observed
    }
}

impl Retirement {
    pub(super) fn validate_binding(&self, repository: &Repository) -> Result<(), InstallError> {
        super::binding::validate_operation_binding(
            repository,
            Location::CommonGit,
            &self.path,
            &self.destination,
            &self.parent,
        )
    }

    /// Retired repository-relative path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Exact prior image identity restored by reversal, if one existed.
    #[must_use]
    pub const fn previous(&self) -> Option<&GeneratedImage> {
        self.previous.as_ref()
    }

    /// Parent directories withdrawn after this reversal, outermost first.
    #[must_use]
    pub fn introduced_parents(&self) -> &[String] {
        &self.introduced_parents
    }

    pub(super) fn publication_parent(&self) -> &super::ParentObservation {
        self.publication_parent.as_ref().unwrap_or(&self.parent)
    }
    pub(super) fn destination(&self, repository: &Repository) -> PathBuf {
        repository.common_dir().join(&self.destination)
    }

    pub(super) fn expects_in_place(&self, observed: Option<&ObservedFile>) -> bool {
        self.superseded.as_ref().map_or_else(
            || images_match(observed, self.observed.as_ref()),
            |superseded| observed.is_some_and(|observed| &observed.generated() == superseded),
        )
    }

    pub(super) const fn discards_payload(&self) -> bool {
        self.discard
    }

    /// Whether this discard is the last of its planned run over one binding
    /// that names missing parent directories, which it withdraws once empty.
    pub(super) fn withdraws_parent(&self, next: Option<&Self>) -> bool {
        self.discard
            && !self.parent.missing.is_empty()
            && next.is_none_or(|next| next.parent != self.parent)
    }
}

impl Operation {
    pub(super) fn validate_binding(&self, repository: &Repository) -> Result<(), InstallError> {
        super::binding::validate_operation_binding(
            repository,
            self.location,
            &self.path,
            &self.destination,
            &self.parent,
        )
    }

    /// Label of the asset this operation owns.
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
    /// Prior regular-file image identity, or absence when this install introduces it.
    #[must_use]
    pub const fn previous(&self) -> Option<&GeneratedImage> {
        self.previous.as_ref()
    }

    /// Exact image this install places.
    #[must_use]
    pub const fn generated(&self) -> &GeneratedImage {
        &self.generated
    }

    /// Installed image a refresh replaces, absent when this install introduces it.
    #[must_use]
    pub const fn superseded(&self) -> Option<&GeneratedImage> {
        self.superseded.as_ref()
    }

    /// Whether `observed` is the exact state this operation may publish over.
    ///
    /// A first install may publish only over the image it planned against. A
    /// refresh may publish only over the build it supersedes, because the
    /// destination already holds an installed image rather than `previous`.
    pub(super) fn expects_in_place(&self, observed: Option<&ObservedFile>) -> bool {
        match &self.superseded {
            Some(superseded) => {
                observed.is_some_and(|observed| &observed.generated() == superseded)
            }
            None => images_match(observed, self.previous.as_ref()),
        }
    }

    /// Parent directories absent when the plan was derived, outermost first.
    #[must_use]
    pub fn introduced_parents(&self) -> &[String] {
        &self.introduced_parents
    }

    pub(super) fn publication_parent(&self) -> &super::ParentObservation {
        self.publication_parent.as_ref().unwrap_or(&self.parent)
    }

    pub(super) fn publication_introduced(&self) -> usize {
        if self.publication_parent.is_some() {
            0
        } else {
            self.introduced_parents.len()
        }
    }

    pub(super) fn destination(&self, repository: &Repository) -> PathBuf {
        self.location.root(repository).join(&self.destination)
    }
}
