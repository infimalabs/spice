//! A directory one run owns for its own use, and the lock that says who owns it.
//!
//! Two surfaces in this crate materialize a scratch directory and hold it for
//! the length of a run: the compiling analysis arms, which resolve a fixed
//! sibling of the selected tree, and gate tree selection, which mints a private
//! workspace below the temporary directory. Both meet the same problem. A
//! directory left behind by a run that was killed before its own teardown is
//! indistinguishable, by looking at it, from a directory a live run is writing
//! into right now. A caller that could see only the directory has to treat both
//! alike -- reclaim both and it deletes live work, refuse both and every killed
//! run leaves wreckage an operator must clear by hand.
//!
//! What tells them apart is an advisory lock outside the held directory, held
//! for the life of that directory, and the invariant is that **a caller removes
//! a held root only while it possesses that root's lock**. [`remove_held_root`]
//! takes a [`Possession`] rather than a path alone, so the invariant is carried
//! by the signature and a removal cannot be spelled without the lock in hand,
//! and [`hold`] is one way to obtain one. Contention is therefore not a removal
//! that gets refused; it is a removal that never becomes reachable, because a
//! contended caller returns before it owns anything to tear down.
//!
//! `flock` grants per-open-file-description, so a second acquisition contends
//! even from inside this process; that is what lets a fixture hold a root and
//! drive the decision directly rather than having to lose a race to observe it.
//!
//! A scratch root uses a sibling lock; an appliance-state cache supplies its
//! canonical `runtime/locks` path. Either stays outside the removable root.
//! Each caller passes its own label and lock name, so every refusal names the
//! surface the operator ran.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sill::{Acquisition, LockFile, LockName, Possession};

/// What one attempt to remove a held root did, so no attempt is discarded.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum HeldRootRemoval {
    /// The root is gone, whether this attempt unlinked it or found it absent.
    Removed,
    /// The root still exists and the reason is carried rather than dropped.
    Failed(String),
}

impl HeldRootRemoval {
    /// The operator-facing reason, or `None` when there is nothing to report.
    ///
    /// A root that cannot be removed refuses every later run that resolves it,
    /// because [`hold`] reclaims before it hands back a possession and fails
    /// with this same text. The report when the removal first fails and the
    /// refusal an operator eventually meets name the same path on purpose: one
    /// is the warning, the other is the consequence.
    pub(crate) fn reason(&self, label: &str, root: &Path) -> Option<String> {
        match self {
            Self::Removed => None,
            Self::Failed(error) => Some(format!(
                "{label} {} could not be removed: {error}; every later analysis of this tree \
                 refuses until it is gone",
                root.display()
            )),
        }
    }
}

/// Remove a held root whose lock this caller already possesses.
///
/// Taking [`Possession`] rather than a path alone is the guard: there is no way
/// to spell a removal without first holding the lock, so a root a live run holds
/// is unreachable here rather than merely unlucky. Absence counts as removed;
/// every other refusal is carried back out.
pub(crate) fn remove_held_root(root: &Path, _possession: &Possession) -> HeldRootRemoval {
    match fs::remove_dir_all(root) {
        Ok(()) => HeldRootRemoval::Removed,
        Err(error) if error.kind() == io::ErrorKind::NotFound => HeldRootRemoval::Removed,
        Err(error) => HeldRootRemoval::Failed(error.to_string()),
    }
}

/// The lock guarding one held root, kept outside the root so removing the root
/// cannot remove the evidence that it was held.
pub(crate) fn held_root_lock_at(
    path: PathBuf,
    lock: &str,
    label: &str,
) -> Result<LockFile, String> {
    let Some(parent) = path.parent() else {
        return Err(format!("{label} lock {} has no parent", path.display()));
    };
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "{label} lock parent {} could not be created: {error}",
            parent.display()
        )
    })?;
    let name =
        LockName::new(lock).map_err(|error| format!("{label} lock name was refused: {error}"))?;
    Ok(LockFile::new(name, path))
}

/// The sibling lock guarding one held root outside the appliance state root.
pub(crate) fn held_root_lock(root: &Path, lock: &str, label: &str) -> Result<LockFile, String> {
    let Some(name) = root.file_name() else {
        return Err(format!("{label} {} has no name to lock", root.display()));
    };
    let mut lock_name = name.to_os_string();
    lock_name.push(".lock");
    held_root_lock_at(root.with_file_name(lock_name), lock, label)
}

/// Take this root's lock, and reclaim whatever a killed run left in it.
///
/// The returned [`Possession`] is the caller's proof that no live run holds this
/// root, which is what makes reclaiming safe: a root whose lock is free was left
/// by a run that died before its own teardown. A contended lock is a live run,
/// and this returns before the caller owns anything to tear down.
pub(crate) fn hold(root: &Path, lock: &str, label: &str) -> Result<Possession, String> {
    let lock = held_root_lock(root, lock, label)?;
    let possession = match lock.try_acquire() {
        Ok(Acquisition::Acquired(possession)) => possession,
        Ok(Acquisition::Contended) => {
            return Err(format!(
                "{label} {} is in use by another analysis of this tree",
                root.display()
            ));
        }
        Err(error) => {
            return Err(format!(
                "{label} {} could not be locked: {error}",
                root.display()
            ));
        }
    };
    match remove_held_root(root, &possession).reason(label, root) {
        Some(reason) => Err(reason),
        None => Ok(possession),
    }
}
