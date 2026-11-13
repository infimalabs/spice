//! Lock-aware collection for Cargo's configured build and target directories.

mod discovery;
mod report;
mod targets;

use report::render_report;
pub use targets::BuildCacheTargetDirectory;
use targets::inventory_target_roots;

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, Metadata};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sill::{Acquisition, LockFile, LockName, Possession};

const CACHE_TAG: &str = "CACHEDIR.TAG";
const BUILD_LOCK_NAME: &str = ".cargo-build-lock";
const TARGET_LOCK_NAME: &str = ".cargo-lock";
const DEFAULT_RETAIN_LIVE_WORKTREES: usize = 2;

macro_rules! is_hex_name {
    ($name:expr, $length:expr) => {
        $name.len() == $length
            && $name
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    };
}

/// One build-cache collection request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildCacheRequest {
    /// Remove selected roots instead of only reporting what would be removed.
    pub apply: bool,
    /// Maximum physical bytes the inventoried cache roots should retain.
    pub max_bytes: Option<u64>,
    /// Minimum free bytes the containing filesystem should have after collection.
    pub free_space_floor: Option<u64>,
    /// Number of non-current linked worktree roots to retain by most recent use.
    pub retain_live_worktrees: usize,
}

impl Default for BuildCacheRequest {
    fn default() -> Self {
        Self {
            apply: false,
            max_bytes: None,
            free_space_floor: None,
            retain_live_worktrees: DEFAULT_RETAIN_LIVE_WORKTREES,
        }
    }
}

/// Fully discovered context for one collection pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildCacheContext {
    /// Cargo's `build_directory` for the current worktree.
    pub build_directory: PathBuf,
    /// Cargo's target directory and owning current worktree, when it exists.
    pub target_directory: Option<BuildCacheTargetDirectory>,
    /// Build directories for linked worktrees in the same hierarchy.
    pub live_build_directories: Vec<PathBuf>,
    /// Cargo target directories and owning linked worktrees.
    pub live_target_directories: Vec<BuildCacheTargetDirectory>,
    /// Available bytes for the filesystem containing the current build root.
    pub available_bytes: Option<u64>,
}

impl BuildCacheContext {
    /// Create a context that protects only the current worktree root.
    #[must_use]
    pub fn new(build_directory: impl Into<PathBuf>) -> Self {
        Self {
            build_directory: build_directory.into(),
            target_directory: None,
            live_build_directories: Vec::new(),
            live_target_directories: Vec::new(),
            available_bytes: None,
        }
    }
}

/// Result of one build-cache collection pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildCacheReport {
    /// The managed build hierarchy, absent when Cargo builds in its target root.
    pub hierarchy: Option<PathBuf>,
    /// The current worktree's resolved build root.
    pub current: PathBuf,
    /// The current worktree's resolved target root, when it is an admitted root.
    pub current_target: Option<PathBuf>,
    /// Physical bytes inventoried below candidate roots.
    pub total_bytes: u64,
    /// Bytes selected for removal by this run.
    pub selected_bytes: u64,
    /// Bytes actually removed by this run.
    pub reclaimed_bytes: u64,
    /// Bytes retained after planning and any attempted removals.
    pub kept_bytes: u64,
    /// Bytes that were not collectible because of locks, retention, or malformed roots.
    pub skipped_bytes: u64,
    /// Target remaining bytes after applying the max-size and free-space settings.
    pub target_bytes: u64,
    /// Available bytes reported for the containing filesystem before collection.
    pub available_bytes: Option<u64>,
    /// Per-root inventory and disposition.
    pub roots: Vec<BuildCacheRootReport>,
}

/// Inventory and outcome for one Cargo cache root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildCacheRootReport {
    /// Cargo cache class this root belongs to.
    pub kind: BuildCacheRootKind,
    /// Absolute path to the candidate root.
    pub path: PathBuf,
    /// Resolved boundary the root must remain below during removal.
    pub removal_boundary: PathBuf,
    /// Physical bytes below this root, computed without following symlinks.
    pub bytes: u64,
    /// Seconds since the Unix epoch for the newest metadata timestamp observed.
    pub last_use_epoch_seconds: u64,
    /// Nanoseconds since the Unix epoch for deterministic LRU ordering.
    pub last_use_epoch_nanos: u128,
    /// Root-relative path that supplied the newest timestamp.
    pub last_use_evidence: PathBuf,
    /// Existing `.cargo-lock` and `.cargo-build-lock` files observed below this root.
    pub locks: Vec<PathBuf>,
    /// Final disposition.
    pub disposition: BuildCacheDisposition,
}

