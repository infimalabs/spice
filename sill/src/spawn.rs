#[cfg(unix)]
use crate::observe::RecordObserver;
use crate::observe::{Observation, ObservationFailure};
#[cfg(unix)]
use crate::spec::ProcessGroup;
use crate::spec::Spec;
#[cfg(unix)]
use crate::surface::process_state;
use std::error::Error;
use std::fmt;
#[cfg(unix)]
use std::fs::File;
use std::io;
#[cfg(unix)]
use std::io::Write as _;
use std::process::ExitStatus;
#[cfg(unix)]
use std::process::{Child, ChildStdin};
#[cfg(unix)]
use std::sync::atomic::Ordering;
#[cfg(unix)]
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

#[cfg(unix)]
const WAKE_BYTE: &[u8] = &[1];
#[cfg(unix)]
const GROUP_POLL: Duration = Duration::from_millis(10);

#[cfg(unix)]
use capture::{CaptureHandle, FailureContext};
#[cfg(unix)]
use launch::spawn_inner;
#[cfg(windows)]
pub use windows::{OwnedChild, Running};
#[cfg(windows)]
pub(crate) use windows::{group_members, signal_group};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
    complete: bool,
    retains_suffix: bool,
}

impl CapturedOutput {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn complete(&self) -> bool {
        self.complete
    }

    /// Whether truncated bytes retain the end rather than the beginning.
    pub fn retains_suffix(&self) -> bool {
        self.retains_suffix
    }
}

#[derive(Debug)]
pub enum Completion {
    Exited(ExitStatus),
    TimedOut(ExitStatus),
    Cancelled(ExitStatus),
}

impl Completion {
    pub fn status(&self) -> ExitStatus {
        match self {
            Self::Exited(status) | Self::TimedOut(status) | Self::Cancelled(status) => *status,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingAction {
    Wait,
    Cancel,
    Timeout,
}

const fn pending_action(cancelled: bool, timed_out: bool) -> PendingAction {
    if cancelled {
        PendingAction::Cancel
    } else if timed_out {
        PendingAction::Timeout
    } else {
        PendingAction::Wait
    }
}

#[derive(Debug)]
pub struct WaitOutcome {
    completion: Completion,
    usage: ProcessUsage,
    observation_failure: Option<ObservationFailure>,
    stdout: Option<CapturedOutput>,
    stderr: Option<CapturedOutput>,
    outputs_drained: bool,
}

/// Kernel-accounted CPU consumed by one reaped process and its waited children.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProcessUsage {
    user_cpu: Duration,
    system_cpu: Duration,
}

impl ProcessUsage {
    /// Construct one normalized user/system CPU accounting record.
    #[must_use]
    pub const fn new(user_cpu: Duration, system_cpu: Duration) -> Self {
        Self {
            user_cpu,
            system_cpu,
        }
    }

    /// CPU time spent executing application code.
    #[must_use]
    pub const fn user_cpu(self) -> Duration {
        self.user_cpu
    }

    /// CPU time spent executing kernel code on the process tree's behalf.
    #[must_use]
    pub const fn system_cpu(self) -> Duration {
        self.system_cpu
    }

    #[cfg(unix)]
    fn from_rusage(usage: libc::rusage) -> Self {
        Self::new(
            duration_from_timeval(usage.ru_utime),
            duration_from_timeval(usage.ru_stime),
        )
    }
}

/// One bounded passive observation of a running child.
#[derive(Debug)]
pub enum WaitStep {
    /// The process exited within the observation bound and was reaped.
    Exited(WaitOutcome),
    /// The bound elapsed while the process remained live.
    Running(Box<Running>),
}

impl WaitOutcome {
    /// Reconstruct one externally supervised typed completion.
    ///
    /// The detached supervisor owns the cause classification even though it
    /// cannot preserve local capture or resource-accounting evidence.
    #[must_use]
    pub fn externally_reaped_completion(completion: Completion) -> Self {
        Self {
            completion,
            usage: ProcessUsage::default(),
            observation_failure: None,
            stdout: None,
            stderr: None,
            outputs_drained: true,
        }
    }

    pub fn completion(&self) -> &Completion {
        &self.completion
    }

    /// Kernel-accounted resource usage captured atomically with child reaping.
    #[must_use]
    pub const fn usage(&self) -> ProcessUsage {
        self.usage
    }

    pub fn stdout(&self) -> Option<&CapturedOutput> {
        self.stdout.as_ref()
    }

    pub fn stderr(&self) -> Option<&CapturedOutput> {
        self.stderr.as_ref()
    }

    /// Selected-stream failure that forced process cleanup.
    #[must_use]
    pub const fn observation_failure(&self) -> Option<&ObservationFailure> {
        self.observation_failure.as_ref()
    }

    /// Whether every capture finished before the output-drain deadline.
    ///
    /// A false answer means the drain was abandoned with a reader or an
    /// observer still working, so the bytes below and every record the
    /// observer was to report are partial. Nothing else here carries that:
    /// [`CapturedOutput::complete`] says only that the reader reached end of
    /// output, which an observer still draining its queue leaves true, and
    /// [`Self::observation_failure`] names an observer that broke rather than
    /// one that was merely still going when the deadline arrived.
    #[must_use]
    pub const fn outputs_drained(&self) -> bool {
        self.outputs_drained
    }
}

#[derive(Debug)]
pub enum WaitError {
    Io {
        owner: String,
        operation: &'static str,
        source: io::Error,
    },
    ReapDeadlineExceeded {
        owner: String,
        deadline: Duration,
    },
}

impl fmt::Display for WaitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                owner,
                operation,
                source,
            } => write!(
                formatter,
                "wait owned by {owner:?} failed while {operation}: {source}"
            ),
            Self::ReapDeadlineExceeded { owner, deadline } => write!(
                formatter,
                "wait owned by {owner:?} exceeded its {deadline:?} reap deadline"
            ),
        }
    }
}

