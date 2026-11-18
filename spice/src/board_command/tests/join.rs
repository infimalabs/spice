use super::completion_target::set_target;
use super::resolution::claim_task;
use super::*;

use wire::intent::TaskJoin;
use wire::{Disposition, SeatAddress, TaskInviteBinding, TaskInviteControl};

mod invite;
mod resource_join;

fn target_of(bus: &Bus, epoch: CanonicalUuid, task: &TaskHandle) -> u32 {
    let view = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&view, task);
    heads
        .completion
        .body
        .target(&heads.workflow.body.phase)
        .get()
}

fn active_claims(bus: &Bus, epoch: CanonicalUuid, task: &TaskHandle) -> usize {
    let view = board(bus, epoch, Actor::TEST_PRINCIPAL);
    at(&view, task).active_claims().count()
}

fn direct_join(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    task: &TaskHandle,
) -> Outcome<CommandAnswer> {
    let actor = Actor::Lane(seat.lane);
    let view = board(bus, epoch, actor.clone());
    let (kind, expected, body) =
        match crate::board_command::direct_join_request(&view, task.clone()) {
            Ok(mapped) => mapped,
            Err(outcome) => return outcome,
        };
    issue(bus, epoch, actor, kind, expected, &body)
}

fn prove_addresses(bus: &Bus, epoch: CanonicalUuid, seats: &[Seat]) {
    let origin = format!("resident:{epoch}")
        .parse()
        .expect("resident fixture origin");
    let mut write = JournalWrite::new(
        Actor::Spice,
        origin,
        fixture_instant("invite address proof"),
        Producer::ResidentRecognition,
    );
    for (group, seat) in seats.iter().enumerate() {
        write
            .fact(
                body::LaneStarted {
                    lane: seat.lane,
                    occupant: seat.occupant,
                    epoch,
                    group: group as u32 + 1,
                    fresh_session: true,
                },
                FactCause::Root,
            )
            .expect("fixture occupant birth builds");
    }
    assert!(matches!(
        bus.commit(write).expect("fixture births reach writer"),
        CommitOutcome::Committed(_)
    ));
}

fn send_invitation(
    bus: &Bus,
    epoch: CanonicalUuid,
    inviter: &Seat,
    recipient: &Seat,
    task: &TaskHandle,
    key: &str,
) -> CanonicalUuid {
    let view = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&view, task);
    let claim = heads
        .held_by(&Actor::Lane(inviter.lane))
        .expect("inviter holds the task");
    let address = match view.addresses.get(&inviter.lane) {
        Some(SeatAddress::Seated { address, .. }) => *address,
        other => panic!("inviter has one proved address: {other:?}"),
    };
    let control = TaskInviteControl::new(TaskInviteBinding {
        key,
        task: &heads.statement.body.born,
        phase: &heads.workflow.body.phase,
        generation: heads.workflow.body.generation,
        inviter_lane: inviter.lane,
        inviter_occupant: address,
        inviter_claim: claim.claim.body.fact,
        recipient_lane: recipient.lane,
    });
    let directive = uuid(NEXT_COMMAND.fetch_add(1, Ordering::Relaxed));
    let published = fixture_instant("invitation publication");
    let expires = fixture_instant_after(published, 60 * 60);
    let sent = body::SteerSent {
        sender: Actor::Lane(inviter.lane),
        room: Some(heads.statement.body.born.clone()),
        directive,
        fanout: None,
        recipients: Vec::new(),
        key: key.to_owned(),
        recipient: Actor::Lane(recipient.lane),
        text: format!("Join {task}"),
        control: Some(control.to_string()),
        attachments: Vec::new(),
        expires,
    };
    let minted_micros = key
        .parse::<steer::Key>()
        .expect("current steering key")
        .minted_micros();
    let origin: wire::Origin = format!("ack:{key}").parse().expect("invitation origin");
    let mut write = JournalWrite::new(
        Actor::Lane(inviter.lane),
        origin.clone(),
        published,
        Producer::CommandDoor,
    );
    let pending = write
        .fact(sent.clone(), FactCause::Root)
        .expect("invitation fact builds");
    write.steering_head(pending, move |created| wire::SteeringHead {
        sender: sent.sender,
        room: sent.room,
        directive: sent.directive,
        fanout: sent.fanout,
        recipients: Vec::new(),
        key: sent.key,
        recipient: sent.recipient,
        text: sent.text,
        control: sent.control,
        attachments: sent.attachments,
        published,
        expires: sent.expires,
        stage: body::SteeringStage::Published,
        escalations: Vec::new(),
        disposition: Disposition::Live,
        origin,
        created,
        minted_micros,
    });
    assert!(matches!(
        bus.commit(write).expect("invitation commits"),
        CommitOutcome::Committed(_)
    ));
    directive
}

