//! One reaped analysis child as the toolchain reducers read it.
//!
//! Launching and reaping belong to `sill`; deciding what a bounded result means
//! belongs to the arm. The view borrows the reaped bytes, so a reducer can be
//! handed a real outcome or an exact captured fixture without either one
//! copying output or starting a process.

use std::fmt;

use sill::{CapturedOutput, Completion, WaitOutcome};

/// One bounded captured stream.
#[derive(Clone, Copy, Debug)]
pub(in crate::toolchain) struct Stream<'a> {
    pub(in crate::toolchain) bytes: &'a [u8],
    pub(in crate::toolchain) truncated: bool,
    pub(in crate::toolchain) complete: bool,
}

impl<'a> Stream<'a> {
    fn of(output: &'a CapturedOutput) -> Self {
        Self {
            bytes: output.bytes(),
            truncated: output.truncated(),
            complete: output.complete(),
        }
    }

    /// Whether every byte the child wrote was read.
    pub(in crate::toolchain) const fn whole(self) -> bool {
        !self.truncated && self.complete
    }
}

/// How one child ended and what its bounded streams retained.
pub(in crate::toolchain) struct Captured<'a> {
    pub(in crate::toolchain) completion: &'a Completion,
    pub(in crate::toolchain) observation_failure: Option<&'a dyn fmt::Display>,
    pub(in crate::toolchain) stdout: Option<Stream<'a>>,
    pub(in crate::toolchain) stderr: Option<Stream<'a>>,
}

impl<'a> Captured<'a> {
    pub(in crate::toolchain) fn of(outcome: &'a WaitOutcome) -> Self {
        Self {
            completion: outcome.completion(),
            observation_failure: outcome
                .observation_failure()
                .map(|failure| failure as &dyn fmt::Display),
            stdout: outcome.stdout().map(Stream::of),
            stderr: outcome.stderr().map(Stream::of),
        }
    }
}
