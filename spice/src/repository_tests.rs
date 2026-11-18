use std::sync::atomic::{AtomicU64, Ordering};

use wire::CanonicalUuid;

use super::*;

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

#[test]
fn main_and_nested_repositories_resolve_the_nearest_common_state() {
    let scratch = Scratch::new();
    let outer = scratch.join("outer repository");
    let inner = outer.join("nested repository");
    fs::create_dir_all(outer.join(".git")).expect("create outer Git directory");
    fs::create_dir_all(inner.join(".git")).expect("create inner Git directory");
    let start = inner.join("source tree").join("deep");
    fs::create_dir_all(&start).expect("create nested start");

    let repository = Repository::discover(&start).expect("resolve nearest repository");
    let worktree = fs::canonicalize(&inner).expect("canonical worktree root");
    let common = fs::canonicalize(inner.join(".git")).expect("canonical common directory");

    assert_eq!(repository.worktree_root(), worktree);
    assert_eq!(repository.git_dir(), common);
    assert_eq!(repository.common_dir(), common);
    let state = repository
        .admit_state()
        .expect("admit absent current state");
    assert_eq!(state.runtime(), common.join(".spice"));
    assert_eq!(state.worktree_runtime(), common.join(".spice"));
    assert_eq!(state.worktree_locks(), common.join(".spice/runtime/locks"));
    assert_eq!(
        state.steering_delivery(),
        common.join(".spice/authority/worktree/delivery.json")
    );
    assert_eq!(
        state.steering_delivery_lock(),
        common.join(".spice/runtime/locks/delivery")
    );
    assert_eq!(state.data(), common.join(".spice/authority/journal"));
    assert_eq!(
        state.journal(),
        common.join(".spice/authority/journal/journal.sqlite3")
    );
    assert_eq!(state.locks(), common.join(".spice/runtime/locks"));
    assert_eq!(
        StatePaths::task_artifacts_under(state.runtime()),
        (
            state.artifacts().to_path_buf(),
            state.artifacts_lock().to_path_buf()
        ),
        "the resident and every command share one artifact store lock"
    );
    assert_eq!(
        state.artifacts_lock(),
        common.join(".spice/runtime/locks/artifacts")
    );
    assert_eq!(state.proof(), common.join(".spice/cache/proof"));
    assert_eq!(state.resident(), common.join(".spice/runtime/resident"));
    assert_eq!(state.serve(), common.join(".spice/runtime/serve"));
    assert_eq!(state.live(), common.join(".spice/runtime"));
    assert_eq!(state.resident().parent(), Some(state.live()));
    assert_eq!(state.degraded(), common.join(".spice/runtime/degraded"));
    assert_eq!(state.lanes(), common.join(".spice/lanes"));
    let lane = CanonicalUuid::new(uuid::Uuid::from_u128(1));
    assert_eq!(
        state.lane_tape(lane),
        common.join(".spice/lanes/00000000-0000-0000-0000-000000000001/tape")
    );
    assert_eq!(
        state.rtk_history(lane),
        common.join(".spice/lanes/00000000-0000-0000-0000-000000000001/rtk/history.db")
    );
    assert_eq!(
        proof::Layout::new(
            repository.worktree_root(),
            state.proof(),
            wire::manifest::ReleaseTarget::Aarch64AppleDarwin,
        )
        .payload_path(),
        worktree.join("dist/stage/targets/aarch64-apple-darwin/payload.json")
    );
}

