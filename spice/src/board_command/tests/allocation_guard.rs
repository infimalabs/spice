use super::*;

mod ranked_population;
mod resource;
mod trust;

/// The task the allocator would hand this actor from one snapshot.
///
/// This is the ranking itself, through the same `board::select` the guard calls,
/// so the winner is fixture setup rather than anything under test here.
pub(super) fn guard_winner(answer: &BoardAnswer, actor: &Actor) -> TaskHandle {
    let entries = actor_entries(answer).expect("allocation guard board entries validate");
    let subscription = allocation_for(answer, actor);
    board::select(&entries, actor, &subscription, &answer.environment())
        .expect("allocation guard fixture selects a task")
        .entry()
        .handle()
        .clone()
}

fn guard_observation(
    answer: &BoardAnswer,
    actor: &Actor,
    _winner: TaskHandle,
) -> AllocationObservation {
    AllocationObservation {
        cursor: answer.cursor,
        actor: actor.clone(),
        residency_position: answer.residency.position,
    }
}

/// Rewrite the residency head over the same seats.
///
/// The lanes and subscriptions are rebuilt identically, so the only thing that
/// moves is the head's position -- which is the single field the allocator's
/// residency comparison reads.
fn reseat(bus: &Bus, epoch: CanonicalUuid, seats: &[Seat]) {
    let lanes = seats
        .iter()
        .map(|seat| wire::LaneResidency {
            lane: seat.lane,
            team: seat.team,
            name: None,
            worktree: "/fixture".to_owned(),
            life: "drive".to_owned(),
            vendor: "omp".parse().expect("reseat fixture vendor"),
            model: None,
            effort: None,
            fast_mode: false,
            occupant: Some(seat.occupant),
            lineage: vec![seat.occupant],
            capabilities: vec![seat.occupant],
            session: None,
            disposition: "occupied".to_owned(),
        })
        .collect::<Vec<_>>();
    let subscriptions = lanes
        .iter()
        .map(|lane| wire::TeamSubscription {
            team: lane.team,
            formed_at: None,
            drive: true,
            drain: true,
            name: None,
            fast_mode: false,
        })
        .collect::<Vec<_>>();
    let origin = format!("resident:{epoch}")
        .parse()
        .expect("reseat fixture origin");
    let at = Instant::new(jiff::Timestamp::now()).expect("reseat fixture instant");
    let mut write = JournalWrite::new(Actor::Spice, origin, at, Producer::CommandDoor);
    let seated = write
        .fact(body::UpSeated { epoch }, FactCause::Root)
        .expect("reseat fact builds");
    write.residency_head(seated, move |_| ResidencyHead {
        epoch,
        state: ResidentState::Seated,
        topology_revision: 0,
        lanes,
        subscriptions,
        renewals: Vec::new(),
    });
    match bus.commit(write).expect("reseat reaches writer") {
        CommitOutcome::Committed(_) => {}
        outcome => panic!("reseat did not commit: {outcome:?}"),
    }
}

/// Commit a write carrying only the allocator's guard, so the refusal observed
/// is the guard's and not some other check on the claim path.
fn commit_guarded(
    bus: &Bus,
    epoch: CanonicalUuid,
    observed: AllocationObservation,
) -> CommitOutcome {
    let origin = format!("resident:{epoch}")
        .parse()
        .expect("allocation guard fixture origin");
    let at = Instant::new(jiff::Timestamp::now()).expect("allocation guard fixture instant");
    let mut write = JournalWrite::new(Actor::Spice, origin, at, Producer::CommandDoor);
    write
        .fact(body::UpSeated { epoch }, FactCause::Root)
        .expect("allocation guard fixture fact builds");
    guard_allocation(&mut write, Vec::new(), observed);
    bus.commit(write)
        .expect("allocation guard write reaches writer")
}

fn refusal_reason(outcome: CommitOutcome) -> String {
    match outcome {
        CommitOutcome::Refused(refusal) => refusal.reason().to_owned(),
        other => panic!("expected the allocator guard to refuse, found {other:?}"),
    }
}

