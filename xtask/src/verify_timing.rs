//! Bounded lifecycle records for serial package-scoped actions.

use std::fmt;
use std::io::{self, Write};
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

const PROGRESS_INTERVAL: Duration = Duration::from_secs(30);
const ATTENTION_THRESHOLD: Duration = Duration::from_secs(60);
const WARNING_THRESHOLD: Duration = Duration::from_secs(120);
const CRITICAL_THRESHOLD: Duration = Duration::from_secs(180);

/// One verifier action's stable identity and position in its report route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Stage<'a> {
    pub(super) ordinal: usize,
    pub(super) total: usize,
    pub(super) identity: &'a str,
    pub(super) family: &'static str,
}

/// How an observed action ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Disposition {
    Exit(Option<i32>),
    LaunchError,
    RenderError,
    Cancelled,
    TimedOut,
    CleanupError,
}

impl Disposition {
    pub(super) const fn from_status(code: Option<i32>) -> Self {
        Self::Exit(code)
    }
}

impl fmt::Display for Disposition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exit(Some(code)) => write!(formatter, "exit code={code}"),
            Self::Exit(None) => formatter.write_str("signal"),
            Self::LaunchError => formatter.write_str("launch-error"),
            Self::RenderError => formatter.write_str("render-error"),
            Self::Cancelled => formatter.write_str("cancelled"),
            Self::TimedOut => formatter.write_str("timed-out"),
            Self::CleanupError => formatter.write_str("cleanup-error"),
        }
    }
}

/// The semantic result and terminal spelling a stage finisher selected.
pub(super) struct Conclusion<T, E> {
    result: Result<T, E>,
    disposition: Disposition,
}

impl<T, E> Conclusion<T, E> {
    pub(super) const fn success(value: T, disposition: Disposition) -> Self {
        Self {
            result: Ok(value),
            disposition,
        }
    }

    pub(super) const fn failure(error: E, disposition: Disposition) -> Self {
        Self {
            result: Err(error),
            disposition,
        }
    }
}

/// Observe one blocking action without allowing the next serial action to run.
///
/// The action owns the worker thread. The calling thread remains the sole
/// writer and wakes every thirty seconds, so child output can remain inherited
/// while verifier lifecycle records stay serialized and flush immediately.
pub(super) fn observe<T, U, E, W, F, G>(
    output: &mut W,
    stage: Stage<'_>,
    action: F,
    finish: G,
) -> Result<U, E>
where
    T: Send,
    E: From<io::Error>,
    W: Write,
    F: FnOnce() -> T + Send,
    G: FnOnce(T, &mut W) -> Result<Conclusion<U, E>, E>,
{
    observe_with_waiter(output, stage, &mut RealWaiter::new(), action, finish)
}

fn observe_with_waiter<T, U, E, W, F, G, C>(
    output: &mut W,
    stage: Stage<'_>,
    waiter: &mut C,
    action: F,
    finish: G,
) -> Result<U, E>
where
    T: Send,
    E: From<io::Error>,
    W: Write,
    F: FnOnce() -> T + Send,
    G: FnOnce(T, &mut W) -> Result<Conclusion<U, E>, E>,
    C: StageWaiter,
{
    write_start(output, stage).map_err(E::from)?;
    let outcome = thread::scope(|scope| -> Result<thread::Result<T>, E> {
        let (sender, receiver) = mpsc::sync_channel(1);
        scope.spawn(move || {
            let outcome = panic::catch_unwind(AssertUnwindSafe(action));
            let _ = sender.send(outcome);
        });

        let mut next_progress = PROGRESS_INTERVAL;
        loop {
            match waiter.wait(&receiver, next_progress) {
                StageWait::Ready(outcome) => break Ok(outcome),
                StageWait::TimedOut => {
                    write_progress(output, stage, next_progress).map_err(E::from)?;
                    next_progress += PROGRESS_INTERVAL;
                }
            }
        }
    })?;

    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(payload) => {
            let _ = write_terminal(output, stage, waiter.elapsed(), "panic");
            panic::resume_unwind(payload);
        }
    };
    let conclusion = match finish(outcome, output) {
        Ok(conclusion) => conclusion,
        Err(error) => {
            // The action's rendering failure remains authoritative even when
            // its best-effort terminal record meets the same broken sink.
            let _ = write_terminal(
                output,
                stage,
                waiter.elapsed(),
                &Disposition::RenderError.to_string(),
            );
            return Err(error);
        }
    };
    write_terminal(
        output,
        stage,
        waiter.elapsed(),
        &conclusion.disposition.to_string(),
    )
    .map_err(E::from)?;
    conclusion.result
}

