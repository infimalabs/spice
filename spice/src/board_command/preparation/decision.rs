//! The operator's decision on one completed Design pass, read from the
//! Design's controlled work: the accepted outputs it authorizes, the rejected
//! outputs it closes, or the conclusion or collapse it commits.
use super::*;
use wire::rooms::{OutputRevision, RoomSource};

/// What one decision commits beyond its own immutable record.
enum Consequence {
    /// A work resolution authorizes exactly its accepted outputs and closes
    /// its rejected ones with their pending subtrees.
    Resolve(Resolution),
    /// A return changes no task head.
    Record(&'static str),
    /// A conclusion or collapse ends its Design.
    Conclude {
        design: TaskHandle,
        conclusion: body::DesignConclusion,
        action: &'static str,
    },
}

/// The accepted output heads one decision releases and the authorization
/// that releases them.
type Authorization = (Vec<TaskHead>, ExecutionAuthorization);

/// The consequences one work resolution stages: accepted outputs gain
/// authorization and release; rejected outputs close.
struct Resolution {
    authorization: Option<Authorization>,
    rejected: Vec<TaskHandle>,
}

pub(super) fn decide_preparation(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    request_body: &PreparationDecide,
    repository: Option<Host<'_>>,
) -> Outcome<CommandAnswer> {
    let decision = &request_body.decision;
    let design = &decision.decision.controller;
    let subject = ProposalRef::Decision {
        decision: decision.decision.clone(),
    };
    if let Err(outcome) = require_proposal_absence(command, &subject) {
        return outcome;
    }
    let snapshot = match bus.task_snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => return store_fault("preparation::heads-unreadable", error.to_string()),
    };
    let controller = match task_heads(&snapshot, design) {
        Ok(heads) => heads,
        Err(outcome) => return outcome,
    };
    if let Err(outcome) = require_open_design(command, controller, design)
        .and_then(|()| require_completed_pass(&snapshot, controller, decision))
        .and_then(|()| require_basis(bus, &snapshot, command, controller, decision))
    {
        return outcome;
    }
    let handle = TaskHandle::for_topic(&controller.statement().body().topic.path, design);
    let consequence = match &decision.outcome {
        DecisionOutcome::Returned { .. } => Consequence::Record("returned the Design pass"),
        DecisionOutcome::Resolved { outputs, .. } => {
            match resolve_outputs(&snapshot, command, controller, decision, outputs) {
                Ok((authorization, rejected)) => Consequence::Resolve(Resolution {
                    authorization,
                    rejected,
                }),
                Err(outcome) => return outcome,
            }
        }
        DecisionOutcome::Concluded {} => Consequence::Conclude {
            design: handle,
            conclusion: body::DesignConclusion::Done {},
            action: "concluded the Design",
        },
        DecisionOutcome::Collapsed { reason } => Consequence::Conclude {
            design: handle,
            conclusion: body::DesignConclusion::Collapsed {
                reason: reason.clone(),
            },
            action: "collapsed the Design",
        },
    };
    commit_decision(bus, command, request_body, consequence, repository)
}

/// Admit a decision only of the Design's completed, undecided pass, sourced
/// at the round completion that ended it.
fn require_completed_pass(
    snapshot: &bus::TaskSnapshot,
    controller: &journal::TaskHeads,
    decision: &wire::rooms::PreparationDecision,
) -> Result<(), Outcome<CommandAnswer>> {
    let statement = controller.statement().body();
    let Some(pass) = snapshot.facts().design(statement).pass else {
        return Err(refusal(
            "preparation::pass-open",
            Some(statement.born.to_string()),
            "decide after the Design's pass ends: each participant records `spice task done`",
        ));
    };
    if decision.source
        != (RoomSource::Journal {
            fact: wire::SequenceCode::new(pass.get()),
        })
    {
        return Err(refusal(
            "preparation::pass-moved",
            Some(pass.to_string()),
            "read the Design's current completed pass and decide it",
        ));
    }
    Ok(())
}

/// The Design's offered work: every pending, acceptance-bearing Draft it
/// controls, at its current revision.
pub(super) fn offered_outputs<'a>(
    snapshot: &'a bus::TaskSnapshot,
    design: &'a TaskStamp,
) -> impl Iterator<Item = &'a journal::TaskHeads> {
    snapshot.tasks().iter().filter(move |heads| {
        let statement = heads.statement().body();
        heads.workflow().body().status.is_pending()
            && heads.workflow().body().is_draft()
            && !statement.term.is_empty()
            && statement
                .preparation
                .as_ref()
                .is_some_and(|preparation| preparation.controller == *design)
    })
}