/// Positive control for the three refusal tests below.
///
/// Every one of them asserts that a guard *refuses*, which stays true if the
/// guard refuses everything. This pins the other side: with nothing moved
/// between the observation and the write, the same guard commits.
#[test]
fn an_unmoved_observation_lets_the_allocator_write_commit() {
    let (_root, bus, epoch, seats) = fixture("allocation-guard-quiet", 1);
    let actor = Actor::Lane(seats[0].lane);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    add(&bus, epoch, actor.clone(), "GUARD-1", "first", None);
    add(&bus, epoch, actor.clone(), "GUARD-2", "second", None);

    let answer = board(&bus, epoch, actor.clone());
    let winner = guard_winner(&answer, &actor);
    let observed = guard_observation(&answer, &actor, winner);

    assert!(
        matches!(
            commit_guarded(&bus, epoch, observed),
            CommitOutcome::Committed(_)
        ),
        "the allocator guard commits when nothing moved under it"
    );
}

/// Pins the head-set comparison in `read_guard.rs` on its own.
///
/// A task added after the observation moves the head set without moving the
/// residency head, so the residency comparison ahead of it passes and this is
/// the comparison that has to refuse.
#[test]
fn a_task_added_after_the_observation_refuses_the_allocator_write() {
    let (_root, bus, epoch, seats) = fixture("allocation-guard-heads", 1);
    let actor = Actor::Lane(seats[0].lane);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    add(&bus, epoch, actor.clone(), "GUARD-1", "first", None);
    add(&bus, epoch, actor.clone(), "GUARD-2", "second", None);

    let answer = board(&bus, epoch, actor.clone());
    let winner = guard_winner(&answer, &actor);
    let observed = guard_observation(&answer, &actor, winner.clone());

    add(&bus, epoch, actor.clone(), "GUARD-3", "arrived late", None);

    let moved = board(&bus, epoch, actor.clone());
    assert_ne!(moved.cursor, observed.cursor);
    assert_eq!(
        guard_winner(&moved, &actor),
        winner,
        "the added task must not move the winner, or this scenario would be \
         caught by the winner comparison instead of the head-set comparison"
    );

    assert_eq!(
        refusal_reason(commit_guarded(&bus, epoch, observed)),
        "board::witness-moved: allocator projection changed"
    );
}

/// Pins the residency comparison in `read_guard.rs` on its own.
///
/// Re-seating moves the residency head and touches no task head, so the two
/// comparisons after it would both pass and this one has to refuse.
#[test]
fn a_residency_head_moved_after_the_observation_refuses_the_allocator_write() {
    let (_root, bus, epoch, seats) = fixture("allocation-guard-residency", 1);
    let actor = Actor::Lane(seats[0].lane);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    add(&bus, epoch, actor.clone(), "GUARD-1", "first", None);
    add(&bus, epoch, actor.clone(), "GUARD-2", "second", None);

    let answer = board(&bus, epoch, actor.clone());
    let winner = guard_winner(&answer, &actor);
    let observed = guard_observation(&answer, &actor, winner);

    reseat(&bus, epoch, &seats);

    let moved = board(&bus, epoch, actor.clone());
    assert_ne!(
        moved.residency.position, observed.residency_position,
        "re-seating moved the residency head the guard re-reads"
    );
    assert_eq!(
        refusal_reason(commit_guarded(&bus, epoch, observed)),
        "board::witness-moved: allocator subscription head changed"
    );
}

// The tests above hand `guard_allocation` an observation built by this file.
// The ones below build no observation at all: they run the production claim
// path, `claim_plan` then `commit_claim`, which is the one sequence `claim_win`
// runs, and let a competing write land between those two steps. That ordering
// is ordinary sequential code -- no sleep, no thread, no clock -- so the write
// arrives late on purpose on every run.
//
// Every refusal below is asserted by its exact reason rather than by refusing
// at all. `committed` flattens every guard refusal to the single outer reason
// `board::witness-moved` and carries the comparison that actually refused in
// `observed`, and the client's own witnesses are checked by `witnesses_hold`
// ahead of all three comparisons, so a bare "it refused" assertion would pass
// without the guard ever reaching an allocator comparison.

/// The reason the guard itself refused, from inside the outer refusal.
fn guard_refusal(outcome: Outcome<CommandAnswer>) -> String {
    let Outcome::Refusal(refusal) = outcome else {
        panic!("expected the claim commit to refuse, found {outcome:?}");
    };
    assert_eq!(
        refusal.reason(),
        "board::witness-moved",
        "every guard refusal reaches the caller under this one outer reason"
    );
    refusal
        .diagnostic()
        .and_then(wire::RefusalDiagnostic::observed)
        .expect("guard refusal carries its comparison")
        .to_owned()
}

