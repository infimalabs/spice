//! The one replay core that sealing and re-derivation share.
//!
//! [`Staged::admit`] admits the selected payload and derives its receipt
//! layout before any environment exists. [`Staged::replay`] selects the
//! target environment, binds it to the payload's target, executes the complete
//! current battery once, and admits only clean standing. [`Proved::verify`]
//! binds an admitted receipt to that exact payload and replay. Sealing then
//! creates and publishes a new receipt; re-derivation verifies the stored one
//! without effects.

use std::error::Error;
use std::fmt;
use std::path::Path;

use wire::manifest::{ProofStanding, ReleaseTarget};

use crate::config::Config;
use crate::container::Limits;
use crate::environment::{EnvironmentError, SealedEnvironment};
use crate::evidence::{AdmittedReceipt, EvidenceError, VerifiedEvidence, verify_admitted};
use crate::layout::{InputError, Layout, LayoutError, ReceiptInput};
use crate::payload::{AdmittedPayload, PayloadError, admit};
use crate::replay::{ReplayError, ReplayVerdict, replay};

#[cfg(test)]
pub(crate) mod fixture;
#[cfg(test)]
mod tests;

/// One admitted payload and the detached-receipt location its layout selects,
/// before any environment exists.
pub(crate) struct Staged {
    payload: AdmittedPayload,
    receipt: ReceiptInput,
}

impl Staged {
    /// Admit the payload `layout` selects and derive its receipt location.
    pub(crate) fn admit(layout: &Layout) -> Result<Self, ProofError> {
        let bytes = layout.read_payload().map_err(ProofError::ReadPayload)?;
        let payload = admit(&bytes).map_err(ProofError::AdmitPayload)?;
        let receipt = layout
            .receipt_for(payload.value())
            .map_err(ProofError::Layout)?;
        Ok(Self { payload, receipt })
    }

    /// The exact detached-receipt location this payload's layout selects.
    pub(crate) const fn receipt(&self) -> &ReceiptInput {
        &self.receipt
    }

    /// Replay the complete current battery once in the target environment.
    pub(crate) fn replay(
        self,
        config: &Config,
        workspace: impl AsRef<Path>,
        limits: Limits,
    ) -> Result<Proved, ProofError> {
        let environment = SealedEnvironment::current(config, &self.payload, workspace, limits)
            .map_err(ProofError::Environment)?;
        self.bind(environment.target(), || replay(&environment))
    }

    /// Bind the environment target before `run` executes, then admit only a
    /// clean replay.
    pub(crate) fn bind<R>(self, environment: ReleaseTarget, run: R) -> Result<Proved, ProofError>
    where
        R: FnOnce() -> Result<ReplayVerdict, ReplayError>,
    {
        let payload = self.payload.value().target;
        if payload != environment {
            return Err(ProofError::EnvironmentTarget {
                payload,
                environment,
            });
        }
        let replay = run().map_err(ProofError::Replay)?;
        if replay.standing() != ProofStanding::Clean {
            return Err(ProofError::Refused(ReplayRefusal {
                replay: Box::new(replay),
            }));
        }
        Ok(Proved {
            staged: self,
            replay,
        })
    }
}

/// A clean, target-bound replay of one admitted payload.
pub(crate) struct Proved {
    staged: Staged,
    replay: ReplayVerdict,
}

impl Proved {
    pub(crate) const fn payload(&self) -> &AdmittedPayload {
        &self.staged.payload
    }

    pub(crate) const fn receipt(&self) -> &ReceiptInput {
        &self.staged.receipt
    }

    pub(crate) const fn replay(&self) -> &ReplayVerdict {
        &self.replay
    }

    /// Verify that `receipt` binds this exact payload and replay.
    pub(crate) fn verify(&self, receipt: &AdmittedReceipt) -> Result<VerifiedEvidence, ProofError> {
        verify_admitted(&self.staged.payload, receipt, &self.replay).map_err(ProofError::Evidence)
    }
}

/// A complete replay that ran but did not establish clean standing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayRefusal {
    replay: Box<ReplayVerdict>,
}

impl ReplayRefusal {
    /// The complete typed replay, including every member's standing.
    #[must_use]
    pub const fn replay(&self) -> &ReplayVerdict {
        &self.replay
    }
}

impl fmt::Display for ReplayRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("sealed proof replay did not establish clean standing")
    }
}

impl Error for ReplayRefusal {}

/// Why the shared replay core did not prove the selected payload.
#[derive(Debug)]
pub enum ProofError {
    ReadPayload(InputError),
    AdmitPayload(PayloadError),
    Layout(LayoutError),
    Environment(EnvironmentError),
    EnvironmentTarget {
        payload: ReleaseTarget,
        environment: ReleaseTarget,
    },
    Replay(ReplayError),
    Refused(ReplayRefusal),
    Evidence(EvidenceError),
}

impl fmt::Display for ProofError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadPayload(source) => source.fmt(formatter),
            Self::AdmitPayload(source) => source.fmt(formatter),
            Self::Layout(source) => source.fmt(formatter),
            Self::Environment(source) => source.fmt(formatter),
            Self::EnvironmentTarget {
                payload,
                environment,
            } => write!(
                formatter,
                "sealed environment target {environment} does not match payload target {payload}"
            ),
            Self::Replay(source) => source.fmt(formatter),
            Self::Refused(source) => source.fmt(formatter),
            Self::Evidence(source) => source.fmt(formatter),
        }
    }
}

impl Error for ProofError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ReadPayload(source) => Some(source),
            Self::AdmitPayload(source) => Some(source),
            Self::Layout(source) => Some(source),
            Self::Environment(source) => Some(source),
            Self::Replay(source) => Some(source),
            Self::Refused(source) => Some(source),
            Self::Evidence(source) => Some(source),
            Self::EnvironmentTarget { .. } => None,
        }
    }
}
