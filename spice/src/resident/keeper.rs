//! One repository resident's native per-user lifecycle owner.
//!
//! Record identity, relaunch pacing, registration ordering, replacement, and
//! package standing are one host-independent keeper protocol. Host adapters
//! supply only their native definition and per-user manager operations.
//! Explicit `up --keeper` and `down --keeper` are the public control surface;
//! private entry points run the owner and establish Windows package-default
//! standing without exposing another user contract.

/// Exact private argument used only by the native lifecycle registration.
pub(crate) const WORKER_ARGUMENT: &str = "--spice-keeper-worker";
/// Exact private argument by which package apply establishes Default standing.
pub(crate) const PACKAGE_DEFAULT_ARGUMENT: &str = "--spice-keeper-package-default";

/// Whether this process invocation is exactly the private keeper worker.
pub(crate) fn worker_selected(mut arguments: impl Iterator<Item = std::ffi::OsString>) -> bool {
    let _program = arguments.next();
    arguments
        .next()
        .is_some_and(|argument| argument == WORKER_ARGUMENT)
        && arguments.next().is_none()
}

/// Whether this invocation is exactly the private package-default entry point.
pub(crate) fn package_default_selected(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> bool {
    let _program = arguments.next();
    arguments
        .next()
        .is_some_and(|argument| argument == PACKAGE_DEFAULT_ARGUMENT)
        && arguments.next().is_none()
}

mod lifecycle;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "linux", all(test, unix)))]
mod linux_definition;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "macos", test))]
mod macos_definition;
mod manager;
mod owner;
mod ownership;
mod policy;
mod record;
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod unsupported;
#[cfg(windows)]
mod windows;
#[cfg(any(windows, test))]
mod windows_definition;

#[cfg(test)]
#[path = "keeper/tests.rs"]
mod tests;

pub(crate) use lifecycle::{
    admit_owner, finish_unapply, hold_stopped, install, install_default, install_standing, keep,
    owner_installed, quiesce_for_install, uninstall, up_detached,
};

/// An environment failure in keeper work.
fn keeper_fault(reason: &str, error: &dyn std::fmt::Display) -> super::Outcome<()> {
    super::environment_fault(reason, &error.to_string())
}

/// Refuse keeper work because the host's native per-user manager, named by
/// `witness`, cannot be reached.
fn manager_unavailable(
    witness: &str,
    expected: impl Into<String>,
    observed: &dyn std::fmt::Display,
    detail: &str,
) -> super::Outcome<()> {
    super::Outcome::Refusal(super::Refusal::bare(
        wire::RefusalDiagnostic::new(
            "resident::keeper-manager-unavailable".to_owned(),
            Some(witness.to_owned()),
            Some(expected.into()),
            Some(observed.to_string()),
        )
        .with_detail(detail),
    ))
}