/// Cargo cache class represented by one report row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildCacheRootKind {
    /// A hashed root in Cargo's configured `build_directory` hierarchy.
    ManagedBuild,
    /// A Cargo-tagged `target_directory` below its owning worktree.
    WorktreeTarget,
}

/// What one root's collection pass did with it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BuildCacheDisposition {
    /// The root was eligible but newer than every selected victim.
    Kept,
    /// The current worktree root is never collected.
    RetainedCurrent,
    /// One of the configured hottest linked worktree roots is retained.
    RetainedLiveWorktree,
    /// A lock file was held or unreadable, so the whole root was skipped.
    SkippedActive,
    /// The root did not satisfy the collector's safety shape.
    SkippedMalformed(String),
    /// Dry-run selected this root but did not remove it.
    WouldReclaim,
    /// Apply selected this root and removed it.
    Reclaimed,
}

impl BuildCacheDisposition {
    fn is_selected(&self) -> bool {
        matches!(self, Self::WouldReclaim | Self::Reclaimed)
    }

    fn is_skipped(&self) -> bool {
        matches!(
            self,
            Self::RetainedCurrent
                | Self::RetainedLiveWorktree
                | Self::SkippedActive
                | Self::SkippedMalformed(_)
        )
    }
}

/// Discover Cargo's build and target directories and collect them.
///
/// The command asks Cargo once per linked worktree for the `build_directory`
/// and `target_directory` fields emitted by `cargo metadata`. It inventories
/// both the two-level managed hierarchy and real Cargo-tagged target roots below
/// their owning worktrees in one global report and LRU selection.
///
/// `max_bytes` is a physical-byte cap for all candidate roots. A
/// `free_space_floor` asks the collector to reclaim at least the byte deficit
/// reported by `statvfs` before applying the same deterministic oldest-first
/// selection. When neither setting is present the command is inventory-only.
pub fn build_cache(
    request: &BuildCacheRequest,
    output: &mut impl Write,
) -> Result<BuildCacheReport, BuildCacheError> {
    let workspace = workspace_root()?;
    let directories = discovery::discover_cargo_directories(&workspace)?;
    let mut context = BuildCacheContext {
        available_bytes: filesystem_available_bytes(&directories.current.build_directory).ok(),
        build_directory: directories.current.build_directory,
        target_directory: Some(BuildCacheTargetDirectory::new(
            &workspace,
            directories.current.target_directory,
        )),
        live_build_directories: Vec::new(),
        live_target_directories: Vec::new(),
    };
    let hierarchy = hierarchy_from_context(&context)?;
    for linked in directories.linked {
        if let Some(hierarchy) = &hierarchy
            && root_under_boundary(&hierarchy.root, &linked.directories.build_directory).is_ok()
        {
            context
                .live_build_directories
                .push(linked.directories.build_directory);
        }
        context
            .live_target_directories
            .push(BuildCacheTargetDirectory::new(
                linked.worktree,
                linked.directories.target_directory,
            ));
    }
    collect_build_cache_at(request, &context, output)
}

