use super::*;

mod command_dispatch;
pub(crate) use command_dispatch::dispatch_with_resources;

mod interchange;
pub(crate) use interchange::{TaskSetExport, TaskSetImport};
pub(crate) use interchange::{export as export_task_set, import as import_task_set};
mod preparation;
#[cfg(test)]
pub(in crate::board_command) use preparation::transition_witnesses;
mod join;
mod principal;
use join::client_join;
pub(crate) use join::{INVITE_EXPIRED, INVITE_EXPIRED_GUIDANCE};
pub(crate) use join::{JoinRequest, JoinSubmission, invite_recipient, join_task};
#[cfg(test)]
pub(in crate::board_command) use join::{direct_join_request, retain_current_handle};
pub(in crate::board_command) use join::{invitation_join, live_invitation};
#[cfg(test)]
pub(crate) use join::{invitation_offer, join_request};
mod requirements;
use requirements::{client_require, environment_requirements};
mod room;
use room::dispatch_room_client;
#[cfg(test)]
pub(super) use room::{cite, cited_tasks};
mod design_request;
use design_request::design_request_reply;
mod cycle;
mod place;
use cycle::{design_cycle, design_holder_next};

mod statement_items;
use statement_items::edited_term;
pub(super) use statement_items::statement_item_index;
/// Execute one generated board-family CLI leaf.
pub(crate) fn run(
    matches: &ArgMatches,
    contract: &Contract,
    ambient: &Ambient,
) -> Outcome<CommandAnswer> {
    match discover_repository() {
        Ok(repository) => dispatch_client(
            Host {
                repository: &repository,
                ambient,
            },
            contract,
            matches,
        ),
        Err(outcome) => outcome,
    }
}

/// Execute one generated board-family CLI leaf against `repository`.
pub(crate) fn run_at(
    repository: Host<'_>,
    matches: &ArgMatches,
    contract: &Contract,
) -> Outcome<CommandAnswer> {
    dispatch_client(repository, contract, matches)
}

/// Execute `task next`, keeping the empty-drain readout beside the outcome.
pub(crate) fn run_task_next(contract: &Contract, ambient: &Ambient) -> TaskNext {
    match discover_repository() {
        Ok(repository) => client_next(
            Host {
                repository: &repository,
                ambient,
            },
            contract,
        ),
        Err(outcome) => TaskNext {
            outcome,
            drain: None,
        },
    }
}

fn discover_repository() -> Result<Repository, Outcome<CommandAnswer>> {
    let current = std::env::current_dir()
        .map_err(|error| environment_fault("board::cwd-unavailable", error.to_string()))?;
    Repository::discover(current).map_err(|error| {
        environment_fault(spice::repository::ResolveError::REASON, error.to_string())
    })
}

fn dispatch_client(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    if let Some(outcome) = dispatch_room_client(repository, contract, matches)
        .or_else(|| principal::dispatch_principal_client(repository, contract, matches))
    {
        return outcome;
    }
    match contract.path {
        ["doc", "ledger"] => client_ledger(repository, contract, matches),
        ["sweep"] => run_sweep(repository, contract, matches),
        ["task", "decide"] => preparation::client_preparation_decide(repository, contract, matches),
        ["task", "adopt"] => preparation::client_output_adopt(repository, contract, matches),
        ["task", "list"] => client_list(repository, matches),
        ["task", "status"] => client_status(repository),
        ["task", "show"] => client_show(repository, contract, matches),
        ["task", "next"] => client_next(repository, contract).outcome,
        ["task", "add" | "plan" | "design"] => client_add(repository, contract, matches),
        ["task", "capture"] => client_capture(repository, contract, matches),
        ["task", "join"] => client_join(repository, contract, matches),
        ["task", "oops"] => client_oops(repository, contract, matches),
        ["task", "note"] => client_note(repository, contract, matches),
        ["task", "defer"] => client_defer(repository, contract, matches),
        ["task", "wake"] => client_wake(repository, contract, matches),
        ["task", "target"] => client_target(repository, contract, matches),
        ["task", "file"] => client_file(repository, contract, matches),
        ["task", "place"] => place::run(repository, contract, matches),
        ["task", "free"] => client_free(repository, contract, matches),
        ["task", "depends"] => client_depends(repository, contract, matches),
        ["task", "modify"] => client_modify(repository, contract, matches),
        ["task", "require"] => client_require(repository, contract, matches),
        ["task", "close" | "delete" | "conclude"] => client_settle(repository, contract, matches),
        ["task", "done"] => client_done(repository, contract, matches),
        ["task", "review"] => client_review(repository, contract, matches),
        _ => refusal(
            "board::client-mapping-missing",
            Some(contract.path.join(" ")),
            "use a registered board, task, or doc leaf",
        ),
    }
}

