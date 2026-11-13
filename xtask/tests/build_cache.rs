#![cfg(unix)]

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use sill::{Acquisition, LockFile, LockName, Possession};
use xtask::{
    BuildCacheContext, BuildCacheDisposition, BuildCacheError, BuildCacheRequest,
    BuildCacheRootKind, BuildCacheRootReport, BuildCacheTargetDirectory, collect_build_cache_at,
    parse_byte_quantity,
};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);
static NEXT_LAST_USE: AtomicU64 = AtomicU64::new(4_000_000_000);
const CACHE_TAG: &str = "CACHEDIR.TAG";
const LOCK_NAME: &str = ".cargo-build-lock";
const TARGET_LOCK_NAME: &str = ".cargo-lock";

#[test]
fn build_cache_dry_run_inventories_and_selects_oldest_without_removing() {
    let scratch = Scratch::new("dry-run-lru");
    let cache = scratch.join("CargoBuild");
    let old = write_cache_root(&cache, "00", "00000000000000", "old");
    let newer = write_cache_root(&cache, "01", "00000000000000", "newer");
    let current = write_cache_root(&cache, "ff", "ffffffffffffff", "current");
    let inventory = collect_cache(&current, BuildCacheRequest::default(), Vec::new(), None);
    let old_bytes = root_report(&inventory, &old).bytes;
    let report = collect_cache(
        &current,
        BuildCacheRequest {
            max_bytes: Some(inventory.total_bytes - old_bytes),
            ..BuildCacheRequest::default()
        },
        Vec::new(),
        None,
    );

    assert_eq!(
        root_report(&report, &old).disposition,
        BuildCacheDisposition::WouldReclaim
    );
    assert_eq!(
        root_report(&report, &newer).disposition,
        BuildCacheDisposition::Kept
    );
    assert_eq!(
        root_report(&report, &current).disposition,
        BuildCacheDisposition::RetainedCurrent
    );
    assert!(old.is_dir(), "dry-run removed the selected old root");
    assert_eq!(report.reclaimed_bytes, 0);
    assert_eq!(report.selected_bytes, old_bytes);
}

#[test]
fn build_cache_applies_lru_cap_and_is_idempotent() {
    let scratch = Scratch::new("lru");
    let cache = scratch.join("CargoBuild");
    let mut roots = Vec::new();
    for index in 0..8_u16 {
        roots.push(write_cache_root(
            &cache,
            &format!("{:02x}", index % 2),
            &format!("{index:014x}"),
            "payload",
        ));
    }
    let current = roots.last().expect("fixture root exists").clone();
    let inventory = collect_cache(&current, BuildCacheRequest::default(), Vec::new(), None);
    let current_bytes = root_report(&inventory, &current).bytes;
    let request = BuildCacheRequest {
        apply: true,
        max_bytes: Some(current_bytes),
        retain_live_worktrees: 0,
        ..BuildCacheRequest::default()
    };

    let first = collect_cache(&current, request.clone(), Vec::new(), None);
    assert!(first.reclaimed_bytes > 0, "the cap reclaimed nothing");
    assert!(
        roots[..roots.len() - 1].iter().all(|root| !root.exists()),
        "collector left an old root behind"
    );
    assert!(current.is_dir(), "collector removed the current root");

    let second = collect_cache(&current, request, Vec::new(), None);
    assert_eq!(second.reclaimed_bytes, 0, "second run was not idempotent");
    assert_eq!(second.selected_bytes, 0, "second run selected a root");
}

