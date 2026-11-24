use super::*;

/// The one open phase a Design walks until the operator concludes it.
const DESIGN_PHASE: &str = "design";

/// The flow a creation that names none walks. A Design's open questions are
/// its acceptance, which it must make explicit before leaving draft; a
/// creation without acceptance prepends no plan bookend.
pub(super) fn default_flow(intent: Option<wire::TaskIntent>, flow: Option<&str>) -> Option<&str> {
    flow.or(match intent {
        Some(wire::TaskIntent::Plan) => Some("plan,review"),
        Some(wire::TaskIntent::Design | wire::TaskIntent::OpportunisticDesign) => {
            Some(DESIGN_PHASE)
        }
        None => None,
    })
}

/// Admit a Design's typed routes, each with draft at index zero: the direct
/// Design flow, or one ordinary intake phase before it.
pub(super) fn admit_design_flow(
    intent: Option<wire::TaskIntent>,
    flow: &[wire::Phase],
) -> Result<(), Outcome<CommandAnswer>> {
    if intent.is_none_or(|intent| intent.admits_flow(flow)) {
        return Ok(());
    }
    let flow = flow.iter().map(|phase| phase.as_str()).collect::<Vec<_>>();
    Err(refusal(
        "task::design-flow",
        Some(flow.join(",")),
        "use one admitted Design route: design, plan,design, or review,design",
    ))
}

/// Whether `phase` constructs work: a parent in it is a planning context.
fn constructs_work(phase: &str) -> bool {
    matches!(phase, "plan" | DESIGN_PHASE)
}

pub(super) fn declares_plan(intent: Option<wire::TaskIntent>, flow: &[wire::Phase]) -> bool {
    intent == Some(wire::TaskIntent::Plan)
        || (intent.is_none()
            && flow.contains(&wire::Phase::Plan)
            && flow.iter().all(|phase| {
                matches!(
                    phase,
                    wire::Phase::Draft | wire::Phase::Plan | wire::Phase::Review
                )
            }))
}

/// Whether `entry` delivers planning rather than implementation: loose
/// commits and recovered work never become part of one.
pub(super) fn planning_deliverable(entry: &Entry) -> bool {
    entry.statement().intent.is_some()
        || declares_plan(entry.statement().intent, &entry.workflow().flow)
        || entry.workflow().phase.as_str() == "design"
}

pub(super) fn admit_capture(entry: &Entry) -> Result<(), Outcome<CommandAnswer>> {
    if planning_deliverable(entry) {
        return Err(refusal(
            "task::capture-planning",
            Some(entry.handle().to_string()),
            "capture implementation only into an execution task, not a planning deliverable",
        ));
    }
    Ok(())
}

pub(super) fn construction_owner<'a>(
    born: &'a wire::TaskStamp,
    preparation: Option<&'a wire::TaskPreparation>,
) -> &'a wire::TaskStamp {
    preparation.map_or(born, |preparation| &preparation.construction)
}

pub(super) fn preparation_parent<'a>(
    board: &'a BoardAnswer,
    add: &TaskAdd,
) -> Result<Option<&'a BoardTask>, Outcome<CommandAnswer>> {
    if let Some(parent) = &add.preparation {
        return task_heads(board, parent).map(Some);
    }
    if add.origin.is_some() || !matches!(board.principal, Actor::Lane(_)) {
        return Ok(None);
    }
    let mut held = board
        .tasks
        .iter()
        .filter(|task| task.held_by(&board.principal).is_some());
    let Some(parent) = held.next() else {
        return Ok(None);
    };
    if held.next().is_some() {
        return Err(protocol_fault(
            "claim::active-ambiguous",
            "creation has more than one ambient claim",
        ));
    }
    Ok((parent.statement.body.intent.is_some()
        || declares_plan(parent.statement.body.intent, &parent.workflow.body.flow)
        || constructs_work(parent.workflow.body.phase.as_str()))
    .then_some(parent))
}

/// Bind a created statement to its planning parent.
pub(super) fn admit_preparation(
    snapshot: &ValidatedSnapshot,
    command: &wire::AdmittedCommand,
    add: &TaskAdd,
    statement: &mut TaskHead,
) -> Result<(), Outcome<CommandAnswer>> {
    if command.kind() != CommandKind::TaskAdd {
        return Ok(());
    }
    if let Some(parent) = preparation_parent(&snapshot.answer, add)? {
        if add.preparation.is_none() {
            require_expected(command, &claim_witness(parent))?;
        }
        bind_preparation(command, parent, statement)?;
    }
    Ok(())
}

/// Construction lineage is independent of execution prerequisites.
pub(super) fn bind_preparation(
    command: &wire::AdmittedCommand,
    parent: &BoardTask,
    statement: &mut TaskHead,
) -> Result<(), Outcome<CommandAnswer>> {
    require_expected(command, &statement_witness(parent))?;
    require_expected(command, &workflow_witness(parent))?;
    if !parent.workflow.body.status.is_pending()
        || (parent.statement.body.intent.is_none()
            && !constructs_work(parent.workflow.body.phase.as_str())
            && !declares_plan(parent.statement.body.intent, &parent.workflow.body.flow))
        || (parent
            .statement
            .body
            .intent
            .is_some_and(wire::TaskIntent::is_design)
            && !wire::intent::is_open_design(parent.statement.body.intent, &parent.workflow.body))
    {
        return Err(refusal(
            "task::preparation-refused",
            Some(parent.statement.body.handle.to_string()),
            "associate a preparation Plan or ordinary output with a pending planning context",
        ));
    }
    statement.preparation = Some(wire::TaskPreparation {
        parent: parent.statement.body.born.clone(),
        controller: parent.statement.body.preparation.as_ref().map_or_else(
            || parent.statement.body.born.clone(),
            |preparation| preparation.controller.clone(),
        ),
        construction: statement.born.clone(),
    });
    Ok(())
}

