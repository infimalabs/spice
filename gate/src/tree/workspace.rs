//! Private materialization workspace ownership and abandoned-root reclamation.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(target_family = "unix")]
use std::os::unix::fs::MetadataExt as _;

use sill::{Acquisition, LockFile, Possession};

use super::{
    SelectedFiles, SelectedTree, SelectionSeal, StagedAuthority, TreeError,
    create_private_workspace, make_removable, private_directory,
};
use crate::held_root::{self, HeldRootRemoval};
use crate::state::RenameMap;

/// How gate tree selection names itself in a refusal, and names its locks.
const WORKSPACE_LABEL: &str = "gate tree workspace";
const WORKSPACE_LOCK: &str = "gate-tree-workspace";

/// The prefix every gate tree workspace name begins with.
const WORKSPACE_PREFIX: &str = "spice-gate-tree-";
const WORKSPACE_PARENT_PREFIX: &str = "spice-gate-workspaces-";

/// The prefix every workspace retained between runs begins with.
///
/// Retained workspaces carry the exact bytes of the last selection
/// materialized into them, so the next selection can leave every unchanged
/// file's modification time alone. Cargo decides freshness by comparing each
/// source file's modification time against the artifact built from it, so a
/// tree written from scratch on every run reports every file as newer than
/// every artifact and rebuilds the entire local graph. The path the tree is
/// materialized at is not part of that comparison: Cargo records dep-info
/// paths relative to the package root and does not hash the source path into
/// unit metadata. Only the timestamps carry the cost.
///
/// The family is named apart from [`WORKSPACE_PREFIX`] rather than nested
/// inside it because an older gate build sharing this temporary directory
/// sweeps every *unheld* workspace of its own family, and a retained
/// workspace is unheld between runs by design. A separate prefix is invisible
/// to that sweep, so a mixed-version machine costs a recompile at most where a
/// shared prefix would cost one on every run.
const RETAINED_PREFIX: &str = "spice-gate-keep-";

/// How many retained workspaces one repository may hold at once.
///
/// Each is a full copy of the selection, so this is a disk cost paid per
/// concurrently measuring run rather than per run. Two covers a pre-commit
/// reservation overlapping one proof; a third concurrent run falls back to an
/// ephemeral workspace and pays the compile it would have paid anyway.
const RETAINED_WORKSPACES: usize = 2;

/// File inside a retained workspace naming the repository it serves.
///
/// A retained workspace outlives the run that filled it, so nothing about a
/// live process distinguishes it from wreckage. This file is what a later
/// sweep reads to decide that the repository is gone and the copy is owed to
/// nobody.
const RETAINED_OWNER: &str = "repository";

/// Hex characters of the repository digest a retained workspace name carries.
///
/// Long enough that two checkouts on one machine do not collide, short enough
/// that the name stays readable in a listing beside its slot.
const REPOSITORY_KEY_LENGTH: usize = 16;

#[derive(Debug)]
pub(super) struct BuildRoot {
    pub(super) workspace: PathBuf,
    pub(super) root: PathBuf,
    possession: Option<Possession>,
    retained: bool,
}

