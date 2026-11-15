//! Detached keeper runtime and observation control.

use std::fs::OpenOptions;
use std::io::{self, ErrorKind};
use std::path::Path;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant as Monotonic};

use sill::{GroupSignal, await_group_exit, signal_group};
use vendor::omp::OutputPattern;
use wire::CanonicalUuid;

use super::super::super::super::daemon::observer;
use super::retention::{Limit, Log};
use super::{
    DELIVERED, DELIVERY_GRACE, Ended, Ending, Intervention, LOG, OPERATOR_STOP, REQUEST_BYTES,
    Request, STATE, STOP, Sent, State, TERMINATE_GRACE, Trigger, VERSION, WAKE, archive_log,
    now_millis, publish_json, read_json, until,
};
/// What the keeper's loop waits on.
enum Keep {
    Exited(std::process::ExitStatus),
    /// The job's directory changed: a stop may have been asked.
    Changed,
    Failed(String),
    /// The command's observed output crossed a threshold.
    Output(Limit),
}

impl Keep {
    fn woke(woke: io::Result<()>) -> Self {
        woke.map_or_else(|error| Self::Failed(error.to_string()), |()| Self::Changed)
    }
}

/// Keep one command: run it, end it at its deadline, on request, or when its
/// output runs away, publish how it ended, then archive its log.
///
/// # Errors
///
/// Returns the failure of reading the request or publishing the state; a
/// command that cannot run is an ending, not an error.
pub(in crate::resident::supervisor) fn keep(request_path: &Path) -> io::Result<()> {
    let directory = request_path
        .parent()
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "job request has no directory"))?;
    let request: Request = read_json(request_path, REQUEST_BYTES)?;
    if request.version != VERSION {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("unsupported job request version {}", request.version),
        ));
    }
    let state_path = directory.join(STATE);
    let mut state = State {
        version: VERSION,
        id: request.id.clone(),
        command: request.command.clone(),
        keeper: std::process::id(),
        started_millis: now_millis()?,
        timeout_millis: request.timeout_millis,
        ending: None,
        ended_millis: None,
        output: None,
        interventions: Vec::new(),
    };
    publish_json(&state_path, &state)?;
    let (sender, receiver) = mpsc::channel();
    let ending = match spawn(directory, &request, sender.clone()) {
        Ok((group, watcher, log)) => {
            let stop = directory.join(STOP);
            let mut _armed = None;
            let ending = tend(
                &receiver,
                request.timeout_millis.map(Duration::from_millis),
                TERMINATE_GRACE,
                || {
                    _armed = Some(watch(directory, &sender, Keep::woke)?);
                    Ok(stop.exists())
                },
                |signal| signal_group(group, signal),
                |deadline| await_group_exit(group, deadline),
                |signal, trigger| {
                    state.interventions.push(Intervention {
                        signal,
                        trigger,
                        observed: log.observed(),
                        at_millis: now_millis().unwrap_or_default(),
                    });
                },
            );
            state.output = Some(log.close(TERMINATE_GRACE));
            if let Some(watcher) = watcher {
                watcher.complete();
            }
            ending
        }
        Err(reason) => Ending::Failed { reason },
    };
    state.ending = Some(ending);
    state.ended_millis = Some(now_millis()?);
    publish_json(&state_path, &state)?;
    if let Some(lane) = request.lane
        && (state.ending != Some(Ending::Stopped) || directory.join(OPERATOR_STOP).exists())
        && unheard(directory, DELIVERY_GRACE, |directory| {
            directory.join(DELIVERED).exists()
        })
    {
        wake(lane, directory);
    }
    // An unarchived log stays readable as plaintext; the resident's sweep
    // archives it later.
    let _ = archive_log(directory);
    Ok(())
}

/// Whether nobody has heard by the end of the grace: the record an occupant
/// leaves on hearing is awaited on the directory's watch, so an occupant that
/// is there answers at once and a vacant lane costs the grace.
fn unheard(directory: &Path, grace: Duration, heard: impl Fn(&Path) -> bool) -> bool {
    let deadline = Monotonic::now() + grace;
    let (sender, receiver) = mpsc::channel();
    let mut armed;
    loop {
        armed = watch(directory, &sender, Keep::woke).ok();
        if armed.is_none() || heard(directory) {
            return !heard(directory);
        }
        let remaining = deadline.saturating_duration_since(Monotonic::now());
        if !matches!(receiver.recv_timeout(remaining), Ok(Keep::Changed)) {
            return !heard(directory);
        }
    }
}

