//! Lane identity, worktree, occupant, and epoch, reduced from facts.
//!
//! The table is the durable half of a seat. A lane is created once with the
//! capability and worktree it stays bound to; occupants then arrive, end, and
//! are succeeded beneath that identity, each one recorded in the lineage so a
//! successor can always name what it descends from.
//!
//! Exactly one occupant sits in a seat at a time. That is the whole point of a
//! seat, so a start against an occupied lane is refused rather than overwriting
//! the occupant already there — two supervisors believing they own the same
//! lane is the failure this crate exists to prevent.
//!
//! Reduction is total and ordered: the same facts read in the same order always
//! produce the same table, and a fact naming a lane that was never seated is
//! refused instead of quietly creating one.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use journal::crossing::body::{
    LaneCancelled, LaneDrained, LaneExited, LaneImported, LaneKilled, LaneRemoved, LaneRenewed,
    LaneSeated, LaneSessionDeclared, LaneSignalled, LaneStarted, LaneTimedOut,
};
use journal::crossing::{
    CanonicalUuid, LaneResidency, LaneSession, ResidencyHead, ResidentState, TapeLocus,
};
use wire::Name;

/// The word a seat's disposition is reported under.
const OCCUPIED: &str = "occupied";
/// The word a seat between occupants is reported under.
const VACANT: &str = "vacant";

/// One lane's durable identity and current occupancy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seat {
    lane: CanonicalUuid,
    team: CanonicalUuid,
    capability: CanonicalUuid,
    name: Option<String>,
    worktree: String,
    life: String,
    vendor: Name,
    occupant: Option<CanonicalUuid>,
    lineage: Vec<CanonicalUuid>,
    session: Option<LaneSession>,
}

impl Seat {
    /// The lane this seat is.
    #[must_use]
    pub const fn lane(&self) -> CanonicalUuid {
        self.lane
    }

    /// The Canvas team whose lane contains this seat.
    #[must_use]
    pub const fn team(&self) -> CanonicalUuid {
        self.team
    }

    /// The capability minted with the lane.
    #[must_use]
    pub const fn capability(&self) -> CanonicalUuid {
        self.capability
    }

    /// Operator-configured agent display name, absent without an explicit override.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The worktree the lane is bound to.
    #[must_use]
    pub fn worktree(&self) -> &str {
        &self.worktree
    }

    /// The closed lane-life spelling selected for occupants of this seat.
    #[must_use]
    pub fn life(&self) -> &str {
        &self.life
    }

    /// The vendor selected for occupants of this seat.
    #[must_use]
    pub const fn vendor(&self) -> &Name {
        &self.vendor
    }

    /// The occupant sitting in the seat, absent between occupants.
    #[must_use]
    pub const fn occupant(&self) -> Option<CanonicalUuid> {
        self.occupant
    }

    /// Every occupant this seat has held, oldest first.
    #[must_use]
    pub fn lineage(&self) -> &[CanonicalUuid] {
        &self.lineage
    }

    /// Exact current resumable session authority.
    #[must_use]
    pub const fn session(&self) -> Option<&LaneSession> {
        self.session.as_ref()
    }

    /// Whether an occupant sits in the seat.
    #[must_use]
    pub const fn occupied(&self) -> bool {
        self.occupant.is_some()
    }

    /// This seat as the residency row a head carries.
    #[must_use]
    pub fn residency(&self) -> LaneResidency {
        LaneResidency {
            lane: self.lane,
            team: self.team,
            name: self.name.clone(),
            worktree: self.worktree.clone(),
            life: self.life.clone(),
            vendor: self.vendor.clone(),
            model: None,
            effort: None,
            fast_mode: false,
            occupant: self.occupant,
            lineage: self.lineage.clone(),
            capabilities: vec![self.capability],
            session: self.session.clone(),
            disposition: if self.occupied() { OCCUPIED } else { VACANT }.to_owned(),
        }
    }
}

