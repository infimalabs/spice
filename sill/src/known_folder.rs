//! System-assigned Windows folders, answered by the shell rather than by the
//! process environment.

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt as _;
use std::path::PathBuf;
use std::ptr;

use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::{FOLDERID_ProgramFiles, SHGetKnownFolderPath};

/// The Program Files directory matching this process's architecture.
///
/// # Errors
///
/// Returns the shell's refusal resolving the folder.
pub fn program_files() -> io::Result<PathBuf> {
    let mut path = ptr::null_mut();
    // SAFETY: the folder identifier outlives the call, a null token names the
    // current user, and `path` receives a shell allocation released below.
    let result =
        unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramFiles, 0, ptr::null_mut(), &mut path) };
    let resolved = if result < 0 {
        Err(io::Error::from_raw_os_error(result))
    } else {
        // SAFETY: on success `path` is a NUL-terminated UTF-16 string.
        let length = (0..)
            .take_while(|&index| unsafe { *path.add(index) } != 0)
            .count();
        // SAFETY: the `length` units before the terminator are initialized.
        let units = unsafe { std::slice::from_raw_parts(path, length) };
        Ok(PathBuf::from(OsString::from_wide(units)))
    };
    // SAFETY: the shell allocated `path` (or left it null) for the caller to
    // release exactly once; `CoTaskMemFree` accepts null.
    unsafe { CoTaskMemFree(path.cast()) };
    resolved
}
