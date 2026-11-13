use super::frame_queue::{self, FrameReceiver, FrameSender, SendError};
use super::{
    CapturedOutput, Completion, PendingAction, WaitError, WaitOutcome, WaitStep, pending_action,
};
use crate::group::GroupSignal;
use crate::observe::{
    Observation, ObservationFailure, ObservationStall, ObservedRecord, OutputStream,
    RecordBoundary, RecordObserver,
};
use crate::spec::{ProcessGroup, Spec, StdinPolicy, StreamPolicy};
use std::fmt;
use std::io::{self, Read, Write as _};
use std::ops::{Deref, DerefMut};
use std::os::windows::io::AsHandle as _;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
};

use self::job::{Job, Wake, watch_exit, watch_job};

mod job;

const READ_BYTES: usize = 8 * 1024;

pub struct Running {
    pid: u32,
    spec: Spec,
    started: Instant,
    child: Option<Child>,
    root_status: Option<ExitStatus>,
    exit_reported: bool,
    stdin: Option<InputHandle>,
    stdout: Option<CaptureHandle>,
    stderr: Option<CaptureHandle>,
    job: Arc<Job>,
    failure: Arc<FailureState>,
    exit_observer: Option<Arc<dyn RecordObserver>>,
}

impl fmt::Debug for Running {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Running")
            .field("pid", &self.pid)
            .field("spec", &self.spec)
            .field("started", &self.started)
            .field("root_status", &self.root_status)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct InputHandle {
    result: Arc<Mutex<Option<io::Result<()>>>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl InputHandle {
    fn start(mut input: ChildStdin, bytes: Vec<u8>, wake: Arc<Wake>) -> Self {
        let result = Arc::new(Mutex::new(None));
        let worker_result = Arc::clone(&result);
        let worker = thread::spawn(move || {
            let written = input.write_all(&bytes);
            drop(input);
            if let Ok(mut result) = worker_result.lock() {
                *result = Some(written);
            }
            wake.notify();
        });
        Self {
            result,
            worker: Some(worker),
        }
    }

    fn failure(&self) -> io::Result<()> {
        let result = self
            .result
            .lock()
            .map_err(|_| io::Error::other("input state lock is poisoned"))?;
        result.as_ref().map_or(Ok(()), |result| {
            result.as_ref().map(|_| ()).map_err(clone_io)
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

#[derive(Debug, Default)]
struct FailureState {
    observation: Mutex<Option<ObservationFailure>>,
    cleanup: Mutex<Option<io::Error>>,
    failed: AtomicBool,
    wake: Arc<Wake>,
}

impl FailureState {
    fn fail_observation(
        &self,
        job: &Job,
        observer: &Arc<dyn RecordObserver>,
        failure: ObservationFailure,
    ) {
        if self.failed.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Ok(mut observed) = self.observation.lock() {
            *observed = Some(failure.clone());
        }
        if let Err(error) = job.terminate()
            && let Ok(mut cleanup) = self.cleanup.lock()
        {
            *cleanup = Some(error);
        }
        let _reported = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            observer.failed(&failure);
        }));
        self.wake.notify();
    }

    fn fail_cleanup(&self, error: io::Error) {
        if let Ok(mut cleanup) = self.cleanup.lock() {
            *cleanup = Some(error);
        }
        self.wake.notify();
    }
}

/// A preconfigured command whose complete Windows descendant tree belongs to
/// one kill-on-close Job Object.
#[derive(Debug)]
pub struct OwnedChild {
    child: Option<Child>,
    job: Arc<Job>,
}

impl OwnedChild {
    fn into_parts(mut self) -> (Child, Arc<Job>) {
        let child = self
            .child
            .take()
            .expect("an owned child yields its process exactly once");
        (child, Arc::clone(&self.job))
    }

    /// Terminate every process owned by this child's kill-on-close Job Object.
    pub fn terminate_tree(&self) -> io::Result<()> {
        self.job.terminate()
    }
}

impl Deref for OwnedChild {
    type Target = Child;

    fn deref(&self) -> &Self::Target {
        self.child
            .as_ref()
            .expect("an owned child retains its process")
    }
}

impl DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.child
            .as_mut()
            .expect("an owned child retains its process")
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _terminated = self.job.terminate();
            let _reaped = child.wait();
        }
    }
}

pub(super) fn spawn_command(command: &mut Command, group: ProcessGroup) -> io::Result<OwnedChild> {
    spawn_command_with(command, group, false)
}

