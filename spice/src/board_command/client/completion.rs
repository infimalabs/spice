//! Task completion and review client flows.

use super::*;
pub(super) fn client_done(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    let (requested_task, read) = match mutation_read(contract, matches) {
        Ok(read) => read,
        Err(outcome) => return outcome,
    };
    let written_operation = matches.get_one::<String>("operation");
    let operation = match written_operation {
        Some(written) => match written.parse::<wire::CanonicalUuid>() {
            Ok(operation) => Some(operation),
            Err(error) => {
                return refusal(
                    "task::completion-operation-invalid",
                    Some(written.clone()),
                    error.to_string(),
                );
            }
        },
        None => None,
    };
    let validation = matches
        .get_one::<String>("validation")
        .cloned()
        .unwrap_or_else(|| "completed by the authenticated lane".to_owned());
    let disposition = match requested_disposition(repository, matches) {
        Ok(disposition) => disposition,
        Err(outcome) => return outcome,
    };
    let board = match current_board(repository, read.clone()) {
        Ok(board) => board,
        Err(outcome) => return outcome,
    };
    let first = match completion_plan(
        &board,
        requested_task.as_ref(),
        operation,
        validation.clone(),
        disposition.as_ref(),
    ) {
        Ok(plan) => plan,
        Err(outcome) => return outcome,
    };
    let outcome = submit_completion(repository, contract, first.expected.clone(), &first.done);
    let refusal_reason = match &outcome {
        Outcome::Refusal(refusal)
            if matches!(
                refusal.reason(),
                "board::witness-moved" | "claim::generation-stale"
            ) =>
        {
            refusal.reason()
        }
        _ => return outcome,
    };

    let refreshed = match current_board(repository, read) {
        Ok(board) => board,
        Err(_) => return outcome,
    };
    let retry = match completion_plan(
        &refreshed,
        requested_task.as_ref(),
        operation,
        validation,
        disposition.as_ref(),
    ) {
        Ok(plan) => plan,
        Err(_) => return outcome,
    };
    if own_edit_is_only_completion_movement(&first, &retry) {
        return submit_completion(repository, contract, retry.expected, &retry.done);
    }
    if refusal_reason == "claim::generation-stale"
        && !retry.holder_acknowledged
        && task_witness_moved(&first, &retry)
    {
        return refusal(
            "board::witness-moved",
            Some(retry.done.task.to_string()),
            "read the current task before retrying completion",
        );
    }
    outcome
}

struct CompletionPlan {
    task: wire::TaskStamp,
    expected: Vec<Witness>,
    done: TaskDone,
    holder_acknowledged: bool,
}

/// A disposition as written, its commit cover already resolved to a landed id.
struct DispositionRequest {
    kind: String,
    cover: Option<CoverRequest>,
}

enum CoverRequest {
    Task(TaskHandle),
    Commit(String),
}

fn requested_disposition(
    repository: Host<'_>,
    matches: &ArgMatches,
) -> Result<Option<DispositionRequest>, Outcome<CommandAnswer>> {
    let cover = matches.get_one::<String>("cover");
    let Some(kind) = matches.get_one::<String>("disposition") else {
        return match cover {
            Some(cover) => Err(refusal(
                "task::disposition-cover-unbound",
                Some(cover.clone()),
                "name --disposition duplicate|covered|deferred with its --cover",
            )),
            None => Ok(None),
        };
    };
    let cover = match cover {
        None => None,
        Some(written) => Some(match written.parse::<TaskHandle>() {
            Ok(task) => CoverRequest::Task(task),
            Err(_) => CoverRequest::Commit(
                spice::task_git::resolve_landed_commit(&repository, repository.ambient, written)
                    .map_err(task_git_outcome)?,
            ),
        }),
    };
    Ok(Some(DispositionRequest {
        kind: kind.clone(),
        cover,
    }))
}

fn completion_plan(
    board: &BoardAnswer,
    requested_task: Option<&TaskHandle>,
    operation: Option<wire::CanonicalUuid>,
    validation: String,
    disposition: Option<&DispositionRequest>,
) -> Result<CompletionPlan, Outcome<CommandAnswer>> {
    let implicit = requested_task.is_none();
    let (task, heads) = match requested_task {
        Some(task) => {
            let heads = named_task_heads(board, task)?;
            (heads.statement.body.handle.clone(), heads)
        }
        None => sole_active(board)?,
    };
    let active = heads.held_by(&board.principal).is_some();
    // The principal's most recent claim names the completion operation, even
    // after the completion freed it.
    let claim = heads
        .claims
        .iter()
        .rev()
        .map(|held| &held.claim)
        .find(|claim| claim.body.actor == board.principal);
    if !active && implicit {
        return Err(refusal(
            "claim::active-missing",
            Some(board.principal.to_string()),
            "name the completed task to replay its terminal completion",
        ));
    }
    let (lane, occupant) = actor_seat(board)?;
    let mut expected = progress_witnesses(board, heads, lane, occupant, false)?;
    let disposition = disposition
        .map(|requested| plan_disposition(board, requested, &mut expected))
        .transpose()?;
    let operation = match (operation, claim) {
        (Some(operation), _) => operation,
        (None, Some(claim)) => TaskDone::operation_for_claim(claim.body.claimed),
        (None, None) => {
            return Err(refusal(
                "task::completion-operation-unavailable",
                Some(task.to_string()),
                "name an explicit operation when the task no longer retains a claim head",
            ));
        }
    };
    let holder_acknowledged = claim.is_some_and(|claim| {
        claim.body.actor == board.principal
            && claim.body.disposition == wire::ClaimDisposition::Active
            && claim.body.acknowledged == heads.workflow.body.generation
    });
    Ok(CompletionPlan {
        task: heads.statement.body.born.clone(),
        expected,
        done: TaskDone {
            task,
            validation,
            operation: Some(operation),
            disposition,
        },
        holder_acknowledged,
    })
}

