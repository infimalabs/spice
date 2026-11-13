use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{
    OutputLease, Pass, RECENT_USE, RECLAIM_INTERVAL, UnitKind, measure, reclaim, remove_if_empty,
};

/// One scratch directory, removed again when the test that made it ends.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scratch(label: &str) -> Scratch {
    let root = std::env::temp_dir().join(format!(
        "spice-cargo-output-{label}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::create_dir(&root).expect("scratch directory creates");
    Scratch(root)
}

/// A Cargo profile directory holding `lock` and one compiled artifact.
///
/// Units are written in the order a test names them, so a unit written
/// earlier is the less recently used one.
fn unit(directory: &Path, lock: &str) {
    fs::create_dir_all(directory.join("deps")).expect("unit deps create");
    fs::create_dir_all(directory.join(".fingerprint/crate-0123")).expect("fingerprints create");
    fs::write(directory.join(lock), b"").expect("unit lock writes");
    fs::write(
        directory.join("deps/libcrate-0123.rlib"),
        vec![7_u8; 64 * 1024],
    )
    .expect("artifact writes");
    fs::write(
        directory.join(".fingerprint/crate-0123/lib-crate"),
        b"fresh",
    )
    .expect("fingerprint writes");
}

fn emptied(directory: &Path, lock: &str) -> bool {
    let mut entries = fs::read_dir(directory)
        .expect("unit directory remains")
        .map(|entry| entry.expect("entry reads").file_name())
        .collect::<Vec<_>>();
    entries.sort();
    entries == [lock]
}

fn intact(directory: &Path) -> bool {
    directory.join("deps/libcrate-0123.rlib").is_file()
        && directory
            .join(".fingerprint/crate-0123/lib-crate")
            .is_file()
}

/// One rustc incremental cache under `profile`, with its session lock.
fn cache(profile: &Path, krate: &str) -> PathBuf {
    let directory = profile.join("incremental").join(format!("{krate}-0123"));
    let session = directory.join("s-abc-def-working");
    fs::create_dir_all(&session).expect("session creates");
    fs::write(directory.join("s-abc-def.lock"), b"").expect("session lock writes");
    fs::write(session.join("dep-graph.bin"), vec![3_u8; 256 * 1024]).expect("cache writes");
    directory
}

#[test]
fn cold_caches_go_before_the_fresh_profile_that_holds_them() {
    let scratch = scratch("caches");
    let target = scratch.0.join("target");
    let profile = target.join("debug");
    unit(&profile, ".cargo-lock");
    let cold = cache(&profile, "cold");
    let warm = cache(&profile, "warm");
    let now = SystemTime::now();
    let mut footprint = measure(&target, 0).expect("target measures");
    let kinds = footprint
        .units
        .iter()
        .map(|unit| (unit.path.clone(), unit.kind))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        kinds,
        [
            (profile.clone(), UnitKind::Profile),
            (cold.clone(), UnitKind::Incremental),
            (warm.clone(), UnitKind::Incremental),
        ]
        .into_iter()
        .collect(),
        "each cache is a unit beside its profile"
    );
    let cold_bytes = footprint
        .units
        .iter()
        .find(|unit| unit.path == cold)
        .expect("cold cache is a unit")
        .bytes;
    footprint.bound = footprint.bytes - cold_bytes;
    for measured in &mut footprint.units {
        if measured.path == cold {
            measured.last_use = now - RECENT_USE - Duration::from_secs(1);
        }
    }

    let reclamation = reclaim(&footprint, now).expect("reclamation runs");

    assert_eq!(
        reclamation
            .removed
            .iter()
            .map(|removed| (removed.path.as_path(), removed.kind))
            .collect::<Vec<_>>(),
        [(cold.as_path(), UnitKind::Incremental)],
        "only the cold cache goes while its fresh profile and warm cache stay"
    );
    assert!(reclamation.after <= footprint.bound);
    assert!(
        cold.join("s-abc-def.lock").is_file() && !cold.join("s-abc-def-working").exists(),
        "an emptied cache keeps the session lock rustc waits on"
    );
    assert!(intact(&profile) && warm.join("s-abc-def-working/dep-graph.bin").is_file());

    let mut footprint = measure(&target, 0).expect("the current target measures");
    for measured in &mut footprint.units {
        measured.last_use = now - RECENT_USE - Duration::from_secs(1);
    }
    footprint.bound = 0;
    let reclamation = reclaim(&footprint, now).expect("reclamation runs");
    assert_eq!(reclamation.after, 0);
    assert!(!profile.join("incremental").exists());
}

