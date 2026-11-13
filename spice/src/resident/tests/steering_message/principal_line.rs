//! Principal command-door controls retire the same inputs and lines as lane replies.

use super::*;

fn admit_principal(lanes: &Lanes, principal: CanonicalUuid, operator: bool) {
    let roles = if operator {
        std::collections::BTreeSet::from([wire::principal::PrincipalRole::Operator])
    } else {
        std::collections::BTreeSet::new()
    };
    let mut proposal = JournalWrite::new(
        Actor::Spice,
        format!("resident:{}", lanes.epoch)
            .parse()
            .expect("fixture origin"),
        wire::Instant::new(jiff::Timestamp::now()).expect("fixture instant"),
        Producer::CommandDoor,
    );
    let admitted = wire::principal::PrincipalAdmitted {
        principal,
        profile: wire::principal::PrincipalProfile::default(),
        roles: roles.clone(),
        capability: None,
    };
    let born = proposal
        .fact(admitted, FactCause::Root)
        .expect("principal admission builds");
    proposal.principal_head(born, move |born| wire::principal::PrincipalHead {
        principal,
        born,
        profile: wire::principal::PrincipalProfile::default(),
        roles,
        capability: None,
    });
    commit(&lanes.bus, proposal).expect("principal admission commits");
}

fn seed_input(
    lanes: &Lanes,
    principal: CanonicalUuid,
    directive: CanonicalUuid,
    key: &str,
) -> wire::rooms::PrincipalInputRef {
    let at = wire::Instant::new(jiff::Timestamp::now()).expect("input instant");
    let origin: wire::Origin = format!("resident:{}", lanes.epoch)
        .parse()
        .expect("fixture origin");
    let line = wire::body::PeerLineRef {
        root: directive,
        parent: None,
        opening: directive,
    };
    let sent = wire::body::SteerSent {
        sender: Actor::Lane(test_uuid(SENDER)),
        room: None,
        directive,
        fanout: None,
        recipients: Vec::new(),
        key: key.to_owned(),
        recipient: Actor::Principal(principal),
        text: "review this result".to_owned(),
        control: Some(wire::peer_line_control(
            line,
            wire::body::PeerBodyKind::Open,
            &[],
        )),
        attachments: Vec::new(),
        expires: steering::expiry(at, steer::Config::default().expiry().get())
            .expect("input expiry"),
    };
    let mut proposal = JournalWrite::new(
        Actor::Lane(test_uuid(SENDER)),
        origin.clone(),
        at,
        Producer::AckScanner,
    );
    let pending = proposal
        .fact(sent.clone(), FactCause::Root)
        .expect("principal input builds");
    proposal.steering_head(pending, move |created| {
        steer::Ledger::sent_head(&sent, origin, at, created).expect("principal input head builds")
    });
    commit(&lanes.bus, proposal).expect("principal input commits");
    wire::rooms::PrincipalInputRef {
        directive,
        principal,
    }
}

fn admitted_control(
    lanes: &Lanes,
    id: u8,
    principal: CanonicalUuid,
    request: &wire::intent::SteerLineControl,
) -> wire::AdmittedCommand {
    let roles = lanes
        .bus
        .read(|read| read.principal_roles())
        .expect("principal roles read");
    Command {
        v: wire::WireVersion::CURRENT,
        id: test_uuid(id),
        epoch: lanes.epoch,
        kind: CommandKind::SteerLineControl,
        expected: Vec::new(),
        body: serde_json::to_value(request).expect("line control encodes"),
    }
    .admit(
        &wire::Accepted {
            wire: wire::WireVersion::CURRENT,
            actor: Actor::Principal(principal),
            epoch: lanes.epoch,
            release: "1.2.3".parse().expect("fixture release"),
        },
        &roles,
    )
    .expect("line control admits")
}

fn control(
    lanes: &Lanes,
    command: &wire::AdmittedCommand,
    request: &wire::intent::SteerLineControl,
) -> Outcome<CommandAnswer> {
    steering::line_control(
        &lanes.bus,
        lanes.epoch,
        &steer::Config::default(),
        command,
        request,
        &mut CommandEffects::default(),
    )
}

fn answer(outcome: &Outcome<CommandAnswer>) -> &wire::intent::SteerLineAnswer {
    let Outcome::Reply(Reply {
        position: ReplyPosition::Committed(_),
        body: CommandAnswer::SteerLine(answer),
    }) = outcome
    else {
        panic!("line control committed: {outcome:?}");
    };
    answer
}

#[test]
fn only_the_endpoint_principal_retires_the_exact_input_and_retry_replays() {
    let lanes = Lanes::new("principal-line-owner");
    let owner = test_uuid(80);
    let stranger = test_uuid(82);
    admit_principal(&lanes, owner, false);
    admit_principal(&lanes, stranger, true);
    let input = seed_input(&lanes, owner, test_uuid(84), "1kQnrzNZx");
    let request = wire::intent::SteerLineControl {
        input: input.clone(),
        action: wire::intent::SteerLineAction::Ack,
        body: Some("Thanks.!".to_owned()),
    };
    let wrong = admitted_control(&lanes, 86, stranger, &request);
    assert!(matches!(
        control(&lanes, &wrong, &request),
        Outcome::Refusal(_)
    ));
    assert_eq!(
        lanes
            .bus
            .read(move |read| read.steering_head(input.directive()))
            .expect("input reads")
            .expect("input exists")
            .body()
            .disposition,
        wire::Disposition::Live,
        "wrong-endpoint authority writes nothing"
    );

    let command = admitted_control(&lanes, 88, owner, &request);
    let outcome = control(&lanes, &command, &request);
    let answer = answer(&outcome);
    assert_eq!(
        answer.effect.action,
        Some(wire::body::PeerLineAction::Reaction)
    );
    assert_eq!(answer.body, None);
    assert_eq!(
        lanes
            .bus
            .read(move |read| read.steering_head(test_uuid(84)))
            .expect("input reads")
            .expect("input exists")
            .body()
            .disposition,
        wire::Disposition::Acked
    );
    assert_eq!(
        crate::resident::command_surface::replay_command(&lanes.bus, &command),
        Some(outcome),
        "an exact retry returns the recorded answer"
    );
    lanes.finish();
}

