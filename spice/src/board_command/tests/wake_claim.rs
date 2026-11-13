use super::*;

fn claim_transition_expected(
    answer: &BoardAnswer,
    task: &TaskHandle,
    seat: &Seat,
    filing: bool,
) -> Vec<Witness> {
    let heads = at(answer, task);
    let mut expected = vec![workflow_witness(heads), deferral_witness(heads)];
    if filing {
        expected.push(statement_witness(heads));
    }
    expected.push(Witness::Eligibility {
        rule: "capture".parse().expect("fixture capture rule"),
    });
    expected.push(residency(answer, seat));
    expected
}

fn capture_task(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, task: &TaskHandle) -> Actor {
    let lane = Actor::Lane(seat.lane);
    let available = board(bus, epoch, lane.clone());
    let heads = at(&available, task);
    let rule: Name = "capture".parse().expect("capture rule");
    assert_committed(issue(
        bus,
        epoch,
        lane.clone(),
        CommandKind::ClaimWin,
        vec![
            statement_witness(heads),
            workflow_witness(heads),
            claim_witness(heads),
            Witness::Eligibility { rule: rule.clone() },
            residency(&available, seat),
        ],
        &ClaimWin {
            rule,
            recovery: None,
        },
    ));
    lane
}

fn wake(task: &TaskHandle, claim: bool) -> TaskWake {
    TaskWake {
        task: task.clone(),
        claim,
    }
}

#[test]
fn deferred_wake_claims_in_one_transaction_and_an_existing_claim_refuses_without_waking() {
    let (root, bus, epoch, seats) = fixture("wake-claim", 1);
    let seat = &seats[0];
    let actor = Actor::Lane(seat.lane);
    authorize_lane_creation(&bus, epoch, seat);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "WAKE-claim001",
        "Wake and hold deferred work",
        Some("todo"),
    );
    let before = board(&bus, epoch, actor.clone());
    let heads = at(&before, &task);
    let until = instant_after(current_instant().expect("clock"), 300).expect("future instant");
    assert_committed(issue(
        &bus,
        epoch,
        actor.clone(),
        CommandKind::TaskDefer,
        vec![workflow_witness(heads), deferral_witness(heads)],
        &TaskDefer {
            task: task.clone(),
            wait: until,
        },
    ));

    let deferred = board(&bus, epoch, actor.clone());
    let Outcome::Reply(Reply {
        position: ReplyPosition::Committed(range),
        body: CommandAnswer::Task(answer),
    }) = issue(
        &bus,
        epoch,
        actor.clone(),
        CommandKind::TaskWake,
        claim_transition_expected(&deferred, &task, seat, false),
        &wake(&task, true),
    )
    else {
        panic!("wake-and-claim did not commit")
    };
    let held = answer.claim.expect("wake --claim returns its held claim");
    assert_eq!(held.task, task);
    assert_eq!(held.holder, Some(actor.clone()));
    let committed = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("fixture facts remain readable")
        .facts()
        .iter()
        .filter(|fact| fact.seq >= range.first && fact.seq <= range.last)
        .map(|fact| fact.fact.kind())
        .collect::<Vec<_>>();
    assert!(committed.contains(&wire::FactKind::TaskWoken));
    assert!(committed.contains(&wire::FactKind::ClaimWon));
    let current = board(&bus, epoch, actor.clone());
    let heads = at(&current, &task);
    assert_eq!(heads.workflow.body.deferral, None);
    let claim = heads
        .latest_claim()
        .map(|held| &held.claim)
        .expect("woken task is held");
    assert_eq!(claim.body.actor, actor);
    assert_eq!(claim.body.disposition, ClaimDisposition::Active);

    let second = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "WAKE-claim002",
        "Refuse a second held wake",
        Some("todo"),
    );
    let available = board(&bus, epoch, actor.clone());
    let heads = at(&available, &second);
    assert_committed(issue(
        &bus,
        epoch,
        actor.clone(),
        CommandKind::TaskDefer,
        vec![workflow_witness(heads), deferral_witness(heads)],
        &TaskDefer {
            task: second.clone(),
            wait: until,
        },
    ));
    let deferred = board(&bus, epoch, actor.clone());
    let facts = fact_count(&bus);
    let refused = issue(
        &bus,
        epoch,
        actor.clone(),
        CommandKind::TaskWake,
        claim_transition_expected(&deferred, &second, seat, false),
        &wake(&second, true),
    );
    assert!(
        matches!(refused, Outcome::Refusal(ref refusal)
            if refusal.reason() == "claim::active-exists"),
        "existing claim must refuse atomic wake: {refused:?}",
    );
    assert_eq!(fact_count(&bus), facts, "refused wake wrote facts");
    assert_eq!(
        at(&board(&bus, epoch, actor), &second)
            .workflow
            .body
            .deferral,
        Some(until),
        "refused claim must leave the task deferred",
    );
    clean(&root, bus);
}