mod view;
pub(crate) use view::{BoardView, board_view};
#[cfg(test)]
pub(in crate::board_command) use view::{Posture, Standing, ViewRequest, project_board};
mod graph;
#[cfg(test)]
pub(in crate::board_command) use graph::{
    Destination, graph_page, handout_destination, page_or_file,
};
pub(crate) use graph::{GraphView, graph_snapshot_at, graph_view};

mod read;
pub(super) use read::*;
mod settle;
use settle::client_settle;
mod drain;
pub(crate) use drain::{DrainReadout, ParkedCounts, TaskNext};
pub(super) use drain::{actor_drafts, actor_rows};
mod recovery;
#[cfg(test)]
pub(super) use recovery::{RecoveryBoardState, recovery_board_state, recovery_preflight};
use recovery::{
    client_add_recovery, client_capture_recovery, restore_client_recovery,
    resume_pending_client_capture,
};
mod completion;
use completion::{client_done, client_review};
mod submission;
#[cfg(test)]
pub(super) use submission::CAPTURE_CREATION_FIELDS;
#[cfg(test)]
pub(in crate::board_command) use submission::append_preparation_witnesses;
pub(in crate::board_command) use submission::mutate_from_board;
pub(super) use submission::mutation_read;
pub(in crate::board_command) use submission::{OPEN_BOARD, retained_board};
pub(in crate::board_command) use submission::{
    artifact_operation_failure, repository_state_failure,
};
use submission::{client_add, client_capture, client_oops, submit_task_add_with_rule};
pub(crate) use submission::{current_board, submit_task_add};
use submission::{mutate_task, mutate_task_from_board};

fn client_ledger(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    let task = match required(matches, "HANDLE").parse::<TaskHandle>() {
        Ok(task) => task,
        Err(error) => {
            return refusal(
                "doc::handle-invalid",
                Some(required(matches, "HANDLE")),
                error.to_string(),
            );
        }
    };
    let output = matches.get_one::<String>("output");
    let requested_bytes = match matches.get_one::<String>("max-bytes") {
        Some(written) => match written.parse::<usize>() {
            Ok(value) => Some(value),
            Err(_) => {
                return refusal(
                    "doc::ledger-max-bytes",
                    Some(written.clone()),
                    "name a positive integer after --max-bytes",
                );
            }
        },
        None => None,
    };
    let offset = match matches.get_one::<String>("offset") {
        Some(written) => match written.parse::<usize>() {
            Ok(value) => value,
            Err(_) => {
                return refusal(
                    "doc::ledger-offset-invalid",
                    Some(written.clone()),
                    "name a non-negative byte offset printed by the preceding page",
                );
            }
        },
        None => 0,
    };
    let digest = match matches.get_one::<String>("digest") {
        Some(written) => match written.parse::<Digest>() {
            Ok(value) => Some(value),
            Err(error) => {
                return refusal(
                    "doc::ledger-digest-invalid",
                    Some(written.clone()),
                    error.to_string(),
                );
            }
        },
        None => None,
    };
    if output.is_some() && (requested_bytes.is_some() || offset != 0 || digest.is_some()) {
        return refusal(
            "doc::ledger-destination",
            None,
            "choose a bounded terminal page or --output FILE",
        );
    }
    if offset != 0 && digest.is_none() {
        return refusal(
            "doc::ledger-digest-missing",
            Some(offset.to_string()),
            "continue with the digest printed by the preceding page",
        );
    }
    let budget = match wire::output::INTERACTIVE.budget(wire::output::Request {
        rows: None,
        bytes: requested_bytes,
        text: None,
    }) {
        Ok(budget) => budget,
        Err(error) => {
            let detail = error.to_string();
            return refusal("doc::ledger-limits", Some(detail.clone()), detail);
        }
    };
    match crate::command_client::execute_typed(
        repository,
        contract,
        CommandKind::DocLedger,
        Vec::new(),
        &DocLedger { task: task.clone() },
    ) {
        Outcome::Reply(Reply {
            position,
            body: CommandAnswer::Text(answer),
        }) => match ledger_output(
            &task,
            &answer.text,
            output.map(String::as_str),
            budget,
            offset,
            digest.as_ref(),
        ) {
            Ok(text) => Outcome::Reply(Reply {
                position,
                body: CommandAnswer::Text(TextAnswer { text }),
            }),
            Err(refusal) => Outcome::Refusal(refusal),
        },
        Outcome::Reply(_) => protocol_fault(
            "doc::ledger-answer",
            "doc ledger read returned another answer shape",
        ),
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}

pub(crate) fn ledger_output(
    task: &TaskHandle,
    document: &str,
    output: Option<&str>,
    budget: wire::output::Budget,
    offset: usize,
    expected: Option<&Digest>,
) -> Result<String, Refusal> {
    if let Some(path) = output {
        return spice::export::export(std::path::Path::new(path), document.as_bytes())
            .map(|receipt| receipt.to_string());
    }
    let digest = Digest::compute(Domain::CliExport, document.as_bytes());
    if let Some(expected) = expected
        && expected != &digest
    {
        return Err(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "doc::ledger-snapshot-moved",
                Some("ledger-document".to_owned()),
                Some(expected.to_string()),
                Some(digest.to_string()),
            )
            .with_detail("restart at offset 0 to read the current ledger"),
        ));
    }
    let Some(tail) = document.get(offset..) else {
        return Err(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "doc::ledger-offset-invalid",
                Some("ledger-document".to_owned()),
                Some(format!("a UTF-8 byte boundary at most {}", document.len())),
                Some(offset.to_string()),
            )
            .with_detail("use the exact offset printed by the preceding page"),
        ));
    };
    let preview = wire::output::clip(tail, budget.limits().bytes);
    Ok(match preview.withheld_bytes {
        Some(withheld) => {
            let next = offset + preview.text.len();
            let continuation = wire::SpiceCommand::new(
                "doc",
                [
                    "ledger".to_owned(),
                    task.to_string(),
                    "--max-bytes".to_owned(),
                    budget.limits().bytes.to_string(),
                    "--offset".to_owned(),
                    next.to_string(),
                    "--digest".to_owned(),
                    digest.to_string(),
                ],
            )
            .expect("a ledger continuation has structural arguments");
            format!(
                "{}\n… [+{withheld} bytes]; continue: {continuation}\ncomplete: spice doc ledger {} --output {}-ledger.md",
                preview.text, task, task
            )
        }
        None => preview.text,
    })
}