/// Spawn `command` suspended, place it in a fresh job, and resume it; with
/// `notices`, the job reports its process messages through a completion port.
fn spawn_command_with(
    command: &mut Command,
    group: ProcessGroup,
    notices: bool,
) -> io::Result<OwnedChild> {
    use std::os::windows::process::CommandExt as _;

    let creation_group = if group == ProcessGroup::New {
        CREATE_NEW_PROCESS_GROUP
    } else {
        0
    };
    command.creation_flags(CREATE_SUSPENDED | creation_group);
    let mut child = command.spawn()?;
    let pid = child.id();
    // The child stays suspended until it is in the job, so nothing it starts
    // escapes; a new group's job takes its name from the root's id.
    let job = match Job::create((group == ProcessGroup::New).then_some(pid), notices) {
        Ok(job) => Arc::new(job),
        Err(error) => {
            let _killed = child.kill();
            let _reaped = child.wait();
            return Err(error);
        }
    };
    if let Err(error) = job.assign(&child) {
        let _killed = child.kill();
        let _reaped = child.wait();
        return Err(error);
    }
    if let Err(error) = resume_primary_thread(pid) {
        let _terminated = job.terminate();
        let _killed = child.kill();
        let _reaped = child.wait();
        return Err(error);
    }
    Ok(OwnedChild {
        child: Some(child),
        job,
    })
}

/// Deliver `signal` to the group a [`ProcessGroup::New`] spawn rooted at
/// `group`, through the job it named for the group.
pub(crate) fn signal_group(group: u32, signal: GroupSignal) -> io::Result<()> {
    let job = Job::open(group)?;
    if job.active_processes()? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "process group has no remaining process",
        ));
    }
    match signal {
        GroupSignal::Terminate => {
            // SAFETY: the event addresses only the console process group `group`.
            if unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, group) } == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
        GroupSignal::Kill => job.terminate(),
    }
}