impl BuildRoot {
    pub(super) fn create() -> Result<Self, TreeError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temporary = workspace_parent()?;
        reclaim_abandoned(&temporary);
        for _ in 0..128 {
            let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
            let workspace =
                temporary.join(format!("{WORKSPACE_PREFIX}{}-{nonce}", std::process::id()));
            let Some(possession) = possess_workspace(&workspace) else {
                continue;
            };
            match create_private_workspace(&workspace) {
                Ok(()) => {
                    let root = workspace.join("root");
                    let build = Self {
                        workspace,
                        root,
                        possession: Some(possession),
                        retained: false,
                    };
                    private_directory(&build.workspace)?;
                    fs::create_dir(&build.root).map_err(|source| {
                        TreeError::io("creating a selected tree root", &build.root, source)
                    })?;
                    private_directory(&build.root)?;
                    return Ok(build);
                }
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
                Err(source) => {
                    return Err(TreeError::io(
                        "creating a selected tree workspace",
                        &workspace,
                        source,
                    ));
                }
            }
        }
        Err(TreeError::io(
            "creating a selected tree workspace",
            &temporary,
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "selected tree name attempts exhausted",
            ),
        ))
    }

    /// Take the retained workspace this repository refills, or an ephemeral one.
    ///
    /// A retained workspace is possessed exactly as a minted one is, and for
    /// the same reason: the lock is what separates a copy a live run is
    /// measuring from one nobody owns. It differs only in what possession
    /// means afterwards -- the contents are kept for the next run to refill in
    /// place, so that unchanged files keep the modification times the current
    /// artifacts were built against.
    ///
    /// Every retained name belongs to one repository, so two checkouts never
    /// overwrite each other's copy, and the slot suffix admits exactly as many
    /// concurrent runs as [`RETAINED_WORKSPACES`]. Beyond that, and on any
    /// failure to prepare the copy, this falls back to [`Self::create`]: an
    /// ephemeral workspace is always correct and only ever slower.
    pub(super) fn retained(repository: &Path) -> Result<Self, TreeError> {
        let Some(owner) = repository_owner(repository) else {
            return Self::create();
        };
        let Some(key) = repository_key(&owner) else {
            return Self::create();
        };
        let temporary = workspace_parent()?;
        reclaim_abandoned(&temporary);
        for slot in 0..RETAINED_WORKSPACES {
            let workspace = temporary.join(format!("{RETAINED_PREFIX}{key}-{slot}"));
            let Ok(lock) = workspace_lock(&workspace) else {
                continue;
            };
            let Ok(Acquisition::Acquired(possession)) = lock.try_acquire() else {
                continue;
            };
            let build = Self {
                root: workspace.join("root"),
                workspace,
                possession: Some(possession),
                retained: true,
            };
            if prepare_retained(&build, &owner).is_ok() {
                return Ok(build);
            }
        }
        Self::create()
    }

    /// Hand the workspace, and the lock that guards it, to the selected tree.
    ///
    /// Taking the possession is what stops this build root's own [`Drop`] from
    /// removing a workspace the caller is about to be handed: a build root that
    /// has been finished no longer possesses anything to remove.
    pub(super) fn finish(
        &mut self,
        repository: &Path,
        source: SelectionSeal,
        files: SelectedFiles,
        renames: RenameMap,
        staged_authority: Option<StagedAuthority>,
        publishes_state: bool,
    ) -> SelectedTree {
        SelectedTree {
            workspace: self.workspace.clone(),
            root: self.root.clone(),
            repository: repository.to_path_buf(),
            source,
            files,
            renames,
            staged_authority,
            publishes_state,
            retained: self.retained,
            possession: self
                .possession
                .take()
                .map(|possession| super::consumer::ConsumerLease::new(&self.root, possession)),
        }
    }
}

/// Make one possessed retained workspace ready to be refilled in place.
///
/// What the previous run left under `root` is deliberately kept: it is the
/// only record of when each selected byte last changed. Everything else the
/// previous run left -- observation scratch, index copies -- is removed, so a
/// retained workspace offers a caller exactly what a freshly minted one does.
fn prepare_retained(build: &BuildRoot, owner: &str) -> Result<(), TreeError> {
    match create_private_workspace(&build.workspace) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(TreeError::io(
                "creating a retained selected tree workspace",
                &build.workspace,
                source,
            ));
        }
    }
    private_directory(&build.workspace)?;
    let listing = fs::read_dir(&build.workspace).map_err(|source| {
        TreeError::io(
            "reading a retained selected tree workspace",
            &build.workspace,
            source,
        )
    })?;
    for entry in listing.flatten() {
        if entry.file_name() == OsStr::new("root") {
            continue;
        }
        let path = entry.path();
        make_removable(&path);
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            let _ = fs::remove_dir_all(&path);
        } else {
            let _ = fs::remove_file(&path);
        }
    }
    if !fs::symlink_metadata(&build.root).is_ok_and(|metadata| metadata.is_dir()) {
        make_removable(&build.root);
        let _ = fs::remove_file(&build.root);
        fs::create_dir(&build.root).map_err(|source| {
            TreeError::io("creating a selected tree root", &build.root, source)
        })?;
    }
    private_directory(&build.root)?;
    fs::write(build.workspace.join(RETAINED_OWNER), owner).map_err(|source| {
        TreeError::io(
            "recording a retained workspace owner",
            &build.workspace,
            source,
        )
    })
}

