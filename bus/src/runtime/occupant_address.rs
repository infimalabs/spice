//! Exact-occupant peer addresses proved against journal birth facts.
//!
//! An occupant's address is the journal position of the one `lane::started`
//! fact naming it. Proof reads that exact position and the writer's derived
//! birth index at one barrier; execution additionally requires the current
//! residency to seat the same occupant on its birth lane.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::mpsc;

use journal::crossing::{
    CanonicalUuid, FactKind, Instant, LaneResidency, OccupantAddress, ResidencyHead, SeatAddress,
    Seq,
};
use journal::{FactPageStart, FactReadBudget, GuardError, Journal, JournalRead};

use super::task_snapshot::TaskSnapshotState;
use super::{Bus, BusError, BusReadError, BusReader, Request, TaskSnapshot, current_task_snapshot};

/// One address read: prove its birth, or also require that birth's seat.
#[derive(Clone, Copy)]
pub(super) enum AddressRead {
    Birth(OccupantAddress),
    Seat(OccupantAddress),
}

impl BusReader {
    /// Prove the occupant birth one address names, whether or not that
    /// occupant is still seated.
    pub fn occupant_birth(&self, address: OccupantAddress) -> Result<AddressProof, BusReadError> {
        self.occupant(AddressRead::Birth(address))
    }

    /// Resolve one address to the exact occupant the current residency seats
    /// on its birth lane; a departed occupant's address is expired.
    pub fn resolve_occupant(&self, address: OccupantAddress) -> Result<AddressProof, BusReadError> {
        self.occupant(AddressRead::Seat(address))
    }

    fn occupant(&self, read: AddressRead) -> Result<AddressProof, BusReadError> {
        let (answer, receiver) = mpsc::sync_channel(1);
        self.requests
            .send(Request::Occupant { read, answer })
            .map_err(|_| BusReadError::Bus(BusError::Closed))?;
        receiver
            .recv()
            .map_err(|_| BusReadError::Bus(BusError::Closed))?
    }
}

impl Bus {
    /// Prove the occupant birth one address names; see [`BusReader::occupant_birth`].
    pub fn occupant_birth(&self, address: OccupantAddress) -> Result<AddressProof, BusReadError> {
        self.reader().occupant_birth(address)
    }

    /// Resolve one executable address; see [`BusReader::resolve_occupant`].
    pub fn resolve_occupant(&self, address: OccupantAddress) -> Result<AddressProof, BusReadError> {
        self.reader().resolve_occupant(address)
    }
}

/// Answer one queued address read at the writer's current snapshot barrier.
pub(super) fn answer(
    journal: &Journal,
    state: &TaskSnapshotState,
    read: AddressRead,
) -> Result<AddressProof, BusReadError> {
    current_task_snapshot(state).and_then(|snapshot| {
        journal
            .read(|journal| match read {
                AddressRead::Birth(address) => prove_birth(journal, &snapshot, address),
                AddressRead::Seat(address) => resolve(journal, &snapshot, address),
            })
            .map_err(BusReadError::Head)
    })
}

/// The births one occupant identity has accumulated in the journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BirthRecord {
    /// Exactly one `lane::started` fact names the occupant.
    Unique { lane: CanonicalUuid, position: Seq },
    /// More than one `lane::started` fact names the occupant.
    Repeated,
}

impl BirthRecord {
    pub(super) const fn recorded(
        previous: Option<Self>,
        lane: CanonicalUuid,
        position: Seq,
    ) -> Self {
        match previous {
            None => Self::Unique { lane, position },
            Some(_) => Self::Repeated,
        }
    }

    /// The address one occupant holds on `lane`, given every birth it has.
    pub(super) fn address(
        record: Option<Self>,
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
    ) -> Result<OccupantAddress, AddressRefusal> {
        match record {
            Some(Self::Unique {
                lane: born,
                position,
            }) if born == lane => Ok(OccupantAddress::new(position)),
            Some(Self::Repeated) => Err(AddressRefusal::RepeatedBirth { occupant }),
            _ => Err(AddressRefusal::MissingBirth { lane, occupant }),
        }
    }
}

