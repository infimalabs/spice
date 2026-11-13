use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use spice::config::{AcquisitionKind, capture_repository};
use spice::config::{
    FlagOverride, FlagValue, PathSegment, Resolved, capture_repository_scoped, descriptor,
    descriptors, load, load_repository, pre_commit_declarations,
};
use wire::{ConfigSource, Name};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

#[cfg(not(windows))]
fn absolute(name: &str) -> String {
    format!("/opt/spice/{name}")
}

#[cfg(windows)]
fn absolute(name: &str) -> String {
    format!("C:/spice/{name}")
}

fn path_source(resolved: &Resolved, expected: &str) -> ConfigSource {
    resolved
        .provenance()
        .iter()
        .find(|row| row.path().to_string() == expected)
        .unwrap_or_else(|| panic!("missing provenance for {expected}"))
        .source()
}

fn repository(scratch: &Scratch, name: &str) -> PathBuf {
    let root = scratch.join(name);
    fs::create_dir_all(root.join(".git")).expect("create repository");
    root
}

fn local_path(root: &Path) -> PathBuf {
    root.join(".git/.spice/config.toml")
}

fn worktree_path(git_dir: &Path) -> PathBuf {
    git_dir.join(".spice/worktree.toml")
}

fn write(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture parent");
    }
    fs::write(path, bytes).expect("write fixture");
}

#[test]
fn missing_documents_are_defaults_repeatably_without_mutation() {
    let scratch = Scratch::new();
    let root = repository(&scratch, "missing documents");
    let start = root.join("src/nested");
    fs::create_dir_all(&start).expect("create nested start");
    let before = snapshot(scratch.path());

    let first = load(&start, &[]).expect("missing documents are empty intent");
    let second = load(&start, &[]).expect("repeated load succeeds");
    let after = snapshot(scratch.path());

    assert_eq!(first, second);
    assert_eq!(first.root(), &spice::config::Root::default());
    assert_eq!(first.registry(), descriptors());
    assert!(
        first
            .provenance()
            .iter()
            .all(|row| row.source() == ConfigSource::Builtin)
    );
    assert_eq!(after, before);
}

#[test]
fn repository_loader_reads_current_tracked_and_local_intent_without_mutation() {
    let scratch = Scratch::new();
    let root = repository(&scratch, "current");
    write(
        root.join("spice.toml"),
        concat!(
            "[policy]\nenv_names = []\n",
            "[taste.polling]\nwords = ['polling']\nmessage = 'use events'\n",
        ),
    );
    write(
        local_path(&root),
        b"[lock.worker]\ndirectory = 'locks/worker'\nshards = 3\n",
    );
    let repository = spice::repository::Repository::discover(&root).expect("fixture repository");

    let before = snapshot(scratch.path());
    let resolved =
        load_repository(&repository, &[]).expect("repository loader admits current typed metadata");
    assert!(resolved.root().policy().env_names().is_empty());
    assert_eq!(resolved.root().taste().bag().tastes().len(), 1);
    assert_eq!(resolved.root().lock().named().len(), 1);
    assert_eq!(
        snapshot(scratch.path()),
        before,
        "repository load is read-only"
    );
}

#[test]
fn captured_pre_commit_declarations_expose_exact_ordered_capability_material() {
    let scratch = Scratch::new();
    let root = repository(&scratch, "pre-commit declarations");
    let tracked = concat!(
        "[policy]\n",
        "pre_commit_builtins = false\n",
        "pre_commit = [\n",
        "  { label = 'first', run = ['tool', 'one'] },\n",
        "  { label = 'second', run = ['tool', 'two'], scopes = { paths = ['src/**'] } },\n",
        "]\n",
    );
    write(root.join("spice.toml"), tracked);
    let repository = spice::repository::Repository::discover(&root).expect("fixture repository");
    let before = snapshot(scratch.path());
    let mounts = spice::cli::MountRegistry::load(&root).expect("invocation mounts capture");
    let invocation = mounts
        .configuration()
        .expect("repository invocation retains configuration");
    let loaded = capture_repository_scoped(&repository, &[], Some(invocation))
        .expect("operation reuses invocation configuration");
    let repeated = capture_repository_scoped(&repository, &[], Some(invocation))
        .expect("same operation reuses the same generation");
    assert!(Arc::ptr_eq(&loaded, &repeated));
    let declarations =
        pre_commit_declarations(loaded.as_ref()).expect("captured commands expose their authority");

    assert_eq!(declarations.source(), ConfigSource::Tracked);
    assert_eq!(declarations.configuration(), loaded.locations().tracked());
    assert_eq!(declarations.document(), tracked);
    assert_eq!(declarations.generation(), loaded.generation());
    assert_eq!(
        declarations
            .commands()
            .iter()
            .map(|step| {
                (
                    step.label(),
                    step.run().join("\0"),
                    step.scopes().paths().join("\0"),
                )
            })
            .collect::<Vec<_>>(),
        [
            ("first", "tool\0one".to_owned(), String::new()),
            ("second", "tool\0two".to_owned(), "src/**".to_owned(),),
        ]
    );
    assert_eq!(
        snapshot(scratch.path()),
        before,
        "capturing executable policy must not execute or mutate it",
    );
}

