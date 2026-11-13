//! LaunchAgents in the caller's launchd GUI domain, driven through `launchctl`.
//!
//! A registration is the owner-only property list in the user's
//! `Library/LaunchAgents`; launchd's disabled-service override is whether it may
//! start; a job loaded in `gui/UID` is what runs. The job runs as the user in
//! their own login session, so nothing here needs a password or root.

use std::fs;
use std::io;
use std::path::PathBuf;

use super::super::record::replace_private;
use super::{Manager, Status};

/// `launchctl print` status for a domain launchd does not hold.
const DOMAIN_ABSENT: i32 = 112;
/// `launchctl` status for a service its domain does not hold.
const SERVICE_ABSENT: i32 = 113;
/// `launchctl bootout` status (`ESRCH`) for a job that is not loaded.
const NOT_LOADED: i32 = 3;

/// What one `launchctl` invocation answered.
pub(crate) struct Answer {
    /// Exit status; `-1` when a signal ended it.
    pub(crate) code: i32,
    /// Standard output.
    pub(crate) stdout: String,
    /// Standard error.
    pub(crate) stderr: String,
}

/// One way to invoke `launchctl`.
pub(crate) trait Launchctl {
    /// Run `launchctl` with `arguments` and report what it answered.
    fn call(&mut self, arguments: &[&str]) -> io::Result<Answer>;
}

/// The system's `launchctl`.
#[cfg(target_os = "macos")]
pub(crate) struct SystemLaunchctl;

#[cfg(target_os = "macos")]
impl Launchctl for SystemLaunchctl {
    fn call(&mut self, arguments: &[&str]) -> io::Result<Answer> {
        let output = std::process::Command::new("/bin/launchctl")
            .args(arguments)
            .output()?;
        Ok(Answer {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// One user's LaunchAgents in their `gui/UID` domain.
pub(crate) struct LaunchAgents<L> {
    launchctl: L,
    domain: String,
    directory: PathBuf,
}

impl<L: Launchctl> LaunchAgents<L> {
    /// Agents of user `uid` whose property lists live in `directory`.
    pub(crate) fn new(launchctl: L, uid: u32, directory: PathBuf) -> Self {
        Self {
            launchctl,
            domain: format!("gui/{uid}"),
            directory,
        }
    }

    /// Whether the user's GUI login domain exists; only a login session has one.
    ///
    /// # Errors
    ///
    /// Returns a `launchctl` failure other than the domain being absent.
    pub(crate) fn domain_available(&mut self) -> io::Result<bool> {
        let answer = self.launchctl.call(&["print", &self.domain])?;
        match answer.code {
            0 => Ok(true),
            DOMAIN_ABSENT => Ok(false),
            _ => Err(failure(&["print", &self.domain], &answer)),
        }
    }

    fn agent(&self, label: &str) -> PathBuf {
        self.directory.join(format!("{label}.plist"))
    }

    fn service(&self, label: &str) -> String {
        format!("{}/{label}", self.domain)
    }

    /// The loaded job's report, or `None` when the domain does not hold it.
    fn loaded(&mut self, label: &str) -> io::Result<Option<String>> {
        let service = self.service(label);
        let answer = self.launchctl.call(&["print", &service])?;
        match answer.code {
            0 => Ok(Some(answer.stdout)),
            SERVICE_ABSENT => Ok(None),
            _ => Err(failure(&["print", &service], &answer)),
        }
    }

    fn checked(&mut self, arguments: &[&str]) -> io::Result<Answer> {
        let answer = self.launchctl.call(arguments)?;
        if answer.code == 0 {
            Ok(answer)
        } else {
            Err(failure(arguments, &answer))
        }
    }

    fn bootstrap(&mut self, label: &str) -> io::Result<()> {
        let domain = self.domain.clone();
        let agent = self.agent(label);
        self.checked(&["bootstrap", &domain, &agent.to_string_lossy()])
            .map(drop)
    }
}

impl<L: Launchctl> Manager for LaunchAgents<L> {
    fn register(&mut self, label: &str, definition: &str) -> io::Result<()> {
        let agent = self.agent(label);
        let changed = match fs::read(&agent) {
            Ok(current) => current != definition.as_bytes(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => true,
            Err(error) => return Err(error),
        };
        if changed {
            fs::create_dir_all(&self.directory)?;
            replace_private(&agent, definition.as_bytes())?;
        }
        // launchd refuses to bootstrap a disabled service.
        self.set_enabled(label, true)?;
        match self.loaded(label)? {
            // An unchanged definition leaves a running keeper alone.
            Some(_) if !changed => Ok(()),
            Some(_) => {
                self.stop(label)?;
                self.bootstrap(label)
            }
            None => self.bootstrap(label),
        }
    }

    fn status(&mut self, label: &str) -> io::Result<Option<Status>> {
        match fs::symlink_metadata(self.agent(label)) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
        let domain = self.domain.clone();
        let overrides = self.checked(&["print-disabled", &domain])?.stdout;
        let running = self.loaded(label)?.is_some_and(|report| running(&report));
        Ok(Some(Status {
            enabled: !disabled(&overrides, label),
            running,
        }))
    }

    fn set_enabled(&mut self, label: &str, enabled: bool) -> io::Result<()> {
        let service = self.service(label);
        let verb = if enabled { "enable" } else { "disable" };
        self.checked(&[verb, &service]).map(drop)
    }

    fn run(&mut self, label: &str) -> io::Result<()> {
        if self.loaded(label)?.is_none() {
            // Loading starts it: the definition runs at load.
            return self.bootstrap(label);
        }
        let service = self.service(label);
        self.checked(&["kickstart", &service]).map(drop)
    }

    fn stop(&mut self, label: &str) -> io::Result<()> {
        let service = self.service(label);
        let answer = self.launchctl.call(&["bootout", &service])?;
        match answer.code {
            0 | NOT_LOADED | SERVICE_ABSENT => Ok(()),
            _ => Err(failure(&["bootout", &service], &answer)),
        }
    }

    fn delete(&mut self, label: &str) -> io::Result<()> {
        match fs::remove_file(self.agent(label)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// Whether launchd's override list `overrides` disables `label`; a label it
/// does not name is enabled. Current releases print `disabled`, earlier ones
/// `true`.
pub(crate) fn disabled(overrides: &str, label: &str) -> bool {
    let quoted = format!("\"{label}\"");
    overrides.lines().any(|line| {
        line.trim().split_once("=>").is_some_and(|(name, state)| {
            name.trim() == quoted && matches!(state.trim(), "disabled" | "true")
        })
    })
}

/// Whether a loaded job's report names a process executing it now.
pub(crate) fn running(report: &str) -> bool {
    report.lines().any(|line| {
        line.strip_prefix('\t')
            .filter(|property| !property.starts_with('\t'))
            .and_then(|property| property.strip_prefix("pid = "))
            .is_some_and(|pid| pid.trim().parse::<u32>().is_ok())
    })
}

fn failure(arguments: &[&str], answer: &Answer) -> io::Error {
    io::Error::other(format!(
        "launchctl {} exited {}: {}",
        arguments.join(" "),
        answer.code,
        answer.stderr.trim()
    ))
}

#[cfg(test)]
mod tests;
