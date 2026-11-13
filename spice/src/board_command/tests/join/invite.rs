use super::*;

use crate::board_command::JoiningFixture;
use wire::OccupantAddress;
use wire::intent::{TaskInvite, TaskInviteAnswer, TaskInviteDelivery};

mod exclusivity;

fn joining(root: &Path) -> JoiningFixture {
    fs::create_dir_all(root.join(".git")).expect("fixture repository marker exists");
    JoiningFixture::discover(root)
}

/// The shared fixture with every lane on the first seat's team, the only lanes
/// an unaddressed invitation recruits from.
fn team_fixture(name: &str, lane_count: usize) -> (FixtureRoot, Bus, CanonicalUuid, Vec<Seat>) {
    let (root, bus, epoch, mut seats) = fixture(name, lane_count);
    let team = seats[0].team;
    reseat(&bus, epoch, |residency| {
        residency
            .subscriptions
            .retain(|subscription| subscription.team == team);
        for row in &mut residency.lanes {
            row.team = team;
        }
    });
    for seat in &mut seats {
        seat.team = team;
    }
    (root, bus, epoch, seats)
}

fn invite(
    bus: &Bus,
    (epoch, joining): (CanonicalUuid, &JoiningFixture),
    inviter: &Seat,
    task: &TaskHandle,
    recipient: Option<OccupantAddress>,
) -> Outcome<CommandAnswer> {
    let actor = Actor::Lane(inviter.lane);
    let view = board(bus, epoch, actor.clone());
    let (kind, expected, body) =
        match crate::board_command::invitation_offer(&view, Some(task), recipient, None) {
            Ok(mapped) => mapped,
            Err(outcome) => return outcome,
        };
    let intent: TaskInvite = serde_json::from_value(body).expect("invitation intent decodes");
    let command = fixture_command(epoch, actor, kind, expected, &intent);
    crate::board_command::task_invite(bus, &command, (&intent, None), joining.joining(), None).0
}

fn delivered(outcome: Outcome<CommandAnswer>) -> TaskInviteAnswer {
    match assert_committed(outcome) {
        CommandAnswer::TaskInvite(answer) => *answer,
        other => panic!("expected an invitation answer: {other:?}"),
    }
}

fn invited_key(answer: &TaskInviteAnswer) -> &str {
    match &answer.delivery {
        TaskInviteDelivery::Invited { key, .. } => key,
        other => panic!("expected an occupied-lane invitation: {other:?}"),
    }
}

fn steering(bus: &Bus, key: &str) -> wire::SteeringHead {
    bus.read({
        let key = key.to_owned();
        move |read| read.steering_key(&key)
    })
    .expect("steering index reads")
    .expect("invitation is indexed")
    .body()
    .clone()
}

fn pending_keys(bus: &Bus, lane: CanonicalUuid, head: &wire::SteeringHead) -> Vec<String> {
    let ledger =
        steer::Ledger::from_authority_heads([head]).expect("fixture steering authority is valid");
    crate::resident::pending_presentations(
        &ledger,
        &std::collections::BTreeSet::new(),
        &std::collections::BTreeMap::new(),
        &bus.task_snapshot().expect("task authority reads"),
        &bus.read(|read| read.principal_roles())
            .expect("principal roles read"),
        lane,
    )
    .expect("pending presentation projects")
    .into_iter()
    .map(|directive| directive.key)
    .collect()
}

fn address(bus: &Bus, epoch: CanonicalUuid, seat: &Seat) -> OccupantAddress {
    match board(bus, epoch, Actor::TEST_PRINCIPAL)
        .addresses
        .get(&seat.lane)
    {
        Some(SeatAddress::Seated { address, .. }) => *address,
        other => panic!("seat has one proved address: {other:?}"),
    }
}

fn reseat(bus: &Bus, epoch: CanonicalUuid, edit: impl FnOnce(&mut ResidencyHead)) {
    let mut residency = board(bus, epoch, Actor::TEST_PRINCIPAL).residency.body;
    edit(&mut residency);
    residency.topology_revision = residency.topology_revision.saturating_add(1);
    let origin = format!("resident:{epoch}")
        .parse()
        .expect("resident fixture origin");
    let mut write = JournalWrite::new(
        Actor::Spice,
        origin,
        fixture_instant("invite residency change"),
        Producer::CommandDoor,
    );
    let seated = write
        .fact(body::UpSeated { epoch }, FactCause::Root)
        .expect("residency change fact builds");
    write.residency_head(seated, move |_| residency);
    assert!(matches!(
        bus.commit(write).expect("residency change reaches writer"),
        CommitOutcome::Committed(_)
    ));
}

