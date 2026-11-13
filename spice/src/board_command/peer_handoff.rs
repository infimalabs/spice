use super::*;

/// Residency evidence could not name one allocation lifetime for a peer lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PeerHandoffFault {
    lane: wire::CanonicalUuid,
}

impl PeerHandoffFault {
    pub(crate) fn detail(self) -> String {
        format!(
            "peer lane {} has more than one durable task-allocation lifetime",
            self.lane
        )
    }
}

/// The durable deadline while `claimant` must leave its independent handoff to
/// an unclaimed peer that can take the exact row.
///
/// Direct `task next` and automatic resident targeting both call this rule.
/// Residency supplies peer identity; the shared ownership snapshot supplies the
/// one exact-row eligibility predicate used by allocation.
pub(crate) fn independent_handoff_awaits_peer(
    entries: &[Entry],
    satisfied: &board::ResolvedTasks,
    selected: &Entry,
    claimant: &Actor,
    (residency, owners): (
        &wire::ResidencyHead,
        &std::sync::Arc<board::OwnershipSnapshot>,
    ),
    environment: &wire::Environment,
    now: Instant,
) -> Result<Option<Instant>, PeerHandoffFault> {
    board::independent_handoff_awaits_peer(
        selected,
        claimant,
        satisfied,
        || {
            for lane in &residency.lanes {
                if Actor::Lane(lane.lane) == *claimant {
                    continue;
                }
                let mut subscriptions = residency
                    .subscriptions
                    .iter()
                    .filter(|subscription| subscription.team == lane.team);
                let _ = subscriptions.next();
                if subscriptions.next().is_some() {
                    return Err(PeerHandoffFault { lane: lane.lane });
                }
            }
            Ok(residency.lanes.iter().filter_map(|lane| {
                let actor = Actor::Lane(lane.lane);
                if actor == *claimant {
                    return None;
                }
                let subscription = residency
                    .subscriptions
                    .iter()
                    .find(|subscription| subscription.team == lane.team)?;
                let holds_live_claim = entries.iter().any(|entry| {
                    entry
                        .claim_of(&actor)
                        .and_then(board::TaskClaim::lease)
                        .is_some_and(|lease| lease.expiry > now)
                });
                Some(board::HandoffPeer {
                    actor,
                    subscription: owners.for_team(subscription.team),
                    environment,
                    holds_live_claim,
                })
            }))
        },
        now,
    )
}