impl Error for WaitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::ReapDeadlineExceeded { .. } => None,
        }
    }
}

#[cfg(unix)]
#[derive(Debug)]
pub struct Running {
    pid: u32,
    spec: Spec,
    started: Instant,
    wake_receiver: std::os::unix::net::UnixDatagram,
    cancellation_wake: std::os::unix::net::UnixDatagram,
    child: Option<Child>,
    stdin: Option<InputHandle>,
    process: Arc<ProcessState>,
    stdout: Option<CaptureHandle>,
    stderr: Option<CaptureHandle>,
    guardian: Option<guardian::Guardian>,
}

#[cfg(unix)]
#[derive(Debug)]
struct InputHandle {
    result: Arc<Mutex<Option<Result<(), IoFailure>>>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl InputHandle {
    fn start(
        mut input: ChildStdin,
        bytes: Vec<u8>,
        wake: std::os::unix::net::UnixDatagram,
    ) -> Self {
        let result = Arc::new(Mutex::new(None));
        let worker_result = Arc::clone(&result);
        let worker = std::thread::spawn(move || {
            let written = input.write_all(&bytes).map_err(IoFailure::capture);
            drop(input);
            if let Ok(mut result) = worker_result.lock() {
                *result = Some(written);
            }
            let _wake_result = wake.send(WAKE_BYTE);
        });
        Self {
            result,
            worker: Some(worker),
        }
    }

    fn failure(&self) -> io::Result<()> {
        self.result
            .lock()
            .map_err(|_| io::Error::other("input state lock is poisoned"))?
            .as_ref()
            .map_or(Ok(()), |result| {
                result.as_ref().map_err(IoFailure::error).map(|_| ())
            })
    }

    fn finish(mut self) -> io::Result<()> {
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| io::Error::other("input worker panicked"))?;
        }
        self.failure()
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct ProcessState {
    completion: Mutex<ProcessCompletion>,
    exited: Condvar,
}

#[cfg(unix)]
#[derive(Debug, Default)]
struct ProcessCompletion {
    notice: Option<Result<(), IoFailure>>,
    status: Option<ExitStatus>,
    usage: Option<ProcessUsage>,
    taken: bool,
    observation_failure: Option<ObservationFailure>,
    cleanup_failure: Option<IoFailure>,
}

#[cfg(unix)]
#[derive(Clone, Debug)]
struct IoFailure {
    kind: io::ErrorKind,
    detail: String,
}

#[cfg(unix)]
impl IoFailure {
    fn capture(error: io::Error) -> Self {
        Self {
            kind: error.kind(),
            detail: error.to_string(),
        }
    }

