use std::collections::BTreeMap;

use bus::Bus;
use journal::FactCause;
use wire::{Actor, CanonicalUuid, Instant, Producer};

use super::Runtime;
use crate::resident::shutdown::{current_residency, guard_residency};
use crate::resident::{CommandResult, RuntimePaths, commit, store_fault, write};

impl Runtime {
    /// Preserve a durable job park when an intermediate reconciliation projects
    /// no target for its lane. The running job and admitted horizon own that
    /// standing; a successful launch or fresh inbox input explicitly ends it.
    pub(super) fn reconcile_job_parks(
        &self,
        paths: &RuntimePaths,
        snapshot: &bus::TaskSnapshot,
        now: Instant,
        parks: &mut BTreeMap<CanonicalUuid, Option<wire::body::LanePark>>,
    ) {
        let Some(residency) = snapshot.residency() else {
            return;
        };
        for row in &residency.body().lanes {
            let wake = self.projection.inbox_wake(snapshot, row.lane);
            let ordinary_wake = wake.pending && !wake.reminder_only;
            if self.launching.contains(&row.lane) || ordinary_wake {
                if let Some(park) = parks.get_mut(&row.lane) {
                    *park = None;
                }
                continue;
            }
            if parks.contains_key(&row.lane) {
                continue;
            }
            let Some(park @ wire::body::LanePark::Job { until }) =
                snapshot.facts().lane_parked(row.lane)
            else {
                continue;
            };
            if until > now
                && crate::resident::supervisor::terminal::Jobs::for_lane(paths, row.lane)
                    .finite_horizon()
                    .is_some()
            {
                parks.insert(row.lane, Some(park));
            }
        }
    }

    /// Journal each current lane whose park changed since the last park fact,
    /// so the lane read and every subscriber see a park begin or end at the
    /// decision instead of at the next liveness change. A refused start parks
    /// its lane until the lane's next command releases the backoff. A park
    /// that fails to commit is retried by the next reconciliation.
    pub(super) fn journal_parks(
        &self,
        bus: &Bus,
        epoch: CanonicalUuid,
        snapshot: &bus::TaskSnapshot,
        parks: &BTreeMap<CanonicalUuid, Option<wire::body::LanePark>>,
        diagnostic: &mut dyn std::io::Write,
    ) {
        let Some(residency) = snapshot.residency() else {
            return;
        };
        let changes = residency
            .body()
            .lanes
            .iter()
            .filter_map(|row| {
                let park = parks.get(&row.lane).and_then(|park| {
                    if self.backoff.parked.contains_key(&row.lane) {
                        Some(wire::body::LanePark::Refused)
                    } else {
                        *park
                    }
                });
                (snapshot.facts().lane_parked(row.lane) != park).then_some((row.lane, park))
            })
            .collect::<Vec<_>>();
        if changes.is_empty() {
            return;
        }
        if let Err(error) = commit_parks(bus, epoch, changes) {
            let _ = writeln!(diagnostic, "lifecycle park-unrecorded error={error:?}");
        }
    }
}

/// Journal one lifecycle park transition per changed lane, guarded on the
/// residency the parks were decided against.
fn commit_parks(
    bus: &Bus,
    epoch: CanonicalUuid,
    changes: Vec<(CanonicalUuid, Option<wire::body::LanePark>)>,
) -> CommandResult<()> {
    let observed = current_residency(bus, epoch)?;
    let mut proposal = write(Actor::Spice, epoch, Producer::BusTimer)?;
    guard_residency(&mut proposal, &observed);
    for (lane, park) in changes {
        match park {
            Some(park) => proposal.fact(wire::body::LaneParked { lane, park }, FactCause::Root),
            None => proposal.fact(wire::body::LaneUnparked { lane }, FactCause::Root),
        }
        .map_err(|error| store_fault("lifecycle::park-fact-invalid", &error.to_string()))?;
    }
    commit(bus, proposal).map(|_| ())
}
