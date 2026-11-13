//! Independent lock possession tied to one protected process group's lifetime.
//!
//! The child reports its new group before `exec`, while the wrapper retains the
//! notification endpoint as a liveness token. The guardian blocks on that token
//! and begins watching group absence only after the wrapper completes or dies.

use crate::protected::{GROUP_SLOT_BLANK, GROUP_SLOT_BYTES, GROUP_SLOT_PREFIX, group_slot};
use crate::surface::process_state::ProcessState;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::thread::JoinHandle;

mod members;

const GUARDIAN_LOCK_FD: RawFd = 3;
const GUARDIAN_NOTICE_FD: RawFd = 4;
const DUPLICATE_MINIMUM_FD: RawFd = 10;
const READY_BYTE: u8 = 1;
#[cfg(target_os = "linux")]
const PROC_SELF_FD: &[u8] = b"/proc/self/fd\0";
#[cfg(target_os = "linux")]
const DIRENT_BUFFER_BYTES: usize = 1024;
#[cfg(target_os = "linux")]
const DIRENT_RECORD_LENGTH_OFFSET: usize = 16;
#[cfg(target_os = "linux")]
const DIRENT_NAME_OFFSET: usize = 19;
/// How much of a record the guardian reads to find the slot reserved in it.
///
/// A record is one line of JSON whose longest field is a working directory, so
/// this spans every one the platform can produce with room left over, and it is
/// a stack array because the guardian is past the fork and allocates nothing.
const RECORD_SCAN_BYTES: usize = 8192;

#[derive(Debug)]
pub(super) struct PendingGuardian {
    notifier: Option<UnixStream>,
    guardian: Option<Guardian>,
}

impl PendingGuardian {
    pub(super) fn start(file: &File) -> io::Result<Self> {
        let (mut notifier, receiver) = UnixStream::pair()?;
        let lock = duplicate(file.as_raw_fd())?;
        let notice = duplicate(receiver.as_raw_fd())?;

        // SAFETY: the child branch uses fixed-stack descriptor setup and raw
        // process operations, then exits with `_exit` without running destructors.
        let pid = unsafe { libc::fork() };
        if pid == -1 {
            return Err(io::Error::last_os_error());
        }
        if pid == 0 {
            guardian_main(lock.as_raw_fd(), notice.as_raw_fd(), true);
        }

        drop(receiver);
        drop(lock);
        drop(notice);
        if let Err(error) = await_ready(&mut notifier) {
            drop(notifier);
            // SAFETY: `pid` identifies the guardian forked above.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            let _reap_result = reap(pid);
            return Err(error);
        }
        let reaper = match std::thread::Builder::new()
            .name("sill-lock-guardian".to_owned())
            .spawn(move || reap(pid))
        {
            Ok(reaper) => reaper,
            Err(error) => {
                drop(notifier);
                // SAFETY: `pid` identifies the guardian forked above.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
                let _reap_result = reap(pid);
                return Err(error);
            }
        };
        Ok(Self {
            notifier: Some(notifier),
            guardian: Some(Guardian {
                pid: u32::try_from(pid).expect("forked guardian pid is positive"),
                notifier: None,
                reaper: Some(reaper),
            }),
        })
    }

    pub(super) fn notifier_fd(&self) -> RawFd {
        self.notifier
            .as_ref()
            .expect("pending guardian retains its notifier")
            .as_raw_fd()
    }

    pub(super) fn id(&self) -> u32 {
        self.guardian
            .as_ref()
            .expect("pending guardian retains its process")
            .pid
    }

    pub(super) fn activate(mut self) -> Guardian {
        let mut guardian = self
            .guardian
            .take()
            .expect("pending guardian retains its process");
        guardian.notifier = self.notifier.take();
        guardian
    }
}

fn await_ready(notifier: &mut UnixStream) -> io::Result<()> {
    let mut ready = [0_u8; 1];
    notifier.read_exact(&mut ready).map_err(|source| {
        io::Error::new(
            source.kind(),
            format!("lock guardian did not complete descriptor setup: {source}"),
        )
    })?;
    if ready == [READY_BYTE] {
        Ok(())
    } else {
        Err(io::Error::other("lock guardian sent invalid readiness"))
    }
}

