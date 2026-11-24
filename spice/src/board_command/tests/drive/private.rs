use super::*;

#[test]
fn private_creation_and_draft_advance_use_current_owner_not_public_rules() {
    let (root, bus, epoch, seats) = fixture("private-publication", 2);
    for seat in &seats {
        authorize_lane_creation(&bus, epoch, seat);
    }
    let repository = policy(&root, "[]");
    let topic = format!("{}/task/child", seats[0].lane);
    let add = authored_add(
        "Keep private work with its current team",
        &topic,
        Some("todo"),
        &["Only the owner files work"],
    );
    let born = stamp("private-owner-add");
    let command = add_command(epoch, seats[0].lane, &born, &add);
    assert_committed(mutation::task_add(
        &bus,
        &command,
        &add,
        Some(host(&repository)),
    ));
    let task = TaskHandle::for_topic(&topic.parse::<wire::Topic>().unwrap(), &born);
    let visible = board(&bus, epoch, Actor::Lane(seats[0].lane));
    assert_eq!(
        at(&visible, &task).ownership.as_ref().unwrap().teams,
        vec![seats[0].team]
    );

    let born = stamp("private-foreign-add");
    let command = add_command(epoch, seats[1].lane, &born, &add);
    assert_committed(mutation::task_add(
        &bus,
        &command,
        &add,
        Some(host(&repository)),
    ));
    let saved = bus.task_snapshot().unwrap();
    let saved_draft = saved
        .task_heads(&born)
        .expect("foreign private work remains a Draft");
    assert!(saved_draft.workflow().body().is_draft());
    assert!(saved_draft.claims().is_empty());
    let task = TaskHandle::for_phase(
        Some(&topic.parse::<wire::Topic>().unwrap()),
        &born,
        wire::Phase::Draft,
    );
    let visible = board(&bus, epoch, Actor::Lane(seats[1].lane));
    let heads = at(&visible, &task);
    let file = TaskFile {
        task: task.clone(),
        topic: None,
        claim: false,
    };
    let command = fixture_command(
        epoch,
        Actor::Lane(seats[1].lane),
        CommandKind::TaskFile,
        vec![
            statement_witness(heads),
            workflow_witness(heads),
            deferral_witness(heads),
        ],
        &file,
    );
    let before = bus.cursor().unwrap();
    denied(task_file(&bus, &command, &file, Some(host(&repository))));
    assert_eq!(bus.cursor().unwrap(), before);
    let after = board(&bus, epoch, Actor::Lane(seats[1].lane));
    assert!(at(&after, &task).workflow.body.is_draft());
    assert!(at(&after, &task).claims.is_empty());
    clean(&root, bus);
}

#[test]
fn private_work_whose_owner_stops_draining_is_blocked_on_that_drain() {
    let (root, bus, epoch, seats) = fixture("private-drain-off", 1);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    let repository = policy(&root, "[]");
    let topic = format!("{}/task/child", seats[0].lane);
    let add = authored_add(
        "Keep private work with its team",
        &topic,
        Some("todo"),
        &["Only the owner takes it"],
    );
    let born = stamp("private-drain-off");
    assert_committed(mutation::task_add(
        &bus,
        &add_command(epoch, seats[0].lane, &born, &add),
        &add,
        Some(host(&repository)),
    ));
    let draining = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(!draining.blocked.contains(&born));
    assert!(draining.unallocated.is_empty());

    subscription::configure_subscription(&bus, epoch, &seats[0], false);
    let stopped = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(
        stopped.blocked.contains(&born),
        "reserved work no drain takes is blocked, not ready"
    );
    assert_eq!(
        stopped.unallocated.get(&born),
        Some(&wire::intent::Unallocated::DrainOff {
            teams: vec![seats[0].team]
        })
    );
    clean(&root, bus);
}

#[test]
fn private_publication_refuses_moved_membership_and_recaptures_the_new_team() {
    let (root, bus, epoch, seats) = fixture("private-membership-race", 1);
    let seat = &seats[0];
    authorize_lane_creation(&bus, epoch, seat);
    let repository = policy(&root, "[]");
    let topic = format!("{}/task/child", seat.lane);
    let add = authored_add(
        "Follow current private topic ownership",
        &topic,
        Some("todo"),
        &["Current membership is guarded"],
    );
    let born = stamp("private-membership-race");
    let command = add_command(epoch, seat.lane, &born, &add);
    let moved_cursor = std::cell::Cell::new(None);
    let new_team = uuid(98_002);
    let outcome = mutation::task_add_with(&bus, &command, &add, Some(host(&repository)), || {
        subscription::move_fixture_lane_to_team(&bus, epoch, seat, new_team);
        moved_cursor.set(bus.cursor().unwrap());
    });
    assert!(matches!(outcome, Outcome::Refusal(_)), "{outcome:?}");
    assert_eq!(bus.cursor().unwrap(), moved_cursor.get());
    let command = add_command(epoch, seat.lane, &born, &add);
    assert_committed(mutation::task_add(
        &bus,
        &command,
        &add,
        Some(host(&repository)),
    ));
    let task = TaskHandle::for_topic(&topic.parse::<wire::Topic>().unwrap(), &born);
    let visible = board(&bus, epoch, Actor::Lane(seat.lane));
    let row = at(&visible, &task);
    assert_eq!(row.creator_team, Some(new_team));
    assert_eq!(row.ownership.as_ref().unwrap().teams, vec![new_team]);
    assert!(row.ownership.as_ref().unwrap().reserved);
    clean(&root, bus);
}
