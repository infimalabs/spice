//! Occupied-lane invitations as task reads project them.
//!
//! Steering owns routing, lifetime and disposition. The original control
//! context locates current journal authority; that authority reconstructs the
//! complete expected binding and keeps original context available to task reads.

use super::*;

/// Whether a live invitation can still be accepted: its directive has not
/// lapsed, the inviter still holds the exact claim it bound in the exact
/// phase and generation, and the inviter's seat still proves the tag.
pub(in crate::board_command) fn invitation_current(
    head: &wire::SteeringHead,
    control: &wire::TaskInviteControl,
    (phase, generation): (&wire::Phase, u64),
    inviter_address: Option<wire::OccupantAddress>,
    inviter_claim: Option<Seq>,
    at: Instant,
) -> bool {
    let (Some(inviter_lane), Some(recipient_lane)) = (head.sender.lane(), head.recipient.lane())
    else {
        return false;
    };
    control.is_current(wire::TaskInviteStanding {
        key: &head.key,
        room: head.room.as_ref(),
        expires: head.expires,
        at,
        inviter_lane: Some(inviter_lane),
        inviter_occupant: inviter_address,
        recipient_lane,
        phase,
        generation,
        inviter_claim,
    })
}

/// Fill `task.invitations` from every invitation published into its room.
pub(super) fn attach_invitations(
    bus: &Bus,
    task: &mut BoardTask,
    addresses: &std::collections::BTreeMap<wire::CanonicalUuid, wire::SeatAddress>,
    at: Instant,
) -> Result<(), Outcome<CommandAnswer>> {
    let room = task.statement.body.born.clone();
    let heads = bus
        .read(move |read| read.room_invitations(&room))
        .map_err(|error| store_fault("task::invitations-unreadable", error.to_string()))?;
    task.invitations = heads
        .iter()
        .filter_map(|head| {
            let head = head.body();
            let control = head
                .control
                .as_deref()?
                .parse::<wire::TaskInviteControl>()
                .ok()?;
            let inviter = head.sender.lane()?;
            let claim = task
                .active_claims()
                .find(|held| {
                    held.claim.body.claimed == control.claim()
                        && held.claim.body.lane == inviter
                        && held.claim.body.actor == wire::Actor::Lane(inviter)
                })
                .map(|held| held.claim.body.claimed);
            let current = invitation_current(
                head,
                &control,
                (&task.workflow.body.phase, task.workflow.body.generation),
                addresses.get(&inviter).and_then(wire::SeatAddress::address),
                claim,
                at,
            );
            Some(wire::TaskInvitation {
                key: head.key.clone(),
                inviter,
                inviter_claim: control.claim(),
                recipient: head.recipient.lane()?,
                phase: *control.phase(),
                generation: control.generation(),
                published: head.published,
                expires: head.expires,
                state: wire::InvitationState::derive(head.disposition, current),
            })
        })
        .collect();
    Ok(())
}

/// Fill the rows a named-task read carries beyond the open board: each named
/// task's invitations and, with `history`, the record of its named tasks.
pub(super) fn attach_named_rows(
    bus: &Bus,
    observed: &bus::TaskSnapshot,
    answer: &mut BoardAnswer,
    tasks: &[wire::TaskStamp],
    history: bool,
) -> Result<(), Outcome<CommandAnswer>> {
    let at = current_instant()?;
    // Only named rows the snapshot admitted, so a hidden task's notes stay hidden.
    let mut named = std::collections::BTreeSet::new();
    for task in &mut answer.tasks {
        if !tasks.contains(&task.statement.body.born) {
            continue;
        }
        named.insert(task.statement.body.born.clone());
        attach_invitations(bus, task, &answer.addresses, at)?;
    }
    if history {
        let record = task_detail::task_record(observed.facts(), &named);
        answer.acknowledgments = record.acknowledgments;
        answer.closures = record.closures;
        answer.steps = record.steps;
    }
    Ok(())
}