/// The lane a match nobody hears wakes: a deadline-bounded command's. A
/// service never keeps a lane idle, so its match reaches only a seated
/// occupant and wakes nobody.
pub(super) fn match_lane(request: &Request) -> Option<CanonicalUuid> {
    request.timeout_millis.and(request.lane)
}

/// What the keeper does once the watcher publishes its match: with a lane
/// to wake, a match nobody hears within the grace wakes it, as an unheard
/// ending does, so its next occupant hears the match.
fn match_waker(lane: Option<CanonicalUuid>, directory: &Path) -> impl FnOnce() + Send + 'static {
    let directory = directory.to_path_buf();
    move || {
        if let Some(lane) = lane
            && unheard(&directory, DELIVERY_GRACE, until::heard)
        {
            wake(lane, &directory);
        }
    }
}

/// Wake the lane so its next occupant hears the ending. The keeper runs in
/// the occupant's environment, so the lane it names is unset and the wake
/// speaks for the operator; a lane still occupied refuses, and what the
/// wake said is kept with the job rather than in the command's log.
fn wake(lane: CanonicalUuid, directory: &Path) {
    let _woken = OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join(WAKE))
        .and_then(|record| {
            let stderr = record.try_clone()?;
            ProcessCommand::new(std::env::current_exe()?)
                .args(["lane", "wake"])
                .arg(lane.to_string())
                .env_remove(steer::hook::LANE)
                .stdin(Stdio::null())
                .stdout(record)
                .stderr(stderr)
                .status()
        });
}

/// Start the command in its own group with both streams read into the log
/// and a waiter reporting its exit. With `until`, a watcher searches the log
/// for its first matching line.
fn spawn(
    directory: &Path,
    request: &Request,
    sender: mpsc::Sender<Keep>,
) -> Result<(u32, Option<until::Watcher>, Log), String> {
    let (program, arguments) = request
        .argv
        .split_first()
        .ok_or_else(|| "empty argv".to_owned())?;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join(LOG))
        .map_err(|error| format!("log unavailable: {error}"))?;
    let limits = sender.clone();
    let mut log = Log::open(
        file,
        Box::new(move |limit| {
            let _ = limits.send(Keep::Output(limit));
        }),
    );
    let pattern = request
        .until
        .as_deref()
        .map(OutputPattern::new)
        .transpose()?;
    let mut command = ProcessCommand::new(program);
    command
        .args(arguments)
        .current_dir(&request.cwd)
        .env_clear()
        .envs(request.environment.entries().iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn_group(&mut command).map_err(|error| error.to_string())?;
    let pid = child.id();
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    let readers = (|| {
        if let Some(stderr) = stderr {
            log.drain(stderr)?;
        }
        if let Some(stdout) = stdout {
            log.drain(stdout)?;
        }
        pattern
            .map(|pattern| {
                until::arm(
                    directory,
                    pattern,
                    match_waker(match_lane(request), directory),
                )
            })
            .transpose()
    })();
    let watcher = match readers {
        Ok(watcher) => watcher,
        Err(error) => {
            let _ = signal_group(pid, GroupSignal::Kill);
            return Err(format!("output readers unavailable: {error}"));
        }
    };
    thread::Builder::new()
        .name("spice-job-waiter".to_owned())
        .spawn(move || {
            let _ = sender.send(match child.wait() {
                Ok(status) => Keep::Exited(status),
                Err(error) => Keep::Failed(error.to_string()),
            });
        })
        .map_err(|error| format!("waiter unavailable: {error}"))?;
    Ok((pid, watcher, log))
}