#[test]
fn build_cache_skips_held_native_release_and_wasm_locks_until_release() {
    let scratch = Scratch::new("held-locks");
    let cache = scratch.join("CargoBuild");
    let current = write_cache_root(&cache, "ff", "ffffffffffffff", "current");
    let native = write_cache_root(&cache, "00", "00000000000000", "native");
    let release = write_cache_root(&cache, "01", "00000000000000", "release");
    let wasm = write_cache_root(&cache, "02", "00000000000000", "wasm");
    let held = vec![
        hold_lock(&add_lock(&native, "debug/.cargo-build-lock")),
        hold_lock(&add_lock(&release, "release/.cargo-build-lock")),
        hold_lock(&add_lock(
            &wasm,
            "wasm32-unknown-unknown/debug/.cargo-build-lock",
        )),
    ];
    let request = BuildCacheRequest {
        apply: true,
        max_bytes: Some(0),
        retain_live_worktrees: 0,
        ..BuildCacheRequest::default()
    };

    let blocked = collect_cache(&current, request.clone(), Vec::new(), None);
    for root in [&native, &release, &wasm] {
        assert_eq!(
            root_report(&blocked, root).disposition,
            BuildCacheDisposition::SkippedActive,
            "{} was not reported as active",
            root.display()
        );
        assert!(root.is_dir(), "{} was removed while locked", root.display());
    }

    drop(held);
    let collected = collect_cache(&current, request, Vec::new(), None);
    for root in [&native, &release, &wasm] {
        assert_eq!(
            root_report(&collected, root).disposition,
            BuildCacheDisposition::Reclaimed,
            "{} was not reclaimed after release",
            root.display()
        );
        assert!(!root.exists(), "{} survived after release", root.display());
    }
}

#[test]
fn build_cache_retains_current_and_configured_hottest_live_worktree_roots() {
    let scratch = Scratch::new("live-retention");
    let cache = scratch.join("CargoBuild");
    let victim = write_cache_root(&cache, "00", "00000000000000", "victim");
    let cold_live = write_cache_root(&cache, "01", "00000000000000", "cold-live");
    let hot_worktree = scratch.join("hot-worktree");
    let hot_live = write_target_root(&hot_worktree, "hot-live");
    let current = write_cache_root(&cache, "ff", "ffffffffffffff", "current");
    let request = BuildCacheRequest {
        apply: true,
        max_bytes: Some(0),
        retain_live_worktrees: 1,
        ..BuildCacheRequest::default()
    };

    let (report, _) = collect_cache_with_targets(
        &current,
        request,
        vec![cold_live.clone()],
        None,
        vec![BuildCacheTargetDirectory::new(&hot_worktree, &hot_live)],
        None,
    );

    assert_eq!(
        root_report(&report, &current).disposition,
        BuildCacheDisposition::RetainedCurrent
    );
    assert_eq!(
        root_report(&report, &hot_live).disposition,
        BuildCacheDisposition::RetainedLiveWorktree
    );
    assert_eq!(
        root_report(&report, &cold_live).disposition,
        BuildCacheDisposition::Reclaimed
    );
    assert_eq!(
        root_report(&report, &victim).disposition,
        BuildCacheDisposition::Reclaimed
    );
    assert!(current.is_dir(), "current worktree root was removed");
    assert!(hot_live.is_dir(), "hottest live worktree root was removed");
    assert!(
        !cold_live.exists(),
        "unretained live worktree root survived"
    );
}

#[test]
fn build_cache_uses_free_space_floor_as_a_reclaim_target() {
    let scratch = Scratch::new("free-floor");
    let cache = scratch.join("CargoBuild");
    let old = write_cache_root(&cache, "00", "00000000000000", "old");
    let current = write_cache_root(&cache, "ff", "ffffffffffffff", "current");
    let inventory = collect_cache(
        &current,
        BuildCacheRequest::default(),
        Vec::new(),
        Some(100),
    );
    let old_bytes = root_report(&inventory, &old).bytes;

    let report = collect_cache(
        &current,
        BuildCacheRequest {
            free_space_floor: Some(100 + old_bytes),
            ..BuildCacheRequest::default()
        },
        Vec::new(),
        Some(100),
    );

    assert_eq!(
        root_report(&report, &old).disposition,
        BuildCacheDisposition::WouldReclaim
    );
    assert_eq!(report.target_bytes, inventory.total_bytes - old_bytes);
}

