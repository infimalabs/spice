use super::*;
use wire::rooms::{
    DecisionOutcome, ExecutionAuthorization, ExecutionAuthorized, OutputAdopt, OutputControlled,
    PreparationDecide, PreparationDecided, ProposalRef,
};

mod decision;
pub(super) mod release;

pub(super) fn dispatch_preparation(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    repository: Option<Host<'_>>,
) -> Option<Outcome<CommandAnswer>> {
    Some(match command.body() {
        wire::intent::Intent::PreparationDecide(decision) => {
            decision::decide_preparation(bus, command, decision, repository)
        }
        wire::intent::Intent::OutputAdopt(adopt) => adopt_output(bus, command, adopt),
        _ => return None,
    })
}

fn require_statement_workflow_witnesses(
    command: &wire::AdmittedCommand,
    heads: &journal::TaskHeads,
) -> Result<(), Outcome<CommandAnswer>> {
    for witness in [
        Witness::Task {
            task: heads.statement().body().born.clone(),
            head: heads.statement().position(),
        },
        Witness::Workflow {
            task: heads.statement().body().born.clone(),
            head: heads.workflow().position(),
            phase: heads.workflow().body().phase,
        },
    ] {
        require_witness(command, &witness)?;
    }
    Ok(())
}

fn design_controller(
    heads: &journal::TaskHeads,
    controller: &TaskStamp,
) -> Result<(), Outcome<CommandAnswer>> {
    if heads
        .statement()
        .body()
        .intent
        .is_some_and(wire::TaskIntent::is_design)
    {
        return Ok(());
    }
    Err(refusal(
        "preparation::controller-not-design",
        Some(controller.to_string()),
        "decide Design passes only under a Design; a Plan's outputs follow its positive review",
    ))
}

/// Admit a decision's controller as a witnessed, open Design.
fn require_open_design(
    command: &wire::AdmittedCommand,
    heads: &journal::TaskHeads,
    controller: &TaskStamp,
) -> Result<(), Outcome<CommandAnswer>> {
    require_transition_witnesses(command, heads)?;
    design_controller(heads, controller)?;
    if !wire::intent::is_open_design(heads.statement().body().intent, heads.workflow().body()) {
        return Err(refusal(
            "preparation::controller-not-open",
            Some(controller.to_string()),
            "decide after the Design reaches its open design phase",
        ));
    }
    Ok(())
}

fn require_unblocked_selection(
    snapshot: &bus::TaskSnapshot,
    controller: &journal::TaskHeads,
) -> Result<(), Outcome<CommandAnswer>> {
    for dependency in &controller.statement().body().dependencies {
        if task_heads(snapshot, dependency)?
            .workflow()
            .body()
            .status
            .is_pending()
        {
            return Err(refusal(
                "preparation::controller-blocked",
                Some(dependency.to_string()),
                "settle the Design's pending dependencies before selecting its outputs",
            ));
        }
    }
    Ok(())
}

/// Refuse ending a pass while any current question of its Design lacks an
/// item-specific nonzero charge, naming each uncovered question. Coverage is
/// read from the admitted statement head, which the closing arrival witnesses,
/// so an edit refuses at the guard and a later charge can only add coverage.
pub(super) fn require_question_coverage(
    snapshot: &bus::TaskSnapshot,
    controller: &TaskStamp,
) -> Result<(), Outcome<CommandAnswer>> {
    let heads = task_heads(snapshot, controller)?;
    let statement = heads.statement().body();
    let quiet = wire::rooms::DesignItems::default();
    let coverage = snapshot
        .facts()
        .design_items(controller)
        .unwrap_or(&quiet)
        .coverage(&statement.term);
    if coverage.is_full() {
        return Ok(());
    }
    let handle = statement.handle(heads.workflow().body().phase);
    let current = wire::rooms::StatementItem::each(&statement.term).collect::<Vec<_>>();
    let (mut questions, mut items) = (Vec::new(), Vec::new());
    for question in coverage.missing {
        let item = question.handle(&current);
        items.push(format!(
            "{item}: {}",
            question.text_in(&statement.term).unwrap_or_default()
        ));
        questions.push(item.to_owned());
    }
    Err(refusal(
        "preparation::questions-uncovered",
        Some(questions.join(",")),
        format!(
            "investigate every uncovered question of {handle} and weigh each with `spice room \
             contribute {handle} --charge=C --question HEX TEXT` before ending the pass; \
             uncovered: {}",
            items.join("; ")
        ),
    ))
}

