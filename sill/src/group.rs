//! Signals to a process group from a process that may not have spawned it.
//!
//! A [`ProcessGroup::New`](crate::ProcessGroup::New) spawn roots a group whose
//! identifier is the root's process id. A process holding only that
//! identifier, such as a later generation of the spawner's owner, reaches the
//! whole group here: on Unix through the kernel process group, on Windows
//! through the Job Object the spawn named for the group.

use std::io;
use std::time::Instant;

use crate::ProcessExitObserver;

/// What a process group is asked to do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GroupSignal {
    /// Leave on its own: `SIGTERM` on Unix, `CTRL_BREAK_EVENT` on Windows.
    ///
    /// Windows delivers the event only to a group attached to the caller's
    /// console; elsewhere the group hears nothing and a kill must follow.
    Terminate,
    /// End now, every process at once: `SIGKILL` on Unix, termination of the
    /// group's Job Object on Windows.
    Kill,
}

/// Deliver `signal` to every process in `group`.
///
/// # Errors
///
/// Returns the operating system's refusal. A group with no remaining process
/// refuses, so success means some process was there to hear it.
pub fn signal_group(group: u32, signal: GroupSignal) -> io::Result<()> {
    if group == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process group 0 names the caller's own group",
        ));
    }
    platform_signal(group, signal)
}

#[cfg(unix)]
fn platform_signal(group: u32, signal: GroupSignal) -> io::Result<()> {
    let group = libc::pid_t::try_from(group).map_err(io::Error::other)?;
    let signal = match signal {
        GroupSignal::Terminate => libc::SIGTERM,
        GroupSignal::Kill => libc::SIGKILL,
    };
    // SAFETY: a negative PID addresses exactly the process group `group`; the
    // conversion above excludes overflow and the guard above excludes zero.
    if unsafe { libc::kill(-group, signal) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Block until no process remains in `group` or `deadline` passes, answering
/// whether the group emptied.
///
/// Each pass enumerates the group's current members and waits on every one's
/// exit; a member that joined meanwhile is found by the next pass, so the
/// group is empty once a pass finds nobody.
///
/// # Errors
///
/// Returns the operating system's refusal to enumerate the group or to
/// observe one of its members.
pub fn await_group_exit(group: u32, deadline: Instant) -> io::Result<bool> {
    loop {
        let members = platform_members(group)?;
        if members.is_empty() {
            return Ok(true);
        }
        let observers = members
            .into_iter()
            .map(ProcessExitObserver::or_ended)
            .collect::<io::Result<Vec<_>>>()?;
        for observer in &observers {
            if !observer.wait_until(deadline)? {
                return Ok(false);
            }
        }
    }
}

#[cfg(unix)]
fn platform_members(group: u32) -> io::Result<Vec<u32>> {
    crate::process_tree::live_group_members(group)
}

#[cfg(windows)]
fn platform_members(group: u32) -> io::Result<Vec<u32>> {
    crate::spawn::group_members(group)
}

#[cfg(windows)]
fn platform_signal(group: u32, signal: GroupSignal) -> io::Result<()> {
    crate::spawn::signal_group(group, signal)
}
