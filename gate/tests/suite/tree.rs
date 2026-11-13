//! Git selections exercised through in-process repositories.

use std::fs;
#[cfg(target_family = "unix")]
use std::os::unix::fs::{PermissionsExt as _, symlink};

use gate::{Ignore, Jurisdiction, Selector, TreeError};

use super::support::{Tree, git_repository};

fn repository(label: &str) -> Tree {
    git_repository(label)
}

fn commit_all(tree: &Tree, message: &str) -> String {
    tree.stage_all();
    tree.commit(message)
}

#[test]
fn staged_and_revision_sources_never_read_checkout_only_bytes() {
    let tree = repository("git-sources");
    tree.write("src/lib.rs", "committed\n");
    let committed = commit_all(&tree, "committed state");

    tree.write("src/lib.rs", "staged\n");
    tree.write("bin/tool.sh", "#!/bin/sh\nprintf 'staged tool\\n'\n");
    #[cfg(target_family = "unix")]
    fs::set_permissions(tree.path("bin/tool.sh"), fs::Permissions::from_mode(0o755))
        .expect("make the staged fixture executable");
    tree.stage(&["src/lib.rs", "bin/tool.sh"]);
    tree.write("src/lib.rs", "unstaged\n");
    tree.write("notes.untracked", "untracked\n");

    let staged = Selector::staged()
        .materialize(&tree.root)
        .expect("materialize the captured index");
    let repeated = Selector::staged()
        .materialize(&tree.root)
        .expect("materialize the same index again");
    let revision = Selector::revision(&committed)
        .materialize(&tree.root)
        .expect("materialize the committed tree");
    let repeated_revision = Selector::revision(&committed)
        .materialize(&tree.root)
        .expect("materialize the same commit again");

    assert_eq!(
        fs::read(staged.root().join("src/lib.rs")).expect("read selected staged bytes"),
        b"staged\n"
    );
    assert_eq!(
        fs::read(revision.root().join("src/lib.rs")).expect("read selected revision bytes"),
        b"committed\n"
    );
    assert!(!staged.root().join("notes.untracked").exists());
    assert!(!revision.root().join("bin/tool.sh").exists());

    let ignore = Ignore::parse("").expect("declare an empty ignore policy");
    let staged_walk = Jurisdiction::walk(staged.root(), &ignore).expect("walk staged selection");
    let repeated_walk =
        Jurisdiction::walk(repeated.root(), &ignore).expect("walk repeated selection");
    let revision_walk =
        Jurisdiction::walk(revision.root(), &ignore).expect("walk revision selection");
    let repeated_revision_walk = Jurisdiction::walk(repeated_revision.root(), &ignore)
        .expect("walk repeated revision selection");
    assert_eq!(staged_walk.inventory(), repeated_walk.inventory());
    assert_eq!(
        staged_walk.tree_digest().expect("digest staged selection"),
        repeated_walk
            .tree_digest()
            .expect("digest repeated selection")
    );
    assert_eq!(
        revision_walk.inventory(),
        repeated_revision_walk.inventory()
    );
    assert_eq!(
        revision_walk
            .tree_digest()
            .expect("digest revision selection"),
        repeated_revision_walk
            .tree_digest()
            .expect("digest repeated revision selection")
    );

    #[cfg(target_family = "unix")]
    {
        assert_eq!(
            fs::metadata(staged.root())
                .expect("inspect selected root")
                .permissions()
                .mode()
                & 0o777,
            0o500
        );
        assert_eq!(
            fs::metadata(staged.root().join("src/lib.rs"))
                .expect("inspect selected regular file")
                .permissions()
                .mode()
                & 0o777,
            0o400
        );
        assert_eq!(
            fs::metadata(staged.root().join("bin/tool.sh"))
                .expect("inspect selected executable")
                .permissions()
                .mode()
                & 0o777,
            0o500
        );
    }

    let revision_root = revision.root().to_path_buf();
    drop(revision);
    assert!(
        !revision_root.exists(),
        "dropping a revision selection removes its root"
    );
}

/// A staged selection is refilled in place instead of rewritten, because the
/// modification times of its unchanged files are what keeps a build reading
/// the tree from recompiling everything on every gate run.
#[test]
fn a_staged_selection_keeps_the_times_of_bytes_that_did_not_change() {
    let tree = repository("staged-retention");
    tree.write("src/lib.rs", "first\n");
    tree.write("src/other.rs", "steady\n");
    commit_all(&tree, "committed state");

    let staged = Selector::staged()
        .materialize(&tree.root)
        .expect("materialize the captured index");
    let staged_root = staged.root().to_path_buf();
    let stamp = |name: &str| {
        fs::metadata(staged_root.join(name))
            .expect("inspect a selected file")
            .modified()
            .expect("selected file modification time")
    };
    let (changing, steady) = (stamp("src/lib.rs"), stamp("src/other.rs"));
    drop(staged);
    assert!(staged_root.exists(), "a staged selection root is retained");

    tree.write("src/lib.rs", "second\n");
    tree.stage(&["src/lib.rs"]);
    let refilled = Selector::staged()
        .materialize(&tree.root)
        .expect("refill the retained selection");
    assert_eq!(refilled.root(), staged_root, "one root per repository");
    assert_eq!(
        fs::read(refilled.root().join("src/lib.rs")).expect("read refilled bytes"),
        b"second\n"
    );
    assert!(
        stamp("src/lib.rs") > changing,
        "changed bytes are stamped newer than anything built from the bytes they replace"
    );
    assert_eq!(
        stamp("src/other.rs"),
        steady,
        "unchanged bytes keep their modification time"
    );
}

