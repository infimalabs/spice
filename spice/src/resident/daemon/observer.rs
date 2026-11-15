//! Channel wiring over the direct filesystem and process-exit observers.

use std::io;
use std::sync::mpsc;
use std::thread;

use sill::Woke;
pub(crate) use sill::{DirectObserver, Disarm, ProcessExitObserver};

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

/// Send what `signal` makes of the armed observer's wake, once it wakes,
/// unless the returned [`Disarm`] is dropped first.
///
/// The observer is armed before the reader looks, so a change between the
/// look and the wait is not lost; it fires once, and the reader arms another
/// after each wake. Dropping the disarm ends a wait nobody follows any longer
/// without sending, releasing the observer's descriptors and its thread.
///
/// # Errors
///
/// Returns the kernel's refusal to create the disarm.
pub(in crate::resident) fn wait_for_change<T: Send + 'static>(
    observer: DirectObserver,
    sender: mpsc::Sender<T>,
    signal: impl FnOnce(io::Result<()>) -> T + Send + 'static,
) -> io::Result<Disarm> {
    let (disarm, disarmed) = sill::disarm()?;
    thread::spawn(move || {
        let woke = match observer.wait_or_disarmed(disarmed) {
            Ok(Woke::Fired) => Ok(()),
            Ok(Woke::Disarmed) => return,
            Err(error) => Err(error),
        };
        let _ = sender.send(signal(woke));
    });
    Ok(disarm)
}

/// Tell `sender` of process `pid`'s exit unless the returned [`Disarm`] is
/// dropped first, which ends the wait without telling. A disarm the kernel
/// cannot create reaches `sender` as the observer's failure, so `None`
/// still answers.
pub(in crate::resident) fn wait_for_exit(pid: u32, sender: impl ExitSender) -> Option<Disarm> {
    let (disarm, disarmed) = match sill::disarm() {
        Ok(pair) => pair,
        Err(error) => {
            sender.failed(error.to_string());
            return None;
        }
    };
    thread::spawn(move || {
        match ProcessExitObserver::or_ended(pid).and_then(|exit| exit.wait_or_disarmed(disarmed)) {
            Ok(Woke::Fired) => sender.exited(),
            Ok(Woke::Disarmed) => {}
            Err(error) => sender.failed(error.to_string()),
        }
    });
    Some(disarm)
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::*;

    /// A follow that ends before its directory or process changes leaves
    /// nothing waiting: each disarm ends its wait unsent, and the wait's
    /// thread, which owns the observer, ends with it. Every thread holds a
    /// sender clone, so disconnection proves all of them ended.
    #[test]
    fn disarmed_watches_end_unsent_and_release_their_observers() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("fixture time")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("observer-disarm-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&root).expect("fixture directory creates");

        let (sender, receiver) = mpsc::channel();
        for _ in 0..64 {
            let observer = DirectObserver::arm(&root).expect("directory observer arms");
            drop(wait_for_change(observer, sender.clone(), Signal::woke).expect("watch arms"));
            drop(wait_for_exit(std::process::id(), sender.clone()));
        }
        drop(sender);
        assert!(
            matches!(
                receiver.recv_timeout(Duration::from_secs(10)),
                Err(mpsc::RecvTimeoutError::Disconnected)
            ),
            "a disarmed watch sent or outlived its follow"
        );

        let (sender, receiver) = mpsc::channel();
        let observer = DirectObserver::arm(&root).expect("directory observer arms");
        let _armed = wait_for_change(observer, sender, Signal::woke).expect("watch arms");
        fs::write(root.join("changed"), b"").expect("fixture change writes");
        assert!(
            matches!(
                receiver.recv_timeout(Duration::from_secs(10)),
                Ok(Signal::Changed)
            ),
            "an armed watch reports its directory's change"
        );
        fs::remove_dir_all(root).expect("fixture cleanup");
    }
}
