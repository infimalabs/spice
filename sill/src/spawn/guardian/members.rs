//! The protected group's running members, observed from past the fork.
//!
//! One pass lists the group's members, arms an exit observer on each one
//! still running, and blocks until every armed member has exited. Like the
//! rest of the guardian it allocates nothing and calls only raw syscalls:
//! fixed stack buffers hold the listing and the observers.

use super::errno;

/// What one pass over the group established.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Pass {
    /// Running members were armed, and every armed member has exited.
    Exited,
    /// A complete listing found no running member.
    Empty,
    /// The listing or the wait failed with no member left to wait on.
    Unreadable,
}

/// Exit observers armed in one pass, and whether the listing behind them
/// classified every member it named.
struct Armed {
    count: usize,
    complete: bool,
}

impl Armed {
    fn settled(&self, waited: bool) -> Pass {
        match (self.count, waited, self.complete) {
            (0, _, true) => Pass::Empty,
            (0, _, false) | (_, false, _) => Pass::Unreadable,
            _ => Pass::Exited,
        }
    }
}

/// The `proc_listpids` selector naming one process group's members.
#[cfg(target_os = "macos")]
const PROC_PGRP_ONLY: u32 = 2;

/// List `group`, arm a kqueue exit observer on every running member, and
/// wait for all of them. A member already exited refuses its registration,
/// so a zombie counts as gone rather than as running.
///
/// # Safety
///
/// Callable from the forked guardian: it allocates nothing.
#[cfg(target_os = "macos")]
pub(super) unsafe fn await_exits(group: libc::pid_t) -> Pass {
    const MEMBER_BATCH: usize = 512;
    let mut members = [0 as libc::pid_t; MEMBER_BATCH];
    let (Ok(capacity), Ok(selector)) = (
        libc::c_int::try_from(size_of_val(&members)),
        u32::try_from(group),
    ) else {
        return Pass::Unreadable;
    };
    // libproc answers zero both for an empty group and for a refusal, which
    // only errno tells apart.
    // SAFETY: libc exposes the calling thread's errno slot.
    unsafe { *libc::__error() = 0 };
    // SAFETY: the destination is writable for its complete byte capacity.
    let bytes = unsafe {
        libc::proc_listpids(
            PROC_PGRP_ONLY,
            selector,
            members.as_mut_ptr().cast(),
            capacity,
        )
    };
    // SAFETY: errno is read immediately after the call that may have set it.
    let Ok(bytes) = usize::try_from(bytes) else {
        return Pass::Unreadable;
    };
    if bytes == 0 && unsafe { errno() } != 0 {
        return Pass::Unreadable;
    }
    let listed = (bytes / size_of::<libc::pid_t>()).min(MEMBER_BATCH);
    // SAFETY: kqueue creates a descriptor this pass owns.
    let queue = unsafe { libc::kqueue() };
    if queue == -1 {
        return Pass::Unreadable;
    }
    let mut armed = Armed {
        count: 0,
        complete: listed < MEMBER_BATCH,
    };
    for member in members[..listed].iter().filter(|member| **member > 0) {
        let change = libc::kevent {
            ident: *member as libc::uintptr_t,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: the registration is valid for the call and the queue is owned.
        let registered =
            unsafe { libc::kevent(queue, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
        if registered == 0 {
            armed.count += 1;
        } else if unsafe { errno() } != libc::ESRCH {
            armed.complete = false;
        }
    }
    let mut exited = 0;
    let mut waited = true;
    while exited < armed.count {
        let mut event = std::mem::MaybeUninit::<libc::kevent>::uninit();
        // SAFETY: the single output slot is writable and the queue is owned;
        // a null timeout blocks until the next armed exit.
        let delivered = unsafe {
            libc::kevent(
                queue,
                std::ptr::null(),
                0,
                event.as_mut_ptr(),
                1,
                std::ptr::null(),
            )
        };
        if delivered == 1 {
            exited += 1;
        } else if delivered != -1 || unsafe { errno() } != libc::EINTR {
            waited = false;
            break;
        }
    }
    // SAFETY: this pass owns the queue and closes it once.
    unsafe { libc::close(queue) };
    armed.settled(waited)
}

/// List `group` from `/proc`, arm a pidfd on every running member, and wait
/// for all of them. A member the listing finds exited counts as gone.
///
/// # Safety
///
/// Callable from the forked guardian: it allocates nothing.
#[cfg(target_os = "linux")]
pub(super) unsafe fn await_exits(group: libc::pid_t) -> Pass {
    const MEMBER_BATCH: usize = 256;
    let mut observed = [libc::pollfd {
        fd: -1,
        events: libc::POLLIN,
        revents: 0,
    }; MEMBER_BATCH];
    let mut armed = Armed {
        count: 0,
        complete: true,
    };
    // SAFETY: the path is NUL-terminated and the descriptor is owned here.
    let directory = unsafe {
        libc::open(
            b"/proc\0".as_ptr().cast(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if directory == -1 {
        return Pass::Unreadable;
    }
    let mut buffer = [0_u8; super::DIRENT_BUFFER_BYTES];
    'listing: loop {
        // SAFETY: the byte buffer remains writable for the declared capacity.
        let read = unsafe {
            libc::syscall(
                libc::SYS_getdents64,
                directory,
                buffer.as_mut_ptr(),
                buffer.len(),
            )
        };
        if read == -1 && unsafe { errno() } == libc::EINTR {
            continue;
        }
        let Ok(read) = usize::try_from(read) else {
            armed.complete = false;
            break;
        };
        if read == 0 {
            break;
        }
        let mut offset = 0;
        while offset < read {
            let Some(record) = super::dirent_record(&buffer[offset..read]) else {
                armed.complete = false;
                break 'listing;
            };
            offset += record.len();
            let Some(pid) = super::dirent_descriptor(record) else {
                continue;
            };
            match unsafe { member_state(pid, group) } {
                Member::Elsewhere => {}
                Member::Unreadable => armed.complete = false,
                Member::Running if armed.count == MEMBER_BATCH => armed.complete = false,
                Member::Running => {
                    // SAFETY: pidfd_open observes the named process without changing it.
                    let descriptor =
                        unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as libc::c_int };
                    if descriptor != -1 {
                        observed[armed.count].fd = descriptor;
                        armed.count += 1;
                    } else if unsafe { errno() } != libc::ESRCH {
                        armed.complete = false;
                    }
                }
            }
        }
    }
    // SAFETY: the listing owns the directory descriptor and closes it once.
    unsafe { libc::close(directory) };
    let mut pending = armed.count;
    let mut waited = true;
    while pending > 0 {
        // SAFETY: the first `armed.count` slots are writable poll records;
        // a negative descriptor is one already answered, which poll skips.
        let ready = unsafe { libc::poll(observed.as_mut_ptr(), armed.count as libc::nfds_t, -1) };
        if ready == -1 && unsafe { errno() } == libc::EINTR {
            continue;
        }
        if ready < 1 {
            waited = false;
            break;
        }
        for slot in observed[..armed.count].iter_mut() {
            if slot.fd >= 0 && slot.revents != 0 {
                // SAFETY: the slot owns its pidfd and closes it once.
                unsafe { libc::close(slot.fd) };
                slot.fd = -1;
                pending -= 1;
            }
        }
    }
    for slot in observed[..armed.count].iter().filter(|slot| slot.fd >= 0) {
        // SAFETY: each remaining slot owns its pidfd.
        unsafe { libc::close(slot.fd) };
    }
    armed.settled(waited)
}

#[cfg(target_os = "linux")]
enum Member {
    Running,
    Elsewhere,
    Unreadable,
}

/// Whether `pid` is a running member of `group`, read from its `stat`
/// record: a process gone before the read, or exited and awaiting its
/// reaper, is not running.
#[cfg(target_os = "linux")]
unsafe fn member_state(pid: libc::pid_t, group: libc::pid_t) -> Member {
    const PREFIX: &[u8] = b"/proc/";
    const SUFFIX: &[u8] = b"/stat\0";
    let mut path = [0_u8; 32];
    path[..PREFIX.len()].copy_from_slice(PREFIX);
    let mut digits = [0_u8; 10];
    let mut remaining = pid.unsigned_abs();
    let mut count = 0;
    loop {
        digits[count] = b'0' + (remaining % 10) as u8;
        count += 1;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    let mut length = PREFIX.len();
    for digit in digits[..count].iter().rev() {
        path[length] = *digit;
        length += 1;
    }
    path[length..length + SUFFIX.len()].copy_from_slice(SUFFIX);
    // SAFETY: the path is NUL-terminated and the descriptor is owned here.
    let record = unsafe { libc::open(path.as_ptr().cast(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if record == -1 {
        return match unsafe { errno() } {
            libc::ENOENT | libc::ESRCH => Member::Elsewhere,
            _ => Member::Unreadable,
        };
    }
    let mut bytes = [0_u8; 512];
    let read = loop {
        // SAFETY: the buffer is writable for its length.
        let read = unsafe { libc::read(record, bytes.as_mut_ptr().cast(), bytes.len()) };
        if read != -1 || unsafe { errno() } != libc::EINTR {
            break read;
        }
    };
    let gone = read == -1 && unsafe { errno() } == libc::ESRCH;
    // SAFETY: this read owns the record descriptor and closes it once.
    unsafe { libc::close(record) };
    let Ok(read) = usize::try_from(read) else {
        return if gone {
            Member::Elsewhere
        } else {
            Member::Unreadable
        };
    };
    stat_membership(&bytes[..read], group)
}

/// Classify one `stat` record. The command name may hold any byte, so the
/// fields are read after its last closing parenthesis: state, then parent,
/// then process group.
#[cfg(target_os = "linux")]
fn stat_membership(record: &[u8], group: libc::pid_t) -> Member {
    let Some(name_end) = record.iter().rposition(|byte| *byte == b')') else {
        return Member::Unreadable;
    };
    let mut fields = record[name_end + 1..]
        .split(|byte| *byte == b' ')
        .filter(|field| !field.is_empty());
    let (Some(state), Some(_parent), Some(member_group)) =
        (fields.next(), fields.next(), fields.next())
    else {
        return Member::Unreadable;
    };
    let mut parsed: libc::pid_t = 0;
    for byte in member_group {
        let Some(next) = byte
            .is_ascii_digit()
            .then(|| {
                parsed
                    .checked_mul(10)?
                    .checked_add(libc::pid_t::from(byte - b'0'))
            })
            .flatten()
        else {
            return Member::Unreadable;
        };
        parsed = next;
    }
    if parsed != group || matches!(state, [b'Z' | b'X' | b'x']) {
        Member::Elsewhere
    } else {
        Member::Running
    }
}