/// The processes in the group a [`ProcessGroup::New`] spawn rooted at
/// `group`; none once the job named for the group has closed.
pub(crate) fn group_members(group: u32) -> io::Result<Vec<u32>> {
    match Job::open(group) {
        Ok(job) => job.process_ids(),
        Err(error) if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

pub(super) fn spawn(spec: Spec, observation: Option<Observation>) -> io::Result<Running> {
    validate_observation(&spec, observation.as_ref())?;
    let mut command = command(&spec);
    let owned = spawn_command_with(&mut command, spec.group, true)?;
    let (mut child, job) = owned.into_parts();
    let pid = child.id();
    let started = Instant::now();
    let failure = Arc::new(FailureState::default());
    let process = child.as_handle().try_clone_to_owned();
    let process = match process {
        Ok(process) => process,
        Err(error) => {
            let _terminated = job.terminate();
            let _reaped = child.wait();
            return Err(error);
        }
    };
    watch_exit(process, Arc::clone(&failure.wake));
    watch_job(Arc::clone(&job), Arc::clone(&failure.wake));
    let exit_observer = observation.as_ref().map(Observation::observer);
    let selected = observation.as_ref().map(Observation::stream);
    let (stdout_observation, stderr_observation) = match observation {
        Some(observation) if selected == Some(OutputStream::Stdout) => (Some(observation), None),
        Some(observation) if selected == Some(OutputStream::Stderr) => (None, Some(observation)),
        Some(_) => unreachable!("an observation always selects one output stream"),
        None => (None, None),
    };
    let stdout = take_stdout(&mut child, spec.output.stdout())?.map(|capture| {
        start_capture(
            capture,
            OutputStream::Stdout,
            spec.output.stdout(),
            stdout_observation,
            Arc::clone(&job),
            Arc::clone(&failure),
        )
    });
    let stderr = take_stderr(&mut child, spec.output.stderr())?.map(|capture| {
        start_capture(
            capture,
            OutputStream::Stderr,
            spec.output.stderr(),
            stderr_observation,
            Arc::clone(&job),
            Arc::clone(&failure),
        )
    });
    Ok(Running {
        pid,
        spec,
        started,
        child: Some(child),
        root_status: None,
        exit_reported: false,
        stdin: None,
        stdout,
        stderr,
        job,
        failure,
        exit_observer,
    })
}

fn command(spec: &Spec) -> Command {
    use std::os::windows::process::CommandExt as _;

    let mut command = Command::new(spec.argv.program());
    command
        .args(spec.argv.arguments())
        .current_dir(spec.cwd())
        .env_clear()
        .envs(spec.environment.entries().iter().cloned());
    match spec.output.stdin() {
        StdinPolicy::Inherit => command.stdin(Stdio::inherit()),
        StdinPolicy::Null => command.stdin(Stdio::null()),
        StdinPolicy::Pipe => command.stdin(Stdio::piped()),
    };
    configure_stream(&mut command, true, spec.output.stdout());
    configure_stream(&mut command, false, spec.output.stderr());
    let group = if spec.group == ProcessGroup::New {
        CREATE_NEW_PROCESS_GROUP
    } else {
        0
    };
    command.creation_flags(CREATE_SUSPENDED | group);
    command
}

fn resume_primary_thread(pid: u32) -> io::Result<()> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..THREADENTRY32::default()
        };
        let mut present = Thread32First(snapshot, &mut entry) != 0;
        let mut result = Err(io::Error::new(
            io::ErrorKind::NotFound,
            "suspended child has no primary thread",
        ));
        while present {
            if entry.th32OwnerProcessID == pid {
                let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                if thread.is_null() {
                    result = Err(io::Error::last_os_error());
                } else {
                    result = if ResumeThread(thread) == u32::MAX {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(())
                    };
                    CloseHandle(thread);
                }
                break;
            }
            present = Thread32Next(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        result
    }
}

impl Running {
    pub fn id(&self) -> u32 {
        self.pid
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
        self.stdin = Some(InputHandle::start(
            input,
            bytes.to_vec(),
            Arc::clone(&self.failure.wake),
        ));
        Ok(())
    }

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
            let seen = self.failure.wake.observe();
            self.drain_failures()?;
            if let Some(status) = self.take_execution_exit()? {
                return self
                    .finish(Completion::Exited(status))
                    .map(WaitStep::Exited);
            }
            let elapsed = started.elapsed();
            if elapsed >= duration {
                return Ok(WaitStep::Running(Box::new(self)));
            }
            self.block_past(seen, duration.saturating_sub(elapsed))?;
        }
    }

    pub fn signal_terminate(&self) -> Result<(), WaitError> {
        self.request_graceful()
            .map_err(|source| self.io_error("requesting graceful process termination", source))
    }

    pub fn signal_kill(&self) -> Result<(), WaitError> {
        self.job
            .terminate()
            .map_err(|source| self.io_error("terminating Windows Job Object", source))
    }

    pub fn wait(mut self) -> Result<WaitOutcome, WaitError> {
        let wake = Arc::clone(&self.failure.wake);
        let _cancellation_registration = self
            .spec
            .wait
            .cancellation()
            .register_waker(Arc::new(move || wake.notify()));
        loop {
            let seen = self.failure.wake.observe();
            self.drain_failures()?;
            if let Some(status) = self.take_execution_exit()? {
                return self.finish(Completion::Exited(status));
            }
            match pending_action(
                self.spec.wait.cancellation().is_cancelled(),
                self.started.elapsed() >= self.spec.deadlines.runtime(),
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
            self.block_past(seen, self.runtime_remaining())?;
        }
    }

    fn request_graceful(&self) -> io::Result<()> {
        if self.spec.group != ProcessGroup::New {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "graceful Windows interruption requires a new process group",
            ));
        }
        if unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, self.pid) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn terminate_and_reap(&mut self) -> Result<ExitStatus, WaitError> {
        if let Some(status) = self.take_execution_exit()? {
            return Ok(status);
        }
        let _graceful = self.request_graceful();
        if let Some(status) = self.await_execution_for(self.spec.grace)? {
            return Ok(status);
        }
        self.job
            .terminate()
            .map_err(|source| self.io_error("terminating Windows Job Object", source))?;
        self.await_execution_for(self.spec.deadlines.reap())?
            .ok_or_else(|| WaitError::ReapDeadlineExceeded {
                owner: self.spec.wait.owner().to_owned(),
                deadline: self.spec.deadlines.reap(),
            })
    }

    fn await_execution_for(&mut self, duration: Duration) -> Result<Option<ExitStatus>, WaitError> {
        let started = Instant::now();
        loop {
            let seen = self.failure.wake.observe();
            if let Some(status) = self.take_execution_exit()? {
                return Ok(Some(status));
            }
            let elapsed = started.elapsed();
            if elapsed >= duration {
                return Ok(None);
            }
            self.block_past(seen, duration.saturating_sub(elapsed))?;
        }
    }

    fn take_execution_exit(&mut self) -> Result<Option<ExitStatus>, WaitError> {
        if self.root_status.is_none() {
            let child = self.child.as_mut().ok_or_else(|| WaitError::Io {
                owner: self.spec.wait.owner().to_owned(),
                operation: "reading child state",
                source: io::Error::other("child was already reaped"),
            })?;
            self.root_status = child.try_wait().map_err(|source| WaitError::Io {
                owner: self.spec.wait.owner().to_owned(),
                operation: "observing child exit",
                source,
            })?;
        }
        if self.root_status.is_some() && !self.exit_reported {
            self.exit_reported = true;
            if let Some(observer) = &self.exit_observer {
                let _reported = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    observer.exited();
                }));
            }
        }
        if self.root_status.is_some()
            && self
                .job
                .active_processes()
                .map_err(|source| self.io_error("observing Windows Job Object", source))?
                == 0
        {
            Ok(self.root_status.take())
        } else {
            Ok(None)
        }
    }

    fn finish(mut self, completion: Completion) -> Result<WaitOutcome, WaitError> {
        let started = Instant::now();
        // Completion is checked before the deadline so a drain that finished
        // exactly as the bound arrived is reported as the completion it was.
        let outputs_drained = loop {
            let seen = self.failure.wake.observe();
            self.drain_failures()?;
            if self.outputs_complete() {
                break true;
            }
            let elapsed = started.elapsed();
            if elapsed >= self.spec.deadlines.output_drain() {
                break false;
            }
            self.block_past(
                seen,
                self.spec.deadlines.output_drain().saturating_sub(elapsed),
            )?;
        };
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
        let observation_failure = self
            .failure
            .observation
            .lock()
            .map_err(|_| {
                self.io_error(
                    "reading observation completion",
                    io::Error::other("observation state lock is poisoned"),
                )
            })?
            .clone();
        let usage = self
            .job
            .usage()
            .map_err(|source| self.io_error("reading Windows Job Object accounting", source))?;
        self.child.take();
        Ok(WaitOutcome {
            completion,
            usage,
            observation_failure,
            stdout,
            stderr,
            outputs_drained,
        })
    }

    fn drain_failures(&self) -> Result<(), WaitError> {
        if let Some(input) = &self.stdin {
            input
                .failure()
                .map_err(|source| self.io_error("writing stdin", source))?;
        }
        for (operation, capture) in [
            ("draining stdout", self.stdout.as_ref()),
            ("draining stderr", self.stderr.as_ref()),
        ] {
            if let Some(capture) = capture {
                capture
                    .failure()
                    .map_err(|source| self.io_error(operation, source))?;
            }
        }
        let mut cleanup = self.failure.cleanup.lock().map_err(|_| {
            self.io_error(
                "reading cleanup state",
                io::Error::other("cleanup state lock is poisoned"),
            )
        })?;
        if let Some(source) = cleanup.take() {
            return Err(self.io_error("cleaning failed observation", source));
        }
        Ok(())
    }

    fn outputs_complete(&self) -> bool {
        self.stdout.as_ref().is_none_or(CaptureHandle::is_complete)
            && self.stderr.as_ref().is_none_or(CaptureHandle::is_complete)
    }

    fn io_error(&self, operation: &'static str, source: io::Error) -> WaitError {
        WaitError::Io {
            owner: self.spec.wait.owner().to_owned(),
            operation,
            source,
        }
    }

    /// Block until an event source moves the wake past `seen` or `duration`
    /// elapses.
    fn block_past(&self, seen: u64, duration: Duration) -> Result<(), WaitError> {
        self.failure
            .wake
            .wait_past(seen, duration)
            .map_err(|source| self.io_error("waiting for the child", source))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.job.stop_notices();
        let _terminated = self.job.terminate();
        if let Some(child) = &mut self.child {
            let _reaped = child.wait();
        }
        let _input = self.stdin.take().and_then(|input| input.finish().ok());
        let _stdout = self.stdout.take().and_then(|capture| capture.finish().ok());
        let _stderr = self.stderr.take().and_then(|capture| capture.finish().ok());
    }
}

