//! One lane's inbox projects its live directions and durable sibling metadata.

use super::*;

const OTHER: u8 = 2;
const RECIPIENT: u8 = 4;

mod cancellation;

struct Lanes {
    root: PathBuf,
    paths: RuntimePaths,
    bus: Bus,
    epoch: CanonicalUuid,
}

impl Lanes {
    /// Two occupied lanes.
    fn new(tag: &str) -> Self {
        let (root, paths) = resident_fixture(tag);
        let repository = Repository::discover(&root).expect("fixture repository resolves");
        let worktree = repository.worktree_root().to_string_lossy().into_owned();
        let surface =
            crate::trust_command::Surface::load(&repository).expect("fixture trust surface loads");
        let bus =
            Bus::start(journal::Journal::open(&paths.journal).expect("fixture journal opens"))
                .expect("fixture bus starts");
        let epoch = test_uuid(1);
        commit_startup(&bus, &repository, &surface, epoch, None).expect("resident seats");
        for lane in [OTHER, RECIPIENT] {
            seed_vendor_lane_in_worktree(
                &bus,
                epoch,
                test_uuid(lane),
                test_uuid(lane + 1),
                &worktree,
                "omp",
            );
        }
        Self {
            root,
            paths,
            bus,
            epoch,
        }
    }

    fn admit<B: serde::Serialize>(
        &self,
        kind: CommandKind,
        id: u8,
        witness: Witness,
        body: &B,
    ) -> wire::AdmittedCommand {
        Command {
            v: wire::WireVersion::CURRENT,
            id: test_uuid(id),
            epoch: self.epoch,
            kind,
            expected: vec![witness],
            body: serde_json::to_value(body).expect("request encodes"),
        }
        .admit(
            &wire::Accepted {
                wire: wire::WireVersion::CURRENT,
                actor: Actor::TEST_PRINCIPAL,
                epoch: self.epoch,
                release: "1.2.3".parse().expect("fixture release"),
            },
            &wire::principal::PrincipalRoles::default(),
        )
        .expect("operator command admits")
    }

    /// Direct one lane as the operator.
    fn send(&self, id: u8, to: u8, fanout: u8, message: &str) -> wire::intent::DirectiveAnswer {
        let outcome = self.broadcast(id, to, fanout, message, &[]);
        let Outcome::Reply(Reply {
            body: CommandAnswer::Directive(sent),
            ..
        }) = outcome
        else {
            panic!("operator direction is durable: {outcome:?}");
        };
        sent
    }

    /// Direct one lane as the operator, declaring the broadcast `recipients`.
    fn broadcast(
        &self,
        id: u8,
        to: u8,
        fanout: u8,
        message: &str,
        recipients: &[(u8, u8)],
    ) -> Outcome<CommandAnswer> {
        let send = wire::intent::SteerSend {
            fanout: test_uuid(fanout),
            recipients: recipients
                .iter()
                .map(|(lane, directive)| wire::body::SteeringRecipient {
                    lane: test_uuid(*lane),
                    directive: test_uuid(*directive),
                })
                .collect(),
            lane: Some(test_uuid(to)),
            message: message.to_owned(),
            attachments: Vec::new(),
        };
        let witness = Witness::Residency {
            lane: test_uuid(to),
            occupant: Some(test_uuid(to + 1)),
            epoch: self.epoch,
        };
        let command = self.admit(CommandKind::SteerSend, id, witness, &send);
        steering::send(
            &self.bus,
            self.epoch,
            &steer::Config::default(),
            None,
            &command,
            &send,
            &mut CommandEffects::default(),
        )
    }

    fn inbox(&self, id: u8, actor: Actor) -> wire::intent::InboxAnswer {
        let request = wire::intent::SteerInbox { lane: None };
        let command = Command {
            v: wire::WireVersion::CURRENT,
            id: test_uuid(id),
            epoch: self.epoch,
            kind: CommandKind::SteerInbox,
            expected: Vec::new(),
            body: serde_json::to_value(&request).expect("inbox request encodes"),
        }
        .admit(
            &wire::Accepted {
                wire: wire::WireVersion::CURRENT,
                actor,
                epoch: self.epoch,
                release: "1.2.3".parse().expect("fixture release"),
            },
            &wire::principal::PrincipalRoles::default(),
        )
        .expect("inbox admits");
        let Outcome::Reply(Reply {
            body: CommandAnswer::Inbox(inbox),
            ..
        }) = steering::inbox(&self.bus, &self.paths, self.epoch, &command, &request)
        else {
            panic!("inbox reads durable authority");
        };
        inbox
    }

    fn finish(self) {
        self.bus.verify().expect("the authority verifies");
        self.bus.shutdown().expect("fixture bus stops");
        fs::remove_dir_all(self.root).expect("fixture cleanup");
    }
}

#[test]
fn a_broadcast_names_only_durably_published_siblings_and_requires_its_own_identity() {
    let lanes = Lanes::new("steer-inbox-cc");
    let recipients = [(OTHER, 72), (RECIPIENT, 71)];
    let sent = lanes.broadcast(71, RECIPIENT, 69, "hold the parser", &recipients);
    assert!(matches!(sent, Outcome::Reply(_)), "{sent:?}");
    let sibling_refused = lanes.broadcast(73, OTHER, 69, "hold the parser", &[(RECIPIENT, 71)]);
    assert!(
        matches!(&sibling_refused, Outcome::Refusal(refusal) if refusal.reason() == "steer::recipients-invalid"),
        "{sibling_refused:?}"
    );
    let before = lanes.inbox(74, Actor::Lane(test_uuid(RECIPIENT)));
    assert!(
        before.lanes[0].pending[0].cc.is_empty(),
        "a refused sibling was not delivered: {before:?}"
    );

    let sibling = lanes.broadcast(72, OTHER, 69, "hold the parser", &recipients);
    assert!(matches!(sibling, Outcome::Reply(_)), "{sibling:?}");
    lanes.send(75, RECIPIENT, 76, "and keep the lexer");

    let other = lanes
        .bus
        .task_snapshot()
        .expect("lane codes read")
        .facts()
        .lane_code(test_uuid(OTHER))
        .expect("the other lane started an occupant");
    let inbox = lanes.inbox(77, Actor::Lane(test_uuid(RECIPIENT)));
    let [row] = inbox.lanes.as_slice() else {
        panic!("a lane principal reads only its own lane: {inbox:?}")
    };
    let cc = row
        .pending
        .iter()
        .map(|pending| (pending.text.as_str(), pending.cc.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        cc,
        [
            ("hold the parser", vec![other]),
            ("and keep the lexer", Vec::new())
        ]
    );
    lanes.finish();
}