    fn error(&self) -> io::Error {
        io::Error::new(self.kind, self.detail.clone())
    }
}

#[cfg(not(any(unix, windows)))]
#[derive(Debug)]
pub struct Running {
    spec: Spec,
}

#[cfg(unix)]
pub fn spawn(spec: Spec) -> io::Result<Running> {
    spawn_inner(spec, None, None, None, &mut |_| Ok(()))
}

#[cfg(windows)]
pub fn spawn(spec: Spec) -> io::Result<Running> {
    windows::spawn(spec, None)
}

/// Spawn an already-configured command beneath the shared Windows Job Object
/// boundary while leaving its stream handling to the caller.
///
/// # Errors
///
/// Returns an I/O error if the suspended child cannot be created, assigned, or
/// resumed. A refused launch is killed and reaped before this function returns.
#[cfg(windows)]
pub fn spawn_owned_command(
    command: &mut std::process::Command,
    group: crate::spec::ProcessGroup,
) -> io::Result<OwnedChild> {
    windows::spawn_command(command, group)
}

/// Spawn a consumer that inherits an already-held resource lock.
///
/// The inherited open description keeps possession until the consumer and
/// its descendants close it, including when the spawning parent exits first.
/// Only this validated descriptor has close-on-exec cleared in the child.
///
/// # Errors
///
/// Returns an I/O error if observation configuration, descriptor inheritance,
/// child creation, or capture setup fails.
#[cfg(unix)]
pub fn spawn_retaining(
    spec: Spec,
    observation: Option<Observation>,
    possession: &mut crate::Possession,
) -> io::Result<Running> {
    spawn_inner(spec, observation, Some(possession), None, &mut |_| Ok(()))
}

/// Spawn one child whose possession outlives this process, publishing the
/// protected group and its guardian the moment both exist.
///
/// `publish` is called with the child's process group and the guardian holding
/// possession, at the first instant both exist, and a failure there refuses the
/// spawn rather than leaving possession unnamed. The child is already running by
/// then and cannot be held back for it, so `publish` races the child's own first
/// moments and must not be read as happening before them.
#[cfg(unix)]
pub(crate) fn spawn_guarded(
    spec: Spec,
    file: &File,
    arm: &mut dyn FnMut(u32) -> io::Result<()>,
) -> io::Result<Running> {
    spawn_inner(spec, None, None, Some(file), arm)
}

/// Spawn one child with a bounded selected-stream observer.
///
/// The selected stream and every other captured stream are drained from child
/// creation onward, independently of calls to [`Running::wait`] or shutdown.
///
/// # Errors
///
/// Returns an I/O error when the selected stream is not configured for capture
/// or when the child and its capture workers cannot be created.
#[cfg(unix)]
pub fn spawn_observed(spec: Spec, observation: Observation) -> io::Result<Running> {
    spawn_inner(spec, Some(observation), None, None, &mut |_| Ok(()))
}

#[cfg(windows)]
pub fn spawn_observed(spec: Spec, observation: Observation) -> io::Result<Running> {
    windows::spawn(spec, Some(observation))
}

#[cfg(not(any(unix, windows)))]
pub fn spawn_observed(_spec: Spec, _observation: Observation) -> io::Result<Running> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process spawning requires a native release target",
    ))
}

#[cfg(not(any(unix, windows)))]
pub fn spawn(_spec: Spec) -> io::Result<Running> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process spawning requires a native release target",
    ))
}

#[cfg(unix)]
fn start_exit_watcher(
    pid: u32,
    waiter_wake: std::os::unix::net::UnixDatagram,
    process: Arc<ProcessState>,
    observer: Option<Arc<dyn RecordObserver>>,
) {
    std::thread::spawn(move || {
        let result = wait_for_exit(pid).map_err(IoFailure::capture);
        if let Ok(mut completion) = process.completion.lock() {
            completion.notice = Some(result.clone());
        }
        if result.is_ok()
            && let Some(observer) = observer
        {
            let _reported = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                observer.exited();
            }));
        }
        process.exited.notify_all();
        let _wake_result = waiter_wake.send(WAKE_BYTE);
    });
}

