//! Windows observers: a change notification on the watched directory and a
//! synchronize handle on the watched process.

use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::Path;
use std::sync::Arc;

use super::Woke;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    FindCloseChangeNotification, FindFirstChangeNotificationW,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentProcessId, GetProcessTimes, INFINITE, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, SetEvent, WaitForMultipleObjects,
    WaitForSingleObject,
};

/// The owner's end of one disarmable wait. Dropping it signals the shared
/// event, which ends the paired wait.
#[must_use = "dropping the disarm ends its wait at once"]
#[derive(Debug)]
pub struct Disarm(Arc<OwnedHandle>);

impl Drop for Disarm {
    fn drop(&mut self) {
        // SAFETY: the shared event handle stays open while either end holds it.
        unsafe { SetEvent(self.0.as_raw_handle()) };
    }
}

/// The waiter's end of one disarmable wait, consumed by `wait_or_disarmed`.
#[derive(Debug)]
pub struct Disarmed(Arc<OwnedHandle>);

/// One owner's [`Disarm`] and the [`Disarmed`] end its waiter consumes.
///
/// # Errors
///
/// Returns the kernel's refusal to create the shared event.
pub fn disarm() -> io::Result<(Disarm, Disarmed)> {
    // SAFETY: an unnamed manual-reset event starts unsignaled and is owned here.
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if event.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateEventW returned this handle and nothing else owns it.
    let event = Arc::new(unsafe { OwnedHandle::from_raw_handle(event) });
    Ok((Disarm(Arc::clone(&event)), Disarmed(event)))
}

/// Block until `handle` is signaled or the disarm event is, preferring the
/// disarm when both are: nobody awaits the change any longer.
fn signaled_or_disarmed(handle: HANDLE, disarmed: &Disarmed) -> io::Result<Woke> {
    let handles = [disarmed.0.as_raw_handle(), handle];
    // SAFETY: both handles stay open for the duration of the wait.
    match unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) } {
        WAIT_OBJECT_0 => Ok(Woke::Disarmed),
        signaled if signaled == WAIT_OBJECT_0 + 1 => Ok(Woke::Fired),
        _ => Err(io::Error::last_os_error()),
    }
}

pub struct DirectObserver {
    notification: HANDLE,
}

// SAFETY: a change-notification handle may be waited on and closed from any
// thread.
unsafe impl Send for DirectObserver {}

impl DirectObserver {
    /// Arm one notification for entries created, removed, renamed, or
    /// rewritten directly in `watched`.
    pub fn arm(watched: &Path) -> io::Result<Self> {
        let path: Vec<u16> = watched
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `path` is NUL-terminated for the call.
        let notification = unsafe {
            FindFirstChangeNotificationW(
                path.as_ptr(),
                0,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_DIR_NAME
                    | FILE_NOTIFY_CHANGE_LAST_WRITE,
            )
        };
        if notification == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { notification })
    }

    /// Block until the watched directory changes once.
    ///
    /// # Errors
    ///
    /// Returns the kernel's refusal to deliver the change.
    pub fn wait(self) -> io::Result<()> {
        signaled(self.notification)
    }

    /// Block until the watched directory changes once or the [`Disarm`]
    /// paired with `disarmed` is dropped.
    ///
    /// # Errors
    ///
    /// Returns the kernel's refusal to deliver the change.
    pub fn wait_or_disarmed(self, disarmed: Disarmed) -> io::Result<Woke> {
        signaled_or_disarmed(self.notification, &disarmed)
    }
}

impl Drop for DirectObserver {
    fn drop(&mut self) {
        // SAFETY: this observer exclusively owns the notification handle.
        unsafe { FindCloseChangeNotification(self.notification) };
    }
}

/// One process's exit, armed while the process can still be named.
#[derive(Debug)]
pub enum ProcessExitObserver {
    Watching(HANDLE),
    Ended,
}

// SAFETY: a process handle may be waited on and closed from any thread.
unsafe impl Send for ProcessExitObserver {}

