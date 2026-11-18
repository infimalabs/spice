//! Steering command transitions and transcript-backed disposition.

use super::*;

/// One ordinary operator directive staged as part of a larger guarded command.
pub(crate) struct OperatorDirective<'a> {
    pub(crate) directive: CanonicalUuid,
    pub(crate) fanout: Option<CanonicalUuid>,
    pub(crate) lane: CanonicalUuid,
    pub(crate) sender: wire::Actor,
    pub(crate) text: &'a str,
    /// Machine control marker, when the direction carries one.
    pub(crate) control: Option<&'a str>,
    /// The room this direction speaks in, when it has one.
    pub(crate) room: Option<wire::rooms::RoomAnchor>,
}

/// Append ordinary steering authority without committing it separately.
///
/// Compound native commands use this to keep their room marker and steering
/// request in one journal write. The caller owns the residency guard and final
/// envelope-size admission.
pub(crate) fn append_operator_directive(
    bus: &Bus,
    origin: wire::Origin,
    config: &steer::Config,
    proposal: &mut JournalWrite,
    publication: OperatorDirective<'_>,
    at: Instant,
    cause: FactCause,
) -> Result<(String, PendingFact), Outcome<CommandAnswer>> {
    let (existing, minted_micros) = keyspace(bus, publication.directive)?;
    if existing.is_some() {
        return Err(refusal(
            "steer::identity-taken",
            Some("unused directive identity".to_owned()),
            Some(publication.directive.to_string()),
            "submit a new command identity",
        ));
    }
    let key = mint_key(minted_micros)?;
    let expires = expiry(at, config.expiry().get())?;
    let sent = wire::body::SteerSent {
        directive: publication.directive,
        fanout: publication.fanout,
        recipients: Vec::new(),
        key: key.to_string(),
        recipient: wire::Actor::Lane(publication.lane),
        text: publication.text.to_owned(),
        sender: publication.sender,
        room: publication.room,
        control: publication.control.map(str::to_owned),
        attachments: Vec::new(),
        expires,
    };
    let pending = proposal
        .fact(sent.clone(), cause)
        .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
    proposal.steering_head(pending, move |seq| {
        steer::Ledger::sent_head(&sent, origin, at, seq)
            .expect("the admitted compound send body builds its authority head")
    });
    Ok((key.to_string(), pending))
}

/// How long an occupied recipient has to answer a task invitation.
///
/// Help is wanted now or not at all: an invitation nobody answers within
/// minutes lapses on its own, so no lane carries a stale offer for a day.
pub(crate) const INVITATION_LIFETIME: std::time::Duration = std::time::Duration::from_secs(600);

/// One typed task invitation one holder stages into its own guarded write.
pub(crate) struct TaskInvitation<'a> {
    /// Exact task birth, phase and generation the inviter holds.
    pub(crate) task: &'a wire::TaskStamp,
    pub(crate) phase: &'a wire::Phase,
    pub(crate) generation: u64,
    /// Inviting lane, its current occupant's address, and its exact claim.
    pub(crate) inviter: CanonicalUuid,
    pub(crate) inviter_address: wire::OccupantAddress,
    pub(crate) inviter_claim: Seq,
    /// Recipient lane, the occupant residency seats there, and the residency
    /// head that seating was read at.
    pub(crate) recipient: CanonicalUuid,
    pub(crate) recipient_occupant: CanonicalUuid,
    pub(crate) residency: Seq,
    pub(crate) epoch: CanonicalUuid,
    pub(crate) text: String,
}

