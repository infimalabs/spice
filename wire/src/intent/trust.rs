//! The recorded configuration one trust digest names, as the operator reviews
//! it before ruling.

use serde::{Deserialize, Serialize};

use crate::output::{Exhausted, Limits};
use crate::{ConfigSource, Digest, SpiceCommand, TrustState};

/// One bounded repository trust-report request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustReportRead {
    /// Resume after this ruling as the prior page showed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<TrustReportCursor>,
    /// Already-admitted interactive output limits.
    pub limits: Limits,
}

/// Resume point after one shown ruling: the standing and digest it had on its
/// page, plus the original cursor digest excluded from every later page,
/// spelled `STANDING:DIGEST:EXCLUDED`. Carrying the standing keeps the resume
/// point fixed when a ruling changes standing; carrying the exclusion prevents
/// that moved ruling from appearing again in a later tier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustReportCursor {
    /// Standing the ruling had on the page that showed it.
    pub standing: TrustState,
    /// Its exact digest.
    pub digest: Digest,
    /// Original cursor digest omitted from every continuation page.
    pub excluded: Digest,
}

impl std::fmt::Display for TrustReportCursor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}:{}:{}",
            self.standing.as_str(),
            self.digest,
            self.excluded
        )
    }
}

impl std::str::FromStr for TrustReportCursor {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut fields = text.split(':');
        let (Some(standing), Some(digest), Some(excluded), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(format!(
                "trust report cursor {text:?} is not STANDING:DIGEST:EXCLUDED"
            ));
        };
        let standing = match standing {
            "pended" => TrustState::Pended,
            "granted" => TrustState::Granted,
            "revoked" => TrustState::Revoked,
            other => return Err(format!("trust report cursor standing {other:?} is unknown")),
        };
        let digest = digest.parse().map_err(|error| format!("{error}"))?;
        let excluded = excluded.parse().map_err(|error| format!("{error}"))?;
        Ok(Self {
            standing,
            digest,
            excluded,
        })
    }
}

/// One current executable-trust ruling in operator-report order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustReportRuling {
    /// Exact executable configuration digest.
    pub digest: Digest,
    /// Current operator standing.
    pub standing: TrustState,
    /// Recorded configuration differential when it fit this page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub differential: Option<TrustShowAnswer>,
    /// Exact explicit read for a differential withheld by the byte budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<SpiceCommand>,
}

/// First ruling withheld from one bounded trust-report page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustReportNext {
    /// Limit that stopped admission.
    pub exhausted: Exhausted,
    /// Encoded bytes the first withheld record requires.
    pub next_bytes: usize,
    /// Limits this page admitted.
    pub limits: Limits,
    /// Exact command that resumes at the first withheld ruling.
    pub continuation: SpiceCommand,
}

/// Successful bounded repository trust report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustReportAnswer {
    /// Admitted current rulings.
    pub rulings: Vec<TrustReportRuling>,
    /// Encoded row bytes admitted.
    pub shown_bytes: usize,
    /// First withheld ruling, when this page is incomplete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<TrustReportNext>,
}
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
        /// The line's terminator.
        #[serde(default, skip_serializing_if = "TrustLineEnding::is_lf")]
        ending: TrustLineEnding,
    },
    /// A line only the reviewed digest holds.
    Added {
        /// The line text without its terminator.
        text: String,
        /// The line's terminator.
        #[serde(default, skip_serializing_if = "TrustLineEnding::is_lf")]
        ending: TrustLineEnding,
    },
    /// A line only the baseline holds.
    Removed {
        /// The line text without its terminator.
        text: String,
        /// The line's terminator.
        #[serde(default, skip_serializing_if = "TrustLineEnding::is_lf")]
        ending: TrustLineEnding,
    },
    /// Unchanged lines omitted between context.
    Elided {
        /// How many lines.
        count: u64,
    },
}

/// How one differential line ends, so a change to line endings alone stays
/// visible once the terminator is stripped from the text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLineEnding {
    /// A line feed.
    #[default]
    Lf,
    /// A carriage return then a line feed.
    Crlf,
    /// No terminator: the document's last line.
    Missing,
}

impl TrustLineEnding {
    /// Classify one line as the differential tokenizes it, terminator
    /// included, returning the text without its terminator.
    #[must_use]
    pub fn split(line: &str) -> (&str, Self) {
        if let Some(text) = line.strip_suffix("\r\n") {
            (text, Self::Crlf)
        } else if let Some(text) = line.strip_suffix('\n') {
            (text, Self::Lf)
        } else {
            (line, Self::Missing)
        }
    }

    /// Whether this is the ordinary line feed, which renders unmarked.
    #[must_use]
    pub const fn is_lf(&self) -> bool {
        matches!(self, Self::Lf)
    }

    /// The operator-facing note for an ending other than a line feed.
    #[must_use]
    pub const fn note(self) -> Option<&'static str> {
        match self {
            Self::Lf => None,
            Self::Crlf => Some("line ends with CRLF"),
            Self::Missing => Some("no newline at end of file"),
        }
    }
}
