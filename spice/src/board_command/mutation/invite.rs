//! Holder invitations: one eligible partner chosen and reached in one write.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use wire::intent::{TaskInvite, TaskInviteAnswer, TaskInviteDelivery};

use super::join::{append_target, invitation_target};
use super::*;
use crate::board_command::automatic_lifecycle::{AutomaticSeat, claim_deadlines, stage_automatic};
use crate::resident::InvitationMessage;

/// Invite one partner into the task phase the caller holds.
///
/// A vacant recipient receives an ordinary claim that fills any target growth
/// in the same guarded write; the resident lifecycle launches it like any
/// held claim. An occupied recipient receives one durable typed steering
/// invitation, and its staffing changes only when it accepts. Without an
/// address the command recruits only from the inviter's own team and follows
/// allocation there: launchable vacant teammates are tried in wake rank until
/// one claim commits, and a running teammate is asked only as the last resort
/// when none did. An address may name a lane on any team. Exactly one lane is
/// bound or asked. Answers the lane whose inbox the committed write addressed.
///
/// `message` is the requester's own words as its tape proved them. The write
/// that reaches the recipient admits them to the outbox for that lane, so a
/// refused invitation admits nothing and a committed one names the
/// submission its caller settles.
pub(crate) fn task_invite(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    (intent, message): (&TaskInvite, Option<&InvitationMessage>),
    joining: Joining<'_>,
    profiles: OccupantProfiles<'_>,
) -> (Outcome<CommandAnswer>, Option<wire::CanonicalUuid>) {
    match invite(bus, command, (intent, message), joining, profiles) {
        Ok(delivered) => delivered,
        Err(outcome) => (outcome, None),
    }
}

type Delivered = (Outcome<CommandAnswer>, Option<wire::CanonicalUuid>);

fn invite(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    (intent, message): (&TaskInvite, Option<&InvitationMessage>),
    joining: Joining<'_>,
    profiles: OccupantProfiles<'_>,
) -> Result<Delivered, Outcome<CommandAnswer>> {
    let snapshot =
        command_task_snapshot(bus, Some(joining.host.repository), command, &intent.task)?;
    let selected = task_entry(&snapshot.entries, &intent.task)
        .cloned()
        .ok_or_else(|| {
            refusal(
                "task::unknown",
                Some(intent.task.to_string()),
                "read board and name one current task handle",
            )
        })?;
    let unheld = || {
        refusal(
            "task::invite-unheld",
            Some(selected.handle().to_string()),
            "only a current holder of the task's phase invites a partner",
        )
    };
    let Actor::Lane(inviter) = *command.actor() else {
        return Err(unheld());
    };
    let Some(held) = selected.claim_of(command.actor()) else {
        return Err(unheld());
    };
    let inviter_claim = held.claimed();
    let offer = read_guard::AskingOffer::new(&snapshot, &selected, inviter, inviter_claim);
    offer.admit(bus, None, current_instant()?)?;
    if let Some(message) = message {
        refuse_moved_author(&snapshot, inviter, message)?;
    }
    let satisfied = board::resolved_tasks(&snapshot.entries);
    refuse_held_design(&selected, &satisfied)?;
    // The recipient reserves this task's shards, which its participants may
    // already hold, so free pool capacity is no second requirement.
    let mut environment = snapshot.answer.environment();
    for (name, count) in selected.statement().resources.iter() {
        environment = environment.with_resource_pool(name.clone(), count);
    }
    let policy = if intent.recipient.is_none() {
        let team = actor_subscription(&snapshot.answer.residency.body, command.actor())?;
        if !snapshot.owners.for_team(team.team).admits(&selected) {
            return Err(refusal(
                "task::invite-policy",
                Some(wire::topic_label(selected.topic()).to_owned()),
                "this team does not drain this task; address a partner explicitly",
            ));
        }
        Some(std::sync::Arc::clone(&snapshot.policy))
    } else {
        None
    };
    let candidates = Candidates {
        snapshot: &snapshot,
        selected: &selected,
        satisfied: &satisfied,
        environment: &environment,
    };
    let rows = match intent.recipient {
        Some(address) => vec![candidates.addressed(bus, address)?],
        None => candidates.ranked(inviter, profiles)?,
    };
    let mut last = None;
    for row in rows {
        let reached = match row.occupant {
            None => claim_vacant(
                bus,
                command,
                (intent, message),
                joining,
                (&snapshot, &selected),
                row,
                policy.clone(),
            ),
            Some(occupant) => steer_occupied(
                bus,
                command,
                (intent, message),
                (&snapshot, &selected),
                (inviter, inviter_claim),
                (row.lane, occupant),
                policy.clone(),
            ),
        };
        let outcome = match reached {
            Ok(outcome) | Err(outcome) => outcome,
        };
        // A committed claim or the one typed invitation ends the command.
        if matches!(outcome, Outcome::Reply(_)) {
            return Ok((outcome, Some(row.lane)));
        }
        // Another vacant lane may survive a candidate-local refusal or Git
        // preparation fault. Store, protocol, clock, and resource faults are
        // shared authority failures and end the command rather than hiding
        // behind a later invitation.
        if row.occupant.is_none() && retry_vacant_failure(&outcome) {
            last = Some(outcome);
            continue;
        }
        return Ok((outcome, None));
    }
    Ok((last.expect("the candidate list is never empty"), None))
}