/// Append one durable peer directive carrying typed task-invite control,
/// returning its key and the instant it lapses on its own, the publication
/// instant plus [`INVITATION_LIFETIME`].
///
/// The directive is guarded against the recipient's exact seating, so a lane
/// that changed occupant before commit refuses the write rather than
/// receiving an invitation addressed to a seat it no longer has. The caller
/// owns final envelope-size admission.
pub(crate) fn append_task_invitation(
    bus: &Bus,
    origin: wire::Origin,
    proposal: &mut JournalWrite,
    invitation: TaskInvitation<'_>,
    at: Instant,
) -> Result<(String, Instant, CanonicalUuid), Outcome<CommandAnswer>> {
    let directive = CanonicalUuid::new(uuid::Uuid::new_v4());
    let (_, minted_micros) = keyspace(bus, directive)?;
    let key = mint_key(minted_micros)?;
    let control = wire::TaskInviteControl::new(wire::TaskInviteBinding {
        key: key.as_str(),
        task: invitation.task,
        phase: invitation.phase,
        generation: invitation.generation,
        inviter_lane: invitation.inviter,
        inviter_occupant: invitation.inviter_address,
        inviter_claim: invitation.inviter_claim,
        recipient_lane: invitation.recipient,
    });
    let expires = expiry(at, INVITATION_LIFETIME)?;
    let sent = wire::body::SteerSent {
        directive,
        fanout: None,
        recipients: Vec::new(),
        key: key.to_string(),
        recipient: wire::Actor::Lane(invitation.recipient),
        text: invitation.text,
        sender: wire::Actor::Lane(invitation.inviter),
        room: Some(invitation.task.clone()),
        control: Some(control.to_string()),
        attachments: Vec::new(),
        expires,
    };
    proposal.guard(send_guard(
        invitation.residency,
        Witness::Residency {
            lane: invitation.recipient,
            occupant: Some(invitation.recipient_occupant),
            epoch: invitation.epoch,
        },
        directive,
        key.clone(),
    ));
    let pending = proposal
        .fact(sent.clone(), FactCause::Root)
        .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
    proposal.steering_head(pending, move |seq| {
        steer::Ledger::sent_head(&sent, origin, at, seq)
            .expect("the admitted invitation body builds its authority head")
    });
    Ok((key.to_string(), expires, directive))
}

pub(in crate::resident) fn send(
    bus: &Bus,
    epoch: CanonicalUuid,
    config: &steer::Config,
    profiles: Option<&std::sync::Mutex<super::super::message_surface::MessageState>>,
    command: &wire::AdmittedCommand,
    send: &wire::intent::SteerSend,
    effects: &mut CommandEffects,
) -> Outcome<CommandAnswer> {
    if let Err(outcome) = admit_send_content(send) {
        return outcome;
    }
    let residency = match current_residency(bus, epoch) {
        Ok(residency) => residency,
        Err(outcome) => return typed_outcome(outcome),
    };
    let (lane, occupant, client_witness) = match send.lane {
        Some(requested) => {
            let Some(lane) = residency
                .body()
                .lanes
                .iter()
                .find(|lane| lane.lane == requested)
            else {
                return refusal(
                    "steer::lane-unknown",
                    None,
                    Some(requested.to_string()),
                    "seat the lane before sending it direction",
                );
            };
            (requested, lane.occupant, true)
        }
        None => match super::recipient::select(bus, residency.body(), profiles, &send.message) {
            Ok((lane, occupant)) => (lane, occupant, false),
            Err(outcome) => return outcome,
        },
    };
    if let Err(outcome) = admit_recipients(send, command.id(), lane, residency.body()) {
        return outcome;
    }
    publish(
        bus,
        config,
        command,
        &residency,
        Publication {
            lane,
            occupant,
            client_witness,
            fanout: Some(send.fanout),
            text: &send.message,
            attachments: &send.attachments,
            recipients: &send.recipients,
        },
        typed_command(command, send.clone()),
        effects,
    )
}

/// What one new directive says and whom it addresses.
struct Publication<'a> {
    lane: CanonicalUuid,
    occupant: Option<CanonicalUuid>,
    client_witness: bool,
    fanout: Option<CanonicalUuid>,
    text: &'a str,
    attachments: &'a [wire::Media],
    recipients: &'a [wire::body::SteeringRecipient],
}

/// A broadcast's declared targets name distinct seated lanes and directive
/// identities, including this command's own lane and identity. The identities
/// let inbox reads verify which sibling sends became durable.
fn admit_recipients(
    send: &wire::intent::SteerSend,
    directive: CanonicalUuid,
    lane: CanonicalUuid,
    residency: &wire::ResidencyHead,
) -> Result<(), Outcome<CommandAnswer>> {
    if send.recipients.is_empty() {
        return Ok(());
    }
    let mut named_lanes = BTreeSet::new();
    let mut named_directives = BTreeSet::new();
    let owns_self = send
        .recipients
        .contains(&wire::body::SteeringRecipient { lane, directive });
    let admitted = send.lane.is_some()
        && owns_self
        && send.recipients.iter().all(|recipient| {
            named_lanes.insert(recipient.lane)
                && named_directives.insert(recipient.directive)
                && residency.lanes.iter().any(|row| row.lane == recipient.lane)
        });
    if admitted {
        return Ok(());
    }
    Err(refusal(
        "steer::recipients-invalid",
        Some(
            "distinct seated lane and directive pairs including this addressed command".to_owned(),
        ),
        Some(format!("{:?}", send.recipients)),
        "declare broadcast targets once and include this command identity at its addressed lane",
    ))
}

