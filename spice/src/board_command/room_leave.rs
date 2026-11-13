//! A lane's leave from one room's derived background.

use super::*;

use spice::rooms::AttentionReason;
use wire::intent::{RoomLeave, RoomLeaveAnswer, RoomListeningDuty};
use wire::rooms::RoomLeft;

use super::room::mint_publication;

/// Record that the commanding lane declines the Design request it owes in
/// `leave.room`, and name every native duty that still keeps it listening. A
/// lane owing no request there has nothing to decline. Leaving frees no claim,
/// settles no delivery, and answers no question.
pub(crate) fn room_leave(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    leave: &RoomLeave,
    repository: Option<&Repository>,
) -> Outcome<CommandAnswer> {
    let Actor::Lane(lane) = command.actor().clone() else {
        return refusal(
            "room::leave-lane-only",
            None,
            "only a lane listens to a room; the operator has nothing to leave",
        );
    };
    let clause = leave.clause.trim();
    if clause.is_empty() {
        return refusal(
            "room::leave-clause-empty",
            Some(leave.room.to_string()),
            "say why the lane leaves with --clause TEXT",
        );
    }
    let snapshot = match validated_working_snapshot(
        bus,
        repository,
        command.actor().clone(),
        std::slice::from_ref(&leave.room),
    ) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let Some(anchor) = task_entry(&snapshot.entries, &leave.room) else {
        return refusal(
            "room::anchor-unknown",
            Some(leave.room.to_string()),
            "read the board and name one current task handle",
        );
    };
    let room = anchor.born().clone();
    let now = jiff::Timestamp::now();
    let at = match Instant::new(now) {
        Ok(at) => at,
        Err(error) => return environment_fault("room::clock-invalid", error.to_string()),
    };
    let probe = (room.clone(), Actor::Lane(lane));
    let latest = match bus.read(move |read| read.latest_room_publication(&probe.0, &probe.1)) {
        Ok(latest) => latest,
        Err(error) => return store_fault("room::publications-unreadable", error.to_string()),
    };
    let attention = match project_attention(
        bus,
        &snapshot.entries,
        &snapshot.answer.residency.body,
        &room,
        latest.as_slice(),
    ) {
        Ok(attention) => attention,
        Err(outcome) => return outcome,
    };
    let kept = attention
        .lanes
        .iter()
        .find(|row| row.lane == lane)
        .map(|row| kept_duties(&row.attention, &snapshot.entries))
        .unwrap_or_default();
    match owes_request(bus, &snapshot.entries, lane, &room) {
        Ok(true) => {}
        Ok(false) => {
            return refusal(
                "room::leave-nothing-owed",
                Some(nothing_owed(anchor.handle(), &kept)),
                "a leave only declines the Design request this lane owes; a contribution \
                 already answered any, and native duties end only with their work",
            );
        }
        Err(outcome) => return outcome,
    }
    let publication = match mint_publication(bus, &room, now.as_microsecond()) {
        Ok(publication) => publication,
        Err(outcome) => return outcome,
    };
    let origin = match task_origin(&room) {
        Ok(origin) => origin,
        Err(outcome) => return outcome,
    };
    let answer = RoomLeaveAnswer {
        room: anchor.handle().clone(),
        publication: publication.clone(),
        kept,
    };
    let mut write = JournalWrite::new(Actor::Lane(lane), origin, at, Producer::CommandDoor);
    guard(&mut write, command.expected().to_vec());
    if let Err(error) = write.fact(
        RoomLeft {
            room,
            publication,
            lane,
            clause: clause.to_owned(),
        },
        FactCause::Root,
    ) {
        return store_fault("room::leave-build", error.to_string());
    }
    if let Err(error) = write.command_result(
        request(command, leave.clone()),
        command.epoch(),
        answer.clone(),
    ) {
        return store_fault("room::leave-receipt", error.to_string());
    }
    committed(bus, write, CommandAnswer::RoomLeave(answer))
}

/// Whether the lane owes the Design request in `room`, by the one owed-ness
/// rule `task next` reads.
fn owes_request(
    bus: &Bus,
    entries: &[Entry],
    lane: wire::CanonicalUuid,
    room: &TaskStamp,
) -> Result<bool, Outcome<CommandAnswer>> {
    let observed = bus
        .task_snapshot()
        .map_err(|error| store_fault("room::leave-heads-unreadable", error.to_string()))?;
    Ok(
        crate::resident::design_awareness::owed_request(bus, &observed, entries, lane)?
            .is_some_and(|owed| owed.design == *room),
    )
}

/// Why `room` refuses a leave: no Design request is owed there, and what, if
/// anything, still keeps the lane listening.
fn nothing_owed(room: &TaskHandle, kept: &[RoomListeningDuty]) -> String {
    if kept.is_empty() {
        return format!("{room} owes this lane no Design request and it does not listen there");
    }
    let duties = kept
        .iter()
        .map(|duty| match duty {
            RoomListeningDuty::Work { task } => format!("work {task}"),
            RoomListeningDuty::Stop { key } => format!("stop {key}"),
        })
        .collect::<Vec<_>>();
    format!(
        "{room} owes this lane no Design request; it listens for {}",
        duties.join(", ")
    )
}

/// The native duties among `reasons`, each named as the lane acts on it.
fn kept_duties(reasons: &[AttentionReason], entries: &[Entry]) -> Vec<RoomListeningDuty> {
    let mut kept = reasons
        .iter()
        .filter_map(|reason| match reason {
            AttentionReason::Work { task, .. } | AttentionReason::Responsibility { task, .. } => {
                entries
                    .iter()
                    .find(|entry| entry.born() == task)
                    .map(|entry| RoomListeningDuty::Work {
                        task: entry.handle().clone(),
                    })
            }
            AttentionReason::Stop { key, .. } => Some(RoomListeningDuty::Stop { key: key.clone() }),
            AttentionReason::Asked { .. } => None,
        })
        .collect::<Vec<_>>();
    kept.sort();
    kept.dedup();
    kept
}
