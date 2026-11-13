//! The kill-on-close Job Object behind every Windows spawn, the completion
//! port that reports its process messages, and the wake those messages and
//! the root's exit drive.

use std::io;
use std::os::windows::io::{AsRawHandle as _, OwnedHandle};
use std::process::Child;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_MORE_DATA, GENERIC_ALL, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::IO::{
    CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED, PostQueuedCompletionStatus,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_ASSOCIATE_COMPLETION_PORT, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectAssociateCompletionPortInformation, JobObjectBasicAccountingInformation,
    JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation, OpenJobObjectW,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::SystemServices::{JOB_OBJECT_QUERY, JOB_OBJECT_TERMINATE};
use windows_sys::Win32::System::Threading::{INFINITE, WaitForSingleObject};

use crate::ProcessUsage;
use crate::authority::ProtectedSecurity;

const TERMINATED_EXIT: u32 = 1;
/// The completion key the job's own messages carry.
const JOB_KEY: usize = 1;
/// The completion key the waiter posts to end the port's watcher.
const STOP_KEY: usize = 2;

/// One generation counter every event source bumps, so a waiter that read it
/// before looking never misses the wake that followed the look.
#[derive(Debug, Default)]
pub(super) struct Wake {
    generation: Mutex<u64>,
    changed: Condvar,
}

impl Wake {
    pub(super) fn observe(&self) -> u64 {
        self.generation.lock().map_or(0, |generation| *generation)
    }

    pub(super) fn notify(&self) {
        if let Ok(mut generation) = self.generation.lock() {
            *generation += 1;
        }
        self.changed.notify_all();
    }

    /// Block until the generation moves past `seen` or `duration` elapses.
    pub(super) fn wait_past(&self, seen: u64, duration: Duration) -> io::Result<()> {
        let generation = self
            .generation
            .lock()
            .map_err(|_| io::Error::other("wake lock is poisoned"))?;
        let _waited = self
            .changed
            .wait_timeout_while(generation, duration, |generation| *generation == seen)
            .map_err(|_| io::Error::other("wake lock is poisoned"))?;
        Ok(())
    }
}

/// One kernel Job Object and, for a spawn that waits on it, the completion
/// port carrying its process messages.
#[derive(Debug)]
pub(super) struct Job {
    handle: HANDLE,
    port: HANDLE,
}

// SAFETY: a Job Object handle is a kernel reference whose documented query,
// assignment and termination operations are thread-safe. This owner closes it
// exactly once after the final Arc is dropped.
unsafe impl Send for Job {}
// SAFETY: see the Send justification; shared operations do not mutate Rust data.
unsafe impl Sync for Job {}

impl Job {
    /// A kill-on-close Job Object. The job of a new process group is named
    /// for the group, so a process holding only the group's id can open it;
    /// the name grants access to this user alone, and an object already
    /// holding the name is refused rather than joined. With `notices`, a
    /// completion port reports every process exit and the job emptying.
    pub(super) fn create(group: Option<u32>, notices: bool) -> io::Result<Self> {
        let mut security = group
            .map(|_| ProtectedSecurity::object(GENERIC_ALL))
            .transpose()?;
        let attributes = security.as_mut().map(ProtectedSecurity::attributes);
        let name = group.map(group_job_name);
        // SAFETY: both pointers are null or address values that outlive the
        // call: `attributes` points into `security`, and `name` is
        // NUL-terminated.
        let handle = unsafe {
            CreateJobObjectW(
                attributes
                    .as_ref()
                    .map_or(std::ptr::null(), std::ptr::from_ref),
                name.as_ref().map_or(std::ptr::null(), |name| name.as_ptr()),
            )
        };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: reads the calling thread's last error, which the successful
        // `CreateJobObjectW` above set to say whether the name was taken.
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let mut job = Self {
            handle,
            port: std::ptr::null_mut(),
        };
        if existed {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "another Job Object already holds this process group's name",
            ));
        }
        let mut information = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `information` is a fully initialized limit structure of the
        // size passed, and `job` holds a live Job Object handle.
        if unsafe {
            SetInformationJobObject(
                job.handle,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&information).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if notices {
            job.associate_port()?;
        }
        Ok(job)
    }

    /// Route the job's messages through a new completion port, before any
    /// process joins the job.
    fn associate_port(&mut self) -> io::Result<()> {
        // SAFETY: a port created from no file handle is a fresh, owned port.
        let port =
            unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 1) };
        if port.is_null() {
            return Err(io::Error::last_os_error());
        }
        let association = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
            CompletionKey: JOB_KEY as *mut std::ffi::c_void,
            CompletionPort: port,
        };
        // SAFETY: `association` is a fully initialized structure of the size
        // passed, and both handles are live.
        if unsafe {
            SetInformationJobObject(
                self.handle,
                JobObjectAssociateCompletionPortInformation,
                std::ptr::from_ref(&association).cast(),
                std::mem::size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>() as u32,
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            // SAFETY: the port was created above and nothing else holds it.
            unsafe { CloseHandle(port) };
            return Err(error);
        }
        self.port = port;
        Ok(())
    }

    /// The job a new process group's spawn named for `group`.
    pub(super) fn open(group: u32) -> io::Result<Self> {
        let name = group_job_name(group);
        // SAFETY: `name` is NUL-terminated and outlives the call.
        let handle =
            unsafe { OpenJobObjectW(JOB_OBJECT_QUERY | JOB_OBJECT_TERMINATE, 0, name.as_ptr()) };
        if handle.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self {
                handle,
                port: std::ptr::null_mut(),
            })
        }
    }

    pub(super) fn assign(&self, child: &Child) -> io::Result<()> {
        let process = child.as_raw_handle() as HANDLE;
        if unsafe { AssignProcessToJobObject(self.handle, process) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub(super) fn terminate(&self) -> io::Result<()> {
        if self.active_processes()? == 0 {
            return Ok(());
        }
        if unsafe { TerminateJobObject(self.handle, TERMINATED_EXIT) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// End the thread draining this job's completion port.
    pub(super) fn stop_notices(&self) {
        if self.port.is_null() {
            return;
        }
        // SAFETY: the port is open, and a null overlapped pointer is the
        // documented way to post a bare completion.
        unsafe {
            PostQueuedCompletionStatus(self.port, 0, STOP_KEY, std::ptr::null());
        }
    }

    fn accounting(&self) -> io::Result<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION> {
        let mut information = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        if unsafe {
            QueryInformationJobObject(
                self.handle,
                JobObjectBasicAccountingInformation,
                std::ptr::from_mut(&mut information).cast(),
                std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        } == 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(information)
        }
    }

    pub(super) fn active_processes(&self) -> io::Result<u32> {
        self.accounting()
            .map(|information| information.ActiveProcesses)
    }

    /// The ids of the processes in the job now.
    pub(super) fn process_ids(&self) -> io::Result<Vec<u32>> {
        let header = std::mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList);
        let mut capacity = 64_usize;
        loop {
            let bytes = header + capacity * std::mem::size_of::<usize>();
            // A `usize` buffer carries the list's alignment.
            let mut buffer = vec![0_usize; bytes.div_ceil(std::mem::size_of::<usize>())];
            let length = u32::try_from(bytes).map_err(io::Error::other)?;
            // SAFETY: `buffer` is writable for `length` bytes and aligned for
            // the list, and the job handle is live.
            let queried = unsafe {
                QueryInformationJobObject(
                    self.handle,
                    JobObjectBasicProcessIdList,
                    buffer.as_mut_ptr().cast(),
                    length,
                    std::ptr::null_mut(),
                )
            };
            if queried == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_MORE_DATA as i32) {
                    return Err(error);
                }
            }
            let list = buffer.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>();
            // SAFETY: the kernel filled the list's header inside `buffer`.
            let (assigned, listed) = unsafe {
                (
                    (*list).NumberOfAssignedProcesses as usize,
                    (*list).NumberOfProcessIdsInList as usize,
                )
            };
            if queried != 0 && listed >= assigned {
                // SAFETY: the kernel wrote `listed` ids, at most `capacity`,
                // after the header.
                let ids = unsafe {
                    std::slice::from_raw_parts(
                        std::ptr::addr_of!((*list).ProcessIdList).cast::<usize>(),
                        listed.min(capacity),
                    )
                };
                return Ok(ids.iter().map(|id| *id as u32).collect());
            }
            capacity = assigned.max(capacity * 2);
        }
    }

    pub(super) fn usage(&self) -> io::Result<ProcessUsage> {
        let information = self.accounting()?;
        Ok(ProcessUsage::new(
            duration_from_windows_ticks(information.TotalUserTime),
            duration_from_windows_ticks(information.TotalKernelTime),
        ))
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: this owner closes each handle exactly once.
        unsafe {
            if !self.port.is_null() {
                CloseHandle(self.port);
            }
            CloseHandle(self.handle);
        }
    }
}