#[test]
fn lock_only_units_do_not_count_as_reclaimed_data() {
    let scratch = scratch("empty");
    let target = scratch.0.join("target");
    let profile = target.join("debug");
    unit(&profile, ".cargo-lock");
    let empty = profile.join("incremental/empty-0123");
    fs::create_dir_all(&empty).expect("empty cache creates");
    fs::write(empty.join("s-abc-def.lock"), b"").expect("session lock writes");
    let footprint = measure(&target, 0).expect("target measures");
    assert_eq!(
        footprint
            .units
            .iter()
            .find(|unit| unit.path == empty)
            .unwrap()
            .bytes,
        0
    );
    let reclamation = reclaim(&footprint, after_recent_use()).expect("reclamation runs");
    assert!(reclamation.removed.iter().all(|unit| unit.bytes > 0));
    assert_eq!(
        reclamation
            .removed
            .iter()
            .map(|unit| unit.bytes)
            .sum::<u64>(),
        reclamation.before - reclamation.after
    );
    assert!(emptied(&profile, ".cargo-lock"));
}

#[test]
fn an_empty_cache_without_session_locks_is_removed_while_its_profile_stays() {
    let scratch = scratch("empty-directory");
    let target = scratch.0.join("target");
    let profile = target.join("debug");
    unit(&profile, ".cargo-lock");
    let empty = profile.join("incremental/empty-0123");
    fs::create_dir_all(&empty).unwrap();
    let now = SystemTime::now();
    let mut footprint = measure(&target, 0).unwrap();
    for unit in &mut footprint.units {
        if unit.path == empty {
            unit.last_use = now - RECENT_USE - Duration::from_secs(1);
        }
    }
    let reclamation = reclaim(&footprint, now).unwrap();
    assert!(!empty.exists());
    assert!(intact(&profile));
    assert!(reclamation.removed.is_empty());
    assert_eq!(reclamation.after, footprint.bytes);
}

#[test]
fn a_recent_cache_survives_reclamation_of_its_older_profile() {
    let scratch = scratch("recent-cache");
    let target = scratch.0.join("target");
    let profile = target.join("debug");
    unit(&profile, ".cargo-lock");
    let recent = cache(&profile, "recent");
    let payload = recent.join("s-abc-def-working/dep-graph.bin");
    let now = SystemTime::now() + RECENT_USE * 2;
    File::options()
        .write(true)
        .open(&payload)
        .expect("cache payload opens")
        .set_modified(now)
        .expect("cache use records");
    let footprint = measure(&target, 0).expect("target measures");

    let reclamation = reclaim(&footprint, now).expect("reclamation runs");

    assert!(
        payload.is_file(),
        "a recent cache survives profile reclamation"
    );
    assert!(reclamation.recent.contains(&recent));
    assert!(!reclamation.removed.iter().any(|unit| unit.path == recent));
    assert_eq!(
        reclamation.after,
        measure(&target, 0).expect("retained target measures").bytes,
        "the reported bytes include the retained cache"
    );
}