fn client_next(repository: Host<'_>, contract: &Contract) -> TaskNext {
    let mut drain = None;
    let outcome = retry_task_next(|| {
        let board = match current_board(repository, BoardRead::Next) {
            Ok(board) => board,
            Err(outcome) => return outcome,
        };
        let Some(next) = board.next.as_ref() else {
            return protocol_fault(
                "board::next-state-missing",
                "a next-work board read returned no typed decision",
            );
        };
        let task = match next {
            NextWorkState::Active { task } => {
                return refresh_active_claim(repository, contract, &board, task);
            }
            NextWorkState::Candidate { task } => task,
            NextWorkState::AwaitingPeer { task, until } => {
                return Outcome::Reply(awaiting_peer_reply(&board, task, *until));
            }
            NextWorkState::DesignRequest { request } => {
                return Outcome::Reply(design_request_reply(&board, request));
            }
            NextWorkState::Dry {
                board: counts,
                drive,
                drain: draining,
            } => {
                drain = Some(Reply {
                    position: ReplyPosition::Snapshot(board.cursor),
                    body: DrainReadout::from_next(
                        counts,
                        *drive,
                        *draining,
                        &board.residency.body.lanes,
                    ),
                });
                return refusal(
                    "board::allocator-empty",
                    None,
                    "add or wake eligible work before requesting task next",
                );
            }
        };
        let (lane, occupant) = match actor_seat(&board) {
            Ok(seat) => seat,
            Err(outcome) => return outcome,
        };
        let rule = "next".parse::<Name>().expect("next is a wire name");
        crate::command_client::execute_typed(
            repository,
            contract,
            CommandKind::ClaimWin,
            vec![
                Witness::Task {
                    task: task.statement.body.born.clone(),
                    head: task.statement.position,
                },
                Witness::Workflow {
                    task: task.statement.body.born.clone(),
                    head: task.workflow.position,
                    phase: task.workflow.body.phase,
                },
                BoardClaim::plane_witness(&task.statement.body.born, &task.claims),
                Witness::Eligibility { rule: rule.clone() },
                residency_witness(&board, lane, occupant),
            ],
            &ClaimWin {
                rule,
                recovery: None,
            },
        )
    });
    // The resident can also drain empty after the client selected a row that
    // moved under it; that answer arrives without a snapshot, so read one.
    if drain.is_none() && allocator_empty(&outcome) {
        drain = current_board(repository, BoardRead::Next)
            .ok()
            .and_then(|board| next_drain_readout(&board));
    }
    TaskNext { outcome, drain }
}

fn awaiting_peer_reply(
    board: &BoardAnswer,
    task: &NextWorkTask,
    until: Instant,
) -> Reply<CommandAnswer> {
    let handle = task.statement.body.handle(task.workflow.body.phase);
    Reply {
        position: ReplyPosition::Snapshot(board.cursor),
        body: CommandAnswer::Text(TextAnswer {
            text: format!(
                "task {handle} awaits an eligible peer for phase:{} until {until}\n\
                 continuity: this lane authored the prior phase; no claim changed.\n\
                 next: let a peer run spice task next, or retry after {until}",
                task.workflow.body.phase,
            ),
        }),
    }
}

