use std::fmt;
use std::str::FromStr;

use serde::de::{Deserializer, Error as _};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use wire::output::{Export, Omission};
use wire::{Digest, Instant, SpiceCommand, TaskHandle, TaskStamp};

pub(crate) const STORE_VERSION: u16 = 1;
pub(crate) const MAX_ARTIFACTS_PER_TASK: usize = 32;

/// Retention policy attached to one artifact citation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Retention {
    Permanent,
    Prunable,
}

impl fmt::Display for Retention {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Permanent => "permanent",
            Self::Prunable => "prunable",
        })
    }
}

impl FromStr for Retention {
    type Err = VocabularyError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "permanent" => Ok(Self::Permanent),
            "prunable" => Ok(Self::Prunable),
            _ => Err(VocabularyError::Retention(text.to_owned())),
        }
    }
}

/// One admitted task artifact media type.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MediaType(String);

impl MediaType {
    pub const TEXT_PLAIN: &'static str = "text/plain";
    pub const TEXT_MARKDOWN: &'static str = "text/markdown";
    pub const APPLICATION_JSON: &'static str = "application/json";
    pub const TEXT_CSV: &'static str = "text/csv";
    pub const TEXT_TSV: &'static str = "text/tab-separated-values";
    pub const IMAGE_PNG: &'static str = "image/png";
    pub const IMAGE_JPEG: &'static str = "image/jpeg";
    pub const IMAGE_GIF: &'static str = "image/gif";
    pub const IMAGE_WEBP: &'static str = "image/webp";
    pub const APPLICATION_PDF: &'static str = "application/pdf";

    pub const ALL: &'static [&'static str] = &[
        Self::APPLICATION_JSON,
        Self::APPLICATION_PDF,
        Self::IMAGE_JPEG,
        Self::IMAGE_GIF,
        Self::IMAGE_PNG,
        Self::IMAGE_WEBP,
        Self::TEXT_CSV,
        Self::TEXT_MARKDOWN,
        Self::TEXT_PLAIN,
        Self::TEXT_TSV,
    ];

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_text(&self) -> bool {
        matches!(
            self.0.as_str(),
            Self::TEXT_PLAIN
                | Self::TEXT_MARKDOWN
                | Self::APPLICATION_JSON
                | Self::TEXT_CSV
                | Self::TEXT_TSV
        )
    }

    /// Whether this type has a browser-renderable image representation.
    #[must_use]
    pub fn is_image(&self) -> bool {
        matches!(
            self.0.as_str(),
            Self::IMAGE_PNG | Self::IMAGE_JPEG | Self::IMAGE_GIF | Self::IMAGE_WEBP
        )
    }
}

impl fmt::Display for MediaType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for MediaType {
    type Err = VocabularyError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let value = text.to_ascii_lowercase();
        if Self::ALL.contains(&value.as_str()) {
            Ok(Self(value))
        } else {
            Err(VocabularyError::MediaType(text.to_owned()))
        }
    }
}

impl Serialize for MediaType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for MediaType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

/// Stable metadata and provenance for one task artifact citation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub id: String,
    pub name: String,
    pub digest: Digest,
    pub media_type: MediaType,
    pub bytes: u64,
    pub created_at: Instant,
    pub source: String,
    pub retention: Retention,
}

/// What a show returns of a payload whose integrity the store proved.
///
/// No representation carries raw payload bytes: text is a bounded UTF-8
/// window, bytes are hex, and a bare show of a non-text payload names only
/// where the store keeps it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "representation")]
pub enum ArtifactContent {
    /// UTF-8 text at payload bytes `offset..offset + text.len()`; `remaining`
    /// payload bytes follow it.
    Text {
        offset: u64,
        text: String,
        remaining: u64,
    },
    /// Payload bytes `offset..offset + hex.len() / 2`, hex-encoded; `remaining`
    /// payload bytes follow them.
    Bytes {
        offset: u64,
        hex: String,
        remaining: u64,
    },
    /// A non-text payload shown without an explicit window: its store path.
    Binary { path: String },
}

