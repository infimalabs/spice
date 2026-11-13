use super::*;

/// Set a phase target with exact operator workflow authority.
pub(super) fn set_target(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: &Actor,
    task: &TaskHandle,
    (phase, target): (&str, u32),
) -> Outcome<CommandAnswer> {
    let view = board(bus, epoch, actor.clone());
    let heads = at(&view, task);
    let expected = vec![workflow_witness(heads)];
    issue(
        bus,
        epoch,
        actor.clone(),
        CommandKind::TaskTarget,
        expected,
        &serde_json::json!({ "task": task, "phase": phase, "target": target }),
    )
}

fn set_target_as_human_operator(
    bus: &Bus,
    epoch: CanonicalUuid,
    task: &TaskHandle,
    (phase, target): (&str, u32),
) -> Outcome<CommandAnswer> {
    let principal = uuid(0xfeed);
    let roles = wire::principal::PrincipalRoles::new(vec![wire::principal::PrincipalHead {
        principal,
        born: Seq::new(1).expect("fixture principal birth"),
        profile: wire::principal::PrincipalProfile::default(),
        roles: std::collections::BTreeSet::from([wire::principal::PrincipalRole::Operator]),
        capability: None,
    }]);
    let view = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let command = Command {
        v: WireVersion::CURRENT,
        id: uuid(0xbeef),
        epoch,
        kind: CommandKind::TaskTarget,
        expected: vec![workflow_witness(at(&view, task))],
        body: serde_json::json!({ "task": task, "phase": phase, "target": target }),
    }
    .admit(
        &Accepted {
            wire: WireVersion::CURRENT,
            actor: Actor::Principal(principal),
            epoch,
            release: "1.2.3".parse().expect("fixture release"),
        },
        &roles,
    )
    .expect("authenticated operator command admits");
    dispatch(bus, &command).expect("board command owns task target")
}

fn target_of(bus: &Bus, epoch: CanonicalUuid, task: &TaskHandle, phase: &str) -> u32 {
    let view = board(bus, epoch, Actor::TEST_PRINCIPAL);
    at(&view, task)
        .completion
        .body
        .target(&phase.parse().expect("fixture phase"))
        .get()
}

/// `seat`'s own claim on `task`, exactly as the board reads it.
fn claim_of(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, task: &TaskHandle) -> BoardClaim {
    let view = board(bus, epoch, Actor::TEST_PRINCIPAL);
    at(&view, task)
        .claims
        .iter()
        .rev()
        .find(|held| held.claim.body.lane == seat.lane)
        .expect("the lane has a claim on the task")
        .clone()
}

#[test]
fn explicit_targets_require_operator_authority_even_from_a_holder() {
    let (root, bus, epoch, seats) = fixture("completion-target", 2);
    let holder = Actor::Lane(seats[0].lane);
    let bystander = Actor::Lane(seats[1].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "TARGET-1kTarget1",
        "Staff one phase",
        Some("todo,plan,review"),
    );
    claim_task(&bus, epoch, &seats[0], &holder, &task);
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let before = at(&view, &task).clone();
    let zero = admit_command(
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskTarget,
        vec![workflow_witness(&before)],
        &serde_json::json!({ "task": task, "phase": "todo", "target": 0 }),
    );
    assert!(matches!(zero, Err(wire::CommandRejection::Fault(_))));
    for actor in [holder, bystander] {
        for (phase, target) in [("todo", 2), ("review", 7)] {
            let rejected = admit_command(
                epoch,
                actor.clone(),
                CommandKind::TaskTarget,
                vec![workflow_witness(&before)],
                &serde_json::json!({ "task": task, "phase": phase, "target": target }),
            );
            assert!(matches!(rejected, Err(wire::CommandRejection::Refusal(_))));
        }
    }
    let after = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&after, &task).completion, before.completion);
    assert_eq!(at(&after, &task).claims, before.claims);
    assert_committed(set_target(
        &bus,
        epoch,
        &Actor::TEST_PRINCIPAL,
        &task,
        ("todo", 3),
    ));
    assert_committed(set_target_as_human_operator(
        &bus,
        epoch,
        &task,
        ("plan", 2),
    ));
    assert_committed(set_target(
        &bus,
        epoch,
        &Actor::TEST_PRINCIPAL,
        &task,
        ("todo", 1),
    ));
    let after = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(
        (
            target_of(&bus, epoch, &task, "todo"),
            target_of(&bus, epoch, &task, "plan")
        ),
        (1, 2)
    );
    assert_eq!(
        at(&after, &task).completion.body.round,
        before.completion.body.round
    );
    assert_eq!(at(&after, &task).workflow, before.workflow);
    assert_eq!(at(&after, &task).claims, before.claims);
    clean(&root, bus);
}

