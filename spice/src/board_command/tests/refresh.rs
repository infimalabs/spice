use super::*;

fn refresh_at(
    bus: &Bus,
    epoch: CanonicalUuid,
    lane: &Actor,
    task: &TaskHandle,
    expected: Vec<Witness>,
    at: Instant,
) -> Outcome<CommandAnswer> {
    let refresh = TaskRef { task: task.clone() };
    let command = fixture_command(
        epoch,
        lane.clone(),
        CommandKind::ClaimRefresh,
        expected,
        &refresh,
    );
    claim_refresh_with_clock(bus, &command, &refresh, None, || Ok(at))
}

pub(super) fn refresh_held(
    bus: &Bus,
    epoch: CanonicalUuid,
    lane: &Actor,
    seat: &Seat,
    task: &TaskHandle,
) -> wire::intent::ClaimAnswer {
    let held = board(bus, epoch, lane.clone());
    let heads = at(&held, task);
    let instant = fixture_instant_after(
        heads
            .latest_claim()
            .and_then(|held| held.lease.as_ref())
            .expect("active lease")
            .body
            .start,
        1,
    );
    let answer = assert_committed(refresh_at(
        bus,
        epoch,
        lane,
        task,
        refresh_witnesses(&held, task, seat),
        instant,
    ));
    let CommandAnswer::Claim(answer) = answer else {
        panic!("claim refresh returned another answer shape")
    };
    *answer
}

/// The holder's own change left `before` in place: the same claim, current,
/// with only its acknowledgement following the task generation.
pub(super) fn assert_holder_kept(heads: &BoardTask, before: &ClaimHead) {
    let mut kept = before.clone();
    kept.acknowledged = heads.workflow.body.generation;
    assert!(
        kept.acknowledged > before.acknowledged,
        "the holder's change advanced the task"
    );
    assert_eq!(
        heads
            .latest_claim()
            .map(|held| &held.claim)
            .map(|claim| &claim.body),
        Some(&kept)
    );
}

fn active_witnesses(answer: &BoardAnswer, task: &TaskHandle, seat: &Seat) -> Vec<Witness> {
    let heads = at(answer, task);
    vec![
        held_claim_witness(heads),
        held_lease_witness(heads),
        residency(answer, seat),
    ]
}

fn refresh_witnesses(answer: &BoardAnswer, task: &TaskHandle, seat: &Seat) -> Vec<Witness> {
    let heads = at(answer, task);
    let mut expected = vec![
        Witness::Task {
            task: heads.statement.body.born.clone(),
            head: heads.statement.position,
        },
        Witness::Workflow {
            task: heads.statement.body.born.clone(),
            head: heads.workflow.position,
            phase: heads.workflow.body.phase,
        },
    ];
    expected.extend(active_witnesses(answer, task, seat));
    expected
}

pub(super) fn free_active(
    bus: &Bus,
    epoch: CanonicalUuid,
    lane: &Actor,
    seat: &Seat,
    task: &TaskHandle,
) {
    let active = board(bus, epoch, lane.clone());
    assert_committed(issue(
        bus,
        epoch,
        lane.clone(),
        CommandKind::ClaimFree,
        active_witnesses(&active, task, seat),
        &TaskRef { task: task.clone() },
    ));
}

fn rewrite_description(bus: &Bus, epoch: CanonicalUuid, task: &TaskHandle, description: &str) {
    assert_committed(super::edit::correct(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &TaskEdit {
            description: Some(description.to_owned()),
            ..only(task)
        },
    ));
}

fn complete(
    bus: &Bus,
    epoch: CanonicalUuid,
    lane: &Actor,
    seat: &Seat,
    task: &TaskHandle,
) -> Outcome<CommandAnswer> {
    let held = board(bus, epoch, lane.clone());
    issue(
        bus,
        epoch,
        lane.clone(),
        CommandKind::TaskDone,
        progress_witnesses(&held, at(&held, task), seat.lane, seat.occupant, false)
            .expect("held task has progress witnesses"),
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(task, "implementation and focused checks complete")
        },
    )
}

