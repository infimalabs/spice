//! One supervised process group per occupied seat.
//!
//! An occupant is a process group, not a process: the seat owns everything the
//! agent spawns beneath it, so ending an occupant ends its children too. A
//! detached supervisor owns the group and outlives the resident holding this
//! seat handle. The law here is arithmetic: a seat holds one occupant or none,
//! and a second seating is refused without replacing the first.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::time::{Duration, Instant as Monotonic};

use journal::crossing::CanonicalUuid;
use sill::{WaitError, WaitOutcome};

/// Non-owning resident handle to a process owned by a detached supervisor.
///
/// Dropping this handle must never signal the process. Explicit lane lifecycle
/// operations call the signal methods; resident replacement only drops the
/// handle and lets the supervisor continue.
pub trait DetachedProcess: Send + 'static {
    /// Agent process-group identity owned by the supervisor.
    fn group(&self) -> u32;

    /// Grace after an explicit lane termination request.
    fn termination_grace(&self) -> Duration;

    /// Reap bound after an explicit lane kill request.
    fn reap_bound(&self) -> Duration;

    /// Passively observe the supervisor-owned process for a bounded interval.
    fn wait_for(self: Box<Self>, duration: Duration) -> Result<DetachedWait, WaitError>;

    /// Explicitly request graceful termination of the agent process group.
    fn terminate(&self) -> Result<(), WaitError>;

    /// Explicitly force termination of the agent process group.
    fn kill(&self) -> Result<(), WaitError>;

    /// Wait until the supervisor reports the process's exact ending.
    fn wait(self: Box<Self>) -> Result<WaitOutcome, WaitError>;
}

/// One bounded passive observation of a detached supervisor handle.
pub enum DetachedWait {
    /// The supervisor published the process's terminal status.
    Exited(WaitOutcome),
    /// The bound elapsed while the process remained live.
    Running(Box<dyn DetachedProcess>),
}

/// One process group supervised under a lane.
pub struct Occupant {
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    process: Box<dyn DetachedProcess>,
}

/// Latest liveness baseline for one supervised occupant.
///
/// Seating establishes the first baseline; each admitted selected record moves
/// it forward for the exact same occupant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Activity {
    occupant: CanonicalUuid,
    at: Monotonic,
}

impl Activity {
    /// Build one activity observation at its admitted monotonic instant.
    #[must_use]
    pub const fn new(occupant: CanonicalUuid, at: Monotonic) -> Self {
        Self { occupant, at }
    }

    /// Occupant whose selected record was admitted.
    #[must_use]
    pub const fn occupant(self) -> CanonicalUuid {
        self.occupant
    }

    /// Monotonic admission instant.
    #[must_use]
    pub const fn at(self) -> Monotonic {
        self.at
    }
}

impl Occupant {
    /// The lane this occupant sits in.
    #[must_use]
    pub const fn lane(&self) -> CanonicalUuid {
        self.lane
    }

    /// The occupant's identity, which outlives the process it names.
    #[must_use]
    pub const fn occupant(&self) -> CanonicalUuid {
        self.occupant
    }

    /// The process group the door opened.
    #[must_use]
    pub fn group(&self) -> u32 {
        self.process.group()
    }

    /// Configured grace after a termination signal.
    #[must_use]
    pub fn termination_grace(&self) -> Duration {
        self.process.termination_grace()
    }

    /// Configured reap bound after forced termination.
    #[must_use]
    pub fn reap_bound(&self) -> Duration {
        self.process.reap_bound()
    }

    /// Passively waits for this group to exit within `duration`.
    ///
    /// # Errors
    ///
    /// Returns the SILL wait failure with this occupant's owner evidence.
    pub fn wait_for(self, duration: Duration) -> Result<OccupantWait, WaitError> {
        let Self {
            lane,
            occupant,
            process,
        } = self;
        match process.wait_for(duration)? {
            DetachedWait::Exited(outcome) => Ok(OccupantWait::Exited {
                lane,
                occupant,
                outcome,
            }),
            DetachedWait::Running(process) => Ok(OccupantWait::Running(Box::new(Self {
                lane,
                occupant,
                process,
            }))),
        }
    }

    /// Requests graceful termination of the whole process group.
    ///
    /// # Errors
    ///
    /// Returns the SILL signalling failure with this occupant's owner evidence.
    pub fn terminate(&self) -> Result<(), WaitError> {
        self.process.terminate()
    }

    /// Forces termination of the whole process group.
    ///
    /// # Errors
    ///
    /// Returns the SILL signalling failure with this occupant's owner evidence.
    pub fn kill(&self) -> Result<(), WaitError> {
        self.process.kill()
    }

    /// Waits for the process group to end.
    ///
    /// # Errors
    ///
    /// Returns [`WaitError`] as the sill door reports it.
    pub fn wait(self) -> Result<WaitOutcome, WaitError> {
        self.process.wait()
    }
}