fn invited_join_with_disposition(
    bus: &Bus,
    epoch: CanonicalUuid,
    recipient: &Seat,
    task: &TaskHandle,
    key: &str,
    directive: CanonicalUuid,
    disposition: Disposition,
) -> Outcome<CommandAnswer> {
    let actor = Actor::Lane(recipient.lane);
    let view = board(bus, epoch, actor.clone());
    let heads = at(&view, task);
    let control = bus
        .read({
            let key = key.to_owned();
            move |read| read.steering_key(&key)
        })
        .expect("steering index reads")
        .expect("invitation is indexed")
        .body()
        .control
        .as_deref()
        .expect("invitation has control")
        .parse::<TaskInviteControl>()
        .expect("invitation control parses");
    let inviter_claim = heads
        .active_claims()
        .find(|claim| claim.claim.body.claimed == control.claim())
        .expect("inviter claim is active");
    let inviter = inviter_claim.claim.body.lane;
    let inviter_occupant = view
        .residency
        .body
        .lanes
        .iter()
        .find(|row| row.lane == inviter)
        .and_then(|row| row.occupant)
        .expect("inviter remains seated");
    issue(
        bus,
        epoch,
        actor,
        CommandKind::TaskJoin,
        vec![
            statement_witness(heads),
            workflow_witness(heads),
            claim_witness(heads),
            inviter_claim.witness(&heads.statement.body.born),
            Witness::Eligibility {
                rule: "join".parse().expect("join eligibility rule"),
            },
            residency(&view, recipient),
            residency_witness(&view, inviter, inviter_occupant),
            Witness::Directive {
                steer: directive,
                disposition,
            },
        ],
        &TaskJoin::Invitation {
            key: key.to_owned(),
        },
    )
}
fn invited_join(
    bus: &Bus,
    epoch: CanonicalUuid,
    recipient: &Seat,
    task: &TaskHandle,
    key: &str,
    directive: CanonicalUuid,
) -> Outcome<CommandAnswer> {
    invited_join_with_disposition(
        bus,
        epoch,
        recipient,
        task,
        key,
        directive,
        Disposition::Live,
    )
}

fn expired_invited_join(
    bus: &Bus,
    epoch: CanonicalUuid,
    recipient: &Seat,
    task: &TaskHandle,
    key: &str,
    directive: CanonicalUuid,
) -> Outcome<CommandAnswer> {
    let actor = Actor::Lane(recipient.lane);
    let view = board(bus, epoch, actor.clone());
    let heads = at(&view, task);
    issue(
        bus,
        epoch,
        actor,
        CommandKind::TaskJoin,
        vec![
            statement_witness(heads),
            workflow_witness(heads),
            claim_witness(heads),
            Witness::Eligibility {
                rule: "join".parse().expect("join eligibility rule"),
            },
            residency(&view, recipient),
            Witness::Directive {
                steer: directive,
                disposition: Disposition::Live,
            },
        ],
        &TaskJoin::Invitation {
            key: key.to_owned(),
        },
    )
}