/// Collect an explicitly supplied build-cache context.
///
/// Tests use this entry point to build closed fixtures without invoking Cargo or
/// Git. Production callers should use [`build_cache`].
pub fn collect_build_cache_at(
    request: &BuildCacheRequest,
    context: &BuildCacheContext,
    output: &mut impl Write,
) -> Result<BuildCacheReport, BuildCacheError> {
    let hierarchy = hierarchy_from_context(context)?;
    let mut roots = match &hierarchy {
        Some(hierarchy) => inventory_hierarchy(hierarchy)?,
        None => Vec::new(),
    };
    let current =
        fs::canonicalize(&context.build_directory).map_err(|source| BuildCacheError::Io {
            path: context.build_directory.clone(),
            source,
        })?;
    let mut current_roots = BTreeSet::from([current.clone()]);
    let mut requested_live_roots = match &hierarchy {
        Some(hierarchy) => context
            .live_build_directories
            .iter()
            .map(|path| root_under_boundary(&hierarchy.root, path))
            .collect::<Result<BTreeSet<_>, _>>()?,
        None => BTreeSet::new(),
    };
    let current_target = inventory_target_roots(
        context,
        &mut roots,
        &mut current_roots,
        &mut requested_live_roots,
    )?;
    let live_roots =
        retained_live_roots(request.retain_live_worktrees, &requested_live_roots, &roots);

    for root in &mut roots {
        if current_roots.contains(&root.path) {
            root.disposition = BuildCacheDisposition::RetainedCurrent;
        } else if live_roots.contains(&root.path) {
            root.disposition = BuildCacheDisposition::RetainedLiveWorktree;
        } else if matches!(root.disposition, BuildCacheDisposition::Kept) {
            match probe_locks(&root.locks)? {
                LockProbe::Available => {}
                LockProbe::Held => root.disposition = BuildCacheDisposition::SkippedActive,
            }
        }
    }

    let total_bytes = roots
        .iter()
        .map(|root| root.bytes)
        .try_fold(0_u64, checked_add)?;
    let target_bytes = target_bytes(request, total_bytes, context.available_bytes)?;
    mark_lru_victims(target_bytes, total_bytes, &mut roots);

    if request.apply {
        for root in &mut roots {
            if root.disposition != BuildCacheDisposition::WouldReclaim {
                continue;
            }
            let Some(_locks) = acquire_locks(&root.locks)? else {
                root.disposition = BuildCacheDisposition::SkippedActive;
                continue;
            };
            remove_tree(&root.removal_boundary, &root.path)?;
            root.disposition = BuildCacheDisposition::Reclaimed;
        }
    }

    let selected_bytes = roots
        .iter()
        .filter(|root| root.disposition.is_selected())
        .map(|root| root.bytes)
        .try_fold(0_u64, checked_add)?;
    let reclaimed_bytes = roots
        .iter()
        .filter(|root| root.disposition == BuildCacheDisposition::Reclaimed)
        .map(|root| root.bytes)
        .try_fold(0_u64, checked_add)?;
    let skipped_bytes = roots
        .iter()
        .filter(|root| root.disposition.is_skipped())
        .map(|root| root.bytes)
        .try_fold(0_u64, checked_add)?;
    let kept_bytes = total_bytes.saturating_sub(reclaimed_bytes);

    roots.sort_by(|left, right| left.path.cmp(&right.path));
    let report = BuildCacheReport {
        hierarchy: hierarchy.map(|hierarchy| hierarchy.root),
        current,
        current_target,
        total_bytes,
        selected_bytes,
        reclaimed_bytes,
        kept_bytes,
        skipped_bytes,
        target_bytes,
        available_bytes: context.available_bytes,
        roots,
    };
    render_report(&report, request.apply, output)?;
    Ok(report)
}

/// Parse a byte quantity accepted by `spice dev xtask -- build-cache`.
///
/// Bare integers are bytes. `K`, `M`, `G`, `T` and their `iB`/`B` spellings use
/// binary multiples, matching Cargo cache sizing in powers of two.
pub fn parse_byte_quantity(value: &str) -> Result<u64, BuildCacheError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(BuildCacheError::InvalidBytes(value.to_owned()));
    }
    let digits = value
        .char_indices()
        .take_while(|(_, character)| character.is_ascii_digit())
        .map(|(index, character)| index + character.len_utf8())
        .last()
        .unwrap_or(0);
    if digits == 0 {
        return Err(BuildCacheError::InvalidBytes(value.to_owned()));
    }
    let number = value[..digits]
        .parse::<u64>()
        .map_err(|_| BuildCacheError::InvalidBytes(value.to_owned()))?;
    let suffix = value[digits..].trim().to_ascii_lowercase();
    let multiplier = match suffix.as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1024_u64,
        "m" | "mb" | "mib" => 1024_u64.pow(2),
        "g" | "gb" | "gib" => 1024_u64.pow(3),
        "t" | "tb" | "tib" => 1024_u64.pow(4),
        _ => return Err(BuildCacheError::InvalidBytes(value.to_owned())),
    };
    number
        .checked_mul(multiplier)
        .ok_or_else(|| BuildCacheError::InvalidBytes(value.to_owned()))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CacheHierarchy {
    root: PathBuf,
}

