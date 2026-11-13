//! Durable agent-renewal intent and resident-owned handoff reconciliation.
//!
//! Renewal is intent, never a launch. Requesting it marks one member for
//! replacement and, while a predecessor is still running, asks that occupant
//! to finish. Nothing here starts a process: the mark waits in
//! [`wire::LaneRenewalState::HandoffPending`] until something else starts the
//! lane, and that start consumes it through [`pending_for`], [`prepare`], and
//! [`append_successor`].

use super::*;

pub(in crate::resident) fn set(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    seats: &Seats,
    command: &wire::AdmittedCommand,
    request: &wire::intent::LaneRenewal,
) -> Outcome<CommandAnswer> {
    let (observed, row, current) = match admit_set(bus, epoch, request) {
        Ok(admitted) => admitted,
        Err(outcome) => return outcome,
    };
    let record = match if request.requested {
        resolve_request(observed.body(), &row, seats, current, request)
    } else {
        resolve_cancel(current)
    } {
        Ok(record) => record,
        Err(outcome) => return typed_outcome(outcome),
    };
    commit_set(bus, paths, epoch, command, request, observed, record)
}

type AdmittedSet = (
    journal::JournalHead<wire::ResidencyHead>,
    wire::LaneResidency,
    Option<wire::LaneRenewalRecord>,
);

fn admit_set(
    bus: &Bus,
    epoch: CanonicalUuid,
    request: &wire::intent::LaneRenewal,
) -> Result<AdmittedSet, Outcome<CommandAnswer>> {
    let observed = current_residency(bus, epoch).map_err(typed_outcome)?;
    if request.topology_revision != observed.body().topology_revision {
        return Err(lane_refusal(
            "lane::renewal-topology-moved",
            Some(observed.body().topology_revision.to_string()),
            Some(request.topology_revision.to_string()),
            "read lane state and retry against the current topology revision",
        ));
    }
    let Some(row) = observed
        .body()
        .lanes
        .iter()
        .find(|row| row.lane == request.lane)
        .cloned()
    else {
        return Err(missing_lane(request.lane));
    };
    if row.team != request.team {
        return Err(lane_refusal(
            "lane::renewal-team-moved",
            Some(row.team.to_string()),
            Some(request.team.to_string()),
            "read lane state and retry with the member's current team",
        ));
    }
    let current = renewal_for(observed.body(), request.lane)
        .map_err(typed_outcome)?
        .cloned();
    Ok((observed, row, current))
}

fn resolve_request(
    head: &wire::ResidencyHead,
    row: &wire::LaneResidency,
    seats: &Seats,
    current: Option<wire::LaneRenewalRecord>,
    request: &wire::intent::LaneRenewal,
) -> CommandResult<Option<wire::LaneRenewalRecord>> {
    match current {
        None
        | Some(wire::LaneRenewalRecord {
            state: wire::LaneRenewalState::SuccessorStarted { .. },
            ..
        }) => requested_record(head, row, seats).map(Some),
        Some(wire::LaneRenewalRecord {
            state:
                wire::LaneRenewalState::LaunchRefused {
                    predecessor,
                    team_slot,
                    ..
                },
            ..
        }) => Ok(Some(wire::LaneRenewalRecord {
            team: request.team,
            lane: request.lane,
            revision: request.topology_revision,
            state: wire::LaneRenewalState::HandoffPending {
                predecessor,
                team_slot,
            },
        })),
        Some(record) => Err(lane_refusal(
            "lane::renewal-already-active",
            Some("no current renewal or one launch refusal".to_owned()),
            Some(state_name(&record.state).to_owned()),
            "wait for the current handoff or repair a refused successor launch",
        )),
    }
}

