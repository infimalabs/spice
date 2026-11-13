//! Shared scalar vocabulary for typed domain configuration.
//!
//! These values cross configuration loaders and renderers, but they do not
//! make `wire` a configurable domain. There is no `[wire]` table: domain
//! crates own their keys and use these types only where values must agree.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::de::{Deserializer, Error as DeError};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

/// The one tracked repository policy document, at the worktree root. Every
/// reader of tracked policy, from configuration loading through Gate and the
/// installed hook, names it through this constant.
pub const TRACKED_POLICY_FILE: &str = "spice.toml";

/// A duration written as an unsigned ASCII decimal followed by `s`, `m`, or `h`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DurationWord {
    word: String,
    seconds: u64,
}

impl DurationWord {
    /// The checked standard-library duration denoted by this word.
    #[must_use]
    pub const fn get(&self) -> Duration {
        Duration::from_secs(self.seconds)
    }
}

impl FromStr for DurationWord {
    type Err = DurationWordError;

    fn from_str(word: &str) -> Result<Self, Self::Err> {
        let Some((&suffix, digits)) = word.as_bytes().split_last() else {
            return Err(DurationWordError::Grammar(word.to_owned()));
        };
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            return Err(DurationWordError::Grammar(word.to_owned()));
        }
        let unit = DurationUnit::from_suffix(suffix)
            .ok_or_else(|| DurationWordError::Grammar(word.to_owned()))?;
        let value = word[..word.len() - 1]
            .parse::<u64>()
            .map_err(|_| DurationWordError::Overflow(word.to_owned()))?;
        let seconds = value
            .checked_mul(unit.seconds())
            .ok_or_else(|| DurationWordError::Overflow(word.to_owned()))?;
        Ok(Self {
            word: word.to_owned(),
            seconds,
        })
    }
}

impl fmt::Display for DurationWord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.word)
    }
}

impl Serialize for DurationWord {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for DurationWord {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

/// Why a duration word did not cross.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DurationWordError {
    /// The word did not have the exact `digits + unit` grammar.
    #[error("duration must be an unsigned ASCII decimal followed immediately by s, m, or h: {0:?}")]
    Grammar(String),
    /// Its decimal or checked unit multiplication exceeded `u64` seconds.
    #[error("duration exceeds the supported range: {0:?}")]
    Overflow(String),
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum DurationUnit {
    Seconds,
    Minutes,
    Hours,
}

impl DurationUnit {
    const fn from_suffix(suffix: u8) -> Option<Self> {
        match suffix {
            b's' => Some(Self::Seconds),
            b'm' => Some(Self::Minutes),
            b'h' => Some(Self::Hours),
            _ => None,
        }
    }

    const fn seconds(self) -> u64 {
        match self {
            Self::Seconds => 1,
            Self::Minutes => 60,
            Self::Hours => 60 * 60,
        }
    }
}

/// Winning configuration layers in increasing precedence order.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigSource {
    /// Value compiled into the binary.
    Builtin,
    /// Intent read from the tracked policy document, [`TRACKED_POLICY_FILE`].
    Tracked,
    /// Admitted machine-local override shared through Git common state.
    Local,
    /// Admitted override scoped to one Git worktree, read from that worktree's
    /// own Git directory.
    Worktree,
    /// Explicit command-line intent.
    Flag,
}

impl ConfigSource {
    /// Every source from lowest to highest precedence.
    pub const ALL: &[Self] = &[
        Self::Builtin,
        Self::Tracked,
        Self::Local,
        Self::Worktree,
        Self::Flag,
    ];

    /// Stable source spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Tracked => "tracked",
            Self::Worktree => "worktree",
            Self::Local => "local",
            Self::Flag => "flag",
        }
    }
}

impl fmt::Display for ConfigSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The winning layer and any typed builtin value it shadowed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "source",
    rename_all = "lowercase",
    deny_unknown_fields,
    bound(deserialize = "T: Deserialize<'de>")
)]
pub enum Provenance<T> {
    /// The effective value is the compiled builtin itself.
    Builtin {},
    /// Tracked intent won.
    Tracked {
        /// Compiled value hidden by the tracked value, when one exists.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        builtin: Option<T>,
    },
    /// A machine-local override won.
    Local {
        /// Compiled value hidden by the local value, when one exists.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        builtin: Option<T>,
    },
    /// A worktree-scoped override won.
    Worktree {
        /// Compiled value hidden by the worktree value, when one exists.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        builtin: Option<T>,
    },
    /// An explicit command-line flag won.
    Flag {
        /// Compiled value hidden by the flag value, when one exists.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        builtin: Option<T>,
    },
}

impl<T> Provenance<T> {
    /// Layer that supplied the effective value.
    #[must_use]
    pub const fn source(&self) -> ConfigSource {
        match self {
            Self::Builtin {} => ConfigSource::Builtin,
            Self::Tracked { .. } => ConfigSource::Tracked,
            Self::Worktree { .. } => ConfigSource::Worktree,
            Self::Local { .. } => ConfigSource::Local,
            Self::Flag { .. } => ConfigSource::Flag,
        }
    }

    /// Typed compiled value hidden by the winner, when one existed.
    #[must_use]
    pub const fn builtin(&self) -> Option<&T> {
        match self {
            Self::Builtin {} => None,
            Self::Tracked { builtin }
            | Self::Worktree { builtin }
            | Self::Local { builtin }
            | Self::Flag { builtin } => builtin.as_ref(),
        }
    }
}