fn row<'a>(residency: &'a mut ResidencyHead, seat: &Seat) -> &'a mut wire::LaneResidency {
    residency
        .lanes
        .iter_mut()
        .find(|row| row.lane == seat.lane)
        .expect("fixture lane remains present")
}

fn claimed(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, title: &str) -> TaskHandle {
    let task = add(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        title,
        "Keep one lane busy",
        Some("todo"),
    );
    claim_task(bus, epoch, seat, &Actor::Lane(seat.lane), &task);
    task
}

#[test]
fn invitations_preserve_the_holders_phase_and_authority() {
    for phase in ["plan", "design"] {
        for addressed in [false, true] {
            let (root, bus, epoch, seats) = team_fixture("task-invite-planning", 2);
            let joining = joining(&root);
            prove_addresses(&bus, epoch, &seats);
            let task = add(
                &bus,
                epoch,
                Actor::TEST_PRINCIPAL,
                "INVITE-planning",
                "Decide the invitation work",
                Some(&format!("{phase},todo")),
            );
            claim_task(&bus, epoch, &seats[0], &Actor::Lane(seats[0].lane), &task);
            let recipient = addressed.then(|| address(&bus, epoch, &seats[1]));
            let answer = delivered(invite(&bus, (epoch, &joining), &seats[0], &task, recipient));
            let key = invited_key(&answer);
            assert_eq!(
                steering(&bus, key)
                    .control
                    .expect("invitation carries typed control")
                    .parse::<TaskInviteControl>()
                    .expect("invitation control parses")
                    .phase()
                    .as_str(),
                phase
            );
            assert_committed(invited_join(
                &bus,
                epoch,
                &seats[1],
                &task,
                key,
                steering(&bus, key).directive,
            ));
            let view = board(&bus, epoch, Actor::Lane(seats[1].lane));
            let gate::ClaimCapture::Active { phase: held, .. } = claim_capture_from_board(&view)
            else {
                panic!("the partner holds the invited task");
            };
            assert_eq!(held.as_str(), phase);
            assert!(gate::lacks_commit_authority(held.as_str()));
            assert_eq!(active_claims(&bus, epoch, &task), 2);
            clean(&root, bus);
        }
    }
}

#[test]
fn planning_holders_cannot_join_another_task_by_handle_or_invitation() {
    for phase in ["plan", "design"] {
        let (root, bus, epoch, seats) = fixture("planning-join-refusal", 2);
        let joining = joining(&root);
        prove_addresses(&bus, epoch, &seats);
        let other = claimed(&bus, epoch, &seats[1], "INVITE-other");
        let planning = add(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            "INVITE-planning",
            "Keep the planning boundary",
            Some(&format!("{phase},todo")),
        );
        let answer = delivered(invite(
            &bus,
            (epoch, &joining),
            &seats[1],
            &other,
            Some(address(&bus, epoch, &seats[0])),
        ));
        claim_task(
            &bus,
            epoch,
            &seats[0],
            &Actor::Lane(seats[0].lane),
            &planning,
        );
        let key = invited_key(&answer);
        let head = bus
            .read({
                let key = key.to_owned();
                move |read| read.steering_key(&key)
            })
            .expect("steering index reads")
            .expect("invitation is indexed");
        let key = key.parse::<steer::Key>().expect("invitation key parses");
        let control = live_invitation(&head, &key)
            .unwrap_or_else(|outcome| panic!("the offer is live: {outcome:?}"));
        let view = board(&bus, epoch, Actor::Lane(seats[0].lane));
        for request in [
            crate::board_command::direct_join_request(&view, other.clone()),
            crate::board_command::invitation_join(&view, &key, &head, &control),
        ] {
            let outcome = request.expect_err("planning cannot move into other work");
            assert_eq!(refusal_reason(&outcome), "task::planning-implementation");
        }
        assert_eq!(active_claims(&bus, epoch, &planning), 1);
        assert_eq!(active_claims(&bus, epoch, &other), 1);
        assert_eq!(
            steering(&bus, &key.to_string()).disposition,
            Disposition::Live
        );
        clean(&root, bus);
    }
}