impl Drop for PendingGuardian {
    fn drop(&mut self) {
        drop(self.notifier.take());
        if let Some(guardian) = self.guardian.take() {
            let _wait_result = guardian.wait();
        }
    }
}

#[derive(Debug)]
pub(super) struct Guardian {
    pid: u32,
    notifier: Option<UnixStream>,
    reaper: Option<JoinHandle<io::Result<()>>>,
}

impl Guardian {
    pub(super) const fn id(&self) -> u32 {
        self.pid
    }

    pub(super) fn wait(mut self) -> io::Result<()> {
        drop(self.notifier.take());
        self.reaper
            .take()
            .expect("guardian retains its reaper")
            .join()
            .map_err(|_| io::Error::other("lock guardian reaper panicked"))?
    }
}

pub(super) fn notify_group(notifier: RawFd) -> io::Result<()> {
    // SAFETY: this child-only hook uses process-local async-signal-safe calls.
    if unsafe { libc::setpgid(0, 0) } == -1 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: `getpid` has no preconditions and returns this child process's id.
    let pid = unsafe { libc::getpid() };
    let bytes = pid.to_ne_bytes();
    let mut written = 0;
    while written < bytes.len() {
        // SAFETY: the byte slice remains valid for the duration of this call.
        let result = unsafe {
            libc::send(
                notifier,
                bytes[written..].as_ptr().cast(),
                bytes.len() - written,
                libc::MSG_NOSIGNAL,
            )
        };
        if result > 0 {
            written += result as usize;
            continue;
        }
        if result == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "lock guardian notification closed early",
            ));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(())
}

/// What a signal-zero probe established about the protected group.
///
/// An identity that does not fit `pid_t` is reported as unreadable rather than
/// as a group that is still there. Nothing was probed in that case: the number
/// was never a group the kernel could name, so the probe observed neither its
/// presence nor its absence, and saying "still present" would put a claim the
/// caller can act on behind a conversion that simply refused.
pub(super) fn group_state(pid: u32) -> ProcessState {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return ProcessState::Indeterminate;
    };
    raw_group_state(pid)
}

fn duplicate(descriptor: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: `fcntl` duplicates the live descriptor into a newly owned slot.
    let duplicate = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, DUPLICATE_MINIMUM_FD) };
    if duplicate == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful `fcntl` result is a fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

fn guardian_main(lock: RawFd, notice: RawFd, descriptor_setup: bool) -> ! {
    // SAFETY: the guardian is the only thread in this forked process and uses
    // only raw descriptors until it exits without running Rust destructors.
    unsafe {
        if libc::setpgid(0, 0) == -1
            || libc::dup2(lock, GUARDIAN_LOCK_FD) == -1
            || libc::dup2(notice, GUARDIAN_NOTICE_FD) == -1
        {
            libc::_exit(125);
        }
        if !descriptor_setup || !close_unneeded_descriptors() || !send_ready() {
            libc::close(GUARDIAN_NOTICE_FD);
            libc::_exit(125);
        }

        let mut bytes = [0_u8; size_of::<u32>()];
        let mut received = 0;
        while received < bytes.len() {
            let result = libc::read(
                GUARDIAN_NOTICE_FD,
                bytes[received..].as_mut_ptr().cast(),
                bytes.len() - received,
            );
            if result > 0 {
                received += result as usize;
            } else if result == 0 {
                libc::_exit(0);
            } else if errno() != libc::EINTR {
                libc::_exit(125);
            }
        }
        let pid = libc::pid_t::try_from(u32::from_ne_bytes(bytes)).unwrap_or(0);
        if pid <= 0 {
            libc::_exit(125);
        }
        publish_group(u32::from_ne_bytes(bytes));
        wait_for_wrapper_close();
        libc::close(GUARDIAN_NOTICE_FD);
        // Only an observed absence ends this wait. The guardian exists to hold
        // the lock for as long as any member might still be running, so a
        // probe or listing that established nothing is not permission to
        // release it -- exiting there would hand the lock to the next claimant
        // on the strength of an answer nobody got. That is the same rule
        // `crate::surface::owner` takes, and the opposite of the one the
        // post-signal consumers in `crate::spawn` chose, where forgiving an
        // unreadable answer ends a wait that has already delivered its kill
        // rather than releasing a claim. With nothing it can name left to wait
        // on, an unreadable group keeps the lock until the guardian is ended.
        //
        // Absence is the probe finding no group, or two consecutive complete
        // listings finding no running member: a member that exited before
        // the first could arm forked first, and the second lists that child.
        // A member that exited but awaits its reaper still answers the probe,
        // so only the listings can see past it.
        let mut empty_before = false;
        loop {
            if raw_group_state(pid) == ProcessState::Terminal {
                libc::_exit(0);
            }
            match members::await_exits(pid) {
                members::Pass::Exited => empty_before = false,
                members::Pass::Empty if empty_before => libc::_exit(0),
                members::Pass::Empty => empty_before = true,
                members::Pass::Unreadable => loop {
                    libc::pause();
                },
            }
        }
    }
}

