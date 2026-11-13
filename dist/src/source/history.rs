//! The patch floor: the newest first-parent commit that raised the additive
//! product-version counter sum.
//!
//! The walk consumes first-parent commits touching the counters, newest first,
//! and stops at the first raise, so it reads no further back than the floor.

use std::fmt;

/// One first-parent commit that touched product-version counters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Commit {
    pub(super) commit: String,
    pub(super) seconds: i64,
    pub(super) parent: Parent,
    /// The registry sum before and after the commit.
    pub(super) change: Change,
}

/// One counter's value before and after a commit; `None` where it is absent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Change {
    pub(super) old: Option<u64>,
    pub(super) new: Option<u64>,
}

/// What precedes a commit on the first-parent chain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Parent {
    /// A first parent the history carries.
    First,
    /// No parent: the repository's root commit.
    Root,
    /// A first parent a shallow clone omitted.
    Shallow,
}

/// Where the patch counts from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Floor {
    /// The newest commit whose sum exceeds its first parent's.
    Raised { commit: String, seconds: i64 },
    /// Shallow history ended at `commit` before any raise.
    Truncated { commit: String, seconds: i64 },
}

/// Why the history has no floor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum HistoryError {
    /// A first-parent step lowered the counter sum.
    Decrease {
        commit: String,
        parent: u64,
        sum: u64,
    },
    /// A commit's changes do not reconcile with the sum it carries.
    Inconsistent { commit: String },
    /// Complete history never raised the sum.
    Unraised,
    /// The history could not be read.
    Source(String),
}

impl fmt::Display for HistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decrease {
                commit,
                parent,
                sum,
            } => write!(
                formatter,
                "first-parent commit {commit} lowers the product-version counter sum from {parent} to {sum}"
            ),
            Self::Inconsistent { commit } => write!(
                formatter,
                "product-version counter changes at {commit} do not reconcile with the sum at HEAD"
            ),
            Self::Unraised => {
                formatter.write_str("no first-parent commit raises the product-version counter sum")
            }
            Self::Source(detail) => formatter.write_str(detail),
        }
    }
}

/// Walk `commits` back from HEAD, whose counters sum to `head_sum`, to the
/// newest raise. Equal steps, such as a gap retirement, carry the sum and keep
/// walking; a decrease refuses. Iteration stops at the first raise or shallow
/// boundary, so the caller may abandon the remaining history.
pub(super) fn floor(
    head_sum: u64,
    commits: impl IntoIterator<Item = Result<Commit, String>>,
) -> Result<Floor, HistoryError> {
    let mut sum = i128::from(head_sum);
    for commit in commits {
        let commit = commit.map_err(HistoryError::Source)?;
        if commit.parent == Parent::Shallow {
            return Ok(Floor::Truncated {
                commit: commit.commit,
                seconds: commit.seconds,
            });
        }
        let parent = sum - i128::from(commit.change.new.unwrap_or(0))
            + i128::from(commit.change.old.unwrap_or(0));
        let (Ok(parent), Ok(current)) = (u64::try_from(parent), u64::try_from(sum)) else {
            return Err(HistoryError::Inconsistent {
                commit: commit.commit,
            });
        };
        if commit.parent == Parent::Root && parent != 0 {
            return Err(HistoryError::Inconsistent {
                commit: commit.commit,
            });
        }
        if current > parent {
            return Ok(Floor::Raised {
                commit: commit.commit,
                seconds: commit.seconds,
            });
        }
        if current < parent {
            return Err(HistoryError::Decrease {
                commit: commit.commit,
                parent,
                sum: current,
            });
        }
        sum = i128::from(parent);
    }
    Err(HistoryError::Unraised)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(in crate::source) fn commit(
        name: &str,
        seconds: i64,
        parent: Parent,
        (old, new): (Option<u64>, Option<u64>),
    ) -> Commit {
        Commit {
            commit: name.to_owned(),
            seconds,
            parent,
            change: Change { old, new },
        }
    }

    fn walk(head_sum: u64, commits: Vec<Commit>) -> Result<Floor, HistoryError> {
        floor(head_sum, commits.into_iter().map(Ok))
    }

    fn raised(commit: &str, seconds: i64) -> Result<Floor, HistoryError> {
        Ok(Floor::Raised {
            commit: commit.to_owned(),
            seconds,
        })
    }

    #[test]
    fn the_floor_is_the_newest_raise_on_the_first_parent_chain() {
        let history = vec![
            commit("raise", 300, Parent::First, (Some(132), Some(133))),
            commit("root", 100, Parent::Root, (None, Some(132))),
        ];
        assert_eq!(walk(133, history.clone()), raised("raise", 300));
        assert_eq!(walk(132, history[1..].to_vec()), raised("root", 100));
    }

    #[test]
    fn the_walk_stops_reading_at_the_raise() {
        let mut read = 0;
        let history = [
            commit("raise", 300, Parent::First, (Some(1), Some(2))),
            commit("unreadable", 200, Parent::First, (None, None)),
        ];
        let floor = floor(
            2,
            history.into_iter().map(|commit| {
                read += 1;
                if commit.commit == "unreadable" {
                    Err("read past the floor".to_owned())
                } else {
                    Ok(commit)
                }
            }),
        );
        assert_eq!(floor, raised("raise", 300));
        assert_eq!(read, 1);
    }

    #[test]
    fn a_gap_retirement_keeps_the_sum_and_the_floor() {
        let history = vec![
            commit("retire", 500, Parent::First, (Some(132), Some(132))),
            commit("raise", 300, Parent::First, (Some(131), Some(132))),
        ];
        assert_eq!(walk(132, history), raised("raise", 300));
    }

    #[test]
    fn a_first_parent_sum_decrease_refuses() {
        let history = vec![
            commit("lower", 500, Parent::First, (Some(132), Some(131))),
            commit("raise", 300, Parent::First, (Some(131), Some(132))),
        ];
        assert_eq!(
            walk(131, history),
            Err(HistoryError::Decrease {
                commit: "lower".to_owned(),
                parent: 132,
                sum: 131,
            })
        );
    }

    #[test]
    fn shallow_history_ending_before_a_raise_is_truncated() {
        let history = vec![
            commit("retire", 500, Parent::First, (Some(132), Some(132))),
            commit("boundary", 400, Parent::Shallow, (None, Some(132))),
        ];
        assert_eq!(
            walk(132, history),
            Ok(Floor::Truncated {
                commit: "boundary".to_owned(),
                seconds: 400,
            })
        );
    }

    #[test]
    fn changes_that_do_not_reconcile_or_never_raise_refuse() {
        assert_eq!(
            walk(5, vec![commit("bad", 1, Parent::First, (None, Some(9)))]),
            Err(HistoryError::Inconsistent {
                commit: "bad".to_owned()
            })
        );
        assert_eq!(
            walk(5, vec![commit("root", 1, Parent::Root, (None, Some(4)))]),
            Err(HistoryError::Inconsistent {
                commit: "root".to_owned()
            })
        );
        assert_eq!(
            walk(0, vec![commit("root", 1, Parent::Root, (None, Some(0)))]),
            Err(HistoryError::Unraised)
        );
    }
}
