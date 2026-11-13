//! macOS LaunchAgent adapter for the shared keeper lifecycle.

use std::ffi::{CStr, OsStr};
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use super::super::*;
use super::macos_definition::{self as definition, Definition};
use super::manager::launchd::{LaunchAgents, SystemLaunchctl};
use super::manager::{Manager, Registration};
use super::{keeper_fault, manager_unavailable};

/// Size of the scratch buffer an account lookup decodes into.
const ACCOUNT_BUFFER_BYTES: usize = 16 * 1024;

pub(super) fn registration(
    repository: &Path,
    executable: &Path,
    working_directory: &Path,
) -> CommandResult<Registration> {
    let (uid, mut agents) = agents()?;
    match agents.domain_available() {
        Ok(true) => {}
        Ok(false) => return Err(login_session_refusal(uid)),
        Err(error) => {
            return Err(manager_unavailable(
                "launchd-domain",
                format!("a reachable gui/{uid} launchd domain"),
                &error,
                "run from the user's logged-in macOS session, where launchctl reaches its GUI domain",
            ));
        }
    }
    let label = definition::label(repository);
    let definition = definition::render(&Definition {
        label: &label,
        executable,
        working_directory,
    });
    Ok(Registration {
        name: label,
        definition,
    })
}

pub(super) fn connect() -> CommandResult<Box<dyn Manager>> {
    agents().map(|(_, agents)| Box::new(agents) as Box<dyn Manager>)
}

pub(super) fn package_default() -> Option<spice::install::LifecycleOwnerStanding> {
    None
}

/// The effective user's LaunchAgents in their GUI domain.
fn agents() -> CommandResult<(u32, LaunchAgents<SystemLaunchctl>)> {
    // SAFETY: `geteuid` reads this process's credentials and cannot fail.
    let uid = unsafe { libc::geteuid() };
    let home = account_home(uid)
        .map_err(|error| keeper_fault("resident::keeper-principal-unavailable", &error))?;
    Ok((
        uid,
        LaunchAgents::new(
            SystemLaunchctl,
            uid,
            home.join("Library").join("LaunchAgents"),
        ),
    ))
}

/// The home directory the account database records for `uid`; launchd loads
/// that user's agents from it whatever the caller's environment says.
fn account_home(uid: libc::uid_t) -> io::Result<PathBuf> {
    let mut buffer = vec![0 as libc::c_char; ACCOUNT_BUFFER_BYTES];
    // SAFETY: `passwd` is plain old data; `getpwuid_r` fills it before use.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found = std::ptr::null_mut();
    // SAFETY: every pointer names a live, writable value of the size given,
    // and the strings it stores point into `buffer`, which outlives `entry`.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            &raw mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &raw mut found,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status));
    }
    if found.is_null() || entry.pw_dir.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("the account database has no home directory for uid {uid}"),
        ));
    }
    // SAFETY: `getpwuid_r` succeeded, so `pw_dir` is a NUL-terminated string
    // in `buffer`, which is still alive.
    let home = unsafe { CStr::from_ptr(entry.pw_dir) };
    Ok(PathBuf::from(OsStr::from_bytes(home.to_bytes())))
}

fn login_session_refusal(uid: u32) -> Outcome<()> {
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            "resident::keeper-login-session-required".to_owned(),
            Some("launchd-domain".to_owned()),
            Some(format!("gui/{uid}")),
            Some("absent".to_owned()),
        )
        .with_detail(
            "a LaunchAgent runs only in the user's GUI login session, which this Mac does not \
             have for this user now; log in to the desktop as this user (an SSH session alone \
             has none), then rerun the command",
        ),
    ))
}