/// Result of passively observing one occupied seat for a bounded interval.
#[derive(Debug)]
pub enum OccupantWait {
    /// The group exited and was reaped within the interval.
    Exited {
        /// Lane whose group ended.
        lane: CanonicalUuid,
        /// Occupant whose group ended.
        occupant: CanonicalUuid,
        /// Complete bounded process evidence.
        outcome: WaitOutcome,
    },
    /// The interval elapsed and the seat remains occupied.
    Running(Box<Occupant>),
}

impl fmt::Debug for Occupant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Occupant")
            .field("lane", &self.lane)
            .field("occupant", &self.occupant)
            .field("group", &self.group())
            .finish()
    }
}

/// Why a lane did not take an occupant.
#[derive(Debug)]
pub enum SeatError {
    /// A process group already sits in the seat.
    Occupied {
        /// Lane that is already occupied.
        lane: CanonicalUuid,
        /// Occupant already sitting in it.
        sitting: CanonicalUuid,
    },
}

impl fmt::Display for SeatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Occupied { lane, sitting } => write!(
                formatter,
                "lane {lane} is already occupied by {sitting}; a seat holds one process group"
            ),
        }
    }
}

impl Error for SeatError {}

/// Every occupied seat the supervisor holds.
#[derive(Debug, Default)]
pub struct Seats {
    occupied: BTreeMap<CanonicalUuid, Occupant>,
    started: BTreeMap<CanonicalUuid, Activity>,
    activity: BTreeMap<CanonicalUuid, Activity>,
    vacated: Vec<(CanonicalUuid, CanonicalUuid)>,
}

