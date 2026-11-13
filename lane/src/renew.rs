//! Claim renewal, bound to occupant liveness and to the resident epoch.
//!
//! A claim is held on behalf of a living occupant under one resident
//! generation. Renewal therefore proves two things before it extends anything:
//! that the occupant is still alive, and that the epoch it renews under is the
//! one the claim was taken in. A dead occupant renewing would hold work nobody
//! is doing, and a stale epoch renewing would let a superseded resident keep a
//! claim its successor already believes it owns.
//!
//! Liveness arrives as an answer, not as a probe. The caller has already
//! observed the process group; this module only decides what that observation
//! permits, which is what keeps renewal replayable.

use std::error::Error;
use std::fmt;

use journal::crossing::{CanonicalUuid, Instant};

/// What the caller observed of the occupant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Liveness {
    /// The occupant is still running.
    Live,
    /// The occupant has ended.
    Dead,
}

/// Why a claim did not renew.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The occupant the claim is held for has ended.
    Dead {
        /// Occupant the claim is held for.
        occupant: CanonicalUuid,
    },
    /// The renewal names a generation other than the claim's.
    Stale {
        /// Epoch the claim was taken under.
        epoch: CanonicalUuid,
        /// Epoch the renewal named.
        named: CanonicalUuid,
    },
    /// The renewal names an occupant other than the one holding the claim.
    Displaced {
        /// Occupant holding the claim.
        holding: CanonicalUuid,
        /// Occupant the renewal named.
        named: CanonicalUuid,
    },
    /// The renewal would not move the deadline forward.
    Regressive {
        /// Deadline the claim already carries.
        until: Instant,
        /// Deadline the renewal named.
        named: Instant,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dead { occupant } => {
                write!(formatter, "occupant {occupant} has ended and cannot renew")
            }
            Self::Stale { epoch, named } => write!(
                formatter,
                "the claim was taken under epoch {epoch}, not {named}"
            ),
            Self::Displaced { holding, named } => {
                write!(formatter, "the claim is held by {holding}, not {named}")
            }
            Self::Regressive { until, named } => write!(
                formatter,
                "renewal to {named} does not extend the claim beyond {until}"
            ),
        }
    }
}

impl Error for Refusal {}

/// One claim, held for an occupant under a resident generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Claim {
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    epoch: CanonicalUuid,
    until: Instant,
}

impl Claim {
    /// A claim taken for this occupant, under this generation, until then.
    #[must_use]
    pub const fn new(
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
        epoch: CanonicalUuid,
        until: Instant,
    ) -> Self {
        Self {
            lane,
            occupant,
            epoch,
            until,
        }
    }

    /// The lane the claim is bound to.
    #[must_use]
    pub const fn lane(self) -> CanonicalUuid {
        self.lane
    }

    /// The occupant the claim is held for.
    #[must_use]
    pub const fn occupant(self) -> CanonicalUuid {
        self.occupant
    }

    /// The resident generation the claim was taken under.
    #[must_use]
    pub const fn epoch(self) -> CanonicalUuid {
        self.epoch
    }

    /// The deadline the claim currently runs to.
    #[must_use]
    pub const fn until(self) -> Instant {
        self.until
    }

    /// Extends the claim, if liveness and generation both permit it.
    ///
    /// # Errors
    ///
    /// Returns [`Refusal`] when the occupant has ended, when the renewal names
    /// another occupant or another generation, or when the named deadline would
    /// not move the claim forward.
    pub fn renew(
        &mut self,
        occupant: CanonicalUuid,
        epoch: CanonicalUuid,
        liveness: Liveness,
        until: Instant,
    ) -> Result<(), Refusal> {
        if occupant != self.occupant {
            return Err(Refusal::Displaced {
                holding: self.occupant,
                named: occupant,
            });
        }
        if epoch != self.epoch {
            return Err(Refusal::Stale {
                epoch: self.epoch,
                named: epoch,
            });
        }
        if liveness == Liveness::Dead {
            return Err(Refusal::Dead {
                occupant: self.occupant,
            });
        }
        if until <= self.until {
            return Err(Refusal::Regressive {
                until: self.until,
                named: until,
            });
        }
        self.until = until;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Claim, Liveness, Refusal};
    use journal::crossing::{CanonicalUuid, Instant};

    fn uuid(last: u8) -> CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131163{last:02}")
            .parse()
            .expect("fixture uuid")
    }

    fn instant(minute: u8) -> Instant {
        format!("2026-08-01T07:{minute:02}:00.000Z")
            .parse()
            .expect("fixture instant")
    }

    fn claim() -> Claim {
        Claim::new(uuid(1), uuid(3), uuid(0), instant(30))
    }

    /// A living occupant renewing under its own generation extends the claim.
    #[test]
    fn a_live_occupant_renews_under_its_own_epoch() {
        let mut claim = claim();

        claim
            .renew(uuid(3), uuid(0), Liveness::Live, instant(45))
            .expect("a live occupant renews");

        assert_eq!(claim.until(), instant(45));
        assert_eq!(claim.occupant(), uuid(3));
        assert_eq!(claim.epoch(), uuid(0));
        assert_eq!(claim.lane(), uuid(1));
    }

    /// A dead occupant cannot renew, and the deadline does not move.
    #[test]
    fn a_dead_occupant_cannot_renew() {
        let mut claim = claim();

        let refusal = claim
            .renew(uuid(3), uuid(0), Liveness::Dead, instant(45))
            .expect_err("a dead occupant holds nothing");

        assert_eq!(refusal, Refusal::Dead { occupant: uuid(3) });
        assert_eq!(
            refusal.to_string(),
            format!("occupant {} has ended and cannot renew", uuid(3))
        );
        assert_eq!(claim.until(), instant(30));
    }

    /// A stale epoch is refused even when the occupant is alive.
    #[test]
    fn a_stale_epoch_cannot_renew() {
        let mut claim = claim();

        let refusal = claim
            .renew(uuid(3), uuid(9), Liveness::Live, instant(45))
            .expect_err("a superseded generation holds nothing");

        assert_eq!(
            refusal,
            Refusal::Stale {
                epoch: uuid(0),
                named: uuid(9),
            }
        );
        assert_eq!(claim.until(), instant(30));
    }

    /// Another occupant cannot renew a claim it does not hold.
    #[test]
    fn another_occupant_cannot_renew_the_claim() {
        let mut claim = claim();

        assert_eq!(
            claim.renew(uuid(4), uuid(0), Liveness::Live, instant(45)),
            Err(Refusal::Displaced {
                holding: uuid(3),
                named: uuid(4),
            })
        );
        assert_eq!(claim.until(), instant(30));
    }

    /// A renewal that does not move the deadline forward is refused.
    #[test]
    fn a_renewal_must_extend_the_claim() {
        for named in [instant(30), instant(15)] {
            let mut claim = claim();

            assert_eq!(
                claim.renew(uuid(3), uuid(0), Liveness::Live, named),
                Err(Refusal::Regressive {
                    until: instant(30),
                    named,
                })
            );
            assert_eq!(claim.until(), instant(30));
        }
    }

    /// Liveness is checked after identity, so a dead stranger is reported as
    /// the stranger it is rather than as a dead occupant.
    #[test]
    fn a_dead_stranger_is_refused_as_a_stranger() {
        let mut claim = claim();

        assert_eq!(
            claim.renew(uuid(4), uuid(0), Liveness::Dead, instant(45)),
            Err(Refusal::Displaced {
                holding: uuid(3),
                named: uuid(4),
            })
        );
    }
}