/// The exact repository path one retained workspace serves.
///
/// `None` withholds retention rather than recording a path this build cannot
/// read back exactly. A repository whose canonical path is not UTF-8 keeps
/// the ephemeral behavior, which is correct and merely slower.
fn repository_owner(repository: &Path) -> Option<String> {
    fs::canonicalize(repository)
        .unwrap_or_else(|_| repository.to_path_buf())
        .into_os_string()
        .into_string()
        .ok()
}

/// Stable short name for the repository a retained workspace belongs to.
///
/// The repository path itself cannot be the name: it contains separators, has
/// no length bound, and would put a checkout's location in a world-listable
/// temporary directory. The digest is taken over the canonical path so two
/// names for one checkout resolve to one workspace.
fn repository_key(owner: &str) -> Option<String> {
    let digest = gix::objs::compute_hash(
        gix::hash::Kind::Sha1,
        gix::objs::Kind::Blob,
        owner.as_bytes(),
    )
    .ok()?
    .to_string();
    Some(digest[..REPOSITORY_KEY_LENGTH.min(digest.len())].to_owned())
}

/// Whether this path is a workspace retained for reuse between runs.
fn is_retained_workspace(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(RETAINED_PREFIX))
}

/// Whether the repository one retained workspace was filled for still exists.
///
/// An unreadable or absent owner record answers no: the copy is then owed to
/// nobody and the ordinary abandoned-workspace sweep may take it.
fn retained_owner_lives(workspace: &Path) -> bool {
    fs::read_to_string(workspace.join(RETAINED_OWNER)).is_ok_and(|owner| Path::new(&owner).is_dir())
}

/// Release one workspace at the end of the run that possessed it.
///
/// A retained workspace keeps its contents; only the lock is given up, which
/// happens when the possession drops. Every other workspace is removed, which
/// is the contract every gate before retention existed relied on.
pub(super) fn retire_workspace(workspace: &Path, possession: &Possession, retained: bool) {
    if retained {
        return;
    }
    let _ = release_workspace(workspace, possession);
}

pub(super) fn workspace_parent() -> Result<PathBuf, TreeError> {
    let temporary = std::env::temp_dir();
    #[cfg(target_family = "unix")]
    let owner = {
        // SAFETY: geteuid has no preconditions and observes this process only.
        unsafe { libc::geteuid() }
    };
    #[cfg(not(target_family = "unix"))]
    let owner = std::process::id();
    let parent = temporary.join(format!("{WORKSPACE_PARENT_PREFIX}{owner}"));
    let created = match create_private_workspace(&parent) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
        Err(source) => {
            return Err(TreeError::io(
                "creating the selected-tree workspace parent",
                &parent,
                source,
            ));
        }
    };
    validate_workspace_parent(&parent, owner)?;
    if created {
        reclaim_abandoned(&temporary);
    }
    Ok(parent)
}

fn validate_workspace_parent(path: &Path, owner: u32) -> Result<(), TreeError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| {
        TreeError::io(
            "inspecting the selected-tree workspace parent",
            path,
            source,
        )
    })?;
    #[cfg(target_family = "unix")]
    let valid = metadata.is_dir() && metadata.uid() == owner && metadata.mode() & 0o777 == 0o700;
    #[cfg(not(target_family = "unix"))]
    let valid = {
        let _ = owner;
        metadata.is_dir()
    };
    if valid {
        return Ok(());
    }
    Err(TreeError::io(
        "validating the selected-tree workspace parent",
        path,
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "workspace parent must be an owner-private real directory",
        ),
    ))
}