unsafe fn send_ready() -> bool {
    let ready = [READY_BYTE];
    loop {
        // SAFETY: the one-byte source remains valid for the duration of the call.
        let result = unsafe {
            libc::send(
                GUARDIAN_NOTICE_FD,
                ready.as_ptr().cast(),
                ready.len(),
                libc::MSG_NOSIGNAL,
            )
        };
        if result == 1 {
            return true;
        }
        if result != -1 || unsafe { errno() } != libc::EINTR {
            return false;
        }
    }
}

/// Fill in the group slot the wrapper reserved before this guardian was forked.
///
/// This is the whole reason the guardian publishes rather than the wrapper. The
/// group does not exist until the child does, `spawn` returns only after that
/// child has `exec`ed, and a wrapper killed anywhere in between would leave the
/// lock held by a guardian no reader could name. This process cannot be killed
/// in that window: it is already running, already holds the descriptor, and
/// learns the group from the child itself.
///
/// One positioned write does it, and lands where it belongs even against a
/// wrapper rewriting the whole record at the same moment: the slot's offset is
/// fixed for every record that owner writes, and the only other bytes anyone
/// would put in it are the same digits this writes.
///
/// A record carrying no slot is left untouched. It says the owner reserved
/// none, which is not this process's to invent. A write that lands only partly
/// is left where it fell, for the same reason it is safe to attempt at all: a
/// slot names a group only if every byte of one arrived, so the worst a torn
/// write leaves is the unbounded refusal that was already there.
unsafe fn publish_group(group: u32) {
    let mut record = [0_u8; RECORD_SCAN_BYTES];
    // SAFETY: the scan destination remains valid for the duration of the call.
    let read = unsafe {
        libc::pread(
            GUARDIAN_LOCK_FD,
            record.as_mut_ptr().cast(),
            record.len(),
            0,
        )
    };
    let Ok(read) = usize::try_from(read) else {
        return;
    };
    let Some(offset) = blank_slot_offset(&record[..read.min(RECORD_SCAN_BYTES)]) else {
        return;
    };
    let slot = group_slot(group);
    // SAFETY: the slot source remains valid for the duration of the call, and
    // the offset is one this process just read a reserved slot from.
    unsafe {
        libc::pwrite(GUARDIAN_LOCK_FD, slot.as_ptr().cast(), slot.len(), offset);
    }
}

/// Where this record's reserved slot begins, if it carries one still blank.
///
/// A slot already carrying something is not written over. The only writer that
/// could have filled it is the owner, publishing the same group from the other
/// side, so there is nothing here to add and nothing to correct.
fn blank_slot_offset(record: &[u8]) -> Option<libc::off_t> {
    let start = record
        .windows(GROUP_SLOT_PREFIX.len())
        .position(|window| window == GROUP_SLOT_PREFIX)?
        + GROUP_SLOT_PREFIX.len();
    let slot = record.get(start..start.checked_add(GROUP_SLOT_BYTES)?)?;
    if !slot.iter().all(|byte| *byte == GROUP_SLOT_BLANK) {
        return None;
    }
    libc::off_t::try_from(start).ok()
}