fn hierarchy_from_context(
    context: &BuildCacheContext,
) -> Result<Option<CacheHierarchy>, BuildCacheError> {
    if context.target_directory.as_ref().is_some_and(|target| {
        target.directory == context.build_directory
            && target.directory != target.worktree
            && target.directory.starts_with(&target.worktree)
    }) {
        return Ok(None);
    }
    hierarchy_from_build_directory(&context.build_directory).map(Some)
}

fn hierarchy_from_build_directory(
    build_directory: &Path,
) -> Result<CacheHierarchy, BuildCacheError> {
    let canonical = fs::canonicalize(build_directory).map_err(|source| BuildCacheError::Io {
        path: build_directory.to_path_buf(),
        source,
    })?;
    let hash = canonical
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| BuildCacheError::Shape(canonical.clone()))?;
    let bucket = canonical
        .parent()
        .ok_or_else(|| BuildCacheError::Shape(canonical.clone()))?;
    let bucket_name = bucket
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| BuildCacheError::Shape(canonical.clone()))?;
    let root = bucket
        .parent()
        .ok_or_else(|| BuildCacheError::Shape(canonical.clone()))?;
    if !is_hex_name!(bucket_name, 2) || !is_hex_name!(hash, 14) {
        return Err(BuildCacheError::Shape(canonical));
    }
    Ok(CacheHierarchy {
        root: root.to_path_buf(),
    })
}

fn inventory_hierarchy(
    hierarchy: &CacheHierarchy,
) -> Result<Vec<BuildCacheRootReport>, BuildCacheError> {
    let mut roots = Vec::new();
    for bucket in sorted_children(&hierarchy.root)? {
        let Some(bucket_name) = file_name(&bucket) else {
            continue;
        };
        if !is_hex_name!(&bucket_name, 2) {
            continue;
        }
        let metadata = fs::symlink_metadata(&bucket).map_err(|source| BuildCacheError::Io {
            path: bucket.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() {
            continue;
        }
        for root in sorted_children(&bucket)? {
            let Some(hash) = file_name(&root) else {
                continue;
            };
            if !is_hex_name!(&hash, 14) {
                continue;
            }
            roots.push(inventory_root(
                &root,
                BuildCacheRootKind::ManagedBuild,
                &hierarchy.root,
            )?);
        }
    }
    Ok(roots)
}

fn inventory_root(
    path: &Path,
    kind: BuildCacheRootKind,
    removal_boundary: &Path,
) -> Result<BuildCacheRootReport, BuildCacheError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| BuildCacheError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_dir() {
        return Ok(malformed_root(
            path,
            &metadata,
            kind,
            removal_boundary,
            "candidate root is not a real directory",
        ));
    }
    let mut tree = TreeInventory::new(path);
    tree.walk(path)?;
    let tag = path.join(CACHE_TAG);
    let disposition = match fs::symlink_metadata(&tag) {
        Ok(metadata) if metadata.file_type().is_file() => BuildCacheDisposition::Kept,
        Ok(_) => {
            BuildCacheDisposition::SkippedMalformed(format!("{CACHE_TAG} is not a regular file"))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            BuildCacheDisposition::SkippedMalformed(format!("{CACHE_TAG} is missing"))
        }
        Err(source) => {
            return Err(BuildCacheError::Io { path: tag, source });
        }
    };
    Ok(BuildCacheRootReport {
        kind,
        path: path.to_path_buf(),
        removal_boundary: removal_boundary.to_path_buf(),
        bytes: tree.bytes,
        last_use_epoch_seconds: tree.last_use_epoch_seconds(),
        last_use_epoch_nanos: tree.last_use_epoch_nanos(),
        last_use_evidence: tree.last_use_evidence,
        locks: tree.locks,
        disposition,
    })
}

fn malformed_root(
    path: &Path,
    metadata: &Metadata,
    kind: BuildCacheRootKind,
    removal_boundary: &Path,
    reason: &str,
) -> BuildCacheRootReport {
    BuildCacheRootReport {
        kind,
        path: path.to_path_buf(),
        removal_boundary: removal_boundary.to_path_buf(),
        bytes: physical_bytes(metadata),
        last_use_epoch_seconds: epoch_seconds(metadata.modified().unwrap_or(UNIX_EPOCH)),
        last_use_epoch_nanos: epoch_nanos(metadata.modified().unwrap_or(UNIX_EPOCH)),
        last_use_evidence: PathBuf::from("."),
        locks: Vec::new(),
        disposition: BuildCacheDisposition::SkippedMalformed(reason.to_owned()),
    }
}

struct TreeInventory {
    root: PathBuf,
    bytes: u64,
    last_use: SystemTime,
    last_use_evidence: PathBuf,
    locks: Vec<PathBuf>,
}

impl TreeInventory {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            bytes: 0,
            last_use: UNIX_EPOCH,
            last_use_evidence: PathBuf::from("."),
            locks: Vec::new(),
        }
    }

    fn walk(&mut self, root: &Path) -> Result<(), BuildCacheError> {
        let mut stack = vec![root.to_path_buf()];
        while let Some(path) = stack.pop() {
            let metadata = fs::symlink_metadata(&path).map_err(|source| BuildCacheError::Io {
                path: path.clone(),
                source,
            })?;
            self.bytes = checked_add(self.bytes, physical_bytes(&metadata))?;
            self.record_last_use(&path, &metadata);
            if matches!(
                path.file_name(),
                Some(name)
                    if name == OsStr::new(BUILD_LOCK_NAME)
                        || name == OsStr::new(TARGET_LOCK_NAME)
            ) {
                self.locks.push(path.clone());
            }
            if metadata.file_type().is_dir() {
                let mut children = sorted_children(&path)?;
                children.reverse();
                stack.extend(children);
            }
        }
        self.locks.sort();
        Ok(())
    }

    fn record_last_use(&mut self, path: &Path, metadata: &Metadata) {
        let Ok(modified) = metadata.modified() else {
            return;
        };
        let relative = path.strip_prefix(&self.root).unwrap_or(path);
        if modified > self.last_use
            || (modified == self.last_use && relative < self.last_use_evidence.as_path())
        {
            self.last_use = modified;
            self.last_use_evidence = if relative.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                relative.to_path_buf()
            };
        }
    }

    fn last_use_epoch_seconds(&self) -> u64 {
        epoch_seconds(self.last_use)
    }

    fn last_use_epoch_nanos(&self) -> u128 {
        epoch_nanos(self.last_use)
    }
}