#[test]
fn open_capacity_seats_distinct_lanes_whose_claims_end_one_at_a_time() {
    let (root, bus, epoch, seats) = fixture("completion-capacity", 3);
    let lanes = seats
        .iter()
        .map(|seat| Actor::Lane(seat.lane))
        .collect::<Vec<_>>();
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "TARGET-1kTarget2",
        "Two lanes on one round",
        Some("todo,review"),
    );
    assert_committed(set_target(
        &bus,
        epoch,
        &Actor::TEST_PRINCIPAL,
        &task,
        ("todo", 2),
    ));

    claim_task(&bus, epoch, &seats[0], &lanes[0], &task);
    assert_eq!(
        allocated(&bus, epoch, &lanes[1]).map(|(handle, _)| handle),
        Some(task.to_string()),
        "a partially claimed round offers its open seat to another lane"
    );
    claim_task(&bus, epoch, &seats[1], &lanes[1], &task);
    let first = claim_of(&bus, epoch, &seats[0], &task);
    let second = claim_of(&bus, epoch, &seats[1], &task);
    assert_ne!(first.claim.body.claimed, second.claim.body.claimed);
    for held in [&first, &second] {
        assert!(held.is_active());
        let lease = held.lease.as_ref().expect("each claim has its own lease");
        assert_eq!(lease.body.claimed, held.claim.body.claimed);
    }
    assert_eq!(
        allocated(&bus, epoch, &lanes[2]),
        None,
        "a full round offers nothing"
    );

    assert_committed(set_target(
        &bus,
        epoch,
        &Actor::TEST_PRINCIPAL,
        &task,
        ("todo", 1),
    ));
    assert_eq!(claim_of(&bus, epoch, &seats[0], &task), first);
    assert_eq!(claim_of(&bus, epoch, &seats[1], &task), second);
    assert_eq!(
        allocated(&bus, epoch, &lanes[2]),
        None,
        "lowering below active claims opens nothing and revokes nobody"
    );

    let complete = |seat: &Seat, actor: &Actor, evidence: &str| {
        let view = board(&bus, epoch, actor.clone());
        let expected = progress_witnesses(&view, at(&view, &task), seat.lane, seat.occupant, false)
            .expect("completion witnesses");
        issue(
            &bus,
            epoch,
            actor.clone(),
            CommandKind::TaskDone,
            expected,
            &TaskDone {
                operation: Some(completion_operation()),
                ..finished(&task, evidence.to_owned())
            },
        )
    };
    assert_committed(complete(&seats[0], &lanes[0], "first participant"));
    let after_first = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let after_first = at(&after_first, &task);
    assert_eq!(after_first.workflow.body.phase.as_str(), "todo");
    assert_eq!(after_first.completion.body.arrivals.len(), 1);
    assert_eq!(
        claim_of(&bus, epoch, &seats[0], &task)
            .claim
            .body
            .disposition,
        ClaimDisposition::Freed
    );
    assert_eq!(
        claim_of(&bus, epoch, &seats[1], &task),
        second,
        "one arrival leaves its sibling exact claim active"
    );

    assert_committed(complete(&seats[1], &lanes[1], "second participant"));
    let completed = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let completed = at(&completed, &task);
    assert_eq!(completed.workflow.body.phase.as_str(), "review");
    assert!(
        completed.completion.body.arrivals.is_empty(),
        "the phase transition opens one fresh round"
    );
    assert!(
        completed.claims.iter().all(|claim| !claim.is_active()),
        "a shared round carries no arbitrary final claim"
    );
    clean(&root, bus);
}