pub(super) fn correction_preparation(entry: &BoardTask) -> Option<wire::TaskPreparation> {
    let statement = &entry.statement.body;
    if statement.intent.is_none()
        && !declares_plan(statement.intent, &entry.workflow.body.flow)
        && statement.preparation.is_none()
    {
        return None;
    }
    Some(wire::TaskPreparation {
        parent: statement.born.clone(),
        controller: statement.preparation.as_ref().map_or_else(
            || statement.born.clone(),
            |preparation| preparation.controller.clone(),
        ),
        construction: construction_owner(&statement.born, statement.preparation.as_ref()).clone(),
    })
}

/// Acceptance-bearing executable outputs associated through construction lineage.
pub(crate) fn plan_outputs<'a, T: TermRow + 'a>(
    born: &wire::TaskStamp,
    rows: impl Iterator<Item = &'a T> + Clone,
) -> Vec<&'a T> {
    rows.filter(|row| {
        row.preparation().is_some_and(|preparation| {
            preparation.parent == *born || preparation.construction == *born
        }) && constructs(row.intent(), row.term(), row.workflow())
    })
    .collect()
}

/// Admit a successful completion of a task whose intent is Plan only through
/// linked executable work whose statement and workflow the command witnessed.
/// The Plan's own acceptance describes that work; it cannot stand in for it.
pub(super) fn require_output(
    snapshot: &ValidatedSnapshot,
    command: &wire::AdmittedCommand,
    entry: &Entry,
) -> Result<(), Outcome<CommandAnswer>> {
    let expected = command.expected();
    let outputs = plan_outputs(entry.born(), snapshot.answer.tasks.iter());
    if outputs.iter().any(|output| {
        expected.contains(&statement_witness(output))
            && expected.contains(&workflow_witness(output))
    }) {
        return Ok(());
    }
    let handle = entry.handle();
    if !outputs.is_empty() {
        return Err(refusal(
            "task::plan-witness-missing",
            Some(handle.to_string()),
            "read the current Plan and the work linked to it, then retry",
        ));
    }
    Err(refusal(
        "task::plan-output-missing",
        Some(handle.to_string()),
        format!(
            "Plan {handle} has no constructed child output carrying acceptance. Create one \
             under it with `spice task add TITLE --pitch TEXT --term TEXT \
             --preparation {handle}`; a Plan whose need warrants no work records why with \
             `spice task done --disposition no-work|duplicate|covered|deferred \
             [--cover HANDLE|COMMIT]`"
        ),
    ))
}

/// Whether work in this state is accepted non-planning output that answers a
/// Plan's need, which contradicts a no-construction disposition.
fn constructs(
    intent: Option<wire::TaskIntent>,
    term: &[String],
    workflow: &wire::WorkflowHead,
) -> bool {
    (workflow.status.is_pending() || workflow.concluded_positively())
        && intent.is_none()
        && !declares_plan(intent, &workflow.flow)
        && substantive_term(term)
}

/// Refuse a created, filed, edited, or relinked statement whose post state
/// would give a pending Plan that already recorded a no-construction
/// disposition accepted output, so the disposition and constructed work never
/// both hold, in either order. Corrections carry preparation without
/// construction and pass.
pub(super) fn refuse_disposed_construction(
    snapshot: &ValidatedSnapshot,
    statement: &TaskHead,
    same_write: &[wire::TaskStamp],
) -> Result<(), Outcome<CommandAnswer>> {
    let disposed = |plan: &BoardTask| {
        plan.workflow.body.status.is_pending()
            && snapshot
                .facts
                .plan_disposition(&plan.statement.body.born)
                .is_some()
    };
    let constructed_under = statement
        .preparation
        .as_ref()
        .filter(|preparation| preparation.construction == statement.born)
        .map(|preparation| &preparation.parent);
    // A Plan born in this same write has recorded no disposition yet.
    for plan in constructed_under
        .into_iter()
        .filter(|plan| !same_write.contains(plan))
    {
        let plan = visible_task(snapshot, plan)?;
        if disposed(plan) {
            return Err(disposed_refusal(plan));
        }
    }
    Ok(())
}

fn disposed_refusal(plan: &BoardTask) -> Outcome<CommandAnswer> {
    refusal(
        "task::plan-disposed",
        Some(plan.statement.body.handle.to_string()),
        "this Plan recorded that no work is warranted; construct or link accepted work \
         elsewhere, or let its review find the disposition wrong",
    )
}

pub(super) fn visible_task<'a>(
    snapshot: &'a ValidatedSnapshot,
    task: &wire::TaskStamp,
) -> Result<&'a BoardTask, Outcome<CommandAnswer>> {
    snapshot
        .answer
        .tasks
        .iter()
        .find(|heads| heads.statement.body.born == *task)
        .ok_or_else(|| {
            protocol_fault(
                "task::snapshot-incoherent",
                "validated board snapshot lost a task entry",
            )
        })
}

pub(super) fn require_expected(
    command: &wire::AdmittedCommand,
    witness: &Witness,
) -> Result<(), Outcome<CommandAnswer>> {
    if command
        .expected()
        .iter()
        .any(|expected| expected == witness)
    {
        return Ok(());
    }
    Err(refusal(
        "task::plan-witness-missing",
        Some(format!("{witness:?}")),
        "read the current plan task and its accepted child before closing the plan phase",
    ))
}
