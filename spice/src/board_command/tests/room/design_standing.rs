//! Deferral as the one stop for an open Design: who can claim it and what
//! deferring it asks of its holder.

use super::super::preparation::add_controller;
use super::super::refresh::free_active;
use super::*;
use wire::rooms::StopControl;

pub(super) fn design(bus: &Bus, epoch: CanonicalUuid, name: &str) -> TaskHandle {
    add_controller(bus, epoch, name, wire::TaskIntent::Design, None)
}

pub(super) fn born(task: &TaskHandle) -> TaskStamp {
    task.born().expect("fixture task stamp")
}

/// Win `task` for `seat` under the named `rule`, as `spice task capture` and
/// `spice task capture --steal` submit it; a recovery rule carries a
/// well-formed transaction so the claim reaches selection.
pub(in super::super) fn win_named(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    task: &TaskHandle,
    rule: &str,
) -> Outcome<CommandAnswer> {
    let lane = Actor::Lane(seat.lane);
    let available = board(bus, epoch, lane.clone());
    let heads = at(&available, task);
    let rule: Name = rule.parse().expect("named claim rule");
    issue(
        bus,
        epoch,
        lane,
        CommandKind::ClaimWin,
        vec![
            statement_witness(heads),
            workflow_witness(heads),
            claim_witness(heads),
            Witness::Eligibility { rule: rule.clone() },
            residency(&available, seat),
        ],
        &ClaimWin {
            recovery: rule.as_str().starts_with("recover").then(|| "0".repeat(40)),
            rule,
        },
    )
}

pub(in super::super) fn active_holder(
    bus: &Bus,
    epoch: CanonicalUuid,
    task: &TaskHandle,
) -> Option<Actor> {
    at(&board(bus, epoch, Actor::TEST_PRINCIPAL), task)
        .latest_claim()
        .map(|held| &held.claim)
        .filter(|claim| claim.body.disposition == ClaimDisposition::Active)
        .map(|claim| claim.body.actor.clone())
}

/// Every direction ever sent asking a holder of `room` to stop.
fn stop_directions(bus: &Bus, room: &TaskHandle) -> Vec<wire::body::SteerSent> {
    let room = born(room);
    bus.transact(|journal| {
        bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
    })
    .expect("steering facts read")
    .facts()
    .iter()
    .filter_map(|envelope| envelope.fact.read::<wire::body::SteerSent>())
    .filter(|sent| {
        sent.control
            .as_deref()
            .and_then(|control| control.parse::<StopControl>().ok())
            .is_some_and(|stop| stop.room == room)
    })
    .collect()
}

/// The one live stop request standing against `room`'s holder, if any.
fn live_stop(bus: &Bus, room: &TaskHandle) -> Option<wire::SteeringHead> {
    let room = born(room);
    let mut stops = bus
        .read(move |read| read.live_room_stops(&room, 2))
        .expect("steering index reads");
    assert!(stops.len() <= 1, "one live stop at most: {stops:?}");
    stops.pop().map(|head| head.body().clone())
}

/// The operator's indefinite deferral of `task` at the heads it reads now,
/// with the lane it asked to park.
fn defer(
    bus: &Bus,
    epoch: CanonicalUuid,
    task: &TaskHandle,
) -> (Outcome<CommandAnswer>, Option<CanonicalUuid>) {
    let current = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&current, task);
    let command = fixture_command(
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDefer,
        vec![workflow_witness(heads), deferral_witness(heads)],
        &TaskDefer {
            task: task.clone(),
            wait: Instant::indefinite(),
        },
    );
    let wire::intent::Intent::TaskDefer(request) = command.body() else {
        panic!("fixture command carries a deferral");
    };
    crate::board_command::task_defer(bus, &command, request, None, &steer::Config::default())
}