fn steering_disposition(bus: &Bus, key: &str) -> Disposition {
    bus.read({
        let key = key.to_owned();
        move |read| read.steering_key(&key)
    })
    .expect("steering index reads")
    .expect("invitation is indexed")
    .body()
    .disposition
}
fn move_resident_epoch(bus: &Bus, epoch: CanonicalUuid, successor: CanonicalUuid) {
    let current = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let mut residency = current.residency.body;
    residency.epoch = successor;
    residency.topology_revision = residency.topology_revision.saturating_add(1);
    let origin = format!("resident:{successor}")
        .parse()
        .expect("successor resident origin");
    let mut write = JournalWrite::new(
        Actor::Spice,
        origin,
        fixture_instant("successor resident seating"),
        Producer::CommandDoor,
    );
    let seated = write
        .fact(body::UpSeated { epoch: successor }, FactCause::Root)
        .expect("successor seating fact builds");
    write.residency_head(seated, move |_| residency);
    assert!(matches!(
        bus.commit(write).expect("successor residency commits"),
        CommitOutcome::Committed(_)
    ));
}

fn replace_occupant(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, occupant: CanonicalUuid) -> Seat {
    let current = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let mut residency = current.residency.body;
    let row = residency
        .lanes
        .iter_mut()
        .find(|row| row.lane == seat.lane)
        .expect("fixture lane remains present");
    row.occupant = Some(occupant);
    row.lineage.push(occupant);
    row.capabilities.push(occupant);
    row.session = None;
    residency.topology_revision = residency.topology_revision.saturating_add(1);
    let origin = format!("resident:{epoch}")
        .parse()
        .expect("resident replacement origin");
    let mut write = JournalWrite::new(
        Actor::Spice,
        origin,
        fixture_instant("successor occupant seating"),
        Producer::ResidentRecognition,
    );
    let started = write
        .fact(
            body::LaneStarted {
                lane: seat.lane,
                occupant,
                epoch,
                group: 99,
                fresh_session: true,
            },
            FactCause::Existing(current.residency.position),
        )
        .expect("successor occupant fact builds");
    write.residency_head(started, move |_| residency);
    assert!(matches!(
        bus.commit(write).expect("successor occupant commits"),
        CommitOutcome::Committed(_)
    ));
    Seat {
        lane: seat.lane,
        team: seat.team,
        occupant,
    }
}

#[test]
fn direct_join_consumes_only_existing_capacity() {
    let (root, bus, epoch, seats) = fixture("task-direct-join", 3);
    let holder = Actor::Lane(seats[0].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "JOIN-direct1",
        "Join one open task slot",
        Some("todo"),
    );
    claim_task(&bus, epoch, &seats[0], &holder, &task);

    let full = direct_join(&bus, epoch, &seats[1], &task);
    assert_eq!(refusal_reason(&full), "task::join-full");
    assert_eq!(
        (
            target_of(&bus, epoch, &task),
            active_claims(&bus, epoch, &task)
        ),
        (1, 1)
    );

    assert_committed(set_target(
        &bus,
        epoch,
        &Actor::TEST_PRINCIPAL,
        &task,
        ("todo", 2),
    ));
    assert_committed(direct_join(&bus, epoch, &seats[1], &task));
    assert_eq!(
        (
            target_of(&bus, epoch, &task),
            active_claims(&bus, epoch, &task)
        ),
        (2, 2)
    );
    assert_eq!(
        refusal_reason(&direct_join(&bus, epoch, &seats[2], &task)),
        "task::join-full"
    );

    let unheld = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "JOIN-unheld1",
        "Leave one task unheld",
        Some("todo"),
    );
    assert_eq!(
        refusal_reason(&direct_join(&bus, epoch, &seats[2], &unheld)),
        "task::join-unheld"
    );
    clean(&root, bus);
}

