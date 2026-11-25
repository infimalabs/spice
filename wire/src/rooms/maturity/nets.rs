//! Current-statement term nets: each term's retained latest charge signs, the
//! regime they set for ending a pass, and the advisory intensity beside it.

use std::fmt::Write as _;

use super::{DESIGN_MATURITY_LANE_LIMIT, DesignItems};
use crate::rooms::StatementItem;

/// One current term's retained lanes by the sign of their latest nonzero
/// charge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TermNet {
    /// The term's text identity.
    pub item: StatementItem,
    /// Retained lanes whose latest nonzero charge is positive.
    pub positive: u64,
    /// Retained lanes whose latest nonzero charge is negative.
    pub negative: u64,
    /// More lanes charged than the series retains, so the counts are inexact.
    pub truncated: bool,
}

impl TermNet {
    /// Positive lanes less negative lanes.
    #[must_use]
    pub fn net(&self) -> i64 {
        i64::try_from(self.positive).unwrap_or(i64::MAX)
            - i64::try_from(self.negative).unwrap_or(i64::MAX)
    }
}

/// How the current term nets let a lane end a Design pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesignRegime {
    /// Every term nets positive: work should exist.
    Positive,
    /// Every term nets negative: no work should exist.
    Negative,
    /// The statement is empty, a term nets zero, or a term's counts are
    /// inexact: only Plans or Designs may be offered.
    Contested,
    /// Nonzero nets of both signs: output kind is left to judgment.
    Split,
}

impl DesignRegime {
    /// Stable lowercase name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Positive => "positive",
            Self::Negative => "negative",
            Self::Contested => "contested",
            Self::Split => "split",
        }
    }
}

/// Each current term's net, in statement order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DesignNets {
    /// One net per current term.
    pub terms: Vec<TermNet>,
}

impl DesignNets {
    /// The one regime the nets set. A positive or negative regime needs a
    /// nonempty statement, every net of that sign, and exact counts.
    #[must_use]
    pub fn regime(&self) -> DesignRegime {
        if self.terms.is_empty() || self.terms.iter().any(Self::undecided) {
            return DesignRegime::Contested;
        }
        let positive = self.terms.iter().any(|term| term.net() > 0);
        let negative = self.terms.iter().any(|term| term.net() < 0);
        match (positive, negative) {
            (true, false) => DesignRegime::Positive,
            (false, true) => DesignRegime::Negative,
            _ => DesignRegime::Split,
        }
    }

    fn undecided(term: &TermNet) -> bool {
        term.truncated || term.net() == 0
    }

    /// The sum of term nets.
    #[must_use]
    pub fn total(&self) -> i64 {
        self.terms.iter().map(TermNet::net).sum()
    }

    /// Agreement as the exact fraction a/b, with a = Σ|P − N| and
    /// b = Σ(P + N); none when no lane sign is retained.
    #[must_use]
    pub fn agreement(&self) -> Option<(u64, u64)> {
        let signed = self.signed();
        (signed != 0).then(|| (self.aligned(), signed))
    }

    /// Lane signs that opposing signs on the same term cancel: b − a.
    #[must_use]
    pub fn cancellation(&self) -> u64 {
        self.signed() - self.aligned()
    }

    /// Terms whose net is exactly zero.
    #[must_use]
    pub fn zero_nets(&self) -> usize {
        self.terms.iter().filter(|term| term.net() == 0).count()
    }

    /// Some term's counts, and so every aggregate, are inexact.
    #[must_use]
    pub fn inexact(&self) -> bool {
        self.terms.iter().any(|term| term.truncated)
    }

    fn aligned(&self) -> u64 {
        self.terms
            .iter()
            .map(|term| term.net().unsigned_abs())
            .sum()
    }

    fn signed(&self) -> u64 {
        self.terms
            .iter()
            .map(|term| term.positive + term.negative)
            .sum()
    }

    /// The nets that set the regime, one `HANDLE P+/N- net` clause each: a
    /// contested regime names its zero and inexact terms, any other names
    /// every term.
    #[must_use]
    pub fn deciding(&self) -> String {
        let regime = self.regime();
        if self.terms.is_empty() {
            return "the statement names no term".to_owned();
        }
        let current = self
            .terms
            .iter()
            .map(|term| term.item.clone())
            .collect::<Vec<_>>();
        let mut rendered = String::new();
        for term in &self.terms {
            if regime == DesignRegime::Contested && !Self::undecided(term) {
                continue;
            }
            if !rendered.is_empty() {
                rendered.push_str("; ");
            }
            write!(
                rendered,
                "{} {}+/{}- net {:+}",
                term.item.handle(&current),
                term.positive,
                term.negative,
                term.net()
            )
            .expect("writing to a String succeeds");
            if term.truncated {
                write!(
                    rendered,
                    " (more than {DESIGN_MATURITY_LANE_LIMIT} lanes charged this term; exact \
                     counts unavailable)"
                )
                .expect("writing to a String succeeds");
            }
        }
        rendered
    }
}