/// Take one workspace name's lock, and clear anything a dead run left under it.
///
/// `None` means "mint another name": either a live run holds this one, or what
/// it left behind could not be removed. Both are answered by moving to the next
/// nonce, which is what this loop did for a colliding name before any lock
/// existed.
fn possess_workspace(workspace: &Path) -> Option<Possession> {
    let lock = workspace_lock(workspace).ok()?;
    let Ok(Acquisition::Acquired(possession)) = lock.try_acquire() else {
        return None;
    };
    match reclaim_workspace(workspace, &possession) {
        HeldRootRemoval::Removed => Some(possession),
        HeldRootRemoval::Failed(_) => None,
    }
}

/// The lock guarding one gate tree workspace, as every reclaim resolves it.
pub(super) fn workspace_lock(workspace: &Path) -> Result<LockFile, String> {
    held_root::held_root_lock(workspace, WORKSPACE_LOCK, WORKSPACE_LABEL)
}

/// Remove every gate tree workspace no live run holds.
///
/// A workspace name carries the identifier of the process that minted it, so no
/// other *live* run ever resolves the same one, and a run that unwinds removes
/// its own. A dead run's identifier is eventually reissued, which is why
/// [`possess_workspace`] reclaims a minted name before building under it rather
/// than trusting that a fresh nonce means a fresh directory.
///
/// A run that is killed unwinds nothing, and what it leaves is a directory
/// indistinguishable, by looking at it, from one a live gate is measuring out of
/// right now. The lock beside each workspace is what separates them: a lock no
/// one holds is a workspace whose owner is dead.
///
/// Only a workspace with a lock file already beside it is removed, and that
/// restriction is load bearing rather than an optimization. A gate built before
/// this mechanism minted workspaces under the same prefix and took no lock at
/// all, so an unlocked directory is not evidence of a dead owner; on a machine
/// where several checkouts share one temporary directory it is most likely a
/// live run of an older build, and removing it would delete the tree that run
/// is measuring. Those are left for an operator, and every workspace this build
/// mints is reclaimable because it is locked before it exists.
///
/// A lock whose workspace is absent is considered separately. That shape can
/// be residue from a run killed after removing its workspace, but it is also the
/// shape of a live run between locking a fresh name and creating its directory.
/// Ephemeral locks are unlinked only while this sweep possesses them and a
/// second absence check proves that no directory appeared during acquisition.
/// Retained-slot locks keep their inode even without a workspace: the reused
/// name must exclude openers that have not yet acquired kernel possession.
///
/// A retained workspace is the one shape an unheld lock does not condemn. It
/// is kept on purpose, between runs, for the repository named in its owner
/// record, and removing it would only cost the next run of that repository a
/// full recompile. It is swept exactly when that repository is gone, which is
/// also when nothing will ever refill it.
pub(super) fn reclaim_abandoned(temporary: &Path) {
    let Ok(listing) = fs::read_dir(temporary) else {
        return;
    };
    let candidates: Vec<PathBuf> = listing.flatten().map(|entry| entry.path()).collect();
    for workspace in candidates
        .iter()
        .filter(|path| is_workspace_directory(path))
    {
        let Ok(lock) = workspace_lock(workspace) else {
            continue;
        };
        if is_retained_workspace(workspace) && retained_owner_lives(workspace) {
            continue;
        }
        // Acquiring creates the lock file, so a workspace that never had one is
        // asked about rather than locked into existence.
        if !lock.path().try_exists().unwrap_or(false) {
            continue;
        }
        if let Ok(Acquisition::Acquired(possession)) = lock.try_acquire() {
            let _ = release_workspace(workspace, &possession);
        }
    }
    for lock_path in candidates {
        reclaim_parentless_lock(&lock_path);
    }
}

/// Remove one gate tree lock that has no workspace and no live holder.
fn reclaim_parentless_lock(lock_path: &Path) {
    let Some(workspace) = workspace_named_by_lock(lock_path) else {
        return;
    };
    // Retained slots are reused names. Unlinking their lock could strand an
    // opener on the old inode and admit a second owner on its replacement.
    if is_retained_workspace(&workspace) {
        return;
    }
    if !matches!(workspace.try_exists(), Ok(false)) || !is_workspace_lock_file(lock_path) {
        return;
    }
    let Ok(lock) = workspace_lock(&workspace) else {
        return;
    };
    if lock.path() != lock_path {
        return;
    }
    let Ok(Acquisition::Acquired(possession)) = lock.try_acquire() else {
        return;
    };
    if matches!(workspace.try_exists(), Ok(false)) {
        let _ = fs::remove_file(possession.lock().path());
    }
}

