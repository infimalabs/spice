//! Occupant addresses prove an exact birth and execute only for its seat.

use bus::{AddressRefusal, Bus, CommitOutcome, OccupantBirth};
use journal::crossing::{
    Actor, CanonicalUuid, FactKind, Instant, LaneResidency, OccupantAddress, Producer,
    ResidencyHead, ResidentState, SeatAddress, Seq, body,
};
use journal::{FactCause, Journal, JournalWrite};

const EPOCH: u64 = 1;
const LANE_A: u64 = 2;
const LANE_B: u64 = 3;
const LANE_C: u64 = 8;
const FIRST: u64 = 4;
const SECOND: u64 = 5;
const SUCCESSOR: u64 = 6;
const RENEWED: u64 = 7;
const INITIAL_SEQUENCE: u64 = Seq::FIRST.get();

#[test]
fn a_birth_address_names_its_seated_occupant_and_distinct_births_stay_distinct() {
    let bus = seeded();
    let facts = bus.task_snapshot().expect("snapshot reads");
    let first = facts
        .facts()
        .occupant_address(uuid(LANE_A), uuid(FIRST))
        .expect("first birth is proved");
    let second = facts
        .facts()
        .occupant_address(uuid(LANE_B), uuid(SECOND))
        .expect("second birth is proved");
    assert_eq!(
        (first, second),
        (address(INITIAL_SEQUENCE + 1), address(INITIAL_SEQUENCE + 2))
    );
    assert_eq!(
        bus.resolve_occupant(first).expect("resolver answers"),
        Ok(birth(INITIAL_SEQUENCE + 1, LANE_A, FIRST))
    );
    assert_eq!(
        bus.resolve_occupant(second).expect("resolver answers"),
        Ok(birth(INITIAL_SEQUENCE + 2, LANE_B, SECOND))
    );
    assert_eq!(
        facts.facts().occupant_address(uuid(LANE_B), uuid(FIRST)),
        Err(AddressRefusal::MissingBirth {
            lane: uuid(LANE_B),
            occupant: uuid(FIRST),
        })
    );
    bus.shutdown().expect("writer stops");
}

#[test]
fn an_address_off_a_birth_fact_is_refused_by_name() {
    let bus = seeded();
    assert_eq!(
        bus.resolve_occupant(address(INITIAL_SEQUENCE))
            .expect("resolver answers"),
        Err(AddressRefusal::NotBirth {
            address: address(INITIAL_SEQUENCE),
            kind: FactKind::UpSeated,
        })
    );
    for missing in [INITIAL_SEQUENCE + 3, u64::MAX] {
        assert_eq!(
            bus.occupant_birth(address(missing))
                .expect("resolver answers"),
            Err(AddressRefusal::Unrecorded {
                address: address(missing),
            })
        );
    }
    bus.shutdown().expect("writer stops");
}

#[test]
fn a_successor_expires_its_predecessor_address_and_receives_its_own() {
    let bus = seeded();
    let mut write = lane_write();
    let started = write
        .fact(started(LANE_A, SUCCESSOR), FactCause::Root)
        .expect("successor start builds");
    write.residency_head(started, |_| {
        residency(vec![row(LANE_A, SUCCESSOR), row(LANE_B, SECOND)])
    });
    commit(&bus, write);

    let predecessor = birth(INITIAL_SEQUENCE + 1, LANE_A, FIRST);
    assert_eq!(
        bus.resolve_occupant(address(INITIAL_SEQUENCE + 1))
            .expect("resolver answers"),
        Err(AddressRefusal::Expired { birth: predecessor })
    );
    assert_eq!(
        bus.occupant_birth(address(INITIAL_SEQUENCE + 1))
            .expect("resolver answers"),
        Ok(predecessor),
        "historical identity stays readable"
    );
    let snapshot = bus.task_snapshot().expect("snapshot reads");
    let successor = snapshot
        .facts()
        .occupant_address(uuid(LANE_A), uuid(SUCCESSOR))
        .expect("successor birth is proved");
    assert_eq!(successor, address(INITIAL_SEQUENCE + 3));
    assert_eq!(
        bus.resolve_occupant(successor).expect("resolver answers"),
        Ok(birth(INITIAL_SEQUENCE + 3, LANE_A, SUCCESSOR))
    );
    bus.shutdown().expect("writer stops");
}

