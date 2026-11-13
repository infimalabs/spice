//! The one name of the Rust state root, and the one way to find it.
//!
//! The Git-common directory carries repository authority below this root, and
//! each linked worktree's own Git directory carries its worktree-local state
//! below a root of the same name. Every child keeps its relative name; nothing
//! outside this module spells the root. No other root name is recognized: a
//! directory by any other name is foreign state nothing here reads.

#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};

/// Name of the state root this runtime owns beneath a Git directory.
pub const STATE_DIRECTORY: &str = ".spice";

/// The state root below `git_directory`. Shape is not judged here: the
/// admission that opens the root refuses a link or a file at that name.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn state_root(git_directory: &Path) -> PathBuf {
    git_directory.join(STATE_DIRECTORY)
}

/// The directory below `state_root` holding every lock file the appliance
/// takes. Each lock is one named file here, never beside the state it guards.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn state_locks(state_root: &Path) -> PathBuf {
    state_root.join("runtime").join("locks")
}