/// Resolve the workspace named by one regular gate tree lock candidate.
fn workspace_named_by_lock(lock_path: &Path) -> Option<PathBuf> {
    let name = lock_path.file_name()?.to_str()?;
    let workspace = name.strip_suffix(".lock")?;
    (workspace.starts_with(WORKSPACE_PREFIX) || workspace.starts_with(RETAINED_PREFIX))
        .then(|| lock_path.with_file_name(workspace))
}

/// Whether this path is a regular gate tree lock, without following a link.
fn is_workspace_lock_file(path: &Path) -> bool {
    workspace_named_by_lock(path).is_some()
        && fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// Whether this path is a directory named as a gate tree workspace.
///
/// Both families count: the ephemeral one a run mints and removes, and the
/// retained one a repository refills, which this build still sweeps once its
/// repository is gone.
///
/// The lock file beside a workspace shares its prefix, so the kind is checked
/// rather than assumed, and it is checked without following a symlink: a link
/// planted under this name would otherwise be dereferenced by the very call
/// deciding whether to remove it.
fn is_workspace_directory(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    (name.starts_with(WORKSPACE_PREFIX) || name.starts_with(RETAINED_PREFIX))
        && fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

/// Remove one workspace whose lock this caller possesses.
///
/// A materialized tree is sealed read-only, and a sealed directory cannot be
/// read in order to remove what is under it, so the seal is lifted first.
/// Lifting it is safe only because the possession already proves that no live
/// run is measuring this tree.
fn reclaim_workspace(workspace: &Path, possession: &Possession) -> HeldRootRemoval {
    make_removable(workspace);
    held_root::remove_held_root(workspace, possession)
}

/// Remove one workspace and then the lock that guarded it.
///
/// The lock is kept beside the workspace rather than inside it so that removing
/// the workspace cannot remove the evidence that it was held. That evidence has
/// nothing left to describe once the workspace is gone, and a workspace name
/// carries a fresh nonce per run, so a lock retained for every run ever made
/// would accumulate exactly what this reclaim exists to prevent.
/// Retained slot names are reused rather than nonce-minted, so their lock
/// inodes remain stable even after their workspace is reclaimed.
///
/// The order is what keeps that safe. The lock is unlinked last, and only on a
/// removal that succeeded, so this never leaves a workspace standing with no
/// lock beside it -- which is the one state [`reclaim_abandoned`] declines to
/// act on, and so the one state a workspace could never be reclaimed from.
pub(super) fn release_workspace(workspace: &Path, possession: &Possession) -> HeldRootRemoval {
    let removal = reclaim_workspace(workspace, possession);
    if matches!(removal, HeldRootRemoval::Removed) && !is_retained_workspace(workspace) {
        let _ = fs::remove_file(possession.lock().path());
    }
    removal
}

impl Drop for BuildRoot {
    fn drop(&mut self) {
        if let Some(possession) = self.possession.take() {
            retire_workspace(&self.workspace, &possession, self.retained);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn retained_slot_openers_keep_one_exclusive_lock_across_reclamation() {
        let fixture = crate::test_repository::Fixture::repository("retained-slot-ownership");
        let workspace = fixture.join(format!("{RETAINED_PREFIX}slot-0"));
        let lock = workspace_lock(&workspace).unwrap();
        let descriptor = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock.path())
            .unwrap();
        reclaim_parentless_lock(lock.path());
        let Acquisition::Acquired(possession) = lock.try_acquire_opened(descriptor).unwrap() else {
            panic!("first opener must acquire");
        };
        assert!(matches!(
            lock.try_acquire().unwrap(),
            Acquisition::Contended
        ));
        drop(possession);
        assert!(matches!(
            lock.try_acquire().unwrap(),
            Acquisition::Acquired(_)
        ));
    }
}