/// Every invitation write guards the residency head it read, so the message's
/// author is still the seated inviter when its admission commits.
fn refuse_moved_author(
    snapshot: &ValidatedSnapshot,
    inviter: wire::CanonicalUuid,
    message: &InvitationMessage,
) -> Result<(), Outcome<CommandAnswer>> {
    let seated = snapshot
        .answer
        .residency
        .body
        .lanes
        .iter()
        .any(|row| row.lane == inviter && row.occupant == Some(message.occupant()));
    if seated {
        return Ok(());
    }
    Err(refusal(
        "task::invite-message-author",
        Some(inviter.to_string()),
        "write the invitation again from the lane's current occupant",
    ))
}

fn retry_vacant_failure(outcome: &Outcome<CommandAnswer>) -> bool {
    match outcome {
        Outcome::Refusal(_) => true,
        Outcome::Fault(fault) => {
            fault.class == wire::FaultClass::Environment && fault.reason.starts_with("git::")
        }
        Outcome::Reply(_) => false,
    }
}

/// Admit the requester's proved message for `recipient` after every fact the
/// write already stages, and name its submission.
fn stage_message(
    write: &mut JournalWrite,
    message: Option<&InvitationMessage>,
    recipient: wire::CanonicalUuid,
    selected: &Entry,
) -> Result<Option<wire::CanonicalUuid>, Outcome<CommandAnswer>> {
    let Some(message) = message else {
        return Ok(None);
    };
    write
        .fact(
            message.accepted(recipient, selected.handle()),
            FactCause::Root,
        )
        .map_err(|error| store_fault("task::invite-message-build", error.to_string()))?;
    Ok(Some(message.submission()))
}

/// Why one lane cannot receive an invitation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Blocked {
    Participant,
    Busy,
    Unlaunchable,
    Ineligible,
}

impl Blocked {
    const fn reason(self) -> &'static str {
        match self {
            Self::Participant => "task::invite-participant",
            Self::Busy => "task::invite-busy",
            Self::Unlaunchable => "task::invite-unlaunchable",
            Self::Ineligible => "task::invite-ineligible",
        }
    }

    /// How many lanes this reason blocked, as an inviter reads it.
    fn blocked(self, lanes: usize) -> String {
        let (one, many) = match self {
            Self::Participant => (
                "lane already participates in this phase",
                "lanes already participate in this phase",
            ),
            Self::Busy => ("lane holds another claim", "lanes hold other claims"),
            Self::Unlaunchable => ("vacant lane cannot launch", "vacant lanes cannot launch"),
            Self::Ineligible => (
                "lane is ineligible for this task",
                "lanes are ineligible for this task",
            ),
        };
        format!("{lanes} {}", if lanes == 1 { one } else { many })
    }
}

