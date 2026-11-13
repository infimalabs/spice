use super::*;

#[test]
fn a_live_offer_blocks_repeated_and_retargeted_requests_until_it_settles() {
    let (root, bus, epoch, seats) = team_fixture("invite-exclusive", 3);
    let joining = joining(&root);
    prove_addresses(&bus, epoch, &seats);
    let task = claimed(&bus, epoch, &seats[0], "INVITE-single1");
    let first = delivered(invite(&bus, (epoch, &joining), &seats[0], &task, None));
    let key = invited_key(&first).to_owned();
    let head = steering(&bus, &key);
    let before = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    for recipient in [None, Some(address(&bus, epoch, &seats[2]))] {
        let refused = invite(&bus, (epoch, &joining), &seats[0], &task, recipient);
        assert_eq!(refusal_reason(&refused), "task::invite-outstanding");
    }
    let after = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&before, &task).claims, at(&after, &task).claims);
    assert_eq!(at(&before, &task).completion, at(&after, &task).completion);
    assert_eq!(detail_invitations(&bus, &task).len(), 1);
    let recipient = seats.iter().find(|seat| seat.lane == first.lane).unwrap();
    assert_committed(invited_join(
        &bus,
        epoch,
        recipient,
        &task,
        &key,
        head.directive,
    ));
    let second = delivered(invite(&bus, (epoch, &joining), &seats[0], &task, None));
    assert_ne!(invited_key(&second), key);
    assert_eq!(second.lane, seats[2].lane);
    decline(&bus, invited_key(&second), second.lane, epoch);
    assert_eq!(target_of(&bus, epoch, &task), 2);
    let third = delivered(invite(&bus, (epoch, &joining), &seats[0], &task, None));
    assert_ne!(invited_key(&third), invited_key(&second));
    clean(&root, bus);
}

#[test]
fn overlapping_retained_offers_cannot_choose_a_winner_by_acceptance() {
    let (root, bus, epoch, seats) = team_fixture("invite-overlap", 3);
    let joining = joining(&root);
    prove_addresses(&bus, epoch, &seats);
    let task = claimed(&bus, epoch, &seats[0], "INVITE-overlap1");
    let first = send_invitation(&bus, epoch, &seats[0], &seats[1], &task, "1kLGp2NTx");
    let second = send_invitation(&bus, epoch, &seats[0], &seats[2], &task, "1kLGp2NWx");
    for (recipient, key, directive) in [
        (&seats[1], "1kLGp2NTx", first),
        (&seats[2], "1kLGp2NWx", second),
    ] {
        assert_eq!(
            refusal_reason(&invited_join(&bus, epoch, recipient, &task, key, directive)),
            "task::invite-outstanding"
        );
    }
    assert_eq!(
        refusal_reason(&invite(&bus, (epoch, &joining), &seats[0], &task, None)),
        "task::invite-outstanding"
    );
    assert_eq!(
        (
            target_of(&bus, epoch, &task),
            active_claims(&bus, epoch, &task)
        ),
        (1, 1)
    );
    decline(&bus, "1kLGp2NTx", seats[1].lane, epoch);
    assert_committed(invited_join(
        &bus,
        epoch,
        &seats[2],
        &task,
        "1kLGp2NWx",
        second,
    ));
    assert_eq!(
        (
            target_of(&bus, epoch, &task),
            active_claims(&bus, epoch, &task)
        ),
        (2, 2)
    );
    clean(&root, bus);
}

#[test]
fn competing_publications_recheck_the_asking_claim_inside_the_writer() {
    let (root, bus, epoch, seats) = team_fixture("invite-competing", 3);
    prove_addresses(&bus, epoch, &seats);
    let task = claimed(&bus, epoch, &seats[0], "INVITE-compete1");
    let snapshot = crate::board_command::read_guard::validated_working_snapshot_for_tasks(
        &bus,
        None,
        Actor::Lane(seats[0].lane),
        &[task.born().unwrap()],
    )
    .unwrap();
    let entry = snapshot
        .entries
        .iter()
        .find(|entry| entry.handle() == &task)
        .unwrap();
    let claim = entry
        .claim_of(&Actor::Lane(seats[0].lane))
        .unwrap()
        .claimed();
    let authority =
        crate::board_command::read_guard::AskingOffer::new(&snapshot, entry, seats[0].lane, claim);
    let at = fixture_instant("competing offers");
    let prepare = |recipient: &Seat| {
        authority.admit(&bus, None, at).unwrap();
        let origin = format!("task:{}", entry.born())
            .parse::<wire::Origin>()
            .unwrap();
        let mut write = JournalWrite::new(
            Actor::Lane(seats[0].lane),
            origin.clone(),
            at,
            Producer::CommandDoor,
        );
        authority.guard(&mut write, None, at);
        crate::resident::append_task_invitation(
            &bus,
            origin,
            &mut write,
            crate::resident::TaskInvitation {
                task: entry.born(),
                phase: &entry.workflow().phase,
                generation: entry.workflow().generation,
                inviter: seats[0].lane,
                inviter_address: address(&bus, epoch, &seats[0]),
                inviter_claim: claim,
                recipient: recipient.lane,
                recipient_occupant: recipient.occupant,
                residency: snapshot.answer.residency.position,
                epoch,
                text: "Join the held task".to_owned(),
            },
            at,
        )
        .unwrap();
        write
    };
    let first = prepare(&seats[1]);
    let second = prepare(&seats[2]);
    assert!(matches!(
        bus.commit(first).unwrap(),
        CommitOutcome::Committed(_)
    ));
    match bus.commit(second).unwrap() {
        CommitOutcome::Refused(refused) => assert_eq!(refused.reason(), "task::invite-outstanding"),
        other => panic!("a competing offer must refuse before publication: {other:?}"),
    }
    assert_eq!(
        detail_invitations(&bus, &task)
            .iter()
            .map(|offer| offer.recipient)
            .collect::<Vec<_>>(),
        vec![seats[1].lane]
    );
    clean(&root, bus);
}

#[test]
fn each_asking_claim_can_have_its_own_offer() {
    let (root, bus, epoch, seats) = team_fixture("invite-independent", 4);
    let joining = joining(&root);
    prove_addresses(&bus, epoch, &seats);
    let task = claimed(&bus, epoch, &seats[0], "INVITE-independent1");
    let key = "1kLGp2NVx";
    let directive = send_invitation(&bus, epoch, &seats[0], &seats[1], &task, key);
    assert_committed(invited_join(&bus, epoch, &seats[1], &task, key, directive));
    let first = delivered(invite(
        &bus,
        (epoch, &joining),
        &seats[0],
        &task,
        Some(address(&bus, epoch, &seats[2])),
    ));
    let second = delivered(invite(
        &bus,
        (epoch, &joining),
        &seats[1],
        &task,
        Some(address(&bus, epoch, &seats[3])),
    ));
    assert_eq!((first.lane, second.lane), (seats[2].lane, seats[3].lane));
    for (answer, recipient) in [(&first, &seats[2]), (&second, &seats[3])] {
        let key = invited_key(answer);
        assert_committed(invited_join(
            &bus,
            epoch,
            recipient,
            &task,
            key,
            steering(&bus, key).directive,
        ));
    }
    assert_eq!(
        (
            target_of(&bus, epoch, &task),
            active_claims(&bus, epoch, &task)
        ),
        (4, 4)
    );
    clean(&root, bus);
}