/// The `task next` command a lane sends for the winner it just read.
fn claim_next_command(
    epoch: CanonicalUuid,
    actor: &Actor,
    answer: &BoardAnswer,
    winner: &TaskHandle,
    seat: &Seat,
) -> wire::AdmittedCommand {
    fixture_command(
        epoch,
        actor.clone(),
        CommandKind::ClaimWin,
        claim_expected(answer, winner, seat),
        &next_rule(),
    )
}

/// The allocator keeps expected exhaustion distinct from broken allocator state.
///
/// The binary rim may treat this exact refusal as a successful terminal drain
/// for a steered lane. The allocator itself still reports the typed reason, so
/// every other caller and every genuine fault remains distinguishable.
#[test]
fn an_empty_allocator_retains_its_exact_diagnostic() {
    let (root, bus, epoch, seats) = fixture("claim-empty-plan", 1);
    let actor = Actor::Lane(seats[0].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "EMPTY-1",
        "deferred allocator row",
        None,
    );
    let before_defer = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&before_defer, &task);
    let until = instant_after(current_instant().expect("clock"), 300).expect("future instant");
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDefer,
        vec![workflow_witness(heads), deferral_witness(heads)],
        &TaskDefer {
            task: task.clone(),
            wait: until,
        },
    ));
    let empty = board(&bus, epoch, actor.clone());
    let command = fixture_command(
        epoch,
        actor,
        CommandKind::ClaimWin,
        claim_expected(&empty, &task, &seats[0]),
        &next_rule(),
    );

    let Err(Outcome::Refusal(empty_refusal)) = claim_plan(
        &bus,
        &command,
        &next_rule(),
        None,
        &board::NextConfig::default(),
        None,
    ) else {
        panic!("an exhausted allocator must return its typed refusal");
    };
    assert_eq!(empty_refusal.reason(), "board::allocator-empty");

    let moved = |observed: &str| {
        Outcome::Refusal(wire::Refusal::bare(wire::RefusalDiagnostic::new(
            "board::witness-moved",
            Some("current durable heads".to_owned()),
            None,
            Some(observed.to_owned()),
        )))
    };
    let mut attempts = 0;
    let retried = retry_task_next(|| {
        attempts += 1;
        if attempts == 1 {
            moved("board::witness-moved: allocator projection changed")
        } else {
            refusal(
                "board::allocator-empty",
                None,
                "the refreshed board is empty",
            )
        }
    });
    assert_eq!(attempts, TASK_NEXT_ATTEMPTS);
    assert!(
        matches!(retried, Outcome::Refusal(ref refusal) if refusal.reason() == "board::allocator-empty"),
        "one transient witness movement is retried against a fresh terminal board outcome",
    );
    attempts = 0;
    let peer_race = retry_task_next(|| {
        attempts += 1;
        if attempts == 1 {
            refusal(
                "board::allocator-awaits-peer",
                None,
                "the refreshed board must report the peer wait",
            )
        } else {
            refusal(
                "board::allocator-empty",
                None,
                "the refreshed outcome ends the retry",
            )
        }
    });
    assert_eq!(attempts, TASK_NEXT_ATTEMPTS);
    assert!(
        matches!(peer_race, Outcome::Refusal(ref refusal) if refusal.reason() == "board::allocator-empty"),
        "a peer becoming eligible during claim retries once so the refreshed typed decision can answer",
    );
    attempts = 0;
    let exhausted = retry_task_next(|| {
        attempts += 1;
        moved("board::witness-moved: allocator subscription head changed")
    });
    assert_eq!(attempts, TASK_NEXT_ATTEMPTS);
    assert!(
        matches!(exhausted, Outcome::Refusal(ref refusal) if refusal.reason() == "board::witness-moved"),
        "the fixed retry bound preserves the final atomic refusal",
    );
    attempts = 0;
    let terminal = retry_task_next(|| {
        attempts += 1;
        moved("board::witness-moved: allocator winner changed")
    });
    assert_eq!(attempts, 1);
    assert!(
        matches!(terminal, Outcome::Refusal(ref refusal) if refusal.explanation() == "board::witness-moved: allocator winner changed"),
        "semantic winner movement returns its first refusal without replanning",
    );

    clean(&root, bus);
}