/// The lanes one invitation may reach, judged against one snapshot.
struct Candidates<'a> {
    snapshot: &'a ValidatedSnapshot,
    selected: &'a Entry,
    satisfied: &'a board::ResolvedTasks,
    environment: &'a wire::Environment,
}

impl<'a> Candidates<'a> {
    /// Why `row` cannot receive this invitation, if anything stops it.
    fn refusal(&self, row: &wire::LaneResidency) -> Option<Blocked> {
        let actor = Actor::Lane(row.lane);
        if self.selected.participates(&actor) {
            return Some(Blocked::Participant);
        }
        if self
            .snapshot
            .entries
            .iter()
            .any(|entry| entry.claim_of(&actor).is_some())
        {
            return Some(Blocked::Busy);
        }
        if row.occupant.is_none()
            && (row.disposition == "occupied" || self.snapshot.facts.lane_held(row.lane))
        {
            return Some(Blocked::Unlaunchable);
        }
        if !board::joinable_entry(self.satisfied, self.selected, &actor, self.environment) {
            return Some(Blocked::Ineligible);
        }
        None
    }

    /// Why no teammate could take an unaddressed invitation: each reason that
    /// blocked a teammate and how many teammates it blocked.
    fn exhausted(&self, inviter: wire::CanonicalUuid) -> String {
        let mut blocked = BTreeMap::<Blocked, usize>::new();
        for row in self.teammates(inviter) {
            if let Some(reason) = self.refusal(row) {
                *blocked.entry(reason).or_default() += 1;
            }
        }
        if blocked.is_empty() {
            return "no other lane is on your team".to_owned();
        }
        if blocked.keys().eq([&Blocked::Participant]) {
            return "every teammate already participates in this phase".to_owned();
        }
        blocked
            .into_iter()
            .map(|(reason, lanes)| reason.blocked(lanes))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// The lanes an unaddressed invitation may reach: the inviter's own team,
    /// never the inviter. An explicit address is not bounded by team.
    fn teammates(
        &self,
        inviter: wire::CanonicalUuid,
    ) -> impl Iterator<Item = &'a wire::LaneResidency> {
        let lanes = &self.snapshot.answer.residency.body.lanes;
        let team = lanes
            .iter()
            .find(|row| row.lane == inviter)
            .map(|row| row.team);
        lanes
            .iter()
            .filter(move |row| row.lane != inviter && Some(row.team) == team)
    }

    /// The lane lineage an exact occupant address names, whichever occupant
    /// now sits there.
    fn addressed(
        &self,
        bus: &Bus,
        address: wire::OccupantAddress,
    ) -> Result<&'a wire::LaneResidency, Outcome<CommandAnswer>> {
        let birth = bus
            .occupant_birth(address)
            .map_err(|error| store_fault("task::invite-address-unreadable", error.to_string()))?
            .map_err(|refused| {
                refusal(
                    refused.reason(),
                    Some(address.to_string()),
                    "name one occupant address from spice lane list",
                )
            })?;
        let row = self
            .snapshot
            .answer
            .residency
            .body
            .lanes
            .iter()
            .find(|row| row.lane == birth.lane)
            .ok_or_else(|| {
                refusal(
                    "task::invite-lane-missing",
                    Some(address.to_string()),
                    "invite a lane the current residency still names",
                )
            })?;
        match self.refusal(row) {
            Some(blocked) => Err(refusal(
                blocked.reason(),
                Some(address.to_string()),
                "invite an eligible lane that is not already working",
            )),
            None => Ok(row),
        }
    }

    /// Every eligible teammate in the order the invitation tries them:
    /// launchable vacant lanes before running ones, as allocation considers
    /// only vacant lanes; within each, the higher retained match, the newest
    /// durable claim, and lane id, as the lifecycle ranks wakes.
    fn ranked(
        &self,
        inviter: wire::CanonicalUuid,
        profiles: OccupantProfiles<'_>,
    ) -> Result<Vec<&'a wire::LaneResidency>, Outcome<CommandAnswer>> {
        let residency = &self.snapshot.answer.residency.body;
        let matcher = RetainedMatch::open(profiles, residency, &self.snapshot.entries)?;
        let rank = |row: &wire::LaneResidency| Rank {
            running: row.occupant.is_some(),
            matched: matcher
                .as_ref()
                .and_then(|matcher| matcher.score(row.lane, self.selected)),
            last_claim: self
                .snapshot
                .facts
                .latest_claim(&Actor::Lane(row.lane))
                .map(|(claim, _)| claim),
            lane: row.lane,
        };
        let mut ranked = self
            .teammates(inviter)
            .filter(|row| self.refusal(row).is_none())
            .map(|row| (rank(row), row))
            .collect::<Vec<_>>();
        if ranked.is_empty() {
            return Err(refusal(
                "task::invite-none",
                Some(self.selected.handle().to_string()),
                self.exhausted(inviter),
            ));
        }
        ranked.sort_by(|(left, _), (right, _)| left.order(right));
        Ok(ranked.into_iter().map(|(_, row)| row).collect())
    }
}