#[test]
fn a_repeated_birth_proves_no_occupant() {
    let bus = seeded();
    let mut write = lane_write();
    write
        .fact(started(LANE_A, FIRST), FactCause::Root)
        .expect("repeated start builds");
    commit(&bus, write);

    let refusal = AddressRefusal::RepeatedBirth {
        occupant: uuid(FIRST),
    };
    for position in [INITIAL_SEQUENCE + 1, INITIAL_SEQUENCE + 3] {
        assert_eq!(
            bus.occupant_birth(address(position))
                .expect("resolver answers"),
            Err(refusal)
        );
    }
    assert_eq!(
        bus.task_snapshot()
            .expect("snapshot reads")
            .facts()
            .occupant_address(uuid(LANE_A), uuid(FIRST)),
        Err(refusal)
    );
    bus.shutdown().expect("writer stops");
}

#[test]
fn a_seat_without_a_retained_birth_has_no_address() {
    let bus = seeded();
    let mut write = lane_write();
    let renewed = write
        .fact(
            body::LaneRenewed {
                lane: uuid(LANE_A),
                occupant: uuid(RENEWED),
                ancestor: uuid(FIRST),
                epoch: uuid(EPOCH),
                group: 9,
            },
            FactCause::Root,
        )
        .expect("renewal builds");
    write.residency_head(renewed, |_| {
        residency(vec![row(LANE_A, RENEWED), row(LANE_B, SECOND)])
    });
    commit(&bus, write);

    assert_eq!(
        bus.task_snapshot()
            .expect("snapshot reads")
            .facts()
            .occupant_address(uuid(LANE_A), uuid(RENEWED)),
        Err(AddressRefusal::MissingBirth {
            lane: uuid(LANE_A),
            occupant: uuid(RENEWED),
        })
    );
    assert_eq!(
        bus.resolve_occupant(address(INITIAL_SEQUENCE + 3))
            .expect("resolver answers"),
        Err(AddressRefusal::NotBirth {
            address: address(INITIAL_SEQUENCE + 3),
            kind: FactKind::LaneRenewed,
        })
    );
    bus.shutdown().expect("writer stops");
}

#[test]
fn every_seat_address_resolves_to_its_shown_occupant_with_or_without_a_resident() {
    let bus = seeded();
    let mut write = lane_write();
    let successor = write
        .fact(started(LANE_A, SUCCESSOR), FactCause::Root)
        .expect("successor start builds");
    write
        .fact(started(LANE_B, SECOND), FactCause::Root)
        .expect("repeated start builds");
    write.residency_head(successor, |_| {
        let mut vacant = row(LANE_C, RENEWED);
        vacant.occupant = None;
        vacant.disposition = "vacant".to_owned();
        residency(vec![row(LANE_A, SUCCESSOR), row(LANE_B, SECOND), vacant])
    });
    commit(&bus, write);

    let snapshot = bus.task_snapshot().expect("snapshot reads");
    let lanes = snapshot
        .residency()
        .expect("residency head")
        .body()
        .lanes
        .clone();
    let resident = snapshot.facts().seat_addresses(&lanes);
    let daemonless = bus
        .read(move |read| bus::journal_seat_addresses(read, &lanes))
        .expect("journal projection reads");
    assert_eq!(resident, daemonless, "both projections agree");
    assert_eq!(
        resident.into_iter().collect::<Vec<_>>(),
        [
            (
                uuid(LANE_A),
                SeatAddress::Seated {
                    occupant: uuid(SUCCESSOR),
                    address: address(INITIAL_SEQUENCE + 3),
                },
            ),
            (
                uuid(LANE_B),
                SeatAddress::Unproved {
                    occupant: uuid(SECOND),
                    reason: "address::repeated-birth".to_owned(),
                },
            ),
            (uuid(LANE_C), SeatAddress::Vacant),
        ],
        "the replaced occupant's address is never offered for its lane"
    );
    assert_eq!(
        bus.resolve_occupant(address(INITIAL_SEQUENCE + 3))
            .expect("resolver answers"),
        Ok(birth(INITIAL_SEQUENCE + 3, LANE_A, SUCCESSOR))
    );
    bus.shutdown().expect("writer stops");
}