#[test]
fn invitation_acceptance_grows_only_full_capacity_and_retires_atomically() {
    let (root, bus, epoch, seats) = fixture("task-invited-join", 4);
    prove_addresses(&bus, epoch, &seats);
    for (index, (target, key)) in [(1, "1kLGp2NKx"), (2, "1kLGp2NLx")].into_iter().enumerate() {
        let task = add(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            &format!("JOIN-invite{}", index + 1),
            "Accept one task invitation",
            Some("todo"),
        );
        let inviter = &seats[index * 2];
        let recipient = &seats[index * 2 + 1];
        let holder = Actor::Lane(inviter.lane);
        claim_task(&bus, epoch, inviter, &holder, &task);
        if target == 2 {
            assert_committed(set_target(
                &bus,
                epoch,
                &Actor::TEST_PRINCIPAL,
                &task,
                ("todo", target),
            ));
        }
        let directive = send_invitation(&bus, epoch, inviter, recipient, &task, key);
        let before_targets = bus
            .transact(|journal| {
                bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
            })
            .expect("facts read")
            .facts()
            .iter()
            .filter(|fact| fact.fact.kind() == wire::FactKind::CompletionTargeted)
            .count();
        assert_committed(invited_join(&bus, epoch, recipient, &task, key, directive));
        assert_eq!(
            (
                target_of(&bus, epoch, &task),
                active_claims(&bus, epoch, &task)
            ),
            (2, 2)
        );
        assert_eq!(steering_disposition(&bus, key), Disposition::Acked);
        let settled = invited_join_with_disposition(
            &bus,
            epoch,
            recipient,
            &task,
            key,
            directive,
            Disposition::Acked,
        );
        assert_eq!(refusal_reason(&settled), "task::invite-settled");
        let after_targets = bus
            .transact(|journal| {
                bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
            })
            .expect("facts read")
            .facts()
            .iter()
            .filter(|fact| fact.fact.kind() == wire::FactKind::CompletionTargeted)
            .count();
        assert_eq!(after_targets - before_targets, usize::from(target == 1));
    }
    clean(&root, bus);
}

/// Retire one invitation exactly as its deadline or the operator's
/// cancellation would, leaving the inviter's claim and the task untouched.
fn retire_invitation(bus: &Bus, key: &str, disposition: Disposition) {
    let lookup = key.to_owned();
    let head = bus
        .read(move |read| read.steering_key(&lookup))
        .expect("steering index reads")
        .expect("invitation is indexed");
    let at = fixture_instant("invitation retirement");
    let cause = FactCause::Existing(head.body().created);
    let (actor, producer) = match disposition {
        Disposition::Lapsed => (Actor::Spice, Producer::BusTimer),
        _ => (Actor::TEST_PRINCIPAL, Producer::CommandDoor),
    };
    let mut write = JournalWrite::new(actor, head.body().origin.clone(), at, producer);
    let (pending, next) = match disposition {
        Disposition::Lapsed => {
            let lapsed = body::SteerLapsed {
                key: key.to_owned(),
                recipient: head.body().recipient.clone(),
                expires: head.body().expires,
            };
            let next =
                steer::Ledger::transition_head(head.body(), steer::Steering::Lapsed(&lapsed), at);
            (write.fact(lapsed, cause), next)
        }
        Disposition::Canceled => {
            let canceled = body::SteerCanceled {
                key: key.to_owned(),
                recipient: head.body().recipient.clone(),
                reason: None,
            };
            let next = steer::Ledger::transition_head(
                head.body(),
                steer::Steering::Canceled(&canceled),
                at,
            );
            (write.fact(canceled, cause), next)
        }
        other => panic!("an invitation retires by lapse or cancellation, not {other:?}"),
    };
    let pending = pending.expect("retirement fact builds");
    let next = next.expect("retirement transition applies");
    write.steering_head(pending, move |_| next);
    assert!(matches!(
        bus.commit(write).expect("retirement reaches writer"),
        CommitOutcome::Committed(_)
    ));
}

