//! Bounded provider watcher recovery.

/// Watcher restarts admitted while the provider reports the run unfinished.
pub(super) const WATCH_RESUMES: u32 = 16;

/// Seconds between watcher polls. Every poll spends shared provider API quota,
/// and exhausting it ends every watcher on the account at once.
pub(super) const WATCH_INTERVAL_SECONDS: u64 = 30;

/// What the provider's run state asks of a client whose watcher exited
/// unsuccessfully.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Resume {
    Watch,
    Complete,
    Failed,
    Detach,
}

pub(super) fn resume_action(status: &str, conclusion: &str, resumes: u32) -> Resume {
    match (status, conclusion) {
        ("completed", "success") => Resume::Complete,
        ("completed", _) => Resume::Failed,
        _ if resumes < WATCH_RESUMES => Resume::Watch,
        _ => Resume::Detach,
    }
}