#[cfg(unix)]
fn wait_for_exit(pid: u32) -> io::Result<()> {
    loop {
        let mut information = std::mem::MaybeUninit::<platform::SignalInfo>::uninit();
        let result = unsafe {
            platform::waitid(
                platform::P_PID,
                pid,
                information.as_mut_ptr(),
                platform::WEXITED | platform::WNOWAIT,
            )
        };
        if result == 0 {
            return Ok(());
        }

        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(unix)]
impl Running {
    pub fn id(&self) -> u32 {
        self.pid
    }

    pub(crate) fn guardian_id(&self) -> Option<u32> {
        self.guardian.as_ref().map(guardian::Guardian::id)
    }

    pub fn spec(&self) -> &Spec {
        &self.spec
    }

    pub fn runtime_remaining(&self) -> Duration {
        self.spec
            .deadlines
            .runtime()
            .saturating_sub(self.started.elapsed())
    }

    /// Begin writing one complete input and close standard input after it.
    ///
    /// The handle is single-use so a process cannot wait on an accidentally
    /// retained writer after the declared request has been delivered.
    pub fn write_stdin(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.stdin.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "child stdin already owns a request",
            ));
        }
        let input = self
            .child
            .as_mut()
            .and_then(|child| child.stdin.take())
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "child stdin is not piped"))?;
        let wake = self.cancellation_wake.try_clone()?;
        self.stdin = Some(InputHandle::start(input, bytes.to_vec(), wake));
        Ok(())
    }

    /// Hand the caller the child's standard input for the whole process lifetime.
    ///
    /// [`Self::write_stdin`] closes standard input behind one declared request.
    /// A child driven by a command protocol keeps its input open for as long as
    /// it runs, so this hands the pipe out whole: the caller owns every write
    /// and the close, and dropping the returned writer is the end of input the
    /// child reads.
    ///
    /// # Errors
    ///
    /// Returns `BrokenPipe` when the spec did not pipe standard input, and
    /// `AlreadyExists` when a request or an earlier take already owns it.
    pub fn take_stdin(&mut self) -> io::Result<ChildStdin> {
        if self.stdin.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "child stdin already owns a request",
            ));
        }
        self.child
            .as_mut()
            .and_then(|child| child.stdin.take())
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "child stdin is not piped"))
    }

    pub fn wait_for(mut self, duration: Duration) -> Result<WaitStep, WaitError> {
        let started = Instant::now();
        loop {
            self.drain_outputs()?;
            if let Some(status) = self.take_execution_exit()? {
                let completion = self.completion(status)?;
                return self.finish(completion).map(WaitStep::Exited);
            }
            let elapsed = started.elapsed();
            if elapsed >= duration {
                return Ok(WaitStep::Running(Box::new(self)));
            }
            self.block_for(duration.saturating_sub(elapsed))?;
        }
    }

    pub fn signal_terminate(&self) -> Result<(), WaitError> {
        self.signal(platform::SIGTERM)
    }

    pub fn signal_kill(&self) -> Result<(), WaitError> {
        self.signal(platform::SIGKILL)
    }

    pub fn wait(mut self) -> Result<WaitOutcome, WaitError> {
        let cancellation_wake = self
            .cancellation_wake
            .try_clone()
            .map_err(|source| self.io_error("registering cancellation wake", source))?;
        let _cancellation_registration =
            self.spec
                .wait
                .cancellation()
                .register_waker(Arc::new(move || {
                    let _wake_result = cancellation_wake.send(WAKE_BYTE);
                }));

        loop {
            self.drain_outputs()?;

            if let Some(status) = self.take_child_exit()? {
                let completion = self.completion(status)?;
                return self.finish(completion);
            }

            let elapsed = self.started.elapsed();
            match pending_action(
                self.spec.wait.cancellation().is_cancelled(),
                elapsed >= self.spec.deadlines.runtime(),
            ) {
                PendingAction::Cancel => {
                    let status = self.terminate_and_reap()?;
                    return self.finish(Completion::Cancelled(status));
                }
                PendingAction::Timeout => {
                    let status = self.terminate_and_reap()?;
                    return self.finish(Completion::TimedOut(status));
                }
                PendingAction::Wait => {}
            }

            self.block_for(self.spec.deadlines.runtime().saturating_sub(elapsed))?;
        }
    }

    fn terminate_and_reap(&mut self) -> Result<ExitStatus, WaitError> {
        if let Some(status) = self.take_execution_exit()? {
            return Ok(status);
        }

        self.signal(platform::SIGTERM)?;
        if let Some(status) = self.await_execution_for(self.spec.grace)? {
            return Ok(status);
        }

        self.signal(platform::SIGKILL)?;
        if let Some(status) = self.await_execution_for(self.spec.deadlines.reap())? {
            return Ok(status);
        }

        Err(WaitError::ReapDeadlineExceeded {
            owner: self.spec.wait.owner().to_owned(),
            deadline: self.spec.deadlines.reap(),
        })
    }

    fn await_execution_for(&mut self, duration: Duration) -> Result<Option<ExitStatus>, WaitError> {
        let started = Instant::now();
        loop {
            if let Some(status) = self.take_execution_exit()? {
                return Ok(Some(status));
            }
            let elapsed = started.elapsed();
            if elapsed >= duration {
                return Ok(None);
            }
            self.block_for(duration.saturating_sub(elapsed))?;
        }
    }

    fn finish(mut self, completion: Completion) -> Result<WaitOutcome, WaitError> {
        let started = Instant::now();
        // Completion is checked before the deadline so a drain that finished
        // exactly as the bound arrived is reported as the completion it was.
        let outputs_drained = loop {
            self.drain_outputs()?;
            if self.outputs_complete() {
                break true;
            }
            if started.elapsed() >= self.spec.deadlines.output_drain() {
                break false;
            }
            self.block_for(
                self.spec
                    .deadlines
                    .output_drain()
                    .saturating_sub(started.elapsed()),
            )?;
        };

        let observation_failure = self.observation_failure()?;
        // Preserve the declared drain-bound answer before cleanup closes any
        // inherited descriptors. Cleanup may let a reader reach EOF, but it
        // cannot retroactively make output complete inside the earlier bound.
        let stdout = self
            .stdout
            .as_ref()
            .map(CaptureHandle::snapshot)
            .transpose()
            .map_err(|source| self.io_error("snapshotting stdout capture", source))?;
        let stderr = self
            .stderr
            .as_ref()
            .map(CaptureHandle::snapshot)
            .transpose()
            .map_err(|source| self.io_error("snapshotting stderr capture", source))?;
        // A child may exit while a descendant retains one of its pipes. End
        // that process group before joining the capture workers so the join is
        // a proof of cleanup rather than another unbounded wait.
        if !self.execution_group_terminal() {
            self.terminate_and_reap()?;
        }
        let input = self.stdin.take().map(InputHandle::finish).transpose();
        if matches!(completion, Completion::Exited(_)) {
            input.map_err(|source| self.io_error("finishing stdin", source))?;
        }
        self.stdout
            .take()
            .map(CaptureHandle::finish)
            .transpose()
            .map_err(|source| self.io_error("finishing stdout capture", source))?;
        self.stderr
            .take()
            .map(CaptureHandle::finish)
            .transpose()
            .map_err(|source| self.io_error("finishing stderr capture", source))?;
        let usage = self.process_usage()?;
        let outcome = WaitOutcome {
            completion,
            usage,
            observation_failure,
            stdout,
            stderr,
            outputs_drained,
        };
        // The guardian is joined here only for a group observed to have gone.
        // [`guardian::Guardian::wait`] blocks until the guardian itself observes
        // that absence, so joining on an answer that never established it would
        // park this caller on exactly the question the probe failed to settle.
        // A live or unreadable answer leaves the guardian to the detached reaper
        // it already owns, which collects it whenever the group does end.
        if guardian::group_state(self.pid) == process_state::ProcessState::Terminal
            && let Some(guardian) = self.guardian.take()
        {
            guardian
                .wait()
                .map_err(|source| self.io_error("reaping lock guardian", source))?;
        }
        Ok(outcome)
    }

    fn take_child_exit(&mut self) -> Result<Option<ExitStatus>, WaitError> {
        let notice = {
            let mut completion = self.process.completion.lock().map_err(|_| {
                self.io_error(
                    "reading child completion",
                    io::Error::other("child completion lock is poisoned"),
                )
            })?;
            if let Some(failure) = &completion.cleanup_failure {
                return Err(self.io_error("cleaning up failed observation", failure.error()));
            }
            if let Some(status) = completion.status {
                if !completion.taken {
                    completion.taken = true;
                    self.child.take();
                }
                return Ok(Some(status));
            }
            completion.notice.clone()
        };

        match notice {
            Some(Ok(())) => {
                reap_process(self.pid, &self.process)
                    .map_err(|source| self.io_error("reaping notified child", source))?;
                self.take_child_exit()
            }
            Some(Err(source)) => {
                Err(self.io_error("watching for child completion", source.error()))
            }
            None => Ok(None),
        }
    }

    fn take_execution_exit(&mut self) -> Result<Option<ExitStatus>, WaitError> {
        let Some(status) = self.take_child_exit()? else {
            return Ok(None);
        };
        if self.execution_group_terminal() {
            Ok(Some(status))
        } else {
            Ok(None)
        }
    }

    /// Decide whether the child's exit also settles the process group it led.
    ///
    /// This probe runs before [`Self::terminate_and_reap`] sends anything, so it
    /// cannot forgive an unreadable answer the way [`resolve_signal`] and
    /// [`wait_for_exit_notice`] do after a signal was already delivered. That
    /// would skip termination and could leak a live descendant. Instead, one
    /// immediate bounded re-probe gives a group racing toward absence a second
    /// chance to establish its end; a live or still-unreadable answer remains
    /// unsettled and enters the termination path. The bound avoids turning an
    /// unreadable kernel into an unbounded pre-kill wait, while the second read
    /// avoids spending the full SIGTERM grace and reap deadline when the first
    /// answer was transient and an already-ended group is now observable.
    fn execution_group_terminal(&self) -> bool {
        execution_group_terminal_against(self.requires_group_terminal(), || {
            process_state::process_group_state(self.pid)
        })
    }

    fn requires_group_terminal(&self) -> bool {
        self.spec.group == ProcessGroup::New && self.guardian.is_none()
    }

    fn completion(&self, status: ExitStatus) -> Result<Completion, WaitError> {
        Ok(Completion::Exited(status))
    }

    fn observation_failure(&self) -> Result<Option<ObservationFailure>, WaitError> {
        self.process
            .completion
            .lock()
            .map(|completion| completion.observation_failure.clone())
            .map_err(|_| {
                self.io_error(
                    "reading observation completion",
                    io::Error::other("child completion lock is poisoned"),
                )
            })
    }

    fn process_usage(&self) -> Result<ProcessUsage, WaitError> {
        self.process
            .completion
            .lock()
            .map_err(|_| {
                self.io_error(
                    "reading child resource usage",
                    io::Error::other("child completion lock is poisoned"),
                )
            })?
            .usage
            .ok_or_else(|| {
                self.io_error(
                    "reading child resource usage",
                    io::Error::other("reaped child has no resource usage"),
                )
            })
    }

    fn block_for(&mut self, duration: Duration) -> Result<(), WaitError> {
        use std::os::fd::AsRawFd;

        let duration = if self.requires_group_terminal()
            && self
                .process
                .completion
                .lock()
                .is_ok_and(|completion| completion.status.is_some())
        {
            duration.min(GROUP_POLL)
        } else {
            duration
        };
        let mut descriptors = Vec::with_capacity(1);
        descriptors.push(platform::PollFd::read(self.wake_receiver.as_raw_fd()));

        let result = unsafe {
            platform::poll(
                descriptors.as_mut_ptr(),
                platform::Nfds::try_from(descriptors.len()).expect("one poll descriptor fits"),
                poll_timeout(duration),
            )
        };
        if result == -1 {
            let source = io::Error::last_os_error();
            if source.kind() != io::ErrorKind::Interrupted {
                return Err(self.io_error("blocking on process events", source));
            }
        }

        self.drain_wake()?;
        self.drain_outputs()
    }

    fn drain_wake(&self) -> Result<(), WaitError> {
        let mut buffer = [0_u8; 32];
        loop {
            match self.wake_receiver.recv(&mut buffer) {
                Ok(_) => {}
                Err(source) if source.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
                Err(source) => return Err(self.io_error("draining process wake events", source)),
            }
        }
    }

    fn drain_outputs(&mut self) -> Result<(), WaitError> {
        if let Some(stdin) = &self.stdin {
            stdin.failure().map_err(|source| WaitError::Io {
                owner: self.spec.wait.owner().to_owned(),
                operation: "writing stdin",
                source,
            })?;
        }
        if let Some(stdout) = &self.stdout {
            stdout.failure().map_err(|source| WaitError::Io {
                owner: self.spec.wait.owner().to_owned(),
                operation: "draining stdout",
                source,
            })?;
        }
        if let Some(stderr) = &self.stderr {
            stderr.failure().map_err(|source| WaitError::Io {
                owner: self.spec.wait.owner().to_owned(),
                operation: "draining stderr",
                source,
            })?;
        }
        Ok(())
    }

    fn outputs_complete(&self) -> bool {
        self.stdout.as_ref().is_none_or(CaptureHandle::is_complete)
            && self.stderr.as_ref().is_none_or(CaptureHandle::is_complete)
    }

    fn signal(&self, signal: i32) -> Result<(), WaitError> {
        signal_pid(self.pid, self.spec.group, signal, self.spec.wait.owner())
    }

    fn io_error(&self, operation: &'static str, source: io::Error) -> WaitError {
        WaitError::Io {
            owner: self.spec.wait.owner().to_owned(),
            operation,
            source,
        }
    }
}