#[test]
fn a_keyed_join_on_a_lapsed_or_canceled_invitation_answers_expired() {
    let (root, bus, epoch, seats) = fixture("task-invite-retired", 2);
    prove_addresses(&bus, epoch, &seats);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "JOIN-retired1",
        "Answer a retired invitation",
        Some("todo"),
    );
    claim_task(&bus, epoch, &seats[0], &Actor::Lane(seats[0].lane), &task);
    for (key, disposition) in [
        ("1kLGp2NMx", Disposition::Lapsed),
        ("1kLGp2NNx", Disposition::Canceled),
    ] {
        let directive = send_invitation(&bus, epoch, &seats[0], &seats[1], &task, key);
        retire_invitation(&bus, key, disposition);
        let late = invited_join_with_disposition(
            &bus,
            epoch,
            &seats[1],
            &task,
            key,
            directive,
            disposition,
        );
        assert_eq!(
            refusal_reason(&late),
            "task::invite-expired",
            "{disposition:?}"
        );
        assert_eq!(steering_disposition(&bus, key), disposition);
        assert_eq!(
            active_claims(&bus, epoch, &task),
            1,
            "a late join claims nothing"
        );
    }
    clean(&root, bus);
}

#[test]
fn invitation_follows_resident_and_recipient_but_not_inviter_replacement() {
    let (root, bus, epoch, seats) = fixture("task-invite-lifetime", 4);
    prove_addresses(&bus, epoch, &seats);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "JOIN-lifetime1",
        "Carry one invitation across seating",
        Some("todo"),
    );
    claim_task(&bus, epoch, &seats[0], &Actor::Lane(seats[0].lane), &task);
    let key = "1kLGp2NNx";
    let directive = send_invitation(&bus, epoch, &seats[0], &seats[1], &task, key);
    let successor_epoch = register_policy_repository(&root);
    move_resident_epoch(&bus, epoch, successor_epoch);
    let successor = replace_occupant(
        &bus,
        successor_epoch,
        &seats[1],
        uuid(NEXT_COMMAND.fetch_add(1, Ordering::Relaxed)),
    );
    assert_committed(invited_join(
        &bus,
        successor_epoch,
        &successor,
        &task,
        key,
        directive,
    ));

    let stale = add(
        &bus,
        successor_epoch,
        Actor::TEST_PRINCIPAL,
        "JOIN-lifetime2",
        "Expire one invitation with its inviter",
        Some("todo"),
    );
    claim_task(
        &bus,
        successor_epoch,
        &seats[2],
        &Actor::Lane(seats[2].lane),
        &stale,
    );
    let stale_key = "1kLGp2NSx";
    let stale_directive = send_invitation(
        &bus,
        successor_epoch,
        &seats[2],
        &seats[3],
        &stale,
        stale_key,
    );
    replace_occupant(
        &bus,
        successor_epoch,
        &seats[2],
        uuid(NEXT_COMMAND.fetch_add(1, Ordering::Relaxed)),
    );
    let expired = invited_join(
        &bus,
        successor_epoch,
        &seats[3],
        &stale,
        stale_key,
        stale_directive,
    );
    assert_eq!(refusal_reason(&expired), "task::invite-expired");
    assert_eq!(steering_disposition(&bus, stale_key), Disposition::Live);
    clean(&root, bus);
}

