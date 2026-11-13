use std::num::NonZeroU32;

use super::*;
use crate::board_command::mutation::claim::prepare_join_claim;

/// Join one already-shared task, directly or through a live typed invitation.
pub(crate) fn task_join(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    intent: &TaskJoin,
    repository: Option<Host<'_>>,
    resources: Option<&resources::ResourceClaims>,
) -> Outcome<CommandAnswer> {
    match intent {
        TaskJoin::Task { task } => direct_join(bus, command, intent, task, repository, resources),
        TaskJoin::Invitation { key } => {
            invited_join(bus, command, intent, key, repository, resources, None)
        }
    }
}

/// Staging that settles the answer which caused an invitation join, written
/// after the acceptance in the joining write.
type Settle<'a> =
    dyn FnMut(&mut JournalWrite, PendingFact) -> Result<(), Outcome<CommandAnswer>> + 'a;

/// Join the task one live typed invitation offers, as its recipient's ACK.
///
/// This is the invitation join `task join KEY` runs, decided against the same
/// witnesses and recorded under the recipient's own receipt; `settle` stages
/// the ACK's own record into the joining write. Answers the inviter owed the
/// joined reply.
pub(crate) fn accept_invitation(
    bus: &Bus,
    epoch: wire::CanonicalUuid,
    head: &journal::JournalHead<wire::SteeringHead>,
    joining: Joining<'_>,
    settle: &mut Settle<'_>,
) -> Result<Option<wire::CanonicalUuid>, Outcome<CommandAnswer>> {
    let written = &head.body().key;
    let key = written
        .parse::<steer::Key>()
        .map_err(|_| invitation_malformed(written))?;
    let control = live_invitation(head, &key)?;
    let Some(recipient) = head.body().recipient.lane() else {
        return Err(invitation_expired(written));
    };
    let actor = Actor::Lane(recipient);
    let snapshot = validated_working_snapshot_for_tasks(
        bus,
        Some(joining.host.repository),
        actor.clone(),
        std::slice::from_ref(control.task()),
    )?;
    let (kind, expected, body) = invitation_join(&snapshot.answer, &key, head, &control)?;
    let release = spice::version::running()
        .map_err(Outcome::Fault)?
        .product
        .clone();
    let command = wire::Command {
        v: WireVersion::CURRENT,
        id: wire::CanonicalUuid::new(uuid::Uuid::new_v4()),
        epoch,
        kind,
        expected,
        body,
    }
    .admit(
        &wire::Accepted {
            wire: WireVersion::CURRENT,
            actor,
            epoch,
            release,
        },
        &wire::principal::PrincipalRoles::default(),
    )
    .map_err(|rejection| match rejection {
        wire::CommandRejection::Refusal(refusal) => Outcome::Refusal(refusal),
        wire::CommandRejection::Fault(fault) => Outcome::Fault(fault),
    })?;
    let wire::intent::Intent::TaskJoin(intent) = command.body() else {
        return Err(protocol_fault(
            "task::invite-accept-shape",
            "an invitation acceptance admitted another intent",
        ));
    };
    match invited_join(
        bus,
        &command,
        intent,
        written,
        Some(joining.host),
        Some(joining.resources),
        Some(settle),
    ) {
        Outcome::Reply(_) => Ok(head.body().sender.lane()),
        failed => Err(failed),
    }
}

fn direct_join(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    intent: &TaskJoin,
    task: &TaskHandle,
    repository: Option<Host<'_>>,
    resources: Option<&resources::ResourceClaims>,
) -> Outcome<CommandAnswer> {
    let snapshot =
        match command_task_snapshot(bus, repository.map(|host| host.repository), command, task) {
            Ok(snapshot) => snapshot,
            Err(outcome) => return outcome,
        };
    let Some(selected) = task_entry(&snapshot.entries, task).cloned() else {
        return refusal(
            "task::unknown",
            Some(task.to_string()),
            "read board and name one current task handle",
        );
    };
    finish_join(
        bus,
        command,
        intent,
        repository,
        resources,
        (snapshot, selected),
        None,
    )
}

