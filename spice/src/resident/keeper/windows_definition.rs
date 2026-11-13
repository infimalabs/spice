//! The Task Scheduler definition of one repository's lifecycle owner.
//!
//! The task belongs to the repository user: its principal is that user's
//! SID with an interactive-token logon, so it needs no stored password and
//! everything it creates is owned by that user. Its one trigger is that
//! user's logon, the earliest boundary after a reboot at which the user's
//! profile, credentials, and per-user discovery are available. The action is
//! this executable running as the keeper, which relaunches the resident
//! itself, so the scheduler's own restart policy only covers the keeper
//! crashing.

use std::fmt::Write as _;
use std::path::Path;

use super::manager::xml_escape as escape;

/// Task Scheduler folder every owner lives under.
pub(crate) const FOLDER: &str = "\\Spice";
/// How long the scheduler waits between relaunching a crashed keeper.
const RESTART_INTERVAL: &str = "PT1M";
/// How many times the scheduler relaunches a crashed keeper.
const RESTART_COUNT: u32 = 999;
/// Below-normal thread priority is the scheduler's default; the keeper
/// launches the interactive resident and runs at normal priority instead.
const NORMAL_PRIORITY: u32 = 4;
/// The one private argument vector the keeper runs with.
const ARGUMENTS: &str = super::WORKER_ARGUMENT;

/// What one owner's definition is rendered from.
pub(crate) struct Definition<'a> {
    /// String SID of the repository user the task runs as.
    pub(crate) user_sid: &'a str,
    /// The executable to run; the exact appliance installed at the time.
    pub(crate) executable: &'a Path,
    /// Directory the keeper resolves the repository from.
    pub(crate) working_directory: &'a Path,
    /// Git common directory the owner serves, for the description.
    pub(crate) repository: &'a Path,
}

/// The full task path for the repository whose Git common directory is
/// `common_dir`: stable across installs of the same repository and distinct
/// between repositories.
pub(crate) fn task_path(common_dir: &Path) -> String {
    format!(
        "{FOLDER}\\keeper-{}",
        super::manager::repository_key(common_dir)
    )
}

/// Render the definition as Task Scheduler XML.
pub(crate) fn render(definition: &Definition<'_>) -> String {
    let mut xml = String::new();
    let description = format!(
        "Spice lifecycle owner for the repository at {}",
        definition.repository.display()
    );
    // Every push below writes to a String, which cannot fail.
    let _ = write!(
        xml,
        concat!(
            r#"<?xml version="1.0" encoding="UTF-16"?>"#,
            r#"<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">"#,
            "<RegistrationInfo><Description>{description}</Description></RegistrationInfo>",
            "<Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{sid}</UserId></LogonTrigger></Triggers>",
            r#"<Principals><Principal id="Author"><UserId>{sid}</UserId>"#,
            "<LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>",
            "<Settings>",
            "<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>",
            "<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>",
            "<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>",
            "<AllowHardTerminate>true</AllowHardTerminate>",
            "<StartWhenAvailable>true</StartWhenAvailable>",
            "<RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>",
            "<IdleSettings><StopOnIdleEnd>false</StopOnIdleEnd><RestartOnIdle>false</RestartOnIdle></IdleSettings>",
            "<AllowStartOnDemand>true</AllowStartOnDemand>",
            "<Enabled>true</Enabled>",
            "<Hidden>false</Hidden>",
            "<RunOnlyIfIdle>false</RunOnlyIfIdle>",
            "<WakeToRun>false</WakeToRun>",
            "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>",
            "<Priority>{priority}</Priority>",
            "<RestartOnFailure><Interval>{restart_interval}</Interval><Count>{restart_count}</Count></RestartOnFailure>",
            "</Settings>",
            r#"<Actions Context="Author"><Exec><Command>{command}</Command><Arguments>{arguments}</Arguments>"#,
            "<WorkingDirectory>{working_directory}</WorkingDirectory></Exec></Actions>",
            "</Task>"
        ),
        description = escape(&description),
        sid = escape(definition.user_sid),
        priority = NORMAL_PRIORITY,
        restart_interval = RESTART_INTERVAL,
        restart_count = RESTART_COUNT,
        command = escape(&definition.executable.to_string_lossy()),
        arguments = ARGUMENTS,
        working_directory = escape(&definition.working_directory.to_string_lossy()),
    );
    xml
}
