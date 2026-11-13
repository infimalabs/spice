//! The product version parts every build derives from `dist/version/`.
//!
//! `dist/version/base` holds what a person decides: the major number, `0` on
//! main, or a full `MAJOR.MINOR.PATCH` that is the product version verbatim.
//! Under a major, the minor is the sum of every sorted `name=decimal` record in
//! `dist/version/counters`. Owning crates select their record from that same
//! compile-time-readable registry. The `gap` record absorbs the last value of
//! any counter retired from the set so the sum never moves backwards until the
//! major rotates. The patch comes from the build's source history, which this
//! crate never reads. A build that never passes through release staging is
//! marked `-dev`.

use std::fmt;

use wire::manifest::ProductVersion;

use crate::authority::{AuthorityError, tracked_text_line};

const VERSION_TEXT: &str = ::std::include_str!("../version/base");
const COUNTERS_TEXT: &str = ::std::include_str!("../version/counters");
const DEV: &str = "dev";

/// The tracked file holding [`TrackedVersion`].
pub const VERSION_FILE: &str = "dist/version/base";
/// The tracked additive product-version counter registry.
pub(crate) const COUNTERS_FILE: &str = "dist/version/counters";
/// The untracked build record an exported source tree carries: the canonical
/// [`wire::manifest::BuildInfo`] of the commit it was exported from.
pub const BUILD_RECORD_FILE: &str = "dist/build.json";
/// The one source archive a release stages every target from, under the stage
/// root: HEAD's tree exported with [`BUILD_RECORD_FILE`] added, by
/// `spice dev xtask -- source-archive`. Its bytes are the release's source
/// identity: every target payload records their digest.
pub const SOURCE_ARCHIVE_FILE: &str = "source.tar";
/// What [`VERSION_FILE`] tracks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrackedVersion {
    /// The major number; the counter registry supplies minor and source history supplies patch.
    Major(u64),
    /// The exact product version, used verbatim.
    Full(ProductVersion),
}

impl TrackedVersion {
    /// Admit one tracked version: a canonical major number, or a canonical
    /// `MAJOR.MINOR.PATCH` with neither pre-release nor build metadata.
    ///
    /// # Errors
    ///
    /// Refuses any other spelling.
    pub(crate) fn parse(text: &str) -> Result<Self, TrackedVersionError> {
        let refused = || TrackedVersionError(text.to_owned());
        if !text.contains('.') {
            return canonical_number(text)
                .then(|| text.parse().ok())
                .flatten()
                .map(Self::Major)
                .ok_or_else(refused);
        }
        if text.contains(['-', '+']) {
            return Err(refused());
        }
        ProductVersion::parse(text)
            .map(Self::Full)
            .map_err(|_| refused())
    }

    /// The product version a build reports: a full version verbatim, or the
    /// major with `minor` and `patch`, marked `-dev` outside release staging.
    #[must_use]
    pub fn product(&self, minor: u64, patch: u64, build: Build) -> ProductVersion {
        let core = match self {
            Self::Major(major) => format!("{major}.{minor}.{patch}"),
            Self::Full(full) => full.as_str().to_owned(),
        };
        let text = match build {
            Build::Staged => core,
            Build::Dev => format!("{core}-{DEV}"),
        };
        ProductVersion::parse(&text).expect("a derived version is canonical semantic version text")
    }
}

impl fmt::Display for TrackedVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Major(major) => write!(formatter, "{major}"),
            Self::Full(full) => formatter.write_str(full.as_str()),
        }
    }
}

/// Why one tracked version was refused.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("tracked version is neither a canonical major number nor MAJOR.MINOR.PATCH: {0:?}")]
pub struct TrackedVersionError(String);

/// Sum Spice's canonical distribution-counter registry.
///
/// # Errors
///
/// Refuses malformed counters and a sum past `u64`, naming [`COUNTERS_FILE`].
pub(crate) fn counter_sum(text: &str) -> Result<u64, CounterError> {
    let body = text
        .strip_suffix('\n')
        .filter(|body| !body.is_empty())
        .ok_or_else(|| CounterError::Counter {
            path: COUNTERS_FILE.to_owned(),
            text: text.to_owned(),
        })?;
    let mut sum: u64 = 0;
    let mut previous = None;
    for record in body.split('\n') {
        let (key, value) = record
            .split_once('=')
            .ok_or_else(|| CounterError::Counter {
                path: COUNTERS_FILE.to_owned(),
                text: text.to_owned(),
            })?;
        let canonical_key = !key.is_empty()
            && key
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            && previous.is_none_or(|previous| previous < key);
        if !canonical_key || !canonical_number(value) {
            return Err(CounterError::Counter {
                path: COUNTERS_FILE.to_owned(),
                text: text.to_owned(),
            });
        }
        previous = Some(key);
        let value = value.parse::<u64>().map_err(|_| CounterError::Counter {
            path: COUNTERS_FILE.to_owned(),
            text: text.to_owned(),
        })?;
        sum = sum
            .checked_add(value)
            .ok_or_else(|| CounterError::Overflow(COUNTERS_FILE.to_owned()))?;
    }
    Ok(sum)
}