fn invited_join(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    intent: &TaskJoin,
    written_key: &str,
    repository: Option<Host<'_>>,
    resources: Option<&resources::ResourceClaims>,
    settle: Option<&mut Settle<'_>>,
) -> Outcome<CommandAnswer> {
    let key = match written_key.parse::<steer::Key>() {
        Ok(key) if key.to_string() == written_key => key,
        _ => return invitation_malformed(written_key),
    };
    let lookup = key.to_string();
    let head = match bus.read(move |read| read.steering_key(&lookup)) {
        Ok(Some(head)) => head,
        Ok(None) => return invitation_expired(written_key),
        Err(error) => return store_fault("task::invite-head-unreadable", error.to_string()),
    };
    let control = match live_invitation(&head, &key) {
        Ok(control) => control,
        Err(outcome) => return outcome,
    };
    let snapshot = match validated_working_snapshot_for_tasks(
        bus,
        repository.map(|host| host.repository),
        command.actor().clone(),
        std::slice::from_ref(control.task()),
    ) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let Some(selected) = snapshot
        .entries
        .iter()
        .find(|entry| entry.born() == control.task())
        .cloned()
    else {
        return invitation_expired(written_key);
    };
    let at = match current_instant() {
        Ok(at) => at,
        Err(outcome) => return outcome,
    };
    if !invitation_is_current(command, &snapshot, &selected, &head, &control, &key, at) {
        return invitation_expired(written_key);
    }
    let asking = read_guard::AskingOffer::new(
        &snapshot,
        &selected,
        head.body()
            .sender
            .lane()
            .expect("current invitation has a lane sender"),
        control.claim(),
    );
    if let Err(outcome) = asking.admit(bus, Some(head.body().directive), at) {
        return outcome;
    }
    finish_join(
        bus,
        command,
        intent,
        repository,
        resources,
        (snapshot, selected),
        Some((head, asking, settle)),
    )
}

fn invitation_is_current(
    command: &wire::AdmittedCommand,
    snapshot: &ValidatedSnapshot,
    selected: &Entry,
    head: &journal::JournalHead<wire::SteeringHead>,
    control: &wire::TaskInviteControl,
    key: &steer::Key,
    at: Instant,
) -> bool {
    let Actor::Lane(recipient) = command.actor() else {
        return false;
    };
    let head = head.body();
    let Some(inviter) = head.sender.lane() else {
        return false;
    };
    let inviter_claim = selected
        .active_claims()
        .find(|held| {
            held.claimed() == control.claim()
                && held.claim().lane == inviter
                && held.claim().actor == Actor::Lane(inviter)
        })
        .map(|held| held.claimed());
    head.recipient == Actor::Lane(*recipient)
        && head.key == key.as_str()
        && invitation_current(
            head,
            control,
            (&selected.workflow().phase, selected.workflow().generation),
            snapshot
                .answer
                .addresses
                .get(&inviter)
                .and_then(wire::SeatAddress::address),
            inviter_claim,
            at,
        )
}

fn finish_join(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    intent: &TaskJoin,
    repository: Option<Host<'_>>,
    resources: Option<&resources::ResourceClaims>,
    (snapshot, selected): (ValidatedSnapshot, Entry),
    invitation: Option<(
        journal::JournalHead<wire::SteeringHead>,
        read_guard::AskingOffer,
        Option<&mut Settle<'_>>,
    )>,
) -> Outcome<CommandAnswer> {
    let (invitation, settle) = invitation.map_or((None, None), |(head, asking, settle)| {
        (Some((head, asking)), settle)
    });
    let invited = invitation.is_some();
    let target = if invited {
        match invitation_target(&selected) {
            Ok(target) => target,
            Err(outcome) => return outcome,
        }
    } else {
        None
    };
    let (prepared, reservation) = match prepare_join_claim(
        bus, command, repository, &snapshot, &selected, !invited, resources,
    ) {
        Ok(prepared) => prepared,
        Err(outcome) => return outcome,
    };
    let mut completion = selected.completion().clone();
    if let Some(target) = target {
        completion.set_target(selected.workflow().phase, target);
    }
    let post = if target.is_some() {
        selected.clone().with_completion(completion.clone())
    } else {
        selected.clone()
    };
    let origin_context = match task_steering_origin_output(bus, &snapshot.facts, &selected) {
        Ok(context) => context,
        Err(outcome) => return prepared.rollback(outcome),
    };
    let room_context = match claim_room(bus, repository, &selected) {
        Ok(context) => context,
        Err(outcome) => return prepared.rollback(outcome),
    };
    let answer = prepared.answer(command, &snapshot, &post, origin_context, room_context);
    let origin = match task_origin(selected.born()) {
        Ok(origin) => origin,
        Err(outcome) => return prepared.rollback(outcome),
    };
    let mut write = JournalWrite::new(
        command.actor().clone(),
        origin.clone(),
        prepared.at(),
        Producer::CommandDoor,
    );
    prepared.guard(&mut write, &snapshot, command);
    if let Some((head, asking)) = invitation.as_ref() {
        asking.guard(&mut write, Some(head.body().directive), prepared.at());
    }
    let growth = target.map(|target| (completion, target));
    let accepted = match invitation
        .as_ref()
        .map(|(head, _)| stage_invitation(&mut write, head, settle, &selected, growth))
        .transpose()
    {
        Ok(accepted) => accepted,
        Err(outcome) => return prepared.rollback(outcome),
    };
    let prepared_git = match prepared.append_with_cause(&mut write, command, &post, accepted) {
        Ok(prepared) => prepared,
        Err(outcome) => return outcome,
    };
    if let (Some((head, _)), Some(accepted)) = (invitation.as_ref(), accepted)
        && let Err(outcome) = crate::resident::append_peer_reply(
            bus,
            &mut write,
            command.epoch(),
            crate::resident::PeerReply::invitation(
                head,
                origin,
                accepted,
                wire::Disposition::Acked,
            ),
            None,
        )
    {
        return rollback_prepared(prepared_git, outcome);
    }
    if let Err(error) = write.command_result(
        request(command, intent.clone()),
        command.epoch(),
        answer.clone(),
    ) {
        return rollback_prepared(
            prepared_git,
            store_fault("task::join-receipt", error.to_string()),
        );
    }
    let outcome = match prepared_git {
        Some(prepared) => committed_with_task_git(
            bus,
            write,
            CommandAnswer::Claim(Box::new(answer)),
            PreparedTaskGit::Claim(prepared),
        ),
        None => committed(bus, write, CommandAnswer::Claim(Box::new(answer))),
    };
    if matches!(outcome, Outcome::Reply(_))
        && let Some(reservation) = reservation
        && let Err(error) = reservation.commit()
    {
        return error;
    }
    outcome
}