/// Journal one new directive against the residency it witnessed.
fn publish<B: serde::Serialize>(
    bus: &Bus,
    config: &steer::Config,
    command: &wire::AdmittedCommand,
    residency: &journal::JournalHead<wire::ResidencyHead>,
    publication: Publication<'_>,
    receipt: wire::Command<B>,
    effects: &mut CommandEffects,
) -> Outcome<CommandAnswer> {
    let epoch = residency.body().epoch;
    let witness = Witness::Residency {
        lane: publication.lane,
        occupant: publication.occupant,
        epoch,
    };
    if publication.client_witness {
        if command.expected() != [witness.clone()] {
            return witness_refusal(command.expected(), &witness);
        }
    } else if !command.expected().is_empty() {
        return refusal(
            "steer::automatic-witness-unexpected",
            Some("no caller-selected residency witness".to_owned()),
            Some(format!("{:?}", command.expected())),
            "omit expected residency when the resident selects the recipient",
        );
    }
    let (existing, minted_micros) = match keyspace(bus, command.id()) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    if existing.is_some() {
        return refusal(
            "steer::identity-taken",
            Some("unused directive identity".to_owned()),
            Some(command.id().to_string()),
            "submit a new command identity",
        );
    }
    let at = match current_instant() {
        Ok(at) => at,
        Err(outcome) => return outcome,
    };
    let key = match mint_key(minted_micros) {
        Ok(key) => key,
        Err(outcome) => return outcome,
    };
    let expires = match expiry(at, config.expiry().get()) {
        Ok(expires) => expires,
        Err(outcome) => return outcome,
    };
    let sent = wire::body::SteerSent {
        directive: command.id(),
        fanout: publication.fanout,
        recipients: publication.recipients.to_vec(),
        key: key.to_string(),
        recipient: wire::Actor::Lane(publication.lane),
        text: publication.text.to_owned(),
        sender: command.actor().clone(),
        room: None,
        control: None,
        attachments: publication.attachments.to_vec(),
        expires,
    };
    let origin = match resident_origin(epoch) {
        Ok(origin) => origin,
        Err(outcome) => return typed_outcome(outcome),
    };
    let mut proposal = JournalWrite::new(
        command.actor().clone(),
        origin.clone(),
        at,
        Producer::CommandDoor,
    );
    proposal.guard(send_guard(
        residency.position(),
        witness,
        command.id(),
        key.clone(),
    ));
    let pending = match proposal.fact(sent.clone(), FactCause::Existing(residency.position())) {
        Ok(pending) => pending,
        Err(error) => return store_fault("steer::fact-invalid", &error.to_string()),
    };
    if let Err(outcome) = admit_send_envelope(&proposal) {
        return outcome;
    }
    proposal.steering_head(pending, move |seq| {
        steer::Ledger::sent_head(&sent, origin, at, seq)
            .expect("the admitted send body builds its authority head")
    });
    let answer = DirectiveAnswer {
        directive: command.id(),
        recipient: Actor::Lane(publication.lane),
        key: key.to_string(),
        authored: at,
        disposition: Disposition::Live,
    };
    if let Err(error) = proposal.command_result(receipt, epoch, answer.clone()) {
        return store_fault("steer::receipt-invalid", &error.to_string());
    }
    finish(bus, proposal, answer, effects)
}

