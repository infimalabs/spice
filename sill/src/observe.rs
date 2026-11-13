use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

/// Child output stream selected for structured observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum OutputStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

impl fmt::Display for OutputStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        })
    }
}

/// Why an observed record was delivered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordBoundary {
    /// A newline completed the record. The delivered bytes include that newline.
    Complete,
    /// End of stream closed a final record without a newline.
    Terminal,
    /// The record reached the frame limit. The delivered bytes are the bounded
    /// head without a newline; the remainder through the closing newline is
    /// discarded, and the following record frames normally.
    Truncated,
}

/// One byte-exact selected-stream record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservedRecord<'a> {
    bytes: &'a [u8],
    boundary: RecordBoundary,
}

impl<'a> ObservedRecord<'a> {
    #[cfg(any(unix, windows))]
    pub(crate) const fn new(bytes: &'a [u8], boundary: RecordBoundary) -> Self {
        Self { bytes, boundary }
    }

    /// Exact child bytes, including the newline for a complete record.
    #[must_use]
    pub const fn bytes(self) -> &'a [u8] {
        self.bytes
    }

    /// Delimiter or terminal boundary that closed these bytes.
    #[must_use]
    pub const fn boundary(self) -> RecordBoundary {
        self.boundary
    }
}

/// A selected-record sink refused one record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObserveError {
    detail: String,
}

impl ObserveError {
    /// Name a sink refusal without exposing a sink-specific error type to SILL.
    #[must_use]
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }

    /// Stable diagnostic supplied by the sink.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for ObserveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl Error for ObserveError {}

/// Attribution sampled when the sink exhausts its delivery bound.
///
/// Kept behind an allocation only on failure so healthy process outcomes do
/// not carry the size of these diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservationStall {
    /// Actual time spent waiting, including scheduler delay.
    pub elapsed: Duration,
    /// Queue admission or completion of one callback.
    pub waiting: &'static str,
    /// Sink-owned stage sampled without blocking on its work.
    pub stage: &'static str,
}

/// Terminal failure of the selected-stream observation path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObservationFailure {
    /// The selected child pipe could not be read.
    Reader {
        stream: OutputStream,
        detail: String,
    },
    /// A full queue or one sink callback exhausted its delivery bound.
    SinkBackpressure {
        stream: OutputStream,
        capacity: usize,
        deadline: Duration,
        stall: Box<ObservationStall>,
    },
    /// The selected-record sink refused durable handling or interpretation.
    SinkRefused {
        stream: OutputStream,
        detail: String,
    },
    /// The sink worker or callback disappeared unexpectedly.
    ObserverLost { stream: OutputStream },
}

impl ObservationFailure {
    /// Selected stream whose evidence path failed.
    #[must_use]
    pub const fn stream(&self) -> OutputStream {
        match self {
            Self::Reader { stream, .. }
            | Self::SinkBackpressure { stream, .. }
            | Self::SinkRefused { stream, .. }
            | Self::ObserverLost { stream } => *stream,
        }
    }
}

impl fmt::Display for ObservationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reader { stream, detail } => {
                write!(formatter, "reading selected {stream} failed: {detail}")
            }
            Self::SinkBackpressure {
                stream,
                capacity,
                deadline,
                stall,
            } => write!(
                formatter,
                concat!(
                    "selected {} observer stalled for {:?} waiting for {} ",
                    "(sink stage: {}; {}-record queue; {:?} delivery limit)"
                ),
                stream, stall.elapsed, stall.waiting, stall.stage, capacity, deadline,
            ),
            Self::SinkRefused { stream, detail } => {
                write!(
                    formatter,
                    "selected {stream} observer refused a record: {detail}"
                )
            }
            Self::ObserverLost { stream } => {
                write!(formatter, "selected {stream} observer was lost")
            }
        }
    }
}

