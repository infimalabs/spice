//! The shutdown ladder a seat descends, one rung at a time.
//!
//! Shutdown is ordered because each rung establishes what the next one may
//! assume: admission has stopped before work is drained, grace has elapsed
//! before occupants are terminated, and every claim is verified before
//! authority goes dark. A descent that skipped a rung would reach [`Dark`]
//! having proven none of it, so a skip is refused rather than smoothed over.
//!
//! The rungs are [`ResidentState`] itself, not a second spelling of it. The
//! resident lifecycle already names these seven states on the wire, and a
//! parallel enum here would be one more thing to keep in agreement.
//!
//! [`Dark`]: ResidentState::Dark

use std::error::Error;
use std::fmt;

use journal::crossing::ResidentState;

/// The ladder in descent order, shallowest rung first.
pub const LADDER: [ResidentState; 7] = [
    ResidentState::Seated,
    ResidentState::Quiescing,
    ResidentState::Draining,
    ResidentState::Terminating,
    ResidentState::Killing,
    ResidentState::Verifying,
    ResidentState::Dark,
];

/// How deep this rung sits, counting [`Seated`] as zero.
///
/// The match is exhaustive on purpose. An eighth resident state added to the
/// wire cannot reach a released build without failing to compile here, so the
/// ladder cannot silently acquire a rung it does not order.
///
/// [`Seated`]: ResidentState::Seated
#[must_use]
pub const fn depth(state: ResidentState) -> usize {
    match state {
        ResidentState::Seated => 0,
        ResidentState::Quiescing => 1,
        ResidentState::Draining => 2,
        ResidentState::Terminating => 3,
        ResidentState::Killing => 4,
        ResidentState::Verifying => 5,
        ResidentState::Dark => 6,
    }
}

/// Why a step did not descend the ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The step passed over at least one rung.
    Skipped {
        /// Rung the descent stood on.
        from: ResidentState,
        /// Rung the step named.
        to: ResidentState,
    },
    /// The step named a rung at or above the current one.
    Backward {
        /// Rung the descent stood on.
        from: ResidentState,
        /// Rung the step named.
        to: ResidentState,
    },
    /// The descent already reached the terminal rung.
    Beyond {
        /// Rung the step named.
        to: ResidentState,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Skipped { from, to } => write!(
                formatter,
                "{to:?} is {} rungs below {from:?}; the ladder descends one rung at a time",
                depth(*to) - depth(*from)
            ),
            Self::Backward { from, to } => {
                write!(formatter, "{to:?} does not descend from {from:?}")
            }
            Self::Beyond { to } => {
                write!(formatter, "{to:?} cannot follow Dark, which is terminal")
            }
        }
    }
}

impl Error for Refusal {}

/// One seat's progress down the ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descent {
    at: ResidentState,
}

impl Descent {
    /// A descent standing on the first rung.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            at: ResidentState::Seated,
        }
    }

    /// Resume from the last durable rung after an interrupted shutdown.
    #[must_use]
    pub const fn resume(at: ResidentState) -> Self {
        Self { at }
    }

    /// The rung the descent stands on.
    #[must_use]
    pub const fn at(self) -> ResidentState {
        self.at
    }

    /// Whether the descent reached the terminal rung.
    #[must_use]
    pub const fn dark(self) -> bool {
        matches!(self.at, ResidentState::Dark)
    }

    /// The only rung a step may name next, absent at the terminal rung.
    #[must_use]
    pub fn next(self) -> Option<ResidentState> {
        LADDER.get(depth(self.at) + 1).copied()
    }

    /// Descends exactly one rung.
    ///
    /// # Errors
    ///
    /// Returns [`Refusal`] naming both rungs when the step skips a rung, names
    /// a rung at or above the current one, or follows the terminal rung.
    pub fn step(&mut self, to: ResidentState) -> Result<(), Refusal> {
        let Some(next) = self.next() else {
            return Err(Refusal::Beyond { to });
        };
        if to == next {
            self.at = to;
            return Ok(());
        }
        if depth(to) <= depth(self.at) {
            return Err(Refusal::Backward { from: self.at, to });
        }
        Err(Refusal::Skipped { from: self.at, to })
    }
}

impl Default for Descent {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{Descent, LADDER, Refusal, depth};
    use journal::crossing::ResidentState;

    /// The ladder and the depth it is ordered by are the same ordering.
    ///
    /// `depth` is an exhaustive match and `LADDER` is a written sequence; this
    /// is what keeps them from disagreeing, since either alone would look
    /// correct while contradicting the other.
    #[test]
    fn every_rung_sits_at_the_depth_the_ladder_places_it() {
        for (index, rung) in LADDER.into_iter().enumerate() {
            assert_eq!(depth(rung), index, "{rung:?} is ordered inconsistently");
        }
        assert_eq!(LADDER.len(), 7);
    }

    /// The full descent is walkable one rung at a time.
    #[test]
    fn the_descent_reaches_dark_one_rung_at_a_time() {
        let mut descent = Descent::new();
        assert_eq!(descent.at(), ResidentState::Seated);
        for rung in LADDER.into_iter().skip(1) {
            descent.step(rung).expect("the next rung descends");
            assert_eq!(descent.at(), rung);
        }
        assert!(descent.dark());
        assert_eq!(descent.next(), None);
    }

    /// A skipped rung is refused, and the descent does not move.
    #[test]
    fn a_skipped_rung_is_refused_and_names_both_rungs() {
        let mut descent = Descent::new();

        let refusal = descent
            .step(ResidentState::Dark)
            .expect_err("Dark is six rungs below Seated");

        assert_eq!(
            refusal,
            Refusal::Skipped {
                from: ResidentState::Seated,
                to: ResidentState::Dark,
            }
        );
        assert_eq!(
            refusal.to_string(),
            "Dark is 6 rungs below Seated; the ladder descends one rung at a time"
        );
        assert_eq!(descent.at(), ResidentState::Seated);
    }

    /// Every rung refuses every rung but its immediate successor.
    #[test]
    fn no_rung_admits_any_step_but_the_next() {
        for standing in LADDER {
            for named in LADDER {
                let mut descent = Descent::new();
                while descent.at() != standing {
                    let next = descent.next().expect("a rung short of the one sought");
                    descent.step(next).expect("walking to the rung under test");
                }
                let admitted = descent.next() == Some(named);
                assert_eq!(
                    descent.step(named).is_ok(),
                    admitted,
                    "{standing:?} judged the step to {named:?} wrongly",
                );
            }
        }
    }

    /// Climbing back up is refused, including restating the current rung.
    #[test]
    fn the_ladder_does_not_climb() {
        let mut descent = Descent::new();
        descent
            .step(ResidentState::Quiescing)
            .expect("the next rung descends");

        assert_eq!(
            descent.step(ResidentState::Seated),
            Err(Refusal::Backward {
                from: ResidentState::Quiescing,
                to: ResidentState::Seated,
            })
        );
        assert_eq!(
            descent.step(ResidentState::Quiescing),
            Err(Refusal::Backward {
                from: ResidentState::Quiescing,
                to: ResidentState::Quiescing,
            })
        );
        assert_eq!(descent.at(), ResidentState::Quiescing);
    }

    /// Dark is terminal, so every step beyond it is refused by name.
    #[test]
    fn nothing_follows_dark() {
        let mut descent = Descent::new();
        for rung in LADDER.into_iter().skip(1) {
            descent.step(rung).expect("the next rung descends");
        }

        for named in LADDER {
            assert_eq!(
                descent.step(named),
                Err(Refusal::Beyond { to: named }),
                "Dark admitted a step to {named:?}",
            );
        }
    }
}