#[test]
fn occupied_recipient_gets_one_typed_invitation_and_joins_only_on_acceptance() {
    let (root, bus, epoch, seats) = team_fixture("task-invite-occupied", 2);
    let joining = joining(&root);
    prove_addresses(&bus, epoch, &seats);
    let task = claimed(&bus, epoch, &seats[0], "INVITE-occupied1");
    let inviter_claim = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task)
        .held_by(&Actor::Lane(seats[0].lane))
        .expect("inviter holds the task")
        .claim
        .body
        .fact;

    let answer = delivered(invite(&bus, (epoch, &joining), &seats[0], &task, None));
    assert_eq!(answer.lane, seats[1].lane);
    let key = invited_key(&answer);
    let head = steering(&bus, key);
    assert_eq!(
        (head.sender, head.recipient, head.disposition),
        (
            Actor::Lane(seats[0].lane),
            Actor::Lane(seats[1].lane),
            Disposition::Live,
        )
    );
    let control = head
        .control
        .as_deref()
        .expect("invitation carries typed control")
        .parse::<TaskInviteControl>()
        .expect("invitation control parses");
    assert_eq!(control.claim(), inviter_claim);
    assert_eq!(
        (
            target_of(&bus, epoch, &task),
            active_claims(&bus, epoch, &task)
        ),
        (1, 1),
        "publication moves no staffing"
    );

    assert_committed(invited_join(
        &bus,
        epoch,
        &seats[1],
        &task,
        key,
        head.directive,
    ));
    assert_eq!(
        (
            target_of(&bus, epoch, &task),
            active_claims(&bus, epoch, &task)
        ),
        (2, 2)
    );
    assert_eq!(steering_disposition(&bus, key), Disposition::Acked);
    clean(&root, bus);
}

#[test]
fn an_unaddressed_invitation_stays_on_the_inviters_team_while_an_address_crosses_it() {
    let (root, bus, epoch, seats) = fixture("task-invite-team-boundary", 4);
    let joining = joining(&root);
    prove_addresses(&bus, epoch, &seats);
    let task = claimed(&bus, epoch, &seats[0], "INVITE-team1");
    // Another team keeps one eligible idle lane and one eligible vacant lane.
    reseat(&bus, epoch, |residency| {
        let vacated = row(residency, &seats[3]);
        vacated.occupant = None;
        vacated.disposition = "vacant".to_owned();
    });
    assert_eq!(
        refused_hint(&invite(&bus, (epoch, &joining), &seats[0], &task, None)),
        ("task::invite-none", Some("no other lane is on your team")),
        "a lone lane recruits no one, however many lanes elsewhere could join"
    );

    reseat(&bus, epoch, |residency| {
        residency
            .subscriptions
            .retain(|subscription| subscription.team != seats[1].team);
        row(residency, &seats[1]).team = seats[0].team;
    });
    let teammate = delivered(invite(&bus, (epoch, &joining), &seats[0], &task, None));
    assert_eq!(teammate.lane, seats[1].lane);
    decline(&bus, invited_key(&teammate), teammate.lane, epoch);

    claimed(&bus, epoch, &seats[1], "INVITE-busy1");
    assert_eq!(
        refused_hint(&invite(&bus, (epoch, &joining), &seats[0], &task, None)),
        ("task::invite-none", Some("1 lane holds another claim")),
        "a blocked team does not overflow to another team"
    );

    let crossed = delivered(invite(
        &bus,
        (epoch, &joining),
        &seats[0],
        &task,
        Some(address(&bus, epoch, &seats[2])),
    ));
    assert_eq!(crossed.lane, seats[2].lane);
    invited_key(&crossed);
    clean(&root, bus);
}

