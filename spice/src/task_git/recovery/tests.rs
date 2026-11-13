use super::*;

/// One scratch lane directory beside one recovery object store.
struct Lane {
    root: PathBuf,
    ambient: Ambient,
}

impl Lane {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "spice-task-recovery-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(root.join("store").join(OBJECT_DIRECTORY))
            .expect("fixture object store exists");
        fs::create_dir_all(root.join("lane")).expect("fixture lane exists");
        Self {
            root,
            ambient: Ambient::new(std::iter::empty::<(String, String)>()),
        }
    }

    fn lane(&self) -> PathBuf {
        self.root.join("lane")
    }

    fn store(&self) -> PathBuf {
        self.root.join("store")
    }

    fn write(&self, path: &str, bytes: &[u8]) {
        let path = self.lane().join(path);
        fs::create_dir_all(path.parent().expect("fixture path has a parent"))
            .expect("fixture parent exists");
        fs::write(path, bytes).expect("fixture file is written");
    }

    /// Seal `paths` as untracked entries, as capture does.
    fn seal(&self, paths: &[&str]) -> Vec<PathState> {
        let lane = self.lane();
        let paths = paths
            .iter()
            .map(|path| (*path).to_owned())
            .collect::<Vec<_>>();
        capture_paths(
            Worktree::new(&lane, &self.ambient),
            &self.store(),
            &paths,
            false,
        )
        .expect("fixture paths seal")
    }

    fn quarantine(&self, sealed: &[PathState]) {
        let lane = self.lane();
        for state in sealed {
            quarantine_named_path(Worktree::new(&lane, &self.ambient), &state.path)
                .expect("sealed path quarantines");
        }
    }

    fn restore(&self, sealed: &[PathState]) -> Result<(), Error> {
        let lane = self.lane();
        let worktree = Worktree::new(&lane, &self.ambient);
        restore_paths(worktree, &self.store(), sealed)?;
        verify_paths(worktree, &self.store(), sealed)
    }
}

impl Drop for Lane {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn quarantine_removes_the_directories_it_empties_and_restore_returns_them() {
    let lane = Lane::new("prune");
    lane.write("kept/sibling.rs", b"sibling");
    lane.write("kept/fresh/deeper/probe.rs", b"probe");
    lane.write("kept/fresh/deeper/second.rs", b"second");
    lane.write("lonely/only.rs", b"only");
    let sealed = lane.seal(&[
        "kept/fresh/deeper/probe.rs",
        "kept/fresh/deeper/second.rs",
        "lonely/only.rs",
    ]);

    lane.quarantine(&sealed);

    // Every directory the sealed paths alone occupied is gone, up to the
    // first one that still holds something else.
    assert!(!lane.lane().join("kept/fresh").exists());
    assert!(!lane.lane().join("lonely").exists());
    assert_eq!(
        fs::read(lane.lane().join("kept/sibling.rs")).expect("sibling survives"),
        b"sibling"
    );

    lane.restore(&sealed)
        .expect("restore recreates the missing parents");
    assert_eq!(
        fs::read(lane.lane().join("kept/fresh/deeper/probe.rs")).expect("probe returns"),
        b"probe"
    );
    assert_eq!(
        fs::read(lane.lane().join("lonely/only.rs")).expect("only returns"),
        b"only"
    );
}

#[cfg(unix)]
#[test]
fn restore_refuses_a_parent_that_is_not_a_real_directory() {
    let lane = Lane::new("unsafe-parent");
    lane.write("fresh/deeper/probe.rs", b"probe");
    let sealed = lane.seal(&["fresh/deeper/probe.rs"]);
    lane.quarantine(&sealed);
    let outside = lane.root.join("outside");
    fs::create_dir_all(outside.join("deeper")).expect("outside target exists");

    // A link where the parent stood would carry the write out of the lane.
    std::os::unix::fs::symlink(&outside, lane.lane().join("fresh")).expect("parent is linked");
    let linked = lane.restore(&sealed).expect_err("a linked parent refuses");
    assert_eq!(linked.reason(), "git::recovery-symlink-traversal");
    assert!(!outside.join("deeper/probe.rs").exists());

    fs::remove_file(lane.lane().join("fresh")).expect("link is removed");
    fs::write(lane.lane().join("fresh"), b"file").expect("parent is a file");
    let file = lane.restore(&sealed).expect_err("a file parent refuses");
    assert_eq!(file.reason(), "git::recovery-symlink-traversal");
    assert_eq!(
        fs::read(lane.lane().join("fresh")).expect("the file parent is untouched"),
        b"file"
    );
}