/// The typed disposition, witnessing a covering task's current statement.
fn plan_disposition(
    board: &BoardAnswer,
    requested: &DispositionRequest,
    expected: &mut Vec<Witness>,
) -> Result<body::PlanDisposition, Outcome<CommandAnswer>> {
    let cover = match &requested.cover {
        None => None,
        Some(CoverRequest::Commit(commit)) => Some(body::DispositionCover::Commit(commit.clone())),
        Some(CoverRequest::Task(task)) => {
            let heads = named_task_heads(board, task)?;
            if heads.workflow.body.status.is_deleted() {
                return Err(refusal(
                    "task::disposition-cover-invalid",
                    Some(task.to_string()),
                    "a withdrawn task covers nothing; name the task or commit that answers the need",
                ));
            }
            let witness = Witness::Task {
                task: heads.statement.body.born.clone(),
                head: heads.statement.position,
            };
            if !expected.contains(&witness) {
                expected.push(witness);
            }
            Some(body::DispositionCover::Task(
                heads.statement.body.born.clone(),
            ))
        }
    };
    body::PlanDisposition::from_parts(&requested.kind, cover).map_err(|remedy| {
        refusal(
            "task::disposition-invalid",
            Some(requested.kind.clone()),
            remedy,
        )
    })
}

fn submit_completion(
    repository: Host<'_>,
    contract: &Contract,
    expected: Vec<Witness>,
    done: &TaskDone,
) -> Outcome<CommandAnswer> {
    let body = serde_json::to_value(done).expect("typed intent encodes");
    crate::command_client::execute_typed(
        repository,
        contract,
        CommandKind::TaskDone,
        expected,
        &body,
    )
}

/// A holder's own edit is the only stale completion read safe to refresh.
///
/// The journal advances that holder's acknowledgement with the edit. A foreign
/// edit leaves it behind. Every witness outside the edited task's statement
/// and workflow must remain exact, so a dependency, lease, residency, or other
/// concurrent movement still returns the first `board::witness-moved`.
fn own_edit_is_only_completion_movement(before: &CompletionPlan, after: &CompletionPlan) -> bool {
    after.holder_acknowledged && only_task_witnesses_moved(before, after)
}

fn only_task_witnesses_moved(before: &CompletionPlan, after: &CompletionPlan) -> bool {
    task_witness_moved(before, after)
        && stable_completion_witnesses(&before.expected, &before.task)
            .eq(stable_completion_witnesses(&after.expected, &after.task))
}

fn task_witness_moved(before: &CompletionPlan, after: &CompletionPlan) -> bool {
    if before.task != after.task
        || before.done.validation != after.done.validation
        || before.done.operation != after.done.operation
    {
        return false;
    }
    let before_head = task_head(&before.expected, &before.task);
    let after_head = task_head(&after.expected, &after.task);
    before_head.is_some() && after_head.is_some() && before_head != after_head
}

fn task_head(expected: &[Witness], task: &wire::TaskStamp) -> Option<wire::Seq> {
    expected.iter().find_map(|witness| match witness {
        Witness::Task {
            task: subject,
            head,
        } if subject == task => Some(*head),
        _ => None,
    })
}

fn stable_completion_witnesses<'a>(
    expected: &'a [Witness],
    task: &'a wire::TaskStamp,
) -> impl Iterator<Item = &'a Witness> + 'a {
    expected.iter().filter(move |witness| {
        !matches!(
            witness,
            Witness::Task { task: subject, .. } | Witness::Workflow { task: subject, .. }
                if subject == task
        )
    })
}

pub(super) fn client_review(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    mutate_task_from_board(repository, contract, matches, |board, task, heads| {
        let (lane, occupant) = actor_seat(board)?;
        let mut expected = progress_witnesses(board, heads, lane, occupant, true)?;
        let then = plan_followups(board, matches, &mut expected)?;
        let followup = plan_followup_edges(board, matches, &task, &mut expected)?;
        Ok((
            CommandKind::TaskReview,
            expected,
            serde_json::to_value(TaskReview {
                task,
                finding: matches
                    .get_one::<String>("finding")
                    .cloned()
                    .unwrap_or_default(),
                note: matches.get_one::<String>("note").cloned(),
                then,
                followup,
            })
            .expect("typed intent encodes"),
        ))
    })
}