fn refresh_active_claim(
    repository: Host<'_>,
    contract: &Contract,
    board: &BoardAnswer,
    task: &NextWorkTask,
) -> Outcome<CommandAnswer> {
    let Some(held) = BoardClaim::held_by(&task.claims, &board.principal) else {
        return protocol_fault(
            "claim::head-missing",
            "an active next-work decision has no claim head",
        );
    };
    let (claim, Some(lease)) = (&held.claim, held.lease.as_ref()) else {
        return protocol_fault(
            "claim::lease-missing",
            "an active next-work decision has no lease head",
        );
    };
    let (lane, occupant) = match actor_seat(board) {
        Ok(seat) => seat,
        Err(outcome) => return outcome,
    };
    let current_residency = claim.body.lane == lane
        && claim.body.occupant == occupant
        && claim.body.epoch == board.residency.body.epoch
        && lease.body.epoch == board.residency.body.epoch;
    if current_residency && !claim.body.is_stale(&task.workflow.body) {
        return Outcome::Reply(existing_next_claim_reply(board, task));
    }
    let stamp = task.statement.body.born.clone();
    crate::command_client::execute_typed(
        repository,
        contract,
        CommandKind::ClaimRefresh,
        vec![
            Witness::Task {
                task: stamp.clone(),
                head: task.statement.position,
            },
            Witness::Workflow {
                task: stamp.clone(),
                head: task.workflow.position,
                phase: task.workflow.body.phase,
            },
            held.witness(&stamp),
            Witness::Lease {
                task: stamp,
                claimed: claim.body.claimed,
                head: lease.position,
            },
            residency_witness(board, lane, occupant),
        ],
        &TaskRef {
            task: task.statement.body.handle(task.workflow.body.phase),
        },
    )
}

fn existing_next_claim_reply(board: &BoardAnswer, task: &NextWorkTask) -> Reply<CommandAnswer> {
    let handle = task.statement.body.handle(task.workflow.body.phase);
    let next = if task.statement.body.taste.is_some() {
        format!("next: spice task modify {handle} --title \"...\"")
    } else if task.workflow.body.phase.as_str() == "review" {
        format!(
            "next: spice task review {handle} --finding clean --note \"description current; ...\""
        )
    } else if wire::intent::is_open_design(task.statement.body.intent, &task.workflow.body) {
        design_holder_next(&handle)
    } else {
        "next: spice task done --validation \"...\"".to_owned()
    };
    let mut text = format!(
        "claim {handle} phase:{}\nassigned to this lane: {}\n\
         continuity: task next retained the existing assignment; no claim changed.",
        task.workflow.body.phase, task.statement.body.title,
    );
    if let Some(ownership) = &task.ownership {
        text.push('\n');
        write_ownership(&mut text, ownership).expect("writing to a String cannot fail");
    }
    text.push('\n');
    text.push_str(&next);
    Reply {
        position: ReplyPosition::Snapshot(board.cursor),
        body: CommandAnswer::Text(TextAnswer { text }),
    }
}

fn write_ownership(
    writer: &mut impl std::fmt::Write,
    ownership: &wire::intent::TaskOwnership,
) -> std::fmt::Result {
    writer.write_str(if ownership.reserved {
        "reserved: "
    } else {
        "owners: "
    })?;
    if ownership.teams.is_empty() {
        return writer.write_str("none");
    }
    for (index, team) in ownership.teams.iter().enumerate() {
        if index != 0 {
            writer.write_str(", ")?;
        }
        write!(writer, "{team}")?;
    }
    Ok(())
}

fn ownerless_tasks(board: &BoardAnswer) -> std::collections::BTreeSet<&wire::TaskStamp> {
    board.unallocated.keys().collect()
}

fn next_drain_readout(board: &BoardAnswer) -> Option<Reply<DrainReadout>> {
    let NextWorkState::Dry {
        board: counts,
        drive,
        drain,
    } = board.next.as_ref()?
    else {
        return None;
    };
    Some(Reply {
        position: ReplyPosition::Snapshot(board.cursor),
        body: DrainReadout::from_next(counts, *drive, *drain, &board.residency.body.lanes),
    })
}

/// Whether an outcome is the typed refusal an exhausted allocator returns.
pub(crate) fn allocator_empty(outcome: &Outcome<CommandAnswer>) -> bool {
    matches!(
        outcome,
        Outcome::Refusal(refusal) if refusal.reason() == "board::allocator-empty"
    )
}

pub(super) const TASK_NEXT_ATTEMPTS: usize = 2;

pub(super) fn retry_task_next(
    mut attempt: impl FnMut() -> Outcome<CommandAnswer>,
) -> Outcome<CommandAnswer> {
    for index in 0..TASK_NEXT_ATTEMPTS {
        let outcome = attempt();
        if index + 1 < TASK_NEXT_ATTEMPTS && retryable_task_next(&outcome) {
            continue;
        }
        return outcome;
    }
    unreachable!("task-next retry loop always returns")
}

