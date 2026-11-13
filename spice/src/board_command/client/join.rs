use super::*;

/// One `task join` request, whichever surface spelled it: the command line or
/// a write to a task's `spice://` invite or join URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JoinRequest {
    /// Invite one partner into the named task, or the caller's sole held one;
    /// no recipient recruits a teammate. A message is the captured write of
    /// the invite URL whose body is the requester's own words.
    Invite {
        task: Option<TaskHandle>,
        recipient: Option<wire::OccupantAddress>,
        message: Option<wire::outbox::Carrier>,
    },
    /// Fill a free slot of an already-shared task.
    Task(TaskHandle),
    /// Accept one live invitation.
    Invitation(steer::Key),
}

pub(super) fn client_join(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    match join_request(matches) {
        Ok(request) => submit_join(repository, contract, request, None),
        Err(outcome) => outcome,
    }
}

/// One resource-submitted join answer and the current handle resolved from the
/// same board snapshot that mapped its command.
pub(crate) struct JoinSubmission {
    pub(crate) current: Option<TaskHandle>,
    pub(crate) outcome: Outcome<CommandAnswer>,
}

/// Submit one join request as the resource writer's actor, retaining the
/// current handle from the command's authoritative resolution.
pub(crate) fn join_task(repository: Host<'_>, request: JoinRequest) -> JoinSubmission {
    let contract =
        crate::cli::contract(&["task", "join"]).expect("task join contract is registered");
    let mut current = None;
    let outcome = submit_join(repository, contract, request, Some(&mut current));
    JoinSubmission { current, outcome }
}

fn submit_join(
    repository: Host<'_>,
    contract: &Contract,
    request: JoinRequest,
    current: Option<&mut Option<TaskHandle>>,
) -> Outcome<CommandAnswer> {
    match request {
        JoinRequest::Invite {
            task,
            recipient,
            message,
        } => invite(repository, contract, task, (recipient, message), current),
        JoinRequest::Task(task) => direct_join(repository, contract, task, current),
        JoinRequest::Invitation(key) => invited_join(repository, contract, key),
    }
}

/// The request a `task join` command line spells.
pub(crate) fn join_request(matches: &ArgMatches) -> Result<JoinRequest, Outcome<CommandAnswer>> {
    let subject = matches.get_one::<String>("SUBJECT");
    if matches.contains_id("invite") {
        let recipient = matches
            .get_one::<String>("invite")
            .map(String::as_str)
            .map(invite_recipient)
            .transpose()?;
        let task = subject
            .map(|written| {
                written.parse::<TaskHandle>().map_err(|error| {
                    refusal(
                        "task::handle-invalid",
                        Some(written.clone()),
                        error.to_string(),
                    )
                })
            })
            .transpose()?;
        return Ok(JoinRequest::Invite {
            task,
            recipient,
            message: None,
        });
    }
    let Some(subject) = subject else {
        return Err(refusal(
            "task::join-subject",
            None,
            "name a task handle or invitation key, or invite a partner with --invite",
        ));
    };
    if let Ok(task) = subject.parse::<TaskHandle>() {
        return Ok(JoinRequest::Task(task));
    }
    match subject.parse::<steer::Key>() {
        Ok(key) if key.to_string() == *subject => Ok(JoinRequest::Invitation(key)),
        _ => Err(refusal(
            "task::invite-malformed",
            Some(subject.clone()),
            "name one current task handle or canonical invitation key",
        )),
    }
}

/// The lane an invitation addresses, as `--invite=ADDR` spells it, or the
/// refusal an unparseable address earns on every surface.
pub(crate) fn invite_recipient(
    written: &str,
) -> Result<wire::OccupantAddress, Outcome<CommandAnswer>> {
    written.parse::<wire::OccupantAddress>().map_err(|error| {
        refusal(
            "task::invite-address",
            Some(written.to_owned()),
            error.to_string(),
        )
    })
}

/// Invite one partner into the named task, or the caller's sole held one.
///
/// A planning phase may invite: the partner joins the phase the caller holds,
/// with the same board-only deliverable and no commit authority, so the
/// planning guard that keeps a plan holder out of other tasks' work does not
/// apply here.
fn invite(
    repository: Host<'_>,
    contract: &Contract,
    task: Option<TaskHandle>,
    (recipient, message): (Option<wire::OccupantAddress>, Option<wire::outbox::Carrier>),
    current: Option<&mut Option<TaskHandle>>,
) -> Outcome<CommandAnswer> {
    let read = task.as_ref().map_or(
        BoardRead::Tasks {
            tasks: Vec::new(),
            dependencies: false,
            claims: true,
            history: false,
        },
        task_read,
    );
    mutate_from_board(repository, contract, read, move |board| {
        retain_current_handle(&board, task.as_ref(), current)?;
        invitation_offer(&board, task.as_ref(), recipient, message)
    })
}