impl DesignItems {
    /// Each current item of `term` as its net over retained latest nonzero
    /// signs, in statement order. An uncharged item nets zero; an item no
    /// longer in the statement never weighs.
    #[must_use]
    pub fn nets(&self, term: &[String]) -> DesignNets {
        DesignNets {
            terms: StatementItem::each(term)
                .map(|item| {
                    let series = self.series(&item);
                    let lanes = series.map_or(&[][..], |series| &series.lanes);
                    let positive = lanes.iter().filter(|lane| lane.positive).count();
                    TermNet {
                        positive: positive as u64,
                        negative: (lanes.len() - positive) as u64,
                        truncated: series.is_some_and(|series| series.lanes_truncated),
                        item,
                    }
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{ITEMS, charge, term};
    use super::*;

    #[test]
    fn the_regime_follows_every_current_net_and_exact_counts() {
        let current = term(&ITEMS);
        let mut items = DesignItems::default();
        assert_eq!(items.nets(&[]).regime(), DesignRegime::Contested);
        assert_eq!(
            items.nets(&current).regime(),
            DesignRegime::Contested,
            "an uncharged term nets zero"
        );

        items.observe(&charge(1, ITEMS[0], 1));
        items.observe(&charge(1, ITEMS[1], 2));
        assert_eq!(items.nets(&current).regime(), DesignRegime::Positive);

        // A lane's latest nonzero sign replaces its earlier one; a zero
        // charge never does.
        items.observe(&charge(1, ITEMS[1], -1));
        items.observe(&charge(1, ITEMS[1], 0));
        assert_eq!(items.nets(&current).regime(), DesignRegime::Split);
        items.observe(&charge(1, ITEMS[0], -2));
        assert_eq!(items.nets(&current).regime(), DesignRegime::Negative);
        items.observe(&charge(2, ITEMS[0], 1));
        assert_eq!(items.nets(&current).regime(), DesignRegime::Contested);
        assert_eq!(items.nets(&current).deciding().matches("net").count(), 1);

        // Only current items weigh: dropping the tied term leaves the
        // negative one.
        let reworded = term(&[ITEMS[1]]);
        assert_eq!(items.nets(&reworded).regime(), DesignRegime::Negative);
    }

    #[test]
    fn a_truncated_term_contests_and_marks_every_aggregate_inexact() {
        let current = term(&ITEMS[..1]);
        let mut items = DesignItems::default();
        for lane in 0..=64 {
            items.observe(&charge(lane, ITEMS[0], 1));
        }
        let nets = items.nets(&current);
        assert_eq!(nets.terms[0].positive, 64);
        assert!(nets.inexact());
        assert_eq!(
            nets.regime(),
            DesignRegime::Contested,
            "inexact counts never decide a hard regime"
        );
        assert!(nets.deciding().contains("exact counts unavailable"));
    }

    #[test]
    fn intensity_is_exact_and_never_moves_the_regime() {
        let lanes = |nets: &mut DesignItems, item: &str, first: u8, positive: u8, negative: u8| {
            for lane in first..first + positive {
                nets.observe(&charge(lane, item, 1));
            }
            for lane in first + positive..first + positive + negative {
                nets.observe(&charge(lane, item, -1));
            }
        };
        // Four 2+/2- terms beside four unanimous 4+ terms.
        let divided = (0..8).map(|n| format!("Term {n}")).collect::<Vec<_>>();
        let mut items = DesignItems::default();
        for (n, item) in divided.iter().enumerate() {
            let (positive, negative) = if n < 4 { (2, 2) } else { (4, 0) };
            lanes(&mut items, item, 0, positive, negative);
        }
        let nets = items.nets(&divided);
        assert_eq!(nets.agreement(), Some((16, 32)));
        assert_eq!(nets.cancellation(), 16);
        assert_eq!(nets.zero_nets(), 4);
        assert_eq!(nets.total(), 16);
        assert_eq!(nets.regime(), DesignRegime::Contested);

        // One 2+/3- term beside seven unanimous 5+ terms.
        let mut items = DesignItems::default();
        for (n, item) in divided.iter().enumerate() {
            let (positive, negative) = if n == 0 { (2, 3) } else { (5, 0) };
            lanes(&mut items, item, 0, positive, negative);
        }
        let nets = items.nets(&divided);
        assert_eq!(nets.agreement(), Some((36, 40)));
        assert_eq!(nets.cancellation(), 4);
        assert_eq!(nets.regime(), DesignRegime::Split);

        assert_eq!(DesignItems::default().nets(&divided).agreement(), None);
    }
}
