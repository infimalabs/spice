use wire::intent::{BoardAnswer, BoardRead, BoardTask};
use wire::rooms::{OutputAdopt, PreparationDecide, PreparationDecision, ProposalRef};

use super::*;

#[cfg(test)]
mod tests;

/// A complete `DECISION_JSON` the command accepts, shown beside every parse
/// refusal so the shape is learnable from the refusal alone.
const DECISION_EXAMPLE: &str = r#"{"decision":{"controller":"dVzdhh8Nx","decision":"1"},"source":{"kind":"journal","fact":"6dth"},"basis":{"statement":1201,"room":1207,"outputs":[{"task":"dVzfwWPSh","statement":1190,"workflow":1191,"dependencies":["dVzdhh8Nx"]},{"task":"dVzfwgs7d","statement":1194,"workflow":1195,"dependencies":["dVzdhh8Nx"]}]},"outcome":{"kind":"resolved","outputs":[{"output":"dVzfwWPSh","verdict":"accepted","comment":"This boundary is ready."},{"output":"dVzfwgs7d","verdict":"rejected","comment":"Keep the retry contract explicit."}],"comment":"Proceed with the accepted output."}}"#;

fn decode_decision(text: &str) -> Result<PreparationDecision, Outcome<CommandAnswer>> {
    wire::decode(text.as_bytes()).map_err(|error| {
        refusal(
            "preparation::argument-invalid",
            Some("DECISION_JSON".to_owned()),
            format!("{error}; a complete DECISION_JSON this command accepts: {DECISION_EXAMPLE}"),
        )
    })
}

/// Submit the operator's decision, witnessing exactly the heads its basis
/// read, so any edit since refuses it as stale.
pub(super) fn client_preparation_decide(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    let decision = match decode_decision(&required(matches, "DECISION_JSON")) {
        Ok(decision) => decision,
        Err(outcome) => return outcome,
    };
    let design = decision.decision.controller.clone();
    let mut tasks = vec![design.clone()];
    tasks.extend(decision.outcome.accepted_outputs().cloned());
    let outcome = submit_with_tasks(repository, contract, tasks, true, |board| {
        let mut expected = transition_witnesses(task(board, &design)?);
        expected.push(Witness::Proposal {
            proposal: ProposalRef::Decision {
                decision: decision.decision.clone(),
            },
            head: None,
        });
        expected.push(Witness::Room {
            room: design.clone(),
            head: decision.basis.room,
        });
        for revision in &decision.basis.outputs {
            expected.push(Witness::Task {
                task: revision.task.clone(),
                head: revision.statement,
            });
            expected.push(Witness::Workflow {
                task: revision.task.clone(),
                head: revision.workflow,
                phase: wire::Phase::Draft,
            });
        }
        for output in decision.outcome.accepted_outputs() {
            expected.extend(transition_witnesses(task(board, output)?));
        }
        Ok((
            CommandKind::PreparationDecide,
            expected,
            PreparationDecide { decision },
        ))
    });
    with_cycle(repository, &design, outcome)
}

pub(super) fn client_output_adopt(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    let output = required(matches, "OUTPUT")
        .parse::<TaskHandle>()
        .map_err(|error| invalid_handle("OUTPUT", &error.to_string()));
    let preparation = required(matches, "PREPARATION")
        .parse::<TaskHandle>()
        .map_err(|error| invalid_handle("PREPARATION", &error.to_string()));
    let (output, preparation) = match (output, preparation) {
        (Ok(output), Ok(preparation)) => (output, preparation),
        (Err(outcome), _) | (_, Err(outcome)) => return outcome,
    };
    let stamps = match (output.born(), preparation.born()) {
        (Ok(output), Ok(preparation)) => vec![output, preparation],
        (Err(error), _) | (_, Err(error)) => {
            return invalid_handle("task handle", &error.to_string());
        }
    };
    submit_with_tasks(repository, contract, stamps, true, |board| {
        let output_stamp = output.born().expect("validated");
        let preparation_stamp = preparation.born().expect("validated");
        let mut expected = transition_witnesses(task(board, &output_stamp)?);
        expected.extend(transition_witnesses(task(board, &preparation_stamp)?));
        Ok((
            CommandKind::OutputAdopt,
            expected,
            OutputAdopt {
                output,
                preparation,
            },
        ))
    })
}

/// Append `design`'s pass cycle, read after the command committed, to a
/// text reply, so the reply names the state and action the command left.
/// A failed read leaves the committed reply as the resident wrote it.
fn with_cycle(
    repository: Host<'_>,
    design: &TaskStamp,
    outcome: Outcome<CommandAnswer>,
) -> Outcome<CommandAnswer> {
    let Outcome::Reply(Reply {
        position,
        body: CommandAnswer::Text(TextAnswer { mut text }),
    }) = outcome
    else {
        return outcome;
    };
    let read = BoardRead::Tasks {
        tasks: vec![design.clone()],
        dependencies: false,
        claims: false,
        history: false,
    };
    if let Ok(board) = current_board(repository, read)
        && let Some(task) = board
            .tasks
            .iter()
            .find(|task| task.statement.body.born == *design)
        && let Some(cycle) = design_cycle(&board, task)
    {
        for line in cycle.lines() {
            text.push('\n');
            text.push_str(&line);
        }
        text.push_str(&format!("\nnext: {}", cycle.action));
    }
    Outcome::Reply(Reply {
        position,
        body: CommandAnswer::Text(TextAnswer { text }),
    })
}

fn invalid_handle(name: &str, detail: &str) -> Outcome<CommandAnswer> {
    refusal(
        "preparation::argument-invalid",
        Some(name.to_owned()),
        format!("name a current task handle as `spice task show` prints it: {detail}"),
    )
}

fn submit_with_tasks<B, F>(
    repository: Host<'_>,
    contract: &Contract,
    mut tasks: Vec<TaskStamp>,
    claims: bool,
    map: F,
) -> Outcome<CommandAnswer>
where
    B: serde::Serialize,
    F: FnOnce(&BoardAnswer) -> Result<(CommandKind, Vec<Witness>, B), Outcome<CommandAnswer>>,
{
    tasks.sort();
    tasks.dedup();
    let board = match current_board(
        repository,
        BoardRead::Tasks {
            tasks,
            dependencies: false,
            claims,
            history: false,
        },
    ) {
        Ok(board) => board,
        Err(outcome) => return outcome,
    };
    let (kind, expected, body) = match map(&board) {
        Ok(mapped) => mapped,
        Err(outcome) => return outcome,
    };
    crate::command_client::execute_typed(repository, contract, kind, expected, &body)
}

fn task<'a>(
    board: &'a BoardAnswer,
    stamp: &TaskStamp,
) -> Result<&'a BoardTask, Outcome<CommandAnswer>> {
    board
        .tasks
        .iter()
        .find(|task| task.statement.body.born == *stamp)
        .ok_or_else(|| {
            refusal(
                "preparation::task-unavailable",
                Some(stamp.to_string()),
                "read the current board and name an existing visible task",
            )
        })
}

pub(in crate::board_command) fn transition_witnesses(task: &BoardTask) -> Vec<Witness> {
    vec![
        statement_witness(task),
        workflow_witness(task),
        claim_witness(task),
    ]
}
