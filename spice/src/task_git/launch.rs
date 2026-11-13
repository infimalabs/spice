//! GIT-04, the launch boundary: advance when it is safe, and never raise.
//!
//! The lenient sibling of [`super::prepare_claim`]. Both apply the same three
//! rules -- a clean tree, no commits ahead, fast-forward only -- but where the
//! claim boundary refuses and stops the caller, this one declines and says why.
//! A lane always starts, and its tree is never mangled or reset to get it
//! there.
//!
//! Every outcome is a note, including the ones that did nothing. Declining to
//! advance is a decision, so it is named rather than left as an anonymous
//! no-op: why a lane is still on an old commit should be answerable from what
//! this boundary returned instead of inferred from where the lane sits. No
//! The resident publishes the exact note in the launch transition and records
//! it with the durable session identity, so a lane that stayed on an old commit
//! says why both immediately and on later reads.

use std::fmt;

use super::{
    Error, Worktree, ahead_count, dirty_paths, fetch, is_ancestor, materialize_and_update,
    recover_pending, required_read, resolve_target,
};

/// What the launch boundary did to the lane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LaunchAdvance {
    /// The lane already sat on the baseline.
    Current,
    /// The lane fast-forwarded from one commit to another.
    Updated { before: String, after: String },
    /// A safe no-op, named so that not advancing stays visible.
    Skipped(LaunchSkip),
}

/// Why a launch advance safely declined.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaunchSkip {
    /// No integration branch to advance towards: the lane tracks no remote.
    NoRemote,
    /// Uncommitted work in the tree, which is the lane's and is kept.
    Dirty,
    /// The remote could not be reached. Distinct from having no remote.
    FetchFailed,
    /// The baseline could not be read, so safety could not be established.
    BaselineUninspectable,
    /// The lane is ahead of the baseline only, so there is nothing to take.
    Ahead,
    /// The lane and the baseline have both moved, so no fast-forward exists.
    Diverged,
}

impl fmt::Display for LaunchAdvance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Current => formatter.write_str("current"),
            Self::Updated { .. } => {
                formatter.write_str("updated working tree to the current baseline")
            }
            Self::Skipped(skip) => write!(formatter, "skipped:{skip}"),
        }
    }
}

impl fmt::Display for LaunchSkip {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NoRemote => "no-remote",
            Self::Dirty => "dirty",
            Self::FetchFailed => "fetch-failed",
            Self::BaselineUninspectable => "baseline-uninspectable",
            Self::Ahead => "ahead",
            Self::Diverged => "diverged",
        })
    }
}

/// Advance the lane if every rule allows it, reporting what happened.
pub(super) fn advance(root: Worktree<'_>) -> LaunchAdvance {
    match attempt(root) {
        Ok(advance) => advance,
        Err(skip) => LaunchAdvance::Skipped(skip),
    }
}

/// The rules in order, each failure carrying the note it becomes.
///
/// Written with `?` so that the sequence reads as the predecessor documents it:
/// resolve, then clean, then fetch, then inspect, then advance. Every step that
/// can fail names the note it fails into at the call site, so no reader has to
/// hold the mapping in their head.
fn attempt(root: Worktree<'_>) -> Result<LaunchAdvance, LaunchSkip> {
    recover_pending(root).map_err(uninspectable)?;
    let target = resolve_target(root)
        .map_err(uninspectable)?
        .ok_or(LaunchSkip::NoRemote)?;
    if !dirty_paths(root).map_err(uninspectable)?.is_empty() {
        return Err(LaunchSkip::Dirty);
    }
    fetch(root, &target).map_err(|_| LaunchSkip::FetchFailed)?;
    let baseline = read_commit(root, &target.baseline)?;
    let before = read_commit(root, "HEAD")?;
    if ahead_count(root, &target.baseline).map_err(uninspectable)? != 0 {
        return Err(ahead_or_diverged(root, &baseline));
    }
    if before == baseline {
        return Ok(LaunchAdvance::Current);
    }
    if !is_ancestor(root, &before, &baseline).map_err(uninspectable)? {
        return Err(LaunchSkip::Diverged);
    }
    materialize_and_update(root, &baseline, &before, "advance lane at agent launch")
        .map_err(|_| LaunchSkip::Diverged)?;
    Ok(LaunchAdvance::Updated {
        before,
        after: baseline,
    })
}

/// Separate a lane that is merely ahead from one that has also fallen behind.
///
/// Both have commits the baseline lacks, and neither can fast-forward, but they
/// are different situations for the operator: one lane has work to publish and
/// the other has work to publish *and* work to integrate first.
fn ahead_or_diverged(root: Worktree<'_>, baseline: &str) -> LaunchSkip {
    match is_ancestor(root, baseline, "HEAD") {
        Ok(true) => LaunchSkip::Ahead,
        Ok(false) => LaunchSkip::Diverged,
        Err(_) => LaunchSkip::BaselineUninspectable,
    }
}

fn read_commit(root: Worktree<'_>, revision: &str) -> Result<String, LaunchSkip> {
    required_read(root, &["rev-parse", revision], "read commit").map_err(uninspectable)
}

/// Every inspection failure becomes the same note, deliberately.
///
/// The predecessor lets a failed worktree status escape as an exception, which
/// is not available here: GIT-04 says never raise, and a fallible signature
/// would invite a caller to `?` it and turn a declined advance into a failed
/// launch. An inspection that cannot answer is reported as the baseline being
/// uninspectable, which is the note the predecessor already uses for every
/// other inspection that cannot answer.
fn uninspectable(_: Error) -> LaunchSkip {
    LaunchSkip::BaselineUninspectable
}
