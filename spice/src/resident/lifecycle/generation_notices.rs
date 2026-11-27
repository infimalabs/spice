//! Private claimant notices derived from task authority.
//!
//! The task journal is the authority. A resident-minted claim is announced to
//! its exact occupant, and each active claim records the task generation it
//! acknowledged so later edits can require acknowledgement through `task
//! next`. Once any claim's generation is acknowledged, its completion contract
//! follows linked work through a separate projection. Shared-round events wait
//! for proved participant addresses and the receiving claim's disclosure.
//! These projections create no steering directive or acknowledgment key; the
//! detached occupant driver deduplicates repeated resident observations by
//! their authority identities.

use journal::TaskHeads;

use super::*;
use crate::resident::supervisor::command::{
    TaskAssignmentEnd, TaskAssignmentNotice, TaskContractNotice, TaskGenerationNotice,
    TaskRoundUpdateKind, TaskRoundUpdateNotice, TasteHoldNotice, TasteState,
};

/// Index rows one read returns while a claim trails its task.
const ADVANCE_PAGE: usize = 64;

pub(super) fn reconcile(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    snapshot: &bus::TaskSnapshot,
    findings: super::review_feedback::FindingWindow<'_>,
) -> CommandResult<bool> {
    for (lane, notice) in assignment_notices(snapshot) {
        super::super::supervisor::command::task_assignment(paths, lane, notice);
    }
    for (lane, ended) in assignment_ends(snapshot, findings.cursor) {
        super::super::supervisor::command::task_assignment_ended(paths, lane, ended);
    }
    for (lane, notice) in notices(bus, snapshot)? {
        super::super::supervisor::command::task_generation(paths, lane, notice);
    }
    for held in crate::board_command::claim_contracts(snapshot) {
        super::super::supervisor::command::task_contract(
            paths,
            held.lane,
            TaskContractNotice {
                occupant: held.occupant,
                task: held.task.to_string(),
                claim: held.claim,
                completion_contract: held.contract,
                design_research: held.design_research,
            },
        );
    }
    for (lane, notice) in round_update_notices(snapshot, findings.cursor) {
        super::super::supervisor::command::task_round_update(paths, lane, notice);
    }
    for (lane, notice) in taste_holds(bus, snapshot, findings.cursor)? {
        super::super::supervisor::command::taste_hold(paths, lane, notice);
    }
    let departed = super::super::team_departure::reconcile(
        bus,
        paths,
        epoch,
        snapshot,
        findings.cursor,
        findings.lifetime,
    )?;
    let reviewed = super::review_feedback::reconcile(bus, paths, epoch, snapshot, findings)?;
    Ok(departed || reviewed)
}

/// Each place a hold matched, as a lane may read it: findings withheld.
fn hold_places(hold: &wire::TasteHold) -> Vec<String> {
    hold.matches
        .iter()
        .map(|found| {
            format!(
                "{} line {} taste {}",
                found.field.as_str(),
                found.line,
                found.taste
            )
        })
        .collect()
}