#[derive(Debug)]
struct CaptureHandle {
    state: Arc<Mutex<CaptureState>>,
    failure_supervised: bool,
    worker: Option<thread::JoinHandle<()>>,
}

#[derive(Debug)]
struct CaptureState {
    output: CapturedOutput,
    failure: Option<io::Error>,
    observer_complete: bool,
}

impl CaptureHandle {
    fn is_complete(&self) -> bool {
        self.state
            .lock()
            .is_ok_and(|state| state.output.complete && state.observer_complete)
    }

    fn failure(&self) -> io::Result<()> {
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
            .map_or(Ok(()), |error| Err(clone_io(error)))
    }

    fn snapshot(&self) -> io::Result<CapturedOutput> {
        self.state
            .lock()
            .map(|state| state.output.clone())
            .map_err(|_| io::Error::other("capture state lock is poisoned"))
    }

    fn finish(mut self) -> io::Result<()> {
        if self.is_complete()
            && let Some(worker) = self.worker.take()
        {
            worker
                .join()
                .map_err(|_| io::Error::other("capture worker panicked"))?;
        }
        Ok(())
    }
}

fn start_capture<R: Read + Send + 'static>(
    mut reader: R,
    stream: OutputStream,
    policy: StreamPolicy,
    observation: Option<Observation>,
    job: Arc<Job>,
    failure: Arc<FailureState>,
) -> CaptureHandle {
    let (limit, retention, abort_on_limit) = capture_policy(policy);
    let selected = observation.is_some();
    let failure_observer = observation.as_ref().map(Observation::observer);
    let state = Arc::new(Mutex::new(CaptureState {
        output: CapturedOutput {
            bytes: Vec::with_capacity(limit.min(READ_BYTES)),
            truncated: false,
            complete: false,
            retains_suffix: retention == Retention::Suffix,
        },
        failure: None,
        observer_complete: !selected,
    }));
    let worker_state = Arc::clone(&state);
    let worker_failure = Arc::clone(&failure);
    let worker_job = Arc::clone(&job);
    let worker = thread::spawn(move || {
        let mut framer = observation.map(|observation| {
            SelectedFramer::new(
                observation,
                Arc::clone(&worker_state),
                Arc::clone(&worker_job),
                Arc::clone(&worker_failure),
            )
        });
        let mut buffer = [0_u8; READ_BYTES];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    if let Some(framer) = &mut framer {
                        framer.finish();
                    }
                    if let Ok(mut state) = worker_state.lock() {
                        state.output.complete = true;
                    }
                    worker_failure.wake.notify();
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
                        if let Err(error) = worker_job.terminate() {
                            worker_failure.fail_cleanup(error);
                        }
                        worker_failure.wake.notify();
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
                    let detail = error.to_string();
                    if let Ok(mut state) = worker_state.lock() {
                        state.failure = Some(error);
                    }
                    worker_failure.wake.notify();
                    if let Some(observer) = &failure_observer {
                        worker_failure.fail_observation(
                            &worker_job,
                            observer,
                            ObservationFailure::Reader { stream, detail },
                        );
                    }
                    return;
                }
            }
        }
    });
    CaptureHandle {
        state,
        failure_supervised: selected,
        worker: (!selected).then_some(worker),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Retention {
    Prefix,
    Suffix,
}

fn capture_policy(policy: StreamPolicy) -> (usize, Retention, bool) {
    match policy {
        StreamPolicy::Capture { limit } => (limit, Retention::Prefix, false),
        StreamPolicy::CaptureBounded { limit } => (limit, Retention::Prefix, true),
        StreamPolicy::CaptureTail { limit } => (limit, Retention::Suffix, false),
        StreamPolicy::Inherit | StreamPolicy::Null => unreachable!("uncaptured stream"),
    }
}

fn retain_chunk(
    state: &Mutex<CaptureState>,
    bytes: &[u8],
    limit: usize,
    retention: Retention,
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

fn retain(output: &mut CapturedOutput, bytes: &[u8], limit: usize, retention: Retention) {
    if retention == Retention::Prefix {
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

#[derive(Debug)]
enum Frame {
    Record {
        bytes: Vec<u8>,
        boundary: RecordBoundary,
    },
    End,
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
    job: Arc<Job>,
    failure: Arc<FailureState>,
}

impl SelectedFramer {
    fn new(
        observation: Observation,
        state: Arc<Mutex<CaptureState>>,
        job: Arc<Job>,
        failure: Arc<FailureState>,
    ) -> Self {
        let stream = observation.stream();
        let limit = observation.frame_limit();
        let capacity = observation.queue_capacity();
        let sink_deadline = observation.sink_deadline();
        let observer = observation.observer();
        let (sender, receiver) = frame_queue::bounded(capacity);
        start_observer_worker(
            receiver,
            ObserverWorker {
                stream,
                capacity,
                sink_deadline,
                observer: Arc::clone(&observer),
                job: Arc::clone(&job),
                failure: Arc::clone(&failure),
                state,
            },
        );
        Self {
            stream,
            limit,
            capacity,
            sink_deadline,
            record: Vec::with_capacity(limit.min(READ_BYTES)),
            discarding: false,
            sender,
            observer,
            job,
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
        self.failure
            .fail_observation(&self.job, &self.observer, failure);
    }
}

struct ObserverWorker {
    stream: OutputStream,
    capacity: usize,
    sink_deadline: Duration,
    observer: Arc<dyn RecordObserver>,
    job: Arc<Job>,
    failure: Arc<FailureState>,
    state: Arc<Mutex<CaptureState>>,
}

fn start_observer_worker(receiver: FrameReceiver<Frame>, worker: ObserverWorker) {
    let ObserverWorker {
        stream,
        capacity,
        sink_deadline,
        observer,
        job,
        failure,
        state,
    } = worker;
    let (requests, callbacks) = mpsc::sync_channel::<Frame>(0);
    let (results, result_receiver) = mpsc::sync_channel(0);
    let callback_observer = Arc::clone(&observer);
    thread::spawn(move || {
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
    thread::spawn(move || {
        for frame in receiver {
            if requests.send(frame).is_err() {
                failure.fail_observation(
                    &job,
                    &observer,
                    ObservationFailure::ObserverLost { stream },
                );
                break;
            }
            let started = Instant::now();
            let failed = match result_receiver.recv_timeout(sink_deadline) {
                Ok(ObserverResult::Accepted) => None,
                Ok(ObserverResult::Refused(error)) => Some(ObservationFailure::SinkRefused {
                    stream,
                    detail: error.detail().to_owned(),
                }),
                Ok(ObserverResult::Lost) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    Some(ObservationFailure::ObserverLost { stream })
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    Some(ObservationFailure::SinkBackpressure {
                        stream,
                        capacity,
                        deadline: sink_deadline,
                        stall: Box::new(ObservationStall {
                            elapsed: started.elapsed(),
                            waiting: "callback completion",
                            stage: observer.stage(),
                        }),
                    })
                }
            };
            if let Some(failed) = failed {
                failure.fail_observation(&job, &observer, failed);
                break;
            }
        }
        if let Ok(mut state) = state.lock() {
            state.observer_complete = true;
        }
        failure.wake.notify();
    });
}

enum ObserverResult {
    Accepted,
    Refused(crate::observe::ObserveError),
    Lost,
}

fn validate_observation(spec: &Spec, observation: Option<&Observation>) -> io::Result<()> {
    let Some(observation) = observation else {
        return Ok(());
    };
    let selected = match observation.stream() {
        OutputStream::Stdout => spec.output.stdout(),
        OutputStream::Stderr => spec.output.stderr(),
    };
    if matches!(
        selected,
        StreamPolicy::Capture { .. } | StreamPolicy::CaptureTail { .. }
    ) {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "selected {} must use the capture stream policy",
            observation.stream()
        ),
    ))
}

fn configure_stream(command: &mut Command, stdout: bool, policy: StreamPolicy) {
    let stdio = match policy {
        StreamPolicy::Inherit => Stdio::inherit(),
        StreamPolicy::Null => Stdio::null(),
        StreamPolicy::Capture { .. }
        | StreamPolicy::CaptureBounded { .. }
        | StreamPolicy::CaptureTail { .. } => Stdio::piped(),
    };
    if stdout {
        command.stdout(stdio);
    } else {
        command.stderr(stdio);
    }
}

fn take_stdout(child: &mut Child, policy: StreamPolicy) -> io::Result<Option<ChildStdout>> {
    if matches!(policy, StreamPolicy::Inherit | StreamPolicy::Null) {
        return Ok(None);
    }
    child
        .stdout
        .take()
        .map(Some)
        .ok_or_else(|| io::Error::other("captured stdout pipe is absent"))
}

fn take_stderr(child: &mut Child, policy: StreamPolicy) -> io::Result<Option<ChildStderr>> {
    if matches!(policy, StreamPolicy::Inherit | StreamPolicy::Null) {
        return Ok(None);
    }
    child
        .stderr
        .take()
        .map(Some)
        .ok_or_else(|| io::Error::other("captured stderr pipe is absent"))
}

fn clone_io(error: &io::Error) -> io::Error {
    io::Error::new(error.kind(), error.to_string())
}
