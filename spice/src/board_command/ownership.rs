//! Policy ownership shared by reads, automatic claims, and resident wakeups.

use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

use super::drive::PolicyCapture;
use super::*;

struct Cached {
    repository: PathBuf,
    config: spice::config::CaptureGeneration,
    residency: Seq,
    cursor: Seq,
    owners: Arc<board::OwnershipSnapshot>,
}

/// Topic-head updates invalidate path-sensitive matches without changing node IDs.
pub(crate) fn capture(
    repository: &Repository,
    observed: &bus::TaskSnapshot,
) -> Result<(PolicyCapture, Arc<board::OwnershipSnapshot>), Outcome<CommandAnswer>> {
    let cursor = observed.cursor().ok_or_else(|| {
        store_fault(
            "board::cursor-missing",
            "policy ownership requires a durable snapshot",
        )
    })?;
    let residency = observed.residency().ok_or_else(|| {
        store_fault(
            "board::residency-missing",
            "policy ownership requires the current residency",
        )
    })?;
    capture_at(repository, residency.body(), residency.position(), cursor)
}

pub(crate) fn capture_required(
    repository: Option<&Repository>,
    observed: &bus::TaskSnapshot,
) -> Result<(PolicyCapture, Arc<board::OwnershipSnapshot>), Outcome<CommandAnswer>> {
    let cursor = observed.cursor().ok_or_else(|| {
        store_fault(
            "board::cursor-missing",
            "policy ownership requires a durable snapshot",
        )
    })?;
    let residency = observed.residency().ok_or_else(|| {
        store_fault(
            "board::residency-missing",
            "policy ownership requires current residency",
        )
    })?;
    let policy = PolicyCapture::required(repository, residency.body().epoch)?;
    capture_policy(policy, residency.body(), residency.position(), cursor)
}

#[cfg(test)]
pub(crate) fn capture_answer(
    repository: &Repository,
    answer: &BoardAnswer,
) -> Result<(PolicyCapture, Arc<board::OwnershipSnapshot>), Outcome<CommandAnswer>> {
    capture_at(
        repository,
        &answer.residency.body,
        answer.residency.position,
        answer.cursor,
    )
}

fn capture_at(
    repository: &Repository,
    residency: &wire::ResidencyHead,
    position: Seq,
    cursor: Seq,
) -> Result<(PolicyCapture, Arc<board::OwnershipSnapshot>), Outcome<CommandAnswer>> {
    capture_policy(
        PolicyCapture::capture(repository)?,
        residency,
        position,
        cursor,
    )
}

fn capture_policy(
    policy: PolicyCapture,
    residency: &wire::ResidencyHead,
    position: Seq,
    cursor: Seq,
) -> Result<(PolicyCapture, Arc<board::OwnershipSnapshot>), Outcome<CommandAnswer>> {
    static CACHE: LazyLock<Mutex<Option<Cached>>> = LazyLock::new(|| Mutex::new(None));
    let mut cache = CACHE.lock().expect("ownership snapshot cache poisoned");
    let current = cache.as_ref().filter(|current| {
        current.repository == policy.repository().worktree_root()
            && current.config == *policy.generation()
            && current.residency == position
            && current.cursor == cursor
    });
    if let Some(current) = current {
        return Ok((policy, Arc::clone(&current.owners)));
    }
    let owners = Arc::new(board::OwnershipSnapshot::from_residency(
        policy.config().clone(),
        residency,
    ));
    *cache = Some(Cached {
        repository: policy.repository().worktree_root().to_path_buf(),
        config: policy.generation().clone(),
        residency: position,
        cursor,
        owners: Arc::clone(&owners),
    });
    Ok((policy, owners))
}
