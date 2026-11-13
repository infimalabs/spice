//! The adapter for hosts without a native per-user manager binding: every
//! registration and connection refuses by name, and package apply selects no
//! owner, so no keeper record is ever written here.

use std::path::Path;

use super::super::*;
use super::manager::{Manager, Registration};
use super::manager_unavailable;

pub(super) fn registration(
    _repository: &Path,
    _executable: &Path,
    _working_directory: &Path,
) -> CommandResult<Registration> {
    Err(refusal())
}

pub(super) fn connect() -> CommandResult<Box<dyn Manager>> {
    Err(refusal())
}

pub(super) fn package_default() -> Option<spice::install::LifecycleOwnerStanding> {
    None
}

fn refusal() -> Outcome<()> {
    manager_unavailable(
        "native-user-manager",
        "a supported per-user lifecycle manager",
        &std::env::consts::OS,
        "this release has no native per-user keeper adapter for the current host",
    )
}
