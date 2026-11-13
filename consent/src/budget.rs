use std::collections::BTreeMap;

/// What spending one grant against a rule's bound produced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Spend {
    /// The grant was within bound; this many remain before renewal.
    Granted { remaining: u32 },
    /// The bound is reached, so the request queues for renewal instead.
    ///
    /// Reaching the bound never extends it. An operator renews explicitly, and
    /// until they do, every further request under this rule queues.
    Exhausted,
}

/// Grants spent per rule name since each rule's last renewal.
///
/// Budgets meter per name because the name is the rule's identity: renaming a
/// rule starts a fresh budget rather than inheriting the old rule's spend.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Budgets {
    spent: BTreeMap<String, u32>,
}

impl Budgets {
    /// A ledger in which no rule has spent a grant.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Grants this rule has left before its bound queues renewal.
    #[must_use]
    pub fn remaining(&self, rule: &str, bound: u32) -> u32 {
        bound.saturating_sub(self.spent.get(rule).copied().unwrap_or_default())
    }

    /// Spend one grant against this rule's bound.
    ///
    /// At the bound the spend is refused rather than recorded, so an exhausted
    /// rule cannot be pushed further past it by repeated asking.
    pub fn spend(&mut self, rule: &str, bound: u32) -> Spend {
        let spent = self.spent.entry(rule.to_owned()).or_default();
        if *spent >= bound {
            return Spend::Exhausted;
        }
        *spent += 1;
        Spend::Granted {
            remaining: bound - *spent,
        }
    }

    /// Restore this rule's full bound after an operator renews it.
    pub(crate) fn renew(&mut self, rule: &str) {
        self.spent.remove(rule);
    }
}