fn retryable_task_next(outcome: &Outcome<CommandAnswer>) -> bool {
    matches!(
        outcome,
        Outcome::Refusal(refusal)
            if refusal.reason() == "board::allocator-awaits-peer"
                || (refusal.reason() == "board::witness-moved"
                    && matches!(
                        refusal.diagnostic().and_then(wire::RefusalDiagnostic::observed),
                        Some(
                            "board::witness-moved: allocator projection changed"
                                | "board::witness-moved: allocator subscription head changed"
                                | "board::witness-moved: Design request projection changed"
                        )
                    ))
    )
}

fn client_note(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    mutate_task(repository, contract, matches, |task, heads| {
        let note = TaskNote {
            task: task.clone(),
            note: required(matches, "TEXT"),
        };
        Ok((
            CommandKind::TaskNote,
            vec![Witness::Existence {
                task: heads.statement.body.born.clone(),
            }],
            serde_json::to_value(note).expect("typed intent encodes"),
        ))
    })
}

fn client_defer(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    mutate_task_from_board(repository, contract, matches, |board, task, heads| {
        let wait = required(matches, "UNTIL")
            .parse::<wire::Instant>()
            .map_err(|error| {
                refusal(
                    "task::defer-instant-invalid",
                    Some(required(matches, "UNTIL")),
                    error.to_string(),
                )
            })?;
        let body = TaskDefer {
            task: task.clone(),
            wait,
        };
        let mut expected = vec![workflow_witness(heads), deferral_witness(heads)];
        if let Some(held) = heads.held_by(&board.principal) {
            let (lane, occupant) = actor_seat(board)?;
            let born = &heads.statement.body.born;
            let lease = held.lease_witness(born).ok_or_else(|| {
                protocol_fault("claim::lease-missing", "an active claim has no lease head")
            })?;
            expected.extend([
                held.witness(born),
                lease,
                residency_witness(board, lane, occupant),
            ]);
        }
        Ok((
            CommandKind::TaskDefer,
            expected,
            serde_json::to_value(body).expect("typed intent encodes"),
        ))
    })
}

fn client_wake(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    mutate_task_from_board(repository, contract, matches, |board, task, heads| {
        let claim = matches.get_flag("claim");
        let mut expected = vec![workflow_witness(heads), deferral_witness(heads)];
        if claim {
            let (lane, occupant) = actor_seat(board)?;
            expected.push(Witness::Eligibility {
                rule: "capture".parse().expect("capture is a wire name"),
            });
            expected.push(residency_witness(board, lane, occupant));
        }
        let body = TaskWake {
            task: task.clone(),
            claim,
        };
        Ok((
            CommandKind::TaskWake,
            expected,
            serde_json::to_value(body).expect("typed intent encodes"),
        ))
    })
}

/// Set an explicit completion target with operator authority.
fn client_target(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    mutate_task_from_board(repository, contract, matches, |_board, task, heads| {
        let phase = required(matches, "PHASE")
            .parse::<wire::Phase>()
            .map_err(|error| {
                refusal(
                    "completion::phase-invalid",
                    Some(required(matches, "PHASE")),
                    error.to_string(),
                )
            })?;
        let target = required(matches, "TARGET")
            .parse::<std::num::NonZeroU32>()
            .map_err(|_| {
                refusal(
                    "completion::target-invalid",
                    Some(required(matches, "TARGET")),
                    "name a positive count of distinct lanes",
                )
            })?;
        let expected = vec![workflow_witness(heads)];
        Ok((
            CommandKind::TaskTarget,
            expected,
            serde_json::to_value(TaskTarget {
                task: task.clone(),
                phase,
                target,
            })
            .expect("typed intent encodes"),
        ))
    })
}

fn client_file(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    mutate_task_from_board(repository, contract, matches, |board, task, heads| {
        let topic = submission::filing_topic_argument(matches)?;
        let claim = matches.get_flag("claim");
        let mut expected = vec![
            statement_witness(heads),
            workflow_witness(heads),
            deferral_witness(heads),
        ];
        if claim {
            let (lane, occupant) = actor_seat(board)?;
            expected.push(Witness::Eligibility {
                rule: "capture".parse().expect("capture is a wire name"),
            });
            expected.push(residency_witness(board, lane, occupant));
        }
        let body = wire::intent::TaskFile {
            task: task.clone(),
            topic,
            claim,
        };
        Ok((
            CommandKind::TaskFile,
            expected,
            serde_json::to_value(body).expect("typed intent encodes"),
        ))
    })
}

fn client_free(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    mutate_task_from_board(repository, contract, matches, |board, task, heads| {
        let (lane, occupant) = actor_seat(board)?;
        let born = &heads.statement.body.born;
        let Some((held, lease)) = heads
            .held_by(&board.principal)
            .and_then(|held| Some((held, held.lease_witness(born)?)))
        else {
            return Err(refusal(
                "claim::lease-missing",
                Some(task.to_string()),
                "name a task carrying a current live claim",
            ));
        };
        let expected = vec![
            held.witness(born),
            lease,
            residency_witness(board, lane, occupant),
        ];
        Ok((
            CommandKind::ClaimFree,
            expected,
            serde_json::to_value(TaskRef { task }).expect("typed intent encodes"),
        ))
    })
}