pub(super) fn write_start(output: &mut impl Write, stage: Stage<'_>) -> io::Result<()> {
    writeln!(
        output,
        concat!(
            "[verify] stage=start ordinal={}/{} elapsed_ms=0 ",
            "family={} identity={:?}"
        ),
        stage.ordinal, stage.total, stage.family, stage.identity,
    )?;
    output.flush()
}

pub(super) fn write_progress(
    output: &mut impl Write,
    stage: Stage<'_>,
    elapsed: Duration,
) -> io::Result<()> {
    writeln!(
        output,
        concat!(
            "[verify] stage=progress ordinal={}/{} elapsed_ms={} ",
            "policy_band={} threshold={} family={} identity={:?}"
        ),
        stage.ordinal,
        stage.total,
        elapsed.as_millis(),
        policy_band(elapsed),
        crossed_threshold(elapsed),
        stage.family,
        stage.identity,
    )?;
    output.flush()
}

pub(super) fn write_terminal(
    output: &mut impl Write,
    stage: Stage<'_>,
    elapsed: Duration,
    disposition: &str,
) -> io::Result<()> {
    writeln!(
        output,
        concat!(
            "[verify] stage=terminal ordinal={}/{} elapsed_ms={} ",
            "policy_band={} disposition={} family={} identity={:?}"
        ),
        stage.ordinal,
        stage.total,
        elapsed.as_millis(),
        policy_band(elapsed),
        disposition,
        stage.family,
        stage.identity,
    )?;
    output.flush()
}

fn policy_band(elapsed: Duration) -> &'static str {
    if elapsed >= CRITICAL_THRESHOLD {
        "180s-plus"
    } else if elapsed >= WARNING_THRESHOLD {
        "120s-to-179s"
    } else if elapsed >= ATTENTION_THRESHOLD {
        "60s-to-119s"
    } else {
        "under-60s"
    }
}

fn crossed_threshold(elapsed: Duration) -> &'static str {
    if elapsed == CRITICAL_THRESHOLD {
        "180s"
    } else if elapsed == WARNING_THRESHOLD {
        "120s"
    } else if elapsed == ATTENTION_THRESHOLD {
        "60s"
    } else {
        "none"
    }
}

enum StageWait<T> {
    Ready(thread::Result<T>),
    TimedOut,
}

trait StageWaiter {
    fn elapsed(&self) -> Duration;

    fn wait<T>(
        &mut self,
        receiver: &Receiver<thread::Result<T>>,
        deadline: Duration,
    ) -> StageWait<T>;
}

struct RealWaiter {
    started: Instant,
}

