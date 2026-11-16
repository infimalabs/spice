//! The provider's shared REST quota as an observation boundary.
//!
//! Every client on the account spends one hourly REST quota. Once it is spent,
//! no observation can succeed until it resets, but the run keeps executing; the
//! client waits for the reset rather than reading the outage as the run's.

use std::ffi::OsString;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use super::{Cancellation, WindowsHostedError, cancel_run, command, successful_text};

/// The provider's quota window; no reset lies further ahead.
const QUOTA_WINDOW_SECONDS: u64 = 3600;
/// Slack past the announced reset, so the first observation after it lands
/// in the new window.
const RESET_MARGIN_SECONDS: u64 = 5;

#[derive(Deserialize)]
struct RateLimit {
    resources: Resources,
}

#[derive(Deserialize)]
struct Resources {
    core: Quota,
}

#[derive(Deserialize)]
struct Quota {
    remaining: u64,
    reset: u64,
}

/// The Unix second the REST quota resets, only while it is spent. The quota
/// report itself costs no quota.
pub(super) fn spent_until(root: &Path) -> Option<u64> {
    let text = successful_text(
        "provider quota",
        command(
            root,
            "gh",
            [OsString::from("api"), OsString::from("rate_limit")],
        )
        .ok()?,
    )
    .ok()?;
    let limit: RateLimit = serde_json::from_str(&text).ok()?;
    (limit.resources.core.remaining == 0).then_some(limit.resources.core.reset)
}

/// How long to wait at `now` for a quota resetting at `reset`, both Unix
/// seconds: past the reset, never beyond one quota window.
fn reset_wait(reset: u64, now: u64) -> Duration {
    Duration::from_secs(reset.saturating_sub(now).min(QUOTA_WINDOW_SECONDS) + RESET_MARGIN_SECONDS)
}

/// Wait out a spent quota. An explicit cancellation still cancels the run.
pub(super) fn await_reset(
    root: &Path,
    repository: &str,
    run_id: u64,
    reset: u64,
    cancellation: &Cancellation,
) -> Result<(), WindowsHostedError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| WindowsHostedError::Clock(error.to_string()))?
        .as_secs();
    let (events, receiver) = mpsc::channel();
    cancellation.wake(events.clone());
    // Held so only a cancellation or the deadline ends the wait.
    let _held = events;
    match receiver.recv_timeout(reset_wait(reset, now)) {
        Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
        Ok(_) | Err(mpsc::RecvTimeoutError::Disconnected) => {
            cancel_run(root, repository, run_id)?;
            Err(WindowsHostedError::Cancelled {
                run_id: Some(run_id),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{QUOTA_WINDOW_SECONDS, RESET_MARGIN_SECONDS, reset_wait};

    #[test]
    fn quota_waits_end_past_the_reset_within_one_window() {
        let margin = Duration::from_secs(RESET_MARGIN_SECONDS);
        assert_eq!(reset_wait(1_000, 1_000), margin);
        assert_eq!(reset_wait(1_000, 2_000), margin);
        assert_eq!(reset_wait(1_480, 1_000), Duration::from_secs(480) + margin);
        assert_eq!(
            reset_wait(u64::MAX, 0),
            Duration::from_secs(QUOTA_WINDOW_SECONDS) + margin
        );
    }
}