fn client_depends(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    mutate_task_from_board(repository, contract, matches, |board, task, heads| {
        let written = matches
            .get_many::<String>("AFTER")
            .expect("generated syntax requires dependencies")
            .map(|value| value.parse::<TaskHandle>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| refusal("task::dependency-invalid", None, error.to_string()))?;
        let remove = matches.get_flag("remove");
        let mut expected = vec![
            statement_witness(heads),
            workflow_witness(heads),
            claim_witness(heads),
        ];
        let mut after = Vec::with_capacity(written.len());
        let mut new_edges = Vec::new();
        for dependency in &written {
            let dependency_heads = named_task_heads(board, dependency)?;
            after.push(dependency_heads.statement.body.handle.clone());
            expected.push(statement_witness(dependency_heads));
            expected.push(workflow_witness(dependency_heads));
            let born = &dependency_heads.statement.body.born;
            if !remove
                && !heads.statement.body.dependencies.contains(born)
                && !new_edges.contains(born)
            {
                expected.push(Witness::Edge {
                    task: heads.statement.body.born.clone(),
                    after: born.clone(),
                });
                new_edges.push(born.clone());
            }
        }
        append_dependency_reachability_witnesses(board, &new_edges, &mut expected)?;
        Ok((
            CommandKind::TaskDepends,
            expected,
            serde_json::to_value(TaskDepends {
                task,
                after,
                remove,
            })
            .expect("typed intent encodes"),
        ))
    })
}

fn client_modify(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    mutate_task_from_board(repository, contract, matches, |board, task, heads| {
        let title = matches.get_one::<String>("title").cloned();
        let pitch = matches.get_one::<String>("pitch").cloned();
        let description = matches.get_one::<String>("description").cloned();
        let term = edited_term(matches, &heads.statement.body.term)?;
        let priority = matches.get_one::<String>("priority").cloned();
        let topic = submission::topic_argument(matches)?;
        let intent = intent_edit(matches)?;
        let flow = matches.get_one::<String>("flow").cloned();
        if title.is_none()
            && pitch.is_none()
            && description.is_none()
            && term.is_none()
            && priority.is_none()
            && topic.is_none()
            && intent.is_none()
            && flow.is_none()
        {
            return Err(refusal(
                "task::edit-empty",
                None,
                "name at least one of --title, --pitch, --description, --term, --replace-term, --remove-term, --priority, --topic, --intent, --clear-intent, or --flow",
            ));
        }
        let mut expected = vec![statement_witness(heads)];
        if flow.is_some() {
            expected.push(workflow_witness(heads));
            expected.push(completion_witness(heads));
        }
        let authority = if heads.workflow.body.is_draft() {
            heads
                .statement
                .body
                .preparation
                .as_ref()
                .map(|preparation| &preparation.controller)
                .map(|controller| task_heads_by_stamp(board, controller))
                .transpose()?
        } else {
            flow.as_ref().map(|_| heads)
        };
        if let Some(authority) = authority
            && let Some(claim) = authority.held_by(&board.principal)
        {
            expected.push(claim.witness(&authority.statement.body.born));
        }
        Ok((
            CommandKind::TaskEdit,
            expected,
            serde_json::to_value(TaskEdit {
                intent,
                task,
                title,
                pitch,
                description,
                term,
                requirements: None,
                priority,
                topic,
                flow,
            })
            .expect("typed intent encodes"),
        ))
    })
}

/// The explicit intent change one `task modify` names, if any.
fn intent_edit(
    matches: &ArgMatches,
) -> Result<Option<wire::TaskIntentEdit>, Outcome<CommandAnswer>> {
    let declared = declared_intent(matches)?;
    match (declared, matches.get_flag("clear-intent")) {
        (Some(intent), false) => Ok(Some(wire::TaskIntentEdit::Set { intent })),
        (None, true) => Ok(Some(wire::TaskIntentEdit::Clear {})),
        (None, false) => Ok(None),
        (Some(_), true) => Err(refusal(
            "task::intent-shape",
            None,
            "name --intent plan|design or --clear-intent, never both",
        )),
    }
}

/// The Plan or Design intent named by `--intent`, if any.
pub(super) fn declared_intent(
    matches: &ArgMatches,
) -> Result<Option<wire::TaskIntent>, Outcome<CommandAnswer>> {
    matches
        .get_one::<String>("intent")
        .map(|text| {
            text.parse().map_err(|error: String| {
                refusal(
                    "task::intent-invalid",
                    Some(error),
                    "name --intent plan or --intent design",
                )
            })
        })
        .transpose()
}

