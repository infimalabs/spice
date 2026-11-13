//! Which source names a build: the untracked record, then Git HEAD, then
//! nothing.

use std::fmt;

use wire::manifest::{BuildInfo, BuildSource, ProductVersion};

use super::SourceIdentity;
use super::history::{Floor, HistoryError};
use crate::{BUILD_RECORD_FILE, Build, COUNTERS_FILE, TrackedVersion, VERSION_FILE};

/// The Git history a source build reads.
pub(super) trait History {
    /// HEAD's commit and committer second.
    fn head(&mut self) -> Result<SourceIdentity, String>;
    /// HEAD's counter sum and the floor its patch counts from.
    fn floor(&mut self) -> Result<(u64, Floor), HistoryError>;
}

/// Why no build identity derives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Refusal {
    /// The record is not one canonical build record.
    Record,
    /// A full tracked version differs from the record's product.
    RecordVersion {
        record: ProductVersion,
        tracked: ProductVersion,
    },
    /// The record's major differs from the tracked major.
    RecordMajor {
        record: ProductVersion,
        major: u64,
    },
    /// The record's minor differs from the tree's counter sum.
    RecordMinor {
        record: ProductVersion,
        sum: u64,
    },
    /// A staged build found neither a record nor Git.
    StagedWithoutSource,
    /// A staged build's shallow history ends at `commit` before any raise.
    StagedTruncated {
        commit: String,
    },
    /// HEAD's committer second precedes the floor's.
    BeforeFloor {
        head: i64,
        floor: i64,
    },
    History(HistoryError),
    /// An input could not be read.
    Source(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Record => write!(
                formatter,
                "{BUILD_RECORD_FILE} is not one canonical build record"
            ),
            Self::RecordVersion { record, tracked } => write!(
                formatter,
                "{BUILD_RECORD_FILE} names {record} but {VERSION_FILE} fixes {tracked}"
            ),
            Self::RecordMajor { record, major } => write!(
                formatter,
                "{BUILD_RECORD_FILE} names {record} but {VERSION_FILE} tracks major {major}"
            ),
            Self::RecordMinor { record, sum } => write!(
                formatter,
                "{BUILD_RECORD_FILE} names {record} but {COUNTERS_FILE} counters sum to {sum}"
            ),
            Self::StagedWithoutSource => write!(
                formatter,
                "a staged build requires {BUILD_RECORD_FILE} or Git HEAD"
            ),
            Self::StagedTruncated { commit } => write!(
                formatter,
                "a staged build requires history through the newest counter raise, but shallow history ends at {commit}"
            ),
            Self::BeforeFloor { head, floor } => write!(
                formatter,
                "HEAD committer second {head} precedes the counter raise at {floor}"
            ),
            Self::History(error) => error.fmt(formatter),
            Self::Source(detail) => formatter.write_str(detail),
        }
    }
}

/// The build identity, first match wins:
///
/// 1. `record`, verbatim once it is canonical and agrees with `tracked` and,
///    under a major, with the tree's counter sum;
/// 2. Git `history`: HEAD's identity, with a full `tracked` version verbatim
///    or the major, HEAD's counter sum, and HEAD's committer second past the
///    newest raise;
/// 3. nothing: a staged build refuses, and a development build counts its
///    patch in Unix seconds at `now`.
pub(super) fn derive(
    tracked: &TrackedVersion,
    record: Option<&[u8]>,
    tree_sum: impl FnOnce() -> Result<u64, String>,
    history: Option<&mut dyn History>,
    build: Build,
    now: impl FnOnce() -> i64,
) -> Result<BuildInfo, Refusal> {
    if let Some(bytes) = record {
        return admit_record(tracked, bytes, tree_sum);
    }
    if let Some(history) = history {
        return from_history(tracked, history, build);
    }
    match build {
        Build::Staged => Err(Refusal::StagedWithoutSource),
        Build::Dev => Ok(BuildInfo {
            product: tracked.product(
                tree_sum().map_err(Refusal::Source)?,
                u64::try_from(now()).unwrap_or(0),
                Build::Dev,
            ),
            source: None,
        }),
    }
}

/// The canonical encoding of one build record: the exact bytes of an exported
/// tree's record and of a rack's root `build.json`.
#[must_use]
pub fn build_record_bytes(info: &BuildInfo) -> Vec<u8> {
    serde_json::to_vec(info).expect("a build record encodes as JSON")
}

