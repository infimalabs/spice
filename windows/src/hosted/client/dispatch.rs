//! Provider visibility boundary for freshly dispatched runs.

use std::ffi::OsString;
use std::sync::mpsc;
use std::time::Duration;

use super::{Cancellation, HostedRun, WindowsHostedError, cancel_run, command, successful};

/// Provider reads admitted before a dispatched run is considered unavailable.
const DISPATCH_READ_ATTEMPTS: usize = 5;
/// Delay before each provider read while the dispatch REST view converges.
const DISPATCH_READ_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Eq, PartialEq)]
enum BoundedReadError<E> {
    Wait(E),
    Exhausted(E),
}

fn bounded_read<T, E>(
    attempts: usize,
    mut wait: impl FnMut() -> Result<(), E>,
    mut read: impl FnMut() -> Result<T, E>,
) -> Result<T, BoundedReadError<E>> {
    assert!(attempts > 0, "a bounded provider read needs an attempt");
    for attempt in 0..attempts {
        wait().map_err(BoundedReadError::Wait)?;
        match read() {
            Ok(value) => return Ok(value),
            Err(error) if attempt + 1 == attempts => {
                return Err(BoundedReadError::Exhausted(error));
            }
            Err(_) => {}
        }
    }
    unreachable!("the final bounded provider read returns")
}

/// Return only after the provider can answer for the dispatched run. A run
/// that never becomes readable remains intact with its one-use source ref.
pub(super) fn await_readable(
    run: &HostedRun<'_>,
    run_id: u64,
    cancellation: &Cancellation,
) -> Result<(), WindowsHostedError> {
    let read = || {
        successful(
            "dispatched workflow visibility",
            command(
                run.root,
                "gh",
                [
                    OsString::from("api"),
                    OsString::from("--silent"),
                    OsString::from(format!("repos/{}/actions/runs/{run_id}", run.repository)),
                ],
            )?,
        )
        .map(|_| ())
    };
    match bounded_read(
        DISPATCH_READ_ATTEMPTS,
        || wait_interval(run, run_id, cancellation),
        read,
    ) {
        Ok(()) => Ok(()),
        Err(BoundedReadError::Wait(error)) => Err(error),
        Err(BoundedReadError::Exhausted(reason)) => Err(WindowsHostedError::DispatchUnreadable {
            run_id,
            source_ref: run.source_ref.to_owned(),
            observations: DISPATCH_READ_ATTEMPTS,
            reason: Box::new(reason),
        }),
    }
}

fn wait_interval(
    run: &HostedRun<'_>,
    run_id: u64,
    cancellation: &Cancellation,
) -> Result<(), WindowsHostedError> {
    let (events, receiver) = mpsc::channel();
    cancellation.wake(events.clone());
    // Held so only a cancellation or the retry deadline ends the wait.
    let _held = events;
    match receiver.recv_timeout(DISPATCH_READ_INTERVAL) {
        Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
        Ok(_) | Err(mpsc::RecvTimeoutError::Disconnected) => {
            cancel_run(run.root, run.repository, run_id)?;
            Err(WindowsHostedError::Cancelled {
                run_id: Some(run_id),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::VecDeque;

    use super::{BoundedReadError, DISPATCH_READ_ATTEMPTS, bounded_read};

    #[test]
    fn readable_dispatch_returns_on_the_first_provider_read() {
        let waits = Cell::new(0);
        let reads = Cell::new(0);
        let result = bounded_read(
            DISPATCH_READ_ATTEMPTS,
            || {
                waits.set(waits.get() + 1);
                Ok::<_, &str>(())
            },
            || {
                reads.set(reads.get() + 1);
                Ok::<_, &str>(42)
            },
        );

        assert_eq!(result, Ok(42));
        assert_eq!((waits.get(), reads.get()), (1, 1));
    }

    #[test]
    fn late_dispatch_returns_without_exhausting_the_bound() {
        let mut provider = VecDeque::from([Err("missing"), Err("missing"), Ok(42)]);
        let waits = Cell::new(0);
        let result = bounded_read(
            DISPATCH_READ_ATTEMPTS,
            || {
                waits.set(waits.get() + 1);
                Ok::<_, &str>(())
            },
            || provider.pop_front().expect("one provider outcome remains"),
        );

        assert_eq!(result, Ok(42));
        assert_eq!(waits.get(), 3);
    }

    #[test]
    fn never_readable_dispatch_exhausts_the_exact_bound() {
        let reads = Cell::new(0);
        let result = bounded_read(
            DISPATCH_READ_ATTEMPTS,
            || Ok::<_, &str>(()),
            || {
                reads.set(reads.get() + 1);
                Err::<(), _>("missing")
            },
        );

        assert_eq!(result, Err(BoundedReadError::Exhausted("missing")));
        assert_eq!(reads.get(), DISPATCH_READ_ATTEMPTS);
    }
}