pub(super) fn actor_entries(board: &BoardAnswer) -> Result<Vec<Entry>, Outcome<CommandAnswer>> {
    board
        .tasks
        .iter()
        .map(|heads| {
            Entry::new(
                heads.client_statement(),
                heads.workflow.body.clone(),
                heads.completion.body.clone(),
                heads
                    .claims
                    .iter()
                    .map(|held| {
                        (
                            held.claim.body.clone(),
                            held.lease.as_ref().map(|lease| lease.body.clone()),
                        )
                    })
                    .collect(),
            )
            .map_err(|error| {
                protocol_fault(
                    "board::preflight-invalid",
                    format!(
                        "task {} current heads are invalid: {error}",
                        heads.statement.body.handle
                    ),
                )
            })
            .map(|entry| {
                entry
                    .with_creator_team(heads.creator_team)
                    .with_explicit_release(heads.explicit_release)
                    .with_design(heads.design_cycle)
            })
        })
        .collect()
}

/// Resolve a named task by its born stamp; the handle's stem is display only,
/// so a handle rendered under an earlier topic still names the same task.
pub(super) fn task_heads<'a>(
    board: &'a BoardAnswer,
    task: &TaskHandle,
) -> Result<&'a BoardTask, Outcome<CommandAnswer>> {
    task.born()
        .ok()
        .and_then(|born| {
            board
                .tasks
                .iter()
                .find(|heads| heads.statement.body.born == born)
        })
        .ok_or_else(|| {
            refusal(
                "task::unknown",
                Some(task.to_string()),
                "read board and name one current task handle",
            )
        })
}

/// Resolve a task the caller named on the command line, telling them on stderr
/// when that handle predates a topic move so they adopt the current name.
/// Stdout stays the command's document; resident-side resolution stays silent.
pub(super) fn named_task_heads<'a>(
    board: &'a BoardAnswer,
    task: &TaskHandle,
) -> Result<&'a BoardTask, Outcome<CommandAnswer>> {
    let heads = task_heads(board, task)?;
    if let Some(notice) = rename_notice(task, &heads.statement.body.handle) {
        eprintln!("{notice}");
    }
    Ok(heads)
}

/// The notice owed to a caller who named a task by a handle it no longer carries.
pub(crate) fn rename_notice(named: &TaskHandle, current: &TaskHandle) -> Option<String> {
    (named != current).then(|| format!("spice: {named} is now {current}; use the current handle"))
}

/// The validated entry a named task resolves to by born stamp alone.
pub(super) fn task_entry<'a>(entries: &'a [Entry], task: &TaskHandle) -> Option<&'a Entry> {
    let born = task.born().ok()?;
    entries.iter().find(|entry| entry.born() == &born)
}

pub(super) fn task_heads_by_stamp<'a>(
    board: &'a BoardAnswer,
    task: &wire::TaskStamp,
) -> Result<&'a BoardTask, Outcome<CommandAnswer>> {
    board
        .tasks
        .iter()
        .find(|heads| heads.statement.body.born == *task)
        .ok_or_else(|| {
            protocol_fault(
                "task::board-incoherent",
                "a stored dependency has no visible board task",
            )
        })
}

pub(super) fn statement_witness(heads: &BoardTask) -> Witness {
    Witness::Task {
        task: heads.statement.body.born.clone(),
        head: heads.statement.position,
    }
}

pub(super) fn workflow_witness(heads: &BoardTask) -> Witness {
    Witness::Workflow {
        task: heads.statement.body.born.clone(),
        head: heads.workflow.position,
        phase: heads.workflow.body.phase,
    }
}

pub(super) fn completion_witness(heads: &BoardTask) -> Witness {
    Witness::Completion {
        task: heads.statement.body.born.clone(),
        head: heads.completion.position,
    }
}

/// The claim-plane witness: the task's claims as a whole, as this snapshot saw
/// them.
pub(super) fn claim_witness(heads: &BoardTask) -> Witness {
    BoardClaim::plane_witness(&heads.statement.body.born, &heads.claims)
}

pub(super) fn deferral_witness(heads: &BoardTask) -> Witness {
    Witness::Deferral {
        task: heads.statement.body.born.clone(),
        head: heads.workflow.position,
        deferred: heads.workflow.body.deferral.is_some(),
    }
}

pub(super) fn residency_witness(
    board: &BoardAnswer,
    lane: wire::CanonicalUuid,
    occupant: wire::CanonicalUuid,
) -> Witness {
    Witness::Residency {
        lane,
        occupant: Some(occupant),
        epoch: board.residency.body.epoch,
    }
}