/// Wait for the command to end, ending it at its deadline, on request, or
/// when its output runs away: the group is asked to leave, then killed once
/// `grace` elapses. Without a deadline the command runs until it ends or a
/// stop is asked.
///
/// `stop_asked` arms one watch on the job's directory and says whether a
/// stop has been asked by now; it is consulted on every wake, so an
/// unwatchable directory is read as a stop rather than as a command nobody
/// can reach. `signal` reaches the command's whole group, and `emptied`
/// waits until the group has no member left or the instant it is given
/// passes, saying whether the group emptied. Every signal sent because of
/// output is told to `intervened` once sent.
fn tend(
    receiver: &mpsc::Receiver<Keep>,
    timeout: Option<Duration>,
    grace: Duration,
    mut stop_asked: impl FnMut() -> io::Result<bool>,
    signal: impl Fn(GroupSignal) -> io::Result<()>,
    emptied: impl Fn(Monotonic) -> io::Result<bool>,
    mut intervened: impl FnMut(Sent, Trigger),
) -> Ending {
    let mut intervene = |sent: Sent, trigger: Trigger| {
        let _ = signal(match sent {
            Sent::Term => GroupSignal::Terminate,
            Sent::Kill => GroupSignal::Kill,
        });
        intervened(sent, trigger);
    };
    let mut deadline = timeout.map(|timeout| Monotonic::now() + timeout);
    let mut pending: Option<Ending> = None;
    let mut changed = true;
    loop {
        if changed && pending.is_none() && stop_asked().unwrap_or(true) {
            pending = Some(Ending::Stopped);
            let _ = signal(GroupSignal::Terminate);
            deadline = Some(Monotonic::now() + grace);
        }
        changed = false;
        let remaining = deadline.map_or(Duration::MAX, |deadline| {
            deadline.saturating_duration_since(Monotonic::now())
        });
        match receiver.recv_timeout(remaining) {
            Ok(Keep::Exited(status)) => {
                return match pending {
                    Some(ending) => swept(ending, grace, &signal, &emptied),
                    None => exited(status),
                };
            }
            Ok(Keep::Failed(reason)) => return Ending::Failed { reason },
            Ok(Keep::Changed) => changed = true,
            Ok(Keep::Output(Limit::Terminate)) => {
                if pending.is_none() {
                    pending = Some(Ending::OutputLimit);
                    intervene(Sent::Term, Trigger::Output);
                    deadline = Some(Monotonic::now() + grace);
                }
            }
            Ok(Keep::Output(Limit::Kill)) => {
                intervene(Sent::Kill, Trigger::Output);
                let ending = pending.unwrap_or(Ending::OutputLimit);
                return killed(receiver, ending, grace, &signal, &emptied);
            }
            Err(mpsc::RecvTimeoutError::Timeout) if pending.is_none() => {
                pending = Some(Ending::TimedOut);
                let _ = signal(GroupSignal::Terminate);
                deadline = Some(Monotonic::now() + grace);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if pending == Some(Ending::OutputLimit) {
                    intervene(Sent::Kill, Trigger::Grace);
                } else {
                    let _ = signal(GroupSignal::Kill);
                }
                let ending = pending.unwrap_or(Ending::Stopped);
                return killed(receiver, ending, grace, &signal, &emptied);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Ending::Failed {
                    reason: "the command's waiter ended without a status".to_owned(),
                };
            }
        }
    }
}

/// The ending of a group already killed, once its leader's exit arrives.
fn killed(
    receiver: &mpsc::Receiver<Keep>,
    ending: Ending,
    grace: Duration,
    signal: impl Fn(GroupSignal) -> io::Result<()>,
    emptied: impl Fn(Monotonic) -> io::Result<bool>,
) -> Ending {
    loop {
        match receiver.recv() {
            Ok(Keep::Failed(reason)) => return Ending::Failed { reason },
            Ok(Keep::Exited(_)) | Err(_) => return swept(ending, grace, &signal, &emptied),
            Ok(Keep::Changed | Keep::Output(_)) => {}
        }
    }
}

/// The ending of a group asked to end, once its leader has died: a leader
/// signalled mid-fork leaves a child the signal never reached, so whoever
/// is left in the group is asked to leave and killed if still there at the
/// grace. An empty group refuses the first signal, so the common case waits
/// nothing; a group whose exit cannot be observed is killed at once.
fn swept(
    ending: Ending,
    grace: Duration,
    signal: impl Fn(GroupSignal) -> io::Result<()>,
    emptied: impl Fn(Monotonic) -> io::Result<bool>,
) -> Ending {
    if signal(GroupSignal::Terminate).is_ok() && !emptied(Monotonic::now() + grace).unwrap_or(false)
    {
        let _ = signal(GroupSignal::Kill);
    }
    ending
}

#[cfg(unix)]
fn exited(status: std::process::ExitStatus) -> Ending {
    use std::os::unix::process::ExitStatusExt as _;
    match (status.code(), status.signal()) {
        (Some(code), _) => Ending::Exited { code },
        (None, Some(signal)) => Ending::Signalled { signal },
        (None, None) => Ending::Failed {
            reason: "the command ended without a status".to_owned(),
        },
    }
}

#[cfg(not(unix))]
fn exited(status: std::process::ExitStatus) -> Ending {
    match status.code() {
        Some(code) => Ending::Exited { code },
        None => Ending::Failed {
            reason: "the command ended without a status".to_owned(),
        },
    }
}