#[test]
fn a_cache_whose_session_lock_is_held_is_left_whole() {
    let scratch = scratch("session-lock");
    let target = scratch.0.join("target");
    let profile = target.join("debug");
    unit(&profile, ".cargo-lock");
    let compiling = cache(&profile, "compiling");
    let lock = File::open(compiling.join("s-abc-def.lock")).expect("session lock opens");
    lock.lock().expect("rustc holds its session lock");

    let footprint = measure(&target, 0).expect("target measures");
    let reclamation = reclaim(&footprint, after_recent_use()).expect("reclamation runs");

    assert_eq!(reclamation.busy, std::slice::from_ref(&compiling));
    assert!(compiling.join("s-abc-def-working/dep-graph.bin").is_file());
    assert!(
        reclamation
            .removed
            .iter()
            .any(|removed| removed.path == profile),
        "the profile itself is still emptied around the busy cache: {reclamation:?}"
    );
    assert!(
        compiling.join("s-abc-def-working/dep-graph.bin").is_file(),
        "emptying the profile leaves the cache rustc is writing"
    );
}

/// A pass time at which every unit a test just wrote is past [`RECENT_USE`].
fn after_recent_use() -> SystemTime {
    SystemTime::now() + RECENT_USE * 2
}

#[test]
fn reclamation_empties_least_recent_units_until_the_footprint_fits() {
    let scratch = scratch("lru");
    let target = scratch.0.join("target");
    let oldest = target.join("debug");
    let nested = oldest.join("build/tool/out/release");
    let newer = target.join("cargo-build/debug");
    let newest = target.join("rust-analyzer/debug");
    unit(&oldest, ".cargo-lock");
    unit(&nested, ".cargo-lock");
    unit(&newer, ".cargo-build-lock");
    unit(&newest, ".cargo-lock");
    fs::create_dir_all(target.join("xtask-app-source")).expect("loose output creates");
    fs::write(
        target.join("xtask-app-source/index.js"),
        vec![1_u8; 64 * 1024],
    )
    .expect("loose output writes");

    let whole = measure(&target, u64::MAX).expect("target measures");
    let oldest_bytes = whole
        .units
        .iter()
        .find(|unit| unit.path == oldest)
        .expect("profile directory is a unit")
        .bytes;
    let bound = whole.bytes - oldest_bytes - 1;
    let footprint = measure(&target, bound).expect("target measures");
    let reclamation = reclaim(&footprint, after_recent_use()).expect("reclamation runs");

    assert_eq!(
        reclamation
            .removed
            .iter()
            .map(|removed| removed.path.as_path())
            .collect::<Vec<_>>(),
        [oldest.as_path(), nested.as_path()],
        "the two least recently used units go, oldest first, and no more"
    );
    assert!(reclamation.after <= bound);
    let remeasured = measure(&target, bound).expect("target remeasures");
    assert!(
        remeasured.bytes <= bound,
        "du agrees with the reported footprint"
    );
    assert!(emptied(&nested, ".cargo-lock"));
    assert!(
        oldest.join(".cargo-lock").is_file() && !oldest.join("deps").exists(),
        "an emptied unit keeps only the lock Cargo waits on"
    );
    assert!(intact(&newer) && intact(&newest));
    assert!(
        target.join("xtask-app-source/index.js").is_file(),
        "output outside every Cargo unit is never reclaimed"
    );
}

#[test]
fn a_unit_used_within_the_recent_window_survives_an_over_bound_pass() {
    let scratch = scratch("recent");
    let target = scratch.0.join("target");
    let stale = target.join("debug");
    let recent = target.join("cargo-build/debug");
    unit(&stale, ".cargo-lock");
    unit(&recent, ".cargo-build-lock");
    let now = SystemTime::now();
    let mut footprint = measure(&target, 0).expect("target measures");
    for measured in &mut footprint.units {
        if measured.path == stale {
            measured.last_use = now - RECENT_USE - Duration::from_secs(1);
        }
    }

    let reclamation = reclaim(&footprint, now).expect("reclamation runs");

    assert!(emptied(&stale, ".cargo-lock"), "an idle unit is reclaimed");
    assert!(
        intact(&recent),
        "a unit used within the window keeps its artifacts over the bound"
    );
    assert_eq!(reclamation.recent, std::slice::from_ref(&recent));
    assert!(
        reclamation.after > reclamation.bound,
        "the pass reports the footprint it had to leave over the bound"
    );
}