#[test]
fn an_address_binds_its_lane_lineage_and_inadmissible_lanes_refuse() {
    let (root, bus, epoch, seats) = fixture("task-invite-address", 4);
    let joining = joining(&root);
    prove_addresses(&bus, epoch, &seats);
    let task = claimed(&bus, epoch, &seats[0], "INVITE-address1");
    claimed(&bus, epoch, &seats[1], "INVITE-busy5");

    let retired = address(&bus, epoch, &seats[2]);
    let successor = replace_occupant(
        &bus,
        epoch,
        &seats[2],
        uuid(NEXT_COMMAND.fetch_add(1, Ordering::Relaxed)),
    );
    let answer = delivered(invite(
        &bus,
        (epoch, &joining),
        &seats[0],
        &task,
        Some(retired),
    ));
    assert_eq!(answer.lane, successor.lane);
    assert_eq!(
        steering(&bus, invited_key(&answer)).recipient,
        Actor::Lane(successor.lane)
    );
    decline(&bus, invited_key(&answer), answer.lane, epoch);

    // The inviter's own address refuses before any intent is built.
    let holder = address(&bus, epoch, &seats[0]);
    let invitations_before_self = detail_invitations(&bus, &task);
    assert_eq!(
        refusal_reason(&invite(
            &bus,
            (epoch, &joining),
            &seats[0],
            &task,
            Some(holder)
        )),
        "task::invite-self"
    );
    assert_eq!(detail_invitations(&bus, &task), invitations_before_self);
    let busy = address(&bus, epoch, &seats[1]);
    let invitations_before_busy = detail_invitations(&bus, &task);
    let before_busy = (
        target_of(&bus, epoch, &task),
        active_claims(&bus, epoch, &task),
        states(&invitations_before_busy),
    );
    assert_eq!(
        refusal_reason(&invite(
            &bus,
            (epoch, &joining),
            &seats[0],
            &task,
            Some(busy)
        )),
        "task::invite-busy"
    );
    assert_eq!(
        (
            target_of(&bus, epoch, &task),
            active_claims(&bus, epoch, &task),
            states(&detail_invitations(&bus, &task)),
        ),
        before_busy,
        "a busy refusal changes neither staffing nor invitations"
    );
    clean(&root, bus);
}

/// A vacant lane with an unavailable worktree cannot bind; the command
/// must then ask exactly one running lane rather than fail or ask several.
#[test]
fn an_unsolicited_invitation_asks_one_running_lane_only_after_no_vacant_lane_binds() {
    let (root, bus, epoch, seats) = team_fixture("task-invite-last-resort", 4);
    let joining = joining(&root);
    prove_addresses(&bus, epoch, &seats);
    let task = claimed(&bus, epoch, &seats[0], "INVITE-resort1");
    let vacant_address = address(&bus, epoch, &seats[3]);
    reseat(&bus, epoch, |residency| {
        let vacant = row(residency, &seats[3]);
        vacant.occupant = None;
        vacant.disposition = "vacant".to_owned();
        vacant.worktree = root.join("unavailable-worktree").display().to_string();
    });
    let probe = invite(
        &bus,
        (epoch, &joining),
        &seats[0],
        &task,
        Some(vacant_address),
    );
    assert!(
        matches!(&probe, Outcome::Fault(fault) if fault.reason == "git::worktree-unresolved"),
        "the vacant lane is eligible but cannot bind here: {probe:?}"
    );
    let answer = delivered(invite(&bus, (epoch, &joining), &seats[0], &task, None));
    assert_eq!(answer.lane, seats[1].lane);
    let key = invited_key(&answer).to_owned();
    assert_eq!(
        states(&detail_invitations(&bus, &task)),
        vec![(key.as_str(), wire::InvitationState::Invited)],
        "exactly one invitation exists"
    );
    assert_eq!(
        (
            target_of(&bus, epoch, &task),
            active_claims(&bus, epoch, &task)
        ),
        (1, 1)
    );
    clean(&root, bus);
}

