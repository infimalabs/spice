use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use clap::ArgMatches;
use serde::Serialize;
use wire::output::{Admission, Bounded, INTERACTIVE, Request, Resume};
use wire::{Fault, FaultClass, Outcome, Refusal, Reply, ReplyPosition, SpiceCommand};

use listen::{Observation, TailBudget, Watch, WatchBatch, WatchCursor, WatchDelta};

const FOLLOW_MAX: Duration = Duration::from_secs(300);
const WAKE_INTERVAL: Duration = Duration::from_millis(100);
const WAKE_LIMIT: usize = 3_000;
const TAIL_WINDOW_BYTES: u64 = 128 * 1024;
const TAIL_SOURCE_BYTES: u64 = 16 * 1024 * 1024;
const TAIL_RECORDS: usize = 1_000;
const TAIL_PASSES: usize = 256;
const TAIL_DURATION: Duration = Duration::from_secs(1);
const CURSOR_PREFIX: &str = "watch.v1.";

/// Why one finite watch stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum WatchEnd {
    Snapshot,
    Deadline,
    OutputLimit,
    WakeLimit,
}

#[derive(Clone, Copy, Debug)]
struct FollowState {
    expires: Instant,
    wakes: usize,
    wake_limit: usize,
    last_wake: Option<Instant>,
}

impl FollowState {
    fn new(started: Instant, duration: Duration) -> Self {
        Self {
            expires: started + duration,
            wakes: 0,
            wake_limit: WAKE_LIMIT,
            last_wake: None,
        }
    }

    fn remaining(self, now: Instant) -> Result<Duration, WatchEnd> {
        let remaining = self.expires.saturating_duration_since(now);
        if remaining.is_zero() {
            Err(WatchEnd::Deadline)
        } else {
            Ok(remaining)
        }
    }

    /// How long a pending wake waits so admitted wakes stay `WAKE_INTERVAL` apart.
    fn rate_delay(self, now: Instant) -> Duration {
        self.last_wake.map_or(Duration::ZERO, |last| {
            (last + WAKE_INTERVAL).saturating_duration_since(now)
        })
    }

    fn record_wake(&mut self, now: Instant) -> Result<(), WatchEnd> {
        self.remaining(now)?;
        if self.wakes >= self.wake_limit {
            return Err(WatchEnd::WakeLimit);
        }
        self.wakes += 1;
        self.last_wake = Some(now);
        Ok(())
    }
}

/// One finite, resumable watch result.
#[derive(Debug, Serialize)]
pub(crate) struct WatchReport {
    root: PathBuf,
    batches: Bounded<WatchBatch>,
    cursor: String,
    end: WatchEnd,
    wakes: usize,
    elapsed_millis: u64,
}

impl fmt::Display for WatchReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for batch in &self.batches.rows {
            write!(formatter, "{batch}")?;
        }
        if let Some(omission) = &self.batches.omission {
            writeln!(formatter, "{omission}")?;
        }
        write!(
            formatter,
            "watch end={} wakes={} elapsed_ms={} cursor={}",
            match self.end {
                WatchEnd::Snapshot => "snapshot",
                WatchEnd::Deadline => "deadline",
                WatchEnd::OutputLimit => "output-limit",
                WatchEnd::WakeLimit => "wake-limit",
            },
            self.wakes,
            self.elapsed_millis,
            self.cursor,
        )
    }
}