#[test]
fn invalid_invitation_retires_nothing() {
    let (root, bus, epoch, seats) = fixture("task-expired-invite", 3);
    prove_addresses(&bus, epoch, &seats);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "JOIN-expired1",
        "Reject a stale task invitation",
        Some("todo"),
    );
    claim_task(&bus, epoch, &seats[0], &Actor::Lane(seats[0].lane), &task);
    let key = "1kLGp2NMx";
    let directive = send_invitation(&bus, epoch, &seats[0], &seats[1], &task, key);
    let occupied = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "JOIN-occupied1",
        "Keep the invitee occupied",
        Some("todo"),
    );
    claim_task(
        &bus,
        epoch,
        &seats[1],
        &Actor::Lane(seats[1].lane),
        &occupied,
    );
    let ineligible = invited_join(&bus, epoch, &seats[1], &task, key, directive);
    assert_eq!(refusal_reason(&ineligible), "claim::active-exists");
    assert_eq!(steering_disposition(&bus, key), Disposition::Live);
    free_active(
        &bus,
        epoch,
        &Actor::Lane(seats[1].lane),
        &seats[1],
        &occupied,
    );

    let wrong_lane = invited_join(&bus, epoch, &seats[2], &task, key, directive);
    assert_eq!(refusal_reason(&wrong_lane), "task::invite-expired");
    assert_eq!(steering_disposition(&bus, key), Disposition::Live);
    assert_eq!(
        (
            target_of(&bus, epoch, &task),
            active_claims(&bus, epoch, &task)
        ),
        (1, 1)
    );
    let before_generation = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task)
        .workflow
        .body
        .generation;
    let mut edit = only(&task);
    edit.description = Some("Move the invitation's workflow generation.".to_owned());
    assert_committed(correct(&bus, epoch, Actor::TEST_PRINCIPAL, &edit));
    let after_generation = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task)
        .workflow
        .body
        .generation;
    assert_eq!(after_generation, before_generation + 1);
    let stale_generation = invited_join(&bus, epoch, &seats[1], &task, key, directive);
    assert_eq!(refusal_reason(&stale_generation), "task::invite-expired");
    assert_eq!(steering_disposition(&bus, key), Disposition::Live);

    free_active(&bus, epoch, &Actor::Lane(seats[0].lane), &seats[0], &task);
    let stale_claim = expired_invited_join(&bus, epoch, &seats[1], &task, key, directive);
    assert_eq!(refusal_reason(&stale_claim), "task::invite-expired");
    assert_eq!(steering_disposition(&bus, key), Disposition::Live);

    let malformed = expired_invited_join(
        &bus,
        epoch,
        &seats[1],
        &task,
        "not-an-invitation",
        directive,
    );
    assert_eq!(refusal_reason(&malformed), "task::invite-malformed");
    clean(&root, bus);
}

fn complete(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    task: &TaskHandle,
) -> Outcome<CommandAnswer> {
    let actor = Actor::Lane(seat.lane);
    let view = board(bus, epoch, actor.clone());
    let expected = progress_witnesses(&view, at(&view, task), seat.lane, seat.occupant, false)
        .expect("completion witnesses");
    issue(
        bus,
        epoch,
        actor,
        CommandKind::TaskDone,
        expected,
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(task, format!("evidence from {}", seat.lane))
        },
    )
}

#[test]
fn a_grown_slot_leaves_with_the_participant_that_filled_it() {
    let (root, bus, epoch, seats) = fixture("task-slot-retraction", 4);
    prove_addresses(&bus, epoch, &seats);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "JOIN-retract1",
        "Withdraw a freed participant's slot",
        Some("todo,review"),
    );
    claim_task(&bus, epoch, &seats[0], &Actor::Lane(seats[0].lane), &task);
    let directive = send_invitation(&bus, epoch, &seats[0], &seats[1], &task, "1kLGp2NPx");
    assert_committed(invited_join(
        &bus,
        epoch,
        &seats[1],
        &task,
        "1kLGp2NPx",
        directive,
    ));
    assert_eq!(target_of(&bus, epoch, &task), 2);

    free_active(&bus, epoch, &Actor::Lane(seats[1].lane), &seats[1], &task);
    assert_eq!(
        (
            target_of(&bus, epoch, &task),
            active_claims(&bus, epoch, &task)
        ),
        (1, 1),
        "a freed joiner takes its slot with it"
    );
    assert_eq!(
        allocated(&bus, epoch, &Actor::Lane(seats[2].lane)),
        None,
        "the withdrawn slot is no allocation demand"
    );
    assert_eq!(
        refusal_reason(&direct_join(&bus, epoch, &seats[2], &task)),
        "task::join-full"
    );
    let directive = send_invitation(&bus, epoch, &seats[0], &seats[2], &task, "1kLGp2NQx");
    assert_committed(invited_join(
        &bus,
        epoch,
        &seats[2],
        &task,
        "1kLGp2NQx",
        directive,
    ));
    assert_eq!(target_of(&bus, epoch, &task), 2);

    assert_committed(complete(&bus, epoch, &seats[0], &task));
    let open = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&open, &task).workflow.body.phase.as_str(), "todo");
    lapse_claim(&bus, epoch, &seats[2], &task);
    let closed = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(
        at(&closed, &task).workflow.body.phase.as_str(),
        "review",
        "a lapsed joiner's slot leaves and the remaining arrival closes the round"
    );
    clean(&root, bus);
}

