//! Transcript positions, usage samples, and accounted journal batches.

use serde::{Deserialize, Serialize};

use crate::{CanonicalUuid, Name};

/// Position of the next complete record in one transcript source generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptPosition {
    /// Member holding the next record's first byte.
    pub member: u64,
    /// Offset of that byte within the member.
    pub offset: u64,
}

/// One durable byte span a transcript record was decoded from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptSpan {
    /// Member the bytes live in.
    pub member: u64,
    /// First byte within the member.
    pub offset: u64,
    /// Number of bytes.
    pub length: u64,
}

/// One normalized usage sample beside the exact bytes it was decoded from.
///
/// Every counter is required: a batch that omits one is refused rather than
/// accounted as zero, so an unreported counter can never be spent as free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptUsage {
    /// Completed vendor turns this sample represents.
    pub turns: u64,
    /// Uncached input tokens.
    pub input_tokens: u64,
    /// Cached input tokens.
    pub cached_input_tokens: u64,
    /// Input tokens written to a cache.
    pub cache_write_input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Reasoning output tokens, absent when the dialect does not report them.
    ///
    /// Absence is not zero: a dialect that reports no reasoning counter omits
    /// the field, while a dialect reporting none spent sends an explicit zero.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub reasoning_output_tokens: Option<u64>,
    /// Exact bytes the sample was decoded from, in source order.
    pub spans: Vec<TranscriptSpan>,
    /// Position of this event within the record that carried it.
    pub ordinal: u64,
}

/// One accounted batch of transcript records advanced a transcript source head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalIngested {
    /// Logical lane whose transcript was accounted.
    pub lane: CanonicalUuid,
    /// Vendor that produced the transcript.
    pub vendor: Name,
    /// Logical stream within that lane and vendor.
    pub stream: String,
    /// Opaque durable source generation the batch was read from.
    pub source: CanonicalUuid,
    /// Generation this batch retired, present only when [`Self::source`] is a
    /// new generation restarting the stream from its origin.
    ///
    /// A reader cannot infer supersession from a source that merely differs:
    /// that reading turns a batch written against a stale head, or a batch that
    /// named the wrong source, into a silent generation change that abandons
    /// the accounted position. Naming the retired generation makes the claim
    /// explicit and lets the guard refuse every batch that does not make it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub supersedes: Option<CanonicalUuid>,
    /// Position the batch continued from.
    pub from: TranscriptPosition,
    /// Position the batch left the source head at.
    pub to: TranscriptPosition,
    /// Every normalized usage sample the batch accounted.
    pub usage: Vec<TranscriptUsage>,
    /// Complete records carrying no usage that the batch accounted.
    pub non_usage_records: u64,
    /// Residue records the batch accounted.
    pub residue_records: u64,
}