#[test]
fn self_defer_releases_the_claim_but_peer_defer_leaves_it_held() {
    let (root, bus, epoch, seats) = fixture("defer-release", 1);
    let owner = Actor::Lane(seats[0].lane);
    let peer = Actor::TEST_PRINCIPAL;
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DEFER-release01",
        "Release a self-deferred claim",
        Some("todo"),
    );
    capture_task(&bus, epoch, &seats[0], &task);
    let held = board(&bus, epoch, owner.clone());
    let heads = at(&held, &task);
    let until = instant_after(current_instant().expect("clock"), 300).expect("future instant");
    let Outcome::Reply(Reply {
        position: ReplyPosition::Committed(range),
        ..
    }) = issue(
        &bus,
        epoch,
        owner.clone(),
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
            wait: until,
        },
    )
    else {
        panic!("self defer did not commit")
    };
    let committed = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("fixture facts remain readable")
        .facts()
        .iter()
        .filter(|fact| fact.seq >= range.first && fact.seq <= range.last)
        .map(|fact| fact.fact.kind())
        .collect::<Vec<_>>();
    assert!(committed.contains(&wire::FactKind::TaskDeferred));
    assert!(committed.contains(&wire::FactKind::ClaimFreed));
    let deferred = board(&bus, epoch, owner.clone());
    let heads = at(&deferred, &task);
    assert_eq!(heads.workflow.body.deferral, Some(until));
    assert_eq!(
        heads
            .latest_claim()
            .map(|held| &held.claim)
            .map(|claim| claim.body.disposition),
        Some(ClaimDisposition::Freed),
    );

    let peer_until =
        instant_after(current_instant().expect("clock"), 600).expect("future peer instant");
    assert_committed(issue(
        &bus,
        epoch,
        peer.clone(),
        CommandKind::TaskWake,
        vec![workflow_witness(heads), deferral_witness(heads)],
        &TaskWake {
            task: task.clone(),
            claim: false,
        },
    ));
    capture_task(&bus, epoch, &seats[0], &task);
    let held = board(&bus, epoch, peer.clone());
    let heads = at(&held, &task);
    assert_committed(issue(
        &bus,
        epoch,
        peer,
        CommandKind::TaskDefer,
        vec![workflow_witness(heads), deferral_witness(heads)],
        &TaskDefer {
            task: task.clone(),
            wait: peer_until,
        },
    ));
    let peer_deferred = board(&bus, epoch, owner.clone());
    let heads = at(&peer_deferred, &task);
    assert_eq!(heads.workflow.body.deferral, Some(peer_until));
    assert_eq!(
        heads
            .latest_claim()
            .map(|held| &held.claim)
            .map(|claim| claim.body.disposition),
        Some(ClaimDisposition::Active),
    );
    assert_eq!(
        heads
            .latest_claim()
            .map(|held| &held.claim)
            .map(|claim| &claim.body.actor),
        Some(&owner),
    );
    clean(&root, bus);
}

#[test]
fn settled_task_refuses_wake_claim_without_side_effects() {
    let (root, bus, epoch, seats) = fixture("wake-claim-settled", 1);
    let seat = &seats[0];
    let actor = Actor::Lane(seat.lane);
    authorize_lane_creation(&bus, epoch, seat);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "WAKE-settled1",
        "Completed work cannot wake",
        Some("todo"),
    );
    claim_and_complete_task(&bus, epoch, seat, &actor, &task);
    let completed = board(&bus, epoch, actor.clone());
    let facts = fact_count(&bus);
    let refused = issue(
        &bus,
        epoch,
        actor,
        CommandKind::TaskWake,
        claim_transition_expected(&completed, &task, seat, false),
        &TaskWake { task, claim: true },
    );
    assert!(
        matches!(refused, Outcome::Refusal(ref refusal)
            if refusal.reason() == "task::settled"),
        "settled task must refuse atomic wake: {refused:?}",
    );
    assert_eq!(fact_count(&bus), facts, "settled refusal wrote facts");
    clean(&root, bus);
}