fn admit_send_content(send: &wire::intent::SteerSend) -> Result<(), Outcome<CommandAnswer>> {
    if let Err(error) = wire::media::admit_media(&send.attachments) {
        return Err(refusal(
            "steer::media-invalid",
            Some(format!(
                "at most {} admitted images with at most {} combined data-URL bytes",
                wire::media::MAX_MEDIA_ITEMS,
                wire::media::MAX_MEDIA_DATA_URL_BYTES,
            )),
            Some(error.to_string()),
            "remove or replace the invalid attachment and retry the same authored message",
        ));
    }
    if send
        .attachments
        .iter()
        .any(|media| send.message.contains(&media.data_url))
    {
        return Err(refusal(
            "steer::media-duplicated",
            Some("attachment bytes retained only in attachments".to_owned()),
            None,
            "use bounded attachment Markdown references instead of inline data URLs",
        ));
    }
    if send.message.trim().is_empty() {
        return Err(refusal(
            "steer::message-empty",
            Some("non-empty direction".to_owned()),
            Some(send.message.clone()),
            "supply direction text that the lane can act on",
        ));
    }
    Ok(())
}

pub(crate) fn admit_send_envelope(proposal: &JournalWrite) -> Result<(), Outcome<CommandAnswer>> {
    let bytes = proposal
        .envelope_byte_bound()
        .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
    let maximum = journal::MAX_DURABLE_FACT_ENVELOPE_BYTES;
    if bytes > maximum {
        return Err(refusal(
            "steer::envelope-too-large",
            Some(format!("at most {maximum} durable envelope bytes")),
            Some(format!(
                "{bytes} bytes including reserved journal cursor width"
            )),
            "shorten the direction or reduce its attachments and retry the same authored message",
        ));
    }
    Ok(())
}

pub(in crate::resident) fn cancel(
    bus: &Bus,
    epoch: CanonicalUuid,
    command: &wire::AdmittedCommand,
    cancel: &wire::intent::DirectiveRef,
    effects: &mut CommandEffects,
) -> Outcome<CommandAnswer> {
    let (_, indexed) = match directive_head(bus, cancel.directive) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let directive = match live_head(indexed, cancel.directive) {
        Ok(directive) => directive,
        Err(outcome) => return outcome,
    };
    let own_invitation = matches!(command.actor(), wire::Actor::Lane(lane) if directive.body().sender == wire::Actor::Lane(*lane))
        && directive
            .body()
            .control
            .as_deref()
            .is_some_and(|text| text.parse::<wire::TaskInviteControl>().is_ok());
    if !command.holds(wire::principal::PrincipalRole::Operator) && !own_invitation {
        return refusal(
            "steer::cancel-unauthorized",
            Some("operator or task-invitation sender".to_owned()),
            Some(command.actor().to_string()),
            "cancel only your own typed task invitation; the operator may cancel any directive",
        );
    }
    let witness = Witness::Directive {
        steer: cancel.directive,
        disposition: Disposition::Live,
    };
    if command.expected() != [witness.clone()] {
        return witness_refusal(command.expected(), &witness);
    }
    let at = match current_instant() {
        Ok(at) => at,
        Err(outcome) => return outcome,
    };
    let mut proposal = JournalWrite::new(
        command.actor().clone(),
        directive.body().origin.clone(),
        at,
        Producer::CommandDoor,
    );
    if let Err(outcome) = stage_cancel(bus, &mut proposal, &directive, None, at) {
        return outcome;
    }
    let answer = DirectiveAnswer {
        directive: cancel.directive,
        recipient: directive.body().recipient.clone(),
        key: directive.body().key.clone(),
        authored: directive.body().published,
        disposition: Disposition::Canceled,
    };
    if let Err(error) = proposal.command_result(
        typed_command(command, cancel.clone()),
        epoch,
        answer.clone(),
    ) {
        return store_fault("steer::receipt-invalid", &error.to_string());
    }
    match finish(bus, proposal, answer, effects) {
        Outcome::Refusal(refusal) if refusal.reason() == LINE_CLOSED => {
            self::cancel(bus, epoch, command, cancel, effects)
        }
        outcome => outcome,
    }
}

/// Stage one guarded cancellation of a live directive: its canceled fact,
/// the unanswered end of any line it opened, and its retired head.
pub(super) fn stage_cancel(
    bus: &Bus,
    proposal: &mut JournalWrite,
    directive: &journal::JournalHead<wire::SteeringHead>,
    reason: Option<String>,
    at: Instant,
) -> Result<PendingFact, Outcome<CommandAnswer>> {
    let body = directive.body();
    proposal.guard(live_guard(body.directive, directive.position()));
    let canceled = wire::body::SteerCanceled {
        key: body.key.clone(),
        recipient: body.recipient.clone(),
        reason,
    };
    let pending = proposal
        .fact(canceled.clone(), FactCause::Existing(body.created))
        .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
    UnansweredEnds::default().stage(bus, proposal, body, pending)?;
    let next = steer::Ledger::transition_head(body, steer::Steering::Canceled(&canceled), at)
        .map_err(|error| store_fault("steer::index-invalid", &error.to_string()))?;
    proposal.steering_head(pending, move |_| next);
    Ok(pending)
}

