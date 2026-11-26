//! Work a lane's worktree holds that no landed commit carries: dirty paths,
//! commits the integration baseline lacks, and an unfinished Git operation.
//!
//! Every read here is lock-free and offline. The baseline is the
//! remote-tracking reference as last fetched, so inspecting a lane never
//! reaches the network.

use std::hash::{DefaultHasher, Hash, Hasher};

use super::*;

/// Git operations that leave a worktree half-applied until continued or
/// aborted, each with the per-worktree state path that marks it.
const UNFINISHED: [(&str, &str); 5] = [
    ("merge", "MERGE_HEAD"),
    ("rebase", "rebase-merge"),
    ("rebase", "rebase-apply"),
    ("cherry-pick", "CHERRY_PICK_HEAD"),
    ("revert", "REVERT_HEAD"),
];

/// Commits on HEAD the integration baseline lacks.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Unlanded {
    /// How many commits the baseline lacks.
    pub total: usize,
    /// The newest of them as `abbreviated-id subject`, newest first, at most
    /// the requested rows.
    pub rows: Vec<String>,
    /// The baseline commit they were measured against; absent when the lane
    /// tracks no remote, which leaves no commit unlanded.
    pub baseline: Option<String>,
}

/// One lane worktree's loose work.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LooseWork {
    /// The HEAD commit.
    pub head: String,
    /// Every staged, unstaged, and non-ignored untracked path, sorted.
    pub dirty: Vec<String>,
    pub unlanded: Unlanded,
    /// The unfinished Git operation, if one is in progress.
    pub operation: Option<&'static str>,
}

impl LooseWork {
    /// Whether any work here is loose.
    #[must_use]
    pub fn is_loose(&self) -> bool {
        !self.dirty.is_empty() || self.unlanded.total > 0 || self.operation.is_some()
    }

    /// A process-local identity of this loose state: equal states share it,
    /// and a moved HEAD or baseline, a changed dirty set, or a different
    /// operation changes it.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        (
            &self.head,
            &self.unlanded.baseline,
            &self.dirty,
            self.operation,
        )
            .hash(&mut hasher);
        hasher.finish()
    }
}

/// Read the loose work of the worktree at `root`.
pub fn loose_work(root: &Path, ambient: &Ambient) -> Result<LooseWork, Error> {
    let root = Worktree::new(root, ambient);
    let dirty = dirty_paths(root)?
        .all()
        .into_iter()
        .map(str::to_owned)
        .collect();
    Ok(LooseWork {
        head: required_read(root, &["rev-parse", "HEAD"], "read HEAD")?,
        dirty,
        unlanded: unlanded_in(root, 0)?,
        operation: operation_in(root)?,
    })
}

/// The commits on HEAD of the worktree at `root` that its integration
/// baseline lacks, keeping the newest `rows` of them.
pub fn unlanded(root: &Path, ambient: &Ambient, rows: usize) -> Result<Unlanded, Error> {
    unlanded_in(Worktree::new(root, ambient), rows)
}

/// The unfinished Git operation in the worktree at `root`, if any.
pub fn unfinished_operation(root: &Path, ambient: &Ambient) -> Result<Option<&'static str>, Error> {
    operation_in(Worktree::new(root, ambient))
}

fn unlanded_in(root: Worktree<'_>, rows: usize) -> Result<Unlanded, Error> {
    let Some(target) = resolve_target(root)? else {
        return Ok(Unlanded::default());
    };
    let baseline = required_read(
        root,
        &["rev-parse", &target.baseline],
        "read integration baseline",
    )?;
    let range = format!("{baseline}..HEAD");
    let total = required_read(
        root,
        &["rev-list", "--count", &range],
        "count unlanded commits",
    )?
    .parse::<usize>()
    .map_err(|error| Error::fault("git::ahead-invalid", error.to_string()))?;
    let rows = if total == 0 || rows == 0 {
        Vec::new()
    } else {
        required_read(
            root,
            &["log", "-n", &rows.to_string(), "--format=%h %s", &range],
            "list unlanded commits",
        )?
        .lines()
        .map(str::to_owned)
        .collect()
    };
    Ok(Unlanded {
        total,
        rows,
        baseline: Some(baseline),
    })
}

fn operation_in(root: Worktree<'_>) -> Result<Option<&'static str>, Error> {
    let git_dir = PathBuf::from(required_read(
        root,
        &["rev-parse", "--absolute-git-dir"],
        "resolve worktree Git directory",
    )?);
    Ok(UNFINISHED
        .iter()
        .find(|(_, marker)| git_dir.join(marker).exists())
        .map(|(operation, _)| *operation))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loose() -> LooseWork {
        LooseWork {
            head: "a".repeat(40),
            dirty: vec!["src/lib.rs".to_owned()],
            unlanded: Unlanded {
                total: 1,
                rows: vec!["abc1234 work".to_owned()],
                baseline: Some("b".repeat(40)),
            },
            operation: None,
        }
    }

    #[test]
    fn each_kind_of_loose_work_is_loose_and_moves_the_fingerprint() {
        let clean = LooseWork {
            head: "a".repeat(40),
            ..LooseWork::default()
        };
        assert!(!clean.is_loose());
        let base = loose();
        assert!(base.is_loose());
        let dirty = LooseWork {
            dirty: vec!["src/main.rs".to_owned()],
            ..base.clone()
        };
        let moved = LooseWork {
            head: "c".repeat(40),
            ..base.clone()
        };
        let rebasing = LooseWork {
            operation: Some("rebase"),
            ..base.clone()
        };
        let rebased = LooseWork {
            unlanded: Unlanded {
                baseline: Some("d".repeat(40)),
                ..base.unlanded.clone()
            },
            ..base.clone()
        };
        for changed in [dirty, moved, rebasing, rebased] {
            assert_ne!(changed.fingerprint(), base.fingerprint(), "{changed:?}");
        }
        let reread = LooseWork {
            unlanded: Unlanded {
                rows: Vec::new(),
                ..base.unlanded.clone()
            },
            ..base.clone()
        };
        assert_eq!(
            reread.fingerprint(),
            base.fingerprint(),
            "how many rows a reader keeps is not the state"
        );
    }
}