#[test]
fn a_held_vacant_lane_refuses_without_staffing_and_does_not_hide_an_alternative() {
    let (root, bus, epoch, seats) = team_fixture("task-invite-held", 3);
    let joining = joining(&root);
    prove_addresses(&bus, epoch, &seats);
    let recipient = address(&bus, epoch, &seats[1]);
    let task = claimed(&bus, epoch, &seats[0], "INVITE-held1");
    reseat(&bus, epoch, |residency| {
        let recipient = row(residency, &seats[1]);
        recipient.occupant = None;
        recipient.disposition = "vacant".to_owned();
    });
    let origin: wire::Origin = format!("resident:{epoch}")
        .parse()
        .expect("resident origin");
    let at = fixture_instant("invitation hold");
    let mut write = JournalWrite::new(Actor::Spice, origin.clone(), at, Producer::BusTimer);
    write
        .fact(
            body::LaneHeld {
                lane: seats[1].lane,
                evidence: "Repeated young deaths".to_owned(),
                until: fixture_instant_after(at, 3600),
                vendor_failure: None,
            },
            FactCause::Root,
        )
        .expect("hold fact");
    assert!(matches!(
        bus.commit(write).expect("hold commits"),
        CommitOutcome::Committed(_)
    ));
    let held = bus.task_snapshot().expect("held snapshot");
    assert_eq!(
        refusal_reason(&invite(
            &bus,
            (epoch, &joining),
            &seats[0],
            &task,
            Some(recipient)
        )),
        "task::invite-unlaunchable"
    );
    assert_eq!(
        (
            target_of(&bus, epoch, &task),
            active_claims(&bus, epoch, &task)
        ),
        (1, 1)
    );
    let alternative = delivered(invite(&bus, (epoch, &joining), &seats[0], &task, None));
    assert_eq!(alternative.lane, seats[2].lane);
    assert!(matches!(
        alternative.delivery,
        TaskInviteDelivery::Invited { .. }
    ));
    assert_eq!(
        (
            target_of(&bus, epoch, &task),
            active_claims(&bus, epoch, &task)
        ),
        (1, 1)
    );

    let mut write = JournalWrite::new(Actor::Spice, origin, at, Producer::BusTimer);
    write
        .fact(
            body::LaneResumed {
                lane: seats[1].lane,
                release: body::Release::Wake,
            },
            FactCause::Root,
        )
        .expect("hold release");
    assert!(matches!(
        bus.commit(write).expect("release commits"),
        CommitOutcome::Committed(_)
    ));
    assert!(
        !bus.task_snapshot()
            .expect("resumed snapshot")
            .facts()
            .lane_held(seats[1].lane)
    );
    assert!(
        held.facts().lane_held(seats[1].lane),
        "a later release cannot change prior authority"
    );
    clean(&root, bus);
}

/// The reason and inviter-facing hint of one refused invitation.
fn refused_hint(outcome: &Outcome<CommandAnswer>) -> (&str, Option<&str>) {
    let Outcome::Refusal(refusal) = outcome else {
        panic!("expected a refusal: {outcome:?}");
    };
    (
        refusal.reason(),
        refusal
            .diagnostic()
            .and_then(wire::RefusalDiagnostic::detail),
    )
}

#[test]
fn an_exhausted_invitation_says_every_teammate_already_participates() {
    let (root, bus, epoch, seats) = team_fixture("task-invite-saturated", 3);
    let joining = joining(&root);
    prove_addresses(&bus, epoch, &seats);
    let task = claimed(&bus, epoch, &seats[0], "INVITE-saturated1");
    assert_committed(set_target(
        &bus,
        epoch,
        &Actor::TEST_PRINCIPAL,
        &task,
        ("todo", 3),
    ));
    for seat in &seats[1..] {
        assert_committed(direct_join(&bus, epoch, seat, &task));
    }
    assert_eq!(
        refused_hint(&invite(&bus, (epoch, &joining), &seats[0], &task, None)),
        (
            "task::invite-none",
            Some("every teammate already participates in this phase")
        ),
        "a saturated phase names saturation, not an address that must refuse"
    );
    clean(&root, bus);
}

/// Retire live invitation `key` by `lane`'s NAK.
fn decline(bus: &Bus, key: &str, lane: CanonicalUuid, epoch: CanonicalUuid) {
    let lookup = key.to_owned();
    let head = bus
        .read(move |read| read.steering_key(&lookup))
        .expect("steering index reads")
        .expect("invitation is indexed");
    let mut write = JournalWrite::new(
        Actor::Lane(lane),
        head.body().origin.clone(),
        fixture_instant("invitation refusal"),
        Producer::AckScanner,
    );
    let pending = write
        .fact(
            body::SteerDeclined {
                directive: head.body().directive,
                key: key.to_owned(),
                recipient: Actor::Lane(lane),
                evidence: body::SteeringEvidence::Transcript {
                    epoch,
                    locus: body::TranscriptLocus {
                        source: Some(uuid(NEXT_COMMAND.fetch_add(1, Ordering::Relaxed))),
                        member: 1,
                        offset: 40,
                        length: 7,
                        event: 0,
                        line: 1,
                        column: 1,
                    },
                },
                reason: "busy".to_owned(),
            },
            FactCause::Existing(head.position()),
        )
        .expect("refusal builds");
    let mut declined = head.body().clone();
    declined.disposition = Disposition::Declined;
    write.steering_head(pending, move |_| declined);
    assert!(matches!(
        bus.commit(write).expect("refusal commits"),
        CommitOutcome::Committed(_)
    ));
}