pub(in crate::resident) fn resend(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    config: &steer::Config,
    command: &wire::AdmittedCommand,
    resend: &wire::intent::DirectiveRef,
    effects: &mut CommandEffects,
) -> Outcome<CommandAnswer> {
    if !command.holds(wire::principal::PrincipalRole::Operator) {
        return refusal(
            "steer::operator-required",
            Some("operator".to_owned()),
            Some(command.actor().to_string()),
            "submit resend through the current operator discovery",
        );
    }
    let (position, indexed) = match directive_head(bus, resend.directive) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let directive = match live_head(indexed, resend.directive) {
        Ok(directive) => directive,
        Err(outcome) => return outcome,
    };
    let witness = Witness::Directive {
        steer: resend.directive,
        disposition: Disposition::Live,
    };
    if command.expected() != [witness.clone()] {
        return witness_refusal(command.expected(), &witness);
    }
    let published = directive.body().published;
    let at = match current_instant() {
        Ok(at) => at,
        Err(outcome) => return outcome,
    };
    let due = match due_stage(published, at, config) {
        Ok(stage) => stage,
        Err(outcome) => return outcome,
    };
    let stages = missing_stages(directive.body().stage, due);
    let answer = DirectiveAnswer {
        directive: directive.body().directive,
        recipient: directive.body().recipient.clone(),
        key: directive.body().key.clone(),
        authored: published,
        disposition: Disposition::Live,
    };
    if stages.is_empty() {
        return resend_snapshot(paths, position, answer);
    }
    let mut proposal = match escalation(command, &directive, at, stages) {
        Ok(proposal) => proposal,
        Err(outcome) => return outcome,
    };
    if let Err(error) = proposal.command_result(
        typed_command(command, resend.clone()),
        epoch,
        answer.clone(),
    ) {
        return store_fault("steer::receipt-invalid", &error.to_string());
    }
    finish(bus, proposal, answer, effects)
}

fn resend_snapshot(
    paths: &RuntimePaths,
    position: Seq,
    answer: DirectiveAnswer,
) -> Outcome<CommandAnswer> {
    if let Some(lane) = answer.recipient.lane() {
        notify_inbox(paths, lane);
    }
    Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(position),
        body: CommandAnswer::Directive(answer),
    })
}

/// Stage each escalation a live directive owes at `at`, guarded on the stage
/// it was read at.
fn escalation(
    command: &wire::AdmittedCommand,
    directive: &journal::JournalHead<wire::SteeringHead>,
    at: Instant,
    stages: Vec<wire::body::SteeringStage>,
) -> Result<JournalWrite, Outcome<CommandAnswer>> {
    let origin = directive.body().origin.clone();
    let cause = directive.body().created;
    let mut proposal = JournalWrite::new(command.actor().clone(), origin, at, Producer::BusTimer);
    proposal.guard(stage_guard(
        directive.body().directive,
        directive.position(),
        directive.body().stage,
    ));
    let mut next = directive.body().clone();
    let mut last = None;
    for stage in stages {
        let escalated = wire::body::SteerEscalated {
            directive: directive.body().directive,
            key: directive.body().key.clone(),
            recipient: directive.body().recipient.clone(),
            stage,
        };
        let pending = proposal
            .fact(escalated.clone(), FactCause::Existing(cause))
            .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
        next = steer::Ledger::transition_head(&next, steer::Steering::Escalated(&escalated), at)
            .map_err(|error| store_fault("steer::index-invalid", &error.to_string()))?;
        last = Some(pending);
    }
    proposal.steering_head(last.expect("nonempty stages append a fact"), move |_| next);
    Ok(proposal)
}