/// One candidate's place in the invitation order.
struct Rank {
    running: bool,
    matched: Option<f64>,
    last_claim: Option<Seq>,
    lane: wire::CanonicalUuid,
}

impl Rank {
    fn order(&self, other: &Self) -> Ordering {
        // Cosines are never negative, so an unscored lane ranks below all.
        let score = |rank: &Self| rank.matched.unwrap_or(f64::NEG_INFINITY);
        self.running
            .cmp(&other.running)
            .then_with(|| score(other).total_cmp(&score(self)))
            // Reversed, `None` sorts last: a lane that never claimed is the coldest.
            .then_with(|| other.last_claim.cmp(&self.last_claim))
            .then_with(|| self.lane.cmp(&other.lane))
    }
}

/// Mint the vacant recipient's claim, and any target growth it fills, in one
/// guarded write that also names the occupant the lifecycle will seat and
/// admits the requester's message for that lane.
fn claim_vacant(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    (intent, message): (&TaskInvite, Option<&InvitationMessage>),
    joining: Joining<'_>,
    (snapshot, selected): (&ValidatedSnapshot, &Entry),
    row: &wire::LaneResidency,
    policy: Option<std::sync::Arc<drive::PolicyCapture>>,
) -> Result<Outcome<CommandAnswer>, Outcome<CommandAnswer>> {
    let recipient = Actor::Lane(row.lane);
    let target = invitation_target(selected)?;
    let reservation = joining
        .resources
        .reserve(selected, &recipient, &snapshot.entries)?;
    let (at, expiry, maximum) = claim_deadlines()?;
    let mut write = JournalWrite::new(
        command.actor().clone(),
        task_origin(selected.born())?,
        at,
        Producer::CommandDoor,
    );
    if let Some(policy) = policy {
        write.guard(JournalGuard::new(move |_| policy.require_current()));
    }
    guard_allocation(
        &mut write,
        command.expected().to_vec(),
        AllocationObservation {
            cursor: snapshot.cursor,
            actor: recipient,
            residency_position: snapshot.answer.residency.position,
        },
    );
    let Actor::Lane(inviter) = *command.actor() else {
        unreachable!("invitation admission requires a lane")
    };
    let asking = selected.claim_of(command.actor()).expect("admitted holder");
    read_guard::AskingOffer::new(snapshot, selected, inviter, asking.claimed())
        .guard(&mut write, None, at);
    let grown = target
        .map(|target| {
            let mut completion = selected.completion().clone();
            completion.set_target(selected.workflow().phase, target);
            append_target(&mut write, selected, completion, target, FactCause::Root)
        })
        .transpose()?;
    let occupant = wire::CanonicalUuid::new(uuid::Uuid::new_v4());
    let git = stage_automatic(
        &mut write,
        bus,
        joining.host,
        snapshot,
        (
            row,
            AutomaticSeat {
                epoch: command.epoch(),
                lane: row.lane,
                occupant,
            },
        ),
        selected,
        (expiry, maximum),
        grown,
    )?;
    let message = match stage_message(&mut write, message, row.lane, selected) {
        Ok(message) => message,
        Err(outcome) => return Err(git.rollback().map_or_else(task_git_outcome, |()| outcome)),
    };
    let answer = CommandAnswer::TaskInvite(Box::new(TaskInviteAnswer {
        task: selected.handle().clone(),
        lane: row.lane,
        delivery: TaskInviteDelivery::Joined {
            occupant,
            grown: target,
        },
        message,
    }));
    if let Err(error) = write.command_result(
        request(command, intent.clone()),
        command.epoch(),
        answer.clone(),
    ) {
        let fault = store_fault("task::invite-receipt", error.to_string());
        return Err(git.rollback().map_or_else(task_git_outcome, |()| fault));
    }
    let outcome = committed_with_task_git(bus, write, answer, PreparedTaskGit::Claim(git));
    if matches!(outcome, Outcome::Reply(_)) {
        reservation.commit()?;
    }
    Ok(outcome)
}

