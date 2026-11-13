use super::*;

/// Ordinary work born from the review its author holds is checked by that
/// review, including when the reviewer links it as a prerequisite separately.
/// Explicit routes and planning work retain their own contract.
pub(super) fn omit_origin_review(
    flow: &mut Vec<Phase>,
    witnesses: &mut Vec<Witness>,
    command: &wire::AdmittedCommand,
    add: &TaskAdd,
    snapshot: &ValidatedSnapshot,
    origin: &CreationCause,
) {
    if add.flow.is_some() || add.intent.is_some() || add.preparation.is_some() {
        return;
    }
    let wire::TaskProvenance::Task(parent) = &origin.provenance else {
        return;
    };
    let Some(parent) = snapshot.answer.tasks.iter().find(|heads| {
        &heads.statement.body.born == parent
            && heads.held_by(command.actor()).is_some()
            && heads.workflow.body.phase == Phase::Review
            && heads.workflow.body.status.is_pending()
            && heads.statement.body.intent.is_none()
            && !planning::declares_plan(heads.statement.body.intent, &heads.workflow.body.flow)
    }) else {
        return;
    };
    omit_review_phase(flow, true);
    witnesses.extend([
        statement_witness(parent),
        workflow_witness(parent),
        claim_witness(parent),
    ]);
}
