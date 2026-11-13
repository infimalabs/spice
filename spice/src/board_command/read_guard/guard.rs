use super::*;

/// Refuse an allocator write when its exact witnesses, residency, compact
/// projection cursor, or indexed one-active-claim invariant has moved.
pub(in crate::board_command) fn guard_allocation(
    write: &mut JournalWrite,
    expected: Vec<Witness>,
    observed: AllocationObservation,
) {
    write.guard(JournalGuard::new(move |read| {
        witnesses_hold(read, &expected)?;
        let residency = read.residency_head()?.ok_or_else(|| {
            GuardError::Invalid("allocator current residency head is missing".to_owned())
        })?;
        if residency.position() != observed.residency_position {
            return Err(GuardError::refused(
                "board::witness-moved: allocator subscription head changed",
            ));
        }
        if read.cursor()? != Some(observed.cursor) {
            return Err(GuardError::refused(
                "board::witness-moved: allocator projection changed",
            ));
        }
        refuse_active_claim_conflict(read, &observed.actor)
    }));
}

pub(super) fn refuse_active_claim_conflict(
    read: &JournalRead<'_>,
    actor: &Actor,
) -> Result<(), GuardError> {
    let Some(active) = read.active_claim_for_actor(actor)? else {
        return Ok(());
    };
    let first = active
        .statement()
        .body()
        .handle(active.workflow().body().phase);
    Err(GuardError::refused(format!(
        "claim::active-exists: actor {actor} already holds active claim {first}"
    )))
}

pub(super) fn witnesses_hold(
    read: &JournalRead<'_>,
    expected: &[Witness],
) -> Result<(), GuardError> {
    for witness in expected {
        let holds = witness_holds(read, witness)?;
        if !holds {
            return Err(GuardError::refused(format!(
                "board::witness-moved: {witness:?}"
            )));
        }
    }
    Ok(())
}

fn witness_holds(read: &JournalRead<'_>, witness: &Witness) -> Result<bool, GuardError> {
    match witness {
        Witness::Task { .. }
        | Witness::Absence { .. }
        | Witness::Existence { .. }
        | Witness::Edge { .. } => task_witness_holds(read, witness),
        Witness::Workflow { .. }
        | Witness::Completion { .. }
        | Witness::Claim { .. }
        | Witness::Lease { .. }
        | Witness::Deferral { .. }
        | Witness::Assignment { .. } => workflow_witness_holds(read, witness),
        Witness::Residency { .. }
        | Witness::Eligibility { .. }
        | Witness::Preview { .. }
        | Witness::Barrier { .. } => board_witness_holds(read, witness),
        Witness::OopsGeneration { head, design } => {
            let observed = read.oops_generation()?;
            Ok(
                observed.as_ref().map(journal::JournalHead::position) == *head
                    && observed.and_then(|head| head.into_body().design) == *design,
            )
        }
        Witness::Proposal { proposal, head } => {
            Ok(read.proposal_head(proposal)?.map(|head| head.position()) == *head)
        }
        Witness::Room { room, head } => Ok(read.room_publication_head(room)? == *head),
        Witness::Authorization { output, head } => {
            Ok(read.authorization_head(output)?.map(|head| head.position()) == *head)
        }
        Witness::Directive { steer, disposition } => Ok(read
            .steering_head(*steer)?
            .is_some_and(|head| head.body().disposition == *disposition)),
        Witness::Recoverable { .. }
        | Witness::Consent { .. }
        | Witness::Rule { .. }
        | Witness::Budget { .. }
        | Witness::Trust { .. }
        | Witness::Observation { .. }
        | Witness::Sealed { .. }
        | Witness::Source { .. }
        | Witness::Transition { .. } => Err(GuardError::Invalid(
            "board transaction carried a witness outside its authority plane".to_owned(),
        )),
    }
}

fn task_witness_holds(read: &JournalRead<'_>, witness: &Witness) -> Result<bool, GuardError> {
    Ok(match witness {
        Witness::Task { task, head } => read
            .task_head(task)?
            .is_some_and(|observed| observed.position() == *head),
        Witness::Absence { task } => read.task_head(task)?.is_none(),
        Witness::Existence { task } => read.task_head(task)?.is_some(),
        Witness::Edge { task, after } => {
            let subject = read.task_head(task)?;
            let dependency = read.task_head(after)?;
            subject.is_some_and(|head| !head.body().dependencies.contains(after))
                && dependency.is_some()
        }
        _ => unreachable!("task witness partition is closed"),
    })
}

fn workflow_witness_holds(read: &JournalRead<'_>, witness: &Witness) -> Result<bool, GuardError> {
    Ok(match witness {
        Witness::Workflow { task, head, phase } => {
            read.workflow_head(task)?.is_some_and(|observed| {
                observed.position() == *head && observed.body().phase == *phase
            })
        }
        Witness::Completion { task, head } => read
            .completion_head(task)?
            .is_some_and(|observed| observed.position() == *head),
        Witness::Claim {
            task,
            claimed: Some(claimed),
            head,
        } => {
            read.claim_head(task, *claimed)?
                .map(|observed| observed.position())
                == *head
        }
        Witness::Claim {
            task,
            claimed: None,
            head,
        } => read.claim_plane(task)? == *head,
        Witness::Lease {
            task,
            claimed,
            head,
        } => read
            .lease_head(task, *claimed)?
            .is_some_and(|observed| observed.position() == *head),
        Witness::Deferral {
            task,
            head,
            deferred,
        } => read.workflow_head(task)?.is_some_and(|observed| {
            observed.position() == *head && observed.body().deferral.is_some() == *deferred
        }),
        Witness::Assignment {
            task,
            head,
            reviewer,
        } => read.claim_head(task, *head)?.is_some_and(|observed| {
            let claim = observed.body();
            claim.actor == *reviewer
                && claim.cell.phase.as_str() == "review"
                && claim.disposition == wire::ClaimDisposition::Active
        }),
        _ => unreachable!("workflow witness partition is closed"),
    })
}

fn board_witness_holds(read: &JournalRead<'_>, witness: &Witness) -> Result<bool, GuardError> {
    Ok(match witness {
        Witness::Residency {
            lane,
            occupant: Some(occupant),
            epoch,
        } => read.residency_head()?.is_some_and(|observed| {
            observed.body().epoch == *epoch
                && observed.body().lanes.iter().any(|seat| {
                    seat.lane == *lane
                        && seat.occupant == Some(*occupant)
                        && seat.disposition == "occupied"
                })
        }),
        // Board work is somebody's: a lane nobody sits in holds no claim.
        Witness::Residency { occupant: None, .. } => false,
        Witness::Eligibility { rule } => matches!(
            rule.as_str(),
            "next"
                | "capture"
                | CAPTURE_WORK_RULE
                | "join"
                | "steal"
                | "recover"
                | RECOVER_STEAL_RULE
        ),
        Witness::Preview { .. } => true,
        Witness::Barrier { name, digest } => {
            name.as_str() == "board"
                && task_projection_barrier(read.task_projection_cursor()?) == *digest
        }
        _ => unreachable!("board witness partition is closed"),
    })
}
