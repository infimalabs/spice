//! One lane's share of a steering inbox read.

use super::*;

/// One lane's inbox from one snapshot: its compaction anchor, pending
/// directives, and open lines.
pub(super) fn assemble(
    snapshot: &SteeringSnapshot,
    resolver: &spice::quote::Resolver<'_>,
    principals: &wire::principal::PrincipalRoles,
    lane: &wire::LaneResidency,
) -> Result<LaneInbox, Outcome<CommandAnswer>> {
    let compaction = match lane.occupant {
        Some(occupant) => {
            match crate::resident::taste_surface::context_anchor(
                snapshot.authority.facts(),
                lane.lane,
                occupant,
            ) {
                Ok(anchor) => Some(anchor.get()),
                Err(error) => {
                    return Err(store_fault("steer::compaction-anchor-missing", &error));
                }
            }
        }
        None => None,
    };
    let mut pending = pending_presentations(
        &snapshot.ledger,
        &snapshot.waking,
        &snapshot.recipients,
        &snapshot.authority,
        principals,
        lane.lane,
    )?;
    for directive in &mut pending {
        if let Some(origin) = snapshot.peers.get(&directive.directive) {
            directive.sender = Some(origin.sender);
            directive.addressed = origin.addressed;
        }
        directive.line = snapshot.lines.pending.get(&directive.directive).cloned();
        directive.text = resolver.expand(&directive.text);
        directive.preview = directive_preview(&directive.text);
    }
    Ok(LaneInbox {
        lane: lane.lane,
        occupant: lane.occupant,
        compaction,
        pending,
        lines: snapshot
            .lines
            .open
            .get(&lane.lane)
            .cloned()
            .unwrap_or_default(),
    })
}