#[test]
fn revision_refusals_name_the_requested_revision() {
    let tree = repository("revision-refusals");
    tree.write("tracked.txt", "one\n");
    commit_all(&tree, "one commit");

    let absent = Selector::revision("does-not-exist")
        .materialize(&tree.root)
        .expect_err("an absent revision is refused");
    assert!(matches!(
        &absent,
        TreeError::RevisionUnresolved { revision, .. } if revision == "does-not-exist"
    ));
    assert!(absent.to_string().contains("does-not-exist"));

    let non_commit = Selector::revision("HEAD^{tree}")
        .materialize(&tree.root)
        .expect_err("a tree revision is refused");
    assert!(matches!(
        &non_commit,
        TreeError::RevisionNotCommit { revision, .. } if revision == "HEAD^{tree}"
    ));
    assert!(non_commit.to_string().contains("HEAD^{tree}"));

    let range = Selector::revision("HEAD..HEAD")
        .materialize(&tree.root)
        .expect_err("a revision range is refused");
    assert!(matches!(range, TreeError::RevisionUnresolved { .. }));

    tree.create_ambiguous_ref("colliding");
    let ambiguous = Selector::revision("colliding")
        .materialize(&tree.root)
        .expect_err("an ambiguous branch and tag name is refused");
    assert!(matches!(
        ambiguous,
        TreeError::RevisionUnresolved { revision, .. } if revision == "colliding"
    ));
}

#[test]
fn a_reftable_repository_is_refused_by_name_before_head_is_read() {
    let tree = repository("reftable-refusal");
    tree.write("tracked.txt", "one\n");
    commit_all(&tree, "one commit");
    let config = tree.path(".git/config");
    let mut body = fs::read_to_string(&config).expect("read the fixture config");
    body = body.replace("repositoryformatversion = 0", "repositoryformatversion = 1");
    body.push_str("[extensions]\n\trefStorage = reftable\n");
    fs::write(&config, body).expect("declare reftable storage");

    let refused = Selector::staged()
        .materialize(&tree.root)
        .expect_err("a reftable repository is refused");
    assert!(matches!(
        &refused,
        TreeError::ReferenceStorage { path, format } if path == &tree.root && format == "reftable"
    ));
    assert!(refused.to_string().contains("--ref-format=files"));
}

#[cfg(target_family = "unix")]
#[test]
fn a_tracked_link_is_admitted_and_the_private_root_never_carries_it() {
    let tree = repository("tracked-link");
    tree.write("target.txt", "target\n");
    commit_all(&tree, "ordinary file");

    symlink("target.txt", tree.path("linked.txt")).expect("create a fixture symlink");
    tree.stage(&["linked.txt"]);
    let staged = Selector::staged()
        .materialize(&tree.root)
        .expect("a staged symlink is admitted");
    assert_eq!(staged.paths(), ["target.txt"]);
    assert!(
        fs::symlink_metadata(staged.root().join("linked.txt")).is_err(),
        "the private root holds no entry for the link"
    );

    let committed = commit_all(&tree, "tracked link");
    let revision = Selector::revision(&committed)
        .materialize(&tree.root)
        .expect("a committed symlink is admitted");
    assert_eq!(revision.paths(), ["target.txt"]);
}

#[test]
fn a_submodule_is_named_before_materialization() {
    let tree = repository("unsupported-modes");
    tree.write("target.txt", "target\n");
    commit_all(&tree, "ordinary file");
    tree.stage_all();
    tree.stage_submodule("vendor/dependency", &tree.head());
    let submodule = Selector::staged()
        .materialize(&tree.root)
        .expect_err("a staged submodule is refused");
    assert!(matches!(
        submodule,
        TreeError::Submodule { path } if path == "vendor/dependency"
    ));
}

#[test]
fn a_missing_staged_blob_produces_no_selected_tree() {
    let tree = repository("missing-object");
    tree.write("lost.txt", "lost\n");
    tree.stage(&["lost.txt"]);
    let object = tree.staged_object("lost.txt");
    let object_path = tree
        .root
        .join(".git/objects")
        .join(&object[..2])
        .join(&object[2..]);
    fs::remove_file(&object_path).expect("remove the loose staged object");

    let refused = Selector::staged()
        .materialize(&tree.root)
        .expect_err("a missing staged blob is refused");
    assert!(matches!(
        refused,
        TreeError::MissingObject { path, .. } if path == "lost.txt"
    ));
}