/// Stage one invitation's acceptance, the caller's settlement of it, and the
/// target growth it owns ahead of the claim, answering the acceptance.
fn stage_invitation(
    write: &mut JournalWrite,
    head: &journal::JournalHead<wire::SteeringHead>,
    settle: Option<&mut Settle<'_>>,
    selected: &Entry,
    growth: Option<(wire::CompletionHead, NonZeroU32)>,
) -> Result<PendingFact, Outcome<CommandAnswer>> {
    let accepted = append_acceptance(write, head)?;
    if let Some(settle) = settle {
        settle(write, accepted)?;
    }
    if let Some((completion, target)) = growth {
        append_target(
            write,
            selected,
            completion,
            target,
            FactCause::Pending(accepted),
        )?;
    }
    Ok(accepted)
}

pub(super) fn invitation_target(
    selected: &Entry,
) -> Result<Option<NonZeroU32>, Outcome<CommandAnswer>> {
    if selected.has_capacity() {
        return Ok(None);
    }
    let occupied = selected.completion().arrivals.len()
        + selected
            .active_claims()
            .filter(|held| !selected.has_arrived(&held.claim().actor))
            .count();
    let target = u32::try_from(occupied)
        .ok()
        .and_then(|occupied| occupied.checked_add(1))
        .and_then(NonZeroU32::new)
        .ok_or_else(|| {
            refusal(
                "completion::target-overflow",
                Some(selected.handle().to_string()),
                "the current phase cannot admit another completion participant",
            )
        })?;
    Ok(Some(target))
}

fn append_acceptance(
    write: &mut JournalWrite,
    head: &journal::JournalHead<wire::SteeringHead>,
) -> Result<PendingFact, Outcome<CommandAnswer>> {
    let body = wire::body::SteerAccepted {
        directive: head.body().directive,
        key: head.body().key.clone(),
        lane: head
            .body()
            .recipient
            .lane()
            .expect("accepted invitations address a lane"),
    };
    let directive = body.directive;
    let position = head.position();
    write.guard(JournalGuard::new(move |read| {
        if read
            .steering_head(directive)?
            .is_some_and(|current| current.position() == position)
        {
            Ok(())
        } else {
            Err(GuardError::refused("task::invite-moved"))
        }
    }));
    let pending = write
        .fact(body.clone(), FactCause::Root)
        .map_err(|error| store_fault("task::invite-accept-build", error.to_string()))?;
    let next =
        steer::Ledger::transition_head(head.body(), steer::Steering::Accepted(&body), write.at())
            .map_err(|error| store_fault("task::invite-head-invalid", error.to_string()))?;
    write.steering_head(pending, move |_| next);
    Ok(pending)
}

pub(super) fn append_target(
    write: &mut JournalWrite,
    selected: &Entry,
    completion: wire::CompletionHead,
    target: NonZeroU32,
    cause: FactCause,
) -> Result<PendingFact, Outcome<CommandAnswer>> {
    let targeted = write
        .fact(
            body::CompletionTargeted {
                task: selected.born().clone(),
                phase: selected.workflow().phase,
                target,
            },
            cause,
        )
        .map_err(|error| store_fault("task::join-target-build", error.to_string()))?;
    write.completion_head(selected.born().clone(), targeted, move |_| completion);
    Ok(targeted)
}

fn rollback_prepared(
    prepared: Option<spice::task_git::ClaimPreparation<'_>>,
    outcome: Outcome<CommandAnswer>,
) -> Outcome<CommandAnswer> {
    match prepared {
        Some(prepared) => prepared
            .rollback()
            .map_or_else(task_git_outcome, |()| outcome),
        None => outcome,
    }
}

fn invitation_malformed(key: &str) -> Outcome<CommandAnswer> {
    refusal(
        "task::invite-malformed",
        Some(key.to_owned()),
        "name one canonical invitation key",
    )
}

fn invitation_expired(key: &str) -> Outcome<CommandAnswer> {
    refusal(
        INVITE_EXPIRED,
        Some(key.to_owned()),
        INVITE_EXPIRED_GUIDANCE,
    )
}
