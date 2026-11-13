//! Principal command-door settlement on the shared steering line model.

use super::*;

const INPUT_MOVED: &str = "steer::directive-moved";
const RECIPIENT_MOVED: &str = "steer::residency-moved";

/// Settle one exact principal-addressed input and apply its line control.
pub(in crate::resident) fn control(
    bus: &Bus,
    epoch: CanonicalUuid,
    config: &steer::Config,
    command: &wire::AdmittedCommand,
    request: &wire::intent::SteerLineControl,
    effects: &mut CommandEffects,
) -> Outcome<CommandAnswer> {
    let (answered, line, destination) = match authorize_input(bus, command, request) {
        Ok(authority) => authority,
        Err(outcome) => return outcome,
    };
    let at = match current_instant() {
        Ok(at) => at,
        Err(outcome) => return outcome,
    };
    let state = match reply::line_state(bus, line.root) {
        Ok(state) => state,
        Err(outcome) => return outcome,
    };
    let reaction = request.action == wire::intent::SteerLineAction::Ack
        && steer::ack_reaction(request.body.as_deref());
    let body = (!reaction).then_some(request.body.as_deref()).flatten();
    let action = control_action(request.action, reaction);
    let mut proposal = JournalWrite::new(
        command.actor().clone(),
        answered.body().origin.clone(),
        at,
        Producer::CommandDoor,
    );
    proposal.guard(live_guard(answered.body().directive, answered.position()));
    let retirement = match retire_input(&mut proposal, &answered, command, request, at) {
        Ok(retirement) => retirement,
        Err(outcome) => return outcome,
    };
    let context = ControlContext {
        bus,
        epoch,
        config,
        command,
        answered: &answered,
        retirement,
        state: &state,
        line,
        destination,
        reaction,
        body,
        action,
        at,
    };
    let result = if state.terminal {
        stage_closed(proposal, &context)
    } else {
        stage_live(proposal, &context)
    };
    let result = match result {
        Ok(result) => result,
        Err(outcome) => return outcome,
    };
    finish_control(bus, epoch, config, command, request, result, effects)
}

fn authorize_input(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    request: &wire::intent::SteerLineControl,
) -> Result<
    (
        journal::JournalHead<wire::SteeringHead>,
        wire::body::PeerLineRef,
        CanonicalUuid,
    ),
    Outcome<CommandAnswer>,
> {
    let directive = request.input.directive();
    let principal = request.input.principal;
    if command.actor() != &Actor::Principal(principal) {
        return Err(refusal(
            "steer::principal-input-owner",
            Some(Actor::Principal(principal).to_string()),
            Some(command.actor().to_string()),
            "only the exact principal endpoint can settle this input",
        ));
    }
    let answered = bus
        .read(move |read| read.steering_head(directive))
        .map_err(|error| store_fault("steer::input-unreadable", &error.to_string()))?
        .ok_or_else(|| {
            refusal(
                "steer::principal-input-unknown",
                Some(directive.to_string()),
                None,
                "refresh the principal inbox and retry an exact current input",
            )
        })?;
    if answered.body().recipient != Actor::Principal(principal)
        || answered.body().directive != directive
    {
        return Err(refusal(
            "steer::principal-input-owner",
            Some(Actor::Principal(principal).to_string()),
            Some(answered.body().recipient.to_string()),
            "the input belongs to another endpoint",
        ));
    }
    if answered.body().disposition != Disposition::Live {
        return Err(refusal(
            "steer::principal-input-retired",
            Some("live input".to_owned()),
            Some(format!("{:?}", answered.body().disposition).to_lowercase()),
            "refresh the principal inbox; a different command already settled this input",
        ));
    }
    let line = answered
        .body()
        .control
        .as_deref()
        .and_then(wire::parse_peer_line_control)
        .map(|(line, _, _)| line)
        .ok_or_else(|| {
            protocol_fault(
                "steer::principal-line-missing",
                "the principal input carries no shared line authority",
            )
        })?;
    let destination = answered.body().sender.lane().ok_or_else(|| {
        protocol_fault(
            "steer::principal-line-sender",
            "the principal input was not authored by a lane",
        )
    })?;
    Ok((answered, line, destination))
}

fn control_action(
    action: wire::intent::SteerLineAction,
    reaction: bool,
) -> wire::body::PeerLineAction {
    match action {
        wire::intent::SteerLineAction::Ack if reaction => wire::body::PeerLineAction::Reaction,
        wire::intent::SteerLineAction::Ack => wire::body::PeerLineAction::Continue,
        wire::intent::SteerLineAction::Nak => wire::body::PeerLineAction::Decline,
        wire::intent::SteerLineAction::Fin => wire::body::PeerLineAction::Finish,
    }
}