/// Holds a lane's statement write after `cursor` opened.
///
/// A hold records the fact of the policy-reading write that opened it, so the
/// lane that wrote that fact learns of the hold at its current occupant,
/// whichever lane holds or will hold the task. A later write that carried the
/// hold through, such as a dependency edit, is never taken for its author. A
/// hold recorded before holds named their opening announces nothing, and the
/// first pass has no earlier observation and announces none.
fn taste_holds(
    bus: &Bus,
    snapshot: &bus::TaskSnapshot,
    cursor: Option<Seq>,
) -> CommandResult<Vec<(CanonicalUuid, TasteHoldNotice)>> {
    let Some(cursor) = cursor else {
        return Ok(Vec::new());
    };
    let opened = snapshot
        .tasks()
        .iter()
        .filter_map(|heads| {
            let statement = heads.statement().body();
            let hold = statement.taste.as_ref()?;
            let opening = hold.opened.filter(|opening| *opening > cursor)?;
            let Some(wire::Actor::Lane(lane)) =
                snapshot.facts().at(opening).map(|envelope| &envelope.actor)
            else {
                return None;
            };
            Some((
                *lane,
                opening,
                statement,
                heads.workflow().body().phase,
                hold,
            ))
        })
        .collect::<Vec<_>>();
    if opened.is_empty() {
        return Ok(Vec::new());
    }
    let residency = bus
        .residency_head()
        .map_err(|error| store_fault("taste-hold::residency-unreadable", &error.to_string()))?;
    let Some(residency) = residency else {
        return Ok(Vec::new());
    };
    Ok(opened
        .into_iter()
        .filter_map(|(lane, opening, statement, phase, hold)| {
            let occupant = residency
                .body()
                .lanes
                .iter()
                .find(|durable| durable.lane == lane)?
                .occupant?;
            Some((
                lane,
                TasteHoldNotice {
                    occupant,
                    task: statement.handle(phase).to_string(),
                    position: opening.get(),
                    matches: hold_places(hold),
                    completion_contract: crate::board_command::completion_contract(
                        &statement.term,
                        &statement.born,
                        &statement.dependencies,
                        statement.intent,
                        snapshot.tasks().iter(),
                    ),
                },
            ))
        })
        .collect())
}

fn assignment_notices(snapshot: &bus::TaskSnapshot) -> Vec<(CanonicalUuid, TaskAssignmentNotice)> {
    snapshot
        .tasks()
        .iter()
        .flat_map(|heads| heads.claims().iter().map(move |held| (heads, held)))
        .filter_map(|(heads, held)| {
            let claim = held.claim().body();
            if claim.disposition != ClaimDisposition::Active
                || !crate::board_command::is_automatic_claim(claim, snapshot.facts())
            {
                return None;
            }
            let statement = heads.statement().body();
            Some((
                claim.lane,
                TaskAssignmentNotice {
                    occupant: claim.occupant,
                    task: statement.handle(heads.workflow().body().phase).to_string(),
                    claim: claim.claimed.get(),
                    phase: heads.workflow().body().phase.to_string(),
                    design_research: crate::board_command::design_research(
                        statement,
                        heads.workflow().body(),
                        snapshot.facts().design_items(&statement.born),
                    ),
                    completion_contract: crate::board_command::completion_contract(
                        &statement.term,
                        &statement.born,
                        &statement.dependencies,
                        statement.intent,
                        snapshot.tasks().iter(),
                    ),
                },
            ))
        })
        .collect()
}

/// One participant label only after its claim occupant's address is proved.
fn addressed_participant_label(
    snapshot: &bus::TaskSnapshot,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
) -> Option<String> {
    let row = snapshot
        .residency()?
        .body()
        .lanes
        .iter()
        .find(|row| row.lane == lane)?;
    let address = snapshot.facts().occupant_address(lane, occupant).ok()?;
    let name = row
        .name
        .as_deref()
        .map_or_else(|| lane.to_string(), str::to_owned);
    Some(format!("{name} {address}"))
}

/// Claim endings since the previous lifecycle observation, with their winning
/// claim authority restored even after a later join replaced the ended head.
fn round_departures(
    snapshot: &bus::TaskSnapshot,
    cursor: Option<Seq>,
) -> Vec<(Seq, Seq, wire::body::ClaimWon)> {
    let Some(cursor) = cursor else {
        return Vec::new();
    };
    let facts = snapshot.facts();
    facts
        .after(Some(cursor))
        .filter_map(|envelope| {
            let (claimed, won) = match envelope.fact.kind() {
                wire::FactKind::ClaimFreed => {
                    let claimed = envelope.cause?;
                    let won = facts.at(claimed)?.fact.read::<wire::body::ClaimWon>()?;
                    (claimed, won)
                }
                wire::FactKind::ClaimLapsed => {
                    let lapsed = envelope.fact.read::<wire::body::ClaimLapsed>()?;
                    facts.iter().rev().find_map(|candidate| {
                        if candidate.seq >= envelope.seq {
                            return None;
                        }
                        let won = candidate.fact.read::<wire::body::ClaimWon>()?;
                        (won.task == lapsed.task && won.occupant == lapsed.occupant)
                            .then_some((candidate.seq, won))
                    })?
                }
                _ => return None,
            };
            Some((envelope.seq, claimed, won))
        })
        .collect()
}