impl RealWaiter {
    fn new() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl StageWaiter for RealWaiter {
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    fn wait<T>(
        &mut self,
        receiver: &Receiver<thread::Result<T>>,
        deadline: Duration,
    ) -> StageWait<T> {
        let remaining = deadline.saturating_sub(self.elapsed());
        match receiver.recv_timeout(remaining) {
            Ok(outcome) => StageWait::Ready(outcome),
            Err(RecvTimeoutError::Timeout) => StageWait::TimedOut,
            Err(RecvTimeoutError::Disconnected) => {
                unreachable!("the stage worker always sends its caught outcome")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};

    use super::*;

    struct AcceleratedWaiter {
        elapsed: Duration,
        started: Receiver<()>,
        release: Option<mpsc::Sender<()>>,
        child_started: bool,
        released: bool,
    }

    impl StageWaiter for AcceleratedWaiter {
        fn elapsed(&self) -> Duration {
            self.elapsed
        }

        fn wait<T>(
            &mut self,
            receiver: &Receiver<thread::Result<T>>,
            deadline: Duration,
        ) -> StageWait<T> {
            if !self.child_started {
                self.started
                    .recv_timeout(Duration::from_secs(1))
                    .expect("silent fixture stage starts");
                self.child_started = true;
            }
            if self.released {
                let outcome = receiver
                    .recv_timeout(Duration::from_secs(1))
                    .expect("released fixture stage terminates");
                self.elapsed += Duration::from_secs(1);
                return StageWait::Ready(outcome);
            }

            self.elapsed = deadline;
            if deadline == CRITICAL_THRESHOLD {
                self.release
                    .take()
                    .expect("fixture release is unique")
                    .send(())
                    .expect("fixture worker still receives release");
                self.released = true;
            }
            StageWait::TimedOut
        }
    }

    #[test]
    fn silent_stage_crosses_every_threshold_before_failure_and_the_next_action() {
        let (started_sender, started_receiver) = mpsc::sync_channel(1);
        let (release_sender, release_receiver) = mpsc::channel();
        let terminated = Arc::new(AtomicBool::new(false));
        let worker_terminated = Arc::clone(&terminated);
        let mut waiter = AcceleratedWaiter {
            elapsed: Duration::ZERO,
            started: started_receiver,
            release: Some(release_sender),
            child_started: false,
            released: false,
        };
        let mut output = Vec::new();
        let stage = Stage {
            ordinal: 1,
            total: 2,
            identity: "silent fixture stage",
            family: "child-process:fixture",
        };

        let result: io::Result<()> = observe_with_waiter(
            &mut output,
            stage,
            &mut waiter,
            move || {
                started_sender.send(()).expect("announce fixture stage");
                release_receiver
                    .recv()
                    .expect("receive accelerated release");
                worker_terminated.store(true, Ordering::Release);
                (false, Some(17))
            },
            |(success, code), _| {
                let disposition = Disposition::from_status(code);
                Ok(if success {
                    Conclusion::success((), disposition)
                } else {
                    Conclusion::failure(
                        io::Error::other(format!("fixture exit {code:?}")),
                        disposition,
                    )
                })
            },
        );

        assert_eq!(
            result
                .expect_err("child failure survives observation")
                .to_string(),
            "fixture exit Some(17)"
        );
        assert!(
            terminated.load(Ordering::Acquire),
            "the stage returned before its work terminated"
        );
        writeln!(output, "[verify] later action").expect("write later fixture action");

        let output = String::from_utf8(output).expect("fixture lifecycle is UTF-8");
        assert_eq!(output.matches("stage=start").count(), 1);
        assert_eq!(output.matches("stage=terminal").count(), 1);
        for (elapsed, threshold, band) in [
            (30_000, "none", "under-60s"),
            (60_000, "60s", "60s-to-119s"),
            (90_000, "none", "60s-to-119s"),
            (120_000, "120s", "120s-to-179s"),
            (150_000, "none", "120s-to-179s"),
            (180_000, "180s", "180s-plus"),
        ] {
            assert!(
                output.contains(&format!(
                    "stage=progress ordinal=1/2 elapsed_ms={elapsed} \
                     policy_band={band} threshold={threshold} \
                     family=child-process:fixture identity=\"silent fixture stage\""
                )),
                "missing threshold record from {output}"
            );
        }
        let terminal = output
            .find("stage=terminal")
            .expect("fixture terminal record exists");
        let later = output.find("later action").expect("later action exists");
        assert!(
            terminal < later,
            "later action crossed the stage wait: {output}"
        );
        assert!(output.contains("disposition=exit code=17"));
    }
}
