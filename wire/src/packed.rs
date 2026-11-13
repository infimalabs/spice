//! One list frame for every read, and the provenance a fold-backed read wears.

use std::fmt;
use std::str::FromStr;

use serde::de::{Deserializer, Error as DeError};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

use crate::envelope::Seq;

/// A derived projection name, spelled as a path of single lower-case words.
///
/// The registry that binds each name to its six trait answers is generated
/// separately; this type enforces the spelling only.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FoldName(String);

/// Text that does not spell a fold name.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("fold name must be lower-case single words joined by `::`: {0:?}")]
pub struct FoldNameError(String);

impl FoldName {
    /// Separator joining the words of a fold name.
    const SEPARATOR: &'static str = "::";

    /// The name as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for FoldName {
    type Err = FoldNameError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut words = text.split(Self::SEPARATOR);
        let shaped = words.clone().count() >= 2
            && words.all(|word| !word.is_empty() && word.bytes().all(|b| b.is_ascii_lowercase()));
        if shaped {
            Ok(Self(text.to_owned()))
        } else {
            Err(FoldNameError(text.to_owned()))
        }
    }
}

impl fmt::Display for FoldName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for FoldName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for FoldName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(D::Error::custom)
    }
}

/// What a packed answer left out.
///
/// Overflow is explicit and names both the omitted count and where to
/// continue. Silent truncation is not an admitted shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overflow {
    /// Rows ranked but not carried.
    pub omitted: u64,
    /// Position a continuation read resumes from.
    pub continuation: Seq,
}

/// The one frame every list read uses.
///
/// Rows are ranked before packing. A gate may refuse on overflow; an
/// interactive view may render the packed prefix only when it renders the
/// marker and horizon with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Packed<T> {
    /// Ranked rows carried by this answer.
    pub rows: Vec<T>,
    /// What was omitted, when anything was.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub overflow: Option<Overflow>,
    /// Greatest position through which the declared inputs are complete.
    pub horizon: Seq,
}

/// A fold-backed answer wearing its provenance.
///
/// Head-direct reads return a bare [`Packed`] beside their reply's snapshot
/// cursor; they have no fold or generation and never invent one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Derived<T> {
    /// Fold that produced this answer.
    pub fold: FoldName,
    /// Generation of that fold.
    #[serde(rename = "gen")]
    pub r#gen: u64,
    /// Greatest position the generation has applied.
    pub cursor: Seq,
    /// The packed rows themselves.
    pub packed: Packed<T>,
}
