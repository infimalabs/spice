//! Whether a process identifier still names a running process.

use std::io;

#[cfg(windows)]
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};

/// Whether process `pid` is still running. A process this user may not open
/// or signal belongs to someone, so it counts as running.
///
/// # Errors
///
/// Returns any other operating-system refusal opening or polling the process.
#[cfg(unix)]
pub fn process_running(pid: u32) -> io::Result<bool> {
    let pid = i32::try_from(pid).map_err(io::Error::other)?;
    // SAFETY: signal zero changes no process state and only probes this exact PID.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => Err(error),
    }
}

/// Whether process `pid` is still running. A process this user may not open
/// belongs to someone, so it counts as running.
///
/// # Errors
///
/// Returns any other operating-system refusal opening or polling the process.
#[cfg(windows)]
pub fn process_running(pid: u32) -> io::Result<bool> {
    // SAFETY: synchronize access only observes the named process.
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if process.is_null() {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            // Windows names a process that no longer exists an invalid parameter.
            Some(code) if code == ERROR_INVALID_PARAMETER as i32 => Ok(false),
            Some(code) if code == ERROR_ACCESS_DENIED as i32 => Ok(true),
            _ => Err(error),
        };
    }
    // SAFETY: `OpenProcess` succeeded, so this probe exclusively owns the
    // handle, which `OwnedHandle` closes.
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    // SAFETY: the handle is open for the call; a zero timeout only polls.
    match unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } {
        WAIT_TIMEOUT => Ok(true),
        WAIT_OBJECT_0 => Ok(false),
        _ => Err(io::Error::last_os_error()),
    }
}
