use std::time::Duration;

use bus::Bus;
use journal::crossing::{FactKind, Seq};
use tokio::time::Instant;

use super::super::{CommandResult, store_fault};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Cursor and process-local clock for quota refresh requests.
pub(in crate::resident) struct Triggers {
    cursor: Option<Seq>,
    periodic_at: Instant,
}

impl Triggers {
    pub(in crate::resident) fn start(bus: &Bus) -> CommandResult<Self> {
        let snapshot = task_snapshot(bus)?;
        Ok(Self {
            cursor: snapshot.cursor(),
            periodic_at: Instant::now() + SAMPLE_INTERVAL,
        })
    }

    pub(in crate::resident) async fn wait_periodic(&self) {
        tokio::time::sleep_until(self.periodic_at).await;
    }

    pub(in crate::resident) fn accept_periodic(&mut self) {
        self.periodic_at = Instant::now() + SAMPLE_INTERVAL;
    }

    /// Advance through every retained fact since the last admitted barrier.
    ///
    /// Reading the current shared snapshot after a durable notification also
    /// catches up a lagged broadcast and folds a multi-fact commit to one bit.
    pub(in crate::resident) fn accept_commits(&mut self, bus: &Bus) -> CommandResult<bool> {
        let snapshot = task_snapshot(bus)?;
        let requested = snapshot
            .facts()
            .after(self.cursor)
            .any(|envelope| is_boundary(envelope.fact.kind()));
        self.cursor = snapshot.cursor();
        Ok(requested)
    }
}

fn task_snapshot(bus: &Bus) -> CommandResult<bus::TaskSnapshot> {
    bus.task_snapshot()
        .map_err(|error| store_fault("quota::snapshot-unreadable", &error.to_string()))
}

const fn is_boundary(kind: FactKind) -> bool {
    matches!(
        kind,
        FactKind::ClaimWon
            | FactKind::ClaimCarried
            | FactKind::ClaimRebound
            | FactKind::ClaimFreed
            | FactKind::ClaimLapsed
            | FactKind::TaskAssigned
            | FactKind::TaskPhased
            | FactKind::TaskDone
            | FactKind::TaskReviewed
            | FactKind::LaneStarted
            | FactKind::LaneExited
            | FactKind::LaneTimedOut
            | FactKind::LaneCancelled
            | FactKind::LaneSignalled
            | FactKind::LaneDrained
            | FactKind::LaneKilled
            | FactKind::LaneDegraded
            | FactKind::LaneCompacted
    )
}

#[cfg(test)]
mod tests {
    use journal::crossing::{Actor, CanonicalUuid, Producer};
    use journal::{FactCause, JournalWrite};
    use wire::body::{LaneCompacted, LaneDegraded, LaneRenewed};

    use super::*;

    #[test]
    fn semantic_boundary_allowlist_is_exact() {
        let expected = [
            FactKind::ClaimWon,
            FactKind::ClaimCarried,
            FactKind::ClaimRebound,
            FactKind::ClaimFreed,
            FactKind::ClaimLapsed,
            FactKind::TaskAssigned,
            FactKind::TaskPhased,
            FactKind::TaskDone,
            FactKind::TaskReviewed,
            FactKind::LaneStarted,
            FactKind::LaneExited,
            FactKind::LaneTimedOut,
            FactKind::LaneCancelled,
            FactKind::LaneSignalled,
            FactKind::LaneDrained,
            FactKind::LaneKilled,
            FactKind::LaneDegraded,
            FactKind::LaneCompacted,
        ];
        for kind in FactKind::ALL {
            assert_eq!(is_boundary(*kind), expected.contains(kind), "{kind:?}");
        }
    }

    #[test]
    fn startup_skips_history_and_one_scan_coalesces_commits_and_lag() {
        let epoch = uuid(1);
        let mut journal = journal::Journal::in_memory().expect("quota trigger journal opens");
        journal
            .apply(write(epoch, [degraded(2, "historical")]))
            .expect("historical boundary commits");
        let bus = Bus::start(journal).expect("quota trigger bus starts");
        let mut triggers = Triggers::start(&bus).expect("quota triggers start");

        assert!(!triggers.accept_commits(&bus).expect("startup cursor scans"));
        commit(&bus, write(epoch, [renewed(3), renewed(4)]));
        assert!(!triggers.accept_commits(&bus).expect("renewals scan"));

        let mut boundaries = resident_write(epoch);
        boundaries
            .fact(compacted(5), FactCause::Root)
            .expect("compaction fact builds");
        boundaries
            .fact(degraded(6, "terminal"), FactCause::Root)
            .expect("degradation fact builds");
        commit(&bus, boundaries);
        assert!(triggers.accept_commits(&bus).expect("boundary burst scans"));
        assert!(!triggers.accept_commits(&bus).expect("settled cursor scans"));

        commit(&bus, write(epoch, [renewed(7)]));
        commit(&bus, write(epoch, [degraded(8, "lagged")]));
        commit(&bus, write(epoch, [renewed(9)]));
        assert!(triggers.accept_commits(&bus).expect("lagged commits scan"));
        assert!(
            !triggers
                .accept_commits(&bus)
                .expect("lagged cursor settles")
        );
        bus.shutdown().expect("quota trigger bus stops");
    }

    fn write<T, const N: usize>(epoch: CanonicalUuid, facts: [T; N]) -> JournalWrite
    where
        T: wire::Body,
    {
        let mut write = super::super::super::write(Actor::Spice, epoch, T::KIND.producer())
            .expect("quota trigger write starts");
        for fact in facts {
            write
                .fact(fact, FactCause::Root)
                .expect("quota trigger fact builds");
        }
        write
    }

    fn resident_write(epoch: CanonicalUuid) -> JournalWrite {
        super::super::super::write(Actor::Spice, epoch, Producer::ResidentRecognition)
            .expect("quota trigger write starts")
    }

    fn commit(bus: &Bus, write: JournalWrite) {
        super::super::super::commit(bus, write).expect("quota trigger write commits");
    }

    fn degraded(seed: u8, marker: &str) -> LaneDegraded {
        LaneDegraded {
            lane: uuid(seed),
            marker: marker.to_owned(),
        }
    }

    fn compacted(seed: u8) -> LaneCompacted {
        LaneCompacted {
            lane: uuid(seed),
            occupant: uuid(seed + 30),
            stream: "session:fixture".to_owned(),
            member: 1,
            offset: 2,
            length: 3,
        }
    }

    fn renewed(seed: u8) -> LaneRenewed {
        LaneRenewed {
            lane: uuid(seed),
            occupant: uuid(seed + 30),
            ancestor: uuid(seed + 60),
            epoch: uuid(seed + 90),
            group: u32::from(seed),
        }
    }

    fn uuid(last: u8) -> CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131163{last:02}")
            .parse()
            .expect("fixture UUID")
    }
}