impl ProcessExitObserver {
    /// This process's parent, or `Ended` once no process holding the parent's
    /// id is the one that started this process.
    ///
    /// # Errors
    ///
    /// Returns the kernel's refusal to name or open the parent.
    pub fn parent() -> io::Result<Self> {
        let parent = parent_id()?;
        // SAFETY: synchronize and limited query access only observe the process.
        let process = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                parent,
            )
        };
        if process.is_null() {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
                Ok(Self::Ended)
            } else {
                Err(error)
            };
        }
        let observer = Self::Watching(process);
        // Windows never reparents, so a process the parent's id names that
        // started after this one only reuses the id of a parent that ended.
        // SAFETY: the pseudo handle names this process and needs no close.
        if created(process)? > created(unsafe { GetCurrentProcess() })? {
            return Ok(Self::Ended);
        }
        Ok(observer)
    }

    pub fn or_ended(pid: u32) -> io::Result<Self> {
        // SAFETY: synchronize access only observes the named process.
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if !process.is_null() {
            return Ok(Self::Watching(process));
        }
        let error = io::Error::last_os_error();
        // Windows names a process that no longer exists an invalid parameter.
        if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            return Ok(Self::Ended);
        }
        Err(error)
    }

    pub fn wait(self) -> io::Result<()> {
        match &self {
            Self::Watching(process) => signaled(*process),
            Self::Ended => Ok(()),
        }
    }

    pub fn wait_or_disarmed(self, disarmed: Disarmed) -> io::Result<Woke> {
        match &self {
            Self::Watching(process) => signaled_or_disarmed(*process, &disarmed),
            Self::Ended => Ok(Woke::Fired),
        }
    }

    pub fn wait_until(&self, deadline: std::time::Instant) -> io::Result<bool> {
        let Self::Watching(process) = self else {
            return Ok(true);
        };
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        // A partial millisecond rounds up so the wait never ends early.
        let milliseconds = remaining
            .as_nanos()
            .div_ceil(1_000_000)
            .min(u128::from(INFINITE - 1)) as u32;
        // SAFETY: the observer owns `process` for the duration of the wait.
        match unsafe { WaitForSingleObject(*process, milliseconds) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

/// The id of the process that started this one, as the kernel's process
/// snapshot records it.
fn parent_id() -> io::Result<u32> {
    // SAFETY: a process snapshot reads no caller memory.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: reads only this process's id.
    let current = unsafe { GetCurrentProcessId() };
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(std::mem::size_of::<PROCESSENTRY32W>())
            .expect("a process entry's size fits its size field"),
        ..Default::default()
    };
    let mut parent = None;
    // SAFETY: `entry` is writable and declares its own size, as each call
    // requires, and `snapshot` stays open across the walk.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while more {
        if entry.th32ProcessID == current {
            parent = Some(entry.th32ParentProcessID);
            break;
        }
        // SAFETY: as above.
        more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    // SAFETY: the walk exclusively owns the snapshot handle.
    unsafe { CloseHandle(snapshot) };
    parent.ok_or_else(|| io::Error::other("this process is missing from its own snapshot"))
}

/// When the process behind `process` started, in the kernel's 100ns ticks.
fn created(process: HANDLE) -> io::Result<u64> {
    let mut times = [FILETIME::default(); 4];
    let [creation, exit, kernel, user] = &mut times;
    // SAFETY: each destination is a writable FILETIME, and `process` grants
    // limited query access.
    if unsafe { GetProcessTimes(process, creation, exit, kernel, user) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(u64::from(times[0].dwHighDateTime) << 32 | u64::from(times[0].dwLowDateTime))
}

impl Drop for ProcessExitObserver {
    fn drop(&mut self) {
        if let Self::Watching(process) = self {
            // SAFETY: this observer exclusively owns the process handle.
            unsafe { CloseHandle(*process) };
        }
    }
}

fn signaled(handle: HANDLE) -> io::Result<()> {
    // SAFETY: the caller owns `handle` for the duration of the wait.
    if unsafe { WaitForSingleObject(handle, INFINITE) } == WAIT_OBJECT_0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
