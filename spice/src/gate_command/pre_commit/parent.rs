//! Direct parent-exit cancellation for the pre-commit supervisor.

use std::io;
use std::thread;

use sill::{Cancellation, CancellationToken};

use crate::resident::ProcessExitObserver;

pub(super) struct ParentCancellation {
    cancellation: Cancellation,
}

impl ParentCancellation {
    pub(super) fn capture() -> io::Result<Self> {
        let token = CancellationToken::new();
        match ProcessExitObserver::parent()? {
            ProcessExitObserver::Ended => token.cancel(),
            observer => {
                let cancellation = token.clone();
                thread::Builder::new()
                    .name("spice-pre-commit-parent".to_owned())
                    .spawn(move || {
                        let _ = observer.wait();
                        cancellation.cancel();
                    })?;
            }
        }
        Ok(Self {
            cancellation: Cancellation::Token(token),
        })
    }

    pub(super) fn cancellation(&self) -> Cancellation {
        self.cancellation.clone()
    }
}