fn revision(heads: &journal::TaskHeads) -> OutputRevision {
    let statement = heads.statement();
    OutputRevision {
        task: statement.body().born.clone(),
        statement: statement.position(),
        workflow: heads.workflow().position(),
        dependencies: statement.body().dependencies.clone(),
    }
}

/// Admit the heads the operator read: the Design statement and room, and
/// exactly the current revision of every offered output, each witnessed so a
/// later edit refuses the decision as stale.
fn require_basis(
    bus: &Bus,
    snapshot: &bus::TaskSnapshot,
    command: &wire::AdmittedCommand,
    controller: &journal::TaskHeads,
    decision: &wire::rooms::PreparationDecision,
) -> Result<(), Outcome<CommandAnswer>> {
    let basis = &decision.basis;
    let design = controller.statement().body().born.clone();
    if basis.statement != controller.statement().position() {
        return Err(refusal(
            "preparation::statement-moved",
            Some(basis.statement.to_string()),
            "read the current Design statement and decide again",
        ));
    }
    let room = design.clone();
    let head = bus
        .read(move |read| read.room_publication_head(&room))
        .map_err(|error| store_fault("preparation::room-unreadable", error.to_string()))?;
    if basis.room != head {
        return Err(refusal(
            "preparation::room-moved",
            Some(design.to_string()),
            "read the Design room's latest contributions and decide again",
        ));
    }
    require_witness(
        command,
        &Witness::Room {
            room: design.clone(),
            head,
        },
    )?;
    let mut offered = 0;
    for heads in offered_outputs(snapshot, &design) {
        offered += 1;
        let current = revision(heads);
        if !basis.outputs.contains(&current) {
            return Err(refusal(
                "preparation::output-revised",
                Some(current.task.to_string()),
                "read the Design's current controlled outputs and decide again",
            ));
        }
        require_statement_workflow_witnesses(command, heads)?;
    }
    if offered != basis.outputs.len() {
        return Err(refusal(
            "preparation::output-revised",
            Some(design.to_string()),
            "read the Design's current controlled outputs and decide again",
        ));
    }
    Ok(())
}

/// Match a resolution against every offered output and derive the heads it
/// authorizes and the handles it closes.
fn resolve_outputs(
    snapshot: &bus::TaskSnapshot,
    command: &wire::AdmittedCommand,
    controller: &journal::TaskHeads,
    decision: &wire::rooms::PreparationDecision,
    outputs: &[wire::rooms::OutputDecision],
) -> Result<(Option<Authorization>, Vec<TaskHandle>), Outcome<CommandAnswer>> {
    let offered = &decision.basis.outputs;
    if outputs.len() != offered.len()
        || outputs.iter().any(|verdict| {
            !offered
                .iter()
                .any(|revision| revision.task == verdict.output)
        })
    {
        return Err(refusal(
            "preparation::decision-output-mismatch",
            Some(decision.decision.controller.to_string()),
            "accept or reject every offered output; with none, conclude, collapse or return",
        ));
    }
    let accepted = decision
        .outcome
        .accepted_outputs()
        .cloned()
        .collect::<Vec<_>>();
    let rejected = rejected_output_handles(snapshot, &decision.outcome)?;
    let authorization = if accepted.is_empty() {
        None
    } else {
        Some(prepare_selection(
            snapshot, command, controller, decision, &accepted,
        )?)
    };
    Ok((authorization, rejected))
}

fn prepare_selection(
    snapshot: &bus::TaskSnapshot,
    command: &wire::AdmittedCommand,
    controller: &journal::TaskHeads,
    decision: &wire::rooms::PreparationDecision,
    selected_outputs: &[TaskStamp],
) -> Result<Authorization, Outcome<CommandAnswer>> {
    require_unblocked_selection(snapshot, controller)?;
    let outputs = selected_outputs
        .iter()
        .map(|output| selected_output_head(snapshot, command, output))
        .collect::<Result<Vec<_>, _>>()?;
    let authorization = ExecutionAuthorization {
        decision: decision.decision.clone(),
        selected_outputs: selected_outputs.to_vec(),
        output_revisions: decision
            .basis
            .outputs
            .iter()
            .filter(|revision| selected_outputs.contains(&revision.task))
            .cloned()
            .collect(),
    };
    Ok((outputs, authorization))
}

/// Every rejected output of a work resolution, as the handle its closure
/// names.
fn rejected_output_handles(
    snapshot: &bus::TaskSnapshot,
    outcome: &DecisionOutcome,
) -> Result<Vec<TaskHandle>, Outcome<CommandAnswer>> {
    outcome
        .rejected_outputs()
        .map(|output| {
            let heads = task_heads(snapshot, output)?;
            Ok(TaskHandle::for_topic(
                &heads.statement().body().topic.path,
                output,
            ))
        })
        .collect()
}