unsafe fn wait_for_wrapper_close() {
    let mut byte = 0_u8;
    loop {
        // SAFETY: the one-byte destination remains valid for the duration of the call.
        let result = unsafe { libc::read(GUARDIAN_NOTICE_FD, (&raw mut byte).cast(), 1) };
        if result == 0 {
            return;
        }
        if result == -1 && unsafe { errno() } != libc::EINTR {
            return;
        }
    }
}

#[cfg(target_os = "linux")]
unsafe fn close_unneeded_descriptors() -> bool {
    for descriptor in 0..GUARDIAN_LOCK_FD {
        // SAFETY: each inherited descriptor belongs to this guardian process.
        if unsafe { libc::close(descriptor) } == -1 && unsafe { errno() } != libc::EBADF {
            return false;
        }
    }

    // SAFETY: this closes only descriptors above the guardian's two retained slots.
    let result = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            GUARDIAN_NOTICE_FD + 1,
            libc::c_uint::MAX,
            0,
        )
    };
    // Read errno before another libc call can replace it.
    let error = if result == -1 { unsafe { errno() } } else { 0 };
    match close_range_disposition(result, error) {
        CloseRangeDisposition::Complete => true,
        CloseRangeDisposition::Scan => unsafe { close_proc_descriptors() },
        CloseRangeDisposition::Refused => false,
    }
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CloseRangeDisposition {
    Complete,
    Scan,
    Refused,
}

#[cfg(target_os = "linux")]
const fn close_range_disposition(
    result: libc::c_long,
    error: libc::c_int,
) -> CloseRangeDisposition {
    if result == 0 {
        CloseRangeDisposition::Complete
    } else if result == -1
        && (error == libc::ENOSYS || error == libc::EPERM || error == libc::EINVAL)
    {
        CloseRangeDisposition::Scan
    } else {
        CloseRangeDisposition::Refused
    }
}

