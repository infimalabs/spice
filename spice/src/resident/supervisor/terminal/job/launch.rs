//! Keeper process ownership at the job-record publication boundary.

use std::ffi::OsString;
#[cfg(test)]
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, ErrorKind};
use std::path::Path;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use sill::Environment;

#[cfg(test)]
use super::Jobs;
use super::detach;
use super::{ENTRY, LOG};

/// What one job start records before its keeper launches.
pub(in crate::resident) struct Launch<'a> {
    pub(in crate::resident) command: &'a str,
    pub(in crate::resident) argv: Vec<OsString>,
    pub(in crate::resident) cwd: &'a Path,
    pub(in crate::resident) environment: &'a Environment,
    pub(in crate::resident) timeout: Option<Duration>,
    pub(in crate::resident) notices: &'a [String],
    pub(in crate::resident) until: Option<&'a str>,
}

/// Whether a keeper launch established process ownership. An unknown outcome
/// preserves its record: deleting a possibly live keeper is never recovery.
pub(super) enum KeeperLaunch {
    Started(u32),
    NotStarted(io::Error),
    Unknown(io::Error),
}

/// Remove a record that no process can own, preserving the launch failure
/// unless removing the record itself needs the caller's attention.
pub(super) fn failed_start_error(
    directory: &Path,
    failure: io::Error,
    cleanup: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Error {
    match cleanup(directory) {
        Ok(()) => failure,
        Err(error) if error.kind() == ErrorKind::NotFound => failure,
        Err(error) => io::Error::new(
            error.kind(),
            format!(
                "{failure}; removing unpublished job {} failed: {error}",
                directory.display()
            ),
        ),
    }
}

/// Launch and reap one keeper from the same dedicated thread. The thread is
/// established before it spawns the process, so setup and process-spawn
/// failures prove no keeper exists. A disconnected ownership report is
/// explicitly unknown and must preserve the published record.
pub(super) fn launch_detached(
    directory: &Path,
    request_path: &Path,
    cwd: &Path,
    environment: &Environment,
) -> KeeperLaunch {
    let prepared = (|| -> io::Result<mpsc::Receiver<io::Result<u32>>> {
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join(LOG))?;
        let stderr = log.try_clone()?;
        let mut command = ProcessCommand::new(std::env::current_exe()?);
        command
            .arg(ENTRY)
            .arg(request_path)
            .current_dir(cwd)
            .env_clear()
            .envs(environment.entries().iter().cloned())
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(stderr);
        detach(&mut command);
        let (launched, result) = mpsc::sync_channel(1);
        let _reaper = thread::Builder::new()
            .name("spice-job-reaper".to_owned())
            .spawn(move || match command.spawn() {
                Ok(mut keeper) => {
                    let pid = keeper.id();
                    let _ = launched.send(Ok(pid));
                    let _ = keeper.wait();
                }
                Err(error) => {
                    let _ = launched.send(Err(error));
                }
            })?;
        Ok(result)
    })();
    let result = match prepared {
        Ok(result) => result,
        Err(error) => return KeeperLaunch::NotStarted(error),
    };
    match result.recv() {
        Ok(Ok(pid)) => KeeperLaunch::Started(pid),
        Ok(Err(error)) => KeeperLaunch::NotStarted(error),
        Err(_) => KeeperLaunch::Unknown(io::Error::new(
            ErrorKind::BrokenPipe,
            "keeper launcher ended before reporting process ownership",
        )),
    }
}

#[cfg(test)]
impl Jobs {
    /// Start one job whose keeper cannot launch, as a missing executable
    /// refuses, and answer its failure plus whether its record survived.
    pub(in crate::resident) fn start_unlaunchable_for_tests(
        &self,
        command: &str,
    ) -> (io::Error, bool) {
        self.launch_outcome_for_tests(
            command,
            KeeperLaunch::NotStarted(io::Error::from(ErrorKind::NotFound)),
            |path| fs::remove_dir_all(path),
        )
    }

    /// Start one job whose keeper and record cleanup both fail.
    pub(in crate::resident) fn start_uncleanable_for_tests(
        &self,
        command: &str,
    ) -> (io::Error, bool) {
        self.launch_outcome_for_tests(
            command,
            KeeperLaunch::NotStarted(io::Error::from(ErrorKind::NotFound)),
            |_| Err(io::Error::from(ErrorKind::PermissionDenied)),
        )
    }

    /// Lose the launcher's ownership report and preserve the possibly owned
    /// record without attempting cleanup.
    pub(in crate::resident) fn start_unknown_for_tests(&self, command: &str) -> (io::Error, bool) {
        self.launch_outcome_for_tests(
            command,
            KeeperLaunch::Unknown(io::Error::from(ErrorKind::BrokenPipe)),
            |_| panic!("unknown process ownership must not clean the record"),
        )
    }

    fn launch_outcome_for_tests(
        &self,
        command: &str,
        outcome: KeeperLaunch,
        cleanup: impl FnOnce(&Path) -> io::Result<()>,
    ) -> (io::Error, bool) {
        let cwd = self.root.clone();
        let mut directory = None;
        let failure = self
            .start_with(
                Launch {
                    command,
                    argv: Vec::new(),
                    cwd: &cwd,
                    environment: &Environment::empty(),
                    timeout: Some(Duration::from_secs(60)),
                    notices: &[],
                    until: None,
                },
                None,
                None,
                |path, _, _, _| {
                    directory = Some(path.to_path_buf());
                    outcome
                },
                cleanup,
            )
            .expect_err("a launch without confirmed ownership fails the start");
        (
            failure,
            directory.is_some_and(|directory| directory.exists()),
        )
    }
}
