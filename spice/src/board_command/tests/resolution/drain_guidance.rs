use super::*;

#[test]
fn explicit_release_clears_possession_and_ranks_the_row_last_for_that_lane() {
    let (root, bus, epoch, seats) = fixture("drain-explicit-release", 1);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    let lane = Actor::Lane(seats[0].lane);
    let released_task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "RELEASE-first01",
        "Release this task",
        Some("todo"),
    );
    let next_task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "RELEASE-second1",
        "Continue draining here",
        Some("todo"),
    );
    claim_task(&bus, epoch, &seats[0], &lane, &released_task);

    let held = board(&bus, epoch, lane.clone());
    let held_heads = at(&held, &released_task);
    let released = assert_committed(issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::ClaimFree,
        vec![
            held_claim_witness(held_heads),
            held_lease_witness(held_heads),
            residency(&held, &seats[0]),
        ],
        &TaskRef {
            task: released_task.clone(),
        },
    ));
    let CommandAnswer::Claim(released) = released else {
        panic!("claim release did not return a claim answer");
    };
    assert_eq!(released.holder, None);

    let available = board(&bus, epoch, lane.clone());
    let released_heads = at(&available, &released_task);
    assert!(released_heads.explicit_release);
    assert!(
        task_show(&available, released_heads, &[])
            .lines()
            .any(|line| line == "claim -"),
        "an inactive historical claim is not current possession",
    );
    claim_task(&bus, epoch, &seats[0], &lane, &next_task);

    let continued = board(&bus, epoch, lane.clone());
    assert_eq!(
        at(&continued, &next_task)
            .latest_claim()
            .map(|held| &held.claim)
            .map(|claim| claim.body.disposition),
        Some(ClaimDisposition::Active),
    );
    assert_eq!(
        at(&continued, &released_task)
            .latest_claim()
            .map(|held| &held.claim)
            .map(|claim| claim.body.disposition),
        Some(ClaimDisposition::Freed),
    );
    let entries = actor_entries(&continued).expect("continued board projects to entries");
    let subscription = allocation_for(&continued, &lane);
    let quiet = board::select(&entries, &lane, &subscription, &continued.environment())
        .expect("a freed row is ranked last, never excluded");
    assert_eq!(quiet.entry().handle(), &released_task);
    clean(&root, bus);
}

fn drain_add(title: &str, origin: &wire::Origin) -> TaskAdd {
    TaskAdd {
        topic: Some("/board/test".parse().expect("fixture topic")),
        term: vec!["the fixture work is verified".to_owned()],
        origin: Some(origin.clone()),
        flow: Some("todo".to_owned()),
        ..adding(title.to_owned(), "This work keeps the fixture honest.")
    }
}

#[test]
fn drain_guidance_follows_lane_ownership_across_add_release_and_wake() {
    let (root, bus, epoch, seats) = fixture("drain-continuation", 1);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    let lane = Actor::Lane(seats[0].lane);
    let first_stamp = stamp("DRAIN-first001");
    let first = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &first_stamp,
    );
    let origin = fixture_origin();
    let add = |title: &str| drain_add(title, &origin);
    let idle_add = assert_committed(issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::TaskAdd,
        vec![Witness::Absence {
            task: first_stamp.clone(),
        }],
        &add("Idle lane creates durable work"),
    ));
    let CommandAnswer::Task(idle_add) = idle_add else {
        panic!("task creation did not return a task answer");
    };
    assert_eq!(idle_add.next.as_deref(), Some("spice task next"));

    claim_task(&bus, epoch, &seats[0], &lane, &first);
    let second_stamp = stamp("DRAIN-second01");
    let second = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &second_stamp,
    );
    let claimed_add = assert_committed(issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::TaskAdd,
        vec![Witness::Absence {
            task: second_stamp.clone(),
        }],
        &add("Claimed lane captures follow-up work"),
    ));
    let CommandAnswer::Task(claimed_add) = claimed_add else {
        panic!("claimed task creation did not return a task answer");
    };
    assert_eq!(claimed_add.next, None);

    let held = board(&bus, epoch, lane.clone());
    let held_heads = at(&held, &first);
    let released = assert_committed(issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::ClaimFree,
        vec![
            held_claim_witness(held_heads),
            held_lease_witness(held_heads),
            residency(&held, &seats[0]),
        ],
        &TaskRef {
            task: first.clone(),
        },
    ));
    let CommandAnswer::Claim(released) = released else {
        panic!("claim release did not return a claim answer");
    };
    assert_eq!(released.next.as_deref(), Some("spice task next"));

    let before_defer = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let second_heads = at(&before_defer, &second);
    let until = instant_after(current_instant().expect("clock"), 300).expect("future instant");
    let deferred_answer = assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDefer,
        vec![
            workflow_witness(second_heads),
            deferral_witness(second_heads),
        ],
        &TaskDefer {
            task: second.clone(),
            wait: until,
        },
    ));
    let CommandAnswer::Task(deferred_answer) = deferred_answer else {
        panic!("task deferral did not return a task answer");
    };
    assert_eq!(deferred_answer.next, None);
    let deferred = board(&bus, epoch, lane.clone());
    let woken = assert_committed(issue(
        &bus,
        epoch,
        lane,
        CommandKind::TaskWake,
        vec![
            workflow_witness(at(&deferred, &second)),
            deferral_witness(at(&deferred, &second)),
        ],
        &TaskWake {
            task: second,
            claim: false,
        },
    ));
    let CommandAnswer::Task(woken) = woken else {
        panic!("task wake did not return a task answer");
    };
    assert_eq!(woken.next.as_deref(), Some("spice task next"));
    clean(&root, bus);
}