#[test]
fn role_loss_settles_the_header_but_refuses_new_direction_text() {
    let lanes = Lanes::new("principal-line-role-loss");
    let principal = test_uuid(90);
    admit_principal(&lanes, principal, false);
    let input = seed_input(&lanes, principal, test_uuid(92), "1kQnrzNZx");
    let request = wire::intent::SteerLineControl {
        input,
        action: wire::intent::SteerLineAction::Ack,
        body: Some("continue without authority".to_owned()),
    };
    let command = admitted_control(&lanes, 94, principal, &request);
    let outcome = control(&lanes, &command, &request);
    let answer = answer(&outcome);
    assert_eq!(answer.body, Some(wire::outbox::BodyFate::Unsent));
    assert!(matches!(
        answer.effect.body,
        Some(wire::outbox::LineBody::Refused { .. })
    ));
    assert_eq!(
        lanes
            .bus
            .read(move |read| read.steering_head(test_uuid(92)))
            .expect("input reads")
            .expect("input exists")
            .body()
            .disposition,
        wire::Disposition::Acked,
        "role loss does not revoke exact receipt ownership"
    );
    lanes.finish();
}

#[test]
fn an_operator_principal_continues_and_finishes_with_raw_line_bodies() {
    let lanes = Lanes::new("principal-line-bodies");
    let principal = lanes.bootstrap().principal().expect("bootstrap principal");
    let continued = seed_input(&lanes, principal, test_uuid(70), "1kQnrzNZx");
    let continuation = wire::intent::SteerLineControl {
        input: continued,
        action: wire::intent::SteerLineAction::Ack,
        body: Some("plain continuation".to_owned()),
    };
    let continue_command = admitted_control(&lanes, 72, principal, &continuation);
    let continued = control(&lanes, &continue_command, &continuation);
    let continued = answer(&continued);
    assert_eq!(continued.body, Some(wire::outbox::BodyFate::Sent));
    assert_eq!(
        continued.effect.action,
        Some(wire::body::PeerLineAction::Continue)
    );
    assert!(
        lanes
            .pending(SENDER)
            .iter()
            .any(|head| head.text == "plain continuation"),
        "the ordinary lane input carries its raw text unchanged"
    );
    let lane_key = match continued.effect.body.as_ref().expect("body is judged") {
        wire::outbox::LineBody::Admitted(branch) => branch.key.clone(),
        wire::outbox::LineBody::Refused { reason } => {
            panic!("the lane body was refused: {reason}")
        }
    };
    lanes.answer(
        SENDER,
        &[lane_key.as_str()],
        &format!("FIN {lane_key}: lane return"),
    );
    let returned = lanes
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("facts read")
        .facts()
        .iter()
        .filter_map(|envelope| envelope.fact.read::<wire::body::SteerSent>())
        .find(|sent| {
            sent.sender == Actor::Lane(test_uuid(SENDER))
                && sent.recipient == Actor::Principal(principal)
                && sent.text == "lane return"
        })
        .expect("the recursive principal input is durable");
    let (returned_line, _, covered) = returned
        .control
        .as_deref()
        .and_then(wire::parse_peer_line_control)
        .expect("the recursive input carries line authority");
    assert_eq!(
        returned_line,
        wire::body::PeerLineRef {
            root: test_uuid(70),
            parent: None,
            opening: test_uuid(70),
        },
        "recursive bodies preserve the line identity"
    );
    assert_eq!(covered, [test_uuid(72)]);
    let recursive = wire::intent::SteerLineControl {
        input: wire::rooms::PrincipalInputRef {
            directive: returned.directive,
            principal,
        },
        action: wire::intent::SteerLineAction::Ack,
        body: None,
    };
    let recursive_command = admitted_control(&lanes, 78, principal, &recursive);
    assert_eq!(
        answer(&control(&lanes, &recursive_command, &recursive))
            .effect
            .action,
        None,
        "the principal can mark a recursive terminal body seen without replacing the winning FIN"
    );

    let finished = seed_input(&lanes, principal, test_uuid(74), "1kLGp2NWx");
    let finish = wire::intent::SteerLineControl {
        input: finished,
        action: wire::intent::SteerLineAction::Fin,
        body: None,
    };
    let finish_command = admitted_control(&lanes, 76, principal, &finish);
    let finished = control(&lanes, &finish_command, &finish);
    assert_eq!(
        answer(&finished).effect.action,
        Some(wire::body::PeerLineAction::Finish)
    );
    assert_eq!(
        lanes
            .bus
            .read(move |read| read.peer_line_head(test_uuid(74)))
            .expect("line reads")
            .expect("line exists")
            .terminal()
            .expect("line is terminal")
            .action,
        wire::body::PeerLineAction::Finish
    );
    lanes.finish();
}