fn retained_live_roots(
    count: usize,
    requested: &BTreeSet<PathBuf>,
    roots: &[BuildCacheRootReport],
) -> BTreeSet<PathBuf> {
    if count == 0 || requested.is_empty() {
        return BTreeSet::new();
    }
    let mut live = roots
        .iter()
        .filter(|root| requested.contains(&root.path))
        .collect::<Vec<_>>();
    live.sort_by(|left, right| {
        right
            .last_use_epoch_seconds
            .cmp(&left.last_use_epoch_seconds)
            .then_with(|| right.last_use_epoch_nanos.cmp(&left.last_use_epoch_nanos))
            .then_with(|| left.path.cmp(&right.path))
    });
    live.into_iter()
        .take(count)
        .map(|root| root.path.clone())
        .collect()
}

fn root_under_boundary(boundary: &Path, candidate: &Path) -> Result<PathBuf, BuildCacheError> {
    let boundary = fs::canonicalize(boundary).map_err(|source| BuildCacheError::Io {
        path: boundary.to_path_buf(),
        source,
    })?;
    let root = fs::canonicalize(candidate).map_err(|source| BuildCacheError::Io {
        path: candidate.to_path_buf(),
        source,
    })?;
    if root != boundary && root.starts_with(&boundary) {
        Ok(root)
    } else {
        Err(BuildCacheError::OutsideHierarchy {
            hierarchy: boundary,
            path: root,
        })
    }
}

enum LockProbe {
    Available,
    Held,
}

fn probe_locks(locks: &[PathBuf]) -> Result<LockProbe, BuildCacheError> {
    match acquire_locks(locks)? {
        Some(_) => Ok(LockProbe::Available),
        None => Ok(LockProbe::Held),
    }
}