#[test]
fn an_operator_asked_slot_outlives_the_participant_that_freed_it() {
    let (root, bus, epoch, seats) = fixture("task-slot-asked", 3);
    prove_addresses(&bus, epoch, &seats);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "JOIN-asked1",
        "Keep an asked slot open",
        Some("todo"),
    );
    assert_committed(set_target(
        &bus,
        epoch,
        &Actor::TEST_PRINCIPAL,
        &task,
        ("todo", 2),
    ));
    claim_task(&bus, epoch, &seats[0], &Actor::Lane(seats[0].lane), &task);
    assert_committed(direct_join(&bus, epoch, &seats[1], &task));
    free_active(&bus, epoch, &Actor::Lane(seats[1].lane), &seats[1], &task);
    assert_eq!(
        (
            target_of(&bus, epoch, &task),
            active_claims(&bus, epoch, &task)
        ),
        (2, 1),
        "the operator's ask stays open"
    );
    assert_eq!(
        allocated(&bus, epoch, &Actor::Lane(seats[2].lane)).map(|(handle, _)| handle),
        Some(task.to_string()),
        "an asked slot is allocation demand again"
    );
    clean(&root, bus);
}

/// Lanes whose claims on `task` are active, in claim order.
fn active_lanes(bus: &Bus, epoch: CanonicalUuid, task: &TaskHandle) -> Vec<CanonicalUuid> {
    let view = board(bus, epoch, Actor::TEST_PRINCIPAL);
    at(&view, task)
        .claims
        .iter()
        .filter(|held| held.is_active())
        .map(|held| held.claim.body.lane)
        .collect()
}

#[test]
fn a_steal_on_a_full_round_displaces_the_newest_claim() {
    let (root, bus, epoch, seats) = fixture("task-steal-newest", 4);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "JOIN-steal1",
        "Steal into a full round",
        Some("todo"),
    );
    assert_committed(set_target(
        &bus,
        epoch,
        &Actor::TEST_PRINCIPAL,
        &task,
        ("todo", 2),
    ));
    claim_task(&bus, epoch, &seats[0], &Actor::Lane(seats[0].lane), &task);
    assert_committed(direct_join(&bus, epoch, &seats[1], &task));

    assert_committed(super::room::design_standing::win_named(
        &bus, epoch, &seats[2], &task, "steal",
    ));
    assert_eq!(
        active_lanes(&bus, epoch, &task),
        vec![seats[0].lane, seats[2].lane],
        "a steal ends the newest claim and spares the holder"
    );
    assert_committed(super::room::design_standing::win_named(
        &bus, epoch, &seats[3], &task, "steal",
    ));
    assert_eq!(
        active_lanes(&bus, epoch, &task),
        vec![seats[0].lane, seats[3].lane],
        "a counter-steal evicts the stealer"
    );
    assert_eq!(target_of(&bus, epoch, &task), 2);
    clean(&root, bus);
}