/// Every current-round join, arrival, and departure projected to each active
/// partner once its participant has a proved address.
///
/// Joins and arrivals remain in the current heads and reproject until the
/// occupant driver deduplicates them. Departures consume immutable ending facts
/// after the lifecycle cursor because a later join replaces an ended claim head.
fn round_update_notices(
    snapshot: &bus::TaskSnapshot,
    cursor: Option<Seq>,
) -> Vec<(CanonicalUuid, TaskRoundUpdateNotice)> {
    let departures = round_departures(snapshot, cursor);
    let mut notices = Vec::new();
    for heads in snapshot.tasks() {
        let completion = heads.completion().body();
        let phase = &heads.workflow().body().phase;
        let statement = heads.statement().body();
        let task = statement.handle(*phase).to_string();
        let active = heads
            .claims()
            .iter()
            .map(|held| held.claim().body())
            .filter(|claim| claim.disposition == ClaimDisposition::Active)
            .collect::<Vec<_>>();

        for joined in heads.claims().iter().map(|held| held.claim().body()) {
            let Some(participant) =
                addressed_participant_label(snapshot, joined.lane, joined.occupant)
            else {
                continue;
            };
            for holder in active
                .iter()
                .copied()
                .filter(|holder| holder.claimed < joined.claimed)
            {
                notices.push((
                    holder.lane,
                    TaskRoundUpdateNotice {
                        occupant: holder.occupant,
                        task: task.clone(),
                        holder_claim: holder.claimed.get(),
                        event: joined.claimed.get(),
                        phase: phase.to_string(),
                        participant: participant.clone(),
                        action: TaskRoundUpdateKind::Joined,
                    },
                ));
            }
        }
        for arrival in &completion.arrivals {
            let Some(claim) = heads
                .claims()
                .iter()
                .map(|held| held.claim().body())
                .find(|claim| claim.claimed == arrival.claimed)
            else {
                continue;
            };
            let Some(participant) =
                addressed_participant_label(snapshot, arrival.lane, claim.occupant)
            else {
                continue;
            };
            for holder in active
                .iter()
                .copied()
                .filter(|holder| holder.lane != arrival.lane && holder.claimed < arrival.fact)
            {
                notices.push((
                    holder.lane,
                    TaskRoundUpdateNotice {
                        occupant: holder.occupant,
                        task: task.clone(),
                        holder_claim: holder.claimed.get(),
                        event: arrival.fact.get(),
                        phase: phase.to_string(),
                        participant: participant.clone(),
                        action: TaskRoundUpdateKind::Arrived,
                    },
                ));
            }
        }
        for (event, claimed, left) in &departures {
            if left.task != statement.born
                || left.cell.phase != *phase
                || *claimed < completion.round
                || completion
                    .arrivals
                    .iter()
                    .any(|arrival| arrival.claimed == *claimed)
            {
                continue;
            }
            let Some(participant) = addressed_participant_label(snapshot, left.lane, left.occupant)
            else {
                continue;
            };
            for holder in active
                .iter()
                .copied()
                .filter(|holder| holder.lane != left.lane && holder.claimed < *event)
            {
                notices.push((
                    holder.lane,
                    TaskRoundUpdateNotice {
                        occupant: holder.occupant,
                        task: task.clone(),
                        holder_claim: holder.claimed.get(),
                        event: event.get(),
                        phase: phase.to_string(),
                        participant: participant.clone(),
                        action: TaskRoundUpdateKind::Left,
                    },
                ));
            }
        }
    }
    notices
}

