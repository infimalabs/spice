//! Channel wiring over the direct filesystem and process-exit observers.

use std::io;
use std::sync::mpsc;
use std::thread;

pub(crate) use sill::{DirectObserver, ProcessExitObserver};

pub(super) enum Signal {
    Changed,
    Exited,
    ObserverFailed(String),
}

impl Signal {
    pub(super) fn woke(woke: io::Result<()>) -> Self {
        woke.map_or_else(
            |error| Self::ObserverFailed(error.to_string()),
            |()| Self::Changed,
        )
    }
}

/// Send what `signal` makes of the armed observer's wake, once it wakes.
///
/// The observer is armed before the reader looks, so a change between the
/// look and the wait is not lost; it fires once, and the reader arms another
/// after each wake. A watch the directory never wakes again holds its thread
/// until this process ends.
pub(in crate::resident) fn wait_for_change<T: Send + 'static>(
    observer: DirectObserver,
    sender: mpsc::Sender<T>,
    signal: impl FnOnce(io::Result<()>) -> T + Send + 'static,
) {
    thread::spawn(move || {
        let _ = sender.send(signal(observer.wait()));
    });
}

pub(in crate::resident) fn wait_for_exit(pid: u32, sender: impl ExitSender) {
    thread::spawn(move || {
        match ProcessExitObserver::or_ended(pid).and_then(ProcessExitObserver::wait) {
            Ok(()) => sender.exited(),
            Err(error) => sender.failed(error.to_string()),
        }
    });
}

pub(in crate::resident) trait ExitSender: Send + 'static {
    fn exited(self);
    fn failed(self, error: String);
}

impl ExitSender for mpsc::Sender<Signal> {
    fn exited(self) {
        let _ = self.send(Signal::Exited);
    }

    fn failed(self, error: String) {
        let _ = self.send(Signal::ObserverFailed(error));
    }
}

impl ExitSender for mpsc::SyncSender<()> {
    fn exited(self) {
        let _ = self.send(());
    }

    fn failed(self, _error: String) {
        let _ = self.send(());
    }
}
