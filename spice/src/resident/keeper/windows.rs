//! Windows Task Scheduler adapter for the shared keeper lifecycle.

use std::path::Path;

use super::super::*;
use super::manager::windows::TaskService;
use super::manager::{Manager, Registration};
use super::windows_definition::{self as definition, Definition};
use super::{keeper_fault, manager_unavailable};

pub(super) fn registration(
    repository: &Path,
    executable: &Path,
    working_directory: &Path,
) -> CommandResult<Registration> {
    let user_sid = door::current_user_sid()
        .map_err(|error| keeper_fault("resident::keeper-principal-unavailable", &error))?;
    Ok(Registration {
        name: definition::task_path(repository),
        definition: definition::render(&Definition {
            user_sid: &user_sid,
            executable,
            working_directory,
            repository,
        }),
    })
}

pub(super) fn connect() -> CommandResult<Box<dyn Manager>> {
    TaskService::connect()
        .map(|service| Box::new(service) as Box<dyn Manager>)
        .map_err(|error| {
            manager_unavailable(
                "task-scheduler",
                "a reachable Task Scheduler service for this user",
                &error,
                "run from the user's own Windows session with the Task Scheduler service running",
            )
        })
}

pub(super) fn package_default() -> Option<spice::install::LifecycleOwnerStanding> {
    Some(spice::install::LifecycleOwnerStanding::Default)
}
