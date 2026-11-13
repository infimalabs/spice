use super::frame_queue::{self, FrameReceiver, FrameSender, SendError};
use super::{CapturedOutput, IoFailure, ProcessState, WAKE_BYTE, fail_observation, platform};
use crate::observe::{
    Observation, ObservationFailure, ObservationStall, ObservedRecord, OutputStream,
    RecordBoundary, RecordObserver,
};
use crate::spec::ProcessGroup;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixDatagram;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(super) struct CaptureHandle {
    state: Arc<Mutex<CaptureState>>,
    failure_supervised: bool,
    interrupt: Option<UnixDatagram>,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[derive(Debug)]
struct CaptureState {
    output: CapturedOutput,
    failure: Option<IoFailure>,
    observer_complete: bool,
}

impl CaptureHandle {
    pub(super) fn is_complete(&self) -> bool {
        self.state
            .lock()
            .is_ok_and(|state| state.output.complete && state.observer_complete)
    }

    pub(super) fn failure(&self) -> io::Result<()> {
        let state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("capture state lock is poisoned"))?;
        if self.failure_supervised {
            return Ok(());
        }
        state
            .failure
            .as_ref()
            .map_or(Ok(()), |failure| Err(failure.error()))
    }

    pub(super) fn snapshot(&self) -> io::Result<CapturedOutput> {
        self.state
            .lock()
            .map(|state| state.output.clone())
            .map_err(|_| io::Error::other("capture state lock is poisoned"))
    }

    pub(super) fn finish(mut self) -> io::Result<()> {
        let complete = self
            .state
            .lock()
            .map(|state| state.output.complete)
            .map_err(|_| io::Error::other("capture state lock is poisoned"))?;
        if !complete && let Some(interrupt) = &self.interrupt {
            let _interrupt_result = interrupt.send(WAKE_BYTE);
        }
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| io::Error::other("capture worker panicked"))?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct FailureContext {
    pub(super) pid: u32,
    pub(super) group: ProcessGroup,
    pub(super) owner: String,
    pub(super) grace: Duration,
    pub(super) reap: Duration,
    pub(super) process: Arc<ProcessState>,
    pub(super) wake: std::os::unix::net::UnixDatagram,
    pub(super) failed: AtomicBool,
}

#[derive(Debug)]
enum Frame {
    Record {
        bytes: Vec<u8>,
        boundary: RecordBoundary,
    },
    End,
}

pub(super) struct CaptureWorker {
    pub(super) stream: OutputStream,
    pub(super) observation: Option<Observation>,
    pub(super) failure_observer: Option<Arc<dyn RecordObserver>>,
    pub(super) failure: Arc<FailureContext>,
    pub(super) wake: std::os::unix::net::UnixDatagram,
    pub(super) observer_wake: Option<std::os::unix::net::UnixDatagram>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CaptureRetention {
    Prefix,
    Suffix,
}

pub(super) fn start_capture<R>(
    mut reader: R,
    limit: usize,
    retention: CaptureRetention,
    abort_on_limit: bool,
    worker: CaptureWorker,
) -> CaptureHandle
where
    R: Read + AsRawFd + Send + 'static,
{
    let reader_descriptor = reader.as_raw_fd();
    let CaptureWorker {
        stream,
        observation,
        failure_observer,
        failure,
        wake,
        observer_wake,
    } = worker;
    let selected = observation.is_some();
    let failure_supervised = failure_observer.is_some();
    let (interrupt, interrupt_receiver) = if selected {
        (None, None)
    } else {
        let (sender, receiver) = UnixDatagram::pair()
            .expect("capture interruption descriptors are created before the worker");
        (Some(sender), Some(receiver))
    };
    let state = new_capture_state(limit, retention, selected);
    let worker_state = Arc::clone(&state);
    let worker = std::thread::spawn(move || {
        let mut framer = observation.map(|observation| {
            SelectedFramer::new(
                observation,
                Arc::clone(&failure),
                Arc::clone(&worker_state),
                observer_wake.expect("selected capture has a preallocated observer wake"),
                failure_observer
                    .as_ref()
                    .map(Arc::clone)
                    .expect("selected capture has a failure observer"),
            )
        });
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            match wait_to_read(reader_descriptor, interrupt_receiver.as_ref()) {
                Ok(true) => {}
                Ok(false) => return,
                Err(error) => {
                    record_reader_failure(
                        stream,
                        error,
                        &worker_state,
                        failure_observer.as_ref(),
                        &failure,
                    );
                    let _wake_result = wake.send(WAKE_BYTE);
                    return;
                }
            }
            match reader.read(&mut buffer) {
                Ok(0) => {
                    if let Some(framer) = &mut framer {
                        framer.finish();
                    }
                    if let Ok(mut state) = worker_state.lock() {
                        state.output.complete = true;
                    }
                    let _wake_result = wake.send(WAKE_BYTE);
                    return;
                }
                Ok(read) => {
                    let overflow = retain_chunk(
                        &worker_state,
                        &buffer[..read],
                        limit,
                        retention,
                        abort_on_limit,
                    );
                    if overflow {
                        super::fail_capture(&failure);
                        return;
                    }
                    if let Some(framer) = &mut framer
                        && !framer.push(&buffer[..read])
                    {
                        return;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    record_reader_failure(
                        stream,
                        error,
                        &worker_state,
                        failure_observer.as_ref(),
                        &failure,
                    );
                    let _wake_result = wake.send(WAKE_BYTE);
                    return;
                }
            }
        }
    });

    CaptureHandle {
        state,
        failure_supervised,
        interrupt,
        // Opaque capture is wholly owned by Sill and can therefore be joined
        // after process-group cleanup. A selected capture may still be inside
        // caller-owned observer code whose separate deadline and failure path
        // deliberately prevent it from bounding process cleanup.
        worker: (!selected).then_some(worker),
    }
}

fn new_capture_state(
    limit: usize,
    retention: CaptureRetention,
    selected: bool,
) -> Arc<Mutex<CaptureState>> {
    Arc::new(Mutex::new(CaptureState {
        output: CapturedOutput {
            bytes: Vec::with_capacity(limit.min(8 * 1024)),
            truncated: false,
            complete: false,
            retains_suffix: retention == CaptureRetention::Suffix,
        },
        failure: None,
        observer_complete: !selected,
    }))
}

fn retain_chunk(
    state: &Mutex<CaptureState>,
    bytes: &[u8],
    limit: usize,
    retention: CaptureRetention,
    abort_on_limit: bool,
) -> bool {
    state.lock().is_ok_and(|mut state| {
        retain(&mut state.output, bytes, limit, retention);
        if abort_on_limit && state.output.truncated {
            state.output.complete = true;
            return true;
        }
        false
    })
}

fn retain(output: &mut CapturedOutput, bytes: &[u8], limit: usize, retention: CaptureRetention) {
    if retention == CaptureRetention::Prefix {
        let available = limit.saturating_sub(output.bytes.len());
        let retained = available.min(bytes.len());
        output.bytes.extend_from_slice(&bytes[..retained]);
        output.truncated |= retained < bytes.len();
        return;
    }
    if bytes.len() >= limit {
        output.truncated |= !output.bytes.is_empty() || bytes.len() > limit;
        output.bytes.clear();
        output
            .bytes
            .extend_from_slice(&bytes[bytes.len().saturating_sub(limit)..]);
        return;
    }
    let overflow = output
        .bytes
        .len()
        .saturating_add(bytes.len())
        .saturating_sub(limit);
    if overflow > 0 {
        output.bytes.drain(..overflow);
        output.truncated = true;
    }
    output.bytes.extend_from_slice(bytes);
}

fn wait_to_read(reader_descriptor: RawFd, interrupt: Option<&UnixDatagram>) -> io::Result<bool> {
    let Some(interrupt) = interrupt else {
        return Ok(true);
    };
    let mut descriptors = [
        platform::PollFd::read(reader_descriptor),
        platform::PollFd::read(interrupt.as_raw_fd()),
    ];
    loop {
        let result = unsafe {
            platform::poll(
                descriptors.as_mut_ptr(),
                platform::Nfds::try_from(descriptors.len()).expect("two descriptors fit"),
                -1,
            )
        };
        if result >= 0 {
            if descriptors[1].ready() {
                return Ok(false);
            }
            if descriptors[0].ready() {
                return Ok(true);
            }
            continue;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn record_reader_failure(
    stream: OutputStream,
    error: io::Error,
    state: &Mutex<CaptureState>,
    failure_observer: Option<&Arc<dyn RecordObserver>>,
    failure: &FailureContext,
) {
    let captured = IoFailure::capture(error);
    if let Ok(mut state) = state.lock() {
        state.failure = Some(captured.clone());
    }
    if let Some(observer) = failure_observer {
        fail_observation(
            failure,
            observer,
            ObservationFailure::Reader {
                stream,
                detail: captured.detail,
            },
        );
    }
}

struct SelectedFramer {
    stream: OutputStream,
    limit: usize,
    capacity: usize,
    sink_deadline: Duration,
    record: Vec<u8>,
    discarding: bool,
    sender: FrameSender<Frame>,
    observer: Arc<dyn RecordObserver>,
    failure: Arc<FailureContext>,
}

impl SelectedFramer {
    fn new(
        observation: Observation,
        failure: Arc<FailureContext>,
        state: Arc<Mutex<CaptureState>>,
        wake: std::os::unix::net::UnixDatagram,
        observer: Arc<dyn RecordObserver>,
    ) -> Self {
        let stream = observation.stream();
        let limit = observation.frame_limit();
        let capacity = observation.queue_capacity();
        let sink_deadline = observation.sink_deadline();
        let (sender, receiver) = frame_queue::bounded(capacity);
        start_observer_worker(
            receiver,
            ObserverWorker {
                stream,
                capacity,
                sink_deadline,
                observer: Arc::clone(&observer),
                failure: Arc::clone(&failure),
                state,
                wake,
            },
        );
        Self {
            stream,
            limit,
            capacity,
            sink_deadline,
            record: Vec::with_capacity(limit.min(8 * 1024)),
            discarding: false,
            sender,
            observer,
            failure,
        }
    }

    fn push(&mut self, bytes: &[u8]) -> bool {
        let mut rest = bytes;
        loop {
            if self.discarding {
                let Some(index) = rest.iter().position(|byte| *byte == b'\n') else {
                    return true;
                };
                self.discarding = false;
                rest = &rest[index + 1..];
            }
            let Some(index) = rest.iter().position(|byte| *byte == b'\n') else {
                return self.extend_or_truncate(rest, false);
            };
            if !self.extend_or_truncate(&rest[..=index], true) {
                return false;
            }
            rest = &rest[index + 1..];
        }
    }

    /// Admits one segment into the pending record, truncating at the limit.
    ///
    /// An oversized record is delivered as its bounded head under
    /// [`RecordBoundary::Truncated`]; the remainder through the closing newline
    /// is discarded and the next record frames normally. The limit is a
    /// per-record admission bound, not a failure of the stream, so capture
    /// survives and nothing beyond the limit is ever buffered.
    fn extend_or_truncate(&mut self, segment: &[u8], delimited: bool) -> bool {
        let available = self.limit.saturating_sub(self.record.len());
        if segment.len() > available {
            self.record.extend_from_slice(&segment[..available]);
            self.discarding = !delimited;
            return self.send(RecordBoundary::Truncated);
        }
        self.record.extend_from_slice(segment);
        if delimited {
            return self.send(RecordBoundary::Complete);
        }
        true
    }

    fn finish(&mut self) {
        if !self.record.is_empty() && !self.send(RecordBoundary::Terminal) {
            return;
        }
        let _accepted = self.submit(Frame::End);
    }

    fn send(&mut self, boundary: RecordBoundary) -> bool {
        let frame = Frame::Record {
            bytes: std::mem::take(&mut self.record),
            boundary,
        };
        self.submit(frame)
    }

    fn submit(&self, frame: Frame) -> bool {
        let started = Instant::now();
        match self.sender.send_by(frame, started, self.sink_deadline) {
            Ok(()) => true,
            Err(SendError::Full(_)) => {
                self.fail(ObservationFailure::SinkBackpressure {
                    stream: self.stream,
                    capacity: self.capacity,
                    deadline: self.sink_deadline,
                    stall: Box::new(ObservationStall {
                        elapsed: started.elapsed(),
                        waiting: "queue admission",
                        stage: self.observer.stage(),
                    }),
                });
                false
            }
            Err(SendError::Disconnected(_)) => {
                self.fail(ObservationFailure::ObserverLost {
                    stream: self.stream,
                });
                false
            }
        }
    }

    fn fail(&self, failure: ObservationFailure) {
        fail_observation(&self.failure, &self.observer, failure);
    }
}

struct ObserverWorker {
    stream: OutputStream,
    capacity: usize,
    sink_deadline: Duration,
    observer: Arc<dyn RecordObserver>,
    failure: Arc<FailureContext>,
    state: Arc<Mutex<CaptureState>>,
    wake: std::os::unix::net::UnixDatagram,
}

fn start_observer_worker(receiver: FrameReceiver<Frame>, worker: ObserverWorker) {
    let ObserverWorker {
        stream,
        capacity,
        sink_deadline,
        observer,
        failure,
        state,
        wake,
    } = worker;
    let (requests, callbacks) = mpsc::sync_channel::<Frame>(0);
    let (results, result_receiver) = mpsc::sync_channel(0);
    let callback_observer = Arc::clone(&observer);
    std::thread::spawn(move || {
        for frame in callbacks {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match frame {
                Frame::Record { bytes, boundary } => {
                    callback_observer.observe(ObservedRecord::new(&bytes, boundary))
                }
                Frame::End => callback_observer.finish(),
            }));
            let result = match result {
                Ok(Ok(())) => ObserverResult::Accepted,
                Ok(Err(error)) => ObserverResult::Refused(error),
                Err(_) => ObserverResult::Lost,
            };
            if results.send(result).is_err() {
                return;
            }
        }
    });
    std::thread::spawn(move || {
        for frame in receiver {
            if requests.send(frame).is_err() {
                fail_observation(
                    &failure,
                    &observer,
                    ObservationFailure::ObserverLost { stream },
                );
                break;
            }
            let started = Instant::now();
            match result_receiver.recv_timeout(sink_deadline) {
                Ok(ObserverResult::Accepted) => {}
                Ok(ObserverResult::Refused(error)) => {
                    fail_observation(
                        &failure,
                        &observer,
                        ObservationFailure::SinkRefused {
                            stream,
                            detail: error.detail().to_owned(),
                        },
                    );
                    break;
                }
                Ok(ObserverResult::Lost) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    fail_observation(
                        &failure,
                        &observer,
                        ObservationFailure::ObserverLost { stream },
                    );
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    fail_observation(
                        &failure,
                        &observer,
                        ObservationFailure::SinkBackpressure {
                            stream,
                            capacity,
                            deadline: sink_deadline,
                            stall: Box::new(ObservationStall {
                                elapsed: started.elapsed(),
                                waiting: "callback completion",
                                stage: observer.stage(),
                            }),
                        },
                    );
                    break;
                }
            }
        }
        if let Ok(mut state) = state.lock() {
            state.observer_complete = true;
        }
        let _wake_result = wake.send(WAKE_BYTE);
    });
}

enum ObserverResult {
    Accepted,
    Refused(crate::observe::ObserveError),
    Lost,
}