/// Map the CLI's invitation against the holder's current board authority.
pub(crate) fn invitation_offer(
    board: &BoardAnswer,
    task: Option<&TaskHandle>,
    recipient: Option<wire::OccupantAddress>,
    message: Option<wire::outbox::Carrier>,
) -> Result<(CommandKind, Vec<Witness>, serde_json::Value), Outcome<CommandAnswer>> {
    let (task, heads) = match task {
        Some(task) => (task.clone(), named_task_heads(board, task)?),
        None => sole_active(board)?,
    };
    let Some(claim) = heads.held_by(&board.principal) else {
        return Err(refusal(
            "task::invite-unheld",
            Some(task.to_string()),
            "only a current holder of the task's phase invites a partner",
        ));
    };
    let (lane, occupant) = actor_seat(board)?;
    if let Some(address) = recipient
        && seated(board, lane).is_some_and(|(_, own)| own == address)
    {
        return Err(refusal(
            "task::invite-self",
            Some(address.to_string()),
            "omit the address to invite a teammate, or name another lane",
        ));
    }
    Ok((
        CommandKind::TaskInvite,
        vec![
            statement_witness(heads),
            workflow_witness(heads),
            claim.witness(&heads.statement.body.born),
            residency_witness(board, lane, occupant),
        ],
        serde_json::to_value(wire::intent::TaskInvite {
            task,
            recipient,
            message,
        })
        .expect("typed intent encodes"),
    ))
}

fn direct_join(
    repository: Host<'_>,
    contract: &Contract,
    task: TaskHandle,
    current: Option<&mut Option<TaskHandle>>,
) -> Outcome<CommandAnswer> {
    let read = task_read(&task);
    mutate_from_board(repository, contract, read, move |board| {
        retain_current_handle(&board, Some(&task), current)?;
        direct_join_request(&board, task)
    })
}

/// Map a direct join without letting a planning holder move into other work.
pub(in crate::board_command) fn direct_join_request(
    board: &BoardAnswer,
    task: TaskHandle,
) -> Result<(CommandKind, Vec<Witness>, serde_json::Value), Outcome<CommandAnswer>> {
    require_no_planning_implementation(board, "task join")?;
    let heads = named_task_heads(board, &task)?;
    let (lane, occupant) = actor_seat(board)?;
    Ok((
        CommandKind::TaskJoin,
        vec![
            statement_witness(heads),
            workflow_witness(heads),
            claim_witness(heads),
            join_eligibility(),
            residency_witness(board, lane, occupant),
        ],
        serde_json::to_value(TaskJoin::Task { task }).expect("typed intent encodes"),
    ))
}

/// Retain the current rendering of a named task from the exact board snapshot
/// that maps its join. CLI submissions pass no slot and avoid the clone.
pub(in crate::board_command) fn retain_current_handle(
    board: &BoardAnswer,
    named: Option<&TaskHandle>,
    slot: Option<&mut Option<TaskHandle>>,
) -> Result<(), Outcome<CommandAnswer>> {
    let (Some(named), Some(slot)) = (named, slot) else {
        return Ok(());
    };
    *slot = Some(task_heads(board, named)?.statement.body.handle.clone());
    Ok(())
}

fn invited_join(
    repository: Host<'_>,
    contract: &Contract,
    key: steer::Key,
) -> Outcome<CommandAnswer> {
    let head = match steering_head(repository, &key) {
        Ok(head) => head,
        Err(outcome) => return outcome,
    };
    let control = match live_invitation(&head, &key) {
        Ok(control) => control,
        Err(outcome) => return outcome,
    };
    let read = invitation_read(&control);
    mutate_from_board(repository, contract, read, move |board| {
        invitation_join(&board, &key, &head, &control)
    })
}

/// The typed control of one still-live invitation directive. A lapsed or
/// canceled one expired; one already accepted or declined was answered.
pub(in crate::board_command) fn live_invitation(
    head: &journal::JournalHead<wire::SteeringHead>,
    key: &steer::Key,
) -> Result<wire::TaskInviteControl, Outcome<CommandAnswer>> {
    match head.body().disposition {
        wire::Disposition::Live => {}
        wire::Disposition::Lapsed | wire::Disposition::Canceled => return Err(expired(key)),
        wire::Disposition::Acked
        | wire::Disposition::Declined
        | wire::Disposition::DeadLettered => {
            return Err(refusal(
                "task::invite-settled",
                Some(key.to_string()),
                "this invitation was already answered; ask for a new invitation if more help is needed",
            ));
        }
    }
    head.body()
        .control
        .as_deref()
        .and_then(|control| control.parse::<wire::TaskInviteControl>().ok())
        .ok_or_else(|| expired(key))
}