fn resolve_cancel(
    current: Option<wire::LaneRenewalRecord>,
) -> CommandResult<Option<wire::LaneRenewalRecord>> {
    match current {
        Some(wire::LaneRenewalRecord {
            state:
                wire::LaneRenewalState::Requested { .. } | wire::LaneRenewalState::HandoffPending { .. },
            ..
        }) => Ok(None),
        Some(record) => Err(lane_refusal(
            "lane::renewal-cancel-late",
            Some("requested or handoff-pending".to_owned()),
            Some(state_name(&record.state).to_owned()),
            "a successor already started under this renewal",
        )),
        None => Err(lane_refusal(
            "lane::renewal-not-requested",
            Some("one requested renewal".to_owned()),
            Some("absent".to_owned()),
            "request renewal before attempting to clear it",
        )),
    }
}

fn commit_set(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    command: &wire::AdmittedCommand,
    request: &wire::intent::LaneRenewal,
    observed: journal::JournalHead<wire::ResidencyHead>,
    record: Option<wire::LaneRenewalRecord>,
) -> Outcome<CommandAnswer> {
    let mut next = observed.body().clone();
    replace_renewal(&mut next, request.lane, record.clone());
    let mut proposal = match write(
        command.actor().clone(),
        epoch,
        Producer::ResidentRecognition,
    ) {
        Ok(proposal) => proposal,
        Err(outcome) => return typed_outcome(outcome),
    };
    guard_residency(&mut proposal, &observed);
    let changed = wire::body::LaneRenewalChanged {
        team: request.team,
        lane: request.lane,
        renewal: record.clone(),
    };
    let head = match proposal.fact(changed, FactCause::Existing(observed.position())) {
        Ok(head) => head,
        Err(error) => {
            return store_fault("lane::renewal-fact-invalid", &error.to_string());
        }
    };
    proposal.residency_head(head, move |_| next);
    let answer = wire::intent::LaneRenewalAnswer {
        team: request.team,
        lane: request.lane,
        topology_revision: request.topology_revision,
        renewal: record,
    };
    if let Err(error) =
        proposal.command_result(typed_command(command, *request), epoch, answer.clone())
    {
        return store_fault("lane::renewal-receipt-invalid", &error.to_string());
    }
    let committed = match commit(bus, proposal) {
        Ok(committed) => committed,
        Err(outcome) => return typed_outcome(outcome),
    };
    finish_set(bus, paths, epoch, committed, answer)
}

fn finish_set(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    committed: bus::JournalCommit,
    answer: wire::intent::LaneRenewalAnswer,
) -> Outcome<CommandAnswer> {
    if let Err(outcome) = reconcile(bus, paths, epoch) {
        return typed_outcome(outcome);
    }
    Outcome::Reply(Reply {
        position: ReplyPosition::Committed(SeqRange {
            first: seq(committed.first()),
            last: seq(committed.last()),
        }),
        body: CommandAnswer::LaneRenewal(Box::new(answer)),
    })
}

fn requested_record(
    head: &wire::ResidencyHead,
    row: &wire::LaneResidency,
    seats: &Seats,
) -> CommandResult<wire::LaneRenewalRecord> {
    let session = super::session::validate_session_head(row)?;
    if let Some(occupant) = row.occupant {
        if session.occupant != occupant {
            return Err(store_fault(
                "lane::renewal-session-moved",
                "the durable session does not identify the current lane occupant",
            ));
        }
        if !seats
            .occupant(row.lane)
            .is_some_and(|seated| seated.occupant() == occupant)
        {
            return Err(store_fault(
                "lane::renewal-occupant-unowned",
                "the durable renewal predecessor is not supervised by this resident",
            ));
        }
    } else if seats.occupied(row.lane) {
        return Err(store_fault(
            "lane::renewal-capacity-occupied",
            "the durably vacant lane still has a supervised occupant",
        ));
    }
    let team_slot = head
        .lanes
        .iter()
        .filter(|candidate| candidate.team == row.team)
        .position(|candidate| candidate.lane == row.lane)
        .and_then(|slot| u64::try_from(slot).ok())
        .ok_or_else(|| {
            store_fault(
                "lane::renewal-slot-missing",
                "the current lane has no position inside its durable lineage",
            )
        })?;
    let predecessor = identity(session.occupant, session);
    Ok(wire::LaneRenewalRecord {
        team: row.team,
        lane: row.lane,
        revision: head.topology_revision,
        state: if row.occupant.is_some() {
            wire::LaneRenewalState::Requested {
                predecessor,
                team_slot,
            }
        } else {
            wire::LaneRenewalState::HandoffPending {
                predecessor,
                team_slot,
            }
        },
    })
}

