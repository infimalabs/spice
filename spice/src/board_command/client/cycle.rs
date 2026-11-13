//! One open Design's pass cycle as every CLI surface presents it.
//!
//! The state is the resident's [`BoardAnswer::design_cycles`] judgment and
//! the text is [`wire::rooms::DesignCyclePresentation`], so task detail,
//! board rows, group reads, and command replies never derive readiness
//! themselves.

use super::*;

/// What a Design participant does before its share of the pass ends.
pub(super) fn design_holder_next(handle: &TaskHandle) -> String {
    format!(
        "next: investigate every remaining question and record item-specific, source-backed \
         findings before `spice task done --validation \"...\"` ends your share of this pass \
         of {handle}; only the operator decides a Design"
    )
}

/// `task`'s cycle when `board` judged it an open Design.
pub(super) fn design_cycle(
    board: &BoardAnswer,
    task: &BoardTask,
) -> Option<wire::rooms::DesignCyclePresentation> {
    let cycle = *board.design_cycles.get(&task.statement.body.born)?;
    Some(wire::rooms::DesignCyclePresentation::new(
        cycle,
        task.design_cycle,
        &task.statement.body.handle,
        task.design_availability(),
        task.design_coverage.as_ref()?,
        &task.statement.body.term,
        task.completion.body.target(&task.workflow.body.phase),
    ))
}
