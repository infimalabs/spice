//! The relaunch loop the installed lifecycle owner runs, and its pacing.
//!
//! The loop is written against two seams so it can be exercised in process:
//! the native manager that says whether the owner is still wanted, and an
//! attempt that starts one resident and later reports how it ended.

use std::io::{self, Write};
use std::time::Duration;

use super::manager::Manager;

/// A resident that served at least this long had a real run; the failure
/// count that paces relaunches starts over after it.
pub(crate) const STABLE_RUN: Duration = Duration::from_secs(60);
/// Pause before relaunching a resident whose last run was stable.
pub(crate) const RELAUNCH_PAUSE: Duration = Duration::from_secs(1);
/// Longest pause between attempts while residents keep failing early.
pub(crate) const MAX_PAUSE: Duration = Duration::from_secs(60);

/// How one launch attempt turned out.
pub(crate) enum Attempt {
    /// The resident published discovery; `wait` blocks until it exits.
    Ready {
        endpoint: String,
        wait: Box<dyn FnOnce() -> Exit>,
    },
    /// No resident reached readiness; the detail is what refused.
    Failed(String),
}

/// One finished resident run.
pub(crate) struct Exit {
    /// The exit status or the error waiting on it, as text.
    pub(crate) detail: String,
    /// How long the resident served after publishing discovery.
    pub(crate) ran_for: Duration,
}

/// Why the loop ended on its own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum End {
    /// The owner's registration was disabled: `spice down` asked it to stay stopped.
    Disabled,
    /// The owner's registration was removed: `spice down --keeper` ran.
    Unregistered,
}

/// The pause before the next attempt after `early_failures` consecutive
/// runs that never became stable: doubling from [`RELAUNCH_PAUSE`] and
/// capped at [`MAX_PAUSE`].
pub(crate) fn pause_after(early_failures: u32) -> Duration {
    let doubled =
        RELAUNCH_PAUSE.saturating_mul(1_u32.checked_shl(early_failures).unwrap_or(u32::MAX));
    doubled.min(MAX_PAUSE)
}

/// Keep one resident running until the native manager stops wanting it.
///
/// Every iteration first asks the manager, so a stop issued while a resident
/// was serving ends the loop as soon as that resident exits rather than
/// relaunching it once more.
///
/// # Errors
///
/// Returns the manager's refusal to answer, or the log's write failure.
pub(crate) fn keep(
    manager: &mut dyn Manager,
    registration: &str,
    attempt: &mut dyn FnMut() -> Attempt,
    pause: &mut dyn FnMut(Duration),
    log: &mut dyn Write,
) -> io::Result<End> {
    let mut early_failures = 0_u32;
    loop {
        match manager.status(registration)? {
            None => {
                writeln!(
                    log,
                    "{} registration {registration} is absent; keeper exits",
                    stamp()
                )?;
                return Ok(End::Unregistered);
            }
            Some(status) if !status.enabled => {
                writeln!(
                    log,
                    "{} registration {registration} is disabled; keeper exits",
                    stamp()
                )?;
                return Ok(End::Disabled);
            }
            Some(_) => {}
        }
        match attempt() {
            Attempt::Ready { endpoint, wait } => {
                writeln!(log, "{} resident serving at {endpoint}", stamp())?;
                let exit = wait();
                writeln!(
                    log,
                    "{} resident exited after {}s: {}",
                    stamp(),
                    exit.ran_for.as_secs(),
                    exit.detail
                )?;
                early_failures = if exit.ran_for >= STABLE_RUN {
                    0
                } else {
                    early_failures.saturating_add(1)
                };
            }
            Attempt::Failed(detail) => {
                writeln!(log, "{} resident did not start: {detail}", stamp())?;
                early_failures = early_failures.saturating_add(1);
            }
        }
        let delay = pause_after(early_failures);
        writeln!(log, "{} next attempt in {}s", stamp(), delay.as_secs())?;
        log.flush()?;
        pause(delay);
    }
}

fn stamp() -> String {
    jiff::Timestamp::now()
        .round(jiff::Unit::Second)
        .map_or_else(|_| jiff::Timestamp::now().to_string(), |at| at.to_string())
}