pub(super) fn adopt_output(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    request_body: &OutputAdopt,
) -> Outcome<CommandAnswer> {
    let (output, parent) = match adoption_stamps(request_body) {
        Ok(stamps) => stamps,
        Err(outcome) => return outcome,
    };
    let snapshot = match bus.task_snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => return store_fault("preparation::heads-unreadable", error.to_string()),
    };
    let output_heads = match task_heads(&snapshot, &output) {
        Ok(heads) => heads,
        Err(outcome) => return outcome,
    };
    let parent_heads = match task_heads(&snapshot, &parent) {
        Ok(heads) => heads,
        Err(outcome) => return outcome,
    };
    for heads in [output_heads, parent_heads] {
        if let Err(outcome) = require_transition_witnesses(command, heads) {
            return outcome;
        }
    }
    if !output_heads.workflow().body().status.is_pending()
        || output_heads.actively_claimed()
        || !output_heads.workflow().body().is_draft()
        || output_heads.statement().body().preparation.is_some()
        || output_heads.statement().body().intent.is_some()
    {
        return refusal(
            "preparation::adoption-output-ineligible",
            Some(request_body.output.to_string()),
            "adopt only pending, unclaimed, uncontrolled ordinary Draft work",
        );
    }
    if !parent_heads.workflow().body().status.is_pending()
        || parent_heads.actively_claimed()
        || !declares_plan(
            parent_heads.statement().body().intent,
            &parent_heads.workflow().body().flow,
        )
    {
        return refusal(
            "preparation::adoption-controller-ineligible",
            Some(request_body.preparation.to_string()),
            "choose a pending, unclaimed Plan or Design context as the controller",
        );
    }
    let preparation = wire::TaskPreparation {
        parent: parent.clone(),
        controller: parent_heads
            .statement()
            .body()
            .preparation
            .as_ref()
            .map_or_else(
                || parent.clone(),
                |preparation| preparation.controller.clone(),
            ),
        construction: output.clone(),
    };
    let at = match current_instant() {
        Ok(at) => at,
        Err(outcome) => return outcome,
    };
    let origin = match task_origin(&output) {
        Ok(origin) => origin,
        Err(outcome) => return outcome,
    };
    let mut write = JournalWrite::new(command.actor().clone(), origin, at, Producer::CommandDoor);
    guard(&mut write, command.expected().to_vec());
    let fact = match write.fact(
        OutputControlled {
            output: output.clone(),
            preparation: preparation.clone(),
        },
        FactCause::Root,
    ) {
        Ok(fact) => fact,
        Err(error) => return store_fault("preparation::adoption-invalid", error.to_string()),
    };
    let mut head = output_heads.statement().body().clone();
    head.preparation = Some(preparation);
    write.task_head(fact, move |revision| TaskHead { revision, ..head });
    let answer = TextAnswer {
        text: format!(
            "adopted {output} under controller {}",
            parent_heads.statement().body().born
        ),
    };
    if let Err(error) = write.command_result(
        request(command, request_body.clone()),
        command.epoch(),
        answer.clone(),
    ) {
        return store_fault("preparation::adoption-receipt-invalid", error.to_string());
    }
    committed(bus, write, CommandAnswer::Text(answer))
}
fn adoption_stamps(
    request_body: &OutputAdopt,
) -> Result<(TaskStamp, TaskStamp), Outcome<CommandAnswer>> {
    let output = request_body.output.born().map_err(|error| {
        refusal(
            "preparation::output-invalid",
            Some(request_body.output.to_string()),
            format!("name a task handle with a birth stamp: {error}"),
        )
    })?;
    let parent = request_body.preparation.born().map_err(|error| {
        refusal(
            "preparation::controller-invalid",
            Some(request_body.preparation.to_string()),
            format!("name a preparation task handle with a birth stamp: {error}"),
        )
    })?;
    Ok((output, parent))
}

fn require_proposal_absence(
    command: &wire::AdmittedCommand,
    proposal: &ProposalRef,
) -> Result<(), Outcome<CommandAnswer>> {
    require_witness(
        command,
        &Witness::Proposal {
            proposal: proposal.clone(),
            head: None,
        },
    )
}

fn require_transition_witnesses(
    command: &wire::AdmittedCommand,
    heads: &journal::TaskHeads,
) -> Result<(), Outcome<CommandAnswer>> {
    for witness in [
        Witness::Task {
            task: heads.statement().body().born.clone(),
            head: heads.statement().position(),
        },
        Witness::Workflow {
            task: heads.statement().body().born.clone(),
            head: heads.workflow().position(),
            phase: heads.workflow().body().phase,
        },
        Witness::Claim {
            task: heads.statement().body().born.clone(),
            claimed: None,
            head: heads.claim_plane(),
        },
    ] {
        require_witness(command, &witness)?;
    }
    Ok(())
}

fn require_witness(
    command: &wire::AdmittedCommand,
    witness: &Witness,
) -> Result<(), Outcome<CommandAnswer>> {
    if command.expected().contains(witness) {
        return Ok(());
    }
    Err(refusal(
        "preparation::witness-missing",
        Some(format!("{witness:?}")),
        "read the current preparation authority and retry the exact command",
    ))
}

fn task_heads<'a>(
    snapshot: &'a bus::TaskSnapshot,
    task: &TaskStamp,
) -> Result<&'a journal::TaskHeads, Outcome<CommandAnswer>> {
    snapshot
        .tasks()
        .iter()
        .find(|heads| heads.statement().body().born == *task)
        .ok_or_else(|| {
            refusal(
                "preparation::task-missing",
                Some(task.to_string()),
                "read the current board and name an existing task",
            )
        })
}