fn detail_invitations(bus: &Bus, task: &TaskHandle) -> Vec<wire::TaskInvitation> {
    let detail = wire::intent::TaskDetail {
        task: task.born().expect("handle carries its stamp"),
    };
    match crate::board_command::read_task_detail(bus, None, &detail, Actor::TEST_PRINCIPAL) {
        Outcome::Reply(reply) => match reply.body {
            CommandAnswer::TaskDetail(answer) => answer.task.invitations,
            other => panic!("expected task detail: {other:?}"),
        },
        other => panic!("task detail reads: {other:?}"),
    }
}

fn states(invitations: &[wire::TaskInvitation]) -> Vec<(&str, wire::InvitationState)> {
    invitations
        .iter()
        .map(|invitation| (invitation.key.as_str(), invitation.state))
        .collect()
}

#[test]
fn task_reads_derive_each_invitation_state_from_steering_and_board_heads() {
    use wire::InvitationState::{Declined, Expired, Invited, Joined};
    let (root, bus, epoch, seats) = team_fixture("task-invite-projection", 4);
    let joining = joining(&root);
    prove_addresses(&bus, epoch, &seats);
    let task = claimed(&bus, epoch, &seats[0], "INVITE-project1");
    let invite_to = |seat: &Seat| {
        let answer = delivered(invite(
            &bus,
            (epoch, &joining),
            &seats[0],
            &task,
            Some(address(&bus, epoch, seat)),
        ));
        invited_key(&answer).to_owned()
    };

    let accepted = invite_to(&seats[1]);
    let [invitation] = detail_invitations(&bus, &task)
        .try_into()
        .unwrap_or_else(|rows| panic!("one invitation projects: {rows:?}"));
    let head = steering(&bus, &accepted);
    assert_eq!(
        (
            invitation.key.as_str(),
            invitation.inviter,
            invitation.recipient,
            invitation.phase.to_string().as_str(),
            invitation.expires,
            invitation.state,
        ),
        (
            accepted.as_str(),
            seats[0].lane,
            seats[1].lane,
            "todo",
            head.expires,
            Invited,
        )
    );
    assert_committed(invited_join(
        &bus,
        epoch,
        &seats[1],
        &task,
        &accepted,
        head.directive,
    ));

    let declined = invite_to(&seats[2]);
    decline(&bus, &declined, seats[2].lane, epoch);
    let outlived = invite_to(&seats[3]);
    assert_eq!(
        states(&detail_invitations(&bus, &task)),
        vec![
            (accepted.as_str(), Joined),
            (declined.as_str(), Declined),
            (outlived.as_str(), Invited),
        ]
    );
    let outlived_head = steering(&bus, &outlived);
    let pending = pending_keys(&bus, seats[3].lane, &outlived_head);
    assert_eq!(
        pending.as_slice(),
        std::slice::from_ref(&outlived),
        "a current invitation remains actionable"
    );

    // The inviter's release expires its live invitation while the directive
    // stays live; an accepted one is history and keeps its joined state.
    free_active(&bus, epoch, &Actor::Lane(seats[0].lane), &seats[0], &task);
    assert_eq!(steering_disposition(&bus, &outlived), Disposition::Live);
    assert_eq!(
        states(&detail_invitations(&bus, &task)),
        vec![
            (accepted.as_str(), Joined),
            (declined.as_str(), Declined),
            (outlived.as_str(), Expired),
        ]
    );
    assert_eq!(
        pending_keys(&bus, seats[3].lane, &outlived_head),
        Vec::<String>::new(),
        "an invitation whose inviter authority ended offers no stale ACK or NAK"
    );

    // A named-task board read carries the same rows; the open board carries none.
    let Outcome::Reply(Reply {
        body: CommandAnswer::Board(named),
        ..
    }) = issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::BoardRead,
        Vec::new(),
        &BoardRead::Tasks {
            tasks: vec![task.born().expect("handle carries its stamp")],
            dependencies: false,
            claims: false,
            history: false,
        },
    )
    else {
        panic!("named-task read returns a board answer");
    };
    assert_eq!(
        states(&at(&named, &task).invitations),
        states(&detail_invitations(&bus, &task))
    );
    assert!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task)
            .invitations
            .is_empty()
    );
    clean(&root, bus);
}

