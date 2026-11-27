use std::collections::BTreeSet;

use wire::{CanonicalUuid, Seq};

use super::super::{RuntimePaths, design_input, supervisor};

/// Privately report a malformed newest arm fact once a lane has an occupant.
/// Vacant lanes retain the fault until their next seating.
pub(super) fn report_faults(
    reported: &mut BTreeSet<(CanonicalUuid, Seq, CanonicalUuid)>,
    paths: &RuntimePaths,
    snapshot: &bus::TaskSnapshot,
    invalid: &[(CanonicalUuid, Seq)],
) {
    let Some(residency) = snapshot.residency() else {
        reported.clear();
        return;
    };
    let current = invalid
        .iter()
        .filter_map(|&(lane, position)| {
            residency
                .body()
                .lanes
                .iter()
                .find(|row| row.lane == lane)
                .and_then(|row| row.occupant)
                .map(|occupant| (lane, position, occupant))
        })
        .collect::<BTreeSet<_>>();
    reported.retain(|fault| current.contains(fault));
    for (lane, position, occupant) in current {
        if reported.contains(&(lane, position, occupant)) {
            continue;
        }
        let notice = design_input::BackgroundNotice {
            occupant,
            purpose: format!("line-arm-invalid:{}", position.get()),
            text: format!(
                "<system-notice>\nThe newest line-arm fact at position {} is unreadable or \
                 names another lane, so it parks nothing. Re-arm the intended peer lines. \
                 Private notice; no acknowledgment key or visible ACK/NAK is required.\n\
                 </system-notice>",
                position.get()
            ),
        };
        if supervisor::command::confirmed_background_notice(paths, lane, notice, None)
            .unwrap_or(false)
        {
            reported.insert((lane, position, occupant));
        }
    }
}