#[test]
fn a_unit_whose_cargo_lock_is_held_is_left_whole() {
    let scratch = scratch("busy");
    let target = scratch.0.join("target");
    let busy = target.join("debug");
    let idle = target.join("wasm32-unknown-unknown/debug");
    unit(&busy, ".cargo-lock");
    fs::write(busy.join(".cargo-artifact-lock"), b"").expect("second lock writes");
    let compiling = cache(&busy, "compiling");
    unit(&idle, ".cargo-lock");
    let cargo = File::open(busy.join(".cargo-artifact-lock")).expect("lock opens");
    cargo.try_lock().expect("the live Cargo holds its lock");

    let footprint = measure(&target, 0).expect("target measures");
    let reclamation = reclaim(&footprint, after_recent_use()).expect("reclamation runs");

    assert_eq!(
        reclamation
            .busy
            .iter()
            .collect::<std::collections::BTreeSet<_>>(),
        [&busy, &compiling].into_iter().collect()
    );
    assert!(compiling.join("s-abc-def-working/dep-graph.bin").is_file());
    assert!(intact(&busy), "a held unit keeps every artifact");
    assert!(emptied(&idle, ".cargo-lock"));
    drop(cargo);
    let footprint = measure(&target, 0).expect("target remeasures");
    let reclamation = reclaim(&footprint, after_recent_use()).expect("reclamation reruns");
    assert!(reclamation.busy.is_empty());
    assert!(!busy.join("deps").exists());
}

#[test]
fn the_lease_reclaims_only_when_exclusive_and_due() {
    let scratch = scratch("lease");
    let locks = scratch.0.join("locks");
    let target = scratch.0.join("target");
    let profile = target.join("debug");
    let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    unit(&profile, ".cargo-lock");

    let (first, pass) = OutputLease::enter(&locks, &target, 0, start).expect("lease enters");
    let Pass::Ran(Ok(reclamation)) = pass else {
        panic!("a never-reclaimed worktree is due: {pass:?}");
    };
    assert_eq!(reclamation.removed.len(), 1);
    assert!(emptied(&profile, ".cargo-lock"));

    unit(&profile, ".cargo-lock");
    let later = start + RECLAIM_INTERVAL * 2;
    let (second, pass) = OutputLease::enter(&locks, &target, 0, later).expect("lease shares");
    assert!(
        matches!(pass, Pass::Shared),
        "a run holding the lease excludes reclamation: {pass:?}"
    );
    assert!(intact(&profile));
    drop((first, second));

    let soon = start + RECLAIM_INTERVAL - Duration::from_secs(1);
    let (lease, pass) = OutputLease::enter(&locks, &target, 0, soon).expect("lease enters");
    assert!(matches!(pass, Pass::NotDue), "{pass:?}");
    assert!(intact(&profile));
    drop(lease);

    let (_lease, pass) =
        OutputLease::enter(&locks, &target, 0, start + RECLAIM_INTERVAL).expect("lease enters");
    assert!(matches!(pass, Pass::Ran(Ok(_))), "{pass:?}");
    assert!(emptied(&profile, ".cargo-lock"));
}