fn recoverable_directive(
    bus: &Bus,
    config: &steer::Config,
    command: &wire::AdmittedCommand,
    recover: &wire::intent::DirectiveRef,
) -> Result<(journal::JournalHead<wire::SteeringHead>, Instant, Instant), Outcome<CommandAnswer>> {
    let (_, indexed) = directive_head(bus, recover.directive)?;
    let Some(directive) = indexed else {
        return Err(refusal(
            "steer::recovery-missing",
            Some("one durable lapsed or deadlettered directive".to_owned()),
            Some(recover.directive.to_string()),
            "inspect durable steering history and name its exact directive identity",
        ));
    };
    if !matches!(
        directive.body().disposition,
        Disposition::Lapsed | Disposition::DeadLettered
    ) {
        let reason = if directive.body().disposition == Disposition::Live {
            "steer::recovery-live"
        } else {
            "steer::recovery-retired"
        };
        return Err(refusal(
            reason,
            Some("lapsed or deadlettered".to_owned()),
            Some(format!("{:?}", directive.body().disposition).to_lowercase()),
            "recover only a durable directive that lapsed or was deadlettered",
        ));
    }
    if let Some(refused) = unanswerable_peer(bus, directive.body()) {
        return Err(refused);
    }
    let witness = Witness::Recoverable {
        steer: recover.directive,
    };
    if command.expected() != [witness.clone()] {
        return Err(witness_refusal(command.expected(), &witness));
    }
    let at = current_instant()?;
    let expires = expiry(at, config.expiry().get())?;
    Ok((directive, at, expires))
}

pub(in crate::resident) fn recover(
    bus: &Bus,
    epoch: CanonicalUuid,
    config: &steer::Config,
    command: &wire::AdmittedCommand,
    recover: &wire::intent::DirectiveRef,
    effects: &mut CommandEffects,
) -> Outcome<CommandAnswer> {
    if !command.holds(wire::principal::PrincipalRole::Operator) {
        return refusal(
            "steer::operator-required",
            Some("operator".to_owned()),
            Some(command.actor().to_string()),
            "submit recovery through the current operator discovery",
        );
    }
    let (directive, at, expires) = match recoverable_directive(bus, config, command, recover) {
        Ok(context) => context,
        Err(outcome) => return outcome,
    };
    let origin = directive.body().origin.clone();
    let cause = directive.body().created;
    let key: steer::Key = directive
        .body()
        .key
        .parse()
        .expect("indexed steering key is admitted");
    let recipient = directive.body().recipient.clone();
    let lapsed_at = directive.body().expires;
    let mut proposal =
        JournalWrite::new(command.actor().clone(), origin, at, Producer::CommandDoor);
    proposal.guard(recovery_guard(
        recover.directive,
        directive.position(),
        key.clone(),
        recipient.clone(),
        lapsed_at,
        directive.body().disposition,
    ));
    let recovered = wire::body::SteerRecovered {
        directive: recover.directive,
        key: key.to_string(),
        recipient: recipient.clone(),
        lapsed_at,
        expires,
    };
    let pending = match proposal.fact(recovered.clone(), FactCause::Existing(cause)) {
        Ok(pending) => pending,
        Err(error) => {
            return store_fault("steer::recovery-fact-invalid", &error.to_string());
        }
    };
    let next = match steer::Ledger::transition_head(
        directive.body(),
        steer::Steering::Recovered(&recovered),
        at,
    ) {
        Ok(next) => next,
        Err(error) => return store_fault("steer::index-invalid", &error.to_string()),
    };
    proposal.steering_head(pending, move |_| next);
    let answer = DirectiveAnswer {
        directive: recover.directive,
        recipient,
        key: key.to_string(),
        authored: directive.body().published,
        disposition: Disposition::Live,
    };
    if let Err(error) = proposal.command_result(
        typed_command(command, recover.clone()),
        epoch,
        answer.clone(),
    ) {
        return store_fault("steer::recovery-receipt-invalid", &error.to_string());
    }
    finish(bus, proposal, answer, effects)
}

/// Refuse recovering a peer directive whose lane left the residency: a peer
/// duty is bound to its logical lane, and a lane that replaces it is another
/// destination, so recovery would hand it to nobody.
fn removed_destination(
    bus: &Bus,
    directive: &wire::SteeringHead,
) -> Option<Outcome<CommandAnswer>> {
    directive.sender.lane()?;
    let view = match bus.task_snapshot() {
        Ok(view) => view,
        Err(error) => {
            return Some(store_fault(
                "steer::residency-unreadable",
                &error.to_string(),
            ));
        }
    };
    let recipient = directive.recipient.lane()?;
    let resident = view.residency().is_some_and(|residency| {
        residency
            .body()
            .lanes
            .iter()
            .any(|row| row.lane == recipient)
    });
    (!resident).then(|| {
        refusal(
            super::LANE_REMOVED,
            Some("a lane in the residency".to_owned()),
            Some(directive.recipient.to_string()),
            "a peer duty belongs to its lane alone; author a new SYN to the lane that replaced it",
        )
    })
}

