//! Platform-native process statuses for fixtures that classify completions.
//!
//! A Unix wait status and a Windows exit code encode an ordinary exit
//! differently, and only Unix can report termination by a signal. Fixtures
//! build statuses here so each assertion states the completion it means rather
//! than one platform's encoding of it.

use std::process::ExitStatus;

/// An ordinary exit with `code`.
pub(crate) fn exited(code: i32) -> ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        ExitStatus::from_raw(code << 8)
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt as _;
        ExitStatus::from_raw(code.cast_unsigned())
    }
}

/// The status a supervisor's forced termination leaves: SIGKILL on Unix, and
/// on Windows the exit code `sill` hands `TerminateJobObject`.
pub(crate) fn terminated() -> ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        ExitStatus::from_raw(9)
    }
    #[cfg(windows)]
    {
        exited(1)
    }
}
