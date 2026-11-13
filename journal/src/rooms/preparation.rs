//! Design pass decision and execution authority.

use std::collections::BTreeSet;

use wire::Seq;
use wire::rooms::{PreparationDecision, ProposalRef};

use super::{
    Authority, AuthorizationRow, Change, Refusal, record, require_proposal, require_source,
    require_task, subject, unsound,
};

/// The operator's decision of one completed pass. Every output it bases on or
/// rules on is controlled by the decided Design; the command witnessed each
/// basis head current when the decision was admitted.
pub(super) fn decide(
    authority: &mut impl Authority,
    seq: Seq,
    decision: PreparationDecision,
) -> Result<(), Refusal> {
    let controller = &decision.decision.controller;
    require_task(authority, controller, "decision controller")?;
    let ruled = decision
        .outcome
        .accepted_outputs()
        .chain(decision.outcome.rejected_outputs());
    for output in decision
        .basis
        .outputs
        .iter()
        .map(|revision| &revision.task)
        .chain(ruled)
    {
        match authority.task(output)? {
            Some(Some(preparation)) if preparation.controller == *controller => {}
            Some(_) => {
                return unsound(format!(
                    "decided output {output} is not controlled by {controller}"
                ));
            }
            None => return unsound(format!("decided output {output} does not exist")),
        }
    }
    require_source(authority, seq, &decision.source)?;
    let proposal = ProposalRef::Decision {
        decision: decision.decision.clone(),
    };
    record(authority, seq, &proposal, controller.to_string(), &decision)
}

pub(super) fn authorize(
    authority: &mut impl Authority,
    seq: Seq,
    authorization: wire::rooms::ExecutionAuthorization,
) -> Result<(), Refusal> {
    let decision_ref = ProposalRef::Decision {
        decision: authorization.decision.clone(),
    };
    let decision: PreparationDecision = wire::decode(&require_proposal(authority, &decision_ref)?)?;
    let selected = decision
        .outcome
        .accepted_outputs()
        .cloned()
        .collect::<BTreeSet<_>>();
    if selected.is_empty() {
        return unsound("authorization requires a decision that accepts an output");
    }
    if authorization
        .selected_outputs
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        != selected
    {
        return unsound("authorization outputs differ from the decision's exact acceptance");
    }
    let decision_key = subject(&decision_ref)?;
    if authority.decision_authorized(&decision_key)? {
        return unsound("decision already authorized a selection");
    }
    let revisions = decision
        .basis
        .outputs
        .iter()
        .filter(|revision| selected.contains(&revision.task))
        .cloned()
        .collect::<Vec<_>>();
    if authorization.output_revisions != revisions {
        return unsound("authorization revisions differ from the decision's basis");
    }
    for revision in &authorization.output_revisions {
        require_revision(authority, revision)?;
    }
    let outputs = authorization
        .selected_outputs
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    authority.change(Change::Authorization {
        outputs,
        row: AuthorizationRow {
            decision: decision_key,
            seq: seq.get(),
            body: wire::encode(&authorization)?,
        },
    })?;
    Ok(())
}

fn require_revision(
    authority: &impl Authority,
    revision: &wire::rooms::OutputRevision,
) -> Result<(), Refusal> {
    if authority.task_statement(&revision.task)? != Some(revision.statement)
        || authority.task_dependencies(&revision.task)?.as_ref() != Some(&revision.dependencies)
    {
        return unsound(format!(
            "output {} changed since its decision",
            revision.task
        ));
    }
    Ok(())
}
