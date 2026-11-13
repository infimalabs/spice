use std::fs;
use std::io;
use std::path::Path;

use super::{ProcessState, kernel_identity_absent, membership};

/// Where the kernel publishes per-process status.
///
/// Reading it is ordinary file I/O rather than a syscall that only exists on one
/// kernel, so this module is compiled for tests everywhere and its fixtures
/// drive the branches below by pointing at a directory they control. The macOS
/// module cannot be built that way because `libproc` exists only on macOS.
pub(super) const KERNEL_PROCESSES: &str = "/proc";

pub(super) fn process_zombie(pid: u32) -> ProcessState {
    zombie_under(Path::new(KERNEL_PROCESSES), pid)
}

pub(super) fn process_group_members_terminal(pid: u32) -> ProcessState {
    let Ok(identity) = libc::pid_t::try_from(pid) else {
        return ProcessState::Indeterminate;
    };
    group_members_terminal_under(Path::new(KERNEL_PROCESSES), pid, || {
        kernel_identity_absent(-identity)
    })
}

pub(super) fn zombie_under(processes: &Path, pid: u32) -> ProcessState {
    let Ok(stat) = fs::read_to_string(processes.join(pid.to_string()).join("stat")) else {
        return ProcessState::Indeterminate;
    };
    let Some((_, state)) = stat_group_and_state(&stat) else {
        return ProcessState::Indeterminate;
    };
    reported_state(state)
}

pub(super) fn group_members_terminal_under(
    processes: &Path,
    pid: u32,
    group_absent: impl FnOnce() -> bool,
) -> ProcessState {
    let Ok(entries) = fs::read_dir(processes) else {
        return ProcessState::Indeterminate;
    };
    let mut members = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            return ProcessState::Indeterminate;
        };
        let Some(member) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let stat = match fs::read_to_string(processes.join(member.to_string()).join("stat")) {
            Ok(stat) => stat,
            // The process ended between listing the directory and reading it, so
            // it is in no group at all now. Any other refusal left this entry's
            // group unread, and an unread entry cannot be assumed to be elsewhere.
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return ProcessState::Indeterminate,
        };
        let Some((group, state)) = stat_group_and_state(&stat) else {
            return ProcessState::Indeterminate;
        };
        if group == pid {
            members.push(reported_state(state));
        }
    }
    membership(members.into_iter(), group_absent)
}

/// Read the process group and run state out of one `/proc/<pid>/stat` line.
///
/// The executable name is parenthesized and may itself contain spaces and
/// parentheses, so the fields are taken from after its last closing paren.
fn stat_group_and_state(stat: &str) -> Option<(u32, u8)> {
    let (_, fields) = stat.rsplit_once(") ")?;
    let mut fields = fields.split_whitespace();
    let state = *fields.next()?.as_bytes().first()?;
    let _parent = fields.next()?;
    let group = fields.next()?.parse().ok()?;
    Some((group, state))
}

/// Classify a run state the kernel actually reported.
///
/// `Z` is a reaped-pending zombie and `X`/`x` are the dead states a process
/// passes through on its way out of the table. Every other letter names a
/// process the kernel is still scheduling, which is a positive observation of
/// life rather than an absence of evidence.
fn reported_state(state: u8) -> ProcessState {
    if matches!(state, b'Z' | b'X' | b'x') {
        ProcessState::Terminal
    } else {
        ProcessState::Live
    }
}