#[test]
fn linked_worktree_resolves_relative_gitdir_and_commondir_with_spaces() {
    let scratch = Scratch::new();
    let origin = scratch.join("origin repository");
    let common = origin.join(".git");
    let git = common.join("worktrees").join("lane one");
    let worktree = scratch.join("linked worktree");
    fs::create_dir_all(&git).expect("create linked Git directory");
    fs::create_dir_all(worktree.join("nested")).expect("create linked worktree");
    fs::write(
        worktree.join(".git"),
        b"gitdir: ../origin repository/.git/worktrees/lane one\n",
    )
    .expect("write Git directory pointer");
    fs::write(git.join("commondir"), b"../..\n").expect("write common pointer");

    let repository =
        Repository::discover(worktree.join("nested")).expect("resolve linked worktree");

    assert_eq!(
        repository.worktree_root(),
        fs::canonicalize(&worktree).expect("canonical linked worktree")
    );
    assert_eq!(
        repository.common_dir(),
        fs::canonicalize(&common).expect("canonical common directory")
    );
    assert_eq!(
        repository.git_dir(),
        fs::canonicalize(&git).expect("canonical linked Git directory")
    );
    let state = repository
        .admit_state()
        .expect("admit linked current state");
    assert_eq!(
        state.runtime(),
        fs::canonicalize(&common)
            .expect("canonical Rust state parent")
            .join(".spice")
    );
    assert_eq!(
        state.worktree_runtime(),
        fs::canonicalize(&git)
            .expect("canonical linked worktree state parent")
            .join(".spice")
    );
    assert_eq!(
        state.worktree_locks(),
        fs::canonicalize(&git)
            .expect("canonical linked worktree lock parent")
            .join(".spice/runtime/locks")
    );
    assert_common_steering_media(&common, &state);
    assert_eq!(
        proof::Layout::new(
            repository.worktree_root(),
            state.proof(),
            wire::manifest::ReleaseTarget::X86_64UnknownLinuxMusl,
        )
        .payload_path(),
        fs::canonicalize(&worktree)
            .expect("canonical linked worktree")
            .join("dist/stage/targets/x86_64-unknown-linux-musl/payload.json")
    );

    let absolute_git = origin.join("absolute git directory");
    let absolute_worktree = scratch.join("absolute linked worktree");
    fs::create_dir(&absolute_git).expect("create absolute Git directory");
    fs::create_dir(&absolute_worktree).expect("create absolute linked worktree");
    fs::write(
        absolute_worktree.join(".git"),
        format!("gitdir: {}\n", absolute_git.display()),
    )
    .expect("write absolute Git pointer");
    fs::write(
        absolute_git.join("commondir"),
        format!("{}\n", common.display()),
    )
    .expect("write absolute common pointer");

    let absolute_repository =
        Repository::discover(absolute_worktree).expect("resolve absolute pointers");
    assert_eq!(
        absolute_repository.common_dir(),
        fs::canonicalize(&common).expect("canonical absolute common directory")
    );
    let absolute_state = absolute_repository
        .admit_state()
        .expect("admit absolute linked current state");
    assert_ne!(
        state.worktree_locks(),
        absolute_state.worktree_locks(),
        "linked worktrees sharing journal authority need distinct activity locks"
    );
    assert_eq!(
        state.journal(),
        absolute_state.journal(),
        "linked worktrees share one durable ACK authority"
    );
}

fn assert_common_steering_media(common: &Path, state: &StatePaths) {
    assert_eq!(
        state.lanes(),
        fs::canonicalize(common)
            .expect("canonical Rust media parent")
            .join(".spice/lanes"),
        "linked worktree media uses the stable common runtime rather than Git administration",
    );
}