pub(super) fn actor_seat(
    board: &BoardAnswer,
) -> Result<(wire::CanonicalUuid, wire::CanonicalUuid), Outcome<CommandAnswer>> {
    let occupied =
        |lane: &wire::LaneResidency| lane.disposition == "occupied" && lane.occupant.is_some();
    let seat = match &board.principal {
        Actor::Lane(actor) => board
            .residency
            .body
            .lanes
            .iter()
            .find(|seat| seat.lane == *actor && occupied(seat)),
        Actor::Principal(_) => {
            let mut seats = board
                .residency
                .body
                .lanes
                .iter()
                .filter(|seat| occupied(seat));
            let first = seats.next();
            if seats.next().is_some() {
                return Err(refusal(
                    "claim::operator-lane-ambiguous",
                    None,
                    "use a lane capability when more than one occupied lane is seated",
                ));
            }
            first
        }
        _ => None,
    }
    .ok_or_else(|| {
        refusal(
            "claim::lane-unavailable",
            Some(board.principal.to_string()),
            "seat one occupied lane before changing claim ownership",
        )
    })?;
    Ok((
        seat.lane,
        seat.occupant.expect("occupied filter requires an occupant"),
    ))
}

fn sole_active(board: &BoardAnswer) -> Result<(TaskHandle, &BoardTask), Outcome<CommandAnswer>> {
    let mut held = board
        .tasks
        .iter()
        .filter(|heads| heads.held_by(&board.principal).is_some());
    let Some(heads) = held.next() else {
        return Err(refusal(
            "claim::active-missing",
            Some(board.principal.to_string()),
            "win one task with task next before completing its held phase",
        ));
    };
    if held.next().is_some() {
        return Err(protocol_fault(
            "claim::active-ambiguous",
            "one principal holds more than one active claim",
        ));
    }
    Ok((heads.statement.body.handle.clone(), heads))
}

pub(super) fn progress_witnesses(
    board: &BoardAnswer,
    heads: &BoardTask,
    lane: wire::CanonicalUuid,
    occupant: wire::CanonicalUuid,
    require_assignment: bool,
) -> Result<Vec<Witness>, Outcome<CommandAnswer>> {
    // The lane's latest claim, ended or not: the resident, not this client,
    // refuses progress under a claim that no longer holds.
    let held = heads
        .claims
        .iter()
        .rev()
        .find(|held| held.claim.body.lane == lane)
        .ok_or_else(|| {
            refusal(
                "claim::head-missing",
                Some(heads.statement.body.handle.to_string()),
                "win the task before advancing its workflow",
            )
        })?;
    let born = &heads.statement.body.born;
    let lease = held.lease_witness(born).ok_or_else(|| {
        protocol_fault(
            "claim::lease-missing",
            "an active claim head has no lease head",
        )
    })?;
    let mut expected = vec![
        statement_witness(heads),
        workflow_witness(heads),
        held.witness(born),
        lease,
        residency_witness(board, lane, occupant),
    ];
    if require_assignment {
        if held.claim.body.cell.phase.as_str() != "review" {
            return Err(refusal(
                "review::assignment-missing",
                Some(heads.statement.body.handle.to_string()),
                "win the review phase with task next before recording its verdict",
            ));
        }
        expected.push(Witness::Assignment {
            task: born.clone(),
            head: held.claim.body.claimed,
            reviewer: held.claim.body.actor.clone(),
        });
    }
    term_witnesses(board, heads, &mut expected)?;
    if heads.statement.body.intent == Some(wire::TaskIntent::Plan) {
        for output in planning::plan_outputs(&heads.statement.body.born, board.tasks.iter()) {
            expected.push(statement_witness(output));
            expected.push(workflow_witness(output));
        }
    }
    Ok(expected)
}

pub(super) fn allocate_task_stamp(
    board: &BoardAnswer,
    epoch_micros: i64,
) -> Result<wire::TaskStamp, Outcome<CommandAnswer>> {
    Ok(allocate_task_stamps(board, epoch_micros, 1)?
        .pop()
        .expect("one requested task stamp is allocated"))
}

pub(super) fn allocate_task_stamps(
    board: &BoardAnswer,
    epoch_micros: i64,
    count: usize,
) -> Result<Vec<wire::TaskStamp>, Outcome<CommandAnswer>> {
    wire::TaskStamp::allocate(
        epoch_micros,
        count,
        board.tasks.iter().map(|task| &task.statement.body.born),
    )
    .map_err(|error| environment_fault("task::stamp-clock-invalid", error.to_string()))
}

/// Dispatch one board-family request after transport admission.
#[cfg(test)]
pub(crate) fn dispatch(
    bus: &Bus,
    command: &wire::AdmittedCommand,
) -> Option<Outcome<CommandAnswer>> {
    if let wire::intent::Intent::TaskDefer(defer) = command.body() {
        return Some(task_defer(bus, command, defer, None, &steer::Config::default()).0);
    }
    dispatch_with_repository(bus, command, None, &board::NextConfig::default(), None)
}

/// Dispatch with the resident repository required by production Git boundaries
/// and the retained lane profiles `task next` and claims rank work by.
#[cfg(test)]
pub(crate) fn dispatch_with_repository(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    repository: Option<Host<'_>>,
    allocation: &board::NextConfig,
    profiles: OccupantProfiles<'_>,
) -> Option<Outcome<CommandAnswer>> {
    dispatch_with_resources(bus, command, repository, allocation, profiles, None)
}