/// A seated resident whose two lanes were born in one write at one instant.
fn seeded() -> Bus {
    let bus = Bus::start(Journal::in_memory().expect("in-memory authority"))
        .expect("dedicated writer starts");
    let mut seat = JournalWrite::new(Actor::Spice, origin(), at(), Producer::CommandDoor);
    let seated = seat
        .fact(body::UpSeated { epoch: uuid(EPOCH) }, FactCause::Root)
        .expect("seat builds");
    seat.residency_head(seated, |_| residency(Vec::new()));
    commit(&bus, seat);

    let mut births = lane_write();
    births
        .fact(started(LANE_A, FIRST), FactCause::Root)
        .expect("first start builds");
    let second = births
        .fact(started(LANE_B, SECOND), FactCause::Root)
        .expect("second start builds");
    births.residency_head(second, |_| {
        residency(vec![row(LANE_A, FIRST), row(LANE_B, SECOND)])
    });
    commit(&bus, births);
    bus
}

fn lane_write() -> JournalWrite {
    JournalWrite::new(Actor::Spice, origin(), at(), Producer::ResidentRecognition)
}

fn started(lane: u64, occupant: u64) -> body::LaneStarted {
    body::LaneStarted {
        lane: uuid(lane),
        occupant: uuid(occupant),
        epoch: uuid(EPOCH),
        group: 7,
        fresh_session: true,
    }
}

fn residency(lanes: Vec<LaneResidency>) -> ResidencyHead {
    ResidencyHead {
        epoch: uuid(EPOCH),
        state: ResidentState::Seated,
        topology_revision: 0,
        lanes,
        subscriptions: Vec::new(),
        renewals: Vec::new(),
    }
}

fn row(lane: u64, occupant: u64) -> LaneResidency {
    LaneResidency {
        lane: uuid(lane),
        team: uuid(lane),
        name: None,
        worktree: format!("/worktrees/{lane}"),
        life: "drive".to_owned(),
        vendor: "omp".parse().expect("vendor"),
        model: None,
        effort: None,
        fast_mode: false,
        occupant: Some(uuid(occupant)),
        lineage: vec![uuid(occupant)],
        capabilities: Vec::new(),
        session: None,
        disposition: "occupied".to_owned(),
    }
}

fn commit(bus: &Bus, write: JournalWrite) {
    assert!(matches!(
        bus.commit(write).expect("writer answers"),
        CommitOutcome::Committed(_)
    ));
}

fn birth(position: u64, lane: u64, occupant: u64) -> OccupantBirth {
    OccupantBirth {
        address: address(position),
        lane: uuid(lane),
        occupant: uuid(occupant),
        epoch: uuid(EPOCH),
        at: at(),
    }
}

fn address(position: u64) -> OccupantAddress {
    OccupantAddress::new(Seq::new(position).expect("fixture position is nonzero"))
}

fn origin() -> journal::crossing::Origin {
    format!("resident:{}", uuid(EPOCH)).parse().expect("origin")
}

fn at() -> Instant {
    "2030-04-12T18:44:09.271Z".parse().expect("instant")
}

fn uuid(value: u64) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-{value:012x}")
        .parse()
        .expect("canonical uuid")
}