#[cfg(unix)]
#[test]
fn state_admission_refuses_every_link_at_its_exact_component() {
    use std::os::unix::fs::symlink;

    let scratch = Scratch::new();
    for (label, relative) in [
        ("runtime", Path::new("")),
        ("live", Path::new("runtime")),
        ("journal-parent", Path::new("authority/journal")),
        ("journal", Path::new("authority/journal/journal.sqlite3")),
        ("locks", Path::new("runtime/locks")),
        ("proof", Path::new("cache/proof")),
        ("resident", Path::new("runtime/resident")),
        ("serve", Path::new("runtime/serve")),
        ("degraded", Path::new("runtime/degraded")),
        ("lanes", Path::new("lanes")),
        ("services", Path::new("runtime/services")),
        ("releases", Path::new("authority/releases")),
        ("artifact-data", Path::new("authority/artifacts")),
        ("artifacts", Path::new("authority/artifacts/tasks")),
    ] {
        let (repository, runtime) = state_case(&scratch, &format!("linked-{label}"));
        let outside = scratch.join(format!("outside-{label}"));
        fs::create_dir(&outside).expect("create outside link target");
        let linked = state_component(&runtime, relative);
        if let Some(parent) = linked.parent() {
            fs::create_dir_all(parent).expect("create linked component parent");
        }
        symlink(&outside, &linked).expect("plant linked state component");

        let error = repository
            .admit_state()
            .expect_err("linked state component must refuse");
        assert!(
            matches!(&error, StateError::Linked { path } if path == &linked),
            "{label} refused as {error:?}"
        );
        assert_eq!(error.path(), linked);
    }
}

#[test]
fn state_admission_refuses_non_directory_parents_and_non_regular_leaves() {
    let scratch = Scratch::new();
    for (label, relative) in [
        ("runtime", Path::new("")),
        ("live", Path::new("runtime")),
        ("journal-parent", Path::new("authority/journal")),
        ("locks", Path::new("runtime/locks")),
        ("proof", Path::new("cache/proof")),
        ("degraded", Path::new("runtime/degraded")),
        ("lanes", Path::new("lanes")),
        ("services", Path::new("runtime/services")),
        ("releases", Path::new("authority/releases")),
        ("artifact-data", Path::new("authority/artifacts")),
        ("artifacts", Path::new("authority/artifacts/tasks")),
    ] {
        let (repository, runtime) = state_case(&scratch, &format!("file-{label}"));
        let path = state_component(&runtime, relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create malformed state parent");
        }
        fs::write(&path, b"not a directory").expect("plant non-directory state parent");

        let error = repository
            .admit_state()
            .expect_err("non-directory state parent must refuse");
        assert!(
            matches!(&error, StateError::NotDirectory { path: observed } if observed == &path),
            "{label} refused as {error:?}"
        );
    }

    for (label, relative) in [
        ("journal", Path::new("authority/journal/journal.sqlite3")),
        ("resident", Path::new("runtime/resident")),
        ("serve", Path::new("runtime/serve")),
    ] {
        let (repository, runtime) = state_case(&scratch, &format!("directory-{label}"));
        let path = runtime.join(relative);
        fs::create_dir_all(&path).expect("plant non-regular state leaf");

        let error = repository
            .admit_state()
            .expect_err("non-regular state leaf must refuse");
        let exact_path = matches!(
            &error,
            StateError::NotRegularFile { path: observed } if observed == &path
        );
        assert!(exact_path, "{label} refused as {error:?}");
    }
}