/// Drive the production claim path up to the point it has decided.
///
/// The allocator config is the one `dispatch` builds, so the plan is the plan
/// the ordinary command door would have made.
fn plan_claim(bus: &Bus, command: &wire::AdmittedCommand) -> ClaimPlan<'static> {
    match claim_plan(
        bus,
        command,
        &next_rule(),
        None,
        &board::NextConfig::default(),
        None,
    ) {
        Ok(plan) => plan,
        Err(outcome) => panic!("the claim plan did not decide: {outcome:?}"),
    }
}

fn commit_planned_claim(bus: &Bus, command: &wire::AdmittedCommand, plan: ClaimPlan) -> String {
    guard_refusal(commit_claim(bus, command, &next_rule(), plan))
}

/// `task next` preserves the one-active-claim invariant before it mints.
///
/// The allocator can still see unheld work after this actor wins the first
/// task. Without the active-claim precondition after selection it would append
/// another `ClaimWon` for the same actor, making every progress surface
/// ambiguous.
#[test]
fn task_next_refuses_when_the_actor_already_holds_one_active_claim() {
    let (root, bus, epoch, seats) = fixture("claim-one-active-plan", 1);
    let actor = Actor::Lane(seats[0].lane);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    add(&bus, epoch, actor.clone(), "ACTIVE-1", "first", None);
    add(&bus, epoch, actor.clone(), "ACTIVE-2", "second", None);

    let answer = board(&bus, epoch, actor.clone());
    let first = guard_winner(&answer, &actor);
    assert_committed(issue(
        &bus,
        epoch,
        actor.clone(),
        CommandKind::ClaimWin,
        claim_expected(&answer, &first, &seats[0]),
        &next_rule(),
    ));

    let held = board(&bus, epoch, actor.clone());
    let second = held
        .tasks
        .iter()
        .map(|task| task.statement.body.handle.clone())
        .find(|handle| *handle != first)
        .expect("fixture keeps another task selectable");
    assert!(
        at(&held, &first).latest_claim().is_some(),
        "first task is held"
    );
    assert!(
        at(&held, &second).latest_claim().is_none(),
        "second task is still free"
    );
    let refused = issue(
        &bus,
        epoch,
        actor.clone(),
        CommandKind::ClaimWin,
        claim_expected(&held, &second, &seats[0]),
        &next_rule(),
    );
    assert!(
        matches!(
            refused,
            Outcome::Refusal(ref refusal) if refusal.reason() == "claim::active-exists"
        ),
        "a second claim for the same actor was not refused cleanly: {refused:?}"
    );
    let after = board(&bus, epoch, actor.clone());
    assert!(at(&after, &second).latest_claim().is_none());
    clean(&root, bus);
}

/// The allocator commit guard rechecks one-active-claim on its atomic read.
///
/// This hand-built observation deliberately matches the current head set, so
/// the ordinary head-set guard passes. The only remaining blocker is the
/// current active claim for the same actor, observed inside the write guard
/// immediately before a second claim would be minted.
#[test]
fn an_active_claim_in_the_commit_read_refuses_even_when_heads_match() {
    let (root, bus, epoch, seats) = fixture("claim-one-active-guard", 1);
    let actor = Actor::Lane(seats[0].lane);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    add(&bus, epoch, actor.clone(), "ACTIVE-guard1", "first", None);
    add(&bus, epoch, actor.clone(), "ACTIVE-guard2", "second", None);

    let answer = board(&bus, epoch, actor.clone());
    let first = guard_winner(&answer, &actor);
    assert_committed(issue(
        &bus,
        epoch,
        actor.clone(),
        CommandKind::ClaimWin,
        claim_expected(&answer, &first, &seats[0]),
        &next_rule(),
    ));

    let current = board(&bus, epoch, actor.clone());
    let second = current
        .tasks
        .iter()
        .map(|task| task.statement.body.handle.clone())
        .find(|handle| *handle != first)
        .expect("fixture keeps another task selectable");
    let observed = guard_observation(&current, &actor, second);
    assert_eq!(
        refusal_reason(commit_guarded(&bus, epoch, observed)),
        format!("claim::active-exists: actor {actor} already holds active claim {first}")
    );
    clean(&root, bus);
}

