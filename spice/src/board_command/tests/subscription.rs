use super::*;

pub(super) fn configure_subscription(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, drain: bool) {
    reconfigure_subscription(bus, epoch, seat, |subscription| {
        subscription.set_drain(drain)
    });
}

/// Bind the fixture seat's team to the policy table `[team.NAME]` names.
pub(super) fn name_fixture_team(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, name: &str) {
    reconfigure_subscription(bus, epoch, seat, |subscription| {
        subscription.name = Some(name.to_owned());
    });
}

fn reconfigure_subscription(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    change: impl FnOnce(&mut wire::TeamSubscription),
) {
    let snapshot = bus.task_snapshot().expect("subscription head reads");
    let observed = snapshot.residency().expect("subscription head exists");
    let mut next = observed.body().clone();
    let subscription = next
        .subscriptions
        .iter_mut()
        .find(|subscription| subscription.team == seat.team)
        .expect("fixture team subscription exists");
    let from_drive = subscription.drive;
    let from_drain = subscription.drain;
    let from_name = subscription.name.clone();
    change(subscription);
    let origin = format!("resident:{epoch}")
        .parse()
        .expect("fixture resident origin");
    let at = Instant::new(jiff::Timestamp::now()).expect("current fixture instant");
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin,
        at,
        Producer::ResidentRecognition,
    );
    let configured = write
        .fact(
            wire::body::TeamConfigured {
                team: seat.team,
                member: None,
                from_team: None,
                founded_over: None,
                dissolved_to: None,
                from_drive,
                from_drain,
                from_name,
                drive: subscription.drive,
                drain: subscription.drain,
                name: subscription.name.clone(),
                from_fast_mode: false,
                fast_mode: false,
                sources: Vec::new(),
            },
            FactCause::Root,
        )
        .expect("fixture subscription fact builds");
    write.residency_head(configured, move |_| next);
    match bus
        .commit(write)
        .expect("subscription change reaches writer")
    {
        CommitOutcome::Committed(_) => {}
        outcome => panic!("subscription change did not commit: {outcome:?}"),
    }
}

pub(super) fn move_fixture_lane_to_team(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    team: CanonicalUuid,
) {
    place_fixture_lane(bus, epoch, seat, team, false, None);
}

/// Move the fixture lane with its runtime drain switch independently of lane life.
pub(super) fn place_fixture_lane(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    team: CanonicalUuid,
    drain: bool,
    life: Option<&str>,
) {
    let snapshot = bus
        .task_snapshot()
        .expect("residency head reads before move");
    let observed = snapshot
        .residency()
        .expect("residency head exists before move");
    let mut next = observed.body().clone();
    let row = next
        .lanes
        .iter_mut()
        .find(|row| row.lane == seat.lane)
        .expect("fixture lane remains seated");
    let from_team = row.team;
    row.team = team;
    if let Some(life) = life {
        row.life = life.to_owned();
    }
    next.topology_revision += 1;
    next.subscriptions
        .retain(|subscription| subscription.team != from_team);
    next.subscriptions.push(wire::TeamSubscription {
        team,
        formed_at: None,
        drive: true,
        drain,
        name: None,
        fast_mode: false,
    });

    let origin = format!("resident:{epoch}")
        .parse()
        .expect("fixture resident origin");
    let at = Instant::new(jiff::Timestamp::now()).expect("current fixture instant");
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin,
        at,
        Producer::ResidentRecognition,
    );
    let configured = write
        .fact(
            wire::body::TeamConfigured {
                team,
                member: Some(seat.lane),
                from_team: Some(from_team),
                founded_over: None,
                dissolved_to: None,
                from_drive: true,
                from_drain: drain,
                from_name: None,
                drive: true,
                drain,
                name: None,
                from_fast_mode: false,
                fast_mode: false,
                sources: Vec::new(),
            },
            FactCause::Root,
        )
        .expect("fixture team move fact builds");
    write.residency_head(configured, move |_| next);
    match bus.commit(write).expect("team move reaches writer") {
        CommitOutcome::Committed(_) => {}
        outcome => panic!("team move did not commit: {outcome:?}"),
    }
}

#[test]
fn membership_changes_preserve_immutable_creator_attribution() {
    let (root, bus, epoch, seats) = fixture("creator-team-move", 1);
    let seat = &seats[0];
    let lane = Actor::Lane(seat.lane);
    authorize_lane_creation(&bus, epoch, seat);
    configure_subscription(&bus, epoch, seat, true);
    let task = add_task_in_topic(
        &bus,
        epoch,
        lane.clone(),
        "SCOPE-stable1",
        "Creator-team ownership stays fixed",
        "/task/stable",
        Some("todo"),
    );

    let before = board(&bus, epoch, lane.clone());
    let row = before
        .tasks
        .iter()
        .find(|row| row.statement.body.handle == task)
        .expect("created task remains on the board");
    assert_eq!(row.creator_team, Some(seat.team));

    let moved_team = uuid(NEXT_COMMAND.fetch_add(1, Ordering::Relaxed));
    move_fixture_lane_to_team(&bus, epoch, seat, moved_team);
    let after = board(&bus, epoch, lane.clone());
    let row = after
        .tasks
        .iter()
        .find(|row| row.statement.body.handle == task)
        .expect("old task remains readable after its creator moves");
    assert_eq!(
        row.creator_team,
        Some(seat.team),
        "creator-team ownership is the TaskAdded-time team, not current membership",
    );
    clean(&root, bus);
}

/// Occupant restart life does not grant automatic allocation.
#[test]
fn a_lane_allocates_under_its_current_team_policy_alone() {
    let (root, bus, epoch, seats) = fixture("team-policy-allocation", 1);
    let seat = &seats[0];
    let lane = Actor::Lane(seat.lane);
    authorize_lane_creation(&bus, epoch, seat);
    configure_subscription(&bus, epoch, seat, true);
    let task = add_task_in_topic(
        &bus,
        epoch,
        lane.clone(),
        "SCOPE-alone01",
        "Work the team created",
        "/task/alone",
        Some("todo"),
    );
    place_fixture_lane(&bus, epoch, seat, seat.team, false, Some("drain"));
    assert_eq!(allocated(&bus, epoch, &lane), None);
    place_fixture_lane(&bus, epoch, seat, seat.lane, true, Some("steer"));
    assert_eq!(
        allocated(&bus, epoch, &lane).map(|(handle, _)| handle),
        Some(task.to_string()),
    );
    clean(&root, bus);
}