/// One seating fact, borrowed from the journal that carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seating<'a> {
    /// A lane and its capability were created.
    Seated(&'a LaneSeated),
    /// A selected record declared the exact resumable session.
    Session(&'a LaneSessionDeclared),
    /// An occupant became live under the lane.
    Started(&'a LaneStarted),
    /// A vacant lineage seat moved to another linked worktree.
    Imported(&'a LaneImported),
    /// A vacant lane seat was removed from the topology.
    Removed(&'a LaneRemoved),
    /// The occupant ended.
    Exited(&'a LaneExited),
    /// The occupant exceeded its runtime.
    TimedOut(&'a LaneTimedOut),
    /// Supervision cancellation ended the occupant.
    Cancelled(&'a LaneCancelled),
    /// A signal outside resident termination ended the occupant.
    Signalled(&'a LaneSignalled),
    /// A successor occupant was seated with its ancestor named.
    Renewed(&'a LaneRenewed),
    /// A graceful occupant end completed.
    Drained(&'a LaneDrained),
    /// The resident ended an occupant that outlived its termination grace.
    Killed(&'a LaneKilled),
}

/// Why a fact did not reduce into the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The fact names a lane that was never seated.
    Unseated {
        /// Lane the fact names.
        lane: CanonicalUuid,
    },
    /// The lane was seated already, and identity is created once.
    Reseated {
        /// Lane the fact names.
        lane: CanonicalUuid,
    },
    /// An occupant start belongs to another resident generation.
    Stale {
        /// Epoch this table reduces under.
        epoch: CanonicalUuid,
        /// Epoch the start fact named.
        named: CanonicalUuid,
    },
    /// An occupant already sits in the seat.
    Occupied {
        /// Lane the fact names.
        lane: CanonicalUuid,
        /// Occupant already sitting in the seat.
        sitting: CanonicalUuid,
    },
    /// No occupant sits in the seat the fact ends or succeeds.
    Vacant {
        /// Lane the fact names.
        lane: CanonicalUuid,
    },
    /// The fact names an occupant other than the one sitting in the seat.
    Mismatched {
        /// Lane the fact names.
        lane: CanonicalUuid,
        /// Occupant sitting in the seat.
        sitting: CanonicalUuid,
        /// Occupant the fact named.
        named: CanonicalUuid,
    },
    /// A start or renewal had no preceding session declaration.
    Undeclared {
        /// Lane whose occupant lacked session authority.
        lane: CanonicalUuid,
    },
    /// A session declaration repeated for the same occupant.
    DuplicateSession {
        /// Lane whose declaration repeated.
        lane: CanonicalUuid,
        /// Occupant that already declared.
        occupant: CanonicalUuid,
    },
    /// A successor declared another identity instead of the durable one.
    ConflictingSession {
        /// Lane whose durable session conflicted.
        lane: CanonicalUuid,
        /// Durable identity.
        expected: String,
        /// Newly declared identity.
        observed: String,
    },
    /// A declaration named another worktree.
    WrongWorktree {
        /// Lane whose worktree is fixed.
        lane: CanonicalUuid,
        /// Durable worktree.
        expected: String,
        /// Declared worktree.
        observed: String,
    },
    /// A declaration named another vendor.
    CrossVendor {
        /// Lane whose vendor is fixed.
        lane: CanonicalUuid,
        /// Durable vendor.
        expected: Name,
        /// Declared vendor.
        observed: Name,
    },
    /// A start or renewal named a different occupant from its declaration.
    SessionOccupant {
        /// Lane whose session was declared.
        lane: CanonicalUuid,
        /// Occupant proven by the declaration.
        expected: CanonicalUuid,
        /// Occupant the lifecycle fact named.
        observed: CanonicalUuid,
    },
    /// An import had no retained occupant lineage to descend from.
    TeamMissing {
        /// Lane lacking its predecessor occupant.
        lane: CanonicalUuid,
    },
    /// An import named another predecessor than the retained lineage.
    Team {
        /// Lane being transferred.
        lane: CanonicalUuid,
        /// Latest occupant retained by the seat.
        expected: CanonicalUuid,
        /// Ancestor named by the transfer.
        observed: CanonicalUuid,
    },
    /// An import restated the seat's current worktree.
    UnmovedWorktree {
        /// Lane whose worktree did not move.
        lane: CanonicalUuid,
        /// Existing worktree.
        worktree: String,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unseated { lane } => write!(formatter, "lane {lane} was never seated"),
            Self::Reseated { lane } => write!(formatter, "lane {lane} is already seated"),
            Self::Stale { epoch, named } => {
                write!(formatter, "lane table epoch is {epoch}, not {named}")
            }
            Self::Occupied { lane, sitting } => {
                write!(formatter, "lane {lane} is already occupied by {sitting}")
            }
            Self::Vacant { lane } => write!(formatter, "lane {lane} has no occupant"),
            Self::Mismatched {
                lane,
                sitting,
                named,
            } => write!(
                formatter,
                "lane {lane} is occupied by {sitting}, not {named}"
            ),
            Self::Undeclared { lane } => {
                write!(formatter, "lane {lane} has no declared session authority")
            }
            Self::DuplicateSession { lane, occupant } => write!(
                formatter,
                "lane {lane} occupant {occupant} declared its session twice"
            ),
            Self::ConflictingSession {
                lane,
                expected,
                observed,
            } => write!(
                formatter,
                "lane {lane} session is {expected:?}, not {observed:?}"
            ),
            Self::WrongWorktree {
                lane,
                expected,
                observed,
            } => write!(
                formatter,
                "lane {lane} worktree is {expected:?}, not {observed:?}"
            ),
            Self::CrossVendor {
                lane,
                expected,
                observed,
            } => write!(
                formatter,
                "lane {lane} vendor is {expected}, not {observed}"
            ),
            Self::SessionOccupant {
                lane,
                expected,
                observed,
            } => write!(
                formatter,
                "lane {lane} session belongs to {expected}, not {observed}"
            ),
            Self::TeamMissing { lane } => {
                write!(formatter, "lane {lane} has no predecessor occupant lineage")
            }
            Self::Team {
                lane,
                expected,
                observed,
            } => write!(
                formatter,
                "lane {lane} latest occupant is {expected}, not ancestor {observed}"
            ),
            Self::UnmovedWorktree { lane, worktree } => {
                write!(formatter, "lane {lane} is already bound to {worktree:?}")
            }
        }
    }
}

impl Error for Refusal {}

/// Every seat in one resident generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    epoch: CanonicalUuid,
    seats: BTreeMap<CanonicalUuid, Seat>,
}

impl Table {
    /// An empty table for this resident generation.
    #[must_use]
    pub const fn new(epoch: CanonicalUuid) -> Self {
        Self {
            epoch,
            seats: BTreeMap::new(),
        }
    }

    /// The resident generation this table reduces under.
    #[must_use]
    pub const fn epoch(&self) -> CanonicalUuid {
        self.epoch
    }

    /// One lane's seat, if the lane was seated.
    #[must_use]
    pub fn seat(&self, lane: CanonicalUuid) -> Option<&Seat> {
        self.seats.get(&lane)
    }

    /// Every seat, ordered by lane.
    #[must_use]
    pub fn seats(&self) -> Vec<&Seat> {
        self.seats.values().collect()
    }

    /// Reduces one seating fact into the table.
    ///
    /// # Errors
    ///
    /// Returns [`Refusal`] naming the lane when the fact seats a lane twice,
    /// names a lane that was never seated, starts an occupant against an
    /// occupied seat, or ends an occupant the seat is not holding.
    pub fn absorb(&mut self, seating: Seating<'_>) -> Result<(), Refusal> {
        match seating {
            Seating::Seated(fact) => self.seat_lane(fact),
            Seating::Session(fact) => self.session(fact),
            Seating::Started(fact) => self.start(fact),
            Seating::Imported(fact) => self.import(fact),
            Seating::Removed(fact) => self.remove(fact),
            Seating::Renewed(fact) => self.renew(fact),
            Seating::Exited(fact) => self.end(fact.lane, fact.occupant),
            Seating::TimedOut(fact) => self.end(fact.lane, fact.occupant),
            Seating::Cancelled(fact) => self.end(fact.lane, fact.occupant),
            Seating::Signalled(fact) => self.end(fact.lane, fact.occupant),
            Seating::Drained(fact) => self.end(fact.lane, fact.occupant),
            Seating::Killed(fact) => self.end(fact.lane, fact.occupant),
        }
    }

    /// This table as the residency head the journal carries.
    #[must_use]
    pub fn head(&self, state: ResidentState) -> ResidencyHead {
        ResidencyHead {
            epoch: self.epoch,
            state,
            topology_revision: 0,
            lanes: self.seats.values().map(Seat::residency).collect(),
            subscriptions: Vec::new(),
            renewals: Vec::new(),
        }
    }

    /// Creates a lane's durable identity, once.
    fn seat_lane(&mut self, fact: &LaneSeated) -> Result<(), Refusal> {
        if self.seats.contains_key(&fact.lane) {
            return Err(Refusal::Reseated { lane: fact.lane });
        }
        self.seats.insert(
            fact.lane,
            Seat {
                lane: fact.lane,
                team: fact.team,
                capability: fact.capability,
                name: fact.name.clone(),
                worktree: fact.worktree.clone(),
                life: fact.life.clone(),
                vendor: fact.vendor.clone(),
                occupant: None,
                lineage: Vec::new(),
                session: None,
            },
        );
        Ok(())
    }

    /// Admits one exact declaration before its occupant lifecycle fact.
    fn session(&mut self, fact: &LaneSessionDeclared) -> Result<(), Refusal> {
        if fact.epoch != self.epoch {
            return Err(Refusal::Stale {
                epoch: self.epoch,
                named: fact.epoch,
            });
        }
        let lane = fact.lane;
        let seat = self
            .seats
            .get_mut(&lane)
            .ok_or(Refusal::Unseated { lane })?;
        if fact.worktree != seat.worktree {
            return Err(Refusal::WrongWorktree {
                lane,
                expected: seat.worktree.clone(),
                observed: fact.worktree.clone(),
            });
        }
        if fact.vendor != seat.vendor {
            return Err(Refusal::CrossVendor {
                lane,
                expected: seat.vendor.clone(),
                observed: fact.vendor.clone(),
            });
        }
        if let Some(current) = &seat.session {
            if current.occupant == fact.occupant {
                return Err(Refusal::DuplicateSession {
                    lane,
                    occupant: fact.occupant,
                });
            }
            if current.identity != fact.identity {
                return Err(Refusal::ConflictingSession {
                    lane,
                    expected: current.identity.clone(),
                    observed: fact.identity.clone(),
                });
            }
        }
        seat.session = Some(LaneSession {
            lane,
            occupant: fact.occupant,
            worktree: fact.worktree.clone(),
            vendor: fact.vendor.clone(),
            launch: fact.launch.clone(),
            epoch: fact.epoch,
            stream: fact.stream.clone(),
            identity: fact.identity.clone(),
            locus: TapeLocus {
                member: fact.member,
                offset: fact.offset,
                length: fact.length,
            },
        });
        Ok(())
    }

    /// Seats an occupant, which the seat must be empty to admit.
    fn start(&mut self, fact: &LaneStarted) -> Result<(), Refusal> {
        if fact.epoch != self.epoch {
            return Err(Refusal::Stale {
                epoch: self.epoch,
                named: fact.epoch,
            });
        }
        let lane = fact.lane;
        let seat = self
            .seats
            .get_mut(&lane)
            .ok_or(Refusal::Unseated { lane })?;
        let session = seat.session.as_ref().ok_or(Refusal::Undeclared { lane })?;
        if session.occupant != fact.occupant {
            return Err(Refusal::SessionOccupant {
                lane,
                expected: session.occupant,
                observed: fact.occupant,
            });
        }
        if let Some(sitting) = seat.occupant {
            return Err(Refusal::Occupied { lane, sitting });
        }
        seat.occupant = Some(fact.occupant);
        seat.lineage.push(fact.occupant);
        Ok(())
    }

    /// Move a vacant seat only after re-proving its retained lineage and session.
    fn import(&mut self, fact: &LaneImported) -> Result<(), Refusal> {
        if fact.epoch != self.epoch {
            return Err(Refusal::Stale {
                epoch: self.epoch,
                named: fact.epoch,
            });
        }
        let lane = fact.lane;
        let seat = self
            .seats
            .get_mut(&lane)
            .ok_or(Refusal::Unseated { lane })?;
        if let Some(sitting) = seat.occupant {
            return Err(Refusal::Occupied { lane, sitting });
        }
        if seat.worktree != fact.from_worktree {
            return Err(Refusal::WrongWorktree {
                lane,
                expected: seat.worktree.clone(),
                observed: fact.from_worktree.clone(),
            });
        }
        if fact.worktree == seat.worktree {
            return Err(Refusal::UnmovedWorktree {
                lane,
                worktree: seat.worktree.clone(),
            });
        }
        if fact.vendor != seat.vendor {
            return Err(Refusal::CrossVendor {
                lane,
                expected: seat.vendor.clone(),
                observed: fact.vendor.clone(),
            });
        }
        let ancestor = seat
            .lineage
            .last()
            .copied()
            .ok_or(Refusal::TeamMissing { lane })?;
        if ancestor != fact.ancestor {
            return Err(Refusal::Team {
                lane,
                expected: ancestor,
                observed: fact.ancestor,
            });
        }
        let session = seat.session.as_ref().ok_or(Refusal::Undeclared { lane })?;
        if session.occupant != ancestor {
            return Err(Refusal::SessionOccupant {
                lane,
                expected: ancestor,
                observed: session.occupant,
            });
        }
        if session.worktree != fact.from_worktree {
            return Err(Refusal::WrongWorktree {
                lane,
                expected: fact.from_worktree.clone(),
                observed: session.worktree.clone(),
            });
        }
        if session.identity != fact.identity {
            return Err(Refusal::ConflictingSession {
                lane,
                expected: session.identity.clone(),
                observed: fact.identity.clone(),
            });
        }
        seat.worktree.clone_from(&fact.worktree);
        Ok(())
    }

    /// Remove a vacant seat from the table.
    fn remove(&mut self, fact: &LaneRemoved) -> Result<(), Refusal> {
        let lane = fact.lane;
        let seat = self.seats.get(&lane).ok_or(Refusal::Unseated { lane })?;
        if let Some(sitting) = seat.occupant {
            return Err(Refusal::Occupied { lane, sitting });
        }
        if seat.team != fact.team {
            return Err(Refusal::Team {
                lane,
                expected: seat.team,
                observed: fact.team,
            });
        }
        if seat.worktree != fact.worktree {
            return Err(Refusal::WrongWorktree {
                lane,
                expected: seat.worktree.clone(),
                observed: fact.worktree.clone(),
            });
        }
        self.seats.remove(&lane);
        Ok(())
    }

    /// Succeeds one occupant with another, naming the ancestor it descends
    /// from, which must be the occupant the seat is actually holding.
    fn renew(&mut self, fact: &LaneRenewed) -> Result<(), Refusal> {
        if fact.epoch != self.epoch {
            return Err(Refusal::Stale {
                epoch: self.epoch,
                named: fact.epoch,
            });
        }
        let lane = fact.lane;
        let seat = self
            .seats
            .get_mut(&lane)
            .ok_or(Refusal::Unseated { lane })?;
        let session = seat.session.as_ref().ok_or(Refusal::Undeclared { lane })?;
        if session.occupant != fact.occupant {
            return Err(Refusal::SessionOccupant {
                lane,
                expected: session.occupant,
                observed: fact.occupant,
            });
        }
        let sitting = seat.occupant.ok_or(Refusal::Vacant { lane })?;
        if sitting != fact.ancestor {
            return Err(Refusal::Mismatched {
                lane,
                sitting,
                named: fact.ancestor,
            });
        }
        if sitting == fact.occupant {
            return Err(Refusal::Occupied { lane, sitting });
        }
        seat.occupant = Some(fact.occupant);
        seat.lineage.push(fact.occupant);
        Ok(())
    }

    /// Vacates the seat, which must be holding the occupant the fact names.
    fn end(&mut self, lane: CanonicalUuid, occupant: CanonicalUuid) -> Result<(), Refusal> {
        let seat = self
            .seats
            .get_mut(&lane)
            .ok_or(Refusal::Unseated { lane })?;
        let sitting = seat.occupant.ok_or(Refusal::Vacant { lane })?;
        if sitting != occupant {
            return Err(Refusal::Mismatched {
                lane,
                sitting,
                named: occupant,
            });
        }
        seat.occupant = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Refusal, Seating, Table};
    use journal::crossing::body::{
        LaneDrained, LaneExited, LaneImported, LaneKilled, LaneLaunchIdentity, LaneRemoved,
        LaneRenewed, LaneSeated, LaneSessionDeclared, LaneStarted,
    };
    use journal::crossing::{CanonicalUuid, ResidentState};

    fn uuid(last: u8) -> CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131163{last:02}")
            .parse()
            .expect("fixture uuid")
    }

    fn seated() -> LaneSeated {
        LaneSeated {
            lane: uuid(1),
            team: uuid(1),
            capability: uuid(2),
            name: Some("Ava".to_owned()),
            worktree: "/worktrees/lane-a".to_owned(),
            life: "drive".to_owned(),
            vendor: "omp".parse().expect("fixture vendor"),
            model: None,
            effort: None,
        }
    }

    fn table() -> Table {
        let mut table = Table::new(uuid(0));
        table
            .absorb(Seating::Seated(&seated()))
            .expect("a lane seats once");
        table
    }

    fn started(occupant: CanonicalUuid) -> LaneStarted {
        LaneStarted {
            lane: uuid(1),
            occupant,
            epoch: uuid(0),
            group: 123,
            fresh_session: true,
        }
    }

    fn declared(occupant: CanonicalUuid) -> LaneSessionDeclared {
        LaneSessionDeclared {
            lane: uuid(1),
            occupant,
            worktree: "/worktrees/lane-a".to_owned(),
            vendor: "omp".parse().expect("fixture vendor"),
            launch: LaneLaunchIdentity::new("sonnet", "high"),
            epoch: uuid(0),
            stream: "stdout".to_owned(),
            source: uuid(9),
            identity: "session-a".to_owned(),
            member: 0,
            offset: 0,
            length: 42,
        }
    }

    fn start(table: &mut Table, occupant: CanonicalUuid) {
        table
            .absorb(Seating::Session(&declared(occupant)))
            .expect("the selected record declares the session");
        table
            .absorb(Seating::Started(&started(occupant)))
            .expect("the declared occupant starts");
    }

    /// Identity, worktree, and capability survive the occupant that sat in them.
    #[test]
    fn a_seat_keeps_its_identity_across_occupants() {
        let mut table = table();
        start(&mut table, uuid(3));
        table
            .absorb(Seating::Exited(&LaneExited {
                lane: uuid(1),
                occupant: uuid(3),
                code: 0,
            }))
            .expect("the sitting occupant ends");

        let seat = table.seat(uuid(1)).expect("the lane was seated");
        assert_eq!(seat.name(), Some("Ava"));
        assert_eq!(seat.worktree(), "/worktrees/lane-a");
        assert_eq!(seat.capability(), uuid(2));
        assert_eq!(seat.occupant(), None);
        assert!(!seat.occupied());
        assert_eq!(seat.lineage(), [uuid(3)]);
    }

    #[test]
    fn import_moves_one_vacant_seat_only_after_reproving_its_session_team() {
        let mut table = table();
        start(&mut table, uuid(3));
        table
            .absorb(Seating::Drained(&LaneDrained {
                lane: uuid(1),
                occupant: uuid(3),
            }))
            .expect("predecessor drains");
        let imported = LaneImported {
            lane: uuid(1),
            ancestor: uuid(3),
            from_worktree: "/worktrees/lane-a".to_owned(),
            worktree: "/worktrees/lane-b".to_owned(),
            vendor: "omp".parse().expect("fixture vendor"),
            identity: "session-a".to_owned(),
            epoch: uuid(0),
        };

        table
            .absorb(Seating::Imported(&imported))
            .expect("vacant seat imports with exact retained authority");
        let mut successor = declared(uuid(4));
        successor.worktree = "/worktrees/lane-b".to_owned();
        table
            .absorb(Seating::Session(&successor))
            .expect("successor re-declares the retained session in the target worktree");
        table
            .absorb(Seating::Started(&started(uuid(4))))
            .expect("successor occupies the imported seat");

        let seat = table.seat(uuid(1)).expect("imported seat remains seated");
        assert_eq!(seat.name(), Some("Ava"));
        assert_eq!(seat.worktree(), "/worktrees/lane-b");
        assert_eq!(seat.occupant(), Some(uuid(4)));
        assert_eq!(seat.lineage(), [uuid(3), uuid(4)]);

        let mut stale = imported;
        stale.worktree = "/worktrees/lane-c".to_owned();
        assert!(matches!(
            table.absorb(Seating::Imported(&stale)),
            Err(Refusal::Occupied { .. })
        ));
    }

    #[test]
    fn removal_deletes_only_a_vacant_matching_seat() {
        let mut table = table();
        start(&mut table, uuid(3));
        let removed = LaneRemoved {
            lane: uuid(1),
            team: uuid(1),
            worktree: "/worktrees/lane-a".to_owned(),
        };

        assert_eq!(
            table.absorb(Seating::Removed(&removed)),
            Err(Refusal::Occupied {
                lane: uuid(1),
                sitting: uuid(3),
            })
        );
        table
            .absorb(Seating::Drained(&LaneDrained {
                lane: uuid(1),
                occupant: uuid(3),
            }))
            .expect("the lane becomes vacant");

        table
            .absorb(Seating::Removed(&removed))
            .expect("vacant matching seat removes");

        assert!(table.seat(uuid(1)).is_none());
        assert_eq!(
            table.absorb(Seating::Removed(&removed)),
            Err(Refusal::Unseated { lane: uuid(1) })
        );
    }

    /// One occupant per seat: a second start is refused and changes nothing.
    #[test]
    fn a_seat_holds_exactly_one_occupant() {
        let mut table = table();
        start(&mut table, uuid(3));
        table
            .absorb(Seating::Session(&declared(uuid(4))))
            .expect("the successor declares the same session");

        let refusal = table
            .absorb(Seating::Started(&started(uuid(4))))
            .expect_err("an occupied seat admits no second occupant");

        assert_eq!(
            refusal,
            Refusal::Occupied {
                lane: uuid(1),
                sitting: uuid(3),
            }
        );
        assert_eq!(
            refusal.to_string(),
            format!("lane {} is already occupied by {}", uuid(1), uuid(3))
        );
        assert_eq!(
            table.seat(uuid(1)).expect("seated").occupant(),
            Some(uuid(3))
        );
    }

    /// An old resident cannot seat an occupant in the current generation.
    #[test]
    fn a_stale_epoch_cannot_start_an_occupant() {
        let mut table = table();
        let stale = LaneStarted {
            lane: uuid(1),
            occupant: uuid(3),
            epoch: uuid(9),
            group: 123,
            fresh_session: true,
        };

        assert_eq!(
            table.absorb(Seating::Started(&stale)),
            Err(Refusal::Stale {
                epoch: uuid(0),
                named: uuid(9),
            })
        );
        let seat = table.seat(uuid(1)).expect("the lane remains seated");
        assert_eq!(seat.occupant(), None);
        assert!(seat.lineage().is_empty());
    }

    /// Session authority is single, exact, and bound before lifecycle advance.
    #[test]
    fn session_declarations_refuse_missing_duplicate_conflicting_and_moved_bindings() {
        let mut table = table();
        assert_eq!(
            table.absorb(Seating::Started(&started(uuid(3)))),
            Err(Refusal::Undeclared { lane: uuid(1) })
        );

        let first = declared(uuid(3));
        table
            .absorb(Seating::Session(&first))
            .expect("first declaration is admitted");
        assert_eq!(
            table.absorb(Seating::Started(&started(uuid(4)))),
            Err(Refusal::SessionOccupant {
                lane: uuid(1),
                expected: uuid(3),
                observed: uuid(4),
            })
        );
        assert_eq!(
            table.absorb(Seating::Session(&first)),
            Err(Refusal::DuplicateSession {
                lane: uuid(1),
                occupant: uuid(3),
            })
        );

        let mut conflict = declared(uuid(4));
        conflict.identity = "session-b".to_owned();
        assert!(matches!(
            table.absorb(Seating::Session(&conflict)),
            Err(Refusal::ConflictingSession { .. })
        ));

        let mut worktree = declared(uuid(4));
        worktree.worktree = "/worktrees/lane-b".to_owned();
        assert!(matches!(
            table.absorb(Seating::Session(&worktree)),
            Err(Refusal::WrongWorktree { .. })
        ));

        let mut vendor = declared(uuid(4));
        vendor.vendor = "other".parse().expect("fixture vendor");
        assert!(matches!(
            table.absorb(Seating::Session(&vendor)),
            Err(Refusal::CrossVendor { .. })
        ));

        let mut cross_lane = declared(uuid(4));
        cross_lane.lane = uuid(8);
        assert_eq!(
            table.absorb(Seating::Session(&cross_lane)),
            Err(Refusal::Unseated { lane: uuid(8) })
        );

        let mut stale = declared(uuid(4));
        stale.epoch = uuid(9);
        assert_eq!(
            table.absorb(Seating::Session(&stale)),
            Err(Refusal::Stale {
                epoch: uuid(0),
                named: uuid(9),
            })
        );
    }

    /// A successor names the occupant it descends from, and the team grows.
    #[test]
    fn renewal_succeeds_the_sitting_occupant_and_records_the_team() {
        let mut table = table();
        start(&mut table, uuid(3));
        table
            .absorb(Seating::Session(&declared(uuid(4))))
            .expect("the successor declares the same session");
        table
            .absorb(Seating::Renewed(&LaneRenewed {
                lane: uuid(1),
                occupant: uuid(4),
                ancestor: uuid(3),
                epoch: uuid(0),
                group: 124,
            }))
            .expect("the successor names the sitting occupant");

        let seat = table.seat(uuid(1)).expect("seated");
        assert_eq!(seat.occupant(), Some(uuid(4)));
        assert_eq!(seat.lineage(), [uuid(3), uuid(4)]);
    }

    /// A successor naming an ancestor the seat is not holding is refused.
    #[test]
    fn renewal_refuses_an_ancestor_the_seat_is_not_holding() {
        let mut table = table();
        start(&mut table, uuid(3));
        table
            .absorb(Seating::Session(&declared(uuid(5))))
            .expect("the successor declares the same session");

        let refusal = table
            .absorb(Seating::Renewed(&LaneRenewed {
                lane: uuid(1),
                occupant: uuid(5),
                ancestor: uuid(4),
                epoch: uuid(0),
                group: 125,
            }))
            .expect_err("the named ancestor is not sitting");

        assert_eq!(
            refusal,
            Refusal::Mismatched {
                lane: uuid(1),
                sitting: uuid(3),
                named: uuid(4),
            }
        );
        assert_eq!(
            table.seat(uuid(1)).expect("seated").lineage(),
            [uuid(3)],
            "a refused renewal still grew the team",
        );
    }

    /// Renewal introduces a successor rather than repeating the same occupant.
    #[test]
    fn renewal_refuses_the_sitting_occupant_as_its_own_successor() {
        let mut table = table();
        start(&mut table, uuid(3));

        assert_eq!(
            table.absorb(Seating::Renewed(&LaneRenewed {
                lane: uuid(1),
                occupant: uuid(3),
                ancestor: uuid(3),
                epoch: uuid(0),
                group: 126,
            })),
            Err(Refusal::Occupied {
                lane: uuid(1),
                sitting: uuid(3),
            })
        );
        assert_eq!(table.seat(uuid(1)).expect("seated").lineage(), [uuid(3)]);
    }

    /// Ending an occupant the seat is not holding is refused by name.
    #[test]
    fn ending_refuses_an_occupant_the_seat_is_not_holding() {
        let mut table = table();
        start(&mut table, uuid(3));

        assert_eq!(
            table.absorb(Seating::Drained(&LaneDrained {
                lane: uuid(1),
                occupant: uuid(9),
            })),
            Err(Refusal::Mismatched {
                lane: uuid(1),
                sitting: uuid(3),
                named: uuid(9),
            })
        );
        assert_eq!(
            table.seat(uuid(1)).expect("seated").occupant(),
            Some(uuid(3))
        );
    }

    /// A kill ends the occupancy the same way the other two endings do.
    ///
    /// The table reduces three endings, and a kill has to be one of them rather
    /// than a fact the seat ignores: an occupant the resident had to kill is
    /// exactly the one least likely to have tidied up after itself, so a seat
    /// left occupied here would strand the lane that most needs re-seating.
    ///
    /// The second absorb is what proves it. A seat that merely looked vacant
    /// would still accept a second ending; refusing it shows the first one went
    /// through `end` rather than being dropped on the way in.
    #[test]
    fn a_killed_occupant_vacates_its_seat() {
        let mut table = table();
        start(&mut table, uuid(3));
        let killed = LaneKilled {
            lane: uuid(1),
            occupant: uuid(3),
        };

        table
            .absorb(Seating::Killed(&killed))
            .expect("the resident's kill ends the sitting occupant");

        let seat = table.seat(uuid(1)).expect("the lane was seated");
        assert_eq!(seat.occupant(), None);
        assert!(!seat.occupied());
        assert_eq!(seat.lineage(), [uuid(3)]);
        assert_eq!(
            table.absorb(Seating::Killed(&killed)),
            Err(Refusal::Vacant { lane: uuid(1) })
        );
    }

    /// Ending an already-empty seat is refused rather than doing nothing.
    #[test]
    fn ending_an_empty_seat_is_refused() {
        let mut table = table();

        assert_eq!(
            table.absorb(Seating::Exited(&LaneExited {
                lane: uuid(1),
                occupant: uuid(3),
                code: 0,
            })),
            Err(Refusal::Vacant { lane: uuid(1) })
        );
    }

    /// A fact naming a lane that was never seated does not create one.
    #[test]
    fn a_fact_for_an_unseated_lane_is_refused() {
        let mut table = Table::new(uuid(0));

        assert_eq!(
            table.absorb(Seating::Started(&started(uuid(3)))),
            Err(Refusal::Unseated { lane: uuid(1) })
        );
        assert!(table.seats().is_empty());
    }

    /// Identity is created once, so seating a lane twice is refused.
    #[test]
    fn seating_a_lane_twice_is_refused() {
        let mut table = table();

        assert_eq!(
            table.absorb(Seating::Seated(&seated())),
            Err(Refusal::Reseated { lane: uuid(1) })
        );
    }

    /// The table renders the residency head the journal carries.
    #[test]
    fn the_table_renders_the_residency_head() {
        let mut table = table();
        start(&mut table, uuid(3));

        let head = table.head(ResidentState::Seated);

        assert_eq!(head.epoch, uuid(0));
        assert_eq!(head.state, ResidentState::Seated);
        assert_eq!(head.lanes.len(), 1);
        let row = &head.lanes[0];
        assert_eq!(row.lane, uuid(1));
        assert_eq!(row.team, uuid(1));
        assert_eq!(row.name.as_deref(), Some("Ava"));
        assert_eq!(row.occupant, Some(uuid(3)));
        assert_eq!(row.lineage, vec![uuid(3)]);
        assert_eq!(row.capabilities, vec![uuid(2)]);
        assert_eq!(
            row.session
                .as_ref()
                .map(|session| session.identity.as_str()),
            Some("session-a")
        );
        assert_eq!(row.disposition, "occupied");
    }

    /// A seat between occupants reports itself vacant, not occupied by nobody.
    #[test]
    fn a_vacant_seat_is_reported_as_vacant() {
        let table = table();

        let head = table.head(ResidentState::Quiescing);

        assert_eq!(head.lanes[0].disposition, "vacant");
        assert_eq!(head.lanes[0].occupant, None);
        assert_eq!(head.state, ResidentState::Quiescing);
    }
}
