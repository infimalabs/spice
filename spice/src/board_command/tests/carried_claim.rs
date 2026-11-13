use super::*;

fn phases(flow: &[&str]) -> Vec<wire::Phase> {
    flow.iter()
        .map(|phase| phase.parse().expect("fixture phase"))
        .collect()
}

fn advance(from: &str, to: &str) -> board::TransitionKind {
    board::TransitionKind::Advance {
        from: from.parse().expect("fixture phase"),
        to: to.parse().expect("fixture phase"),
    }
}

fn boundary(local_commits: u32) -> body::GitBoundary {
    body::GitBoundary {
        head: "lane-head".to_owned(),
        integrated: "integrated-head".to_owned(),
        upstream: None,
        upstream_head: None,
        local_commits,
    }
}

fn complete(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, task: &TaskHandle) -> CommandAnswer {
    let lane = Actor::Lane(seat.lane);
    let held = board(bus, epoch, lane.clone());
    let expected = progress_witnesses(&held, at(&held, task), seat.lane, seat.occupant, false)
        .expect("completion witnesses");
    assert_committed(issue(
        bus,
        epoch,
        lane,
        CommandKind::TaskDone,
        expected,
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(task, "phase evidence retained")
        },
    ))
}

fn claim_win(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    task: &TaskHandle,
    rule: &str,
) -> Outcome<CommandAnswer> {
    let lane = Actor::Lane(seat.lane);
    let available = board(bus, epoch, lane.clone());
    let heads = at(&available, task);
    let rule: Name = rule.parse().expect("claim rule");
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
            rule,
            recovery: None,
        },
    )
}

#[test]
fn done_into_a_continuing_phase_carries_the_claim_to_its_lane() {
    let (root, bus, epoch, seats) = fixture("carried-claim", 2);
    let task = add_task_with_term(
        &bus,
        epoch,
        "CARRY-1kCarry01",
        "Carry the claim across the plan phase",
        "/board/test",
        Some("plan,todo,review"),
        &["the next phase has a holder"],
    );
    let holder = Actor::Lane(seats[0].lane);
    claim_task(&bus, epoch, &seats[0], &holder, &task);

    let CommandAnswer::Task(answer) = complete(&bus, epoch, &seats[0], &task) else {
        panic!("task done returns a task answer")
    };
    let carried = answer
        .claim
        .expect("a continuing done names the carried claim");
    assert_eq!(carried.holder, Some(holder.clone()));
    let phase = carried.phase.expect("the carried claim names its phase");
    assert_eq!(phase.name.as_str(), "todo");
    assert_eq!(answer.next, carried.next);

    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&view, &task);
    assert_eq!(heads.workflow.body.phase.as_str(), "todo");
    let claim = &heads
        .latest_claim()
        .map(|held| &held.claim)
        .expect("claim head")
        .body;
    assert_eq!(claim.disposition, ClaimDisposition::Active);
    assert_eq!(claim.actor, holder);
    assert_eq!(claim.lane, seats[0].lane);
    assert_eq!(claim.occupant, seats[0].occupant);
    let lease = &heads
        .latest_claim()
        .and_then(|held| held.lease.as_ref())
        .expect("the carried claim opens a lease");
    assert_eq!(
        lease.position, claim.claimed,
        "the lease is born with the carried win"
    );
    let expiry = instant_after(lease.body.start, 60 * 60).expect("lease expiry");
    assert_eq!((lease.body.expiry, lease.body.maximum), (expiry, expiry));
    let Some(NextWorkState::Active { task: active }) = next_board(&bus, epoch, holder.clone()).next
    else {
        panic!("the carrying lane's task next holds the carried claim")
    };
    assert_eq!(active.statement.body.born, heads.statement.body.born);

    let rival = Actor::Lane(seats[1].lane);
    assert_eq!(allocated(&bus, epoch, &rival), None);
    for rule in ["next", "capture"] {
        let refused = claim_win(&bus, epoch, &seats[1], &task, rule);
        assert!(
            matches!(refused, Outcome::Refusal(_)),
            "a peer's {rule} cannot take a carried claim: {refused:?}"
        );
    }
    let after = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(
        at(&after, &task)
            .latest_claim()
            .map(|held| &held.claim)
            .map(|claim| &claim.body.actor),
        Some(&holder)
    );
    clean(&root, bus);
}

#[test]
fn final_phase_claim_releases_possession_without_resetting_original_claim_time() {
    let (root, bus, epoch, seats) = fixture("carried-claim-final", 2);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CARRY-1kCarry02",
        "Free the claim into review",
        Some("todo,review"),
    );
    assert_eq!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task).original_claimed_at,
        None,
        "an unclaimed task invents no claim age"
    );
    let holder = Actor::Lane(seats[0].lane);
    claim_task(&bus, epoch, &seats[0], &holder, &task);
    let original = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task)
        .original_claimed_at
        .expect("the first win records its durable instant");
    let CommandAnswer::Task(answer) = complete(&bus, epoch, &seats[0], &task) else {
        panic!("task done returns a task answer")
    };
    assert!(answer.claim.is_none());
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&view, &task);
    assert_eq!(heads.workflow.body.phase.as_str(), "review");
    assert_eq!(
        heads
            .latest_claim()
            .map(|held| &held.claim)
            .map(|claim| claim.body.disposition),
        Some(ClaimDisposition::Freed)
    );
    assert!(!matches!(
        next_board(&bus, epoch, holder).next,
        Some(NextWorkState::Active { .. })
    ));

    let reviewer = Actor::Lane(seats[1].lane);
    claim_task(&bus, epoch, &seats[1], &reviewer, &task);
    let reviewed = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&reviewed, &task);
    assert_eq!(
        heads.original_claimed_at,
        Some(original),
        "a later phase winner cannot reset the first durable claim instant"
    );
    assert_eq!(
        heads.latest_claim().map(|claim| &claim.claim.body.actor),
        Some(&reviewer)
    );
    clean(&root, bus);
}

#[test]
fn only_an_unpublished_advance_before_another_non_review_phase_carries() {
    let flow = phases(&["plan", "todo", "review"]);
    let into_todo = advance("plan", "todo");
    assert!(progress::carries_claim(&into_todo, &flow, None, false));
    assert!(progress::carries_claim(
        &into_todo,
        &flow,
        Some(&boundary(0)),
        false
    ));
    assert!(!progress::carries_claim(
        &into_todo,
        &flow,
        Some(&boundary(1)),
        false
    ));
    assert!(!progress::carries_claim(&into_todo, &flow, None, true));
    assert!(!progress::carries_claim(
        &advance("todo", "review"),
        &flow,
        None,
        false
    ));
    assert!(!progress::carries_claim(
        &advance("plan", "review"),
        &phases(&["plan", "review", "todo"]),
        None,
        false
    ));
    assert!(!progress::carries_claim(
        &board::TransitionKind::Complete {
            phase: "review".parse().expect("fixture phase"),
        },
        &flow,
        None,
        false
    ));
}