/// The address each of `lanes` offers, keyed by lane.
pub(super) fn seat_addresses(
    lanes: &[LaneResidency],
    birth: impl Fn(CanonicalUuid) -> Option<BirthRecord>,
) -> BTreeMap<CanonicalUuid, SeatAddress> {
    lanes
        .iter()
        .map(|row| {
            let seat = match row.occupant {
                None => SeatAddress::Vacant,
                Some(occupant) => match BirthRecord::address(birth(occupant), row.lane, occupant) {
                    Ok(address) => SeatAddress::Seated { occupant, address },
                    Err(refusal) => SeatAddress::Unproved {
                        occupant,
                        reason: refusal.reason().to_owned(),
                    },
                },
            };
            (row.lane, seat)
        })
        .collect()
}

/// The address each of `lanes` offers, proved from the journal alone: every
/// `lane::started` fact through this read's cursor.
///
/// This is the daemonless twin of
/// [`TaskFacts::seat_addresses`](super::TaskFacts::seat_addresses).
pub fn journal_seat_addresses(
    read: &JournalRead<'_>,
    lanes: &[LaneResidency],
) -> Result<BTreeMap<CanonicalUuid, SeatAddress>, GuardError> {
    let mut births = HashMap::new();
    let seated = lanes
        .iter()
        .filter_map(|row| row.occupant)
        .collect::<HashSet<_>>();
    let budget = FactReadBudget::new(
        journal::MAX_RETAINED_FACT_PAGE_ROWS,
        journal::MAX_RETAINED_FACT_PAGE_BYTES,
    )
    .expect("the retained fact page ceiling is a repository budget");
    let mut after = None;
    loop {
        let page = read.selected_fact_page(&[FactKind::LaneStarted], after, None, budget)?;
        for row in &page.rows {
            let Some(started) = row.fact.read::<journal::crossing::body::LaneStarted>() else {
                continue;
            };
            if seated.contains(&started.occupant) {
                let record = BirthRecord::recorded(
                    births.get(&started.occupant).copied(),
                    started.lane,
                    row.seq,
                );
                births.insert(started.occupant, record);
            }
        }
        match page.continuation {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    Ok(seat_addresses(lanes, |occupant| {
        births.get(&occupant).copied()
    }))
}

/// One occupant birth proved by the `lane::started` fact its address names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OccupantBirth {
    /// Address derived from the birth position.
    pub address: OccupantAddress,
    /// Lane the occupant was born on.
    pub lane: CanonicalUuid,
    /// Occupant the birth recorded.
    pub occupant: CanonicalUuid,
    /// Resident generation that observed the birth.
    pub epoch: CanonicalUuid,
    /// Commit instant of the birth fact.
    pub at: Instant,
}

/// Why an address or occupant has no proved, executable birth.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressRefusal {
    /// The journal records no fact at the addressed position.
    Unrecorded { address: OccupantAddress },
    /// The addressed fact is not an occupant birth.
    NotBirth {
        address: OccupantAddress,
        kind: FactKind,
    },
    /// More than one `lane::started` fact names the occupant, so none is its birth.
    RepeatedBirth { occupant: CanonicalUuid },
    /// No retained `lane::started` fact records the occupant's birth on its lane.
    MissingBirth {
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
    },
    /// The proved occupant no longer sits on its birth lane.
    Expired { birth: OccupantBirth },
}

impl AddressRefusal {
    /// Stable refusal name.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::Unrecorded { .. } => "address::unrecorded",
            Self::NotBirth { .. } => "address::not-birth",
            Self::RepeatedBirth { .. } => "address::repeated-birth",
            Self::MissingBirth { .. } => "address::missing-birth",
            Self::Expired { .. } => "address::expired",
        }
    }
}

