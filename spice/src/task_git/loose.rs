//! Work a lane's worktree holds that no landed commit carries: dirty paths,
//! commits the integration baseline lacks, and an unfinished Git operation.
//!
//! Every read here is lock-free and offline. The baseline is the
//! remote-tracking reference as last fetched, so inspecting a lane never
//! reaches the network.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Read as _;

use super::*;

mod snapshot;

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

/// Read the loose work of the worktree at `root`, with `native_system` the
/// system config native Git reads, as
/// [`crate::git_shadow::native_system_config`] resolves it.
pub fn loose_work(
    root: &Path,
    ambient: &Ambient,
    native_system: Option<&Path>,
) -> Result<LooseWork, Error> {
    snapshot::read(root, ambient, native_system)
}

fn hash_worktree_path(root: &Path, path: &str, hasher: &mut DefaultHasher) -> Result<(), Error> {
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
        let mut hasher = DefaultHasher::new();
        hash_worktree_path(root, path, &mut hasher).expect("worktree path hashes");
        hasher.finish()
    }

    #[test]
    fn gix_snapshot_covers_clean_dirty_untracked_unlanded_and_failures() {
        let scratch = Scratch::new();
        let root = &scratch.0;
        let ambient = Ambient::default();
        crate::test_repository::initialize(root);
        fs::write(root.join(".gitignore"), "ignored\n").expect("write fixture ignore");
        fs::write(root.join("tracked"), "first").expect("write tracked fixture");
        crate::test_repository::stage_all(root);
        let baseline = crate::test_repository::commit(root, "baseline");
        fs::write(
            root.join(".git/refs/heads/baseline"),
            format!("{baseline}\n"),
        )
        .expect("write baseline reference");
        let config = fs::read_to_string(root.join(".git/config")).expect("read fixture config");
        fs::write(
            root.join(".git/config"),
            format!("{config}\n[branch \"main\"]\n\tremote = .\n\tmerge = refs/heads/baseline\n"),
        )
        .expect("configure local integration baseline");

        let clean = snapshot::read(root, &ambient, None).expect("clean snapshot reads");
        assert!(!clean.is_loose());
        assert_eq!(clean.unlanded.total, 0);

        fs::write(root.join("tracked"), "next!").expect("write next tracked content");
        crate::test_repository::stage(root, &["tracked"]);
        crate::test_repository::commit(root, "local work");
        let unlanded = snapshot::read(root, &ambient, None).expect("unlanded snapshot reads");
        assert!(unlanded.is_loose());
        assert_eq!(unlanded.unlanded.total, 1);

        fs::write(root.join("tracked"), "third").expect("write staged content");
        crate::test_repository::stage(root, &["tracked"]);
        let staged = snapshot::read(root, &ambient, None).expect("staged snapshot reads");
        assert_eq!(staged.dirty, ["tracked"]);

        fs::write(root.join("tracked"), "other").expect("write unstaged same-length content");
        let unstaged = snapshot::read(root, &ambient, None).expect("unstaged snapshot reads");
        assert_eq!(unstaged.dirty, ["tracked"]);
        assert_ne!(unstaged.dirty_fingerprint, staged.dirty_fingerprint);

        fs::write(root.join("untracked"), "loose").expect("write untracked content");
        fs::write(root.join("ignored"), "ignored").expect("write ignored content");
        let index_path = root.join(".git/index");
        let index_before = fs::read(&index_path).expect("read index before inspection");
        let untracked = snapshot::read(root, &ambient, None).expect("untracked snapshot reads");
        assert_eq!(untracked.dirty, ["tracked", "untracked"]);
        assert_eq!(
            fs::read(&index_path).expect("read index after inspection"),
            index_before,
            "inspection never writes the index"
        );

        fs::write(&index_path, "not an index").expect("corrupt fixture index");
        assert!(
            snapshot::read(root, &ambient, None).is_err(),
            "a corrupt index refuses"
        );
    }

    #[test]
    fn detached_head_still_reports_an_unfinished_operation() {
        let scratch = Scratch::new();
        let root = &scratch.0;
        let ambient = Ambient::default();
        crate::test_repository::initialize(root);
        fs::write(root.join("tracked"), "first").expect("write tracked fixture");
        crate::test_repository::stage_all(root);
        let head = crate::test_repository::commit(root, "baseline");
        fs::write(root.join(".git/HEAD"), format!("{head}\n")).expect("detach fixture HEAD");
        fs::create_dir(root.join(".git/rebase-merge")).expect("mark rebase in progress");

        let work = snapshot::read(root, &ambient, None).expect("detached operation snapshot reads");
        assert_eq!(work.operation, Some("rebase"));
        assert_eq!(work.unlanded, Unlanded::default());
    }

    #[test]
    fn native_pairing_uses_ambient_home_for_a_dotted_branch() {
        let scratch = Scratch::new();
        let root = &scratch.0;
        crate::test_repository::initialize(root);
        fs::write(root.join("tracked"), "baseline").expect("write tracked fixture");
        crate::test_repository::stage_all(root);
        let baseline = crate::test_repository::commit(root, "baseline");
        fs::write(root.join("tracked"), "local").expect("write local commit");
        crate::test_repository::stage(root, &["tracked"]);
        let head = crate::test_repository::commit(root, "local");
        fs::write(root.join(".git/HEAD"), "ref: refs/heads/topic.1\n")
            .expect("select dotted fixture branch");
        fs::write(root.join(".git/refs/heads/topic.1"), format!("{head}\n"))
            .expect("write dotted fixture branch");
        fs::write(
            root.join(".git/refs/heads/baseline"),
            format!("{baseline}\n"),
        )
        .expect("write integration baseline");

        let home = root.join("ambient-home");
        fs::create_dir(&home).expect("create ambient home");
        let pairing = home.join("pairing.config");
        fs::write(
            &pairing,
            "[branch \"topic.1\"]\n\tremote = .\n\tmerge = refs/heads/baseline\n",
        )
        .expect("write native pairing");
        fs::write(
            home.join(".gitconfig"),
            format!(
                "[includeIf \"onbranch:topic.1\"]\n\tpath = {}\n",
                pairing.display()
            ),
        )
        .expect("write conditional native config");
        let system = root.join("system.config");
        fs::write(&system, "").expect("write native system config");
        let shadow = root.join("shadow.config");
        fs::write(
            &shadow,
            crate::git_shadow::shadow_content("topic.1", system.to_str().expect("utf8 fixture")),
        )
        .expect("write generated shadow");
        let ambient = Ambient::new([
            ("HOME", home.as_os_str()),
            ("GIT_CONFIG_SYSTEM", shadow.as_os_str()),
        ]);

        let work =
            snapshot::read(root, &ambient, Some(&system)).expect("native pairing snapshot reads");
        assert_eq!(work.unlanded.total, 1);
        assert_eq!(work.unlanded.baseline, Some(baseline.to_string()));
    }

    #[test]
    fn unshadowed_system_pairing_ignores_the_ambient_override() {
        let scratch = Scratch::new();
        let root = &scratch.0;
        crate::test_repository::initialize(root);
        fs::write(root.join("tracked"), "baseline").expect("write tracked fixture");
        crate::test_repository::stage_all(root);
        let baseline = crate::test_repository::commit(root, "baseline");
        fs::write(root.join("tracked"), "local").expect("write local commit");
        crate::test_repository::stage(root, &["tracked"]);
        let head = crate::test_repository::commit(root, "local");
        fs::write(root.join(".git/HEAD"), "ref: refs/heads/topic\n")
            .expect("select fixture branch");
        fs::write(root.join(".git/refs/heads/topic"), format!("{head}\n"))
            .expect("write fixture branch");
        fs::write(
            root.join(".git/refs/heads/baseline"),
            format!("{baseline}\n"),
        )
        .expect("write integration baseline");
        let system = root.join("system.config");
        fs::write(
            &system,
            "[branch \"topic\"]\n\tremote = .\n\tmerge = refs/heads/baseline\n",
        )
        .expect("write native system pairing");
        let override_config = root.join("override.config");
        fs::write(
            &override_config,
            "[branch \"topic\"]\n\tremote = .\n\tmerge = refs/heads/missing\n",
        )
        .expect("write ignored system override");
        let ambient = Ambient::new([("GIT_CONFIG_SYSTEM", override_config.as_os_str())]);

        let work =
            snapshot::read(root, &ambient, Some(&system)).expect("system pairing snapshot reads");
        assert_eq!(work.unlanded.total, 1);
        assert_eq!(work.unlanded.baseline, Some(baseline.to_string()));
    }

    #[test]
    fn configured_remote_without_a_fetch_url_refuses() {
        let scratch = Scratch::new();
        let root = &scratch.0;
        crate::test_repository::initialize(root);
        fs::write(root.join("tracked"), "baseline").expect("write tracked fixture");
        crate::test_repository::stage_all(root);
        crate::test_repository::commit(root, "baseline");
        let config = fs::read_to_string(root.join(".git/config")).expect("read fixture config");
        fs::write(
            root.join(".git/config"),
            format!(
                "{config}\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n[remote \"origin\"]\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n"
            ),
        )
        .expect("configure URL-less remote");

        assert!(
            snapshot::read(root, &Ambient::default(), None).is_err(),
            "a configured remote without a fetch URL refuses"
        );
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
