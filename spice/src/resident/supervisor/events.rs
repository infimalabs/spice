//! Blocking state and process-exit observation for detached supervisors.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant as Monotonic};

use sill::{WaitError, WaitOutcome};

use super::Handle;
use crate::resident::daemon::observer::{self, DirectObserver};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum StateSignal {
    Changed,
    SupervisorExited,
    ObserverFailed(String),
}

impl StateSignal {
    fn woke(woke: io::Result<()>) -> Self {
        woke.map_or_else(
            |error| Self::ObserverFailed(error.to_string()),
            |()| Self::Changed,
        )
    }
}

impl observer::ExitSender for mpsc::Sender<StateSignal> {
    fn exited(self) {
        let _ = self.send(StateSignal::SupervisorExited);
    }

    fn failed(self, error: String) {
        let _ = self.send(StateSignal::ObserverFailed(error));
    }
}

/// The state file's directory, watched directly; each wake re-arms the watch
/// before the reader looks, so what changes between a look and the next
/// wait is not lost.
pub(super) struct StateEvents {
    directory: PathBuf,
    sender: mpsc::Sender<StateSignal>,
    receiver: mpsc::Receiver<StateSignal>,
}

impl StateEvents {
    pub(super) fn arm(path: &Path, supervisor: u32) -> io::Result<Self> {
        let events = Self::watch(path)?;
        events.observe_exit(supervisor);
        Ok(events)
    }

    pub(super) fn watch(path: &Path) -> io::Result<Self> {
        let directory = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "supervisor state path has no parent directory",
            )
        })?;
        let (sender, receiver) = mpsc::channel();
        let events = Self {
            directory: directory.to_path_buf(),
            sender,
            receiver,
        };
        events.arm_watch()?;
        Ok(events)
    }

    pub(super) fn observe_exit(&self, supervisor: u32) {
        observer::wait_for_exit(supervisor, self.sender.clone());
    }

    fn arm_watch(&self) -> io::Result<()> {
        let observer = DirectObserver::arm(&self.directory)?;
        observer::wait_for_change(observer, self.sender.clone(), StateSignal::woke);
        Ok(())
    }

    /// Block on the predecessor's kernel exit, not an agent or launch timer.
    pub(super) fn wait_for_exit(&self) -> io::Result<()> {
        loop {
            match self.wait()? {
                StateSignal::SupervisorExited => return Ok(()),
                StateSignal::ObserverFailed(error) => return Err(io::Error::other(error)),
                StateSignal::Changed => {}
            }
        }
    }
    fn wait(&self) -> io::Result<StateSignal> {
        let signal = self.receiver.recv().map_err(|_| disconnected())?;
        self.woken(signal)
    }

    pub(super) fn wait_for(&self, duration: Duration) -> io::Result<Option<StateSignal>> {
        match self.receiver.recv_timeout(duration) {
            Ok(signal) => self.woken(signal).map(Some),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(disconnected()),
        }
    }

    /// A change reaches the reader with the next watch already armed.
    fn woken(&self, signal: StateSignal) -> io::Result<StateSignal> {
        if signal == StateSignal::Changed {
            self.arm_watch()?;
        }
        Ok(signal)
    }
}

fn disconnected() -> io::Error {
    io::Error::new(
        io::ErrorKind::BrokenPipe,
        "supervisor state observer disconnected",
    )
}

impl Handle {
    fn wait_event(&self, duration: Option<Duration>) -> Result<Option<StateSignal>, WaitError> {
        let events = self.events.as_ref().ok_or_else(|| {
            self.wait_error(
                "waiting for supervisor state change",
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "running supervisor handle has no state observer",
                ),
            )
        })?;
        match duration {
            Some(duration) => events.wait_for(duration),
            None => events.wait().map(Some),
        }
        .map_err(|error| self.wait_error("waiting for supervisor state change", error))
    }

    pub(super) fn observe_until(
        &self,
        duration: Option<Duration>,
    ) -> Result<Option<WaitOutcome>, WaitError> {
        let started = Monotonic::now();
        loop {
            if let Some(outcome) = self.terminal()? {
                return Ok(Some(outcome));
            }
            let remaining = duration.map(|duration| duration.saturating_sub(started.elapsed()));
            if remaining.is_some_and(|remaining| remaining.is_zero()) {
                return Ok(None);
            }
            match self.wait_event(remaining)? {
                Some(StateSignal::Changed) => {}
                Some(StateSignal::SupervisorExited) => {
                    if let Some(outcome) = self.terminal()? {
                        return Ok(Some(outcome));
                    }
                    return Err(self.wait_error(
                        "observing supervisor",
                        io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "supervisor exited before publishing an ending",
                        ),
                    ));
                }
                Some(StateSignal::ObserverFailed(error)) => {
                    return Err(self.wait_error(
                        "waiting for supervisor state change",
                        io::Error::other(error),
                    ));
                }
                None => return Ok(None),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::resident::supervisor::{now_millis, publish_json};

    #[test]
    fn atomic_state_publication_wakes_the_observer() {
        let root = std::env::temp_dir().join(format!(
            "spice-supervisor-state-events-{}-{}",
            std::process::id(),
            now_millis().expect("fixture time")
        ));
        fs::create_dir_all(&root).expect("fixture directory creates");
        let path = root.join("state.json");
        let events = StateEvents::watch(&path).expect("state observer arms");

        publish_json(&path, &serde_json::json!({ "phase": "running" }))
            .expect("atomic state fixture publishes");

        assert_eq!(
            events
                .wait_for(Duration::from_secs(2))
                .expect("state observer remains connected"),
            Some(StateSignal::Changed)
        );
        drop(events);
        fs::remove_dir_all(root).expect("fixture cleanup");
    }
}
