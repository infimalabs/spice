//! Work a lane's worktree holds that no landed commit carries: dirty paths,
//! commits the integration baseline lacks, and an unfinished Git operation.
//!
//! Every read here is lock-free and offline. The baseline is the
//! remote-tracking reference as last fetched, so inspecting a lane never
//! reaches the network.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Read as _;

use super::dirty::DirtyPaths;
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
    /// The staged state and worktree bytes behind `dirty`.
    dirty_fingerprint: u64,
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
    /// and a moved HEAD or baseline, changed dirty paths or bytes, or a
    /// different operation changes it.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        (
            &self.head,
            &self.unlanded.baseline,
            &self.dirty,
            self.dirty_fingerprint,
            self.operation,
        )
            .hash(&mut hasher);
        hasher.finish()
    }
}

/// Read the loose work of the worktree at `root`.
pub fn loose_work(root: &Path, ambient: &Ambient) -> Result<LooseWork, Error> {
    let root = Worktree::new(root, ambient);
    let dirty = dirty_paths(root)?;
    let dirty_fingerprint = dirty_fingerprint(root, &dirty)?;
    let dirty = dirty.all().into_iter().map(str::to_owned).collect();
    Ok(LooseWork {
        head: required_read(root, &["rev-parse", "HEAD"], "read HEAD")?,
        dirty,
        dirty_fingerprint,
        unlanded: unlanded_in(root, 0)?,
        operation: operation_in(root)?,
    })
}

/// Hash the index state and worktree bytes behind every dirty path without
/// writing the index. Git's raw diffs carry staged object ids and mode/status
/// changes; regular files and symlink targets carry unstaged and untracked
/// content. Reads are streamed so one large loose file does not become one
/// large allocation.
fn dirty_fingerprint(root: Worktree<'_>, dirty: &DirtyPaths) -> Result<u64, Error> {
    let mut hasher = DefaultHasher::new();
    if !dirty.staged.is_empty() {
        hash_raw_diff(
            root,
            &[
                "diff",
                "--cached",
                "--raw",
                "--no-abbrev",
                "-z",
                "HEAD",
                "--",
            ],
            &mut hasher,
        )?;
    }
    if !dirty.unstaged.is_empty() {
        hash_raw_diff(
            root,
            &["diff", "--raw", "--no-abbrev", "-z", "--"],
            &mut hasher,
        )?;
    }
    for (kind, paths) in [(b'w', &dirty.unstaged), (b'u', &dirty.untracked)] {
        kind.hash(&mut hasher);
        for path in paths {
            path.hash(&mut hasher);
            hash_worktree_path(root, path, &mut hasher)?;
        }
    }
    Ok(hasher.finish())
}

fn hash_raw_diff(
    root: Worktree<'_>,
    command: &[&str],
    hasher: &mut DefaultHasher,
) -> Result<(), Error> {
    let args = [&["--no-optional-locks"][..], command].concat();
    let output = run(root, &args)?;
    if !output.success() {
        return Err(Error::fault(
            "git::command-failed",
            output.detail("fingerprint loose index state"),
        ));
    }
    output.stdout.hash(hasher);
    Ok(())
}

fn hash_worktree_path(
    root: Worktree<'_>,
    path: &str,
    hasher: &mut DefaultHasher,
) -> Result<(), Error> {
    let absolute = root.join(path);
    let metadata = match fs::symlink_metadata(&absolute) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            b'm'.hash(hasher);
            return Ok(());
        }
        Err(error) => {
            return Err(Error::fault(
                "git::loose-path-unreadable",
                format!("read metadata for loose path {path:?}: {error}"),
            ));
        }
    };
    let kind = metadata.file_type();
    if kind.is_symlink() {
        b'l'.hash(hasher);
        fs::read_link(&absolute)
            .map_err(|error| {
                Error::fault(
                    "git::loose-path-unreadable",
                    format!("read loose symlink {path:?}: {error}"),
                )
            })?
            .hash(hasher);
        return Ok(());
    }
    if !kind.is_file() {
        b'o'.hash(hasher);
        metadata.len().hash(hasher);
        return Ok(());
    }
    b'f'.hash(hasher);
    metadata.len().hash(hasher);
    let mut file = fs::File::open(&absolute).map_err(|error| {
        Error::fault(
            "git::loose-path-unreadable",
            format!("open loose path {path:?}: {error}"),
        )
    })?;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            Error::fault(
                "git::loose-path-unreadable",
                format!("read loose path {path:?}: {error}"),
            )
        })?;
        if read == 0 {
            return Ok(());
        }
        hasher.write(&buffer[..read]);
    }
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
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "spice-loose-fingerprint-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            fs::create_dir(&root).expect("scratch directory creates");
            Self(root)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn worktree_path_fingerprint(root: &Path, path: &str) -> u64 {
        let ambient = Ambient::default();
        let mut hasher = DefaultHasher::new();
        hash_worktree_path(Worktree::new(root, &ambient), path, &mut hasher)
            .expect("worktree path hashes");
        hasher.finish()
    }

    #[test]
    fn same_path_bytes_move_the_worktree_fingerprint() {
        let scratch = Scratch::new();
        let path = scratch.0.join("loose.txt");
        fs::write(&path, b"first").expect("first loose bytes write");
        let first = worktree_path_fingerprint(&scratch.0, "loose.txt");
        fs::write(&path, b"other").expect("changed loose bytes write");
        let changed = worktree_path_fingerprint(&scratch.0, "loose.txt");
        assert_ne!(changed, first, "same-length changed bytes move the state");
        fs::write(&path, b"first").expect("original loose bytes restore");
        assert_eq!(
            worktree_path_fingerprint(&scratch.0, "loose.txt"),
            first,
            "equal bytes retain the state"
        );
    }

    fn loose() -> LooseWork {
        LooseWork {
            head: "a".repeat(40),
            dirty: vec!["src/lib.rs".to_owned()],
            dirty_fingerprint: 7,
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
        let same_path_content = LooseWork {
            dirty_fingerprint: 8,
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
        for changed in [dirty, same_path_content, moved, rebasing, rebased] {
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
