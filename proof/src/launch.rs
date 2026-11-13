//! The one boundary where proof hands a bounded process to its host.

use std::fmt;
use std::io;

use sill::{Completion, Spec, WaitError};

use crate::container::{Captured, Execution, ExecutionCompletion};

/// Runs one bounded process to completion and reports what it observed.
///
/// Every runtime probe, battery member, and native runner exchange crosses
/// this boundary, so admission, attestation, and lifecycle decisions are made
/// over the returned [`Execution`] alone.
pub(crate) trait Launch: fmt::Debug + Send + Sync {
    fn run(&self, spec: Spec) -> Result<Execution, LaunchError>;
}

/// Why a bounded process produced no [`Execution`].
#[derive(Debug)]
pub(crate) enum LaunchError {
    Spawn(io::Error),
    Wait(WaitError),
    MissingCapture(&'static str),
}

/// The host operating system, reached through `sill`.
#[derive(Debug)]
pub(crate) struct System;

impl Launch for System {
    fn run(&self, spec: Spec) -> Result<Execution, LaunchError> {
        let outcome = sill::spawn(spec)
            .map_err(LaunchError::Spawn)?
            .wait()
            .map_err(LaunchError::Wait)?;
        let completion = match outcome.completion() {
            Completion::Exited(status) => ExecutionCompletion::Exited {
                success: status.success(),
                code: status.code(),
            },
            Completion::TimedOut(status) => ExecutionCompletion::TimedOut {
                code: status.code(),
            },
            Completion::Cancelled(status) => ExecutionCompletion::Cancelled {
                code: status.code(),
            },
        };
        let stdout = outcome
            .stdout()
            .map(Captured::from)
            .ok_or(LaunchError::MissingCapture("stdout"))?;
        let stderr = outcome
            .stderr()
            .map(Captured::from)
            .ok_or(LaunchError::MissingCapture("stderr"))?;
        Ok(Execution {
            completion,
            stdout,
            stderr,
        })
    }
}