#[test]
fn build_cache_removal_never_follows_symlinks_or_collects_malformed_root_links() {
    let scratch = Scratch::new("symlink-boundary");
    let cache = scratch.join("CargoBuild");
    let outside = scratch.join("outside");
    fs::create_dir(&outside).expect("create outside directory");
    fs::write(outside.join("kept"), b"outside").expect("write outside witness");
    let current = write_cache_root(&cache, "ff", "ffffffffffffff", "current");
    let linked_inside = write_cache_root(&cache, "00", "00000000000000", "linked-inside");
    fs::create_dir_all(linked_inside.join("debug")).expect("create symlink parent");
    std::os::unix::fs::symlink(&outside, linked_inside.join("debug/outside-link"))
        .expect("create inside symlink");
    let malformed_bucket = cache.join("01");
    fs::create_dir_all(&malformed_bucket).expect("create malformed bucket");
    let malformed = malformed_bucket.join("11111111111111");
    std::os::unix::fs::symlink(&outside, &malformed).expect("create root symlink");

    let report = collect_cache(
        &current,
        BuildCacheRequest {
            apply: true,
            max_bytes: Some(0),
            retain_live_worktrees: 0,
            ..BuildCacheRequest::default()
        },
        Vec::new(),
        None,
    );

    assert_eq!(
        root_report(&report, &linked_inside).disposition,
        BuildCacheDisposition::Reclaimed
    );
    assert!(
        outside.join("kept").is_file(),
        "collector followed a symlink outside the hierarchy"
    );
    assert!(
        malformed.exists(),
        "collector removed a symlink candidate root"
    );
    assert!(matches!(
        root_report(&report, &malformed).disposition,
        BuildCacheDisposition::SkippedMalformed(_)
    ));
}

#[test]
fn ordinary_target_collection_preserves_current_live_and_locked_roots() {
    let scratch = Scratch::new("ordinary-targets");
    let current_worktree = scratch.join("current");
    let cold_worktree = scratch.join("cold");
    let hot_worktree = scratch.join("hot");
    let busy_worktree = scratch.join("busy");
    let current = write_target_root(&current_worktree, "current");
    let cold = write_target_root(&cold_worktree, "cold");
    let hot = write_target_root(&hot_worktree, "hot");
    let busy = write_target_root(&busy_worktree, "busy");
    let unrelated = write_cache_root(
        &scratch.join("CargoBuild"),
        "00",
        "00000000000000",
        "unrelated",
    );
    let mut context = BuildCacheContext::new(&current);
    context.target_directory = Some(BuildCacheTargetDirectory::new(&current_worktree, &current));
    context.live_target_directories = vec![
        BuildCacheTargetDirectory::new(&cold_worktree, &cold),
        BuildCacheTargetDirectory::new(&hot_worktree, &hot),
        BuildCacheTargetDirectory::new(&busy_worktree, &busy),
    ];
    let request = BuildCacheRequest {
        max_bytes: Some(0),
        retain_live_worktrees: 1,
        ..BuildCacheRequest::default()
    };
    // The busy root is newest and retained; the hot root is protected by its lock instead.
    let lock = hold_named_lock(&hot.join("debug").join(TARGET_LOCK_NAME));
    let preview = collect_build_cache_at(&request, &context, &mut Vec::new())
        .expect("ordinary target inventory");
    assert_eq!(preview.hierarchy, None);
    assert_eq!(preview.current_target, Some(current.clone()));
    for (root, disposition) in [
        (&current, BuildCacheDisposition::RetainedCurrent),
        (&cold, BuildCacheDisposition::WouldReclaim),
        (&busy, BuildCacheDisposition::RetainedLiveWorktree),
        (&hot, BuildCacheDisposition::SkippedActive),
    ] {
        assert_eq!(root_report(&preview, root).disposition, disposition);
        assert!(root.is_dir());
    }
    assert!(!preview.roots.iter().any(|root| root.path == unrelated));
    assert_eq!(preview.reclaimed_bytes, 0);
    let apply = BuildCacheRequest {
        apply: true,
        ..request
    };
    let collected = collect_build_cache_at(&apply, &context, &mut Vec::new())
        .expect("ordinary target collection");
    assert_eq!(
        root_report(&collected, &cold).disposition,
        BuildCacheDisposition::Reclaimed
    );
    assert!(!cold.exists());
    for root in [&current, &busy, &hot, &unrelated] {
        assert!(root.is_dir());
    }
    drop(lock);
    let released = collect_build_cache_at(&apply, &context, &mut Vec::new())
        .expect("ordinary target collection after lock release");
    assert_eq!(
        root_report(&released, &hot).disposition,
        BuildCacheDisposition::Reclaimed
    );
    assert!(!hot.exists());
    assert!(current.is_dir());
    assert!(busy.is_dir());
}

