//! The life policy a seat is supervised under.
//!
//! A seat is always under exactly one of three policies. Under [`Steer`] the
//! occupant is directed and waits to be told; under [`Drive`] it works the
//! queue on its own; under [`Drain`] it is finishing and will not be re-seated.
//! The set is closed because a fourth policy would be a fourth answer to
//! "should this seat be refilled", and that question admits only these three.
//!
//! Transitions are guarded rather than free. Steering and driving exchange
//! freely, both may begin draining, and draining is where a policy ends: a
//! seat that has begun finishing does not return to work, because the work it
//! would return to has already been handed on.
//!
//! [`Steer`]: Life::Steer
//! [`Drive`]: Life::Drive
//! [`Drain`]: Life::Drain

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

/// The closed set of policies a seat is supervised under.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Life {
    /// The occupant is directed and waits to be told.
    Steer,
    /// The occupant works the queue on its own.
    Drive,
    /// The occupant is finishing and will not be re-seated.
    Drain,
}

impl Life {
    /// Every policy, in the order they are named.
    pub const ALL: [Self; 3] = [Self::Steer, Self::Drive, Self::Drain];

    /// The word this policy is reported under.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Steer => "steer",
            Self::Drive => "drive",
            Self::Drain => "drain",
        }
    }

    /// Whether this policy admits moving to that one.
    ///
    /// The match is exhaustive over the pair rather than falling through a
    /// wildcard, so a fourth policy cannot be added without every transition
    /// it participates in being decided here.
    #[must_use]
    pub const fn admits(self, to: Self) -> bool {
        match (self, to) {
            (Self::Drain, _) => false,
            (Self::Steer, Self::Drive) | (Self::Drive, Self::Steer) => true,
            (Self::Steer, Self::Drain) | (Self::Drive, Self::Drain) => true,
            (Self::Steer, Self::Steer) | (Self::Drive, Self::Drive) => false,
        }
    }
}

impl fmt::Display for Life {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why a seat did not adopt a policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The seat is draining, which is where a policy ends.
    Draining {
        /// Policy the seat was asked to adopt.
        to: Life,
    },
    /// The seat already sits under this policy, so nothing would move.
    Unmoved {
        /// Policy the seat already sits under.
        at: Life,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Draining { to } => {
                write!(formatter, "a draining seat cannot adopt {to}")
            }
            Self::Unmoved { at } => {
                write!(formatter, "the seat already sits under {at}")
            }
        }
    }
}

impl Error for Refusal {}

/// One seat's current policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    at: Life,
}

impl Policy {
    /// A seat beginning under this policy.
    #[must_use]
    pub const fn new(at: Life) -> Self {
        Self { at }
    }

    /// The policy the seat sits under.
    #[must_use]
    pub const fn at(self) -> Life {
        self.at
    }

    /// Adopts a policy, if this one admits it.
    ///
    /// # Errors
    ///
    /// Returns [`Refusal`] when the seat is draining or when the named policy
    /// is the one it already sits under.
    pub fn adopt(&mut self, to: Life) -> Result<(), Refusal> {
        if self.at.admits(to) {
            self.at = to;
            return Ok(());
        }
        if self.at == Life::Drain {
            return Err(Refusal::Draining { to });
        }
        Err(Refusal::Unmoved { at: self.at })
    }
}

#[cfg(test)]
mod tests {
    use super::{Life, Policy, Refusal};

    /// Every policy is reported under one word, and the words are distinct.
    #[test]
    fn every_policy_is_named_under_one_word() {
        let words: Vec<&str> = Life::ALL.iter().map(|life| life.as_str()).collect();
        assert_eq!(words, vec!["steer", "drive", "drain"]);
        for life in Life::ALL {
            assert_eq!(life.to_string(), life.as_str());
        }
        assert_eq!(Life::ALL.len(), 3);
    }

    /// Steering and driving exchange freely, and either may begin draining.
    #[test]
    fn working_policies_exchange_and_both_may_begin_draining() {
        for (from, to) in [
            (Life::Steer, Life::Drive),
            (Life::Drive, Life::Steer),
            (Life::Steer, Life::Drain),
            (Life::Drive, Life::Drain),
        ] {
            let mut policy = Policy::new(from);
            policy.adopt(to).expect("admitted transition");
            assert_eq!(policy.at(), to);
        }
    }

    /// Draining is terminal, so every policy is refused from it by name.
    #[test]
    fn a_draining_seat_adopts_nothing() {
        for to in Life::ALL {
            let mut policy = Policy::new(Life::Drain);

            let refusal = policy.adopt(to).expect_err("draining is terminal");

            assert_eq!(refusal, Refusal::Draining { to });
            assert_eq!(
                refusal.to_string(),
                format!("a draining seat cannot adopt {to}")
            );
            assert_eq!(policy.at(), Life::Drain);
        }
    }

    /// Restating the current policy is refused, so a transition always moves.
    #[test]
    fn restating_the_current_policy_is_refused() {
        for at in [Life::Steer, Life::Drive] {
            let mut policy = Policy::new(at);

            assert_eq!(policy.adopt(at), Err(Refusal::Unmoved { at }));
            assert_eq!(policy.at(), at);
        }
    }

    /// `admits` and `adopt` decide the same transitions, so neither can drift
    /// from the other while looking correct on its own.
    #[test]
    fn the_admitted_transitions_are_the_adopted_ones() {
        for from in Life::ALL {
            for to in Life::ALL {
                let mut policy = Policy::new(from);
                assert_eq!(
                    policy.adopt(to).is_ok(),
                    from.admits(to),
                    "{from} judged the move to {to} inconsistently",
                );
            }
        }
    }
}