/// The command as the leader of its own group. On Windows the group is the
/// kill-on-close Job the spawn names for it, so whatever the leader leaves
/// behind ends once the waiter lets go of it rather than outliving the job.
#[cfg(unix)]
fn spawn_group(command: &mut ProcessCommand) -> io::Result<std::process::Child> {
    super::detach(command);
    command.spawn()
}

#[cfg(windows)]
fn spawn_group(command: &mut ProcessCommand) -> io::Result<sill::OwnedChild> {
    sill::spawn_owned_command(command, sill::ProcessGroup::New)
}

/// Send what `signal` makes of the next change to `path`, a directory or a
/// file; the watch is armed before the reader looks, and ends unsent once
/// the returned disarm is dropped.
pub(super) fn watch<T: Send + 'static>(
    path: &Path,
    sender: &mpsc::Sender<T>,
    signal: impl FnOnce(io::Result<()>) -> T + Send + 'static,
) -> io::Result<observer::Disarm> {
    let observer = observer::DirectObserver::arm(path)?;
    observer::wait_for_change(observer, sender.clone(), signal)
}

/// What reaches a job's observer: the job's own progress, and the lane's
/// word on the call that is observing it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::resident) enum Signal {
    /// The job's directory changed.
    Changed,
    /// The job's log was written.
    Wrote,
    KeeperExited,
    ObserverFailed(String),
    /// The call this observation answers was withdrawn.
    Cancel,
    /// A message arrived for the occupant; the observation should yield.
    Interrupt,
}

impl Signal {
    pub(super) fn woke(woke: io::Result<()>) -> Self {
        woke.map_or_else(
            |error| Self::ObserverFailed(error.to_string()),
            |()| Self::Changed,
        )
    }

    pub(super) fn wrote(wrote: io::Result<()>) -> Self {
        wrote.map_or_else(
            |error| Self::ObserverFailed(error.to_string()),
            |()| Self::Wrote,
        )
    }
}

impl observer::ExitSender for mpsc::Sender<Signal> {
    fn exited(self) {
        let _ = self.send(Signal::KeeperExited);
    }

    fn failed(self, error: String) {
        let _ = self.send(Signal::ObserverFailed(error));
    }
}

/// One call's observation channel: the job's progress and the lane's
/// interruptions arrive on it together, so a wait needs no polling.
pub(in crate::resident) struct Control {
    pub(super) sender: mpsc::Sender<Signal>,
    pub(super) receiver: mpsc::Receiver<Signal>,
}

/// The lane's end of one call's control: cancel it, or interrupt it.
#[derive(Clone)]
pub(in crate::resident) struct Handle(mpsc::Sender<Signal>);

impl Control {
    pub(in crate::resident) fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self { sender, receiver }
    }

    pub(in crate::resident) fn handle(&self) -> Handle {
        Handle(self.sender.clone())
    }

    #[cfg(test)]
    pub(in crate::resident) fn was_interrupted(&self) -> bool {
        matches!(self.receiver.try_recv(), Ok(Signal::Interrupt))
    }
}

impl Handle {
    pub(in crate::resident) fn cancel(&self) {
        let _ = self.0.send(Signal::Cancel);
    }

    pub(in crate::resident) fn interrupt(&self) {
        let _ = self.0.send(Signal::Interrupt);
    }
}