/// The pre-kill group decision, separated so a fixture can supply the answer
/// the kernel cannot be made to give on demand.
#[cfg(unix)]
fn execution_group_terminal_against(
    requires_group_terminal: bool,
    mut observe: impl FnMut() -> process_state::ProcessState,
) -> bool {
    if !requires_group_terminal {
        return true;
    }
    match observe() {
        process_state::ProcessState::Terminal => true,
        process_state::ProcessState::Live => false,
        process_state::ProcessState::Indeterminate => {
            matches!(observe(), process_state::ProcessState::Terminal)
        }
    }
}

#[cfg(not(any(unix, windows)))]
impl Running {
    pub fn id(&self) -> u32 {
        unreachable!("unsupported targets cannot construct Running")
    }

    pub fn spec(&self) -> &Spec {
        &self.spec
    }

    pub fn runtime_remaining(&self) -> Duration {
        self.spec.deadlines.runtime()
    }

    pub fn write_stdin(&mut self, _bytes: &[u8]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process input requires a native release target",
        ))
    }

    pub fn take_stdin(&mut self) -> io::Result<std::process::ChildStdin> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process input requires a native release target",
        ))
    }

    pub fn wait_for(self, _duration: Duration) -> Result<WaitStep, WaitError> {
        Err(WaitError::Io {
            owner: self.spec.wait.owner().to_owned(),
            operation: "waiting for a process on an unsupported target",
            source: io::Error::new(
                io::ErrorKind::Unsupported,
                "process waiting requires a native release target",
            ),
        })
    }

    pub fn signal_terminate(&self) -> Result<(), WaitError> {
        Err(WaitError::Io {
            owner: self.spec.wait.owner().to_owned(),
            operation: "signalling a process on an unsupported target",
            source: io::Error::new(
                io::ErrorKind::Unsupported,
                "process signalling requires a native release target",
            ),
        })
    }

    pub fn signal_kill(&self) -> Result<(), WaitError> {
        self.signal_terminate()
    }

    pub fn wait(self) -> Result<WaitOutcome, WaitError> {
        Err(WaitError::Io {
            owner: self.spec.wait.owner().to_owned(),
            operation: "waiting for a process on an unsupported target",
            source: io::Error::new(
                io::ErrorKind::Unsupported,
                "process waiting requires a native release target",
            ),
        })
    }
}