/// The board read an invitation acceptance witnesses.
fn invitation_read(control: &wire::TaskInviteControl) -> BoardRead {
    BoardRead::Tasks {
        tasks: vec![control.task().clone()],
        dependencies: false,
        claims: true,
        history: false,
    }
}

/// The canonical invitation join request against one board answer: the
/// command kind, the exact witnesses it carries, and its body.
pub(in crate::board_command) fn invitation_join(
    board: &BoardAnswer,
    key: &steer::Key,
    head: &journal::JournalHead<wire::SteeringHead>,
    control: &wire::TaskInviteControl,
) -> Result<(CommandKind, Vec<Witness>, serde_json::Value), Outcome<CommandAnswer>> {
    require_no_planning_implementation(board, "task join")?;
    let heads = board
        .tasks
        .iter()
        .find(|heads| heads.statement.body.born == *control.task())
        .ok_or_else(|| expired(key))?;
    let (lane, occupant) = actor_seat(board)?;
    let Some(inviter) = head.body().sender.lane() else {
        return Err(expired(key));
    };
    let Some((inviter_occupant, _)) = seated(board, inviter) else {
        return Err(expired(key));
    };
    let Some(claim) = heads
        .active_claims()
        .find(|claim| claim.claim.body.claimed == control.claim())
    else {
        return Err(expired(key));
    };
    Ok((
        CommandKind::TaskJoin,
        vec![
            statement_witness(heads),
            workflow_witness(heads),
            claim_witness(heads),
            claim.witness(&heads.statement.body.born),
            join_eligibility(),
            residency_witness(board, lane, occupant),
            residency_witness(board, inviter, inviter_occupant),
            Witness::Directive {
                steer: head.body().directive,
                disposition: wire::Disposition::Live,
            },
        ],
        serde_json::to_value(TaskJoin::Invitation {
            key: key.to_string(),
        })
        .expect("typed intent encodes"),
    ))
}

fn steering_head(
    repository: Host<'_>,
    key: &steer::Key,
) -> Result<journal::JournalHead<wire::SteeringHead>, Outcome<CommandAnswer>> {
    let state = repository
        .admit_state()
        .map_err(|error| repository_state_failure(&error))?;
    let reader = journal::JournalReader::open(state.journal()).map_err(|error| {
        store_fault(
            "task::invite-head-unreadable",
            format!("cannot inspect invitation {key}: {error}"),
        )
    })?;
    let lookup = key.to_string();
    reader
        .read(move |read| read.steering_key(&lookup))
        .map_err(|error| {
            store_fault(
                "task::invite-head-unreadable",
                format!("cannot inspect invitation {key}: {error}"),
            )
        })?
        .ok_or_else(|| expired(key))
}

fn task_read(task: &TaskHandle) -> BoardRead {
    BoardRead::Tasks {
        tasks: vec![
            task.born()
                .expect("a parsed task handle carries a canonical task stamp"),
        ],
        dependencies: false,
        claims: true,
        history: false,
    }
}

fn join_eligibility() -> Witness {
    Witness::Eligibility {
        rule: "join".parse().expect("join is a wire name"),
    }
}

fn seated(
    board: &BoardAnswer,
    lane: wire::CanonicalUuid,
) -> Option<(wire::CanonicalUuid, wire::OccupantAddress)> {
    match board.addresses.get(&lane)? {
        wire::SeatAddress::Seated { occupant, address } => Some((*occupant, *address)),
        wire::SeatAddress::Vacant | wire::SeatAddress::Unproved { .. } => None,
    }
}

/// The refusal a join against an invitation that is no longer current earns;
/// the recipient's ACK scanner lapses such an invitation on this reason, and a
/// late answer to one that already lapsed or was canceled earns it too.
pub(crate) const INVITE_EXPIRED: &str = "task::invite-expired";
/// What a lane holding an expired invitation can do instead.
pub(crate) const INVITE_EXPIRED_GUIDANCE: &str = "ask the current task holder for a new invitation";

fn expired(key: &steer::Key) -> Outcome<CommandAnswer> {
    refusal(
        INVITE_EXPIRED,
        Some(key.to_string()),
        INVITE_EXPIRED_GUIDANCE,
    )
}