fn identity(occupant: CanonicalUuid, session: &wire::LaneSession) -> wire::RenewalIdentity {
    wire::RenewalIdentity {
        occupant,
        session: session.identity.clone(),
        thread: session.identity.clone(),
    }
}

pub(super) fn requested_exit(
    head: &wire::ResidencyHead,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
) -> bool {
    renewal_for(head, lane)
        .ok()
        .flatten()
        .is_some_and(|record| {
            matches!(
                &record.state,
                wire::LaneRenewalState::Requested { predecessor, .. }
                    if predecessor.occupant == occupant
                        || head.lanes.iter().find(|row| row.lane == lane).is_some_and(|row| {
                            row.occupant == Some(occupant)
                                && row.session.as_ref().is_some_and(|session| {
                                    session.occupant == occupant
                                        && session.identity == predecessor.session
                                })
                        })
            )
        })
}

pub(super) fn advance_exit(
    proposal: &mut JournalWrite,
    next: &mut wire::ResidencyHead,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    cause: PendingFact,
) -> CommandResult<PendingFact> {
    let Some(current) = renewal_for(next, lane)?.cloned() else {
        return Ok(cause);
    };
    let wire::LaneRenewalState::Requested {
        predecessor,
        team_slot,
    } = current.state
    else {
        return Ok(cause);
    };
    let resumed_predecessor = next
        .lanes
        .iter()
        .find(|row| row.lane == lane)
        .and_then(|row| row.session.as_ref())
        .is_some_and(|session| {
            session.occupant == occupant && session.identity == predecessor.session
        });
    if predecessor.occupant != occupant && !resumed_predecessor {
        return Ok(cause);
    }
    let renewal = wire::LaneRenewalRecord {
        team: current.team,
        lane,
        revision: next.topology_revision,
        state: wire::LaneRenewalState::HandoffPending {
            predecessor,
            team_slot,
        },
    };
    replace_renewal(next, lane, Some(renewal.clone()));
    proposal
        .fact(
            wire::body::LaneRenewalChanged {
                team: renewal.team,
                lane,
                renewal: Some(renewal),
            },
            FactCause::Pending(cause),
        )
        .map_err(|error| store_fault("lane::renewal-fact-invalid", &error.to_string()))
}

/// The pending replacement one starting lane must consume, if it carries one.
///
/// A mark survives until a start takes it, so this is a pure read of durable
/// state: whether a start happens at all is never decided here.
pub(super) fn pending_for(
    head: &wire::ResidencyHead,
    lane: CanonicalUuid,
) -> CommandResult<Option<wire::LaneRenewalRecord>> {
    let Some(renewal) = renewal_for(head, lane)? else {
        return Ok(None);
    };
    if !matches!(renewal.state, wire::LaneRenewalState::HandoffPending { .. }) {
        return Ok(None);
    }
    let row = head
        .lanes
        .iter()
        .find(|row| row.lane == lane)
        .ok_or_else(|| {
            store_fault(
                "lane::renewal-member-missing",
                "a handoff-pending renewal names no current lane member",
            )
        })?;
    if row.team != renewal.team {
        return Err(store_fault(
            "lane::renewal-team-moved",
            "a handoff-pending renewal no longer belongs to its recorded team",
        ));
    }
    Ok(Some(renewal.clone()))
}

/// Settle what the successor of one consumed mark is owed, before it exists.
///
/// Every consent request the predecessor left pending lapses because the
/// occupant that would have answered a ruling is gone. The successor's
/// rehydration context needs no publication: its launch briefing derives it
/// from this handoff-pending record.
pub(super) fn prepare(
    bus: &Bus,
    runtime: &Runtime<'_>,
    renewal: &wire::LaneRenewalRecord,
) -> CommandResult<()> {
    super::super::consent_surface::commit_lapse(bus, runtime.reads, renewal.lane)
}