#[cfg(unix)]
impl Drop for Running {
    fn drop(&mut self) {
        let complete = self.take_execution_exit().ok().flatten().is_some();
        if !complete {
            let _signal_result = self.signal(platform::SIGKILL);
            let _reap_result = self.await_execution_for(self.spec.deadlines.reap());
        }
        let _stdin = self.stdin.take().and_then(|input| input.finish().ok());
        // Opaque capture workers are Sill-owned and joinable after the group
        // cleanup above. Selected observers intentionally detach in
        // `CaptureHandle::finish`, so caller code cannot make this drop block.
        let _stdout = self.stdout.take().and_then(|capture| capture.finish().ok());
        let _stderr = self.stderr.take().and_then(|capture| capture.finish().ok());
    }
}

#[cfg(unix)]
fn fail_capture(context: &FailureContext) {
    if context.failed.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Err(error) = abort_process(context)
        && let Ok(mut completion) = context.process.completion.lock()
    {
        completion.cleanup_failure = Some(error);
    }
    let _wake_result = context.wake.send(WAKE_BYTE);
}

#[cfg(unix)]
fn fail_observation(
    context: &FailureContext,
    observer: &Arc<dyn RecordObserver>,
    failure: ObservationFailure,
) {
    if context.failed.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Ok(mut completion) = context.process.completion.lock() {
        completion.observation_failure = Some(failure.clone());
    }

    if let Err(error) = abort_process(context)
        && let Ok(mut completion) = context.process.completion.lock()
    {
        completion.cleanup_failure = Some(error);
    }
    let _wake_result = context.wake.send(WAKE_BYTE);
    let _reported = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        observer.failed(&failure);
    }));
}