/// One result shape for all task artifact command leaves.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "answer")]
pub enum ArtifactAnswer {
    Added {
        task: TaskHandle,
        artifact: Artifact,
        budget: ArtifactBudget,
    },
    /// Citations newest first, and what the page withheld.
    Listed {
        task: TaskHandle,
        artifacts: Vec<Artifact>,
        budget: ArtifactBudget,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        omission: Option<Omission>,
    },
    /// The removed citation and the budget it left.
    Removed {
        task: TaskHandle,
        artifact: Artifact,
        budget: ArtifactBudget,
    },
    /// Metadata and one bounded window; `next` reads the window after it.
    Shown {
        task: TaskHandle,
        artifact: Artifact,
        content: ArtifactContent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next: Option<SpiceCommand>,
    },
    /// The exact payload written to a named file and verified against the
    /// artifact's retained digest after the write.
    Extracted {
        task: TaskHandle,
        artifact: Artifact,
        export: Export,
    },
    Pruned {
        plan: PrunePlan,
        applied: bool,
    },
}

/// How many citations one task holds against the per-task bound.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactBudget {
    pub count: u64,
    pub bound: u64,
    pub remaining: u64,
}

impl ArtifactBudget {
    /// The budget a task holding `count` citations has left.
    #[must_use]
    pub fn of(count: usize) -> Self {
        let wide = |value: usize| u64::try_from(value).expect("artifact counts fit u64");
        Self {
            count: wide(count),
            bound: wide(MAX_ARTIFACTS_PER_TASK),
            remaining: wide(MAX_ARTIFACTS_PER_TASK.saturating_sub(count)),
        }
    }
}

/// One deterministic removal in a retention preview.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PruneOperation {
    pub order: u64,
    pub born: TaskStamp,
    pub handle: TaskHandle,
    pub artifact_id: String,
    pub name: String,
}

/// One artifact-bearing task excluded by current workflow state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PruneSkip {
    pub order: u64,
    pub born: TaskStamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<TaskHandle>,
    pub status: String,
}

/// Ordered, side-effect-free retention decision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrunePlan {
    pub schema_version: u16,
    pub artifact_root_exists: bool,
    pub older_than_seconds: Option<u64>,
    pub operations: Vec<PruneOperation>,
    pub skipped: Vec<PruneSkip>,
    #[serde(skip)]
    pub(crate) changes: Vec<PruneChange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PruneChange {
    pub task: TaskStamp,
    pub original: Vec<Artifact>,
    pub remaining: Vec<Artifact>,
    /// The remaining manifest's issued high-water mark.
    pub issued: Option<u64>,
    pub remove: Vec<Artifact>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    pub version: u16,
    pub task: TaskStamp,
    pub artifacts: Vec<Artifact>,
    /// Highest artifact number ever issued, present only while it exceeds
    /// the last remaining citation's, so a removed id is never issued again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued: Option<u64>,
}

impl Manifest {
    pub(crate) fn empty(task: TaskStamp) -> Self {
        Self {
            version: STORE_VERSION,
            task,
            artifacts: Vec::new(),
            issued: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ObjectMetadata {
    pub version: u16,
    pub digest: Digest,
    pub bytes: u64,
}

/// A caller-supplied artifact vocabulary value was not admitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VocabularyError {
    Retention(String),
    MediaType(String),
}

impl fmt::Display for VocabularyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Retention(value) => write!(
                formatter,
                "artifact retention must be permanent or prunable: {value:?}"
            ),
            Self::MediaType(value) => write!(
                formatter,
                "unsupported artifact media type {value:?}; admitted values are {}",
                MediaType::ALL.join(", ")
            ),
        }
    }
}

impl std::error::Error for VocabularyError {}