#[test]
fn build_cache_dry_run_reports_managed_and_worktree_targets_in_one_inventory() {
    let scratch = Scratch::new("mixed-inventory");
    let cache = scratch.join("CargoBuild");
    let linked_build = write_cache_root(&cache, "00", "00000000000000", "linked-build");
    let current_build = write_cache_root(&cache, "ff", "ffffffffffffff", "current-build");
    let current_worktree = scratch.join("current-worktree");
    let linked_worktree = scratch.join("linked-worktree");
    let current_target = write_target_root(&current_worktree, "current-target");
    let linked_target = write_target_root(&linked_worktree, "linked-target");
    let (report, _) = collect_cache_with_targets(
        &current_build,
        BuildCacheRequest {
            max_bytes: Some(0),
            retain_live_worktrees: 0,
            ..BuildCacheRequest::default()
        },
        vec![linked_build.clone()],
        Some(BuildCacheTargetDirectory::new(
            &current_worktree,
            &current_target,
        )),
        vec![BuildCacheTargetDirectory::new(
            &linked_worktree,
            &linked_target,
        )],
        None,
    );

    assert_eq!(
        root_report(&report, &current_build).kind,
        BuildCacheRootKind::ManagedBuild
    );
    assert_eq!(
        root_report(&report, &current_target).kind,
        BuildCacheRootKind::WorktreeTarget
    );
    for current in [&current_build, &current_target] {
        assert_eq!(
            root_report(&report, current).disposition,
            BuildCacheDisposition::RetainedCurrent,
            "{} was not retained as a current root",
            current.display()
        );
    }
    for linked in [&linked_build, &linked_target] {
        assert_eq!(
            root_report(&report, linked).disposition,
            BuildCacheDisposition::WouldReclaim,
            "{} was not selected in the global LRU inventory",
            linked.display()
        );
    }
    assert_eq!(report.current_target, Some(current_target.clone()));
    assert_eq!(
        report.total_bytes,
        report.roots.iter().map(|root| root.bytes).sum::<u64>()
    );
    let target = root_report(&report, &linked_target);
    assert!(
        target
            .locks
            .iter()
            .any(|lock| lock.file_name() == Some(OsStr::new(TARGET_LOCK_NAME)))
    );
    assert!(
        target
            .locks
            .iter()
            .any(|lock| lock.file_name() == Some(OsStr::new(LOCK_NAME)))
    );
}

#[test]
fn build_cache_reprobes_target_and_build_locks_before_removal() {
    let scratch = Scratch::new("target-locks");
    let cache = scratch.join("CargoBuild");
    let current = write_cache_root(&cache, "ff", "ffffffffffffff", "current");
    let linked_worktree = scratch.join("linked-worktree");
    let target = write_target_root(&linked_worktree, "linked-target");
    let target_lock = target.join("debug").join(TARGET_LOCK_NAME);
    let held = hold_named_lock(&target_lock);
    let request = BuildCacheRequest {
        apply: true,
        max_bytes: Some(0),
        retain_live_worktrees: 0,
        ..BuildCacheRequest::default()
    };

    let (blocked, _) = collect_cache_with_targets(
        &current,
        request.clone(),
        Vec::new(),
        None,
        vec![BuildCacheTargetDirectory::new(&linked_worktree, &target)],
        None,
    );
    assert_eq!(
        root_report(&blocked, &target).disposition,
        BuildCacheDisposition::SkippedActive
    );
    assert!(target.is_dir(), "held worktree target was removed");

    drop(held);
    let (collected, _) = collect_cache_with_targets(
        &current,
        request,
        Vec::new(),
        None,
        vec![BuildCacheTargetDirectory::new(&linked_worktree, &target)],
        None,
    );
    assert_eq!(
        root_report(&collected, &target).disposition,
        BuildCacheDisposition::Reclaimed
    );
    assert!(!target.exists(), "released worktree target survived");
}