/// Refuse recovering a peer directive whose destination is no longer resident.
fn unanswerable_peer(bus: &Bus, directive: &wire::SteeringHead) -> Option<Outcome<CommandAnswer>> {
    removed_destination(bus, directive)
}

pub(super) fn append_disposition(
    proposal: &mut JournalWrite,
    directive: &journal::JournalHead<wire::SteeringHead>,
    key: &steer::Key,
    disposition: Disposition,
    locus: TranscriptLocus,
    epoch: CanonicalUuid,
    at: Instant,
) -> Result<(PendingFact, wire::SteeringHead), Outcome<CommandAnswer>> {
    let cause = FactCause::Existing(directive.body().created);
    let (pending, next) = match disposition {
        Disposition::Acked => {
            let body = wire::body::SteerAcked {
                directive: directive.body().directive,
                key: key.to_string(),
                recipient: directive.body().recipient.clone(),
                evidence: wire::body::SteeringEvidence::Transcript { epoch, locus },
            };
            let pending = match proposal.fact(body.clone(), cause) {
                Ok(pending) => pending,
                Err(error) => {
                    return Err(store_fault("steer::fact-invalid", &error.to_string()));
                }
            };
            let next =
                steer::Ledger::transition_head(directive.body(), steer::Steering::Acked(&body), at);
            (pending, next)
        }
        Disposition::Declined => {
            let body = wire::body::SteerDeclined {
                directive: directive.body().directive,
                key: key.to_string(),
                recipient: directive.body().recipient.clone(),
                evidence: wire::body::SteeringEvidence::Transcript { epoch, locus },
                reason: "declined at the cited assistant transcript locus".to_owned(),
            };
            let pending = match proposal.fact(body.clone(), cause) {
                Ok(pending) => pending,
                Err(error) => {
                    return Err(store_fault("steer::fact-invalid", &error.to_string()));
                }
            };
            let next = steer::Ledger::transition_head(
                directive.body(),
                steer::Steering::Declined(&body),
                at,
            );
            (pending, next)
        }
        Disposition::Lapsed => {
            let body = wire::body::SteerLapsed {
                key: key.to_string(),
                recipient: directive.body().recipient.clone(),
                expires: directive.body().expires,
            };
            let pending = match proposal.fact(body.clone(), cause) {
                Ok(pending) => pending,
                Err(error) => {
                    return Err(store_fault("steer::fact-invalid", &error.to_string()));
                }
            };
            let next = steer::Ledger::transition_head(
                directive.body(),
                steer::Steering::Lapsed(&body),
                at,
            );
            (pending, next)
        }
        _ => unreachable!("the transcript scanner emits only terminal lane dispositions"),
    };
    next.map(|next| (pending, next))
        .map_err(|error| store_fault("steer::index-invalid", &error.to_string()))
}

pub(super) fn evidence_refusal(
    token: &str,
    observed: &str,
    repair: &str,
) -> Outcome<CommandAnswer> {
    refusal(
        "steer::transcript-evidence-absent",
        Some(format!("assistant text recognizing {token}")),
        Some(observed.to_owned()),
        repair,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_refuses_duplicate_attachment_payloads() {
        let media = wire::Media::from_base64("capture.png", "image/png", "iVBORw0KGgo=")
            .expect("fixture image admits");
        let send = wire::intent::SteerSend {
            fanout: "5f72a0b1-6273-4495-9f46-0718293a4b5c".parse().unwrap(),
            recipients: Vec::new(),
            lane: Some("5f72a0b1-6273-4495-9f46-0718293a4b5d".parse().unwrap()),
            message: media.markdown().unwrap(),
            attachments: vec![media],
        };
        assert!(matches!(
            admit_send_content(&send),
            Err(Outcome::Refusal(refusal)) if refusal.reason() == "steer::media-duplicated"
        ));
    }
}