fn admit_record(
    tracked: &TrackedVersion,
    bytes: &[u8],
    tree_sum: impl FnOnce() -> Result<u64, String>,
) -> Result<BuildInfo, Refusal> {
    let info = serde_json::from_slice::<BuildInfo>(bytes).map_err(|_| Refusal::Record)?;
    if build_record_bytes(&info) != bytes {
        return Err(Refusal::Record);
    }
    match tracked {
        TrackedVersion::Full(full) => {
            if info.product != *full {
                return Err(Refusal::RecordVersion {
                    record: info.product,
                    tracked: full.clone(),
                });
            }
        }
        TrackedVersion::Major(major) => {
            let mut core = info.product.as_str().split(['.', '-', '+']);
            let mut number = || core.next().and_then(|part| part.parse::<u64>().ok());
            let (record_major, record_minor) = (number(), number());
            if record_major != Some(*major) {
                return Err(Refusal::RecordMajor {
                    record: info.product,
                    major: *major,
                });
            }
            let sum = tree_sum().map_err(Refusal::Source)?;
            if record_minor != Some(sum) {
                return Err(Refusal::RecordMinor {
                    record: info.product,
                    sum,
                });
            }
        }
    }
    Ok(info)
}

fn from_history(
    tracked: &TrackedVersion,
    history: &mut dyn History,
    build: Build,
) -> Result<BuildInfo, Refusal> {
    let head = history.head().map_err(Refusal::Source)?;
    let product = match tracked {
        TrackedVersion::Full(_) => tracked.product(0, 0, build),
        TrackedVersion::Major(_) => {
            let (sum, floor) = history.floor().map_err(Refusal::History)?;
            let floor = match floor {
                Floor::Raised { seconds, .. } => seconds,
                Floor::Truncated { commit, .. } if build == Build::Staged => {
                    return Err(Refusal::StagedTruncated { commit });
                }
                Floor::Truncated { seconds, .. } => seconds,
            };
            let head_seconds = head.instant().seconds();
            let patch = head_seconds
                .checked_sub(floor)
                .and_then(|patch| u64::try_from(patch).ok())
                .ok_or(Refusal::BeforeFloor {
                    head: head_seconds,
                    floor,
                })?;
            tracked.product(sum, patch, build)
        }
    };
    Ok(BuildInfo {
        product,
        source: Some(BuildSource {
            commit: head.commit().to_owned(),
            committed: head.instant().seconds(),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::super::history::tests::commit;
    use super::super::history::{Commit, Parent, floor};
    use super::*;
    use crate::SourceInstant;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const NOW: i64 = 1_800_000_000;

    /// A Git history whose floor the real walk derives from fixture commits.
    struct Fixture {
        head: i64,
        sum: u64,
        commits: Vec<Commit>,
    }

    impl History for Fixture {
        fn head(&mut self) -> Result<SourceIdentity, String> {
            Ok(SourceIdentity::new(
                COMMIT.to_owned(),
                SourceInstant::new(self.head),
            ))
        }

        fn floor(&mut self) -> Result<(u64, Floor), HistoryError> {
            let floor = floor(self.sum, self.commits.drain(..).map(Ok))?;
            Ok((self.sum, floor))
        }
    }

    fn version(text: &str) -> ProductVersion {
        ProductVersion::parse(text).expect("fixture version")
    }

    fn major() -> TrackedVersion {
        TrackedVersion::Major(0)
    }

    fn record(product: &str, source: Option<(&str, i64)>) -> Vec<u8> {
        build_record_bytes(&BuildInfo {
            product: version(product),
            source: source.map(|(commit, committed)| BuildSource {
                commit: commit.to_owned(),
                committed,
            }),
        })
    }

    fn main_history(head: i64) -> Fixture {
        Fixture {
            head,
            sum: 135,
            commits: vec![
                commit(
                    "retire",
                    1_700_000_500,
                    Parent::First,
                    (Some(135), Some(135)),
                ),
                commit(
                    "raise",
                    1_700_000_000,
                    Parent::First,
                    (Some(132), Some(135)),
                ),
            ],
        }
    }

    fn derived(
        tracked: &TrackedVersion,
        record: Option<&[u8]>,
        history: Option<&mut dyn History>,
        build: Build,
    ) -> Result<BuildInfo, Refusal> {
        derive(tracked, record, || Ok(135), history, build, || NOW)
    }

    #[test]
    fn a_git_source_counts_the_patch_from_the_newest_raise() {
        let info = derived(
            &major(),
            None,
            Some(&mut main_history(1_700_079_083)),
            Build::Staged,
        )
        .expect("Git source derives");
        assert_eq!(info.product, version("0.135.79083"));
        assert_eq!(
            info.source,
            Some(BuildSource {
                commit: COMMIT.to_owned(),
                committed: 1_700_079_083,
            })
        );
        let dev = derived(
            &major(),
            None,
            Some(&mut main_history(1_700_079_083)),
            Build::Dev,
        )
        .expect("dirty Git source derives");
        assert_eq!(dev.product, version("0.135.79083-dev"));
        assert_eq!(
            derived(
                &major(),
                None,
                Some(&mut main_history(1_699_999_999)),
                Build::Staged
            ),
            Err(Refusal::BeforeFloor {
                head: 1_699_999_999,
                floor: 1_700_000_000,
            })
        );
    }

    #[test]
    fn a_full_tracked_version_is_the_product_verbatim_without_walking_history() {
        let pinned = TrackedVersion::Full(version("2.7.1"));
        let mut unwalkable = Fixture {
            head: 1_700_000_000,
            sum: 135,
            commits: vec![commit("lower", 1, Parent::First, (Some(1), Some(0)))],
        };
        let info = derived(&pinned, None, Some(&mut unwalkable), Build::Staged)
            .expect("full version derives");
        assert_eq!(info.product, version("2.7.1"));
        assert_eq!(
            info.source.map(|source| source.commit),
            Some(COMMIT.to_owned())
        );
        assert_eq!(unwalkable.commits.len(), 1);
    }

    #[test]
    fn a_matching_record_wins_over_git_verbatim() {
        let bytes = record("0.135.79083", Some((COMMIT, 1_700_079_083)));
        let mut later = main_history(1_800_000_000);
        let info = derived(&major(), Some(&bytes), Some(&mut later), Build::Staged)
            .expect("matching record admits");
        assert_eq!(build_record_bytes(&info), bytes);
        let pinned = record("2.7.1", None);
        assert_eq!(
            derived(
                &TrackedVersion::Full(version("2.7.1")),
                Some(&pinned),
                None,
                Build::Staged
            )
            .map(|info| build_record_bytes(&info)),
            Ok(pinned)
        );
    }

    #[test]
    fn a_record_disagreeing_with_the_tree_refuses() {
        let bytes = record("0.135.79083", Some((COMMIT, 1_700_079_083)));
        assert_eq!(
            derived(
                &TrackedVersion::Full(version("0.135.79084")),
                Some(&bytes),
                None,
                Build::Dev
            ),
            Err(Refusal::RecordVersion {
                record: version("0.135.79083"),
                tracked: version("0.135.79084"),
            })
        );
        assert_eq!(
            derived(&TrackedVersion::Major(1), Some(&bytes), None, Build::Dev),
            Err(Refusal::RecordMajor {
                record: version("0.135.79083"),
                major: 1,
            })
        );
        assert_eq!(
            derive(&major(), Some(&bytes), || Ok(136), None, Build::Dev, || NOW),
            Err(Refusal::RecordMinor {
                record: version("0.135.79083"),
                sum: 136,
            })
        );
        let mut spaced = bytes.clone();
        spaced.push(b'\n');
        for refused in [
            spaced.as_slice(),
            b"{\"product\":\"0.135.1\",\"extra\":1}",
            b"0.135.1",
        ] {
            assert_eq!(
                derived(&major(), Some(refused), None, Build::Dev),
                Err(Refusal::Record)
            );
        }
    }

    #[test]
    fn without_any_source_dev_counts_unix_seconds_and_staged_refuses() {
        let info = derived(&major(), None, None, Build::Dev).expect("dev derives");
        assert_eq!(info.product, version("0.135.1800000000-dev"));
        assert_eq!(info.source, None);
        assert_eq!(
            derived(&major(), None, None, Build::Staged),
            Err(Refusal::StagedWithoutSource)
        );
    }

    #[test]
    fn shallow_history_before_a_raise_refuses_staged_and_counts_dev_from_the_boundary() {
        let shallow = || Fixture {
            head: 1_700_000_900,
            sum: 135,
            commits: vec![
                commit(
                    "retire",
                    1_700_000_500,
                    Parent::First,
                    (Some(135), Some(135)),
                ),
                commit(
                    "boundary",
                    1_700_000_400,
                    Parent::Shallow,
                    (None, Some(135)),
                ),
            ],
        };
        assert_eq!(
            derived(&major(), None, Some(&mut shallow()), Build::Staged),
            Err(Refusal::StagedTruncated {
                commit: "boundary".to_owned()
            })
        );
        assert_eq!(
            derived(&major(), None, Some(&mut shallow()), Build::Dev).map(|info| info.product),
            Ok(version("0.135.500-dev"))
        );
    }

    #[test]
    fn a_first_parent_decrease_refuses_the_git_source() {
        let mut lowered = Fixture {
            head: 1_700_000_900,
            sum: 134,
            commits: vec![commit(
                "lower",
                1_700_000_800,
                Parent::First,
                (Some(135), Some(134)),
            )],
        };
        assert!(matches!(
            derived(&major(), None, Some(&mut lowered), Build::Dev),
            Err(Refusal::History(HistoryError::Decrease { .. }))
        ));
    }
}
