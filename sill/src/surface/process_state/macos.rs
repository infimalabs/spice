use std::io;
use std::mem::{MaybeUninit, size_of, size_of_val};

use super::{ProcessState, kernel_identity_absent, membership, process_state};

pub(super) fn process_zombie(pid: u32) -> ProcessState {
    let Some((pid, size)) = probe_arguments(pid, size_of::<libc::proc_bsdshortinfo>()) else {
        return ProcessState::Indeterminate;
    };
    let mut information = MaybeUninit::<libc::proc_bsdshortinfo>::uninit();
    // SAFETY: the process information destination is writable for its complete size.
    let bytes = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            information.as_mut_ptr().cast(),
            size,
        )
    };
    // The errno is read here rather than inside the branch that needs it because
    // it belongs to the call above and any later call would overwrite it.
    let errno = io::Error::last_os_error().raw_os_error();

    match short_info_state(bytes, size, errno) {
        Some(state) => state,
        // SAFETY: an exact-size result initialized the process information.
        None => bsd_status(unsafe { information.assume_init() }.pbsi_status),
    }
}

/// Both fixed-width conversions the libproc interface requires, refused as one.
///
/// Neither conversion learned anything about the process, so a refusal here is
/// the same unreadable answer whichever one of them did not fit.
pub(super) fn probe_arguments(pid: u32, destination: usize) -> Option<(libc::pid_t, libc::c_int)> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    let destination = libc::c_int::try_from(destination).ok()?;
    Some((pid, destination))
}

/// The answer a `proc_pidinfo` result settles by itself, or `None` when it
/// filled the destination exactly and the status word decides instead.
///
/// This is separated from the call because the kernel conditions that produce a
/// refusal or a short read cannot be staged by a fixture, while classifying
/// those conditions is ordinary arithmetic that can. Returning the answer itself
/// rather than an intermediate reading leaves no untested step between this
/// classification and what the probe reports.
pub(super) fn short_info_state(
    bytes: libc::c_int,
    size: libc::c_int,
    errno: Option<libc::c_int>,
) -> Option<ProcessState> {
    if bytes == 0 {
        return Some(if errno == Some(libc::ESRCH) {
            ProcessState::Terminal
        } else {
            ProcessState::Indeterminate
        });
    }
    if bytes == size {
        None
    } else {
        Some(ProcessState::Indeterminate)
    }
}

pub(super) fn bsd_status(status: u32) -> ProcessState {
    if status == libc::SZOMB {
        ProcessState::Terminal
    } else {
        ProcessState::Live
    }
}

pub(super) fn process_group_members_terminal(pid: u32) -> ProcessState {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return ProcessState::Indeterminate;
    };
    // A null buffer asks libproc how many PIDs to make room for, and both calls
    // answer in PIDs rather than bytes. The sizing answer is not scoped to the
    // group: on this host it reported 613 for a one-member group and 615 for a
    // three-member group, tracking the whole process table, so it is an upper
    // bound on any group rather than a measurement of this one. The buffer is
    // therefore always larger than the membership, which is why the truncation
    // check below can refuse but is not expected to.
    // SAFETY: a null buffer requests the sizing answer without writing anything.
    let required = unsafe { libc::proc_listpgrppids(pid, std::ptr::null_mut(), 0) };
    let capacity = match listing_capacity(required) {
        Ok(capacity) => capacity,
        Err(state) => return state,
    };
    let mut members = vec![0 as libc::pid_t; capacity];
    let bytes = match listing_bytes(size_of_val(members.as_slice())) {
        Ok(bytes) => bytes,
        Err(state) => return state,
    };
    // SAFETY: the PID buffer is writable for its complete byte capacity.
    let found = unsafe { libc::proc_listpgrppids(pid, members.as_mut_ptr().cast(), bytes) };
    let found = match listing_length(found, capacity) {
        Ok(found) => found,
        Err(state) => return state,
    };

    let members = members.into_iter().take(found).filter(|member| *member > 0);
    membership(members.map(|member| process_state(member as u32)), || {
        kernel_identity_absent(-pid)
    })
}

/// How many PIDs to make room for, given what the sizing call answered.
///
/// A negative answer is the call refusing rather than counting, and a refusal
/// counted nothing. The buffer is deliberately one larger than the answer so the
/// length check below has room to notice a listing that did not fit.
pub(super) fn listing_capacity(required: libc::c_int) -> Result<usize, ProcessState> {
    let Ok(required) = usize::try_from(required) else {
        return Err(ProcessState::Indeterminate);
    };
    // A `c_int` cannot reach `usize::MAX` on any platform this builds for, so
    // the guard is what keeps that assumption from becoming an overflow if it
    // ever stops holding.
    required.checked_add(1).ok_or(ProcessState::Indeterminate)
}

/// Convert the PID buffer's byte count to the width libproc accepts.
///
/// A byte count that does not fit means the listing call cannot be made, so it
/// leaves the group unreadable rather than answering anything about its state.
pub(super) fn listing_bytes(bytes: usize) -> Result<libc::c_int, ProcessState> {
    libc::c_int::try_from(bytes).map_err(|_| ProcessState::Indeterminate)
}

/// How many PIDs the listing actually wrote, or why the listing cannot be used.
///
/// A negative answer is again a refusal. A listing that reached capacity wrote
/// more members than the sizing call just said exist, so it may have been cut
/// off and cannot be read as a complete membership.
pub(super) fn listing_length(found: libc::c_int, capacity: usize) -> Result<usize, ProcessState> {
    let Ok(found) = usize::try_from(found) else {
        return Err(ProcessState::Indeterminate);
    };
    if found >= capacity {
        return Err(ProcessState::Indeterminate);
    }
    Ok(found)
}
