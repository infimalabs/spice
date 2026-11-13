//! Direction for a lane nobody sits in waits for its next occupant.

use super::*;

fn send_to(
    bus: &Bus,
    epoch: CanonicalUuid,
    send: &wire::intent::SteerSend,
    id: u8,
    witness: Witness,
) -> (Outcome<CommandAnswer>, CommandEffects) {
    let command = Command {
        v: wire::WireVersion::CURRENT,
        id: test_uuid(id),
        epoch,
        kind: CommandKind::SteerSend,
        expected: vec![witness],
        body: serde_json::to_value(send).expect("steer request encodes"),
    }
    .admit(
        &wire::Accepted {
            wire: wire::WireVersion::CURRENT,
            actor: Actor::TEST_PRINCIPAL,
            epoch,
            release: "1.2.3".parse().expect("fixture release"),
        },
        &wire::principal::PrincipalRoles::default(),
    )
    .expect("operator send admits");
    let mut effects = CommandEffects::default();
    let outcome = steering::send(
        bus,
        epoch,
        &steer::Config::default(),
        None,
        &command,
        send,
        &mut effects,
    );
    (outcome, effects)
}

#[test]
fn a_vacant_lane_takes_direction_under_its_vacancy_and_refuses_a_departed_occupant() {
    let (root, paths) = resident_fixture("steer-vacant");
    let repository = Repository::discover(&root).expect("fixture repository resolves");
    let worktree = repository.worktree_root().to_string_lossy().into_owned();
    let surface =
        crate::trust_command::Surface::load(&repository).expect("fixture trust surface loads");
    let bus = Bus::start(journal::Journal::open(&paths.journal).expect("fixture journal opens"))
        .expect("fixture bus starts");
    let epoch = test_uuid(1);
    let lane = test_uuid(2);
    commit_startup(&bus, &repository, &surface, epoch, None).expect("resident seats");
    seed_vacant_lane_in_worktree(&bus, epoch, lane, &worktree);
    let send = wire::intent::SteerSend {
        fanout: test_uuid(31),
        recipients: Vec::new(),
        lane: Some(lane),
        message: "pick this up when you are seated".to_owned(),
        attachments: Vec::new(),
    };

    let departed = Witness::Residency {
        lane,
        occupant: Some(test_uuid(3)),
        epoch,
    };
    let vacancy = Witness::Residency {
        lane,
        occupant: None,
        epoch,
    };
    let (outcome, effects) = send_to(&bus, epoch, &send, 30, departed);
    assert!(
        matches!(&outcome, Outcome::Refusal(refusal) if refusal.reason() == "steer::witness-moved"),
        "{outcome:?}"
    );
    assert!(effects.inbox_lanes.is_empty());

    let (outcome, effects) = send_to(&bus, epoch, &send, 32, vacancy);
    let Outcome::Reply(Reply {
        body: CommandAnswer::Directive(sent),
        ..
    }) = outcome
    else {
        panic!("direction for a vacant lane is durable: {outcome:?}");
    };
    assert_eq!(sent.directive, test_uuid(32));
    assert_eq!(effects.inbox_lanes, [lane]);

    bus.verify().expect("the authority verifies");
    bus.shutdown().expect("fixture bus stops");
    fs::remove_dir_all(root).expect("fixture cleanup");
}
