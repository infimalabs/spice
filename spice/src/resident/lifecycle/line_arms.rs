use std::collections::{BTreeMap, BTreeSet};

use wire::{CanonicalUuid, Seq};

use super::super::{RuntimePaths, design_input, supervisor};
use super::{Runtime, targets::ArmedLines};

type ReportIdentity = (CanonicalUuid, Seq, CanonicalUuid);
type OpenReport<'a> = (ReportIdentity, &'a [CanonicalUuid]);

pub(super) fn report(
    runtime: &mut Runtime,
    paths: &RuntimePaths,
    snapshot: &bus::TaskSnapshot,
    armed: &ArmedLines,
) {
    report_faults(
        &mut runtime.reported_line_arm_faults,
        paths,
        snapshot,
        &armed.invalid,
    );
    report_open(
        &mut runtime.reported_open_line_arms,
        paths,
        snapshot,
        &armed.open,
    );
}

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

/// Privately list the still-open roots once for each seating and arm revision.
/// Only the report key is retained; roots are derived from the current heads on
/// every reconciliation and never copied into resident state.
pub(super) fn report_open(
    reported: &mut BTreeSet<(CanonicalUuid, Seq, CanonicalUuid)>,
    paths: &RuntimePaths,
    snapshot: &bus::TaskSnapshot,
    open: &BTreeMap<CanonicalUuid, (Seq, Vec<CanonicalUuid>)>,
) {
    let Some(residency) = snapshot.residency() else {
        reported.clear();
        return;
    };
    reported.retain(|&(lane, position, occupant)| {
        residency
            .body()
            .lanes
            .iter()
            .any(|row| row.lane == lane && row.occupant == Some(occupant))
            && open
                .get(&lane)
                .is_some_and(|(current, roots)| *current == position && !roots.is_empty())
    });
    for row in &residency.body().lanes {
        let Some((identity, roots)) = open_report(open, row.lane, row.occupant) else {
            continue;
        };
        let (lane, position, occupant) = identity;
        if reported.contains(&identity) {
            continue;
        }
        let notice = design_input::BackgroundNotice {
            occupant,
            purpose: format!("line-arm-open:{}", position.get()),
            text: open_notice(roots),
        };
        if supervisor::command::confirmed_background_notice(paths, lane, notice, None)
            .unwrap_or(false)
        {
            reported.insert(identity);
        }
    }
}

fn open_report(
    open: &BTreeMap<CanonicalUuid, (Seq, Vec<CanonicalUuid>)>,
    lane: CanonicalUuid,
    occupant: Option<CanonicalUuid>,
) -> Option<OpenReport<'_>> {
    let occupant = occupant?;
    let (position, roots) = open.get(&lane)?;
    (!roots.is_empty()).then_some(((lane, *position, occupant), roots))
}

fn open_notice(roots: &[CanonicalUuid]) -> String {
    let mut roots = roots
        .iter()
        .map(|root| steer::Key::for_identity(*root).as_str().to_owned())
        .collect::<Vec<_>>();
    roots.sort();
    format!(
        "<system-notice>\nThis lane has armed peer lines still open at roots: {}. \
         Fresh peer input or operator steering still wakes it; held-claim continuation \
         waits after the turn. Private notice; no acknowledgment key or visible ACK/NAK \
         is required.\n</system-notice>",
        roots.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{open_notice, open_report};
    use wire::{CanonicalUuid, Seq};

    #[test]
    fn private_open_notice_names_only_root_keys() {
        let first = uuid(20);
        let second = uuid(21);
        let notice = open_notice(&[second, first]);
        assert!(notice.contains(steer::Key::for_identity(first).as_str()));
        assert!(notice.contains(steer::Key::for_identity(second).as_str()));
        assert!(!notice.contains(&first.to_string()));
        assert!(!notice.contains(&second.to_string()));
        assert!(notice.contains("Fresh peer input or operator steering still wakes it"));
    }

    #[test]
    fn every_seating_has_a_distinct_open_root_report() {
        let lane = uuid(10);
        let first = uuid(11);
        let second = uuid(12);
        let root = uuid(20);
        let position = Seq::new(3).expect("position");
        let open = BTreeMap::from([(lane, (position, vec![root]))]);

        let first_report = open_report(&open, lane, Some(first)).expect("first seating");
        let repeated = open_report(&open, lane, Some(first)).expect("same seating");
        let second_report = open_report(&open, lane, Some(second)).expect("second seating");
        assert_eq!(first_report, repeated, "one seating has one identity");
        assert_ne!(
            first_report.0, second_report.0,
            "a replacement occupant receives the current roots"
        );
        assert_eq!(second_report.1, [root]);
        assert!(
            open_report(&open, lane, None).is_none(),
            "a vacant lane receives no notice"
        );
    }

    fn uuid(value: u128) -> CanonicalUuid {
        CanonicalUuid::new(uuid::Uuid::from_u128(value))
    }
}