/// Project current renewal intent to the exact running occupants privately,
/// and end services owned by occupant turns no longer resident.
///
/// Reconciliation starts no process and creates no steering message.
pub(in crate::resident) fn reconcile(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
) -> CommandResult<()> {
    let observed = current_residency(bus, epoch)?;
    super::super::supervisor::terminal::reconcile_turn_services(paths, |lane, occupant| {
        observed
            .body()
            .lanes
            .iter()
            .any(|row| row.lane == lane && row.occupant == Some(occupant))
    });
    for row in &observed.body().lanes {
        let Some(occupant) = row.occupant else {
            continue;
        };
        let requested = renewal_for(observed.body(), row.lane)?.is_some_and(|renewal| {
            matches!(&renewal.state, wire::LaneRenewalState::Requested { predecessor, .. }
                if predecessor.occupant == occupant)
        });
        super::super::supervisor::command::renewal::notify(
            paths,
            row.lane,
            super::super::supervisor::command::renewal::Notice {
                occupant,
                position: observed.position().get(),
                requested,
            },
        );
    }
    Ok(())
}

/// Record the successor one consumed mark seated, inside the start's own
/// commit: a lane is renewed exactly when its replacement starts.
pub(super) fn append_successor(
    write: &mut JournalWrite,
    next: &mut wire::ResidencyHead,
    renewal: &wire::LaneRenewalRecord,
    session: &wire::LaneSession,
    cause: PendingFact,
) -> CommandResult<PendingFact> {
    let wire::LaneRenewalState::HandoffPending {
        predecessor,
        team_slot,
    } = &renewal.state
    else {
        return Err(store_fault(
            "lane::renewal-state-moved",
            "a successor can start only from handoff-pending state",
        ));
    };
    let successor = wire::LaneRenewalRecord {
        team: renewal.team,
        lane: renewal.lane,
        revision: next.topology_revision,
        state: wire::LaneRenewalState::SuccessorStarted {
            predecessor: predecessor.clone(),
            successor: identity(session.occupant, session),
            team_slot: *team_slot,
        },
    };
    replace_renewal(next, renewal.lane, Some(successor.clone()));
    write
        .fact(
            wire::body::LaneRenewalChanged {
                team: successor.team,
                lane: successor.lane,
                renewal: Some(successor),
            },
            FactCause::Pending(cause),
        )
        .map_err(|error| store_fault("lane::renewal-fact-invalid", &error.to_string()))
}

fn replace_renewal(
    head: &mut wire::ResidencyHead,
    lane: CanonicalUuid,
    renewal: Option<wire::LaneRenewalRecord>,
) {
    head.renewals.retain(|record| record.lane != lane);
    if let Some(renewal) = renewal {
        head.renewals.push(renewal);
        head.renewals
            .sort_by_key(|record| (record.team, record.lane));
    }
}

fn renewal_for(
    head: &wire::ResidencyHead,
    lane: CanonicalUuid,
) -> CommandResult<Option<&wire::LaneRenewalRecord>> {
    let mut matching = head.renewals.iter().filter(|record| record.lane == lane);
    let selected = matching.next();
    if matching.next().is_some() {
        return Err(store_fault(
            "lane::renewal-ambiguous",
            "one lane has more than one durable renewal state",
        ));
    }
    Ok(selected)
}

fn state_name(state: &wire::LaneRenewalState) -> &'static str {
    match state {
        wire::LaneRenewalState::Requested { .. } => "requested",
        wire::LaneRenewalState::HandoffPending { .. } => "handoff-pending",
        wire::LaneRenewalState::SuccessorStarted { .. } => "successor-started",
        wire::LaneRenewalState::LaunchRefused { .. } => "launch-refused",
    }
}

#[cfg(all(test, unix))]
mod tests;
