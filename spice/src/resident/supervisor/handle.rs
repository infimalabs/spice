//! The resident's hold on a detached supervisor's occupant: its typed wait
//! over published state, its group-addressed stop, and the raw status and
//! liveness evidence the state records.

use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

use lane::occupant::{DetachedProcess, DetachedWait};
use sill::{GroupSignal, WaitError, WaitOutcome};

use super::events::StateEvents;
use super::{CommandResult, Phase, State, command, environment_fault, read_state, required};

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt as _;
#[cfg(windows)]
use std::os::windows::process::ExitStatusExt as _;

pub(super) struct Handle {
    path: PathBuf,
    /// The kick door a command-driven occupant's driver listens at.
    door: PathBuf,
    owner: String,
    group: u32,
    grace: Duration,
    reap: Duration,
    pub(super) events: Option<StateEvents>,
}

impl Handle {
    pub(super) fn new(
        path: PathBuf,
        door: PathBuf,
        state: &State,
        events: Option<StateEvents>,
    ) -> CommandResult<Self> {
        Ok(Self {
            path,
            door,
            owner: format!("lane {} detached supervisor", state.lane),
            group: required(state.group, "group")?,
            grace: Duration::from_millis(required(state.grace_millis, "termination grace")?),
            reap: Duration::from_millis(required(state.reap_millis, "reap bound")?),
            events,
        })
    }

    fn state(&self) -> Result<State, WaitError> {
        read_state(&self.path).map_err(|source| self.wait_error("reading supervisor state", source))
    }

    pub(super) fn wait_error(&self, operation: &'static str, source: io::Error) -> WaitError {
        WaitError::Io {
            owner: self.owner.clone(),
            operation,
            source,
        }
    }

    pub(super) fn terminal(&self) -> Result<Option<WaitOutcome>, WaitError> {
        let state = self.state()?;
        match state.phase {
            Phase::Exited => state.completion.as_ref().map_or_else(
                || {
                    Err(self.wait_error(
                        "decoding supervisor exit",
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "exited supervisor state has no typed completion",
                        ),
                    ))
                },
                |completion| {
                    completion
                        .decode()
                        .map(WaitOutcome::externally_reaped_completion)
                        .map(Some)
                        .map_err(|error| self.wait_error("decoding supervisor exit", error))
                },
            ),
            Phase::Failed => Err(self.wait_error(
                "observing supervisor",
                io::Error::other(
                    state
                        .failure
                        .unwrap_or_else(|| "supervisor failed".to_owned()),
                ),
            )),
            Phase::Starting | Phase::Running | Phase::Idle => Ok(None),
        }
    }

    fn signal(&self, signal: GroupSignal, operation: &'static str) -> Result<(), WaitError> {
        sill::signal_group(self.group, signal).map_err(|error| self.wait_error(operation, error))
    }
}

impl DetachedProcess for Handle {
    fn group(&self) -> u32 {
        self.group
    }

    fn termination_grace(&self) -> Duration {
        self.grace
    }

    fn reap_bound(&self) -> Duration {
        self.reap
    }

    fn wait_for(self: Box<Self>, duration: Duration) -> Result<DetachedWait, WaitError> {
        match self.observe_until(Some(duration))? {
            Some(outcome) => Ok(DetachedWait::Exited(outcome)),
            None => Ok(DetachedWait::Running(self)),
        }
    }

    fn terminate(&self) -> Result<(), WaitError> {
        // An occupant driven over its own protocol is interrupted there; the
        // signal is for one nobody drives, or one that stays after the ask.
        if command::abort(&self.door).is_ok() {
            return Ok(());
        }
        self.signal(
            GroupSignal::Terminate,
            "terminating supervisor-owned process group",
        )
    }

    fn kill(&self) -> Result<(), WaitError> {
        self.signal(GroupSignal::Kill, "killing supervisor-owned process group")
    }

    fn wait(self: Box<Self>) -> Result<WaitOutcome, WaitError> {
        self.observe_until(None)?.ok_or_else(|| {
            self.wait_error(
                "waiting for supervisor state change",
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "unbounded supervisor wait ended without a terminal state",
                ),
            )
        })
    }
}