/// The counter sum compiled into this distribution generation.
///
/// # Errors
///
/// Refuses a malformed compiled counter.
pub fn current_counter_sum() -> Result<u64, CounterError> {
    counter_sum(COUNTERS_TEXT)
}

/// Why tracked product-version counters were refused.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CounterError {
    #[error("{path} is not a canonical product-version counter source: {text:?}")]
    Counter { path: String, text: String },
    #[error("product-version counters sum past u64 at {0}")]
    Overflow(String),
}

/// A source commit's committer second.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct SourceInstant(i64);

impl SourceInstant {
    /// Place a commit by its committer second.
    #[must_use]
    pub(crate) fn new(seconds: i64) -> Self {
        Self(seconds)
    }

    /// The committer second.
    #[must_use]
    pub(crate) fn seconds(&self) -> i64 {
        self.0
    }
}

/// Whether a build carries the unmarked release version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Build {
    /// A `stage-target` payload, or the local Windows appliance built from a
    /// checkout equal to HEAD.
    Staged,
    /// Any other build, marked `-dev`.
    Dev,
}

/// The tracked version compiled into this distribution generation.
///
/// # Errors
///
/// Refuses a malformed [`VERSION_FILE`].
pub fn current_tracked_version() -> Result<TrackedVersion, AuthorityError> {
    admit_tracked_version(VERSION_TEXT)
}

/// Admit the full text of one [`VERSION_FILE`].
///
/// # Errors
///
/// Refuses anything but one LF-terminated canonical major number or full
/// version.
pub(crate) fn admit_tracked_version(text: &str) -> Result<TrackedVersion, AuthorityError> {
    Ok(TrackedVersion::parse(tracked_text_line(
        text,
        "product version",
    )?)?)
}

fn canonical_number(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::*;

    fn full(text: &str) -> TrackedVersion {
        TrackedVersion::Full(ProductVersion::parse(text).expect("fixture version"))
    }

    #[test]
    fn tracked_versions_admit_a_canonical_major_or_a_full_release_version() {
        assert_eq!(TrackedVersion::parse("0"), Ok(TrackedVersion::Major(0)));
        assert_eq!(TrackedVersion::parse("12"), Ok(TrackedVersion::Major(12)));
        assert_eq!(TrackedVersion::parse("1.2.3"), Ok(full("1.2.3")));
        for refused in [
            "",
            "01",
            "calendar",
            " 0",
            "0.26298",
            "1.2",
            "01.2.3",
            "v1",
            "1-dev",
            "1.2.3-dev",
            "1.2.3+build",
        ] {
            assert!(TrackedVersion::parse(refused).is_err(), "{refused:?}");
        }
        assert_eq!(
            current_tracked_version().expect("tracked version admits"),
            TrackedVersion::Major(0)
        );
    }

    #[test]
    fn a_major_takes_minor_and_patch_while_a_full_version_is_verbatim() {
        let main = TrackedVersion::Major(0);
        assert_eq!(main.product(132, 0, Build::Staged).as_str(), "0.132.0");
        assert_eq!(
            main.product(132, 90_061, Build::Dev).as_str(),
            "0.132.90061-dev"
        );
        let pinned = full("2.7.1");
        assert_eq!(pinned.product(132, 5, Build::Staged).as_str(), "2.7.1");
        assert_eq!(pinned.product(132, 5, Build::Dev).as_str(), "2.7.1-dev");
    }

    #[test]
    fn counter_registry_enforces_canonical_grammar_and_sum() {
        assert_eq!(counter_sum("gap=0\ninstall=22\njournal=110\n"), Ok(132));
        for text in [
            "",
            "22\n",
            "gap=0",
            "gap=\n",
            "gap=00\n",
            "Gap=0\n",
            "journal=1\ninstall=1\n",
            "gap=0\ngap=1\n",
        ] {
            assert_eq!(
                counter_sum(text),
                Err(CounterError::Counter {
                    path: COUNTERS_FILE.to_owned(),
                    text: text.to_owned(),
                }),
                "{text:?}"
            );
        }
        assert_eq!(
            counter_sum("a=18446744073709551615\nb=1\n"),
            Err(CounterError::Overflow(COUNTERS_FILE.to_owned()))
        );
    }

    #[test]
    fn versions_order_by_major_then_sum_then_patch() {
        let main = TrackedVersion::Major(0);
        let ascending = [
            main.product(0, 0, Build::Dev),
            main.product(132, 0, Build::Dev),
            main.product(132, 0, Build::Staged),
            main.product(132, 1, Build::Dev),
            main.product(132, 1, Build::Staged),
            main.product(132, 1_000_000, Build::Staged),
            main.product(133, 0, Build::Staged),
            TrackedVersion::Major(1).product(0, 0, Build::Staged),
        ];
        for pair in ascending.windows(2) {
            assert_eq!(pair[0].precedence(&pair[1]), Ordering::Less, "{pair:?}");
        }
    }
}
