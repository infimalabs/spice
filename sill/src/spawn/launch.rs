use super::capture::{CaptureRetention, CaptureWorker, FailureContext, start_capture};
use super::guardian::PendingGuardian;
use super::{ProcessCompletion, ProcessState, Running, start_exit_watcher};
use crate::observe::{Observation, OutputStream};
use crate::spec::{ProcessGroup, Spec, StdinPolicy, StreamPolicy};
use std::fs::File;
use std::io;
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use std::os::unix::net::UnixDatagram;
use std::os::unix::process::CommandExt;

pub(super) fn spawn_inner(
    spec: Spec,
    observation: Option<Observation>,
    retained: Option<&mut crate::Possession>,
    guarded: Option<&File>,
    arm: &mut dyn FnMut(u32) -> io::Result<()>,
) -> io::Result<Running> {
    validate_observation(&spec, observation.as_ref())?;
    if guarded.is_some() && spec.group != ProcessGroup::New {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "guarded processes require a new process group",
        ));
    }
    let wakes = SpawnWakes::new(&spec, observation.as_ref())?;
    let guardian = guarded.map(PendingGuardian::start).transpose()?;
    // The record is armed before the child exists, because after this line
    // there is no moment at which this process can be relied on to reach one.
    // `spawn` does not return until the child has already `exec`ed, so no
    // wrapper-side ordering can name the group before it is running, and a
    // wrapper killed between the two would hand possession to a guardian
    // nothing on disk names. What is written here names the guardian and
    // leaves the group blank; the guardian fills the blank in, from the group
    // the child announces to it before `exec`.
    //
    // Nothing on this side needs to create that group first. A reader told to
    // probe a group its child had not created yet would be told it is absent,
    // which reads as terminal -- the one answer that licenses a second holder.
    // The record cannot say that here, because the only writer of the group is
    // the guardian and the only source it has is `notify_group`, which the
    // child reaches by creating its group and sends from the other side of it.
    if let Some(guardian) = guardian.as_ref() {
        arm(guardian.id())?;
    }
    let mut command = command(&spec, guardian.as_ref());
    if let Some(possession) = retained {
        possession.retain_through_child(&mut command);
    }
    let mut child = command.spawn()?;
    let started = Instant::now();
    let pid = child.id();
    let pipes = match capture_pipes(&mut child, &spec) {
        Ok(pipes) => pipes,
        Err(error) => return spawn_cleanup(child, spec.group, pid, error),
    };
    let process = Arc::new(ProcessState {
        completion: Mutex::new(ProcessCompletion::default()),
        exited: Condvar::new(),
    });
    let failure_observer = observation.as_ref().map(Observation::observer);
    start_exit_watcher(
        pid,
        wakes.exit,
        Arc::clone(&process),
        failure_observer.as_ref().map(Arc::clone),
    );
    let failure = Arc::new(FailureContext {
        pid,
        group: spec.group,
        owner: spec.wait.owner().to_owned(),
        grace: spec.grace,
        reap: spec.deadlines.reap(),
        process: Arc::clone(&process),
        wake: wakes.failure,
        failed: AtomicBool::new(false),
    });
    let mut captures = CaptureSetup {
        observation,
        failure_observer,
        observer_wake: wakes.observer,
        selected_stream: wakes.selected_stream,
        failure,
    };
    let stdout = pipes
        .stdout
        .map(|(reader, limit, retention, abort_on_limit)| {
            start_capture(
                reader,
                limit,
                retention,
                abort_on_limit,
                captures.worker(OutputStream::Stdout, wakes.stdout),
            )
        });
    let stderr = pipes
        .stderr
        .map(|(reader, limit, retention, abort_on_limit)| {
            start_capture(
                reader,
                limit,
                retention,
                abort_on_limit,
                captures.worker(OutputStream::Stderr, wakes.stderr),
            )
        });
    let guardian = guardian.map(PendingGuardian::activate);
    Ok(Running {
        pid,
        spec,
        started,
        wake_receiver: wakes.receiver,
        cancellation_wake: wakes.cancellation,
        child: Some(child),
        stdin: None,
        process,
        stdout,
        stderr,
        guardian,
    })
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

struct SpawnWakes {
    receiver: UnixDatagram,
    cancellation: UnixDatagram,
    exit: UnixDatagram,
    failure: UnixDatagram,
    stdout: Option<UnixDatagram>,
    stderr: Option<UnixDatagram>,
    observer: Option<UnixDatagram>,
    selected_stream: Option<OutputStream>,
}