#[test]
fn shared_round_history_records_each_participants_own_completion_once() {
    let (root, bus, epoch, seats) = fixture("completion-round-history", 2);
    let lanes = seats
        .iter()
        .map(|seat| Actor::Lane(seat.lane))
        .collect::<Vec<_>>();
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "TARGET-1kTarget6",
        "Read every participant's evidence",
        Some("todo,review"),
    );
    for phase in ["todo", "review"] {
        assert_committed(set_target(
            &bus,
            epoch,
            &Actor::TEST_PRINCIPAL,
            &task,
            (phase, 2),
        ));
    }
    for (seat, actor) in seats.iter().zip(&lanes) {
        claim_task(&bus, epoch, seat, actor, &task);
    }
    for (seat, actor) in seats.iter().zip(&lanes).rev() {
        let view = board(&bus, epoch, actor.clone());
        let expected = progress_witnesses(&view, at(&view, &task), seat.lane, seat.occupant, false)
            .expect("completion witnesses");
        assert_committed(issue(
            &bus,
            epoch,
            actor.clone(),
            CommandKind::TaskDone,
            expected,
            &TaskDone {
                operation: Some(completion_operation()),
                ..finished(&task, format!("evidence from {actor}"))
            },
        ));
    }
    for (seat, actor) in seats.iter().zip(&lanes) {
        claim_task(&bus, epoch, seat, actor, &task);
    }
    for seat in &seats {
        assert_committed(preparation::review_clean(&bus, epoch, seat, &task));
    }

    let born = task.born().expect("fixture stamp");
    let Outcome::Reply(Reply {
        body: CommandAnswer::Board(answer),
        ..
    }) = issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::BoardRead,
        Vec::new(),
        &BoardRead::Tasks {
            tasks: vec![born.clone()],
            dependencies: false,
            claims: false,
            history: true,
        },
    )
    else {
        panic!("task history read did not reply");
    };
    let completions = answer.steps[&born]
        .iter()
        .filter_map(|step| match &step.step {
            wire::intent::TaskStep::Done {
                phase, validation, ..
            } => Some((step.author.clone(), phase.to_string(), validation.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        completions,
        [&lanes[1], &lanes[0]]
            .map(|actor| (
                actor.clone(),
                "todo".to_owned(),
                format!("evidence from {actor}")
            ))
            .to_vec(),
        "each todo participant completes once, in arrival order, and review arrivals add no completion"
    );
    clean(&root, bus);
}

#[test]
fn lowering_a_satisfied_unheld_round_advances_from_durable_arrivals() {
    let (root, bus, epoch, seats) = fixture("completion-lowered-barrier", 2);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "TARGET-1kTarget3",
        "Close a lowered barrier",
        Some("todo,review"),
    );
    assert_committed(set_target(
        &bus,
        epoch,
        &Actor::TEST_PRINCIPAL,
        &task,
        ("todo", 3),
    ));
    for seat in &seats {
        let actor = Actor::Lane(seat.lane);
        claim_task(&bus, epoch, seat, &actor, &task);
        let view = board(&bus, epoch, actor.clone());
        let expected = progress_witnesses(&view, at(&view, &task), seat.lane, seat.occupant, false)
            .expect("completion witnesses");
        assert_committed(issue(
            &bus,
            epoch,
            actor,
            CommandKind::TaskDone,
            expected,
            &TaskDone {
                operation: Some(completion_operation()),
                ..finished(&task, format!("participant {}", seat.lane))
            },
        ));
    }
    let waiting = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let waiting = at(&waiting, &task);
    assert_eq!(waiting.workflow.body.phase.as_str(), "todo");
    assert_eq!(waiting.completion.body.arrivals.len(), 2);
    assert!(waiting.claims.iter().all(|claim| !claim.is_active()));

    assert_committed(set_target(
        &bus,
        epoch,
        &Actor::TEST_PRINCIPAL,
        &task,
        ("todo", 2),
    ));
    let advanced = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let advanced = at(&advanced, &task);
    assert_eq!(advanced.workflow.body.phase.as_str(), "review");
    assert!(advanced.completion.body.arrivals.is_empty());
    clean(&root, bus);
}

#[test]
fn ending_the_last_claim_closes_a_lowered_satisfied_round() {
    for (name, born, lapse) in [
        ("completion-free-barrier", "TARGET-1kTarget4", false),
        ("completion-lapse-barrier", "TARGET-1kTarget5", true),
    ] {
        let (root, bus, epoch, seats) = fixture(name, 2);
        let task = add(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            born,
            "Close after the last holder leaves",
            Some("todo,review"),
        );
        assert_committed(set_target(
            &bus,
            epoch,
            &Actor::TEST_PRINCIPAL,
            &task,
            ("todo", 2),
        ));
        let actors = seats
            .iter()
            .map(|seat| Actor::Lane(seat.lane))
            .collect::<Vec<_>>();
        for (seat, actor) in seats.iter().zip(&actors) {
            claim_task(&bus, epoch, seat, actor, &task);
        }
        let view = board(&bus, epoch, actors[0].clone());
        let expected = progress_witnesses(
            &view,
            at(&view, &task),
            seats[0].lane,
            seats[0].occupant,
            false,
        )
        .expect("completion witnesses");
        assert_committed(issue(
            &bus,
            epoch,
            actors[0].clone(),
            CommandKind::TaskDone,
            expected,
            &TaskDone {
                operation: Some(completion_operation()),
                ..finished(&task, "first participant arrived")
            },
        ));
        assert_committed(set_target(
            &bus,
            epoch,
            &Actor::TEST_PRINCIPAL,
            &task,
            ("todo", 1),
        ));
        if lapse {
            lapse_claim(&bus, epoch, &seats[1], &task);
        } else {
            super::refresh::free_active(&bus, epoch, &actors[1], &seats[1], &task);
        }
        let advanced = board(&bus, epoch, Actor::TEST_PRINCIPAL);
        let advanced = at(&advanced, &task);
        assert_eq!(advanced.workflow.body.phase.as_str(), "review");
        assert!(advanced.completion.body.arrivals.is_empty());
        assert!(advanced.claims.iter().all(|claim| !claim.is_active()));
        clean(&root, bus);
    }
}