/// Claims that ended after `cursor`, the position the previous pass observed.
///
/// An assignment notice waits in the occupant's queue until its next step
/// boundary, so the claim it announces can end first. Each ending since the
/// last pass is told to the occupant that held the claim, whose driver
/// withdraws a notice for it that has not been delivered yet. The first pass
/// has no earlier observation and announces no endings.
fn assignment_ends(
    snapshot: &bus::TaskSnapshot,
    cursor: Option<Seq>,
) -> Vec<(CanonicalUuid, TaskAssignmentEnd)> {
    let Some(cursor) = cursor else {
        return Vec::new();
    };
    snapshot
        .tasks()
        .iter()
        .flat_map(|heads| heads.claims().iter().map(move |held| (heads, held)))
        .filter_map(|(heads, held)| {
            let claim = held.claim().body();
            if claim.disposition == ClaimDisposition::Active || claim.fact <= cursor {
                return None;
            }
            let statement = heads.statement().body();
            Some((
                claim.lane,
                TaskAssignmentEnd {
                    occupant: claim.occupant,
                    task: statement.handle(heads.workflow().body().phase).to_string(),
                    claim: claim.claimed.get(),
                },
            ))
        })
        .collect()
}

fn notices(
    bus: &Bus,
    snapshot: &bus::TaskSnapshot,
) -> CommandResult<Vec<(CanonicalUuid, TaskGenerationNotice)>> {
    let mut notices = Vec::new();
    for heads in snapshot.tasks() {
        for held in heads.claims() {
            let claim = held.claim().body();
            if claim.disposition == ClaimDisposition::Active {
                notices.extend(task_notices(bus, snapshot, heads, claim)?);
            }
        }
    }
    Ok(notices)
}

fn task_notices(
    bus: &Bus,
    snapshot: &bus::TaskSnapshot,
    heads: &TaskHeads,
    claim: &wire::ClaimHead,
) -> CommandResult<Vec<(CanonicalUuid, TaskGenerationNotice)>> {
    let facts = snapshot.facts();
    if claim.acknowledged >= heads.workflow().body().generation {
        return Ok(Vec::new());
    }
    let statement = heads.statement().body();
    let handle = statement.handle(heads.workflow().body().phase).to_string();
    let mut after = claim.acknowledged;
    let mut notices = Vec::new();
    loop {
        let task = statement.born.clone();
        let advances = bus
            .read(move |read| read.task_generations_after(&task, after, ADVANCE_PAGE))
            .map_err(|error| {
                store_fault("task-generation::index-unreadable", &error.to_string())
            })?;
        for advance in &advances {
            let envelope = facts.at(advance.position).ok_or_else(|| {
                store_fault(
                    "task-generation::fact-missing",
                    &format!(
                        "task {} generation {} names missing position {}",
                        statement.born, advance.generation, advance.position
                    ),
                )
            })?;
            // The holder hears where the statement stands after the change;
            // an unheld statement is worth naming only after a statement edit,
            // which may have just cleared it.
            let taste = match &statement.taste {
                Some(hold) => Some(TasteState::Held {
                    matches: hold_places(hold),
                    rechecks: hold.rechecks,
                }),
                None => envelope
                    .fact
                    .read::<wire::body::TaskEdited>()
                    .is_some()
                    .then_some(TasteState::Clear),
            };
            notices.push((
                claim.lane,
                TaskGenerationNotice {
                    occupant: claim.occupant,
                    task: handle.clone(),
                    claim: claim.claimed.get(),
                    generation: advance.generation,
                    change: envelope.fact.kind().to_string(),
                    editor: envelope.actor.to_string(),
                    position: advance.position.get(),
                    taste,
                    design_research: crate::board_command::design_research(
                        statement,
                        heads.workflow().body(),
                        facts.design_items(&statement.born),
                    ),
                    completion_contract: crate::board_command::completion_contract(
                        &statement.term,
                        &statement.born,
                        &statement.dependencies,
                        statement.intent,
                        snapshot.tasks().iter(),
                    ),
                },
            ));
        }
        match advances.last() {
            Some(last) if advances.len() == ADVANCE_PAGE => after = last.generation,
            _ => return Ok(notices),
        }
    }
}

#[cfg(test)]
mod tests;
