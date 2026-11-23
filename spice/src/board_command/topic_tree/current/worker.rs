//! One writer per live registry. Wakes carry no snapshots: a burst consumes
//! one channel slot and the worker obtains the latest journal view when ready.

use super::*;
use std::io;
use std::sync::mpsc;
use std::time::{Duration, Instant};

// Measured from the first edit, not a sliding deadline: sustained writes
// cannot postpone publication indefinitely.
const EDIT_BATCH: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub(super) enum Wake {
    Cold,
    Changed,
}

impl TopicTree {
    pub(crate) fn start(
        self: &Arc<Self>,
        snapshot: impl Fn() -> Option<TaskSnapshot> + Send + 'static,
    ) -> io::Result<()> {
        let _starting = self.published.lock().expect("topic tree lock");
        if self.wake.get().is_some() {
            return Ok(());
        }
        let (wake, requests) = mpsc::sync_channel(1);
        let owner = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("spice-topic-worker".into())
            .spawn(move || {
                while let Ok(first) = requests.recv() {
                    if matches!(first, Wake::Changed) {
                        let deadline = Instant::now() + EDIT_BATCH;
                        while Instant::now() < deadline {
                            match requests
                                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                            {
                                Ok(Wake::Changed) => (),
                                Ok(Wake::Cold) | Err(_) => break,
                            }
                        }
                    }
                    let Some(tree) = owner.upgrade() else {
                        break;
                    };
                    if let Some(observed) = snapshot() {
                        tree.update(&observed);
                    }
                }
            })?;
        self.wake.set(wake).expect("sole topic worker");
        self.signal(Wake::Cold);
        Ok(())
    }

    pub(crate) fn notify(&self) {
        self.signal(Wake::Changed);
    }

    pub(super) fn signal(&self, reason: Wake) {
        if let Some(wake) = self.wake.get() {
            let _ = wake.try_send(reason);
        }
    }

    /// Publish complete generations in journal order. Background refreshes
    /// and command-side catch-up share this lock, so an older computation can
    /// never overwrite a newer generation.
    pub(in crate::board_command) fn update(&self, observed: &TaskSnapshot) {
        let mut published = self.published.lock().expect("topic tree lock");
        let state = match published.as_ref() {
            Some(prior) if prior.through >= observed.cursor() => return,
            Some(prior) => {
                let mut state = (**prior).clone();
                state.advance(observed);
                state
            }
            None => Generation::build(observed, GroupingRecovery::Cold),
        };
        *published = Some(Arc::new(state));
    }
}
