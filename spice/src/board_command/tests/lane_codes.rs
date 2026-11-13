//! Occupant codes as lane selectors: every code a lane's occupants were born
//! with selects that lane through one board snapshot, while the lane shows its
//! newest code whether seated or vacant.

use super::*;

use wire::{LaneSelection, OccupantAddress};

fn origin(epoch: CanonicalUuid) -> wire::Origin {
    format!("resident:{epoch}")
        .parse()
        .expect("resident fixture origin")
}

fn now() -> Instant {
    Instant::new(jiff::Timestamp::now()).expect("fixture instant")
}

/// Record one occupant birth on `lane`, edit the residency in the same write,
/// and return the code the birth mints.
fn start(
    bus: &Bus,
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
) -> OccupantAddress {
    let mut residency = board(bus, epoch, Actor::TEST_PRINCIPAL).residency.body;
    let row = residency
        .lanes
        .iter_mut()
        .find(|row| row.lane == lane)
        .expect("started lane is present");
    row.occupant = Some(occupant);
    row.lineage.push(occupant);
    row.disposition = "occupied".to_owned();
    let mut write = JournalWrite::new(
        Actor::Spice,
        origin(epoch),
        now(),
        Producer::ResidentRecognition,
    );
    let started = write
        .fact(
            body::LaneStarted {
                lane,
                occupant,
                epoch,
                group: 1,
                fresh_session: true,
            },
            FactCause::Root,
        )
        .expect("occupant birth builds");
    write.residency_head(started, move |_| residency);
    let CommitOutcome::Committed(commit) = bus.commit(write).expect("birth reaches writer") else {
        panic!("occupant birth did not commit");
    };
    OccupantAddress::new(wire::Seq::new(commit.first()).expect("committed position is nonzero"))
}

/// Replace the residency through `edit`, recording no occupant birth.
fn reseat(bus: &Bus, epoch: CanonicalUuid, edit: impl FnOnce(&mut ResidencyHead)) {
    let mut residency = board(bus, epoch, Actor::TEST_PRINCIPAL).residency.body;
    edit(&mut residency);
    residency.topology_revision = residency.topology_revision.saturating_add(1);
    let mut write = JournalWrite::new(Actor::Spice, origin(epoch), now(), Producer::CommandDoor);
    let seated = write
        .fact(body::UpSeated { epoch }, FactCause::Root)
        .expect("residency change fact builds");
    write.residency_head(seated, move |_| residency);
    assert!(matches!(
        bus.commit(write).expect("residency change reaches writer"),
        CommitOutcome::Committed(_)
    ));
}

fn select(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    code: OccupantAddress,
) -> Outcome<CommandAnswer> {
    issue(
        bus,
        epoch,
        actor,
        CommandKind::BoardRead,
        Vec::new(),
        &BoardRead::Lane { code },
    )
}

fn selected(bus: &Bus, epoch: CanonicalUuid, code: OccupantAddress) -> LaneSelection {
    let Outcome::Reply(Reply {
        body: CommandAnswer::Board(answer),
        ..
    }) = select(bus, epoch, Actor::TEST_PRINCIPAL, code)
    else {
        panic!("{code} selects a lane");
    };
    answer.selected.expect("a lane read answers its selection")
}

fn selected_after(bus: &Bus, epoch: CanonicalUuid, code: OccupantAddress) -> LaneSelection {
    let Outcome::Reply(Reply {
        body: CommandAnswer::Board(answer),
        ..
    }) = issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::BoardRead,
        Vec::new(),
        &BoardRead::Open {
            recent_finished: 0,
            after: Some(code),
        },
    )
    else {
        panic!("{code} selects a lane-list continuation");
    };
    answer
        .selected
        .expect("an open read with a code cursor answers its selection")
}
fn refused(bus: &Bus, epoch: CanonicalUuid, code: OccupantAddress) -> String {
    let Outcome::Refusal(refusal) = select(bus, epoch, Actor::TEST_PRINCIPAL, code) else {
        panic!("{code} selects no lane");
    };
    refusal.reason().to_owned()
}

fn shown(bus: &Bus, lane: CanonicalUuid) -> Option<OccupantAddress> {
    bus.task_snapshot()
        .expect("snapshot reads")
        .facts()
        .lane_code(lane)
}

#[test]
fn every_occupant_code_selects_its_lane_through_vacancy_and_until_removal() {
    let (_root, bus, epoch, seats) = fixture("lane-codes", 2);
    let (first, second) = (&seats[0], &seats[1]);
    assert_eq!(shown(&bus, first.lane), None, "no occupant has started yet");

    let original = start(&bus, epoch, first.lane, first.occupant);
    let neighbor = start(&bus, epoch, second.lane, second.occupant);
    let successor = start(&bus, epoch, first.lane, uuid(9_001));
    let latest = start(&bus, epoch, first.lane, uuid(9_002));

    // Every code the first lane has shown selects it, counting the occupants
    // started since, and each reports the newest code.
    for (code, since) in [(original, 2), (successor, 1), (latest, 0)] {
        assert_eq!(
            selected(&bus, epoch, code),
            LaneSelection {
                lane: first.lane,
                code: latest,
                since,
            }
        );
    }
    assert_eq!(selected(&bus, epoch, neighbor).lane, second.lane);
    assert_eq!(shown(&bus, first.lane), Some(latest));

    // A vacant lane keeps its newest code and every code still selects it.
    reseat(&bus, epoch, |residency| {
        let row = residency
            .lanes
            .iter_mut()
            .find(|row| row.lane == first.lane)
            .expect("first lane remains");
        row.occupant = None;
        row.disposition = "vacant".to_owned();
    });
    assert_eq!(shown(&bus, first.lane), Some(latest));
    assert_eq!(selected(&bus, epoch, original).lane, first.lane);
    assert_eq!(selected(&bus, epoch, latest).since, 0);
    assert_eq!(
        selected_after(&bus, epoch, original),
        LaneSelection {
            lane: first.lane,
            code: latest,
            since: 2,
        }
    );

    // A lane may target another lane, not only the operator.
    let Outcome::Reply(Reply {
        body: CommandAnswer::Board(answer),
        ..
    }) = select(&bus, epoch, Actor::Lane(second.lane), successor)
    else {
        panic!("a lane may select another lane by code");
    };
    assert_eq!(
        answer.selected.map(|selection| selection.lane),
        Some(first.lane)
    );

    // Removal retires every code the removed lane showed.
    reseat(&bus, epoch, |residency| {
        residency.lanes.retain(|row| row.lane != second.lane);
    });
    assert_eq!(refused(&bus, epoch, neighbor), "lane::code-removed");
    assert_eq!(selected(&bus, epoch, original).lane, first.lane);
}

#[test]
fn a_code_naming_no_occupant_birth_selects_no_lane() {
    let (_root, bus, epoch, seats) = fixture("lane-codes-unknown", 1);
    let born = start(&bus, epoch, seats[0].lane, seats[0].occupant);
    let cursor = board(&bus, epoch, Actor::TEST_PRINCIPAL).cursor;

    // The seating fact precedes every birth; a position past the cursor has
    // no fact at all.
    let seating = OccupantAddress::new(wire::Seq::FIRST);
    let unrecorded =
        OccupantAddress::new(wire::Seq::new(cursor.get() + 1).expect("position past the cursor"));
    assert_ne!(seating, born);
    assert_eq!(refused(&bus, epoch, seating), "lane::code-unknown");
    assert_eq!(refused(&bus, epoch, unrecorded), "lane::code-unknown");
}