#[test]
fn every_repository_shape_failure_is_named_at_its_path() {
    let scratch = Scratch::new();

    let no_repository = scratch.join("no repository").join("nested");
    fs::create_dir_all(&no_repository).expect("create ordinary directory");
    assert!(matches!(
        Repository::discover(&no_repository),
        Err(ResolveError::NoRepository { start })
            if start == fs::canonicalize(&no_repository).expect("canonical start")
    ));

    let malformed = case(&scratch, "malformed gitdir");
    fs::write(malformed.join(".git"), b"gitdir: target\nsecond line\n")
        .expect("write malformed pointer");
    assert!(matches!(
        Repository::discover(&malformed),
        Err(ResolveError::MalformedGitdir { path }) if path == malformed.join(".git")
    ));

    let missing = case(&scratch, "missing gitdir");
    fs::write(missing.join(".git"), b"gitdir: absent\n").expect("write missing pointer");
    assert!(matches!(
        Repository::discover(&missing),
        Err(ResolveError::MissingGitdir { path }) if path == missing.join("absent")
    ));

    let not_directory = case(&scratch, "file gitdir");
    fs::write(not_directory.join("target"), b"ordinary file").expect("write Git target");
    fs::write(not_directory.join(".git"), b"gitdir: target\n").expect("write Git pointer");
    assert!(matches!(
        Repository::discover(&not_directory),
        Err(ResolveError::GitdirNotDirectory { path }) if path == not_directory.join("target")
    ));

    let malformed_common = case(&scratch, "malformed commondir");
    fs::create_dir(malformed_common.join(".git")).expect("create Git directory");
    fs::write(
        malformed_common.join(".git/commondir"),
        b"target\nsecond line\n",
    )
    .expect("write malformed common pointer");
    assert!(matches!(
        Repository::discover(&malformed_common),
        Err(ResolveError::MalformedCommondir { path })
            if path == malformed_common.join(".git/commondir")
    ));

    let missing_common = case(&scratch, "missing commondir");
    fs::create_dir(missing_common.join(".git")).expect("create Git directory");
    fs::write(missing_common.join(".git/commondir"), b"absent\n")
        .expect("write missing common pointer");
    assert!(matches!(
        Repository::discover(&missing_common),
        Err(ResolveError::MissingCommondir { path })
            if path == missing_common.join(".git/absent")
    ));

    let common_file = case(&scratch, "file commondir");
    fs::create_dir(common_file.join(".git")).expect("create Git directory");
    fs::write(common_file.join(".git/target"), b"ordinary file").expect("write common target");
    fs::write(common_file.join(".git/commondir"), b"target\n").expect("write common pointer");
    assert!(matches!(
        Repository::discover(&common_file),
        Err(ResolveError::CommondirNotDirectory { path })
            if path == common_file.join(".git/target")
    ));

    let absent_start = scratch.join("absent start");
    assert!(matches!(
        Repository::discover(&absent_start),
        Err(ResolveError::Filesystem { path, .. }) if path == absent_start
    ));

    let file_start = scratch.join("file start");
    fs::write(&file_start, b"ordinary file").expect("write non-directory start");
    assert!(matches!(
        Repository::discover(&file_start),
        Err(ResolveError::StartNotDirectory { path }) if path == file_start
    ));
}

#[cfg(windows)]
#[test]
fn git_spellings_name_local_drives_with_forward_slashes_and_refuse_shares() {
    for (path, spelled) in [
        (r"\\?\C:\work\repo\.git\index", "C:/work/repo/.git/index"),
        (r"C:\work\repo\.git\index", "C:/work/repo/.git/index"),
        (
            r"\\?\d:\work tree\.git\spice-index",
            "D:/work tree/.git/spice-index",
        ),
    ] {
        assert_eq!(
            git_spelling(Path::new(path)).as_deref(),
            Ok(spelled),
            "{path}"
        );
    }
    for refused in [
        r"\\server\share\repo\.git\index",
        r"\\?\UNC\server\share\index",
    ] {
        assert!(git_spelling(Path::new(refused)).is_err(), "{refused}");
    }
}

fn case(scratch: &Scratch, name: &str) -> PathBuf {
    let path = scratch.join(name);
    fs::create_dir(&path).expect("create refusal case");
    path
}

fn state_case(scratch: &Scratch, name: &str) -> (Repository, PathBuf) {
    let root = case(scratch, name);
    let common = root.join(".git");
    fs::create_dir(&common).expect("create state-case Git directory");
    let repository = Repository::discover(&root).expect("discover state-case repository");
    (repository, common.join(STATE_DIRECTORY))
}

fn state_component(runtime: &Path, relative: &Path) -> PathBuf {
    if relative.as_os_str().is_empty() {
        runtime.to_path_buf()
    } else {
        runtime.join(relative)
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "spice-repository-resolver-{}-{}",
            std::process::id(),
            NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("create repository scratch");
        Self(fs::canonicalize(path).expect("canonical repository scratch"))
    }

    fn join(&self, path: impl AsRef<Path>) -> PathBuf {
        self.0.join(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove repository scratch");
    }
}