fn retire_input(
    proposal: &mut JournalWrite,
    answered: &journal::JournalHead<wire::SteeringHead>,
    command: &wire::AdmittedCommand,
    request: &wire::intent::SteerLineControl,
    at: Instant,
) -> Result<PendingFact, Outcome<CommandAnswer>> {
    let evidence = wire::body::SteeringEvidence::Command {
        command: command.id(),
    };
    let cause = FactCause::Existing(answered.body().created);
    let (retirement, next) = match request.action {
        wire::intent::SteerLineAction::Nak => {
            let declined = wire::body::SteerDeclined {
                directive: answered.body().directive,
                key: answered.body().key.clone(),
                recipient: answered.body().recipient.clone(),
                evidence,
                reason: if command.holds(wire::principal::PrincipalRole::Operator) {
                    request
                        .body
                        .as_deref()
                        .unwrap_or("declined by the endpoint principal")
                        .to_owned()
                } else {
                    "declined by the endpoint principal".to_owned()
                },
            };
            let pending = proposal
                .fact(declined.clone(), cause)
                .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
            let next = steer::Ledger::transition_head(
                answered.body(),
                steer::Steering::Declined(&declined),
                at,
            )
            .map_err(|error| store_fault("steer::index-invalid", &error.to_string()))?;
            (pending, next)
        }
        wire::intent::SteerLineAction::Ack | wire::intent::SteerLineAction::Fin => {
            let acked = wire::body::SteerAcked {
                directive: answered.body().directive,
                key: answered.body().key.clone(),
                recipient: answered.body().recipient.clone(),
                evidence,
            };
            let pending = proposal
                .fact(acked.clone(), cause)
                .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
            let next =
                steer::Ledger::transition_head(answered.body(), steer::Steering::Acked(&acked), at)
                    .map_err(|error| store_fault("steer::index-invalid", &error.to_string()))?;
            (pending, next)
        }
    };
    proposal.steering_head(retirement, move |_| next);
    Ok(retirement)
}

struct ControlContext<'a> {
    bus: &'a Bus,
    epoch: CanonicalUuid,
    config: &'a steer::Config,
    command: &'a wire::AdmittedCommand,
    answered: &'a journal::JournalHead<wire::SteeringHead>,
    retirement: PendingFact,
    state: &'a reply::LineState,
    line: wire::body::PeerLineRef,
    destination: CanonicalUuid,
    reaction: bool,
    body: Option<&'a str>,
    action: wire::body::PeerLineAction,
    at: Instant,
}

fn stage_closed(
    mut proposal: JournalWrite,
    context: &ControlContext<'_>,
) -> Result<ControlCommit, Outcome<CommandAnswer>> {
    proposal
        .fact(
            wire::body::SteerLineTransition {
                line: context.line.root,
                input: context.answered.body().directive,
                action: wire::body::PeerLineAction::Reaction,
                body: None,
            },
            FactCause::Pending(context.retirement),
        )
        .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
    Ok(ControlCommit {
        proposal,
        effect: wire::outbox::LineEffect {
            root: context.state.root.clone(),
            action: None,
            body: context.body.map(|_| wire::outbox::LineBody::Refused {
                reason: reply::LINE_CLOSED.to_owned(),
            }),
        },
        body: context.body.map(|_| wire::outbox::BodyFate::Unsent),
        notified: None,
    })
}

struct AdmittedBody {
    carried: Option<wire::outbox::LineBody>,
    fate: Option<wire::outbox::BodyFate>,
    notified: Option<CanonicalUuid>,
    fact: Option<CanonicalUuid>,
}

