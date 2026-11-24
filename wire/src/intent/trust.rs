//! The recorded configuration one trust digest names, as the operator reviews
//! it before ruling.

use serde::{Deserialize, Serialize};

use crate::{ConfigSource, Digest, TrustState};

/// Successful answer to `trust::show`: one digest's recorded documents as a
/// line differential against the most recently granted other digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustShowAnswer {
    /// The digest read.
    pub digest: Digest,
    /// Its current standing.
    pub standing: TrustState,
    /// The most recently granted other digest the documents are compared
    /// against; absent when no other digest is granted.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub baseline: Option<TrustBaseline>,
    /// Each configuration document's differential, tracked first; absent when
    /// the digest's pend recorded no document text.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub documents: Option<Vec<TrustDocumentDiff>>,
}

/// The granted digest a differential is taken against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustBaseline {
    /// The most recently granted other digest.
    pub digest: Digest,
    /// Whether its pend recorded document text; when it did not, every
    /// differential line is an addition.
    pub recorded: bool,
}

/// One configuration document's line differential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustDocumentDiff {
    /// Which document.
    pub source: ConfigSource,
    /// Whether the reviewed digest includes this document.
    pub present: bool,
    /// Changed lines with bounded unchanged context, in document order.
    pub lines: Vec<TrustDiffLine>,
}

/// One line of a document differential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TrustDiffLine {
    /// Unchanged context.
    Kept {
        /// The line text without its terminator.
        text: String,
    },
    /// A line only the reviewed digest holds.
    Added {
        /// The line text without its terminator.
        text: String,
    },
    /// A line only the baseline holds.
    Removed {
        /// The line text without its terminator.
        text: String,
    },
    /// Unchanged lines omitted between context.
    Elided {
        /// How many lines.
        count: u64,
    },
}
