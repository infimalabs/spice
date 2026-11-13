//! Repository-bounded journal work derived from the admitted app row setting.

use super::{Failure, Fault, FaultClass, Source};

impl Source {
    pub(super) fn fact_budget(&self) -> Result<journal::FactReadBudget, Failure> {
        journal::FactReadBudget::new(self.rows, journal::MAX_RETAINED_FACT_PAGE_BYTES).map_err(
            |error| {
                Failure::Fault(Fault {
                    class: FaultClass::Protocol,
                    reason: "stream::fact-budget-invalid".to_owned(),
                    detail: error.to_string(),
                })
            },
        )
    }
}