fn acquire_locks(locks: &[PathBuf]) -> Result<Option<Vec<Possession>>, BuildCacheError> {
    let mut possessions = Vec::new();
    for path in locks {
        let name = path.file_name().and_then(OsStr::to_str).ok_or_else(|| {
            BuildCacheError::RefuseRemove(format!("lock path has no file name: {}", path.display()))
        })?;
        let name = LockName::new(name).map_err(BuildCacheError::LockName)?;
        let lock = LockFile::new(name, path);
        match lock.try_acquire() {
            Ok(Acquisition::Acquired(possession)) => possessions.push(possession),
            Ok(Acquisition::Contended) => return Ok(None),
            Err(source) => {
                return Err(BuildCacheError::Io {
                    path: lock.path().to_path_buf(),
                    source,
                });
            }
        }
    }
    Ok(Some(possessions))
}

fn target_bytes(
    request: &BuildCacheRequest,
    total_bytes: u64,
    available_bytes: Option<u64>,
) -> Result<u64, BuildCacheError> {
    let mut target = request.max_bytes.unwrap_or(total_bytes);
    if let Some(floor) = request.free_space_floor {
        let available = available_bytes.ok_or(BuildCacheError::FreeSpaceUnavailable)?;
        if available < floor {
            target = target.min(total_bytes.saturating_sub(floor - available));
        }
    }
    Ok(target)
}

fn mark_lru_victims(target_bytes: u64, total_bytes: u64, roots: &mut [BuildCacheRootReport]) {
    let mut remaining = total_bytes;
    let mut candidates = roots
        .iter()
        .enumerate()
        .filter(|(_, root)| root.disposition == BuildCacheDisposition::Kept)
        .map(|(index, root)| {
            (
                root.last_use_epoch_seconds,
                root.last_use_epoch_nanos,
                root.path.clone(),
                root.bytes,
                index,
            )
        })
        .collect::<Vec<_>>();
    candidates.sort();

    for (_, _, _, bytes, index) in candidates {
        if remaining <= target_bytes {
            break;
        }
        remaining = remaining.saturating_sub(bytes);
        roots[index].disposition = BuildCacheDisposition::WouldReclaim;
    }
}

fn remove_tree(boundary: &Path, root: &Path) -> Result<(), BuildCacheError> {
    let root = root_under_boundary(boundary, root)?;
    let metadata = fs::symlink_metadata(&root).map_err(|source| BuildCacheError::Io {
        path: root.clone(),
        source,
    })?;
    if !metadata.file_type().is_dir() {
        return Err(BuildCacheError::RefuseRemove(format!(
            "{} is not a real directory",
            root.display()
        )));
    }
    remove_tree_no_follow(&root)
}