#[test]
fn capture_and_steal_refuse_when_the_actor_already_holds_an_active_claim() {
    let (root, bus, epoch, seats) = fixture("claim-active-named-rules", 2);
    let actor = Actor::Lane(seats[0].lane);
    let rival = Actor::Lane(seats[1].lane);
    let active = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "ACTIVE-rule1",
        "Already held work",
        Some("todo"),
    );
    let free = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "ACTIVE-rule2",
        "Free capture target",
        Some("todo"),
    );
    let stolen = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "ACTIVE-rule3",
        "Held steal target",
        Some("todo"),
    );

    capture_for(&bus, epoch, &seats[0], &active);
    capture_for(&bus, epoch, &seats[1], &stolen);

    let capture: Name = "capture".parse().expect("capture rule");
    let steal: Name = "steal".parse().expect("steal rule");
    for (task, rule) in [(&free, capture), (&stolen, steal)] {
        let visible = board(&bus, epoch, actor.clone());
        let heads = at(&visible, task);
        let refused = issue(
            &bus,
            epoch,
            actor.clone(),
            CommandKind::ClaimWin,
            vec![
                statement_witness(heads),
                workflow_witness(heads),
                claim_witness(heads),
                Witness::Eligibility { rule: rule.clone() },
                residency(&visible, &seats[0]),
            ],
            &ClaimWin {
                rule,
                recovery: None,
            },
        );
        assert!(
            matches!(
                refused,
                Outcome::Refusal(ref refusal) if refusal.reason() == "claim::active-exists"
            ),
            "named claim rule appended or refused for the wrong reason: {refused:?}",
        );
    }
    let after = board(&bus, epoch, actor);
    assert!(at(&after, &free).latest_claim().is_none());
    assert_eq!(
        at(&after, &stolen)
            .latest_claim()
            .map(|held| &held.claim)
            .unwrap()
            .body
            .actor,
        rival
    );
    clean(&root, bus);
}

#[test]
fn task_status_and_progress_select_the_actor_claim_when_peers_are_active() {
    let (root, bus, epoch, seats) = fixture("task-active-peer-progress", 2);
    let actor = Actor::Lane(seats[0].lane);
    let peer = Actor::Lane(seats[1].lane);
    let held = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-progress01",
        "Advance the held row",
        Some("todo"),
    );
    let peer_held = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-progress02",
        "Peer active row",
        Some("todo"),
    );

    let actor_board = board(&bus, epoch, actor.clone());
    assert_committed(issue(
        &bus,
        epoch,
        actor.clone(),
        CommandKind::ClaimWin,
        claim_expected(&actor_board, &held, &seats[0]),
        &next_rule(),
    ));
    let peer_board = board(&bus, epoch, peer.clone());
    assert_committed(issue(
        &bus,
        epoch,
        peer,
        CommandKind::ClaimWin,
        claim_expected(&peer_board, &peer_held, &seats[1]),
        &next_rule(),
    ));

    let answer = board(&bus, epoch, actor);
    let status = task_status(&answer).expect("status reads");
    assert!(
        status
            .lines()
            .any(|line| line.starts_with(&format!("claim {held} "))),
        "{status}"
    );
    assert!(!status.contains(&peer_held.to_string()), "{status}");
    assert!(status.lines().any(|line| line == "active 2"), "{status}");
    let actor_claims = answer
        .tasks
        .iter()
        .filter(|task| {
            task.latest_claim()
                .map(|held| &held.claim)
                .is_some_and(|claim| {
                    claim.body.actor == answer.principal
                        && claim.body.disposition == ClaimDisposition::Active
                })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actor_claims.len(),
        1,
        "actor claim is ambiguous: {answer:?}"
    );
    let heads = actor_claims[0];
    assert_eq!(heads.statement.body.handle, held);
    let witnesses = progress_witnesses(&answer, heads, seats[0].lane, seats[0].occupant, false)
        .expect("progress witnesses are derived from the actor claim");
    assert!(
        witnesses.iter().any(|witness| matches!(
            witness,
            Witness::Claim { task, claimed: Some(_), head: Some(_) } if *task == heads.statement.body.born
        )),
        "progress witnesses did not include the held claim: {witnesses:?}",
    );
    clean(&root, bus);
}