impl Seats {
    /// A supervisor holding no seats.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            occupied: BTreeMap::new(),
            started: BTreeMap::new(),
            activity: BTreeMap::new(),
            vacated: Vec::new(),
        }
    }

    /// The occupant sitting in a lane, if one does.
    #[must_use]
    pub fn occupant(&self, lane: CanonicalUuid) -> Option<&Occupant> {
        self.occupied.get(&lane)
    }

    /// Whether a process group sits in this lane.
    #[must_use]
    pub fn occupied(&self, lane: CanonicalUuid) -> bool {
        self.occupied.contains_key(&lane)
    }

    /// Latest seating or selected-record activity for the supervised occupant.
    #[must_use]
    pub fn activity(&self, lane: CanonicalUuid) -> Option<Activity> {
        self.activity.get(&lane).copied()
    }

    /// Immutable seating instant for the supervised occupant.
    #[must_use]
    pub fn started(&self, lane: CanonicalUuid) -> Option<Activity> {
        self.started.get(&lane).copied()
    }

    /// Advance activity with one selected record from the exact supervised occupant.
    ///
    /// Returns `false` for a stale report naming a vacant or succeeded seat.
    pub fn observe_activity(
        &mut self,
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
        at: Monotonic,
    ) -> bool {
        if !self
            .occupied
            .get(&lane)
            .is_some_and(|sitting| sitting.occupant == occupant)
        {
            return false;
        }
        self.activity.insert(lane, Activity::new(occupant, at));
        true
    }

    /// Every occupied lane, ordered by lane.
    #[must_use]
    pub fn lanes(&self) -> Vec<CanonicalUuid> {
        self.occupied.keys().copied().collect()
    }

    /// How many seats are occupied.
    #[must_use]
    pub fn len(&self) -> usize {
        self.occupied.len()
    }

    /// Whether no seat is occupied.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.occupied.is_empty()
    }

    /// Admit a process already owned by a detached lane supervisor.
    ///
    /// Dropping the returned resident seat never signals the process; only an
    /// explicit lane lifecycle operation reaches the supervisor handle.
    ///
    /// # Errors
    ///
    /// Returns [`SeatError::Occupied`] when the lane already has an occupant.
    pub fn seat_detached(
        &mut self,
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
        process: Box<dyn DetachedProcess>,
    ) -> Result<&Occupant, SeatError> {
        if let Some(sitting) = self.occupied.get(&lane) {
            return Err(SeatError::Occupied {
                lane,
                sitting: sitting.occupant,
            });
        }
        let occupant = self.occupied.entry(lane).or_insert(Occupant {
            lane,
            occupant,
            process,
        });
        let seated = Activity::new(occupant.occupant, Monotonic::now());
        self.started.insert(lane, seated);
        self.activity.insert(lane, seated);
        Ok(occupant)
    }

    pub fn vacate(&mut self, lane: CanonicalUuid) -> Option<Occupant> {
        self.started.remove(&lane);
        self.activity.remove(&lane);
        let occupant = self.occupied.remove(&lane)?;
        self.vacated.push((lane, occupant.occupant()));
        Some(occupant)
    }

    /// Whether a closing sweep still owes at least one vacated occupant.
    #[must_use]
    pub fn has_vacated(&self) -> bool {
        !self.vacated.is_empty()
    }

    /// Every `(lane, occupant)` vacated since the last take, in vacate order.
    ///
    /// A seat closes moments after its occupant wrote its last records, and
    /// the notices for those records may still be unread when the exit is
    /// accounted. The ledger lets the coordinator sweep each closed tape once
    /// the turn that closed it is over, whichever path closed it.
    pub fn take_vacated(&mut self) -> Vec<(CanonicalUuid, CanonicalUuid)> {
        std::mem::take(&mut self.vacated)
    }

    /// Release every resident handle without signalling any process.
    ///
    /// Detached supervisors need only have their lightweight handles dropped.
    /// This method is used only when the coordinator itself is going away;
    /// explicit lane drain continues through [`Self::vacate`] and the normal
    /// signal sequence.
    pub fn release_all(&mut self) {
        self.occupied.clear();
        self.started.clear();
        self.activity.clear();
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::time::Duration;

    use super::{DetachedProcess, DetachedWait, SeatError, Seats};
    use journal::crossing::CanonicalUuid;
    use sill::{WaitError, WaitOutcome};

    fn uuid(last: u8) -> CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131163{last:02}")
            .parse()
            .expect("fixture uuid")
    }

    /// A detached handle that never touches a process, naming its group.
    struct Detached(u32);

    impl DetachedProcess for Detached {
        fn group(&self) -> u32 {
            self.0
        }

        fn termination_grace(&self) -> Duration {
            Duration::from_secs(1)
        }

        fn reap_bound(&self) -> Duration {
            Duration::from_secs(1)
        }

        fn wait_for(self: Box<Self>, _: Duration) -> Result<DetachedWait, WaitError> {
            unreachable!("vacating a seat waits on nothing")
        }

        fn terminate(&self) -> Result<(), WaitError> {
            unreachable!("vacating a seat signals nothing")
        }

        fn kill(&self) -> Result<(), WaitError> {
            unreachable!("vacating a seat signals nothing")
        }

        fn wait(self: Box<Self>) -> Result<WaitOutcome, WaitError> {
            unreachable!("vacating a seat waits on nothing")
        }
    }

    /// Whichever path vacates a seat, the ledger names the occupant once.
    #[test]
    fn vacating_a_seat_records_its_occupant_until_taken() {
        let mut seats = Seats::new();
        seats
            .seat_detached(uuid(1), uuid(3), Box::new(Detached(1)))
            .expect("the seat takes a detached handle");
        seats
            .seat_detached(uuid(2), uuid(4), Box::new(Detached(2)))
            .expect("the second seat takes a detached handle");
        assert!(seats.take_vacated().is_empty());

        drop(seats.vacate(uuid(2)));
        drop(seats.vacate(uuid(1)));
        assert!(seats.vacate(uuid(1)).is_none());

        assert_eq!(
            seats.take_vacated(),
            vec![(uuid(2), uuid(4)), (uuid(1), uuid(3))]
        );
        assert!(seats.take_vacated().is_empty());
    }

    /// An empty seat takes one group, and the seat names what sits in it.
    #[test]
    fn an_empty_seat_takes_one_supervised_group() {
        let mut seats = Seats::new();

        let occupant = seats
            .seat_detached(uuid(1), uuid(3), Box::new(Detached(7)))
            .expect("the seat takes a group");
        assert_eq!(occupant.lane(), uuid(1));
        assert_eq!(occupant.occupant(), uuid(3));
        assert_eq!(occupant.group(), 7);

        assert!(seats.occupied(uuid(1)));
        assert_eq!(seats.len(), 1);
        assert_eq!(seats.lanes(), vec![uuid(1)]);
        let started = seats.started(uuid(1)).expect("seating is timed");
        let selected = started.at() + Duration::from_secs(1);
        assert!(seats.observe_activity(uuid(1), uuid(3), selected));
        assert_eq!(
            seats.started(uuid(1)),
            Some(started),
            "selected records must not rewrite the occupant lifetime baseline",
        );
        assert_eq!(
            seats.activity(uuid(1)).expect("activity is timed").at(),
            selected,
        );

        assert_eq!(
            seats
                .vacate(uuid(1))
                .expect("the seat was occupied")
                .group(),
            7
        );
        assert!(seats.is_empty());
        assert_eq!(seats.started(uuid(1)), None);
        assert_eq!(seats.activity(uuid(1)), None);
    }

    /// A second seating is refused and leaves the first group in place.
    #[test]
    fn an_occupied_seat_refuses_a_second_group() {
        let mut seats = Seats::new();
        seats
            .seat_detached(uuid(1), uuid(3), Box::new(Detached(7)))
            .expect("the seat takes a group");

        let error = seats
            .seat_detached(uuid(1), uuid(4), Box::new(Detached(8)))
            .expect_err("an occupied seat takes no second group");

        assert!(
            matches!(
                error,
                SeatError::Occupied {
                    lane,
                    sitting,
                } if lane == uuid(1) && sitting == uuid(3)
            ),
            "unexpected refusal: {error}",
        );
        assert_eq!(
            seats.occupant(uuid(1)).expect("still occupied").group(),
            7,
            "the refused seating replaced the supervised group",
        );
        assert_eq!(seats.len(), 1);
    }

    /// Vacating a seat that holds nothing hands back nothing.
    #[test]
    fn vacating_an_empty_seat_hands_back_nothing() {
        let mut seats = Seats::new();

        assert!(seats.vacate(uuid(1)).is_none());
        assert!(!seats.occupied(uuid(1)));
    }
}
