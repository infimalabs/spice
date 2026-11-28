//! Work an involuntarily released holder leaves in its lane, copied for the
//! task's next claim without touching that lane.

use super::*;

/// Copy the holder lane's unpublished commits and dirty work.
///
/// `Err` names why nothing could be carried; the claim moves regardless.
pub(crate) fn copy_holder_work(
    host: Host<'_>,
    worktree: &str,
    opening: Option<&GitBoundary>,
) -> Result<Option<body::TaskParkedCommits>, String> {
    let lane = automatic_lifecycle::lane_repository(host, worktree).map_err(|_| {
        format!(
            "the holder lane {worktree} no longer names one worktree in the resident repository"
        )
    })?;
    spice::task_git::copy_parked(&lane, host.ambient, opening)
        .map_err(|error| format!("{}: {}", error.reason(), error.detail()))
}