/// Reaches the head-set comparison in `read_guard.rs` through the claim path.
///
/// The competing task is added through `dispatch`, the same door production
/// adds tasks through, after the claim is decided and before it is written. It
/// moves no head the command witnessed and does not reseat, so the guard passes
/// `witnesses_hold` and the residency comparison, and this is the comparison
/// left to refuse.
#[test]
fn a_task_added_between_the_claim_plan_and_its_commit_refuses_the_claim() {
    let (root, bus, epoch, seats) = fixture("claim-plan-heads", 1);
    let actor = Actor::Lane(seats[0].lane);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    add(&bus, epoch, actor.clone(), "GUARD-1", "first", None);
    add(&bus, epoch, actor.clone(), "GUARD-2", "second", None);

    let answer = board(&bus, epoch, actor.clone());
    let winner = guard_winner(&answer, &actor);
    let command = claim_next_command(epoch, &actor, &answer, &winner, &seats[0]);
    let plan = plan_claim(&bus, &command);
    assert_eq!(
        plan.selected.handle(),
        &winner,
        "the production plan decided the winner this command witnessed"
    );

    add(&bus, epoch, actor.clone(), "GUARD-3", "arrived late", None);

    let moved = board(&bus, epoch, actor.clone());
    assert_ne!(moved.cursor, answer.cursor);
    assert_eq!(
        moved.residency.position, answer.residency.position,
        "the added task must leave the residency head still, or the residency \
         comparison ahead of the head set would be the one refusing"
    );

    assert_eq!(
        commit_planned_claim(&bus, &command, plan),
        "board::witness-moved: allocator projection changed"
    );
    assert!(
        at(&board(&bus, epoch, actor), &winner)
            .latest_claim()
            .is_none(),
        "the refused claim left no claim head behind"
    );
    clean(&root, bus);
}

/// Reaches the residency comparison in `read_guard.rs` through the claim path.
///
/// Re-seating rewrites the residency head over the same epoch, lane, occupant
/// and disposition, which is every field `Witness::Residency` pins, so the
/// command's own witnesses still hold in `witnesses_hold` and the moved head
/// position is only visible to this comparison.
#[test]
fn a_reseat_between_the_claim_plan_and_its_commit_refuses_the_claim() {
    let (root, bus, epoch, seats) = fixture("claim-plan-residency", 1);
    let actor = Actor::Lane(seats[0].lane);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    add(&bus, epoch, actor.clone(), "GUARD-1", "first", None);
    add(&bus, epoch, actor.clone(), "GUARD-2", "second", None);

    let answer = board(&bus, epoch, actor.clone());
    let winner = guard_winner(&answer, &actor);
    let command = claim_next_command(epoch, &actor, &answer, &winner, &seats[0]);
    let plan = plan_claim(&bus, &command);

    reseat(&bus, epoch, &seats);

    let moved = board(&bus, epoch, actor.clone());
    assert_ne!(
        moved.residency.position, answer.residency.position,
        "re-seating moved the residency head the guard re-reads"
    );
    assert_eq!(
        commit_planned_claim(&bus, &command, plan),
        "board::witness-moved: allocator subscription head changed"
    );
    assert!(
        at(&board(&bus, epoch, actor), &winner)
            .latest_claim()
            .is_none(),
        "the refused claim left no claim head behind"
    );
    clean(&root, bus);
}

/// Pins the premise the two assertions above depend on.
///
/// `witnesses_hold` in `read_guard.rs` runs before every allocator
/// comparison, so a claim whose own witnessed head moved refuses there and
/// never reaches one. Both scenarios above would still "refuse" if they landed
/// here instead, which is why neither of them settles for that.
#[test]
fn a_moved_witness_refuses_before_any_allocator_comparison_is_reached() {
    let (root, bus, epoch, seats) = fixture("claim-plan-witness", 1);
    let actor = Actor::Lane(seats[0].lane);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    add(&bus, epoch, actor.clone(), "GUARD-1", "first", None);
    add(&bus, epoch, actor.clone(), "GUARD-2", "second", None);

    let answer = board(&bus, epoch, actor.clone());
    let winner = guard_winner(&answer, &actor);
    let command = claim_next_command(epoch, &actor, &answer, &winner, &seats[0]);
    let plan = plan_claim(&bus, &command);

    assert_committed(issue(
        &bus,
        epoch,
        actor.clone(),
        CommandKind::TaskEdit,
        vec![statement_witness(at(&answer, &winner))],
        &TaskEdit {
            title: Some("Retitled while the claim was in flight".to_owned()),
            ..only(&winner)
        },
    ));

    let refusal = commit_planned_claim(&bus, &command, plan);
    assert!(
        refusal.contains(
            winner
                .born()
                .expect("fixture winner carries a birth stamp")
                .as_str()
        ),
        "the refusal names the client witness that moved, not a comparison: {refusal}"
    );
    assert!(
        !refusal.contains("allocator"),
        "the moved witness refuses ahead of every allocator comparison: {refusal}"
    );
    clean(&root, bus);
}
