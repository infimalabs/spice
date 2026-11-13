//! Ordered native lifecycle transitions shared by install, apply, stop, and removal.

use std::fs;
use std::io;
use std::path::Path;

use super::manager::Manager;

/// Register or refresh the native definition, allow it, and start it; an
/// instance already running is left alone.
///
/// # Errors
///
/// Returns the manager's refusal at the first step that fails.
pub(crate) fn apply(
    manager: &mut dyn Manager,
    registration: &str,
    definition: &str,
) -> io::Result<()> {
    manager.register(registration, definition)?;
    manager.set_enabled(registration, true)?;
    manager.run(registration)
}

/// Forbid the manager to start the keeper again; an absent registration is
/// already held.
///
/// This runs before a resident is drained, so the keeper finds the
/// registration disabled when that resident exits and stops rather than
/// relaunching.
///
/// # Errors
///
/// Returns the manager's refusal to change the registration.
pub(crate) fn hold(manager: &mut dyn Manager, registration: &str) -> io::Result<()> {
    if manager.status(registration)?.is_none() {
        return Ok(());
    }
    manager.set_enabled(registration, false)
}

/// End any keeper still running and remove the registration.
///
/// # Errors
///
/// Returns the manager's refusal at the first step that fails.
pub(crate) fn release(manager: &mut dyn Manager, registration: &str) -> io::Result<()> {
    manager.stop(registration)?;
    manager.delete(registration)
}

/// Remove the owner's files; absent entries are fine.
///
/// # Errors
///
/// Returns the first removal failure other than a missing entry.
pub(crate) fn remove_artifacts(paths: &[&Path]) -> io::Result<()> {
    for path in paths {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
