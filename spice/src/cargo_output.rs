//! A per-worktree bound on Cargo output.
//!
//! Every lane compiles into its own `target/`, and Cargo never removes output
//! a later build stopped using, so without a bound each lane grows until the
//! shared volume fills. This module holds a worktree's `target/` to
//! [`LANE_OUTPUT_BOUND`] by emptying its least recently used Cargo units.
//!
//! A unit is one directory that directly holds Cargo's `.cargo-lock`,
//! `.cargo-build-lock` or `.cargo-artifact-lock`: a profile directory of a
//! target or build directory. Cargo holds one of those locks for as long as it
//! reads or writes the unit, so a unit is emptied only while this process holds
//! every one of its locks exclusively. A Cargo that starts meanwhile waits on
//! the same lock files, which are kept, and then rebuilds what it needs. The
//! unit's `.fingerprint` directory is removed first, so a pass that fails part
//! way leaves Cargo rebuilding rather than trusting a partial unit.
//!
//! Each `incremental/<crate>-<hash>` directory under a profile is a unit of its
//! own, retained by its own last use rather than its profile's. Incremental
//! state is most of a lane's footprint and one fresh executable would
//! otherwise keep every cold cache beside it, so cold caches go before the
//! compiled dependencies and executables of the profile that holds them. A
//! cache is emptied only under its profile's Cargo locks and its own rustc
//! session locks (`s-*.lock`); emptying a profile takes its caches with it.
//! A profile's last use includes its caches, so emptying the profile cannot
//! bypass a recent cache's retention.
//!
//! Some users of a unit hold no Cargo lock: the gate runs prebuilt test
//! executables after Cargo exits. Each such run holds the worktree's
//! [`OutputLease`] shared for its whole length, and reclamation runs only while
//! the lease is held exclusively, so it never overlaps a gate or developer
//! Cargo run in the same worktree. An outer run may reclaim at two points: on
//! entry, and on completion once its children have exited, so the output of a
//! session's last build is bounded without waiting for another build. A run
//! nested in another shares the outer lease and reclaims at neither point.
//!
//! Emptying preserves lock files and their directories so a waiting Cargo or
//! rustc keeps using the same lock. Only units with allocated data contribute
//! to reclamation counts. Reports name those units and their bytes; lease-driven
//! pass timing includes the existing inventory scan as well as removal.
//!
//! Windows refuses to remove a file another process holds open, such as a
//! built executable that is still running. A unit that refuses is reported
//! busy beside the units a live Cargo holds, keeps what could not be removed,
//! and does not fail the pass.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, Metadata, OpenOptions, TryLockError};
use std::io::{self, Read as _, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sill::authority::{self, FileObject};

#[cfg(test)]
mod tests;

/// Most physical bytes one worktree's `target/` holds after a reclamation pass.
pub const LANE_OUTPUT_BOUND: u64 = 24 << 30;

/// Shortest pause between two reclamation passes over one worktree.
pub const RECLAIM_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// How long a unit stays out of reclamation after its last use.
///
/// One gate prove's build cache can exceed [`LANE_OUTPUT_BOUND`] on its own,
/// so a pass ordered only by least recent use would empty what a prove built
/// minutes earlier, and a retry after a timed-out prebuild would start cold. A
/// unit used within this window is kept even while the footprint stays over
/// the bound.
pub const RECENT_USE: Duration = Duration::from_secs(60 * 60);

/// Cargo's per-profile locks; a directory directly holding any of them is a unit.
const CARGO_LOCKS: [&str; 3] = [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"];

/// The profile directory whose children are rustc's incremental caches.
const INCREMENTAL: &str = "incremental";

/// Cargo's freshness records, removed first so a partial unit is never fresh.
const FINGERPRINTS: &str = ".fingerprint";

const LEASE_FILE: &str = "cargo-output.lock";

/// Open flag for a lock: a final link is refused (Unix) or opened as itself
/// (Windows), so one planted over a lock never redirects the open.
#[cfg(unix)]
const UNFOLLOWED: i32 = libc::O_NOFOLLOW;
#[cfg(windows)]
const UNFOLLOWED: u32 = windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

/// The Cargo output directory this bound governs for one worktree.
#[must_use]
pub fn worktree_target(worktree: &Path) -> PathBuf {
    worktree.join("target")
}

/// Physical bytes below one worktree's Cargo output directory.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Footprint {
    pub target: PathBuf,
    pub bound: u64,
    pub bytes: u64,
    /// Bytes outside every unit, which reclamation never removes.
    pub unowned_bytes: u64,
    pub units: Vec<Unit>,
}

/// What one unit is to Cargo.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnitKind {
    /// One crate's rustc incremental cache under a profile; goes first.
    Incremental,
    /// A profile directory: compiled dependencies and uplifted executables.
    Profile,
}

