//! Linux systemd user-manager adapter for the shared keeper lifecycle.

use std::path::Path;

use super::super::*;
use super::linux_definition::{self, Definition};
use super::manager::linux::SystemdUser;
use super::manager::{Manager, Registration};
use super::manager_unavailable;

pub(super) fn registration(
    repository: &Path,
    executable: &Path,
    working_directory: &Path,
) -> CommandResult<Registration> {
    let definition = linux_definition::render(&Definition {
        executable,
        working_directory,
        repository,
    })
    .map_err(|refused| {
        Outcome::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "resident::keeper-path-unrepresentable".to_owned(),
                Some("systemd-unit".to_owned()),
                Some("a path one systemd unit line carries verbatim".to_owned()),
                Some(refused.to_string()),
            )
            .with_detail(
                "systemd reads this directive verbatim and cannot express the path; run \
                 `spice up --keeper` from a worktree path without line breaks, surrounding \
                 whitespace, or a trailing backslash",
            ),
        ))
    })?;
    Ok(Registration {
        name: linux_definition::service_name(repository),
        definition,
    })
}

pub(super) fn connect() -> CommandResult<Box<dyn Manager>> {
    SystemdUser::connect()
        .map(|manager| Box::new(manager) as Box<dyn Manager>)
        .map_err(|error| {
            manager_unavailable(
                "systemd-user-manager",
                "a reachable systemd user manager and user bus",
                &error,
                "run from a logged-in user session with `systemctl --user`; for boot or \
                 post-logout persistence an administrator may enable loginctl linger for this \
                 user, but Spice never changes account linger policy",
            )
        })
}

pub(super) fn package_default() -> Option<spice::install::LifecycleOwnerStanding> {
    None
}
