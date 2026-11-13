//! The state of one on-demand resident read a surface shows.

use std::ops::Deref;

use wire::Outcome;

/// Capability-scoped state of one on-demand read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadState<A> {
    /// Shown, and not yet answered: its read is owed or in flight.
    Loading,
    /// The current admitted answer.
    Available(A),
    /// The last admitted answer, retained while its replacement is read.
    Refreshing(A),
    /// The resident refused or could not serve the read.
    Unavailable {
        /// Stable resident reason.
        reason: String,
        /// Human diagnostic detail, when the resident supplied it.
        detail: Option<String>,
    },
}

impl<A: Deref> ReadState<A> {
    /// Last admitted answer, retained during an in-flight refresh.
    #[must_use]
    pub fn answer(&self) -> Option<&A::Target> {
        match self {
            Self::Available(answer) | Self::Refreshing(answer) => Some(answer),
            Self::Loading | Self::Unavailable { .. } => None,
        }
    }
}

impl<A> ReadState<A> {
    /// Settle one admitted outcome. `admit` keeps a reply that answers this
    /// read; one that does not reads as `mismatch`, a pair of reason and
    /// detail, and a refusal without a diagnostic reads as `refused`.
    pub(super) fn settle<B>(
        outcome: &Outcome<B>,
        admit: impl FnOnce(&B) -> Option<A>,
        mismatch: (&str, &str),
        refused: &str,
    ) -> Self {
        match outcome {
            Outcome::Reply(reply) => admit(&reply.body).map_or_else(
                || unavailable(mismatch.0, Some(mismatch.1)),
                Self::Available,
            ),
            Outcome::Refusal(refusal) => {
                let diagnostic = refusal.diagnostic();
                unavailable(
                    diagnostic.map_or(refused, wire::RefusalDiagnostic::reason),
                    diagnostic.and_then(wire::RefusalDiagnostic::detail),
                )
            }
            Outcome::Fault(fault) => unavailable(&fault.reason, Some(&fault.detail)),
        }
    }

    /// Keep a stale answer on screen while its replacement is read.
    pub(super) fn refresh(&mut self) {
        *self = match std::mem::replace(self, Self::Loading) {
            Self::Available(answer) | Self::Refreshing(answer) => Self::Refreshing(answer),
            Self::Loading | Self::Unavailable { .. } => Self::Loading,
        };
    }
}

fn unavailable<A>(reason: &str, detail: Option<&str>) -> ReadState<A> {
    ReadState::Unavailable {
        reason: reason.to_owned(),
        detail: detail.map(str::to_owned),
    }
}
