//! Self-contained steering delivery from journal-backed quote authority.

use super::*;

#[test]
fn inbox_expands_nested_quotes_without_rewriting_journal_text() {
    let (root, paths) = resident_fixture("steer-quote-delivery");
    let repository = Repository::discover(&root).expect("fixture repository resolves");
    let worktree = repository.worktree_root().to_string_lossy().into_owned();
    let surface =
        crate::trust_command::Surface::load(&repository).expect("fixture trust surface loads");
    let bus = Bus::start(journal::Journal::open(&paths.journal).expect("fixture journal opens"))
        .expect("fixture bus starts");
    let epoch = test_uuid(1);
    let lane = test_uuid(2);
    let occupant = test_uuid(3);
    commit_startup(&bus, &repository, &surface, epoch, None).expect("resident seats");
    seed_lane_in_worktree(&bus, epoch, lane, occupant, &worktree);

    let inner_text = "authenticated inner direction";
    let inner = send(&bus, epoch, lane, occupant, test_uuid(10), inner_text);
    let inner_target = wire::QuoteTarget::Steering {
        key: inner.key,
        digest: wire::QuoteTarget::text_digest(inner_text),
    };
    let middle_text = format!("before {} after", inner_target.markdown());
    let middle = send(&bus, epoch, lane, occupant, test_uuid(11), &middle_text);
    let middle_target = wire::QuoteTarget::Steering {
        key: middle.key,
        digest: wire::QuoteTarget::text_digest(&middle_text),
    };
    let outer_text = format!("deliver {} now", middle_target.markdown());
    let outer = send(&bus, epoch, lane, occupant, test_uuid(12), &outer_text);

    let inbox = read_inbox(&bus, &paths, epoch, lane, test_uuid(13));
    let delivered = inbox.lanes[0]
        .pending
        .iter()
        .find(|pending| pending.directive == outer.directive)
        .expect("outer directive remains pending");
    assert_eq!(
        delivered.text,
        "deliver before authenticated inner direction after now"
    );
    assert!(!delivered.text.contains("[quote]("));

    let recorded = bus
        .read(move |read| read.steering_head(outer.directive))
        .expect("steering index reads")
        .expect("outer directive is indexed");
    assert_eq!(recorded.body().text, outer_text);

    bus.verify().expect("quote delivery authority verifies");
    bus.shutdown().expect("fixture bus stops");
    fs::remove_dir_all(root).expect("fixture cleanup");
}

fn send(
    bus: &Bus,
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    directive: CanonicalUuid,
    message: &str,
) -> wire::intent::DirectiveAnswer {
    let request = wire::intent::SteerSend {
        fanout: test_uuid(90),
        recipients: Vec::new(),
        lane: Some(lane),
        message: message.to_owned(),
        attachments: Vec::new(),
    };
    let command = Command {
        v: wire::WireVersion::CURRENT,
        id: directive,
        epoch,
        kind: CommandKind::SteerSend,
        expected: vec![Witness::Residency {
            lane,
            occupant: Some(occupant),
            epoch,
        }],
        body: serde_json::to_value(&request).expect("steer request encodes"),
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
    let Outcome::Reply(Reply {
        body: CommandAnswer::Directive(answer),
        ..
    }) = steering::send(
        bus,
        epoch,
        &steer::Config::default(),
        None,
        &command,
        &request,
        &mut effects,
    )
    else {
        panic!("steering send commits");
    };
    assert_eq!(effects.inbox_lanes, [lane]);
    answer
}

fn read_inbox(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    command_id: CanonicalUuid,
) -> wire::intent::InboxAnswer {
    let request = wire::intent::SteerInbox { lane: Some(lane) };
    let command = Command {
        v: wire::WireVersion::CURRENT,
        id: command_id,
        epoch,
        kind: CommandKind::SteerInbox,
        expected: Vec::new(),
        body: serde_json::to_value(&request).expect("inbox request encodes"),
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
    .expect("operator inbox admits");
    let Outcome::Reply(Reply {
        body: CommandAnswer::Inbox(inbox),
        ..
    }) = steering::inbox(bus, paths, epoch, &command, &request)
    else {
        panic!("inbox reads durable authority");
    };
    inbox
}