/// Publish one typed steering invitation to an occupied recipient and admit
/// the requester's message for that lane; nothing about the task's staffing
/// moves until it answers.
fn steer_occupied(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    (intent, message): (&TaskInvite, Option<&InvitationMessage>),
    (snapshot, selected): (&ValidatedSnapshot, &Entry),
    (inviter, inviter_claim): (wire::CanonicalUuid, Seq),
    (lane, occupant): (wire::CanonicalUuid, wire::CanonicalUuid),
    policy: Option<std::sync::Arc<drive::PolicyCapture>>,
) -> Result<Outcome<CommandAnswer>, Outcome<CommandAnswer>> {
    let Some(wire::SeatAddress::Seated {
        address: inviter_address,
        ..
    }) = snapshot.answer.addresses.get(&inviter)
    else {
        return Err(refusal(
            "task::invite-unseated",
            Some(inviter.to_string()),
            "invite from a seated lane whose occupant address is proved",
        ));
    };
    let at = current_instant()?;
    let origin = task_origin(selected.born())?;
    let mut write = JournalWrite::new(
        command.actor().clone(),
        origin.clone(),
        at,
        Producer::CommandDoor,
    );
    if let Some(policy) = policy {
        write.guard(JournalGuard::new(move |_| policy.require_current()));
    }
    guard_allocation(
        &mut write,
        command.expected().to_vec(),
        AllocationObservation {
            cursor: snapshot.cursor,
            actor: Actor::Lane(lane),
            residency_position: snapshot.answer.residency.position,
        },
    );
    read_guard::AskingOffer::new(snapshot, selected, inviter, inviter_claim)
        .guard(&mut write, None, at);
    let workflow = selected.workflow();
    let text = format!(
        "Join {} in {}: {}",
        selected.handle(),
        workflow.phase,
        selected.statement().title
    );
    let (key, expires, directive) = crate::resident::append_task_invitation(
        bus,
        origin,
        &mut write,
        crate::resident::TaskInvitation {
            task: selected.born(),
            phase: &workflow.phase,
            generation: workflow.generation,
            inviter,
            inviter_address: *inviter_address,
            inviter_claim,
            recipient: lane,
            recipient_occupant: occupant,
            residency: snapshot.answer.residency.position,
            epoch: snapshot.answer.residency.body.epoch,
            text,
        },
        at,
    )?;
    let message = stage_message(&mut write, message, lane, selected)?;
    let answer = CommandAnswer::TaskInvite(Box::new(TaskInviteAnswer {
        task: selected.handle().clone(),
        lane,
        delivery: TaskInviteDelivery::Invited {
            key,
            expires,
            directive,
        },
        message,
    }));
    write
        .command_result(
            request(command, intent.clone()),
            command.epoch(),
            answer.clone(),
        )
        .map_err(|error| store_fault("task::invite-receipt", error.to_string()))?;
    crate::resident::steering_envelope_admits(&write)?;
    Ok(committed(bus, write, answer))
}

#[cfg(test)]
mod tests;