#[test]
fn build_cache_skips_malformed_symlinked_and_outside_worktree_targets() {
    let scratch = Scratch::new("target-boundaries");
    let cache = scratch.join("CargoBuild");
    let current = write_cache_root(&cache, "ff", "ffffffffffffff", "current");
    let worktree = scratch.join("worktree");
    fs::create_dir_all(&worktree).expect("create owning worktree");
    let outside = scratch.join("outside-target");
    write_target_at(&outside, "outside");
    let symlink = worktree.join("symlink-target");
    std::os::unix::fs::symlink(&outside, &symlink).expect("link target outside worktree");
    let untagged = worktree.join("untagged-target");
    fs::create_dir(&untagged).expect("create untagged target");
    fs::write(untagged.join("payload"), b"untagged").expect("write untagged target");

    let (report, _) = collect_cache_with_targets(
        &current,
        BuildCacheRequest {
            apply: true,
            max_bytes: Some(0),
            retain_live_worktrees: 0,
            ..BuildCacheRequest::default()
        },
        Vec::new(),
        None,
        [&outside, &symlink, &untagged]
            .map(|target| BuildCacheTargetDirectory::new(&worktree, target))
            .to_vec(),
        None,
    );

    for target in [&outside, &symlink, &untagged] {
        assert!(matches!(
            root_report(&report, target).disposition,
            BuildCacheDisposition::SkippedMalformed(_)
        ));
        assert!(target.exists(), "{} was removed", target.display());
    }
    assert!(outside.join("payload").is_file());
}

#[test]
fn build_cache_names_the_exact_owner_read_only_target_path() {
    let scratch = Scratch::new("read-only-target");
    let cache = scratch.join("CargoBuild");
    let current = write_cache_root(&cache, "ff", "ffffffffffffff", "current");
    let worktree = scratch.join("worktree");
    let target = write_target_root(&worktree, "read-only");
    let read_only = target.join("xtask-app-source/journal");
    fs::create_dir_all(&read_only).expect("create read-only snapshot");
    let refused = read_only.join("Cargo.toml");
    fs::write(&refused, b"[workspace]\n").expect("write refused snapshot file");
    fs::set_permissions(&read_only, fs::Permissions::from_mode(0o500))
        .expect("make snapshot owner-read-only");
    let mut context = BuildCacheContext::new(&current);
    context
        .live_target_directories
        .push(BuildCacheTargetDirectory::new(&worktree, &target));
    let mut output = Vec::new();
    let error = collect_build_cache_at(
        &BuildCacheRequest {
            apply: true,
            max_bytes: Some(0),
            retain_live_worktrees: 0,
            ..BuildCacheRequest::default()
        },
        &context,
        &mut output,
    )
    .expect_err("owner-read-only target subtree refuses removal");
    fs::set_permissions(&read_only, fs::Permissions::from_mode(0o700))
        .expect("restore snapshot permissions");

    let BuildCacheError::Io { path, .. } = error else {
        panic!("read-only target returned the wrong refusal: {error}");
    };
    assert_eq!(path, refused);
}

#[test]
fn build_cache_byte_parser_accepts_documented_binary_suffixes() {
    assert_eq!(
        parse_byte_quantity("7G").expect("parse G"),
        7 * 1024 * 1024 * 1024
    );
    assert_eq!(
        parse_byte_quantity("50GiB").expect("parse GiB"),
        50 * 1024 * 1024 * 1024
    );
    assert!(parse_byte_quantity("eight").is_err());
}