#[test]
fn inviter_seat_replacement_expires_an_invitation_that_recipient_replacement_keeps() {
    use wire::InvitationState::{Expired, Invited};
    let (root, bus, epoch, seats) = team_fixture("task-invite-seat-replacement", 4);
    let joining = joining(&root);
    prove_addresses(&bus, epoch, &seats);
    let task = claimed(&bus, epoch, &seats[0], "INVITE-reseat1");
    let answer = delivered(invite(
        &bus,
        (epoch, &joining),
        &seats[0],
        &task,
        Some(address(&bus, epoch, &seats[3])),
    ));
    let key = invited_key(&answer).to_owned();
    let head = steering(&bus, &key);

    // The invitation binds the recipient lane, so its next occupant inherits it.
    replace_occupant(
        &bus,
        epoch,
        &seats[3],
        uuid(NEXT_COMMAND.fetch_add(1, Ordering::Relaxed)),
    );
    assert_eq!(
        states(&detail_invitations(&bus, &task)),
        vec![(key.as_str(), Invited)]
    );
    assert_eq!(
        pending_keys(&bus, seats[3].lane, &head).as_slice(),
        std::slice::from_ref(&key)
    );

    // The inviter's seat issued it, so its successor cannot keep it current
    // even while the directive stays live.
    replace_occupant(
        &bus,
        epoch,
        &seats[0],
        uuid(NEXT_COMMAND.fetch_add(1, Ordering::Relaxed)),
    );
    assert_eq!(steering_disposition(&bus, &key), Disposition::Live);
    assert_eq!(
        states(&detail_invitations(&bus, &task)),
        vec![(key.as_str(), Expired)]
    );
    assert_eq!(
        pending_keys(&bus, seats[3].lane, &head),
        Vec::<String>::new()
    );
    clean(&root, bus);
}

#[test]
fn an_occupied_invitation_lapses_minutes_after_publication() {
    let (root, bus, epoch, seats) = team_fixture("task-invite-lifetime", 4);
    let joining = joining(&root);
    prove_addresses(&bus, epoch, &seats);
    let task = claimed(&bus, epoch, &seats[0], "INVITE-window1");
    let answer = delivered(invite(
        &bus,
        (epoch, &joining),
        &seats[0],
        &task,
        Some(address(&bus, epoch, &seats[3])),
    ));
    let head = steering(&bus, invited_key(&answer));
    let window = head.expires.get().duration_since(head.published.get());
    assert_eq!(
        window,
        jiff::SignedDuration::try_from(crate::resident::INVITATION_LIFETIME)
            .expect("the invitation lifetime is a signed duration")
    );
    assert!(
        window < steer::Config::default().expiry().get().try_into().unwrap(),
        "an invitation lapses well before ordinary direction would"
    );
    clean(&root, bus);
}

#[test]
fn an_ack_on_an_expired_invitation_lapses_it_and_tells_the_inviter() {
    let (root, bus, epoch, seats) = team_fixture("task-invite-late-ack", 4);
    let joining = joining(&root);
    prove_addresses(&bus, epoch, &seats);
    let task = claimed(&bus, epoch, &seats[0], "INVITE-late1");
    let answer = delivered(invite(
        &bus,
        (epoch, &joining),
        &seats[0],
        &task,
        Some(address(&bus, epoch, &seats[3])),
    ));
    let key = invited_key(&answer).to_owned();
    let lookup = key.clone();
    let head = bus
        .read(move |read| read.steering_key(&lookup))
        .expect("steering index reads")
        .expect("invitation is indexed");

    // The inviter releases the claim the invitation bound; the recipient's
    // ACK then arrives too late to join.
    free_active(&bus, epoch, &Actor::Lane(seats[0].lane), &seats[0], &task);
    let (disposition, inviter) =
        crate::resident::dispose_acknowledged(&bus, epoch, &head, joining.joining())
            .expect("a late acknowledgment settles the invitation");
    assert_eq!(
        (disposition, inviter),
        (Disposition::Lapsed, Some(seats[0].lane))
    );
    assert_eq!(steering_disposition(&bus, &key), Disposition::Lapsed);
    assert_eq!(
        states(&detail_invitations(&bus, &task)),
        vec![(key.as_str(), wire::InvitationState::Expired)]
    );
    assert!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task)
            .active_claims()
            .next()
            .is_none(),
        "a late acknowledgment claims nothing"
    );

    // The inviter hears one typed reply naming the attempt as expired.
    let inviter = seats[0].lane;
    let replies = bus
        .read(move |read| crate::steering_read::pending(read, Some(inviter)))
        .expect("steering index reads");
    let controls = replies
        .iter()
        .filter_map(|reply| reply.body().control.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        controls,
        vec![wire::peer_reply_control(&key, wire::PeerAnswer::Expired)]
    );
    clean(&root, bus);
}