#[test]
fn main_worktree_reads_only_the_three_exact_paths_and_passes_typed_flags() {
    let scratch = Scratch::new();
    let root = repository(&scratch, "main worktree");
    let start = root.join("src");
    fs::create_dir(&start).expect("create start");
    write(root.join("spice.toml"), b"[lane]\nvendor = \"omp\"\n");
    let container = absolute("container");
    let runner = absolute("runner");
    write(
        worktree_path(&root.join(".git")),
        format!(
            "[vendor.omp]\nmodel = 'lane-model'\neffort = 'xhigh'\n[proof]\nrunner = \"{runner}\"\n"
        ),
    );
    write(
        local_path(&root),
        format!(
            concat!(
                "[listen]\nwindow = \"3m\"\n[lock.alpha]\ndirectory = 'locks/alpha'\nshards = 2\n",
                "[proof]\ncontainer = \"{}\"\n",
            ),
            container
        ),
    );

    for ignored in [
        root.join("config.toml"),
        root.join(".spice/config.toml"),
        root.join(".git/spice.toml"),
        root.join(".git/.spice/config/spice.toml"),
        root.join(".git/.spice/local.toml"),
        root.join(".spice/local.toml"),
        root.join(".spice/config.toml"),
        scratch.join("spice.toml"),
    ] {
        write(ignored, b"[tasks]\nforbidden = true\n");
    }

    let mapping = descriptor(&[PathSegment::Key("lane"), PathSegment::Key("vendor")])
        .and_then(|entry| entry.flag())
        .expect("vendor mapping");
    let vendor = FlagOverride::new(
        mapping,
        FlagValue::Name("gemini".parse::<Name>().expect("valid name")),
    )
    .expect("typed vendor flag");
    let resolved = load(&start, &[vendor]).expect("exact documents resolve");

    assert_eq!(resolved.root().lane().vendor().as_str(), "gemini");
    assert_eq!(
        resolved.root().listen().window(),
        std::time::Duration::from_secs(180)
    );
    assert_eq!(
        resolved
            .root()
            .proof()
            .container()
            .expect("container path")
            .as_path(),
        Path::new(&container)
    );
    assert_eq!(
        resolved
            .root()
            .proof()
            .runner()
            .expect("runner path")
            .as_path(),
        Path::new(&runner)
    );
    assert_eq!(
        resolved
            .root()
            .lock()
            .named()
            .map(|(name, target)| (name.as_str(), target.shards().get()))
            .collect::<Vec<_>>(),
        [("alpha", 2)]
    );
    assert_eq!(path_source(&resolved, "lane.vendor"), ConfigSource::Flag);
    assert_eq!(resolved.root().vendor().omp().model(), "lane-model");
    assert_eq!(
        resolved.root().vendor().omp().effort(),
        vendor::Effort::XHigh
    );
    assert_eq!(
        path_source(&resolved, "vendor.omp.model"),
        ConfigSource::Worktree
    );
    assert_eq!(path_source(&resolved, "listen.window"), ConfigSource::Local);
    assert_eq!(
        path_source(&resolved, "proof.container"),
        ConfigSource::Local
    );
    assert_eq!(
        path_source(&resolved, "proof.runner"),
        ConfigSource::Worktree
    );
}

