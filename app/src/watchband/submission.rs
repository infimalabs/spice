//! Where one composer's draft may go, derived once from the connection and the
//! command surface the admitted runtime offers.
//!
//! The surface keeps the last admitted runtime on screen while its connection
//! is down and while a restarted resident hydrates, so the runtime's offers
//! alone describe a resident that may no longer exist.
//! Every submission is witnessed by the residency epoch the runtime remembers
//! and, for an occupied seat, its occupant. The resident refuses a witness it
//! never published, so a draft admitted while offline is a draft queued to be
//! refused. Only an online connection turns an offer into authority, and every
//! path that submits a draft asks here.

use wire::{CanonicalUuid, CommandKind};

/// Authority a detached draft may use for its next transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmissionTarget {
    /// Steer the seated occupant under the generation that seated it.
    Resident {
        occupant: CanonicalUuid,
        epoch: CanonicalUuid,
    },
    /// Publish to the vacant seat under the current residency generation.
    Vacant { epoch: CanonicalUuid },
}

/// What the session may currently do with a composer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubmissionAuthority {
    /// An occupied or vacant reachable seat may be steered.
    pub send: bool,
}

impl SubmissionAuthority {
    /// Nothing may be submitted.
    pub const NONE: Self = Self { send: false };

    /// Derive from the connection state and the runtime's offers.
    ///
    /// `online` is the surface's connection being `Online`: the socket is
    /// admitted and the runtime it published is the one the resident is
    /// serving now. Both occupied and vacant seats use the same durable
    /// steering command.
    #[must_use]
    pub fn derive(online: bool, offers: impl Fn(CommandKind) -> bool) -> Self {
        Self {
            send: online && offers(CommandKind::SteerSend),
        }
    }

    /// Where one member's draft goes, or nowhere.
    ///
    /// A seated member is steered when sending is authorised; a vacant,
    /// wakeable member receives durable steering under the current residency
    /// generation. A member the authority cannot reach has no target, and a
    /// draft with no target stays in its composer.
    #[must_use]
    pub fn target(
        self,
        occupant: Option<CanonicalUuid>,
        wakeable: bool,
        epoch: CanonicalUuid,
    ) -> Option<SubmissionTarget> {
        match occupant {
            Some(occupant) => self
                .send
                .then_some(SubmissionTarget::Resident { occupant, epoch }),
            None => (self.send && wakeable).then_some(SubmissionTarget::Vacant { epoch }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPERATOR: fn(CommandKind) -> bool = |_| true;

    fn uuid(text: &str) -> CanonicalUuid {
        text.parse().expect("fixture identity is canonical")
    }

    fn occupant() -> CanonicalUuid {
        uuid("33333333-3333-4333-8333-333333333333")
    }

    fn epoch() -> CanonicalUuid {
        uuid("44444444-4444-4444-8444-444444444444")
    }

    #[test]
    fn an_offline_connection_offers_no_authority_whatever_the_runtime_offers() {
        assert_eq!(
            SubmissionAuthority::derive(false, OPERATOR),
            SubmissionAuthority::NONE,
        );
        let offline = SubmissionAuthority::derive(false, OPERATOR);
        assert_eq!(offline.target(Some(occupant()), true, epoch()), None);
        assert_eq!(offline.target(None, true, epoch()), None);
    }

    #[test]
    fn an_online_connection_turns_the_offers_into_authority() {
        let online = SubmissionAuthority::derive(true, OPERATOR);
        assert_eq!(online, SubmissionAuthority { send: true });
        assert_eq!(
            online.target(Some(occupant()), false, epoch()),
            Some(SubmissionTarget::Resident {
                occupant: occupant(),
                epoch: epoch(),
            }),
        );
        assert_eq!(
            online.target(None, true, epoch()),
            Some(SubmissionTarget::Vacant { epoch: epoch() })
        );
        assert_eq!(
            online.target(None, false, epoch()),
            None,
            "a started seat without a session cannot receive steering"
        );
    }

    #[test]
    fn a_vacant_send_needs_only_the_steering_offer() {
        let no_send = SubmissionAuthority::derive(true, |kind| kind != CommandKind::SteerSend);
        assert_eq!(no_send, SubmissionAuthority::NONE);
        for omitted in [CommandKind::LaneState, CommandKind::LaneRead] {
            let authority = SubmissionAuthority::derive(true, |kind| kind != omitted);
            assert_eq!(
                authority.target(None, true, epoch()),
                Some(SubmissionTarget::Vacant { epoch: epoch() })
            );
        }
    }
}