/// Sink for byte-exact selected-stream records.
///
/// `observe` is called by a bounded worker, never by the pipe reader. Its
/// delivery limit belongs to [`Observation`], independently of the process's
/// post-exit output-drain deadline. A finite queue bounds buffered records
/// while a slow sink catches up.
pub trait RecordObserver: Send + Sync + 'static {
    /// Accept one complete or terminal record.
    ///
    /// # Errors
    ///
    /// Returning an error terminates the observed process group.
    fn observe(&self, record: ObservedRecord<'_>) -> Result<(), ObserveError>;

    /// Finish the selected stream after all records, including a terminal tail.
    /// Runs on the same bounded worker as [`Self::observe`], even for empty
    /// streams and streams ending at a newline.
    ///
    /// # Errors
    ///
    /// A refusal terminates observation through the same path as a record error.
    fn finish(&self) -> Result<(), ObserveError> {
        Ok(())
    }

    /// Name the active sink stage without waiting on the sink's own locks.
    /// Called only to diagnose exhausted delivery bounds.
    fn stage(&self) -> &'static str {
        "observe"
    }

    /// Receive the single terminal observation failure, after bounded cleanup.
    fn failed(&self, _failure: &ObservationFailure) {}

    /// Receive the child process's kernel-backed exit notification.
    ///
    /// This callback is driven by the blocking `waitid` watcher. It does not
    /// imply that selected output has finished draining; the owner must reap
    /// the [`crate::Running`] value to join both completion paths.
    fn exited(&self) {}
}

/// One bounded selected-stream observation contract.
pub struct Observation {
    stream: OutputStream,
    frame_limit: usize,
    queue_capacity: usize,
    sink_deadline: Duration,
    #[cfg_attr(
        not(any(unix, windows)),
        expect(
            dead_code,
            reason = "only the unix and windows spawners read the observer"
        )
    )]
    observer: Arc<dyn RecordObserver>,
}

impl Observation {
    /// Select one captured stream with explicit framing, queue and delivery bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ObservationSpecError`] when any bound is zero. `sink_deadline`
    /// bounds one callback or queue admission, not process-exit draining.
    pub fn new(
        stream: OutputStream,
        frame_limit: usize,
        queue_capacity: usize,
        sink_deadline: Duration,
        observer: Arc<dyn RecordObserver>,
    ) -> Result<Self, ObservationSpecError> {
        if frame_limit == 0 {
            return Err(ObservationSpecError::ZeroFrameLimit);
        }
        if queue_capacity == 0 {
            return Err(ObservationSpecError::ZeroQueueCapacity);
        }
        if sink_deadline.is_zero() {
            return Err(ObservationSpecError::ZeroSinkDeadline);
        }
        Ok(Self {
            stream,
            frame_limit,
            queue_capacity,
            sink_deadline,
            observer,
        })
    }

    #[cfg(any(unix, windows))]
    pub(crate) const fn stream(&self) -> OutputStream {
        self.stream
    }

    #[cfg(any(unix, windows))]
    pub(crate) const fn frame_limit(&self) -> usize {
        self.frame_limit
    }

    #[cfg(any(unix, windows))]
    pub(crate) const fn queue_capacity(&self) -> usize {
        self.queue_capacity
    }

    #[cfg(any(unix, windows))]
    pub(crate) const fn sink_deadline(&self) -> Duration {
        self.sink_deadline
    }
    #[cfg(any(unix, windows))]
    pub(crate) fn observer(&self) -> Arc<dyn RecordObserver> {
        Arc::clone(&self.observer)
    }
}

impl fmt::Debug for Observation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Observation")
            .field("stream", &self.stream)
            .field("frame_limit", &self.frame_limit)
            .field("queue_capacity", &self.queue_capacity)
            .field("sink_deadline", &self.sink_deadline)
            .finish_non_exhaustive()
    }
}

/// Invalid selected-stream observation bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationSpecError {
    /// A record frame must retain at least one byte.
    ZeroFrameLimit,
    /// The reader-to-sink handoff must admit at least one record.
    ZeroQueueCapacity,
    /// Delivery must have a nonzero bound distinct from process-exit draining.
    ZeroSinkDeadline,
}

impl fmt::Display for ObservationSpecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ZeroFrameLimit => "selected-stream frame limit must be nonzero",
            Self::ZeroQueueCapacity => "selected-stream queue capacity must be nonzero",
            Self::ZeroSinkDeadline => "selected-stream sink delivery deadline must be nonzero",
        })
    }
}

impl Error for ObservationSpecError {}
