use std::collections::BTreeSet;

use super::*;

/// The staged, unstaged, and non-ignored untracked paths of one lane, each
/// sorted and unique.
///
/// `git status` may answer a stale untracked directory when its optional
/// untracked cache has not observed a parent-directory metadata change, so
/// untracked paths come from `ls-files`, which reads the tree. Tracked paths
/// come from `status` with untracked reporting off: unlike porcelain
/// `git diff`, which rewrites a stat-refreshed index even under
/// `--no-optional-locks`, it refreshes in memory only, and still ignores a
/// stat-only change.
pub(super) struct DirtyPaths {
    pub(super) staged: Vec<String>,
    pub(super) unstaged: Vec<String>,
    pub(super) untracked: Vec<String>,
}

impl DirtyPaths {
    pub(super) fn is_empty(&self) -> bool {
        self.staged.is_empty() && self.unstaged.is_empty() && self.untracked.is_empty()
    }

    /// Every dirty path once, whichever listings name it.
    pub(super) fn all(&self) -> BTreeSet<&str> {
        self.staged
            .iter()
            .chain(&self.unstaged)
            .chain(&self.untracked)
            .map(String::as_str)
            .collect()
    }

    pub(super) fn rendered(&self) -> String {
        self.all().into_iter().collect::<Vec<_>>().join("\n")
    }
}

/// Read the lane's dirty paths without writing its index, so deciding
/// cleanliness never holds the index lock a concurrent Git command needs.
pub(super) fn dirty_paths(root: Worktree<'_>) -> Result<DirtyPaths, Error> {
    let tracked = listing(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=no",
            "--no-renames",
        ],
    )?;
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    for entry in tracked.split_terminator('\0') {
        let (Some(&[index, work, b' ']), Some(path)) = (
            entry.as_bytes().get(..3),
            entry.get(3..).filter(|path| !path.is_empty()),
        ) else {
            return Err(Error::fault(
                "git::path-list-invalid",
                format!("Git returned a malformed status entry {entry:?}"),
            ));
        };
        if index != b' ' {
            staged.push(path.to_owned());
        }
        if work != b' ' {
            unstaged.push(path.to_owned());
        }
    }
    let untracked = listing(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    Ok(DirtyPaths {
        staged: sorted(staged),
        unstaged: sorted(unstaged),
        untracked: sorted(
            untracked
                .split_terminator('\0')
                .map(str::to_owned)
                .collect(),
        ),
    })
}

/// One NUL-terminated Git listing, read without optional locks.
fn listing(root: Worktree<'_>, command: &[&str]) -> Result<String, Error> {
    let args = [&["--no-optional-locks"][..], command].concat();
    let output = run(root, &args)?;
    if !output.success() {
        return Err(Error::fault(
            "git::command-failed",
            output.detail("inspect worktree"),
        ));
    }
    if !output.stdout.is_empty() && !output.stdout.ends_with('\0') {
        return Err(Error::fault(
            "git::path-list-invalid",
            "Git returned an unterminated dirty path list",
        ));
    }
    Ok(output.stdout)
}

fn sorted(mut paths: Vec<String>) -> Vec<String> {
    paths.sort();
    paths.dedup();
    paths
}