/// An occupant that lives in process: it runs until the resident ends it, and
/// its ending is the status the group signal that did leaves.
#[cfg(test)]
pub(super) struct StandInOccupant {
    grace: Duration,
    reap: Duration,
    ending: std::sync::OnceLock<GroupSignal>,
}

#[cfg(test)]
impl StandInOccupant {
    /// Beyond every process identifier a host hands out, so no group signal
    /// can reach a real process.
    pub(super) const GROUP: u32 = i32::MAX.unsigned_abs();

    pub(super) fn new(spec: &sill::Spec) -> Self {
        Self {
            grace: spec.grace,
            reap: spec.deadlines.reap(),
            ending: std::sync::OnceLock::new(),
        }
    }

    fn outcome(&self) -> Result<WaitOutcome, WaitError> {
        let raw = self.ending.get().copied().map_or(0, signalled_raw);
        let status = status_from_raw(raw).map_err(|source| WaitError::Io {
            owner: "in-process occupant".to_owned(),
            operation: "reading its ending",
            source,
        })?;
        Ok(WaitOutcome::externally_reaped_completion(
            sill::Completion::Exited(status),
        ))
    }
}

/// The raw status a group member ended by `signal` reports: the signal itself
/// on Unix; on Windows the console-interrupt exit for a break, and the code a
/// terminated Job gives its members for a kill.
#[cfg(test)]
fn signalled_raw(signal: GroupSignal) -> i32 {
    #[cfg(unix)]
    return match signal {
        GroupSignal::Terminate => libc::SIGTERM,
        GroupSignal::Kill => libc::SIGKILL,
    };
    #[cfg(windows)]
    return match signal {
        GroupSignal::Terminate => windows_sys::Win32::Foundation::STATUS_CONTROL_C_EXIT,
        GroupSignal::Kill => 1,
    };
}

#[cfg(test)]
impl DetachedProcess for StandInOccupant {
    fn group(&self) -> u32 {
        Self::GROUP
    }

    fn termination_grace(&self) -> Duration {
        self.grace
    }

    fn reap_bound(&self) -> Duration {
        self.reap
    }

    fn wait_for(self: Box<Self>, _duration: Duration) -> Result<DetachedWait, WaitError> {
        if self.ending.get().is_none() {
            return Ok(DetachedWait::Running(self));
        }
        self.outcome().map(DetachedWait::Exited)
    }

    fn terminate(&self) -> Result<(), WaitError> {
        let _first = self.ending.set(GroupSignal::Terminate);
        Ok(())
    }

    fn kill(&self) -> Result<(), WaitError> {
        let _first = self.ending.set(GroupSignal::Kill);
        Ok(())
    }

    /// Having written its whole stream, an unended stand-in exits cleanly once
    /// awaited.
    fn wait(self: Box<Self>) -> Result<WaitOutcome, WaitError> {
        self.outcome()
    }
}

#[cfg(unix)]
pub(super) fn status_raw(status: ExitStatus) -> i32 {
    status.into_raw()
}

/// A Windows exit code is 32 bits the record keeps as they are; Windows
/// always reports one.
#[cfg(windows)]
pub(super) fn status_raw(status: ExitStatus) -> i32 {
    status.code().unwrap_or(i32::MIN)
}

#[cfg(unix)]
pub(super) fn status_from_raw(raw: i32) -> io::Result<ExitStatus> {
    Ok(ExitStatus::from_raw(raw))
}

#[cfg(windows)]
pub(super) fn status_from_raw(raw: i32) -> io::Result<ExitStatus> {
    Ok(ExitStatus::from_raw(raw.cast_unsigned()))
}

pub(super) fn process_exists(pid: u32) -> CommandResult<bool> {
    process_exists_raw(pid).map_err(|error| {
        environment_fault("lane::supervisor-liveness-unavailable", &error.to_string())
    })
}

pub(super) fn process_exists_raw(pid: u32) -> io::Result<bool> {
    sill::process_running(pid)
}