/// How one bounded observation of a job ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::resident) enum Observed {
    Ended(Ended),
    Cancelled,
    Interrupted,
    Elapsed,
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::process::ExitStatus;

    use super::*;

    const GRACE: Duration = Duration::from_millis(20);

    /// A process group as the keeper reaches it: a kill always ends it, a
    /// request to leave ends it only when it obeys, and once ended it refuses
    /// every signal the way an empty group does.
    struct Group {
        waiter: mpsc::Sender<Keep>,
        obeys: bool,
        alive: Cell<bool>,
        heard: RefCell<Vec<GroupSignal>>,
    }

    impl Group {
        fn new(waiter: mpsc::Sender<Keep>, obeys: bool) -> Self {
            Self {
                waiter,
                obeys,
                alive: Cell::new(true),
                heard: RefCell::new(Vec::new()),
            }
        }

        fn signal(&self, signal: GroupSignal) -> io::Result<()> {
            self.heard.borrow_mut().push(signal);
            if !self.alive.get() {
                return Err(io::Error::from(ErrorKind::NotFound));
            }
            if signal == GroupSignal::Kill || self.obeys {
                self.alive.set(false);
                let _ = self.waiter.send(Keep::Exited(ExitStatus::default()));
            }
            Ok(())
        }

        /// The fake has no clock: a group still there when its emptying is
        /// awaited is one that stays past the deadline.
        fn emptied(&self, _deadline: Monotonic) -> io::Result<bool> {
            Ok(!self.alive.get())
        }
    }

    #[test]
    fn a_stopped_group_that_leaves_is_never_killed() {
        let (sender, receiver) = mpsc::channel();
        let group = Group::new(sender, true);

        let ending = tend(
            &receiver,
            None,
            GRACE,
            || Ok(true),
            |signal| group.signal(signal),
            |deadline| group.emptied(deadline),
            |_, _| {},
        );

        assert_eq!(ending, Ending::Stopped);
        assert_eq!(group.heard.borrow()[0], GroupSignal::Terminate);
        assert!(!group.heard.borrow().contains(&GroupSignal::Kill));
    }

    #[test]
    fn a_deadline_asks_first_and_kills_a_group_that_stays_past_the_grace() {
        let (sender, receiver) = mpsc::channel();
        let group = Group::new(sender, false);

        let ending = tend(
            &receiver,
            Some(Duration::ZERO),
            GRACE,
            || Ok(false),
            |signal| group.signal(signal),
            |deadline| group.emptied(deadline),
            |_, _| {},
        );

        assert_eq!(ending, Ending::TimedOut);
        assert_eq!(
            group.heard.borrow()[..2],
            [GroupSignal::Terminate, GroupSignal::Kill]
        );
    }

    #[test]
    fn an_unwatchable_job_directory_is_read_as_a_stop() {
        let (sender, receiver) = mpsc::channel();
        let group = Group::new(sender, false);

        let ending = tend(
            &receiver,
            None,
            GRACE,
            || Err(io::Error::from(ErrorKind::PermissionDenied)),
            |signal| group.signal(signal),
            |deadline| group.emptied(deadline),
            |_, _| {},
        );

        assert_eq!(ending, Ending::Stopped);
        assert_eq!(
            group.heard.borrow()[..2],
            [GroupSignal::Terminate, GroupSignal::Kill]
        );
    }

    #[test]
    fn a_command_that_ends_on_its_own_keeps_its_status_and_hears_nothing() {
        let (sender, receiver) = mpsc::channel();
        let group = Group::new(sender.clone(), false);
        sender
            .send(Keep::Exited(ExitStatus::default()))
            .expect("the receiver is held");

        let ending = tend(
            &receiver,
            Some(Duration::from_secs(60)),
            GRACE,
            || Ok(false),
            |signal| group.signal(signal),
            |deadline| group.emptied(deadline),
            |_, _| {},
        );

        assert_eq!(ending, Ending::Exited { code: 0 });
        assert!(group.heard.borrow().is_empty());
    }

    #[test]
    fn runaway_output_is_asked_to_leave_then_killed_and_each_signal_is_recorded() {
        let (sender, receiver) = mpsc::channel();
        let group = Group::new(sender.clone(), false);
        sender
            .send(Keep::Output(Limit::Terminate))
            .expect("the receiver is held");
        let mut interventions = Vec::new();

        let ending = tend(
            &receiver,
            None,
            GRACE,
            || Ok(false),
            |signal| group.signal(signal),
            |deadline| group.emptied(deadline),
            |signal, trigger| interventions.push((signal, trigger)),
        );

        assert_eq!(ending, Ending::OutputLimit);
        assert_eq!(
            group.heard.borrow()[..2],
            [GroupSignal::Terminate, GroupSignal::Kill]
        );
        assert_eq!(
            interventions,
            [(Sent::Term, Trigger::Output), (Sent::Kill, Trigger::Grace)]
        );
    }

    #[test]
    fn members_that_outlive_the_leader_are_killed_only_if_they_stay_past_the_grace() {
        for (empties, heard) in [
            (true, &[GroupSignal::Terminate][..]),
            (false, &[GroupSignal::Terminate, GroupSignal::Kill][..]),
        ] {
            let signals = RefCell::new(Vec::new());

            let ending = swept(
                Ending::Stopped,
                GRACE,
                |signal| {
                    signals.borrow_mut().push(signal);
                    Ok(())
                },
                |_deadline| Ok(empties),
            );

            assert_eq!(ending, Ending::Stopped);
            assert_eq!(signals.borrow()[..], *heard);
        }
    }
}