#[test]
fn task_next_refresh_accepts_an_edited_statement_without_moving_its_lease() {
    let (root, bus, epoch, seats) = fixture("next-acknowledge", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "NEXT-ack001",
        "Accept the statement currently governing completion",
        Some("todo"),
    );
    hold(&bus, epoch, &seats[0], &lane, &task);
    rewrite_description(&bus, epoch, &task, "the operator narrowed the scope");

    let Outcome::Refusal(refusal) = complete(&bus, epoch, &lane, &seats[0], &task) else {
        panic!("completion against an unacknowledged edit was admitted")
    };
    assert_eq!(refusal.reason(), "claim::generation-stale");
    let before = board(&bus, epoch, lane.clone());
    let lease = at(&before, &task)
        .latest_claim()
        .and_then(|held| held.lease.clone())
        .expect("active lease");

    let answer = refresh_held(&bus, epoch, &lane, &seats[0], &task);
    let current = board(&bus, epoch, lane.clone());
    let heads = at(&current, &task);
    assert_eq!(answer.acknowledged, Some(heads.workflow.body.generation));
    assert_eq!(
        answer
            .statement
            .as_ref()
            .and_then(|statement| statement.description.as_deref()),
        Some("the operator narrowed the scope")
    );
    assert_eq!(
        heads.latest_claim().and_then(|held| held.lease.as_ref()),
        Some(&lease)
    );
    assert!(
        !heads
            .latest_claim()
            .map(|held| &held.claim)
            .expect("active claim")
            .body
            .is_stale(&heads.workflow.body)
    );
    assert!(
        bus.transact(|journal| bus::JournalTransaction::settled(
            journal.fact_snapshot().expect("journal snapshot")
        ))
        .expect("refresh facts read")
        .facts()
        .iter()
        .any(|fact| fact.fact.kind() == wire::FactKind::ClaimAcknowledged)
    );
    assert_committed(complete(&bus, epoch, &lane, &seats[0], &task));
    clean(&root, bus);
}

fn move_residency(
    bus: &Bus,
    old_epoch: CanonicalUuid,
    lane: &Actor,
    successor_epoch: CanonicalUuid,
) {
    let current = board(bus, old_epoch, lane.clone());
    let mut residency = current.residency.body;
    residency.epoch = successor_epoch;
    residency.topology_revision = residency.topology_revision.saturating_add(1);
    let mut write = JournalWrite::new(
        Actor::Spice,
        format!("resident:{successor_epoch}")
            .parse()
            .expect("resident origin"),
        fixture_instant("successor resident seating"),
        Producer::CommandDoor,
    );
    let seated = write
        .fact(
            body::UpSeated {
                epoch: successor_epoch,
            },
            FactCause::Root,
        )
        .expect("successor seating fact builds");
    write.residency_head(seated, move |_| residency);
    assert!(matches!(
        bus.commit(write).expect("successor residency commits"),
        CommitOutcome::Committed(_)
    ));
}

#[test]
fn task_next_rebinds_a_held_claim_after_resident_generation_moves() {
    let (root, bus, epoch, seats) = fixture("next-rebind", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "NEXT-rebind1",
        "Keep held work across resident replacement",
        Some("todo"),
    );
    hold(&bus, epoch, &seats[0], &lane, &task);
    let before = board(&bus, epoch, lane.clone());
    let lease = at(&before, &task)
        .latest_claim()
        .and_then(|held| held.lease.clone())
        .expect("active lease");
    let successor_epoch = register_policy_repository(&root);
    move_residency(&bus, epoch, &lane, successor_epoch);

    let moved = board(&bus, successor_epoch, lane.clone());
    let instant = fixture_instant_after(lease.body.start, 1);
    let answer = assert_committed(refresh_at(
        &bus,
        successor_epoch,
        &lane,
        &task,
        refresh_witnesses(&moved, &task, &seats[0]),
        instant,
    ));
    let CommandAnswer::Claim(answer) = answer else {
        panic!("claim refresh returned another answer shape")
    };
    assert_eq!(answer.acknowledged, None);
    let rebound = board(&bus, successor_epoch, lane.clone());
    let heads = at(&rebound, &task);
    let claim = heads
        .latest_claim()
        .map(|held| &held.claim)
        .expect("rebound claim");
    let rebound_lease = heads
        .latest_claim()
        .and_then(|held| held.lease.as_ref())
        .expect("rebound lease");
    assert_eq!(claim.body.epoch, successor_epoch);
    assert_eq!(rebound_lease.body.epoch, successor_epoch);
    assert_eq!(rebound_lease.body.expiry, lease.body.expiry);
    assert_eq!(rebound_lease.body.maximum, lease.body.maximum);
    assert_committed(complete(&bus, successor_epoch, &lane, &seats[0], &task));
    clean(&root, bus);
}
