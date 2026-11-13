//! The one no-construction disposition each Plan recorded when its plan phase
//! closed, so admission of every later phase reads it with one lookup.

use std::sync::Arc;

use journal::crossing::TaskStamp;
use journal::crossing::body::PlanDisposition;

use super::{Indexes, TaskFacts};

impl Indexes {
    /// Record `task`'s disposition. Later completions without one leave it in
    /// place, and admission refuses a second, different one.
    pub(super) fn observe_disposition(
        &mut self,
        task: &TaskStamp,
        disposition: Option<PlanDisposition>,
    ) {
        if let Some(disposition) = disposition {
            Arc::make_mut(&mut self.plan_dispositions).insert(task.clone(), disposition);
        }
    }
}

impl TaskFacts {
    /// The no-construction disposition `task` recorded, through settlement.
    #[must_use]
    pub fn plan_disposition(&self, task: &TaskStamp) -> Option<&PlanDisposition> {
        self.indexes.plan_dispositions.get(task)
    }
}