fn remove_tree_no_follow(path: &Path) -> Result<(), BuildCacheError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| BuildCacheError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_dir() {
        for child in sorted_children(path)? {
            remove_tree_no_follow(&child)?;
        }
        fs::remove_dir(path).map_err(|source| BuildCacheError::Io {
            path: path.to_path_buf(),
            source,
        })
    } else {
        fs::remove_file(path).map_err(|source| BuildCacheError::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

fn sorted_children(path: &Path) -> Result<Vec<PathBuf>, BuildCacheError> {
    let mut children = fs::read_dir(path)
        .map_err(|source| BuildCacheError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|source| BuildCacheError::Io {
                    path: path.to_path_buf(),
                    source,
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    children.sort();
    Ok(children)
}

fn checked_add(left: u64, right: u64) -> Result<u64, BuildCacheError> {
    left.checked_add(right).ok_or(BuildCacheError::SizeOverflow)
}

fn file_name(path: &Path) -> Option<String> {
    path.file_name().and_then(OsStr::to_str).map(str::to_owned)
}

fn epoch_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn epoch_nanos(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

#[cfg(unix)]
fn physical_bytes(metadata: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt as _;

    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn physical_bytes(metadata: &Metadata) -> u64 {
    metadata.len()
}

#[cfg(unix)]
fn filesystem_available_bytes(path: &Path) -> Result<u64, BuildCacheError> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt as _;

    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        BuildCacheError::PathContainsNul {
            path: path.to_path_buf(),
        }
    })?;
    let mut stats = MaybeUninit::<libc::statvfs>::uninit();
    let result = unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) };
    if result != 0 {
        return Err(BuildCacheError::Io {
            path: PathBuf::from(path.to_string_lossy().into_owned()),
            source: io::Error::last_os_error(),
        });
    }
    let stats = unsafe { stats.assume_init() };
    let available = u128::from(stats.f_bavail) * u128::from(stats.f_frsize);
    Ok(available.min(u128::from(u64::MAX)) as u64)
}

#[cfg(not(unix))]
fn filesystem_available_bytes(path: &Path) -> Result<u64, BuildCacheError> {
    Err(BuildCacheError::Io {
        path: path.to_path_buf(),
        source: io::Error::new(
            io::ErrorKind::Unsupported,
            "filesystem free-space measurement requires a Unix target",
        ),
    })
}

fn workspace_root() -> Result<PathBuf, BuildCacheError> {
    std::env::current_dir().map_err(|source| BuildCacheError::Io {
        path: PathBuf::from("."),
        source,
    })
}

/// Every way the build-cache collector refuses.
#[derive(Debug)]
pub enum BuildCacheError {
    /// The build directory does not have Cargo's two-level hash shape.
    Shape(PathBuf),
    /// A candidate path escaped the resolved hierarchy.
    OutsideHierarchy { hierarchy: PathBuf, path: PathBuf },
    /// A configured free-space floor could not be evaluated.
    FreeSpaceUnavailable,
    /// A byte quantity did not parse.
    InvalidBytes(String),
    /// A filesystem path contained a nul byte.
    PathContainsNul { path: PathBuf },
    /// Physical-byte addition overflowed.
    SizeOverflow,
    /// A removal was refused after safety checks.
    RefuseRemove(String),
    /// A child process did not launch.
    Launch {
        program: &'static str,
        source: io::Error,
    },
    /// A child process returned failure.
    CommandFailed {
        program: &'static str,
        code: Option<i32>,
        stderr: String,
    },
    /// An observation could not be interpreted.
    Observation {
        label: &'static str,
        observed: String,
    },
    /// JSON output was malformed.
    Json {
        label: &'static str,
        source: serde_json::Error,
    },
    /// A lock name was impossible.
    LockName(sill::LockNameError),
    /// The filesystem refused an operation.
    Io { path: PathBuf, source: io::Error },
    /// The report could not be written.
    Output(io::Error),
}

impl fmt::Display for BuildCacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shape(path) => write!(
                formatter,
                "Cargo managed build directory is not CargoBuild/<hex>/<hash>: {}",
                path.display()
            ),
            Self::OutsideHierarchy { hierarchy, path } => write!(
                formatter,
                "{} is outside resolved build-cache removal boundary {}",
                path.display(),
                hierarchy.display()
            ),
            Self::FreeSpaceUnavailable => {
                formatter.write_str("free-space floor requires filesystem free-space observation")
            }
            Self::InvalidBytes(value) => write!(formatter, "invalid byte quantity {value:?}"),
            Self::PathContainsNul { path } => {
                write!(formatter, "path contains nul byte: {}", path.display())
            }
            Self::SizeOverflow => formatter.write_str("build-cache byte inventory overflowed"),
            Self::RefuseRemove(reason) => {
                write!(formatter, "build-cache removal refused: {reason}")
            }
            Self::Launch { program, source } => {
                write!(formatter, "{program} did not launch: {source}")
            }
            Self::CommandFailed {
                program,
                code,
                stderr,
            } => write!(
                formatter,
                "{program} failed with exit {code:?}: {}",
                stderr.trim()
            ),
            Self::Observation { label, observed } => {
                write!(formatter, "{label} observation is invalid: {observed:?}")
            }
            Self::Json { label, source } => write!(formatter, "{label} JSON is invalid: {source}"),
            Self::LockName(error) => write!(formatter, "Cargo lock name is invalid: {error}"),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Output(error) => write!(formatter, "build-cache report failed: {error}"),
        }
    }
}

impl Error for BuildCacheError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Launch { source, .. } | Self::Io { source, .. } | Self::Output(source) => {
                Some(source)
            }
            Self::Json { source, .. } => Some(source),
            Self::LockName(source) => Some(source),
            _ => None,
        }
    }
}