#[test]
fn linked_worktrees_select_their_own_tracked_worktree_and_shared_local_intent() {
    let scratch = Scratch::new();
    let origin = repository(&scratch, "origin");
    let common = origin.join(".git");
    let first = linked_worktree(&scratch, &common, "first");
    let second = linked_worktree(&scratch, &common, "second");
    write(first.join("spice.toml"), b"[lane]\nvendor = \"omp\"\n");
    write(second.join("spice.toml"), b"[lane]\nvendor = \"gemini\"\n");
    write(
        common.join(".spice/config.toml"),
        b"[listen]\nwindow = \"5m\"\n",
    );
    write(
        worktree_path(&common.join("worktrees/first")),
        b"[vendor.omp]\nmodel = 'first-model'\n",
    );
    write(
        worktree_path(&common.join("worktrees/second")),
        b"[vendor.omp]\nmodel = 'second-model'\n",
    );
    write(origin.join("spice.toml"), b"[tasks]\nwrong = true\n");
    write(
        first.join(".spice/config.toml"),
        b"[policy]\nwrong = true\n",
    );

    let first = load(first.join("src"), &[]).expect("first linked worktree");
    let second = load(second.join("src"), &[]).expect("second linked worktree");

    assert_eq!(first.root().lane().vendor().as_str(), "omp");
    assert_eq!(second.root().lane().vendor().as_str(), "gemini");
    assert_eq!(first.root().vendor().omp().model(), "first-model");
    assert_eq!(second.root().vendor().omp().model(), "second-model");
    assert_eq!(
        first.root().listen().window(),
        std::time::Duration::from_secs(300)
    );
    assert_eq!(
        second.root().listen().window(),
        std::time::Duration::from_secs(300)
    );
    assert_eq!(path_source(&first, "lane.vendor"), ConfigSource::Tracked);
    assert_eq!(
        path_source(&first, "vendor.omp.model"),
        ConfigSource::Worktree
    );
    assert_eq!(path_source(&first, "listen.window"), ConfigSource::Local);
}

#[test]
fn resolver_diagnostics_from_both_exact_documents_are_retained() {
    let scratch = Scratch::new();
    let root = repository(&scratch, "decode issues");
    write(
        root.join("spice.toml"),
        b"[lane]\nlease = \"soon\"\n[rtk]\nexecutable = \"rtk\"\n",
    );
    write(
        local_path(&root),
        b"[lane]\nlife = \"Drain\"\n[listen]\nwindow = \"never\"\n",
    );

    let error = load(&root, &[]).expect_err("both documents contain refused values");
    assert!(error.acquisition().is_empty());
    let resolution = error.resolution().expect("resolver diagnostics");
    for (source, path) in [
        (ConfigSource::Tracked, "lane.lease"),
        (ConfigSource::Tracked, "rtk.executable"),
        (ConfigSource::Local, "lane.life"),
        (ConfigSource::Local, "listen.window"),
    ] {
        assert!(resolution.issues().iter().any(|issue| {
            issue.source() == source
                && issue
                    .path()
                    .is_some_and(|actual| actual.to_string() == path)
        }));
    }
}

#[cfg(unix)]
#[test]
fn symlink_and_directory_documents_are_both_typed_without_following() {
    use std::os::unix::fs::symlink;

    let scratch = Scratch::new();
    let root = repository(&scratch, "shapes");
    let target = root.join("other.toml");
    write(&target, b"[listen]\nwindow = \"2m\"\n");
    symlink(&target, root.join("spice.toml")).expect("create tracked symlink");
    fs::create_dir_all(local_path(&root)).expect("create local directory");

    let error = load(&root, &[]).expect_err("both document shapes refuse");
    let canonical = fs::canonicalize(&root).expect("canonical fixture root");
    assert_eq!(error.acquisition().len(), 2);
    assert_eq!(
        (
            error.acquisition()[0].source(),
            error.acquisition()[0].kind(),
            error.acquisition()[0].path(),
        ),
        (
            ConfigSource::Tracked,
            AcquisitionKind::Symlink,
            canonical.join("spice.toml").as_path(),
        )
    );
    assert_eq!(error.acquisition()[1].source(), ConfigSource::Local);
    assert_eq!(error.acquisition()[1].kind(), AcquisitionKind::Directory);
    assert_eq!(
        error.acquisition()[1].path(),
        canonical.join(".git/.spice/config.toml")
    );
    assert!(error.resolution().is_none());
    assert_eq!(
        fs::read(target).expect("target remains"),
        b"[listen]\nwindow = \"2m\"\n"
    );
}

