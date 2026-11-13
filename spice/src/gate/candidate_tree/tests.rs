use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use crate::repository::Repository;
use crate::test_repository;

use super::{CandidateTree, capture_candidate_tree};

#[test]
fn ordinary_staged_content_resolves_its_candidate_tree() {
    let fixture = Fixture::new("ordinary");
    fixture.write("tracked.txt", b"candidate\n");
    test_repository::stage(&fixture.main, &["tracked.txt"]);

    let candidate = fixture.capture(None);

    assert_eq!(candidate.as_str(), fixture.tree(None));
}

#[test]
fn alternate_index_is_the_only_candidate_tree_authority() {
    let fixture = Fixture::new("alternate-index");
    fixture.write("tracked.txt", b"opening\n");
    fixture.commit_all("opening");
    let alternate = fixture.main.join("alternate.index");
    fs::copy(fixture.main.join(".git/index"), &alternate).expect("copy alternate fixture index");
    fixture.write("tracked.txt", b"alternate candidate\n");
    test_repository::stage_at(&fixture.main, &alternate, &["tracked.txt"]);

    let candidate = fixture.capture(Some(Path::new("alternate.index")));

    assert_eq!(candidate.as_str(), fixture.tree(Some(&alternate)));
    assert_ne!(candidate.as_str(), fixture.tree(None));
}

#[test]
fn staged_deletion_changes_the_candidate_tree() {
    let fixture = Fixture::new("deletion");
    fixture.write("deleted.txt", b"opening\n");
    fixture.write("retained.txt", b"retained\n");
    fixture.commit_all("opening");
    let opening = fixture.tree(None);
    fs::remove_file(fixture.main.join("deleted.txt")).expect("remove tracked fixture file");
    test_repository::stage(&fixture.main, &["deleted.txt"]);

    let candidate = fixture.capture(None);

    assert_eq!(candidate.as_str(), fixture.tree(None));
    assert_ne!(candidate.as_str(), opening);
}

#[cfg(unix)]
#[test]
fn staged_mode_change_changes_the_candidate_tree() {
    let fixture = Fixture::new("mode-change");
    fixture.write("tool.sh", b"#!/bin/sh\nexit 0\n");
    fixture.commit_all("opening");
    let opening = fixture.tree(None);
    let path = fixture.main.join("tool.sh");
    let mut permissions = fs::metadata(&path)
        .expect("read fixture mode")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("change fixture mode");
    test_repository::stage(&fixture.main, &["tool.sh"]);

    let candidate = fixture.capture(None);

    assert_eq!(candidate.as_str(), fixture.tree(None));
    assert_ne!(candidate.as_str(), opening);
    assert_eq!(
        test_repository::index_mode(&fixture.main, "tool.sh"),
        gix::index::entry::Mode::FILE_EXECUTABLE,
        "candidate index did not retain the executable mode"
    );
}

#[test]
fn linked_worktree_uses_its_own_staged_index() {
    let fixture = Fixture::new("linked-worktree");
    fixture.write("tracked.txt", b"opening\n");
    fixture.commit_all("opening");
    test_repository::create_linked_worktree(&fixture.main, &fixture.linked, "candidate-lane");
    fs::write(fixture.linked.join("tracked.txt"), b"linked candidate\n")
        .expect("write linked candidate");
    test_repository::stage(&fixture.linked, &["tracked.txt"]);
    let repository = Repository::discover(&fixture.linked).expect("discover linked worktree");

    let candidate =
        capture_candidate_tree(&repository, None).expect("capture linked candidate tree");

    assert_eq!(
        candidate.as_str(),
        test_repository::write_tree(&fixture.linked, None)
    );
    assert_ne!(candidate.as_str(), fixture.tree(None));
}

#[test]
fn concurrent_capture_never_takes_the_caller_index_lock() {
    let fixture = Fixture::new("concurrent");
    fixture.write("tracked.txt", b"candidate\n");
    test_repository::stage(&fixture.main, &["tracked.txt"]);
    let expected = fixture.tree(None);
    let lock = fixture.main.join(".git/index.lock");
    fs::write(&lock, b"caller-owned\n").expect("hold caller index lock");
    let barrier = Arc::new(Barrier::new(2));
    let captures = (0..2)
        .map(|_| {
            let root = fixture.main.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let repository = Repository::discover(&root).expect("discover concurrent fixture");
                barrier.wait();
                capture_candidate_tree(&repository, None).expect("capture without caller lock")
            })
        })
        .collect::<Vec<_>>();

    for capture in captures {
        assert_eq!(capture.join().expect("join capture").as_str(), expected);
    }
    assert_eq!(
        fs::read(&lock).expect("read caller lock"),
        b"caller-owned\n"
    );
    fs::remove_file(lock).expect("release caller index lock");
}

struct Fixture {
    scratch: PathBuf,
    main: PathBuf,
    linked: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let scratch = loop {
            let candidate = std::env::temp_dir().join(format!(
                "spice-candidate-tree-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("create candidate-tree fixture: {error}"),
            }
        };
        let main = scratch.join("main");
        let linked = scratch.join("linked");
        fs::create_dir(&main).expect("create main worktree");
        test_repository::initialize(&main);
        Self {
            scratch,
            main,
            linked,
        }
    }

    fn write(&self, path: &str, contents: &[u8]) {
        fs::write(self.main.join(path), contents).expect("write fixture path");
    }

    fn commit_all(&self, message: &str) {
        test_repository::stage_all(&self.main);
        test_repository::commit(&self.main, message);
    }

    fn capture(&self, index: Option<&Path>) -> CandidateTree {
        let repository = Repository::discover(&self.main).expect("discover fixture repository");
        capture_candidate_tree(&repository, index.map(Path::as_os_str))
            .expect("capture candidate tree")
    }

    fn tree(&self, index: Option<&Path>) -> String {
        test_repository::write_tree(&self.main, index)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.scratch).expect("remove candidate-tree fixture");
    }
}