/// Succeed the inviter's occupant: the claim head moves to a carry fact while
/// the win that identifies the claim stays.
fn carry(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, task: &TaskHandle) {
    let prior = at(&board(bus, epoch, Actor::TEST_PRINCIPAL), task)
        .latest_claim()
        .expect("inviter holds the task")
        .claim
        .body
        .clone();
    let born = task.born().expect("handle carries its stamp");
    let source = bus
        .source_boundary(&seat.lane)
        .expect("source boundary reads");
    let mut write = JournalWrite::new(
        Actor::Spice,
        format!("lane:{}", seat.lane)
            .parse()
            .expect("fixture carry origin"),
        fixture_instant("invite claim carry"),
        Producer::ResidentRecognition,
    );
    let carried = write
        .fact(
            body::ClaimCarried {
                task: born.clone(),
                lane: seat.lane,
                occupant: prior.occupant,
                ancestor: prior.occupant,
                epoch: prior.epoch,
                source,
            },
            FactCause::Root,
        )
        .expect("claim carry builds");
    write.claim_head(born, carried, move |fact| wire::ClaimHead { fact, ..prior });
    assert!(matches!(
        bus.commit(write).expect("claim carry commits"),
        CommitOutcome::Committed(_)
    ));
}

/// The invitation names the inviter's claim by the win that identifies it, so
/// an occupant succession that moves the claim head leaves the recipient's
/// ACK joining rather than reading an expired offer.
#[test]
fn an_ack_joins_after_the_inviter_claim_head_moved_under_the_same_win() {
    let (root, bus, epoch, seats) = team_fixture("task-invite-carried-claim", 2);
    let joining = joining(&root);
    prove_addresses(&bus, epoch, &seats);
    let task = claimed(&bus, epoch, &seats[0], "INVITE-carried");
    let answer = delivered(invite(
        &bus,
        (epoch, &joining),
        &seats[0],
        &task,
        Some(address(&bus, epoch, &seats[1])),
    ));
    let key = invited_key(&answer).to_owned();

    carry(&bus, epoch, &seats[0], &task);
    let moved = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task)
        .latest_claim()
        .expect("carried claim")
        .claim
        .body
        .clone();
    assert_ne!(moved.fact, moved.claimed);

    // The recipient's ACK plans its join exactly as `task join KEY` does.
    let head = bus
        .read({
            let key = key.clone();
            move |read| read.steering_key(&key)
        })
        .expect("steering index reads")
        .expect("invitation is indexed");
    let control = head
        .body()
        .control
        .as_deref()
        .expect("invitation has control")
        .parse::<wire::TaskInviteControl>()
        .expect("invitation control parses");
    let recipient = Actor::Lane(seats[1].lane);
    let parsed = key.parse::<steer::Key>().expect("invitation key parses");
    let (kind, expected, body) = crate::board_command::invitation_join(
        &board(&bus, epoch, recipient.clone()),
        &parsed,
        &head,
        &control,
    )
    .unwrap_or_else(|outcome| panic!("the ACK plans under the carried claim: {outcome:?}"));
    assert_committed(issue(&bus, epoch, recipient, kind, expected, &body));
    assert_eq!(
        states(&detail_invitations(&bus, &task)),
        vec![(key.as_str(), wire::InvitationState::Joined)]
    );
    clean(&root, bus);
}