impl std::fmt::Display for AddressRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = self.reason();
        match self {
            Self::Unrecorded { address } => {
                write!(
                    formatter,
                    "{reason}: no journal fact is recorded at {address}"
                )
            }
            Self::NotBirth { address, kind } => write!(
                formatter,
                "{reason}: {address} names a {kind} fact, not an occupant birth"
            ),
            Self::RepeatedBirth { occupant } => write!(
                formatter,
                "{reason}: occupant {occupant} has more than one recorded birth"
            ),
            Self::MissingBirth { lane, occupant } => write!(
                formatter,
                "{reason}: occupant {occupant} has no recorded birth on lane {lane}"
            ),
            Self::Expired { birth } => write!(
                formatter,
                "{reason}: occupant {} born at {} no longer sits on lane {}",
                birth.occupant, birth.address, birth.lane
            ),
        }
    }
}

/// Proof outcome for one address or occupant.
pub type AddressProof = Result<OccupantBirth, AddressRefusal>;

/// Why an occupant code selects no current lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaneCodeRefusal {
    /// No `lane::started` fact sits at the code's position.
    Unknown { code: OccupantAddress },
    /// The code's lane has left the residency.
    Removed {
        code: OccupantAddress,
        lane: CanonicalUuid,
    },
}

impl LaneCodeRefusal {
    /// Stable refusal name.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::Unknown { .. } => "lane::code-unknown",
            Self::Removed { .. } => "lane::code-removed",
        }
    }
}

impl std::fmt::Display for LaneCodeRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown { code } => {
                write!(
                    formatter,
                    "{code} is no occupant code this journal recorded"
                )
            }
            Self::Removed { code, lane } => {
                write!(
                    formatter,
                    "{code} belongs to lane {lane}, which was removed"
                )
            }
        }
    }
}

/// Prove the occupant birth one address names, current or historical.
fn prove_birth(
    read: &JournalRead<'_>,
    snapshot: &TaskSnapshot,
    address: OccupantAddress,
) -> Result<AddressProof, GuardError> {
    let position = address.birth();
    if read.cursor()?.is_none_or(|cursor| position > cursor) {
        return Ok(Err(AddressRefusal::Unrecorded { address }));
    }
    let row = read
        .fact_page(
            FactPageStart::At(position),
            position,
            FactReadBudget::new(1, journal::MAX_RETAINED_FACT_PAGE_BYTES)
                .expect("one addressed birth fact is a repository budget"),
        )?
        .rows
        .into_iter()
        .next()
        .ok_or_else(|| GuardError::Invalid(format!("journal position {position} has no fact")))?;
    let Some(started) = row.fact.read::<journal::crossing::body::LaneStarted>() else {
        return Ok(Err(AddressRefusal::NotBirth {
            address,
            kind: row.fact.kind(),
        }));
    };
    match snapshot.facts().birth_record(started.occupant) {
        Some(BirthRecord::Repeated) => Ok(Err(AddressRefusal::RepeatedBirth {
            occupant: started.occupant,
        })),
        Some(BirthRecord::Unique {
            position: indexed, ..
        }) if indexed == position => Ok(Ok(OccupantBirth {
            address,
            lane: started.lane,
            occupant: started.occupant,
            epoch: started.epoch,
            at: row.at,
        })),
        _ => Err(GuardError::Invalid(format!(
            "occupant birth index disagrees with the lane start at {position}"
        ))),
    }
}

/// Resolve one address to the exact occupant its current residency seats.
///
/// Execution never follows a lane to a successor: a proved birth whose
/// occupant has left its birth lane is expired.
fn resolve(
    read: &JournalRead<'_>,
    snapshot: &TaskSnapshot,
    address: OccupantAddress,
) -> Result<AddressProof, GuardError> {
    Ok(prove_birth(read, snapshot, address)?.and_then(|birth| {
        if seats(snapshot.residency().map(|head| head.body()), &birth) {
            Ok(birth)
        } else {
            Err(AddressRefusal::Expired { birth })
        }
    }))
}

fn seats(residency: Option<&ResidencyHead>, birth: &OccupantBirth) -> bool {
    residency.is_some_and(|head| {
        head.lanes
            .iter()
            .any(|row| row.lane == birth.lane && row.occupant == Some(birth.occupant))
    })
}