#[test]
fn completing_an_outer_run_reclaims_once_when_the_lease_is_free_and_due() {
    let scratch = scratch("completion");
    let locks = scratch.0.join("locks");
    let target = scratch.0.join("target");
    let profile = target.join("debug");
    let start = SystemTime::now();
    let due = start + RECLAIM_INTERVAL;

    let (outer, pass) = OutputLease::enter(&locks, &target, 0, start).expect("outer enters");
    assert!(matches!(pass, Pass::Ran(Ok(_))), "{pass:?}");
    unit(&profile, ".cargo-lock");
    let (nested, pass) = OutputLease::enter(&locks, &target, 0, due).expect("nested shares");
    assert!(matches!(pass, Pass::Shared), "{pass:?}");
    let pass = nested.complete(&target, 0, due).expect("nested completes");
    assert!(
        matches!(pass, Pass::Shared),
        "a nested run leaves completion to its outer run: {pass:?}"
    );
    let pass = outer
        .complete(&target, 0, due - Duration::from_secs(1))
        .expect("outer completes");
    assert!(matches!(pass, Pass::NotDue), "{pass:?}");
    assert!(intact(&profile));

    let (first, _) = OutputLease::enter(&locks, &target, 0, start).expect("first enters");
    let (second, _) = OutputLease::enter(&locks, &target, 0, start).expect("second shares");
    let pass = first.complete(&target, 0, due).expect("first completes");
    assert!(
        matches!(pass, Pass::Shared),
        "a run still holding the lease excludes completion: {pass:?}"
    );
    let pass = second.complete(&target, 0, due).expect("second completes");
    let Pass::Ran(Ok(reclamation)) = pass else {
        panic!("the last run out of a due worktree reclaims: {pass:?}");
    };
    assert!(reclamation.removed.is_empty());
    assert_eq!(
        reclamation.recent,
        std::slice::from_ref(&profile),
        "the output a run just built stays through its completion"
    );
    assert!(intact(&profile));

    let (again, _) = OutputLease::enter(&locks, &target, 0, start).expect("next run enters");
    let pass = again.complete(&target, 0, due).expect("next run completes");
    assert!(
        matches!(pass, Pass::NotDue),
        "a completion pass records the shared cooldown: {pass:?}"
    );
}

#[test]
fn only_a_pass_that_removed_data_or_was_stopped_short_reports() {
    let scratch = scratch("reportable");
    let target = scratch.0.join("target");
    unit(&target.join("debug"), ".cargo-lock");

    let under = measure(&target, u64::MAX).expect("target measures");
    let quiet = reclaim(&under, after_recent_use()).expect("reclamation runs");
    assert!(
        !quiet.reportable(),
        "a pass within its bound has nothing to say: {quiet}"
    );

    let over = measure(&target, 0).expect("target measures");
    let held = reclaim(&over, SystemTime::now()).expect("reclamation runs");
    assert!(held.removed.is_empty() && !held.recent.is_empty());
    assert!(
        held.reportable(),
        "a recent unit that kept the target over its bound is reported"
    );

    let emptied = reclaim(&over, after_recent_use()).expect("reclamation runs");
    assert!(!emptied.removed.is_empty());
    assert!(emptied.reportable(), "removed data is reported");
}

#[test]
fn a_unit_directory_refilled_before_removal_stays_without_failing() {
    let scratch = scratch("refilled");
    let refilled = scratch.0.join("incremental/refilled-0123");
    fs::create_dir_all(&refilled).expect("unit directory creates");
    fs::write(refilled.join("s-new-session.lock"), b"").expect("a writer's lock lands");
    remove_if_empty(&refilled).expect("a refilled directory is not a failure");
    assert!(refilled.join("s-new-session.lock").is_file());

    let empty = scratch.0.join("incremental/empty-0123");
    fs::create_dir_all(&empty).expect("unit directory creates");
    remove_if_empty(&empty).expect("an empty directory removes");
    assert!(!empty.exists());
}

#[cfg(windows)]
#[test]
fn a_unit_with_a_file_held_open_is_busy_and_does_not_fail_the_pass() {
    use std::os::windows::fs::OpenOptionsExt as _;

    let scratch = scratch("held");
    let target = scratch.0.join("target");
    let held = target.join("debug");
    let free = target.join("release");
    unit(&held, ".cargo-lock");
    unit(&free, ".cargo-lock");
    let pinned = held.join("deps/libcrate-0123.rlib");
    let handle = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&pinned)
        .expect("artifact opens unshared");
    let footprint = measure(&target, 0).expect("a held file still measures");
    let reclamation = reclaim(&footprint, SystemTime::now() + RECENT_USE)
        .expect("a held file does not fail the pass");
    assert_eq!(reclamation.busy, [held]);
    assert!(pinned.is_file(), "the held file stays");
    assert_eq!(
        reclamation
            .removed
            .iter()
            .map(|removed| removed.path.clone())
            .collect::<Vec<_>>(),
        [free.clone()],
        "the pass goes on to the next unit"
    );
    assert!(emptied(&free, ".cargo-lock"));
    drop(handle);
}
