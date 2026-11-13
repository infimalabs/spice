use wire::Disposition;

use super::*;

#[test]
fn a_sender_cancels_only_its_typed_invitation_and_operator_authority_stays_broader() {
    let lanes = Lanes::new("invitation-cancellation");
    let author = Actor::Lane(test_uuid(OTHER));
    let at = wire::Instant::new(jiff::Timestamp::now()).unwrap();
    let origin: wire::Origin = "task:1kKRRsDPx".parse().unwrap();
    let mut write = JournalWrite::new(author.clone(), origin.clone(), at, Producer::CommandDoor);
    let residency = lanes
        .bus
        .task_snapshot()
        .unwrap()
        .residency()
        .unwrap()
        .position();
    let (key, expires, directive) = crate::resident::append_task_invitation(
        &lanes.bus,
        origin,
        &mut write,
        crate::resident::TaskInvitation {
            task: &"1kKRRsDPx".parse().unwrap(),
            phase: &"todo".parse().unwrap(),
            generation: 0,
            inviter: test_uuid(OTHER),
            inviter_address: "1000".parse().unwrap(),
            inviter_claim: wire::Seq::FIRST,
            recipient: test_uuid(RECIPIENT),
            recipient_occupant: test_uuid(RECIPIENT + 1),
            residency,
            epoch: lanes.epoch,
            text: "Join this task".to_owned(),
        },
        at,
    )
    .unwrap();
    assert!(matches!(
        lanes.bus.commit(write).unwrap(),
        bus::CommitOutcome::Committed(_)
    ));
    for (actor, id) in [
        (Actor::Lane(test_uuid(RECIPIENT)), 71),
        (Actor::Lane(test_uuid(99)), 72),
    ] {
        assert!(matches!(
            lanes.cancel_as(actor, id, directive),
            Outcome::Refusal(_)
        ));
        let lookup = key.clone();
        assert_eq!(
            lanes
                .bus
                .read(move |read| read.steering_key(&lookup))
                .unwrap()
                .unwrap()
                .body()
                .disposition,
            Disposition::Live
        );
    }
    assert!(matches!(
        lanes.cancel_as(author.clone(), 73, directive),
        Outcome::Reply(_)
    ));
    let lookup = key.clone();
    assert_eq!(
        lanes
            .bus
            .read(move |read| read.steering_key(&lookup))
            .unwrap()
            .unwrap()
            .body()
            .disposition,
        Disposition::Canceled
    );
    // An ordinary peer message from that same lane is not an invitation.
    let plain = wire::body::SteerSent {
        sender: Actor::Lane(test_uuid(OTHER)),
        room: None,
        directive: test_uuid(78),
        fanout: None,
        recipients: Vec::new(),
        key: "1kLGp2NWx".to_owned(),
        recipient: Actor::Lane(test_uuid(RECIPIENT)),
        text: "Ordinary peer note".to_owned(),
        control: None,
        attachments: Vec::new(),
        expires,
    };
    let origin: wire::Origin = "ack:1kLGp2NWx".parse().unwrap();
    let mut write = JournalWrite::new(author.clone(), origin.clone(), at, Producer::CommandDoor);
    let sent = write.fact(plain.clone(), FactCause::Root).unwrap();
    write.steering_head(sent, move |created| {
        steer::Ledger::sent_head(&plain, origin, at, created).unwrap()
    });
    assert!(matches!(
        lanes.bus.commit(write).unwrap(),
        bus::CommitOutcome::Committed(_)
    ));
    assert!(matches!(
        lanes.cancel_as(author.clone(), 79, test_uuid(78)),
        Outcome::Refusal(_)
    ));
    assert!(matches!(
        lanes.cancel_as(Actor::TEST_PRINCIPAL, 80, test_uuid(78)),
        Outcome::Reply(_)
    ));
    let ordinary = lanes.send(74, RECIPIENT, 75, "Operator direction");
    assert!(matches!(
        lanes.cancel_as(author, 76, ordinary.directive),
        Outcome::Refusal(_)
    ));
    assert!(matches!(
        lanes.cancel_as(Actor::TEST_PRINCIPAL, 77, ordinary.directive),
        Outcome::Reply(_)
    ));
    lanes.finish();
}

impl Lanes {
    fn cancel_as(
        &self,
        actor: Actor,
        id: u8,
        directive: wire::CanonicalUuid,
    ) -> Outcome<wire::intent::CommandAnswer> {
        let request = wire::intent::DirectiveRef { directive };
        let command = wire::Command {
            v: wire::WireVersion::CURRENT,
            id: test_uuid(id),
            epoch: self.epoch,
            kind: CommandKind::SteerCancel,
            expected: vec![Witness::Directive {
                steer: directive,
                disposition: Disposition::Live,
            }],
            body: serde_json::to_value(&request).unwrap(),
        }
        .admit(
            &wire::Accepted {
                wire: wire::WireVersion::CURRENT,
                actor,
                epoch: self.epoch,
                release: "1.2.3".parse().unwrap(),
            },
            &wire::principal::PrincipalRoles::default(),
        )
        .unwrap();
        steering::cancel(
            &self.bus,
            self.epoch,
            &command,
            &request,
            &mut CommandEffects::default(),
        )
    }
}