/// Wake the waiter on every message the job's completion port delivers, until
/// the stop message posted when the waiter is done.
pub(super) fn watch_job(job: Arc<Job>, wake: Arc<Wake>) {
    thread::spawn(move || {
        loop {
            let mut bytes = 0_u32;
            let mut key = 0_usize;
            let mut overlapped: *mut OVERLAPPED = std::ptr::null_mut();
            // SAFETY: every out-pointer addresses a local that outlives the
            // call, and the port stays open while this `Arc<Job>` lives.
            let dequeued = unsafe {
                GetQueuedCompletionStatus(job.port, &mut bytes, &mut key, &mut overlapped, INFINITE)
            };
            if (dequeued == 0 && overlapped.is_null()) || key == STOP_KEY {
                return;
            }
            wake.notify();
        }
    });
}

/// Wake the waiter once the process behind `process` has exited.
pub(super) fn watch_exit(process: OwnedHandle, wake: Arc<Wake>) {
    thread::spawn(move || {
        // SAFETY: the owned handle stays open for the wait.
        let _signaled = unsafe { WaitForSingleObject(process.as_raw_handle() as HANDLE, INFINITE) };
        wake.notify();
    });
}

/// The session-local name of the job rooting process group `group`.
fn group_job_name(group: u32) -> Vec<u16> {
    format!(r"Local\sill-process-group-{group}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

fn duration_from_windows_ticks(ticks: i64) -> Duration {
    u64::try_from(ticks)
        .map(|ticks| Duration::from_nanos(ticks.saturating_mul(100)))
        .unwrap_or_default()
}