/// One Cargo profile directory or incremental cache and the bytes it alone owns.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Unit {
    pub path: PathBuf,
    pub kind: UnitKind,
    /// Bytes below this unit, excluding units nested inside it.
    pub bytes: u64,
    /// Newest modification or status change below the unit.
    pub last_use_epoch_seconds: u64,
    #[serde(skip)]
    last_use: SystemTime,
    /// Every lock held exclusively while the unit is emptied; a cache's include
    /// its profile's Cargo locks.
    #[serde(skip)]
    locks: Vec<PathBuf>,
    /// Profile units below this one, kept whole when it is emptied.
    #[serde(skip)]
    nested: Vec<PathBuf>,
    /// Incremental caches this profile holds, emptied with it.
    #[serde(skip)]
    caches: Vec<PathBuf>,
}

/// What one reclamation pass removed and what it had to leave.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Reclamation {
    pub bound: u64,
    pub before: u64,
    pub after: u64,
    /// Time spent reclaiming; lease-driven passes include their inventory scan.
    pub elapsed: Duration,
    /// Emptied units that owned nonzero allocated bytes, excluding lock-only units.
    pub removed: Vec<Removed>,
    /// Units left whole or part emptied while the footprint was still over the
    /// bound: a live Cargo held their locks, or another process held one of
    /// their files open.
    pub busy: Vec<PathBuf>,
    /// Units used within [`RECENT_USE`] while the footprint was still over the bound.
    pub recent: Vec<PathBuf>,
}

/// One unit a pass emptied that owned nonzero allocated bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Removed {
    pub path: PathBuf,
    pub kind: UnitKind,
    pub bytes: u64,
}

/// Measure `target` without following symbolic links.
///
/// A missing directory is an empty footprint. Hard-linked files count once.
///
/// # Errors
///
/// Returns the first filesystem refusal other than an entry vanishing
/// mid-walk.
pub fn measure(target: &Path, bound: u64) -> io::Result<Footprint> {
    let mut walk = Walk::default();
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.is_dir() => walk.directory(target, None, None, &metadata)?,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a directory", target.display()),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    for unit in &mut walk.units {
        unit.last_use_epoch_seconds = unit
            .last_use
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
    }
    Ok(Footprint {
        target: target.to_path_buf(),
        bound,
        bytes: walk.bytes,
        unowned_bytes: walk.unowned,
        units: walk.units,
    })
}

