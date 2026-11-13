//! Target-neutral reconnect pacing for the browser's live socket.
//!
//! The loop that runs connections is wasm-only, so the decision it would
//! otherwise make inline lives here and is reachable from an ordinary native
//! test. What remains there is the wait itself, which has nothing to decide.
//! Nothing here may be duplicated there: a second copy is how a bound that is
//! proven and a bound that ships stop being the same number.

/// The shortest delay before reconnecting, doubled per consecutive failure.
const BASE_MILLIS: u32 = 500;

/// The longest delay before reconnecting, whatever the failure count.
///
/// Bounded so a resident that comes back after a long outage is found within a
/// known time rather than after a backoff that grew without limit.
const MAX_MILLIS: u32 = 10_000;

/// How long to wait before the attempt numbered `attempt`, counting from zero.
///
/// Every arithmetic step is checked rather than wrapped. A shift wide enough to
/// overflow would land on a *short* delay, turning a long outage into the one
/// case that hammers the resident hardest.
const fn delay_for(attempt: u32) -> u32 {
    match 2_u32.checked_pow(attempt) {
        Some(factor) => match BASE_MILLIS.checked_mul(factor) {
            Some(delay) if delay < MAX_MILLIS => delay,
            _ => MAX_MILLIS,
        },
        None => MAX_MILLIS,
    }
}

/// How long the surface waits between connection attempts.
///
/// The counter resets on a connection that was *admitted*, not on one that
/// merely opened. A resident that accepts the socket and then refuses the
/// opening would otherwise reset the backoff on every try, so the outage that
/// most deserves pacing would be the one paced least.
pub(crate) struct Backoff {
    attempt: u32,
}

impl Backoff {
    /// A backoff that has not yet paced anything.
    pub(crate) const fn new() -> Self {
        Self { attempt: 0 }
    }

    /// Record how one connection ended; report the wait before the next.
    ///
    /// Reporting the delay and advancing the counter are one step because a
    /// caller that could read the delay without advancing could wait the same
    /// interval forever and still look like it was backing off.
    pub(crate) fn settle(&mut self, admitted: bool) -> u32 {
        if admitted {
            self.attempt = 0;
        }
        let delay = delay_for(self.attempt);
        self.attempt = self.attempt.saturating_add(1);
        delay
    }
}

#[cfg(test)]
mod tests {
    use super::{BASE_MILLIS, Backoff, MAX_MILLIS};

    /// Delays reported for `count` consecutive unadmitted connections.
    fn failing(count: usize) -> Vec<u32> {
        let mut backoff = Backoff::new();
        (0..count).map(|_| backoff.settle(false)).collect()
    }

    #[test]
    fn consecutive_failures_double_the_delay_up_to_the_maximum() {
        assert_eq!(
            failing(8),
            vec![500, 1_000, 2_000, 4_000, 8_000, 10_000, 10_000, 10_000]
        );
    }

    #[test]
    fn no_failure_count_reports_a_delay_outside_the_declared_bounds() {
        let mut backoff = Backoff::new();
        for _ in 0..64 {
            let delay = backoff.settle(false);
            assert!(
                (BASE_MILLIS..=MAX_MILLIS).contains(&delay),
                "delay {delay} outside {BASE_MILLIS}..={MAX_MILLIS}"
            );
        }
        // Past `2^32` the doubling can no longer be represented. The maximum is
        // the answer there too, so the count that overflows is not the count
        // that reconnects fastest.
        for _ in 0..u32::BITS + 2 {
            assert_eq!(backoff.settle(false), MAX_MILLIS);
        }
    }

    #[test]
    fn an_admitted_connection_returns_the_next_wait_to_the_base_delay() {
        let mut backoff = Backoff::new();
        for _ in 0..5 {
            backoff.settle(false);
        }
        assert_eq!(backoff.settle(true), BASE_MILLIS);
        // The reset leaves the counter where a first failure does, so recovery
        // resumes the ladder from the bottom instead of skipping a rung.
        assert_eq!(backoff.settle(false), BASE_MILLIS * 2);
    }

    #[test]
    fn an_unadmitted_connection_does_not_return_the_wait_to_the_base_delay() {
        let mut backoff = Backoff::new();
        backoff.settle(false);
        assert_eq!(backoff.settle(false), BASE_MILLIS * 2);
    }
}