pub(crate) fn run(
    matches: &ArgMatches,
    dialects: Vec<(&'static str, listen::Dialect)>,
) -> Outcome<WatchReport> {
    match run_inner(matches, dialects) {
        Ok(report) => Outcome::Reply(Reply {
            position: ReplyPosition::Direct {},
            body: report,
        }),
        Err(Failure::Refusal(refusal)) => Outcome::Refusal(refusal),
        Err(Failure::Fault(fault)) => Outcome::Fault(fault),
    }
}

/// Why a watch stopped before it had a report.
enum Failure {
    Refusal(Refusal),
    Fault(Fault),
}

fn run_inner(
    matches: &ArgMatches,
    dialects: Vec<(&'static str, listen::Dialect)>,
) -> Result<WatchReport, Failure> {
    let root = matches
        .get_one::<String>("ROOT")
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    let follow = matches.get_flag("follow");
    let deadline = deadline(matches, follow).map_err(Failure::Refusal)?;
    if follow && !real_directory(&root) {
        return Err(Failure::Refusal(refusal(
            "watch::root-invalid",
            Some(root.display().to_string()),
            "--follow requires an existing real directory",
        )));
    }
    let request = Request {
        rows: count(matches, "limit")?,
        bytes: count(matches, "max-bytes")?,
        text: count(matches, "max-text")?,
    };
    let budget = INTERACTIVE.budget(request).map_err(|error| {
        Failure::Refusal(refusal(
            "watch::bound-exceeded",
            Some(error.to_string()),
            "pass row, byte, and text bounds within the interactive maximum",
        ))
    })?;
    let cursor = matches
        .get_one::<String>("after")
        .map(|encoded| decode_cursor(encoded))
        .transpose()
        .map_err(Failure::Refusal)?;
    let mut watch = cursor.map_or_else(
        || Watch::new(&root, dialects.clone()),
        |cursor| Watch::resume(&root, dialects.clone(), cursor),
    );
    let mut admission = Admission::new(budget);
    let started = Instant::now();
    let mut end = WatchEnd::Snapshot;
    let mut wakes = 0;

    if follow {
        let bound = deadline.expect("follow has an admitted deadline");
        let mut state = FollowState::new(started, bound);
        end = follow_events(&root, &mut watch, &mut admission, budget, &mut state)?;
        wakes = state.wakes;
    } else {
        let initial = watch
            .poll_bounded(tail_budget(TAIL_DURATION))
            .map_err(surface_failure)?;
        if !admit_delta(&mut admission, &mut watch, initial, budget, &root)? {
            end = WatchEnd::OutputLimit;
        }
    }

    let cursor = encode_cursor(watch.cursor()).map_err(protocol_failure)?;
    let continuation_cursor = cursor.clone();
    let batches = admission.finish(|resume| {
        let bytes = match resume {
            Resume::Widened { bytes, .. } => Some(bytes),
            Resume::At(_) | Resume::Export(_) => None,
        };
        SpiceCommand::try_from(continuation_words(
            matches,
            &root,
            &continuation_cursor,
            bytes,
        ))
        .expect("watch continuation is one Spice command")
    });
    Ok(WatchReport {
        root,
        batches,
        cursor,
        end,
        wakes,
        elapsed_millis: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn follow_events(
    root: &Path,
    watch: &mut Watch,
    admission: &mut Admission<WatchBatch>,
    budget: wire::output::Budget,
    state: &mut FollowState,
) -> Result<WatchEnd, Failure> {
    use notify::{RecursiveMode, Watcher};

    // One slot: a burst of native events collapses into a single pending wake,
    // and each wake rescans every tape, so a dropped duplicate loses nothing.
    let (sender, receiver) = mpsc::sync_channel::<notify::Result<notify::Event>>(1);
    let mut observer = notify::recommended_watcher(move |event| {
        let _ = sender.try_send(event);
    })
    .map_err(|error| watch_failure("create filesystem watcher", error))?;
    observer
        .watch(root, RecursiveMode::Recursive)
        .map_err(|error| watch_failure("arm filesystem watcher", error))?;

    let remaining = match state.remaining(Instant::now()) {
        Ok(remaining) => remaining,
        Err(end) => return Ok(end),
    };
    let initial = watch
        .poll_bounded(tail_budget(remaining.min(TAIL_DURATION)))
        .map_err(surface_failure)?;
    if !admit_delta(admission, watch, initial, budget, root)? {
        return Ok(WatchEnd::OutputLimit);
    }

    loop {
        if let Err(end) = await_wake(&receiver, state)? {
            return Ok(end);
        }
        let remaining = match state.remaining(Instant::now()) {
            Ok(remaining) => remaining,
            Err(end) => return Ok(end),
        };
        let delta = watch
            .poll_bounded(tail_budget(remaining.min(TAIL_DURATION)))
            .map_err(surface_failure)?;
        if !admit_delta(admission, watch, delta, budget, root)? {
            return Ok(WatchEnd::OutputLimit);
        }
    }
}

/// Block until a filesystem notification arrives, hold it until the wake rate
/// admits another wake, and fold every notification delivered meanwhile into
/// that one wake. Returns the run's end instead when the deadline or the wake
/// ceiling comes first.
#[cfg(not(target_arch = "wasm32"))]
fn await_wake(
    receiver: &mpsc::Receiver<notify::Result<notify::Event>>,
    state: &mut FollowState,
) -> Result<Result<(), WatchEnd>, Failure> {
    let remaining = match state.remaining(Instant::now()) {
        Ok(remaining) => remaining,
        Err(end) => return Ok(Err(end)),
    };
    match receiver.recv_timeout(remaining) {
        Ok(event) => {
            event.map_err(|error| watch_failure("receive filesystem notification", error))?;
        }
        Err(mpsc::RecvTimeoutError::Timeout) => return Ok(Err(WatchEnd::Deadline)),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            return Err(Failure::Fault(Fault {
                class: FaultClass::Environment,
                reason: "watch::notification".to_owned(),
                detail: "filesystem notification channel disconnected".to_owned(),
            }));
        }
    }
    let now = Instant::now();
    let delay = state.rate_delay(now);
    if !delay.is_zero() {
        match state.remaining(now) {
            Ok(remaining) if delay < remaining => {}
            Ok(_) | Err(_) => return Ok(Err(WatchEnd::Deadline)),
        }
    }
    // Every notification delivered before the rate admits the wake folds
    // into it; the hold ends on the rate, not on a quiet channel.
    let admitted = now + delay;
    loop {
        let hold = admitted.saturating_duration_since(Instant::now());
        if hold.is_zero() {
            break;
        }
        match receiver.recv_timeout(hold) {
            Ok(event) => {
                event.map_err(|error| watch_failure("receive filesystem notification", error))?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Failure::Fault(Fault {
                    class: FaultClass::Environment,
                    reason: "watch::notification".to_owned(),
                    detail: "filesystem notification channel disconnected".to_owned(),
                }));
            }
        }
    }
    while let Ok(event) = receiver.try_recv() {
        event.map_err(|error| watch_failure("receive filesystem notification", error))?;
    }
    Ok(state.record_wake(Instant::now()))
}

#[cfg(target_arch = "wasm32")]
fn follow_events(
    _root: &Path,
    _watch: &mut Watch,
    _admission: &mut Admission<WatchBatch>,
    _budget: wire::output::Budget,
    _state: &mut FollowState,
) -> Result<WatchEnd, Failure> {
    Err(Failure::Fault(Fault {
        class: FaultClass::Environment,
        reason: "watch::unavailable".to_owned(),
        detail: "filesystem notifications are unavailable on wasm32".to_owned(),
    }))
}

fn tail_budget(duration: Duration) -> TailBudget {
    TailBudget::new(
        TAIL_WINDOW_BYTES,
        TAIL_SOURCE_BYTES,
        TAIL_RECORDS,
        TAIL_PASSES,
        duration,
    )
    .expect("the fixed watch tail allowance is valid")
}

fn admit_delta(
    admission: &mut Admission<WatchBatch>,
    watch: &mut Watch,
    delta: WatchDelta,
    budget: wire::output::Budget,
    root: &Path,
) -> Result<bool, Failure> {
    let WatchDelta { rows, cursor } = delta;
    let mut rows = rows.into_iter();
    while let Some(row) = rows.next() {
        let batch = bounded_batch(row.outcome, budget, root, INTERACTIVE.maximum().bytes)
            .map_err(protocol_failure)?;
        match admission.offer(batch) {
            Ok(true) => watch.resume_at(row.cursor),
            Ok(false) => {
                admission.withhold(rows.count());
                return Ok(false);
            }
            Err(error) => return Err(protocol_failure(error)),
        }
    }
    watch.resume_at(cursor);
    Ok(true)
}

fn bounded_batch(
    outcome: listen::StreamOutcome,
    budget: wire::output::Budget,
    root: &Path,
    maximum_bytes: usize,
) -> Result<WatchBatch, serde_json::Error> {
    let mut batch = WatchBatch::clipped(outcome, budget);
    let measured = wire::encoded_len(&batch)?;
    if measured > maximum_bytes {
        batch = WatchBatch::clipped(
            listen::StreamOutcome {
                mode: listen::Mode::Watch,
                root: root.to_path_buf(),
                observations: vec![Observation::Missing {
                    selector: root.display().to_string(),
                    detail: format!(
                        "one clipped watch row required {measured} bytes and was skipped at the repository maximum {maximum_bytes}"
                    ),
                }],
            },
            budget,
        );
    }
    Ok(batch)
}

fn deadline(matches: &ArgMatches, follow: bool) -> Result<Option<Duration>, Refusal> {
    let written = matches.get_one::<String>("deadline-seconds");
    if !follow {
        if written.is_some() {
            return Err(refusal(
                "watch::deadline-without-follow",
                written.cloned(),
                "pass --deadline-seconds only with --follow",
            ));
        }
        return Ok(None);
    }
    let Some(written) = written else {
        return Err(refusal(
            "watch::deadline-required",
            None,
            "--follow requires --deadline-seconds SECONDS",
        ));
    };
    let seconds = written.parse::<f64>().map_err(|_| {
        refusal(
            "watch::deadline-invalid",
            Some(written.clone()),
            "pass a positive finite number of seconds",
        )
    })?;
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(refusal(
            "watch::deadline-invalid",
            Some(written.clone()),
            "pass a positive finite number of seconds",
        ));
    }
    let duration = Duration::try_from_secs_f64(seconds).map_err(|_| {
        refusal(
            "watch::deadline-invalid",
            Some(written.clone()),
            "pass a positive finite number of seconds representable by the host clock",
        )
    })?;
    if duration.is_zero() {
        return Err(refusal(
            "watch::deadline-invalid",
            Some(written.clone()),
            "pass a positive finite number of seconds representable by the host clock",
        ));
    }
    if duration > FOLLOW_MAX {
        return Err(refusal(
            "watch::deadline-exceeded",
            Some(written.clone()),
            "pass at most 300 seconds",
        ));
    }
    Ok(Some(duration))
}

fn count(matches: &ArgMatches, name: &str) -> Result<Option<usize>, Failure> {
    matches
        .get_one::<String>(name)
        .map(|written| {
            written.parse::<usize>().map_err(|_| {
                Failure::Refusal(refusal(
                    "watch::bound-invalid",
                    Some(format!("--{name}={written}")),
                    "pass a nonnegative decimal integer bound",
                ))
            })
        })
        .transpose()
}

fn encode_cursor(cursor: &WatchCursor) -> Result<String, serde_json::Error> {
    wire::encode(cursor).map(|bytes| {
        format!(
            "{CURSOR_PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        )
    })
}

fn decode_cursor(encoded: &str) -> Result<WatchCursor, Refusal> {
    let decoded = encoded
        .strip_prefix(CURSOR_PREFIX)
        .and_then(|value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(value)
                .ok()
        })
        .and_then(|bytes| wire::decode::<WatchCursor>(&bytes).ok())
        .ok_or_else(|| {
            refusal(
                "watch::cursor-invalid",
                Some(encoded.to_owned()),
                "copy the exact watch.v1 cursor emitted by a previous watch",
            )
        })?;
    let canonical = encode_cursor(&decoded).map_err(|_| {
        refusal(
            "watch::cursor-invalid",
            Some(encoded.to_owned()),
            "copy the exact watch.v1 cursor emitted by a previous watch",
        )
    })?;
    if canonical != encoded {
        return Err(refusal(
            "watch::cursor-invalid",
            Some(encoded.to_owned()),
            "copy the exact canonical watch.v1 cursor emitted by a previous watch",
        ));
    }
    Ok(decoded)
}

fn continuation_words(
    matches: &ArgMatches,
    root: &Path,
    cursor: &str,
    bytes: Option<usize>,
) -> Vec<String> {
    let mut words = vec![
        "watch".to_owned(),
        root.display().to_string(),
        "--after".to_owned(),
        cursor.to_owned(),
    ];
    for name in ["limit", "max-text"] {
        if let Some(value) = matches.get_one::<String>(name) {
            words.extend([format!("--{name}"), value.clone()]);
        }
    }
    if let Some(bytes) = bytes.or_else(|| {
        matches
            .get_one::<String>("max-bytes")
            .and_then(|value| value.parse().ok())
    }) {
        words.extend(["--max-bytes".to_owned(), bytes.to_string()]);
    }
    words
}

fn real_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir())
}

