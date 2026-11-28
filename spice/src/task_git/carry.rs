//! A lane's carried work: its unpublished commits and a dirty-worktree snapshot.
//!
//! Collection only reads the lane. Release then returns the lane to published
//! history; an involuntary release copies the same set and leaves the lane as
//! its holder left it.

use super::*;

/// The work one lane holds beyond published history.
pub(super) struct Collected {
    pub(super) parked: wire::body::TaskParkedCommits,
    pub(super) snapshot: Option<snapshot::Captured>,
}

/// Read a lane's unpublished commits and snapshot its dirty tree.
///
/// The snapshot is built through a scratch index, so the lane's HEAD, index,
/// and worktree stay byte-identical. Open merges and paused replays refuse.
pub(super) fn collect(
    root: Worktree<'_>,
    opening: Option<&GitBoundary>,
) -> Result<Option<Collected>, Error> {
    snapshot::require_releasable(root)?;
    let head = required_read(root, &["rev-parse", "HEAD"], "read lane HEAD")?;
    let snapshot = snapshot::capture(root, &head)?;
    let published = match published_reference(root, opening)? {
        Some(published) => published,
        None if snapshot.is_some() => head.clone(),
        None => return Ok(None),
    };
    let commits = rev_list(root, &published, &head)?;
    if commits.is_empty() && snapshot.is_none() {
        return Ok(None);
    }
    let baseline = required_read(
        root,
        &["merge-base", &published, &head],
        "find the newest published lane commit",
    )?;
    Ok(Some(Collected {
        parked: wire::body::TaskParkedCommits {
            baseline,
            head,
            commits,
            snapshot: snapshot
                .as_ref()
                .map(|snapshot| snapshot.commit().to_owned()),
        },
        snapshot,
    }))
}

/// Copy another holder's unpublished commits and dirty work without touching
/// its lane, for a lapse or steal that releases the claim involuntarily.
///
/// A lane mid-transaction refuses rather than being recovered from outside.
pub fn copy_parked(
    repository: &Repository,
    ambient: &Ambient,
    opening: Option<&GitBoundary>,
) -> Result<Option<wire::body::TaskParkedCommits>, Error> {
    let root = Worktree::new(repository.worktree_root(), ambient);
    if transaction_path(root)?.exists() {
        return Err(Error::refusal(
            "git::transaction-pending",
            "the holder lane is inside an unfinished task Git transaction",
            "the holder lane recovers it on its next task command",
        ));
    }
    Ok(collect(root, opening)?.map(|collected| collected.parked))
}
