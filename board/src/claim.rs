use std::fmt;

use wire::{Actor, CanonicalUuid, ClaimDisposition, Instant, Seq, TaskHandle};

use crate::task::Entry;

/// Proof that one actor currently owns a coherent, unexpired claim lease.
///
/// The fields and constructor are private. Callers can only obtain this value
/// by passing the current claim and lease heads through [`Active::admit`].
///
/// ```compile_fail
/// let _forged: board::Active<'static> = board::Active {
///     handle: todo!(),
///     actor: todo!(),
///     claimed: todo!(),
///     lane: todo!(),
///     occupant: todo!(),
///     epoch: todo!(),
///     expiry: todo!(),
/// };
/// ```
#[derive(Debug)]
pub struct Active<'a> {
    handle: &'a TaskHandle,
    actor: &'a Actor,
    claimed: Seq,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    epoch: CanonicalUuid,
    expiry: Instant,
}

impl<'a> Active<'a> {
    /// Admit an active proof for an authenticated `actor` at the supplied instant.
    ///
    /// The actor must come from the resident's accepted capability, never from
    /// request text; this method then binds that principal to its own active
    /// claim among the task's claims and to that claim's lease.
    pub fn admit(entry: &'a Entry, actor: &'a Actor, now: Instant) -> Result<Self, ClaimRefusal> {
        let Some(held) = entry.claim_of(actor) else {
            let latest = entry.latest_claim().ok_or(ClaimRefusal::MissingClaim)?;
            return Err(match entry.active_claims().next() {
                Some(other) => ClaimRefusal::WrongActor {
                    expected: other.claim().actor.clone(),
                    observed: actor.clone(),
                },
                None => ClaimRefusal::Inactive(latest.claim().disposition),
            });
        };
        let claim = held.claim();
        let lease = held.lease().ok_or(ClaimRefusal::MissingLease)?;
        if claim.epoch != lease.epoch {
            return Err(ClaimRefusal::Epoch {
                claim: claim.epoch,
                lease: lease.epoch,
            });
        }
        if !(lease.start <= lease.renewal
            && lease.renewal <= lease.expiry
            && lease.expiry <= lease.maximum)
        {
            return Err(ClaimRefusal::IncoherentLease);
        }
        if now < lease.start {
            return Err(ClaimRefusal::NotStarted { start: lease.start });
        }
        if now >= lease.expiry {
            return Err(ClaimRefusal::Expired {
                expiry: lease.expiry,
            });
        }

        Ok(Self {
            handle: entry.handle(),
            actor,
            claimed: claim.claimed,
            lane: claim.lane,
            occupant: claim.occupant,
            epoch: claim.epoch,
            expiry: lease.expiry,
        })
    }

    /// Claimed task.
    #[must_use]
    pub const fn handle(&self) -> &TaskHandle {
        self.handle
    }

    /// Winning principal.
    #[must_use]
    pub const fn actor(&self) -> &Actor {
        self.actor
    }

    /// Identity of the admitted claim.
    #[must_use]
    pub const fn claimed(&self) -> Seq {
        self.claimed
    }

    /// Lane bound to the proof.
    #[must_use]
    pub const fn lane(&self) -> CanonicalUuid {
        self.lane
    }

    /// Occupant bound to the proof.
    #[must_use]
    pub const fn occupant(&self) -> CanonicalUuid {
        self.occupant
    }

    /// Resident generation bound to the proof.
    #[must_use]
    pub const fn epoch(&self) -> CanonicalUuid {
        self.epoch
    }

    /// Instant beyond which this proof is not valid.
    #[must_use]
    pub const fn expiry(&self) -> Instant {
        self.expiry
    }
}

/// Why current heads do not prove claim possession.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClaimRefusal {
    MissingClaim,
    MissingLease,
    Inactive(ClaimDisposition),
    WrongActor {
        expected: Actor,
        observed: Actor,
    },
    Epoch {
        claim: CanonicalUuid,
        lease: CanonicalUuid,
    },
    IncoherentLease,
    NotStarted {
        start: Instant,
    },
    Expired {
        expiry: Instant,
    },
}

impl fmt::Display for ClaimRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingClaim => formatter.write_str("task has no claim head"),
            Self::MissingLease => formatter.write_str("active claim has no lease head"),
            Self::Inactive(disposition) => {
                write!(formatter, "claim is not active: {disposition:?}")
            }
            Self::WrongActor { expected, observed } => write!(
                formatter,
                "claim belongs to {expected}, not observed actor {observed}"
            ),
            Self::Epoch { claim, lease } => write!(
                formatter,
                "claim epoch {claim} does not match lease epoch {lease}"
            ),
            Self::IncoherentLease => {
                formatter.write_str("lease must satisfy start <= renewal <= expiry <= maximum")
            }
            Self::NotStarted { start } => write!(formatter, "lease starts at {start}"),
            Self::Expired { expiry } => write!(formatter, "lease expired at {expiry}"),
        }
    }
}

impl std::error::Error for ClaimRefusal {}