fn selected_output_head(
    snapshot: &bus::TaskSnapshot,
    command: &wire::AdmittedCommand,
    output: &TaskStamp,
) -> Result<TaskHead, Outcome<CommandAnswer>> {
    let heads = task_heads(snapshot, output)?;
    require_transition_witnesses(command, heads)?;
    if heads.actively_claimed() {
        return Err(refusal(
            "preparation::output-not-authorizable",
            Some(output.to_string()),
            "accept only an unclaimed output this Design offers",
        ));
    }
    Ok(heads.statement().body().clone())
}

fn commit_decision(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    request_body: &PreparationDecide,
    consequence: Consequence,
    repository: Option<Host<'_>>,
) -> Outcome<CommandAnswer> {
    let decision = &request_body.decision;
    let at = match current_instant() {
        Ok(at) => at,
        Err(outcome) => return outcome,
    };
    let origin = match task_origin(&decision.decision.controller) {
        Ok(origin) => origin,
        Err(outcome) => return outcome,
    };
    let mut write = JournalWrite::new(command.actor().clone(), origin, at, Producer::CommandDoor);
    guard(&mut write, command.expected().to_vec());
    let decided = match write.fact(
        PreparationDecided {
            decision: decision.clone(),
        },
        FactCause::Root,
    ) {
        Ok(fact) => fact,
        Err(error) => return store_fault("preparation::decision-invalid", error.to_string()),
    };
    let action = match consequence {
        Consequence::Resolve(resolution) => {
            let validation = format!("Rejected by decision {}", decision.decision.decision);
            match stage_resolution(
                bus,
                command,
                crate::board_command::mutation::DecisionWrite {
                    write: &mut write,
                    decided,
                    at,
                    repository: repository.map(|host| host.repository),
                },
                &validation,
                resolution,
                repository,
            ) {
                Ok(action) => action,
                Err(outcome) => return outcome,
            }
        }
        Consequence::Record(action) => action.to_owned(),
        Consequence::Conclude {
            design,
            conclusion,
            action,
        } => {
            let closed = match crate::board_command::mutation::stage_conclusion(
                bus,
                command,
                &TaskConclude {
                    task: design,
                    conclusion,
                },
                crate::board_command::mutation::DecisionWrite {
                    repository: repository.map(|host| host.repository),
                    write: &mut write,
                    decided,
                    at,
                },
            ) {
                Ok(closed) => closed,
                Err(outcome) => return outcome,
            };
            if closed.is_empty() {
                action.to_owned()
            } else {
                format!(
                    "{action} and closed {}",
                    closed
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
    };
    let answer = TextAnswer {
        text: format!("{action} through decision {}", decision.decision.decision),
    };
    if let Err(error) = write.command_result(
        request(command, request_body.clone()),
        command.epoch(),
        answer.clone(),
    ) {
        return store_fault("preparation::decision-receipt-invalid", error.to_string());
    }
    committed(bus, write, CommandAnswer::Text(answer))
}

/// Stage a work resolution's authorization, releases, and rejected-output
/// closures under its decision fact, then describe what it did.
fn stage_resolution(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    decision: crate::board_command::mutation::DecisionWrite<'_>,
    validation: &str,
    resolution: Resolution,
    host: Option<Host<'_>>,
) -> Result<String, Outcome<CommandAnswer>> {
    let crate::board_command::mutation::DecisionWrite {
        write,
        decided,
        at,
        repository,
    } = decision;
    let Resolution {
        authorization,
        rejected,
    } = resolution;
    let mut parts = Vec::with_capacity(2);
    if let Some((outputs, authorization)) = authorization {
        write
            .fact(
                ExecutionAuthorized {
                    authorization: authorization.clone(),
                },
                FactCause::Pending(decided),
            )
            .map_err(|error| {
                store_fault("preparation::authorization-invalid", error.to_string())
            })?;
        parts.push(format!("selected {} output(s)", outputs.len()));
        let selected = release::selected(
            bus,
            command,
            host,
            &authorization.selected_outputs,
            &authorization.decision.controller,
            write,
        )?;
        selected.append(write)?;
    }
    if !rejected.is_empty() {
        let closed = crate::board_command::mutation::stage_rejection(
            bus,
            command,
            &rejected,
            validation,
            crate::board_command::mutation::DecisionWrite {
                write,
                decided,
                at,
                repository,
            },
        )?;
        if !closed.is_empty() {
            parts.push(format!(
                "closed {}",
                closed
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    Ok(if parts.is_empty() {
        "resolved every output as already settled".to_owned()
    } else {
        parts.join(" and ")
    })
}