#[cfg(unix)]
#[test]
fn linked_root_and_intermediate_parent_refuse_without_following() {
    use std::os::unix::fs::symlink;

    let scratch = Scratch::new();
    let root = repository(&scratch, "linked capture root");
    let admitted = spice::repository::Repository::discover(&root).expect("admit repository");
    let admitted_root = admitted.worktree_root().to_path_buf();
    let replacement = repository(&scratch, "replacement root");
    write(
        replacement.join("spice.toml"),
        b"[listen]\nwindow = \"9m\"\n",
    );
    let displaced = scratch.join("displaced root");
    fs::rename(&root, &displaced).expect("displace admitted root");
    symlink(&replacement, &root).expect("replace admitted root with link");

    let error = capture_repository(&admitted, &[]).expect_err("linked root must refuse");
    assert!(error.acquisition().iter().any(|issue| {
        issue.source() == ConfigSource::Tracked
            && issue.kind() == AcquisitionKind::Symlink
            && issue.path() == admitted_root
    }));
    assert_eq!(
        fs::read(replacement.join("spice.toml")).expect("linked target remains readable"),
        b"[listen]\nwindow = \"9m\"\n"
    );

    fs::remove_file(&root).expect("remove root link");
    fs::rename(&displaced, &root).expect("restore admitted root");
    let local_parent = root.join(".git/.spice");
    let admitted_local_parent = admitted.common_dir().join(".spice");
    let outside = scratch.join("outside local policy");
    fs::create_dir(&outside).expect("create outside local directory");
    write(outside.join("config.toml"), b"[listen]\nwindow = \"8m\"\n");
    symlink(&outside, &local_parent).expect("link local intermediate parent");

    let error = capture_repository(&admitted, &[]).expect_err("linked parent must refuse");
    assert!(error.acquisition().iter().any(|issue| {
        issue.source() == ConfigSource::Local
            && issue.kind() == AcquisitionKind::Symlink
            && issue.path() == admitted_local_parent
    }));
    assert_eq!(
        fs::read(outside.join("config.toml")).expect("outside policy remains readable"),
        b"[listen]\nwindow = \"8m\"\n"
    );
}

#[cfg(unix)]
#[test]
fn equal_policy_bytes_from_another_file_are_another_capture_generation() {
    let scratch = Scratch::new();
    let root = repository(&scratch, "same bytes new generation");
    let tracked = root.join("spice.toml");
    let bytes = b"[lane]\nvendor = \"omp\"\n";
    write(&tracked, bytes);
    let repository = spice::repository::Repository::discover(&root).expect("admit repository");
    let first = capture_repository(&repository, &[]).expect("capture first generation");

    fs::rename(&tracked, root.join("old-spice.toml")).expect("displace first generation");
    fs::write(&tracked, bytes).expect("publish equal replacement bytes");
    let second = capture_repository(&repository, &[]).expect("capture second generation");

    assert_eq!(first.tracked(), second.tracked());
    assert_ne!(first.generation(), second.generation());
}

#[cfg(unix)]
#[test]
fn non_regular_and_non_utf8_documents_are_accumulated() {
    use std::os::unix::net::UnixListener;

    let scratch = Scratch::new();
    let root = repository(&scratch, "non regular");
    // A socket address is bounded by SUN_LEN, which the private temporary root
    // a proof shard hands its tests already spends most of. Bind somewhere
    // short and move the socket into place; the document under test is the
    // file kind, not the path it was created at.
    let bound = PathBuf::from(format!("/tmp/spice-non-regular-{}", std::process::id()));
    let _ = fs::remove_file(&bound);
    let _socket = UnixListener::bind(&bound).expect("bind fixture socket");
    fs::rename(&bound, root.join("spice.toml")).expect("place the non-regular document");
    write(local_path(&root), [0xff, 0xfe]);

    let error = load(&root, &[]).expect_err("both acquisition issues refuse");
    assert_eq!(error.acquisition().len(), 2);
    assert_eq!(error.acquisition()[0].kind(), AcquisitionKind::NonRegular);
    assert_eq!(error.acquisition()[1].kind(), AcquisitionKind::NonUtf8);
    assert!(error.resolution().is_none());
}