#[cfg(unix)]
fn abort_process(context: &FailureContext) -> Result<(), IoFailure> {
    signal_pid(
        context.pid,
        context.group,
        platform::SIGTERM,
        &context.owner,
    )
    .map_err(|error| IoFailure {
        kind: io::ErrorKind::Other,
        detail: error.to_string(),
    })?;
    if wait_for_exit_notice(
        context.pid,
        context.group,
        &context.process,
        context.grace,
        process_state::process_group_state,
    )? {
        reap_process(context.pid, &context.process).map_err(IoFailure::capture)?;
        return Ok(());
    }

    signal_pid(
        context.pid,
        context.group,
        platform::SIGKILL,
        &context.owner,
    )
    .map_err(|error| IoFailure {
        kind: io::ErrorKind::Other,
        detail: error.to_string(),
    })?;
    if wait_for_exit_notice(
        context.pid,
        context.group,
        &context.process,
        context.reap,
        process_state::process_group_state,
    )? {
        reap_process(context.pid, &context.process).map_err(IoFailure::capture)?;
        return Ok(());
    }

    Err(IoFailure {
        kind: io::ErrorKind::TimedOut,
        detail: format!(
            "observation cleanup exceeded its {:?} reap deadline",
            context.reap
        ),
    })
}

/// Wait for the child's exit notice, and for the group it leads to settle.
///
/// The group half reads the same evidence rule the signal path uses: only a
/// member the kernel described as running keeps this waiting. An unreadable
/// probe is not a survivor, so it no longer holds the wait open until the
/// deadline expires and reports a descendant that was never observed to exist.
/// Both paths can share that rule because both are reached only after the
/// signal was already delivered, so forgiving an unread kernel ends a wait and
/// never skips a kill. [`Running::execution_group_terminal`] reads the same
/// probe before the kill rather than after, so it re-probes once and treats a
/// live or still-unreadable answer as grounds to enter termination.
#[cfg(unix)]
fn wait_for_exit_notice(
    pid: u32,
    group: ProcessGroup,
    process: &ProcessState,
    duration: Duration,
    observe: impl Fn(u32) -> process_state::ProcessState,
) -> Result<bool, IoFailure> {
    let started = Instant::now();
    let mut completion = process.completion.lock().map_err(|_| IoFailure {
        kind: io::ErrorKind::Other,
        detail: "child completion lock is poisoned".to_owned(),
    })?;

    loop {
        if let Some(Err(failure)) = &completion.notice {
            return Err(failure.clone());
        }
        let child_exited =
            completion.status.is_some() || matches!(completion.notice.as_ref(), Some(Ok(())));
        if child_exited
            && (group != ProcessGroup::New
                || !matches!(observe(pid), process_state::ProcessState::Live))
        {
            return Ok(true);
        }

        let elapsed = started.elapsed();
        if elapsed >= duration {
            return Ok(false);
        }
        let remaining = duration.saturating_sub(elapsed);
        let wait = if child_exited && group == ProcessGroup::New {
            remaining.min(GROUP_POLL)
        } else {
            remaining
        };
        (completion, _) = process
            .exited
            .wait_timeout(completion, wait)
            .map_err(|_| IoFailure {
                kind: io::ErrorKind::Other,
                detail: "child completion lock is poisoned".to_owned(),
            })?;
    }
}

