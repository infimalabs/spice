//! The start each vacant target of one packet commits.

use super::*;

/// The idle body a vacant `target` of a packet from `from` carries.
pub(super) fn idle(from: CanonicalUuid, target: &Target) -> Idle {
    Idle {
        from,
        lane: target.lane,
        room: target.room.clone(),
        returns: None,
    }
}

/// Decide at `at` the start each vacant target's body commits, returning the
/// targets admitted and why each refused one received nothing. Seated targets
/// are active delivery and read no start state.
pub(super) fn admit_idle(
    bus: &Bus,
    from: CanonicalUuid,
    targets: Vec<Target>,
    at: Instant,
) -> Result<(Vec<Target>, Vec<String>), Unpublished> {
    let idle: Vec<_> = targets
        .iter()
        .filter(|target| target.occupant.is_none())
        .map(|target| idle(from, target))
        .collect();
    if idle.is_empty() {
        return Ok((targets, Vec::new()));
    }
    let mut decided = bus
        .read(move |read| {
            idle.iter()
                .map(|body| body.admit(read, at))
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(|error| {
            Unpublished::Failed(store_fault(
                "steer::admission-unreadable",
                &error.to_string(),
            ))
        })?
        .into_iter();
    let mut admitted = Vec::with_capacity(targets.len());
    let mut spent = Vec::new();
    for mut target in targets {
        if target.occupant.is_some() {
            admitted.push(target);
            continue;
        }
        match decided.next().expect("one decision per vacant target") {
            Ok(admission) => {
                target.admission = Some(admission);
                admitted.push(target);
            }
            Err(exhausted) => spent.push(exhausted.detail(target.recipient)),
        }
    }
    Ok((admitted, spent))
}