fn write_cache_root(cache: &Path, bucket: &str, hash: &str, label: &str) -> PathBuf {
    let root = cache.join(bucket).join(hash);
    fs::create_dir_all(root.join("debug")).expect("create cache root");
    fs::write(
        root.join(CACHE_TAG),
        b"Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .expect("write cache tag");
    fs::write(
        root.join("payload"),
        format!("{label}\n{}", "x".repeat(4096)),
    )
    .expect("write payload");
    fs::write(root.join("debug/.cargo-build-lock"), b"").expect("write idle lock");
    set_last_use(&root);
    root
}

fn add_lock(root: &Path, relative: &str) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("lock has a parent")).expect("create lock parent");
    fs::write(&path, b"").expect("write lock file");
    path
}

fn write_target_root(worktree: &Path, label: &str) -> PathBuf {
    let target = worktree.join("target");
    write_target_at(&target, label);
    target
}

fn write_target_at(target: &Path, label: &str) {
    fs::create_dir_all(target.join("debug")).expect("create worktree target");
    fs::write(
        target.join(CACHE_TAG),
        b"Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .expect("write target cache tag");
    fs::write(target.join("payload"), label).expect("write target payload");
    fs::write(target.join("debug").join(TARGET_LOCK_NAME), b"").expect("write target lock");
    fs::write(target.join("debug").join(LOCK_NAME), b"").expect("write build lock");
    set_last_use(target);
}

fn set_last_use(root: &Path) {
    let modified = UNIX_EPOCH + Duration::from_secs(NEXT_LAST_USE.fetch_add(1, Ordering::Relaxed));
    fs::File::open(root.join("payload"))
        .expect("open last-use evidence")
        .set_times(fs::FileTimes::new().set_modified(modified))
        .expect("set deterministic last-use evidence");
}

fn hold_lock(path: &Path) -> Possession {
    hold_named_lock(path)
}

fn hold_named_lock(path: &Path) -> Possession {
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .expect("fixture lock has a UTF-8 name");
    let lock = LockFile::new(
        LockName::new(name).expect("Cargo lock name is one component"),
        path,
    );
    match lock.try_acquire().expect("hold lock") {
        Acquisition::Acquired(possession) => possession,
        Acquisition::Contended => panic!("fixture lock is unexpectedly held"),
    }
}

fn collect_cache(
    current: &Path,
    request: BuildCacheRequest,
    live_build_directories: Vec<PathBuf>,
    available_bytes: Option<u64>,
) -> xtask::BuildCacheReport {
    collect_cache_with_targets(
        current,
        request,
        live_build_directories,
        None,
        Vec::new(),
        available_bytes,
    )
    .0
}

fn collect_cache_with_targets(
    current: &Path,
    request: BuildCacheRequest,
    live_build_directories: Vec<PathBuf>,
    current_target: Option<BuildCacheTargetDirectory>,
    live_target_directories: Vec<BuildCacheTargetDirectory>,
    available_bytes: Option<u64>,
) -> (xtask::BuildCacheReport, String) {
    let mut context = BuildCacheContext::new(current);
    context.target_directory = current_target;
    context.live_build_directories = live_build_directories;
    context.live_target_directories = live_target_directories;
    context.available_bytes = available_bytes;
    let mut output = Vec::new();
    let report = collect_build_cache_at(&request, &context, &mut output)
        .expect("collect build and target cache");
    (
        report,
        String::from_utf8(output).expect("build-cache report is UTF-8"),
    )
}

fn root_report<'a>(report: &'a xtask::BuildCacheReport, root: &Path) -> &'a BuildCacheRootReport {
    let canonical = fs::canonicalize(root).ok();
    report
        .roots
        .iter()
        .find(|entry| entry.path == root)
        .or_else(|| {
            report.roots.iter().find(|entry| {
                canonical
                    .as_ref()
                    .is_some_and(|canonical| entry.path == *canonical)
            })
        })
        .unwrap_or_else(|| panic!("{} was not reported", root.display()))
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "spice-cargo-build-cache-{label}-{}-{}",
            std::process::id(),
            NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("create build-cache scratch");
        Self(fs::canonicalize(path).expect("canonicalize build-cache scratch"))
    }

    fn join(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.0.join(relative)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove build-cache scratch");
    }
}