impl SpawnWakes {
    fn new(spec: &Spec, observation: Option<&Observation>) -> io::Result<Self> {
        let (waiter, receiver) = UnixDatagram::pair()?;
        let cancellation = waiter.try_clone()?;
        let exit = waiter.try_clone()?;
        let failure = waiter.try_clone()?;
        let stdout = matches!(
            spec.output.stdout(),
            StreamPolicy::Capture { .. }
                | StreamPolicy::CaptureBounded { .. }
                | StreamPolicy::CaptureTail { .. }
        )
        .then(|| waiter.try_clone())
        .transpose()?;
        let stderr = matches!(
            spec.output.stderr(),
            StreamPolicy::Capture { .. }
                | StreamPolicy::CaptureBounded { .. }
                | StreamPolicy::CaptureTail { .. }
        )
        .then(|| waiter.try_clone())
        .transpose()?;
        let selected_stream = observation.map(Observation::stream);
        let observer = selected_stream.map(|_| waiter.try_clone()).transpose()?;
        receiver.set_nonblocking(true)?;
        Ok(Self {
            receiver,
            cancellation,
            exit,
            failure,
            stdout,
            stderr,
            observer,
            selected_stream,
        })
    }
}

struct CapturePipes {
    stdout: Option<(ChildStdout, usize, CaptureRetention, bool)>,
    stderr: Option<(ChildStderr, usize, CaptureRetention, bool)>,
}

fn capture_pipes(child: &mut Child, spec: &Spec) -> io::Result<CapturePipes> {
    Ok(CapturePipes {
        stdout: take_stdout(child, spec.output.stdout())?,
        stderr: take_stderr(child, spec.output.stderr())?,
    })
}

struct CaptureSetup {
    observation: Option<Observation>,
    failure_observer: Option<Arc<dyn crate::observe::RecordObserver>>,
    observer_wake: Option<UnixDatagram>,
    selected_stream: Option<OutputStream>,
    failure: Arc<FailureContext>,
}

impl CaptureSetup {
    fn worker(&mut self, stream: OutputStream, wake: Option<UnixDatagram>) -> CaptureWorker {
        let selected =
            (self.observation.as_ref().map(Observation::stream) == Some(stream)).then(|| {
                self.observation
                    .take()
                    .expect("selected observation remains present")
            });
        CaptureWorker {
            stream,
            observation: selected,
            failure_observer: self.failure_observer.as_ref().map(Arc::clone),
            failure: Arc::clone(&self.failure),
            wake: wake.expect("captured stream has a preallocated wake"),
            observer_wake: (self.selected_stream == Some(stream)).then(|| {
                self.observer_wake
                    .take()
                    .expect("selected stream has an observer wake")
            }),
        }
    }
}

fn command(spec: &Spec, guardian: Option<&PendingGuardian>) -> Command {
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
    if spec.group == ProcessGroup::New {
        command.process_group(0);
    }
    if let Some(guardian) = guardian {
        let notifier = guardian.notifier_fd();
        // SAFETY: this child-only hook performs only async-signal-safe system calls.
        unsafe {
            command.pre_exec(move || super::guardian::notify_group(notifier));
        }
    }
    command
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

fn take_stdout(
    child: &mut Child,
    policy: StreamPolicy,
) -> io::Result<Option<(ChildStdout, usize, CaptureRetention, bool)>> {
    match policy {
        StreamPolicy::Capture { limit } | StreamPolicy::CaptureBounded { limit } => {
            let reader = child
                .stdout
                .take()
                .ok_or_else(|| io::Error::other("captured stdout pipe is absent"))?;
            Ok(Some((
                reader,
                limit,
                CaptureRetention::Prefix,
                matches!(policy, StreamPolicy::CaptureBounded { .. }),
            )))
        }
        StreamPolicy::CaptureTail { limit } => {
            let reader = child
                .stdout
                .take()
                .ok_or_else(|| io::Error::other("captured stdout pipe is absent"))?;
            Ok(Some((reader, limit, CaptureRetention::Suffix, false)))
        }
        StreamPolicy::Inherit | StreamPolicy::Null => Ok(None),
    }
}

fn take_stderr(
    child: &mut Child,
    policy: StreamPolicy,
) -> io::Result<Option<(ChildStderr, usize, CaptureRetention, bool)>> {
    match policy {
        StreamPolicy::Capture { limit } | StreamPolicy::CaptureBounded { limit } => {
            let reader = child
                .stderr
                .take()
                .ok_or_else(|| io::Error::other("captured stderr pipe is absent"))?;
            Ok(Some((
                reader,
                limit,
                CaptureRetention::Prefix,
                matches!(policy, StreamPolicy::CaptureBounded { .. }),
            )))
        }
        StreamPolicy::CaptureTail { limit } => {
            let reader = child
                .stderr
                .take()
                .ok_or_else(|| io::Error::other("captured stderr pipe is absent"))?;
            Ok(Some((reader, limit, CaptureRetention::Suffix, false)))
        }
        StreamPolicy::Inherit | StreamPolicy::Null => Ok(None),
    }
}

fn spawn_cleanup(
    mut child: Child,
    group: ProcessGroup,
    pid: u32,
    error: io::Error,
) -> io::Result<Running> {
    let _signal_result = super::signal_pid(pid, group, super::platform::SIGKILL, "failed spawn");
    let _reap_result = child.wait();
    Err(error)
}