fn refusal(reason: &str, observed: Option<String>, detail: &str) -> Refusal {
    Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason.to_owned(),
            Some("watch".to_owned()),
            Some("one finite bounded transcript watch".to_owned()),
            observed,
        )
        .with_detail(detail),
    )
}

fn surface_failure(error: listen::SurfaceError) -> Failure {
    Failure::Fault(Fault {
        class: FaultClass::Environment,
        reason: "listen::surface".to_owned(),
        detail: error.to_string(),
    })
}

fn protocol_failure(error: serde_json::Error) -> Failure {
    Failure::Fault(Fault {
        class: FaultClass::Protocol,
        reason: "watch::cursor-unencodable".to_owned(),
        detail: error.to_string(),
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn watch_failure(operation: &str, error: notify::Error) -> Failure {
    Failure::Fault(Fault {
        class: FaultClass::Environment,
        reason: "watch::notification".to_owned(),
        detail: format!("failed to {operation}: {error}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_state_expires_at_the_deadline_and_caps_noisy_wakes() {
        let started = Instant::now();
        let mut deadline = FollowState::new(started, Duration::from_secs(1));
        assert_eq!(
            deadline.record_wake(started + Duration::from_secs(1)),
            Err(WatchEnd::Deadline)
        );
        assert_eq!(deadline.wakes, 0);

        let mut noisy = FollowState::new(started, Duration::from_secs(1));
        noisy.wake_limit = 2;
        assert_eq!(noisy.record_wake(started), Ok(()));
        assert_eq!(noisy.record_wake(started), Ok(()));
        assert_eq!(noisy.record_wake(started), Err(WatchEnd::WakeLimit));
        assert_eq!(noisy.wakes, 2);
    }

    #[test]
    fn follow_state_holds_each_wake_until_the_previous_one_is_an_interval_old() {
        let started = Instant::now();
        let mut state = FollowState::new(started, Duration::from_secs(1));
        assert_eq!(state.rate_delay(started), Duration::ZERO);

        assert_eq!(state.record_wake(started), Ok(()));
        let soon = started + WAKE_INTERVAL / 4;
        assert_eq!(state.rate_delay(soon), WAKE_INTERVAL - WAKE_INTERVAL / 4);
        assert_eq!(state.rate_delay(started + WAKE_INTERVAL), Duration::ZERO);

        let late = started + WAKE_INTERVAL * 3;
        assert_eq!(state.record_wake(late), Ok(()));
        assert_eq!(state.rate_delay(late), WAKE_INTERVAL);
    }

    #[test]
    fn follow_requires_a_positive_deadline_within_the_repository_maximum() {
        fn matches(arguments: &[&str]) -> clap::ArgMatches {
            let parsed = spice::cli::command()
                .try_get_matches_from(arguments)
                .expect("watch arguments parse");
            parsed
                .subcommand_matches("watch")
                .expect("watch subcommand is selected")
                .clone()
        }

        assert!(deadline(&matches(&["spice", "watch", "--follow"]), true).is_err());
        assert!(
            deadline(
                &matches(&["spice", "watch", "--follow", "--deadline-seconds", "0"]),
                true
            )
            .is_err()
        );
        assert!(
            deadline(
                &matches(&["spice", "watch", "--follow", "--deadline-seconds", "1e-300"]),
                true
            )
            .is_err()
        );
        assert!(
            deadline(
                &matches(&[
                    "spice",
                    "watch",
                    "--follow",
                    "--deadline-seconds",
                    "300.001"
                ]),
                true
            )
            .is_err()
        );
        assert_eq!(
            deadline(
                &matches(&["spice", "watch", "--follow", "--deadline-seconds", "300"]),
                true
            ),
            Ok(Some(FOLLOW_MAX))
        );
    }

    #[test]
    fn watch_cursor_tokens_are_canonical_and_stable() {
        let cursor = WatchCursor::default();
        let encoded = encode_cursor(&cursor).expect("cursor encodes");
        assert_eq!(decode_cursor(&encoded), Ok(cursor));
        assert!(decode_cursor(&format!("{encoded}=")).is_err());
    }

    #[test]
    fn oversized_rows_become_explicit_missing_input() {
        let root = Path::new("/watch-root");
        let outcome = listen::StreamOutcome {
            mode: listen::Mode::Watch,
            root: root.to_path_buf(),
            observations: vec![Observation::Missing {
                selector: "large".to_owned(),
                detail: "x".repeat(2_048),
            }],
        };
        let budget = INTERACTIVE
            .budget(Request {
                rows: None,
                bytes: None,
                text: None,
            })
            .expect("interactive defaults are valid");
        let batch = bounded_batch(outcome, budget, root, 128).expect("batch encodes");
        assert!(matches!(
            batch.outcome.observations.as_slice(),
            [Observation::Missing { .. }]
        ));
    }
}
