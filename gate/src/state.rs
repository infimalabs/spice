//! Elastic bounds and durable gate state.
//!
//! A run loads one logical ledger, computes all latch and claim changes in
//! memory, and publishes them only through [`PendingState::accept`]. The
//! current ledger lives below Git's common directory, so linked worktrees see
//! one authority. When that ledger is absent, a run starts from empty current
//! state.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sill::{LockFile, LockName};
use wire::{Digest, Domain, TaskHandle};

/// Current ledger schema.
pub const STATE_LEDGER_VERSION: u64 = 1;

/// Current immutable state-transition receipt schema.
pub const STATE_RECEIPT_VERSION: u64 = 1;
/// Lifetime of one peer flex claim.
pub const PEER_CLAIM_TTL_SECONDS: u64 = 6 * 60 * 60;

const CURRENT_STATE_RELEASE: &str = "current";
const CURRENT_STATE_AUTHORITY_DIRECTORY: &str = "authority";
const CURRENT_STATE_GATE_DIRECTORY: &str = "gate";
const CURRENT_STATE_FILE: &str = "state.json";
const CURRENT_STATE_LOCK_FILE: &str = "gate-state";

/// The current gate-state path below one canonical Git common directory.
#[must_use]
pub fn current_state_path(common_git_directory: &Path) -> PathBuf {
    wire::state_root(common_git_directory)
        .join(CURRENT_STATE_AUTHORITY_DIRECTORY)
        .join(CURRENT_STATE_GATE_DIRECTORY)
        .join(CURRENT_STATE_FILE)
}

/// The current gate-state publication lock below one canonical Git common directory.
#[must_use]
pub fn current_state_lock_path(common_git_directory: &Path) -> PathBuf {
    wire::state_locks(&wire::state_root(common_git_directory)).join(CURRENT_STATE_LOCK_FILE)
}

mod bound;
pub use bound::*;
mod ledger;
pub use ledger::*;
mod transaction;
pub use transaction::*;
mod migration;
pub use migration::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn an_accepted_run_uses_the_canonical_authority_and_runtime_lock() {
        let scratch = std::env::temp_dir().join(format!(
            "spice-gate-state-layout-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let common = scratch.join(".git");
        fs::create_dir_all(&common).expect("create fixture common Git directory");
        let destination = current_state_path(&common);
        let lock = current_state_lock_path(&common);
        let mut current =
            CurrentState::load(&destination, &lock).expect("load absent canonical state");
        current
            .begin_run(&RenameMap::default(), std::iter::empty::<&str>(), 1.0, None)
            .expect("begin empty state run");
        current
            .reconcile(std::iter::empty::<LatchObservation>())
            .expect("reconcile empty state run");
        let receipt = current
            .finish()
            .expect("finish empty state run")
            .accept()
            .expect("publish empty state run");

        assert!(receipt.accepted());
        assert_eq!(destination, common.join(".spice/authority/gate/state.json"));
        assert_eq!(lock, common.join(".spice/runtime/locks/gate-state"));
        assert!(destination.is_file());
        assert!(lock.is_file());
        assert!(!common.join(".spice/data").exists());
        assert!(!common.join(".spice/authority/gate/gate-state").exists());
        fs::remove_dir_all(scratch).expect("remove gate-state fixture");
    }
}