#[cfg(unix)]
fn reap_process(pid: u32, process: &ProcessState) -> io::Result<ExitStatus> {
    use std::os::unix::process::ExitStatusExt;

    let mut completion = process
        .completion
        .lock()
        .map_err(|_| io::Error::other("child completion lock is poisoned"))?;
    if let Some(status) = completion.status {
        return Ok(status);
    }
    if let Some(Err(failure)) = &completion.notice {
        return Err(failure.error());
    }
    let pid =
        i32::try_from(pid).map_err(|source| io::Error::new(io::ErrorKind::InvalidData, source))?;
    let mut raw = 0;
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    loop {
        // SAFETY: `pid` names our notified child, and both output pointers are
        // valid until `wait4` returns. This is the single reaping boundary.
        let result = unsafe { libc::wait4(pid, &mut raw, 0, usage.as_mut_ptr()) };
        if result == pid {
            let status = ExitStatus::from_raw(raw);
            // SAFETY: a successful `wait4` initialized the complete rusage.
            let usage = ProcessUsage::from_rusage(unsafe { usage.assume_init() });
            completion.status = Some(status);
            completion.usage = Some(usage);
            process.exited.notify_all();
            return Ok(status);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(unix)]
fn duration_from_timeval(time: libc::timeval) -> Duration {
    let seconds = u64::try_from(time.tv_sec).unwrap_or_default();
    let microseconds = u32::try_from(time.tv_usec).unwrap_or_default().min(999_999);
    Duration::new(seconds, microseconds * 1_000)
}

#[cfg(unix)]
fn signal_pid(pid: u32, group: ProcessGroup, signal: i32, owner: &str) -> Result<(), WaitError> {
    let signed = i32::try_from(pid).map_err(|source| WaitError::Io {
        owner: owner.to_owned(),
        operation: "converting child pid",
        source: io::Error::new(io::ErrorKind::InvalidData, source),
    })?;
    let target = match group {
        ProcessGroup::Inherit => signed,
        ProcessGroup::New => -signed,
    };
    // SAFETY: the target names this child or the group it was given, and the
    // signal is one of the two constants this module sends.
    let refused = unsafe { platform::kill(target, signal) } != 0;
    resolve_signal(refused.then(io::Error::last_os_error), pid, group, owner)
}

/// Answer a refused signal against the group it was aimed at.
///
/// ESRCH is the child that finished first, and every caller here already
/// tolerates one. A whole process group can say the same thing with EPERM: the
/// kernel walks the group, finds members, and refuses because not one of them
/// could be signalled -- which is what a group whose members are all exiting or
/// already reaped looks like from outside it. Reading that as a fault made a
/// child's ordinary teardown fail the caller that asked for it.
///
/// A real permission failure leaves a live member behind, so the group is asked
/// rather than assumed, and only a group with nothing left to signal is
/// forgiven. Nothing softens a refusal about a single process: one process
/// cannot be partway through becoming absent, so EPERM about one is about
/// permission. The refusal arrives already collected because the errno it
/// carries survives only until the next call, and the group must be probed
/// after it rather than before.
///
/// An unreadable probe is forgiven rather than re-probed, and that is a choice
/// about evidence rather than about patience. Only a member the kernel described
/// as running is a reason to call EPERM a permission fault; when the probe could
/// not read the kernel at all it has established nothing, and treating that
/// silence as a live member is what turned an ordinary teardown into a fault
/// under load. A bounded re-probe was rejected because the answer is unreadable
/// precisely when the machine is too busy to answer, so retrying spends the
/// teardown's remaining time to arrive at the same silence. The asymmetry
/// settles it: forgiving a genuine permission failure loses one report on a path
/// that is already tearing the child down, while faulting on an unread kernel
/// fails callers that did nothing wrong.
#[cfg(unix)]
fn resolve_signal(
    refusal: Option<io::Error>,
    pid: u32,
    group: ProcessGroup,
    owner: &str,
) -> Result<(), WaitError> {
    resolve_signal_against(refusal, group, owner, || {
        process_state::process_group_state(pid)
    })
}

/// The decision itself, separated so a fixture can hand it an answer the kernel
/// cannot be made to give on demand.
#[cfg(unix)]
fn resolve_signal_against(
    refusal: Option<io::Error>,
    group: ProcessGroup,
    owner: &str,
    observe: impl FnOnce() -> process_state::ProcessState,
) -> Result<(), WaitError> {
    let Some(source) = refusal else {
        return Ok(());
    };
    let forgiven = match source.raw_os_error() {
        Some(platform::ESRCH) => true,
        Some(platform::EPERM) => {
            group == ProcessGroup::New && !matches!(observe(), process_state::ProcessState::Live)
        }
        _ => false,
    };
    if forgiven {
        return Ok(());
    }
    Err(WaitError::Io {
        owner: owner.to_owned(),
        operation: "signalling child process group",
        source,
    })
}

#[cfg(unix)]
fn poll_timeout(duration: Duration) -> i32 {
    if duration.is_zero() {
        return 0;
    }

    let whole_milliseconds = duration.as_millis();
    let rounded_milliseconds =
        whole_milliseconds + u128::from(!duration.subsec_nanos().is_multiple_of(1_000_000));
    i32::try_from(rounded_milliseconds).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::{PendingAction, pending_action};

    #[test]
    fn pending_wait_prefers_cancellation_to_timeout() {
        assert_eq!(pending_action(false, false), PendingAction::Wait);
        assert_eq!(pending_action(false, true), PendingAction::Timeout);
        assert_eq!(pending_action(true, false), PendingAction::Cancel);
        assert_eq!(pending_action(true, true), PendingAction::Cancel);
    }
}

#[cfg(unix)]
mod capture;
mod frame_queue;
#[cfg(unix)]
mod guardian;
#[cfg(unix)]
mod launch;
#[cfg(unix)]
mod platform;
#[cfg(windows)]
mod windows;