#[test]
fn deferring_a_held_design_asks_its_holder_to_park_once() {
    let (root, bus, epoch, seats) = fixture("design-defer-held", 2);
    prove_seat_addresses(&bus, epoch, &seats);
    let task = design(&bus, epoch, "Held design");
    let holder = Actor::Lane(seats[0].lane);
    assert_committed(win_named(&bus, epoch, &seats[0], &task, "capture"));
    let before = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task)
        .statement
        .clone();

    let (deferred, notified) = defer(&bus, epoch, &task);
    assert_committed(deferred);
    assert_eq!(
        notified,
        Some(seats[0].lane),
        "the seated holder is woken at once"
    );
    // The live claim stays authoritative until its holder parks it.
    assert_eq!(active_holder(&bus, epoch, &task), Some(holder.clone()));
    let stop = live_stop(&bus, &task).expect("the room records the park request");
    assert_eq!(stop.recipient, Actor::Lane(seats[0].lane));
    let [sent] = stop_directions(&bus, &task)
        .try_into()
        .expect("one park request");
    assert_eq!(sent.key, stop.key);
    assert!(matches!(
        win_named(&bus, epoch, &seats[1], &task, "steal"),
        Outcome::Refusal(_)
    ));

    let (again, notified) = defer(&bus, epoch, &task);
    assert_committed(again);
    assert_eq!(notified, None, "a standing stop is not sent twice");
    assert_eq!(stop_directions(&bus, &task).len(), 1);

    free_active(&bus, epoch, &holder, &seats[0], &task);
    assert_eq!(active_holder(&bus, epoch, &task), None);
    assert!(
        matches!(
            win_named(&bus, epoch, &seats[1], &task, "capture"),
            Outcome::Refusal(_)
        ),
        "a parked deferred Design is not recaptured"
    );
    assert_eq!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task).statement,
        before,
        "deferral changes no statement, preparation, or authorization"
    );
    clean(&root, bus);
}

#[test]
fn the_holders_own_deferral_releases_its_design_without_a_stop() {
    let (root, bus, epoch, seats) = fixture("design-defer-self", 1);
    prove_seat_addresses(&bus, epoch, &seats);
    let task = design(&bus, epoch, "Self-deferred design");
    let holder = Actor::Lane(seats[0].lane);
    assert_committed(win_named(&bus, epoch, &seats[0], &task, "capture"));
    let held = board(&bus, epoch, holder.clone());
    let heads = at(&held, &task);
    assert_committed(issue(
        &bus,
        epoch,
        holder,
        CommandKind::TaskDefer,
        vec![
            workflow_witness(heads),
            deferral_witness(heads),
            held_claim_witness(heads),
            held_lease_witness(heads),
            residency(&held, &seats[0]),
        ],
        &TaskDefer {
            task: task.clone(),
            wait: Instant::indefinite(),
        },
    ));
    assert_eq!(active_holder(&bus, epoch, &task), None);
    assert!(stop_directions(&bus, &task).is_empty());
    clean(&root, bus);
}

#[test]
fn a_vacant_holder_is_asked_to_park_through_its_lane() {
    let (root, bus, epoch, seats) = fixture("design-defer-vacant", 1);
    prove_seat_addresses(&bus, epoch, &seats);
    let task = design(&bus, epoch, "Vacant holder design");
    let holder = Actor::Lane(seats[0].lane);
    assert_committed(win_named(&bus, epoch, &seats[0], &task, "capture"));
    vacate(&bus, epoch, &seats[0]);

    assert_committed(defer(&bus, epoch, &task).0);
    let stop = live_stop(&bus, &task).expect("a vacant holder still receives the park request");
    assert_eq!(stop.recipient, Actor::Lane(seats[0].lane));
    let [sent] = stop_directions(&bus, &task)
        .try_into()
        .expect("one park request");
    assert_eq!(
        sent.recipient,
        Actor::Lane(seats[0].lane),
        "the request topics for the lane's next seat"
    );
    assert_eq!(active_holder(&bus, epoch, &task), Some(holder));
    clean(&root, bus);
}

#[test]
fn a_capture_read_before_the_design_was_deferred_cannot_claim_it() {
    let (root, bus, epoch, seats) = fixture("design-defer-stale", 1);
    let task = design(&bus, epoch, "Stale offer design");
    let lane = Actor::Lane(seats[0].lane);
    let available = board(&bus, epoch, lane.clone());
    let heads = at(&available, &task);
    let rule: Name = "capture".parse().expect("capture rule");
    let stale = vec![
        statement_witness(heads),
        workflow_witness(heads),
        claim_witness(heads),
        Witness::Eligibility { rule: rule.clone() },
        residency(&available, &seats[0]),
    ];

    assert_committed(defer(&bus, epoch, &task).0);
    let outcome = issue(
        &bus,
        epoch,
        lane,
        CommandKind::ClaimWin,
        stale,
        &ClaimWin {
            rule,
            recovery: None,
        },
    );
    assert!(matches!(outcome, Outcome::Refusal(_)), "{outcome:?}");
    assert_eq!(active_holder(&bus, epoch, &task), None);
    clean(&root, bus);
}