/// Empty least recently used units until `footprint` fits its bound.
///
/// Units are ordered by last use, a cache before a profile used at the same
/// moment. A unit whose locks are not all free is left whole and reported
/// busy. A unit used within [`RECENT_USE`] of `now` is left whole and reported
/// recent. Emptying a profile empties the caches it still holds.
///
/// # Errors
///
/// Returns the first refusal opening a lock or removing an entry; units
/// emptied before it stay emptied.
pub fn reclaim(footprint: &Footprint, now: SystemTime) -> io::Result<Reclamation> {
    let started = Instant::now();
    let mut reclamation = Reclamation {
        bound: footprint.bound,
        before: footprint.bytes,
        after: footprint.bytes,
        elapsed: Duration::ZERO,
        removed: Vec::new(),
        busy: Vec::new(),
        recent: Vec::new(),
    };
    let recent_since = now.checked_sub(RECENT_USE).unwrap_or(UNIX_EPOCH);
    let mut order = footprint.units.iter().collect::<Vec<_>>();
    order.sort_by(|left, right| {
        left.last_use
            .cmp(&right.last_use)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut emptied = HashSet::new();
    for unit in order {
        if reclamation.after <= footprint.bound {
            break;
        }
        if emptied.contains(&unit.path) {
            continue;
        }
        if unit.last_use >= recent_since {
            reclamation.recent.push(unit.path.clone());
            continue;
        }
        let Some(_held) = hold(&unit.locks)? else {
            if !reclamation.busy.contains(&unit.path) {
                reclamation.busy.push(unit.path.clone());
            }
            continue;
        };
        let mut keep = unit
            .locks
            .iter()
            .filter(|lock| lock.starts_with(&unit.path))
            .chain(&unit.nested)
            .map(PathBuf::as_path)
            .collect::<Vec<_>>();
        // A profile takes its caches with it, except one rustc still holds.
        let mut gone = vec![unit];
        let mut cache_holds = Vec::new();
        for cache in unit
            .caches
            .iter()
            .filter(|cache| !emptied.contains(*cache))
            .filter_map(|cache| footprint.units.iter().find(|unit| &unit.path == cache))
        {
            let own = cache
                .locks
                .iter()
                .filter(|lock| lock.starts_with(&cache.path))
                .cloned()
                .collect::<Vec<_>>();
            match hold(&own)? {
                Some(held) => {
                    cache_holds.push(held);
                    gone.push(cache);
                }
                None => {
                    if !reclamation.busy.contains(&cache.path) {
                        reclamation.busy.push(cache.path.clone());
                    }
                    keep.push(cache.path.as_path());
                }
            }
        }
        let cleared = clear(&unit.path, &keep).and_then(|()| {
            if keep.is_empty() {
                remove_if_empty(&unit.path)
            } else {
                Ok(())
            }
        });
        drop(cache_holds);
        match cleared {
            Ok(()) => {}
            // Fingerprints went first, so Cargo rebuilds whatever was removed
            // before the refusal. The bytes that freed are unknown and stay
            // counted, and the caches are not visited again this pass.
            Err(error) if held_open(&error) => {
                emptied.extend(gone.into_iter().map(|unit| unit.path.clone()));
                if !reclamation.busy.contains(&unit.path) {
                    reclamation.busy.push(unit.path.clone());
                }
                continue;
            }
            Err(error) => return Err(error),
        }
        for unit in gone {
            emptied.insert(unit.path.clone());
            reclamation.after = reclamation.after.saturating_sub(unit.bytes);
            if unit.bytes > 0 {
                reclamation.removed.push(Removed {
                    path: unit.path.clone(),
                    kind: unit.kind,
                    bytes: unit.bytes,
                });
            }
        }
    }
    reclamation.elapsed = started.elapsed();
    Ok(reclamation)
}

/// What entering or completing the lease did about reclamation.
#[derive(Debug)]
pub enum Pass {
    /// The last pass over this worktree is more recent than [`RECLAIM_INTERVAL`].
    NotDue,
    /// Another run holds the lease, so this one could not reclaim.
    Shared,
    /// This run reclaimed under the exclusive lease.
    Ran(io::Result<Reclamation>),
}

/// Shared possession of one worktree's Cargo output for a run's whole length.
#[derive(Debug)]
pub struct OutputLease {
    file: File,
}

impl OutputLease {
    /// Take the worktree's output lease, reclaiming `target` down to `bound`
    /// first when nothing else holds the lease and the last pass is older than
    /// [`RECLAIM_INTERVAL`].
    ///
    /// `locks` is the worktree's lock directory. A reclamation failure is
    /// carried in the returned [`Pass`]; the lease is still held.
    ///
    /// # Errors
    ///
    /// Returns a refusal creating, opening or locking the lease file.
    pub fn enter(
        locks: &Path,
        target: &Path,
        bound: u64,
        now: SystemTime,
    ) -> io::Result<(Self, Pass)> {
        fs::create_dir_all(locks)?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(UNFOLLOWED)
            .open(locks.join(LEASE_FILE))?;
        let pass = try_pass(&mut file, target, bound, now)?;
        file.lock_shared()?;
        Ok((Self { file }, pass))
    }

    /// Release the lease after an outer run's children have exited, then
    /// reclaim `target` down to `bound` once when nothing else holds the lease
    /// and a pass is due, under the same cooldown record as [`Self::enter`].
    ///
    /// A run nested in another shares that run's lease, so its completion
    /// finds the lease held and leaves the pass to the outer run.
    ///
    /// # Errors
    ///
    /// Returns a refusal unlocking or locking the lease file.
    pub fn complete(self, target: &Path, bound: u64, now: SystemTime) -> io::Result<Pass> {
        let Self { mut file } = self;
        file.unlock()?;
        try_pass(&mut file, target, bound, now)
    }
}

/// Reclaim while holding the lease file exclusively, if no run shares it and
/// the last pass is older than [`RECLAIM_INTERVAL`]; the file is left unlocked.
fn try_pass(file: &mut File, target: &Path, bound: u64, now: SystemTime) -> io::Result<Pass> {
    match file.try_lock() {
        Ok(()) => {
            let pass = if due(file, now)? {
                let started = Instant::now();
                let reclaimed = measure(target, bound)
                    .and_then(|footprint| reclaim(&footprint, now))
                    .map(|mut reclamation| {
                        reclamation.elapsed = started.elapsed();
                        reclamation
                    });
                record_pass(file, now)?;
                Pass::Ran(reclaimed)
            } else {
                Pass::NotDue
            };
            file.unlock()?;
            Ok(pass)
        }
        Err(TryLockError::WouldBlock) => Ok(Pass::Shared),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

/// Whether the lease file records no pass, or one older than the interval.
///
/// The record is read through the lease handle: a Windows lock is mandatory,
/// so the exclusive lock this handle holds refuses every other handle's read.
fn due(file: &mut File, now: SystemTime) -> io::Result<bool> {
    let mut recorded = String::new();
    io::Seek::rewind(file)?;
    file.read_to_string(&mut recorded)?;
    let Ok(last) = recorded.trim().parse::<u64>() else {
        return Ok(true);
    };
    let last = UNIX_EPOCH + Duration::from_secs(last);
    Ok(now
        .duration_since(last)
        .is_ok_and(|elapsed| elapsed >= RECLAIM_INTERVAL))
}

fn record_pass(file: &mut File, now: SystemTime) -> io::Result<()> {
    let seconds = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    file.set_len(0)?;
    io::Seek::rewind(file)?;
    writeln!(file, "{seconds}")
}

#[derive(Default)]
struct Walk {
    bytes: u64,
    unowned: u64,
    units: Vec<Unit>,
    linked: HashSet<FileObject>,
}

impl Walk {
    /// Walk `path`, owned by unit `owner`; `caches_of` names the profile whose
    /// `incremental/` directory `path` is a child of, making `path` a cache.
    fn directory(
        &mut self,
        path: &Path,
        owner: Option<usize>,
        caches_of: Option<usize>,
        metadata: &Metadata,
    ) -> io::Result<()> {
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let mut children = Vec::new();
        let mut locks = Vec::new();
        let mut session_locks = Vec::new();
        for entry in entries {
            let entry = entry?;
            let child = entry.path();
            let name = entry.file_name();
            if CARGO_LOCKS.iter().any(|lock| name == *lock) && entry.file_type()?.is_file() {
                locks.push(child.clone());
            } else if caches_of.is_some()
                && name
                    .to_str()
                    .is_some_and(|name| name.starts_with("s-") && name.ends_with(".lock"))
                && entry.file_type()?.is_file()
            {
                session_locks.push(child.clone());
            }
            children.push(child);
        }
        let owner = if !locks.is_empty() {
            if let Some(parent) = owner {
                self.units[parent].nested.push(path.to_path_buf());
            }
            locks.sort();
            Some(self.unit(path, UnitKind::Profile, locks))
        } else if let Some(profile) = caches_of {
            self.units[profile].caches.push(path.to_path_buf());
            session_locks.sort();
            let mut locks = self.units[profile].locks.clone();
            locks.extend(session_locks);
            Some(self.unit(path, UnitKind::Incremental, locks))
        } else {
            owner
        };
        self.record(owner, path, metadata)?;
        let incremental = owner.filter(|unit| {
            let unit = &self.units[*unit];
            unit.kind == UnitKind::Profile
                && path.parent() == Some(unit.path.as_path())
                && path.file_name() == Some(OsStr::new(INCREMENTAL))
        });
        for child in children {
            let metadata = match fs::symlink_metadata(&child) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if metadata.is_dir() {
                self.directory(&child, owner, incremental, &metadata)?;
            } else {
                self.record(owner, &child, &metadata)?;
            }
        }
        if let (Some(profile), Some(cache)) = (caches_of, owner)
            && self.units[cache].kind == UnitKind::Incremental
        {
            self.units[profile].last_use =
                self.units[profile].last_use.max(self.units[cache].last_use);
        }
        Ok(())
    }

    fn unit(&mut self, path: &Path, kind: UnitKind, locks: Vec<PathBuf>) -> usize {
        self.units.push(Unit {
            path: path.to_path_buf(),
            kind,
            bytes: 0,
            last_use_epoch_seconds: 0,
            last_use: UNIX_EPOCH,
            locks,
            nested: Vec::new(),
            caches: Vec::new(),
        });
        self.units.len() - 1
    }

    /// Count one entry toward the walk and its owning unit; an entry that
    /// vanished since it was listed counts for nothing.
    fn record(&mut self, owner: Option<usize>, path: &Path, metadata: &Metadata) -> io::Result<()> {
        let occupancy = match authority::occupancy(path, metadata) {
            Ok(occupancy) => occupancy,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if !metadata.is_dir()
            && occupancy
                .linked
                .is_some_and(|object| !self.linked.insert(object))
        {
            return Ok(());
        }
        let bytes = occupancy.allocated;
        self.bytes = self.bytes.saturating_add(bytes);
        let Some(owner) = owner else {
            self.unowned = self.unowned.saturating_add(bytes);
            return Ok(());
        };
        let unit = &mut self.units[owner];
        unit.bytes = unit.bytes.saturating_add(bytes);
        unit.last_use = unit.last_use.max(occupancy.changed);
        Ok(())
    }
}

/// Whether a removal was refused because another process holds the entry open.
///
/// Only Windows refuses that: a mapped executable or a handle opened without
/// delete sharing pins its file, where Unix unlinks an open file.
fn held_open(error: &io::Error) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION};

        error
            .raw_os_error()
            .and_then(|code| u32::try_from(code).ok())
            .is_some_and(|code| code == ERROR_ACCESS_DENIED || code == ERROR_SHARING_VIOLATION)
    }
    #[cfg(not(windows))]
    {
        let _ = error;
        false
    }
}

/// Exclusively hold every lock, or none when any is held elsewhere.
fn hold(locks: &[PathBuf]) -> io::Result<Option<Vec<File>>> {
    let mut held = Vec::with_capacity(locks.len());
    for path in locks {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(UNFOLLOWED)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => held.push(file),
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
    Ok(Some(held))
}

/// Remove everything below `directory` except `keep` and the directories
/// leading to it, fingerprints first.
fn clear(directory: &Path, keep: &[&Path]) -> io::Result<()> {
    let mut children = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    children.sort_by_key(|child| child.file_name() != Some(OsStr::new(FINGERPRINTS)));
    for child in children {
        if keep.contains(&child.as_path()) {
            continue;
        }
        if keep.iter().any(|kept| kept.starts_with(&child)) {
            clear(&child, keep)?;
            continue;
        }
        let removed = match fs::symlink_metadata(&child) {
            Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&child),
            Ok(_) => fs::remove_file(&child),
            Err(error) => Err(error),
        };
        match removed {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Remove `directory` once it is empty; one refilled since it was cleared
/// stays, since a live writer owns what it now holds.
fn remove_if_empty(directory: &Path) -> io::Result<()> {
    match fs::remove_dir(directory) {
        Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => Ok(()),
        removed => removed,
    }
}

impl Reclamation {
    /// Whether a person should hear about this pass: it removed allocated data,
    /// or a busy or recently used unit stopped it short of the bound.
    #[must_use]
    pub fn reportable(&self) -> bool {
        !self.removed.is_empty() || !self.busy.is_empty() || !self.recent.is_empty()
    }
}

impl fmt::Display for Footprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "cargo output {} of {} bound in {}",
            Bytes(self.bytes),
            Bytes(self.bound),
            self.target.display()
        )?;
        let mut units = self.units.iter().collect::<Vec<_>>();
        units.sort_by_key(|unit| std::cmp::Reverse(unit.bytes));
        for unit in units {
            let path = unit.path.strip_prefix(&self.target).unwrap_or(&unit.path);
            writeln!(formatter, "  {:>9}  {}", Bytes(unit.bytes), path.display())?;
        }
        write!(
            formatter,
            "  {:>9}  outside Cargo units, never reclaimed",
            Bytes(self.unowned_bytes)
        )
    }
}

impl fmt::Display for Reclamation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let removed = self.before.saturating_sub(self.after);
        let caches = self
            .removed
            .iter()
            .filter(|unit| unit.kind == UnitKind::Incremental)
            .count();
        write!(
            formatter,
            "reclaimed {} by emptying {} incremental caches and {} profiles: {} -> {} of {} bound in {:.3}s",
            Bytes(removed),
            caches,
            self.removed.len() - caches,
            Bytes(self.before),
            Bytes(self.after),
            Bytes(self.bound),
            self.elapsed.as_secs_f64(),
        )?;
        if !self.busy.is_empty() {
            write!(formatter, "; left {} units in use", self.busy.len())?;
        }
        if !self.recent.is_empty() {
            write!(
                formatter,
                "; kept {} units used within the last hour",
                self.recent.len()
            )?;
        }
        for unit in &self.removed {
            let kind = match unit.kind {
                UnitKind::Incremental => "incremental cache",
                UnitKind::Profile => "profile",
            };
            write!(
                formatter,
                "\n[cargo output] emptied {kind} {:?}: {} ({} bytes)",
                unit.path,
                Bytes(unit.bytes),
                unit.bytes,
            )?;
        }
        Ok(())
    }
}

struct Bytes(u64);

impl fmt::Display for Bytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
        const STEP: u64 = 1024;
        let mut value = self.0;
        let mut tenths = 0;
        let mut unit = 0;
        while value >= STEP && unit + 1 < UNITS.len() {
            tenths = (value % STEP) * 10 / STEP;
            value /= STEP;
            unit += 1;
        }
        let rendered = if unit == 0 {
            format!("{value} {}", UNITS[unit])
        } else {
            format!("{value}.{tenths} {}", UNITS[unit])
        };
        formatter.pad(&rendered)
    }
}
