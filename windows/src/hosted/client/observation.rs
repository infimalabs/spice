//! Bounded provider observation recovery.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use super::{Cancellation, WindowsHostedError, cancel_run};

/// Watcher restarts admitted while the provider reports the run unfinished.
pub(super) const WATCH_RESUMES: u32 = 16;

/// Seconds between watcher polls and observation-gap retries. Every poll
/// spends shared provider API quota, and exhausting it ends every watcher on
/// the account at once.
pub(super) const WATCH_INTERVAL_SECONDS: u64 = 30;

/// Whether one missing provider observation remains inside the retry bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ObservationGap {
    Wait,
    Detach,
}

pub(super) fn observation_gap_action(resumes: u32) -> ObservationGap {
    if resumes < WATCH_RESUMES {
        ObservationGap::Wait
    } else {
        ObservationGap::Detach
    }
}

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
        _ => match observation_gap_action(resumes) {
            ObservationGap::Wait => Resume::Watch,
            ObservationGap::Detach => Resume::Detach,
        },
    }
}

/// Wait before retrying a provider observation gap. An explicit cancellation
/// still cancels the provider run instead of detaching it.
pub(super) fn await_watch_interval(
    root: &Path,
    repository: &str,
    run_id: u64,
    cancellation: &Cancellation,
) -> Result<(), WindowsHostedError> {
    let (events, receiver) = mpsc::channel();
    cancellation.wake(events.clone());
    // Held so only a cancellation or the retry deadline ends the wait.
    let _held = events;
    match receiver.recv_timeout(Duration::from_secs(WATCH_INTERVAL_SECONDS)) {
        Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
        Ok(_) | Err(mpsc::RecvTimeoutError::Disconnected) => {
            cancel_run(root, repository, run_id)?;
            Err(WindowsHostedError::Cancelled {
                run_id: Some(run_id),
            })
        }
    }
}
