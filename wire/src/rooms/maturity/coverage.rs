//! Current-statement question coverage, folded from the same item series that
//! maturity weighs but judged apart from it.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use super::{DesignItemState, DesignItems};
use crate::rooms::StatementItem;

/// One current statement's question coverage, in statement order. A question
/// is covered once an item-specific nonzero charge weighed it; coverage proves
/// neither concurrence nor the quality of the research behind the charge.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignCoverage {
    /// Items some item-specific nonzero charge weighed.
    pub covered: Vec<StatementItem>,
    /// Items no item-specific nonzero charge has weighed.
    pub missing: Vec<StatementItem>,
}

impl DesignCoverage {
    /// Whether every current question is covered; a statement naming none
    /// is.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.missing.is_empty()
    }

    /// Current-question research instruction, separate from pass readiness.
    #[must_use]
    pub fn research_guidance(&self, questions: &[String], design: &crate::TaskHandle) -> String {
        if self.is_full() {
            return "Design research: all current questions are covered; verify the findings before ending the pass. Coverage is not concurrence or proof of research quality.".to_owned();
        }
        let current = StatementItem::each(questions).collect::<Vec<_>>();
        let mut guidance = String::from(
            "Design research: investigate remaining questions before ending the pass: ",
        );
        for (offset, item) in self.missing.iter().enumerate() {
            if offset != 0 {
                guidance.push_str("; ");
            }
            write!(
                guidance,
                "{}: {}",
                item.handle(&current),
                item.text_in(questions)
                    .expect("coverage belongs to the current statement")
            )
            .expect("writing to a String succeeds");
        }
        write!(
            guidance,
            ". Record source-backed findings with `spice room contribute {design} --question \
             HEX --charge=C --cite SOURCE` (nonzero charge, either sign; coverage is not \
             concurrence or proof of research quality)."
        )
        .expect("writing to a String succeeds");
        guidance
    }
}

impl DesignItems {
    /// Which current items of `term` an item-specific nonzero charge
    /// has weighed, in statement order. A covered item may still be contested
    /// or unopposed: coverage is not concurrence.
    #[must_use]
    pub fn coverage(&self, term: &[String]) -> DesignCoverage {
        let mut coverage = DesignCoverage::default();
        for (item, state) in StatementItem::each(term).zip(self.states(term)) {
            if state == DesignItemState::Quiet {
                coverage.missing.push(item);
            } else {
                coverage.covered.push(item);
            }
        }
        coverage
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{ITEMS, charge, contribution, term};
    use super::*;
    use crate::rooms::{RoomContributed, RoomContributionKind, StatementItem};

    fn item_state(items: &DesignItems, item: &str) -> DesignItemState {
        items.states(&[item.to_owned()])[0]
    }

    #[test]
    fn coverage_counts_each_current_question_some_nonzero_charge_weighed() {
        let item = |text: &str| StatementItem::of(text);
        let mut items = DesignItems::default();
        assert!(
            items.coverage(&[]).is_full(),
            "a statement naming no question is covered"
        );
        let current = term(&ITEMS);
        assert_eq!(
            items.coverage(&current),
            DesignCoverage {
                covered: Vec::new(),
                missing: vec![item(ITEMS[0]), item(ITEMS[1])],
            }
        );

        // An item-specific ask and a zero charge enter no series.
        items.observe(&RoomContributed {
            item: Some(StatementItem::of(ITEMS[0])),
            ..contribution(10, 1, RoomContributionKind::Question)
        });
        items.observe(&charge(1, ITEMS[0], 0));
        assert_eq!(
            items.coverage(&current).missing,
            [item(ITEMS[0]), item(ITEMS[1])]
        );

        // A charge of either sign covers; a contested item stays covered.
        items.observe(&charge(1, ITEMS[1], -1));
        assert_eq!(items.coverage(&current).missing, [item(ITEMS[0])]);
        items.observe(&charge(1, ITEMS[0], 1));
        items.observe(&charge(2, ITEMS[0], -1));
        assert_eq!(item_state(&items, ITEMS[0]), DesignItemState::Contested);
        assert_eq!(
            items.coverage(&current),
            DesignCoverage {
                covered: vec![item(ITEMS[0]), item(ITEMS[1])],
                missing: Vec::new(),
            }
        );

        // Coverage follows the current statement's items in its order.
        let edited = term(&["Who reviews it?", ITEMS[1], ITEMS[0]]);
        assert_eq!(
            items.coverage(&edited),
            DesignCoverage {
                covered: vec![item(ITEMS[1]), item(ITEMS[0])],
                missing: vec![item("Who reviews it?")],
            }
        );
    }
}
