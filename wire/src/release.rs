//! The one typed external-release plan.
//!
//! The dist planner seals it, the dist executor runs it, the release rim
//! previews it, and its operation journal retains it. Every layer carries this
//! value; none rebuilds it from another shape.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::body::PublishedTarget;
use crate::ident::Digest;
use crate::manifest::ProductVersion;

/// The sole ordered operation vocabulary understood by preview and apply.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Operation {
    /// Re-observe every plan binding and validate publication credentials.
    ResolvePreconditionsAndCredentials,
    /// Run the full correctness gate over the release commit.
    RunCorrectnessGate,
    /// Install the packaged artifacts and probe the installed executable.
    InstallAndProbeArtifacts,
    /// Rehearse the package upload without publishing it.
    RehearseUpload,
    /// Publish the package at its immutable ref.
    UploadPackage,
    /// Wait until the registry names the exact published package.
    AwaitRegistryVisibility,
    /// Create the immutable release tag.
    CreateImmutableTag,
    /// Publish the release announcement.
    Announce,
}

impl Operation {
    /// The operation's journal and preview spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ResolvePreconditionsAndCredentials => "resolve-preconditions-and-credentials",
            Self::RunCorrectnessGate => "run-correctness-gate",
            Self::InstallAndProbeArtifacts => "install-and-probe-artifacts",
            Self::RehearseUpload => "rehearse-upload",
            Self::UploadPackage => "upload-package",
            Self::AwaitRegistryVisibility => "await-registry-visibility",
            Self::CreateImmutableTag => "create-immutable-tag",
            Self::Announce => "announce",
        }
    }
}

impl fmt::Display for Operation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Exact registry observation proving the planned package is visible.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryEvidence {
    /// Canonical semantic version observed at the package authority.
    pub version: ProductVersion,
    /// Exact release commit named by the package authority.
    pub release_commit: String,
    /// Canonical package identity made visible by that commit.
    pub package: Digest,
}

/// Complete no-effect observations resolved before a release plan is sealed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseObservations {
    /// Fully resolved HEAD commit.
    pub head: String,
    /// Whether tracked worktree state is clean.
    pub clean: bool,
    /// Number of commits after the selected release commit.
    pub unrecorded_local_commits: u64,
    /// Product version compiled into the source executable.
    pub source_version: ProductVersion,
    /// Product version read from the selected release tree.
    pub release_version: ProductVersion,
    /// Whether the configured remote accepted a no-effect authenticated read.
    pub credentials_valid: bool,
    /// Unexpected package-ref object, absent when unpublished or converged.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub package_ref: Option<String>,
    /// Unexpected tag object, absent when unpublished or converged.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub tag_ref: Option<String>,
    /// Unexpected announcement object, absent when unpublished or converged.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub announcement_ref: Option<String>,
}

/// One exact, complete external-release plan. Its key binds every field below
/// it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleasePlan {
    /// Digest the apply command must re-present.
    pub key: Digest,
    /// Identity of the repository authority observed by the planner.
    pub repository: Digest,
    /// Canonical semantic version requested by the operator.
    pub requested_version: ProductVersion,
    /// Immutable source commit from which release preparation began.
    pub source_commit: String,
    /// Identity of the one source archive every published target was built from.
    pub source_archive: Digest,
    /// Exact release commit all external refs must name.
    pub release_commit: String,
    /// Canonical identity of the sealed all-target manifest.
    pub manifest: Digest,
    /// Identity of the exact curated release-note bytes approved by this plan.
    pub notes: Digest,
    /// Exact curated bytes, not merely a digest reconstructed at apply time.
    pub notes_document: String,
    /// Canonical identity of the complete installable package closure.
    pub package: Digest,
    /// Exact Git tree object published at the immutable package ref.
    pub package_object: String,
    /// Exact independently installed executable identity the probe must return.
    pub installed_artifact: Digest,
    /// Exact target payload and detached-receipt identities to publish.
    pub targets: Vec<PublishedTarget>,
    /// Sole ordered operation inventory shared by preview and execution.
    pub operations: Vec<Operation>,
    /// Exact no-effect preconditions and authority observations bound by `key`.
    pub observations: ReleaseObservations,
}