#[cfg(target_os = "linux")]
unsafe fn close_proc_descriptors() -> bool {
    // SAFETY: the constant path is nul-terminated and the guardian owns the returned descriptor.
    let directory = unsafe {
        libc::open(
            PROC_SELF_FD.as_ptr().cast(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if directory == -1 {
        return false;
    }
    let mut buffer = [0_u8; DIRENT_BUFFER_BYTES];
    loop {
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
        if read < 0 {
            unsafe { libc::close(directory) };
            return false;
        }
        if read == 0 {
            return unsafe { libc::close(directory) } == 0;
        }
        let Ok(read) = usize::try_from(read) else {
            unsafe { libc::close(directory) };
            return false;
        };
        let mut offset = 0;
        while offset < read {
            let Some(record) = dirent_record(&buffer[offset..read]) else {
                unsafe { libc::close(directory) };
                return false;
            };
            if let Some(descriptor) = dirent_descriptor(record)
                && descriptor > GUARDIAN_NOTICE_FD
                && descriptor != directory
                && unsafe { libc::close(descriptor) } == -1
                && unsafe { errno() } != libc::EBADF
            {
                unsafe { libc::close(directory) };
                return false;
            }
            offset += record.len();
        }
    }
}

#[cfg(target_os = "linux")]
fn dirent_record(bytes: &[u8]) -> Option<&[u8]> {
    let length = bytes.get(
        DIRENT_RECORD_LENGTH_OFFSET..DIRENT_RECORD_LENGTH_OFFSET.checked_add(size_of::<u16>())?,
    )?;
    let length = usize::from(u16::from_ne_bytes(length.try_into().ok()?));
    (length >= DIRENT_NAME_OFFSET)
        .then(|| bytes.get(..length))
        .flatten()
}

#[cfg(target_os = "linux")]
fn dirent_descriptor(record: &[u8]) -> Option<RawFd> {
    let name = record.get(DIRENT_NAME_OFFSET..)?;
    let end = name.iter().position(|byte| *byte == 0)?;
    let name = name.get(..end)?;
    if name.is_empty() || !name.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut descriptor = 0_u32;
    for byte in name {
        descriptor = descriptor
            .checked_mul(10)?
            .checked_add(u32::from(*byte - b'0'))?;
    }
    RawFd::try_from(descriptor).ok()
}

#[cfg(target_os = "macos")]
unsafe fn close_unneeded_descriptors() -> bool {
    const DESCRIPTOR_BATCH: usize = 128;
    let byte_capacity = size_of::<libc::proc_fdinfo>() * DESCRIPTOR_BATCH;
    let Ok(byte_capacity) = libc::c_int::try_from(byte_capacity) else {
        return false;
    };

    loop {
        let mut descriptors =
            [const { std::mem::MaybeUninit::<libc::proc_fdinfo>::uninit() }; DESCRIPTOR_BATCH];
        // SAFETY: the fixed stack array is writable for its complete byte capacity.
        let bytes = unsafe {
            libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDLISTFDS,
                0,
                descriptors.as_mut_ptr().cast(),
                byte_capacity,
            )
        };
        let Ok(bytes) = usize::try_from(bytes) else {
            return false;
        };
        if bytes == 0 || bytes % size_of::<libc::proc_fdinfo>() != 0 {
            return false;
        }

        let mut retained_lock = false;
        let mut retained_notice = false;
        let mut closed = 0;
        for descriptor in descriptors
            .iter()
            .take(bytes / size_of::<libc::proc_fdinfo>())
        {
            // SAFETY: `proc_pidinfo` initialized every complete returned entry.
            let descriptor = unsafe { descriptor.assume_init_ref() }.proc_fd;
            match descriptor {
                GUARDIAN_LOCK_FD => retained_lock = true,
                GUARDIAN_NOTICE_FD => retained_notice = true,
                _ => {
                    // SAFETY: each listed descriptor belongs to this guardian process.
                    if unsafe { libc::close(descriptor) } == -1 && unsafe { errno() } != libc::EBADF
                    {
                        return false;
                    }
                    closed += 1;
                }
            }
        }
        if closed == 0 {
            return retained_lock && retained_notice && bytes == 2 * size_of::<libc::proc_fdinfo>();
        }
    }
}

fn raw_group_state(pid: libc::pid_t) -> ProcessState {
    // SAFETY: signal zero probes process-group existence without delivering a signal.
    let result = unsafe { libc::kill(-pid, 0) };
    // SAFETY: libc exposes the calling thread's errno slot, which the classifier
    // reads only on the refusal that set it.
    group_probe_state(result, unsafe { errno() })
}

/// Classify what one signal-zero call to a process group answered.
///
/// Three of the four answers are positive observations, and only one is not.
/// A delivered probe found the group. `ESRCH` is the kernel saying it knows no
/// such group, which is absence observed rather than assumed. `EPERM` is the
/// answer this classification exists to name correctly: it is not the kernel
/// declining to answer but the kernel resolving the group, finding at least one
/// member, and refusing only the delivery -- so it establishes a live member
/// just as firmly as success does, and it is the errno OOPS-1kKY9C8K recorded
/// under fleet contention. That leaves every other errno, which established
/// nothing at all; folding it into presence is what would let a probe that
/// failed impersonate a member the kernel described as running.
///
/// The errno is a parameter so every arm can be driven, because the kernel
/// cannot be asked to refuse this call on demand.
pub(super) fn group_probe_state(result: libc::c_int, errno: libc::c_int) -> ProcessState {
    if result == 0 {
        return ProcessState::Live;
    }
    match errno {
        libc::ESRCH => ProcessState::Terminal,
        libc::EPERM => ProcessState::Live,
        _ => ProcessState::Indeterminate,
    }
}

unsafe fn errno() -> libc::c_int {
    #[cfg(target_os = "linux")]
    // SAFETY: libc exposes the calling thread's errno slot.
    unsafe {
        *libc::__errno_location()
    }
    #[cfg(target_os = "macos")]
    // SAFETY: libc exposes the calling thread's errno slot.
    unsafe {
        *libc::__error()
    }
}

fn reap(pid: libc::pid_t) -> io::Result<()> {
    let mut status = 0;
    loop {
        // SAFETY: `pid` is this process's guardian child and `status` is writable.
        let result = unsafe { libc::waitpid(pid, &mut status, 0) };
        if result == pid {
            return (status == 0).then_some(()).ok_or_else(|| {
                io::Error::other(format!("lock guardian exited with wait status {status}"))
            });
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
