//! Canonical report-only bodies for completed ordinary gates.
//!
//! Reports copy evidence out of completed runs. Their decoders return retained
//! observations only: no report type has a path back to a contract, runner,
//! argument vector, process bounds, or other execution authority.

use std::error::Error;
use std::fmt;
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use crate::long_lines::{LongLineFinding, validate as validate_long_lines};
use crate::{Contract, Metric};

use crate::measure::Measurement;
use crate::ratchet::{Finding, Movement};
use crate::verdict::{CurrentRun, Standing, Verdict, run_evidence_digest};
use serde::{Deserialize, Serialize};

/// The one ordinary gate report schema this build emits and admits.
pub const GATE_REPORT_VERSION: u64 = 1;

fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// Why retained report bytes were not admitted as a current canonical report.
#[derive(Debug)]
pub enum ReportDecodeError {
    /// The JSON shape or one typed field was malformed.
    Json(serde_json::Error),
    /// The bytes name a schema this build does not interpret.
    UnsupportedVersion {
        /// Report family whose version was foreign.
        report: &'static str,
        /// Exact foreign version retained as evidence.
        observed: u64,
        /// Version this build admits for that report family.
        current: u64,
    },
    /// A valid value used a second JSON spelling.
    NonCanonical {
        /// The one JSON spelling this build admits.
        canonical: Vec<u8>,
    },
    /// Duplicated or derived evidence disagreed inside one report.
    Inconsistent {
        /// Report family whose evidence disagreed.
        report: &'static str,
        /// Exact invariant that did not hold.
        evidence: String,
    },
}

impl ReportDecodeError {
    pub(crate) fn unsupported(report: &'static str, observed: u64, current: u64) -> Self {
        Self::UnsupportedVersion {
            report,
            observed,
            current,
        }
    }

    pub(crate) fn inconsistent(report: &'static str, evidence: impl Into<String>) -> Self {
        Self::Inconsistent {
            report,
            evidence: evidence.into(),
        }
    }

    /// Canonical replacement bytes, when only the JSON spelling differed.
    #[must_use]
    pub fn canonical(&self) -> Option<&[u8]> {
        match self {
            Self::NonCanonical { canonical } => Some(canonical),
            Self::Json(_) | Self::UnsupportedVersion { .. } | Self::Inconsistent { .. } => None,
        }
    }
}

impl fmt::Display for ReportDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(source) => write!(formatter, "decoding the report: {source}"),
            Self::UnsupportedVersion {
                report,
                observed,
                current,
            } => write!(
                formatter,
                "{report} version {observed} is unsupported; current version is {current}"
            ),
            Self::NonCanonical { .. } => {
                formatter.write_str("report JSON is not the canonical byte spelling")
            }
            Self::Inconsistent { report, evidence } => {
                write!(formatter, "{report} evidence is inconsistent: {evidence}")
            }
        }
    }
}

impl Error for ReportDecodeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json(source) => Some(source),
            Self::UnsupportedVersion { .. }
            | Self::NonCanonical { .. }
            | Self::Inconsistent { .. } => None,
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct ReportVersionEvidence {
    pub(crate) version: u64,
}

mod gate;
pub use gate::*;

fn require_strict_names<'a>(
    report: &'static str,
    field: &'static str,
    names: impl IntoIterator<Item = &'a String>,
) -> Result<(), ReportDecodeError> {
    let names: Vec<&String> = names.into_iter().collect();
    if names
        .iter()
        .any(|name| name.is_empty() || name.contains(['\n', '\r']))
        || names.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(ReportDecodeError::inconsistent(
            report,
            format!("{field} are not nonempty, unbroken, sorted, and unique"),
        ));
    }
    Ok(())
}

fn quoted(value: &str) -> String {
    serde_json::to_string(value).expect("a Rust string always has one JSON spelling")
}

mod render;
pub use render::*;
