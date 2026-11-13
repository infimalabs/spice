//! A lane that hears steering by interjection is heard as its prose lands.

use super::*;
use wire::Disposition;

/// Where the fixture lane's records are captured and heard.
struct Heard {
    root: PathBuf,
    paths: RuntimePaths,
    bus: Bus,
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    directive: CanonicalUuid,
    key: String,
}

impl Heard {
    /// One occupied lane with one live direction.
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
        let lane = test_uuid(2);
        let occupant = test_uuid(3);
        commit_startup(&bus, &repository, &surface, epoch, None).expect("resident seats");
        seed_vendor_lane_in_worktree(&bus, epoch, lane, occupant, &worktree, "omp");
        let send = wire::intent::SteerSend {
            fanout: test_uuid(31),
            recipients: Vec::new(),
            lane: Some(lane),
            message: "answer as soon as you can".to_owned(),
            attachments: Vec::new(),
        };
        let command = Command {
            v: wire::WireVersion::CURRENT,
            id: test_uuid(30),
            epoch,
            kind: CommandKind::SteerSend,
            expected: vec![Witness::Residency {
                lane,
                occupant: Some(occupant),
                epoch,
            }],
            body: serde_json::to_value(&send).expect("steer request encodes"),
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
            &bus,
            epoch,
            &steer::Config::default(),
            None,
            &command,
            &send,
            &mut effects,
        );
        let Outcome::Reply(Reply {
            body: CommandAnswer::Directive(sent),
            ..
        }) = outcome
        else {
            panic!("the operator direction is durable: {outcome:?}");
        };
        Self {
            root,
            paths,
            bus,
            epoch,
            lane,
            occupant,
            directive: sent.directive,
            key: sent.key,
        }
    }

    /// Capture one assistant record answering the direction and sweep it as
    /// the record notice would.
    fn speak(&self, record: &[u8]) -> String {
        let tape = tape::Tape::open(self.paths.lane_tape(self.lane).join("omp"))
            .expect("the lane tape opens");
        let locus = tape.tee(record).expect("the answer is durable");
        let notice = supervisor::Notice::Record {
            lane: self.lane,
            occupant: self.occupant,
            stream: "stdout".to_owned(),
            boundary: "complete".to_owned(),
            locus: wire::TapeLocus {
                member: locus.member,
                offset: locus.offset,
                length: locus.length,
            },
        };
        let mut diagnostics = Vec::new();
        let residency = current_residency(&self.bus, self.epoch).expect("residency reads");
        let record = capture::materialize(&self.paths, residency.body(), notice, &mut diagnostics)
            .expect("the notice materializes")
            .expect("the seated occupant's record is read back");
        let mut seats = Seats::new();
        seats
            .seat_detached(
                self.lane,
                self.occupant,
                Box::new(DetachedFixture {
                    dropped: Arc::new(AtomicU64::new(0)),
                }),
            )
            .expect("the fixture occupant is seated");
        let source = message_source(self.bus.reader(), self.epoch);
        let publisher = source.message_publisher();
        let message_state = std::sync::Mutex::new(MessageState::default());
        let tastes = taste::Bag::new(Vec::new()).expect("empty taste bag");
        let steering = steer::Config::default();
        let joining = crate::board_command::JoiningFixture::discover(&self.root);
        let surfaces = CaptureSurfaces::new(
            &self.paths,
            &tastes,
            &steering,
            &publisher,
            joining.joining(),
        );
        message_surface::accept_capture_record(
            &self.bus,
            self.epoch,
            &mut seats,
            &surfaces,
            message_surface::TasteRecognition::Direct,
            &record,
            CaptureViews::new(&message_state, &mut diagnostics),
        )
        .expect("the answer is swept");
        seats.release_all();
        String::from_utf8(diagnostics).expect("diagnostics are UTF-8")
    }

    fn disposition(&self) -> Disposition {
        let directive = self.directive;
        self.bus
            .read(move |read| read.steering_head(directive))
            .expect("steering index reads")
            .expect("the operator direction is indexed")
            .body()
            .disposition
    }

    fn pending(&self) -> bool {
        steer::Ledger::from_snapshot(
            &self
                .bus
                .transact(|journal| {
                    bus::JournalTransaction::settled(
                        journal.fact_snapshot().expect("journal snapshot"),
                    )
                })
                .expect("snapshot reads"),
        )
        .expect("steering authority replays")
        .pending(self.lane)
        .iter()
        .any(|directive| directive.key().as_str() == self.key)
    }

    fn finish(self) {
        self.bus.verify().expect("the heard authority verifies");
        self.bus.shutdown().expect("fixture bus stops");
        fs::remove_dir_all(self.root).expect("fixture cleanup");
    }
}

#[test]
fn an_interjected_lane_s_answer_retires_its_key_as_the_prose_lands() {
    let heard = Heard::new("steer-heard-omp");
    let key = heard.key.clone();
    assert!(heard.pending());
    let record = format!(
        r#"{{"type":"message_end","message":{{"role":"assistant","content":[{{"type":"text","text":"ACK {key}: Answering now."}}],"stopReason":"stop"}}}}"#
    );

    heard.speak(format!("{record}\n").as_bytes());

    assert_eq!(heard.disposition(), Disposition::Acked);
    assert!(!heard.pending(), "the disposition retires the pending key");
    heard.finish();
}

#[test]
fn an_intent_acknowledgment_beside_unheaded_visible_text_retires_nothing() {
    let heard = Heard::new("steer-heard-intent-beside-text");
    let key = heard.key.clone();
    let record = format!(
        r#"{{"type":"message_end","message":{{"role":"assistant","content":[{{"type":"text","text":"Checking the listener first."}},{{"type":"toolCall","id":"call-1","name":"read","intent":"Reading the listener","arguments":{{}}}},{{"type":"toolCall","id":"call-2","name":"read","intent":"ACK {key}: answering in the second call","arguments":{{}}}}],"stopReason":"toolUse"}}}}"#
    );

    heard.speak(format!("{record}\n").as_bytes());

    assert_eq!(heard.disposition(), Disposition::Live);
    assert!(heard.pending(), "the intent leaves the key pending");
    heard.finish();
}