#[cfg(unix)]
#[test]
fn symlink_acquisition_and_other_layer_decode_issue_are_both_returned() {
    use std::os::unix::fs::symlink;

    let scratch = Scratch::new();
    let root = repository(&scratch, "linked tracked document");
    let tracked = root.join("spice.toml");
    symlink(root.join("absent-tracked-document.toml"), &tracked)
        .expect("create dangling tracked document link");
    write(local_path(&root), b"[tasks]\nowner = \"local\"\n");

    let error = load(&root, &[]).expect_err("acquisition and decode refuse");

    assert_eq!(error.acquisition().len(), 1);
    assert_eq!(error.acquisition()[0].source(), ConfigSource::Tracked);
    assert_eq!(error.acquisition()[0].kind(), AcquisitionKind::Symlink);
    // The loader reports paths under the canonical repository root, and the
    // platform temporary directory is not always canonical: macOS hands out
    // `/var/folders/...`, a symlink to `/private/var/folders/...`. Only the
    // expectation is canonicalized, because a scratch tree rooted at the
    // longer spelling overruns `SUN_LEN` for the socket fixtures above.
    let canonical_root = fs::canonicalize(&root).expect("canonical fixture root");
    assert_eq!(
        error.acquisition()[0].path(),
        canonical_root.join("spice.toml")
    );
    let resolution = error.resolution().expect("other layer was still decoded");
    assert!(resolution.issues().iter().any(|issue| {
        issue.source() == ConfigSource::Local
            && issue.path().is_some_and(|path| path.to_string() == "tasks")
    }));
}

#[test]
fn repository_failure_is_distinct_from_acquisition_and_resolution() {
    let scratch = Scratch::new();
    let outside = scratch.join("not a repository");
    fs::create_dir(&outside).expect("create non-repository start");
    let error = load(&outside, &[]).expect_err("repository discovery refuses");
    assert!(error.repository().is_some());
    assert!(error.acquisition().is_empty());
    assert!(error.resolution().is_none());
}

fn linked_worktree(scratch: &Scratch, common: &Path, name: &str) -> PathBuf {
    let worktree = scratch.join(format!("linked {name}"));
    let git = common.join("worktrees").join(name);
    fs::create_dir_all(worktree.join("src")).expect("create linked worktree");
    fs::create_dir_all(&git).expect("create linked Git directory");
    write(
        worktree.join(".git"),
        format!("gitdir: {}\n", git.display()),
    );
    write(git.join("commondir"), b"../..\n");
    worktree
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SnapshotEntry {
    path: PathBuf,
    kind: &'static str,
    bytes: Vec<u8>,
}

fn snapshot(root: &Path) -> Vec<SnapshotEntry> {
    fn walk(root: &Path, path: &Path, entries: &mut Vec<SnapshotEntry>) {
        let mut children = fs::read_dir(path)
            .expect("snapshot directory")
            .map(|entry| entry.expect("snapshot entry").path())
            .collect::<Vec<_>>();
        children.sort();
        for child in children {
            let metadata = fs::symlink_metadata(&child).expect("snapshot metadata");
            let relative = child
                .strip_prefix(root)
                .expect("snapshot path")
                .to_path_buf();
            if metadata.is_dir() {
                entries.push(SnapshotEntry {
                    path: relative,
                    kind: "directory",
                    bytes: Vec::new(),
                });
                walk(root, &child, entries);
            } else {
                entries.push(SnapshotEntry {
                    path: relative,
                    kind: "file",
                    bytes: fs::read(child).expect("snapshot file"),
                });
            }
        }
    }

    let mut entries = Vec::new();
    walk(root, root, &mut entries);
    entries
}

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let nonce = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("spice-config-load-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).expect("create scratch directory");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn join(&self, path: impl AsRef<Path>) -> PathBuf {
        self.path.join(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).expect("remove scratch directory");
    }
}