fn admit_body(
    proposal: &mut JournalWrite,
    context: &ControlContext<'_>,
) -> Result<AdmittedBody, Outcome<CommandAnswer>> {
    let Some(text) = context.body else {
        return Ok(AdmittedBody {
            carried: None,
            fate: None,
            notified: None,
            fact: None,
        });
    };
    if !context
        .command
        .holds(wire::principal::PrincipalRole::Operator)
    {
        return Ok(AdmittedBody {
            carried: Some(wire::outbox::LineBody::Refused {
                reason: "steer::operator-required".to_owned(),
            }),
            fate: Some(wire::outbox::BodyFate::Unsent),
            notified: None,
            fact: None,
        });
    }
    let residency = current_residency(context.bus, context.epoch).map_err(typed_outcome)?;
    let Some(row) = residency
        .body()
        .lanes
        .iter()
        .find(|row| row.lane == context.destination)
    else {
        return Ok(AdmittedBody {
            carried: Some(wire::outbox::LineBody::Refused {
                reason: LANE_REMOVED.to_owned(),
            }),
            fate: Some(wire::outbox::BodyFate::Unsent),
            notified: None,
            fact: None,
        });
    };
    let kind = match context.action {
        wire::body::PeerLineAction::Continue => wire::body::PeerBodyKind::Continue,
        wire::body::PeerLineAction::Decline => wire::body::PeerBodyKind::Decline,
        wire::body::PeerLineAction::Finish => wire::body::PeerBodyKind::Final,
        _ => unreachable!("a reaction carries no body"),
    };
    let control = wire::peer_line_control(context.line, kind, &[context.answered.body().directive]);
    let (key, _) = transitions::append_operator_directive(
        context.bus,
        context.answered.body().origin.clone(),
        context.config,
        proposal,
        transitions::OperatorDirective {
            directive: context.command.id(),
            fanout: None,
            lane: context.destination,
            sender: context.command.actor().clone(),
            text,
            control: Some(&control),
            room: context.answered.body().room.clone(),
        },
        context.at,
        FactCause::Pending(context.retirement),
    )?;
    let key_value = key
        .parse::<steer::Key>()
        .map_err(|error| store_fault("steer::key-invalid", &error.to_string()))?;
    proposal.guard(send_guard(
        residency.position(),
        Witness::Residency {
            lane: context.destination,
            occupant: row.occupant,
            epoch: context.epoch,
        },
        context.command.id(),
        key_value,
    ));
    let to = line::lane_address(context.bus, context.destination)?;
    Ok(AdmittedBody {
        carried: Some(wire::outbox::LineBody::Admitted(wire::outbox::Branch {
            to: wire::outbox::BranchTarget::Lane(to),
            key,
            admission: row.occupant.map(|_| wire::body::PeerAdmission::Active),
        })),
        fate: Some(wire::outbox::BodyFate::Sent),
        notified: Some(context.destination),
        fact: Some(context.command.id()),
    })
}

fn stage_live(
    mut proposal: JournalWrite,
    context: &ControlContext<'_>,
) -> Result<ControlCommit, Outcome<CommandAnswer>> {
    let admitted = admit_body(&mut proposal, context)?;
    if !context.reaction {
        proposal.guard(reply::line_open_guard(context.line.root));
    }
    proposal
        .fact(
            wire::body::SteerLineTransition {
                line: context.line.root,
                input: context.answered.body().directive,
                action: context.action,
                body: admitted.fact,
            },
            FactCause::Pending(context.retirement),
        )
        .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
    Ok(ControlCommit {
        proposal,
        effect: wire::outbox::LineEffect {
            root: context.state.root.clone(),
            action: Some(context.action),
            body: admitted.carried,
        },
        body: admitted.fate,
        notified: admitted.notified,
    })
}

struct ControlCommit {
    proposal: JournalWrite,
    effect: wire::outbox::LineEffect,
    body: Option<wire::outbox::BodyFate>,
    notified: Option<CanonicalUuid>,
}

fn finish_control(
    bus: &Bus,
    epoch: CanonicalUuid,
    config: &steer::Config,
    command: &wire::AdmittedCommand,
    request: &wire::intent::SteerLineControl,
    result: ControlCommit,
    effects: &mut CommandEffects,
) -> Outcome<CommandAnswer> {
    let ControlCommit {
        mut proposal,
        effect,
        body,
        notified,
    } = result;
    let answer = wire::intent::SteerLineAnswer {
        input: request.input.clone(),
        effect,
        body,
    };
    if let Err(error) = proposal.command_result(
        typed_command(command, request.clone()),
        epoch,
        answer.clone(),
    ) {
        return store_fault("steer::receipt-invalid", &error.to_string());
    }
    let committed = match commit(bus, proposal) {
        Ok(committed) => committed,
        Err(Outcome::Refusal(refused))
            if refused.reason() == reply::LINE_CLOSED
                || refused.reason() == RECIPIENT_MOVED
                || refused.reason() == INPUT_MOVED
                || refused.reason() == "steer::identity-taken" =>
        {
            return control(bus, epoch, config, command, request, effects);
        }
        Err(outcome) => return typed_outcome(outcome),
    };
    if let Some(lane) = notified {
        effects.notify_inbox(lane);
    }
    Outcome::Reply(Reply {
        position: ReplyPosition::Committed(SeqRange {
            first: seq(committed.first()),
            last: seq(committed.last()),
        }),
        body: CommandAnswer::SteerLine(Box::new(answer)),
    })
}
