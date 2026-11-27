//! An occupant's authored SYN packet publishes one peer message to each named
//! seated occupant exactly once per source packet, and names each address it
//! did not reach.

use std::collections::BTreeSet;

use wire::OccupantAddress;

use super::*;

#[path = "steering_message/rooms.rs"]
mod rooms;

#[path = "steering_message/room_history.rs"]
mod room_history;

#[path = "steering_message/conversation.rs"]
mod conversation;

#[path = "steering_message/design_awareness.rs"]
mod design_awareness;
#[path = "steering_message/principal_line.rs"]
mod principal_line;

#[path = "steering_message/intent.rs"]
mod intent;

#[path = "steering_message/delivery.rs"]
mod delivery;

#[path = "steering_message/replies.rs"]
mod replies;

#[path = "steering_message/receipts.rs"]
mod receipts;

#[path = "steering_message/corrections.rs"]
mod corrections;

#[path = "steering_message/boundaries.rs"]
mod boundaries;

#[path = "steering_message/budget.rs"]
mod budget;

#[path = "steering_message/lines.rs"]
mod lines;

#[path = "steering_message/line_arm.rs"]
mod line_arm;

#[path = "steering_message/outcomes.rs"]
mod outcomes;

#[path = "steering_message/principal_input.rs"]
mod principal_input;

#[path = "steering_message/principal_read.rs"]
mod principal_read;

mod outbox;

const SENDER: u8 = 2;
const FIRST: u8 = 4;
const SECOND: u8 = 6;
const THIRD: u8 = 8;

struct Lanes {
    root: PathBuf,
    paths: RuntimePaths,
    bus: Bus,
    epoch: CanonicalUuid,
    worktree: String,
    joining: crate::board_command::JoiningFixture,
}

impl Lanes {
    /// Three seated lanes, each occupant born on its own lane.
    fn new(tag: &str) -> Self {
        let (root, paths) = resident_fixture(tag);
        let repository = Repository::discover(&root).expect("fixture repository resolves");
        let worktree = repository.worktree_root().to_string_lossy().into_owned();
        let surface =
            crate::trust_command::Surface::load(&repository).expect("fixture trust surface loads");
        let bus =
            Bus::start(journal::Journal::open(&paths.journal).expect("fixture journal opens"))
                .expect("fixture bus starts");
        let epoch = crate::board_command::register_policy_repository(&root);
        commit_startup(&bus, &repository, &surface, epoch, None).expect("resident seats");
        let lanes = Self {
            joining: crate::board_command::JoiningFixture::discover(&root),
            root,
            paths,
            bus,
            epoch,
            worktree,
        };
        for lane in [SENDER, FIRST, SECOND] {
            lanes.seat(lane);
        }
        lanes
    }

    /// Actor of the bootstrap principal resident startup bore, which every
    /// read names as the author of an operator-recorded fact.
    fn bootstrap(&self) -> wire::Actor {
        let roles = self
            .bus
            .read(|read| read.principal_roles())
            .expect("principal roles read");
        wire::Actor::Principal(
            roles
                .bootstrap()
                .expect("startup bears the bootstrap principal"),
        )
    }

    /// Seat one more lane whose occupant is born on it now.
    fn seat(&self, lane: u8) {
        seed_vendor_lane_in_worktree(
            &self.bus,
            self.epoch,
            test_uuid(lane),
            test_uuid(lane + 1),
            &self.worktree,
            "omp",
        );
    }

    fn cursor(&self) -> u64 {
        self.bus
            .task_snapshot()
            .expect("snapshot reads")
            .cursor()
            .expect("the fixture journal has facts")
            .get()
    }

    fn address(&self, lane: u8) -> OccupantAddress {
        self.bus
            .task_snapshot()
            .expect("snapshot reads")
            .facts()
            .occupant_address(test_uuid(lane), test_uuid(lane + 1))
            .expect("the fixture occupant has one birth")
    }

    fn tape(&self, lane: u8) -> tape::Tape {
        tape::Tape::open(self.paths.lane_tape(test_uuid(lane)).join("omp"))
            .expect("the lane tape opens")
    }

    /// Land one complete streamed text block, and its settled twin, on a
    /// lane's tape, returning the streamed record's locus.
    fn author(&self, lane: u8, text: &str) -> listen::Locus {
        let tape = self.tape(lane);
        let streamed = serde_json::json!({
            "type": "message_update",
            "message": {"role": "assistant"},
            "assistantMessageEvent": {"type": "text_end", "contentIndex": 0, "content": text},
        });
        let locus = tape
            .tee(format!("{streamed}\n").as_bytes())
            .expect("the streamed block is durable");
        let settled = serde_json::json!({
            "type": "message_end",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": text}],
                "stopReason": "stop",
            },
        });
        tape.tee(format!("{settled}\n").as_bytes())
            .expect("the settled message is durable");
        locus
    }

    /// Land `text` as the successful result of one tool call and return the
    /// resident capture view.
    fn tool_result(&self, lane: u8, text: &str) -> lane::capture::CaptureRecord {
        let bytes = format!(
            "{}\n",
            serde_json::json!({
                "type": "message_end",
                "message": {
                    "role": "toolResult",
                    "toolCallId": "call-1",
                    "content": [{"type": "text", "text": text}],
                    "isError": false,
                },
            })
        );
        let locus = self
            .tape(lane)
            .tee(bytes.as_bytes())
            .expect("the record is durable");
        lane::capture::CaptureRecord {
            lane: test_uuid(lane),
            occupant: test_uuid(lane + 1),
            stream: vendor::RecordStream::Stdout,
            boundary: sill::RecordBoundary::Complete,
            locus,
            declaration: None,
            decoded: listen::decode(listen::Dialect::Omp, bytes.as_bytes(), locus)
                .expect("the record decodes"),
        }
    }

    /// Send `lane` one operator direction under command `id`, returning its key.
    fn direct(&self, lane: u8, id: u8, message: &str) -> String {
        self.direct_as(self.bootstrap(), lane, id, message)
    }

    /// Send `lane` one direction as `actor` under command `id`, admitted
    /// under the current principal roles, returning its key.
    fn direct_as(&self, actor: Actor, lane: u8, id: u8, message: &str) -> String {
        self.direct_with(actor, lane, id, message, &[])
    }

    /// Send `lane` one direction with `attachments` as `actor` under command
    /// `id`, admitted under the current principal roles, returning its key.
    fn direct_with(
        &self,
        actor: Actor,
        lane: u8,
        id: u8,
        message: &str,
        attachments: &[wire::Media],
    ) -> String {
        let roles = self
            .bus
            .read(|read| read.principal_roles())
            .expect("principal roles read");
        let sent = steering::send(
            &self.bus,
            self.epoch,
            &steer::Config::default(),
            None,
            &Command {
                v: wire::WireVersion::CURRENT,
                id: test_uuid(id),
                epoch: self.epoch,
                kind: CommandKind::SteerSend,
                expected: vec![Witness::Residency {
                    lane: test_uuid(lane),
                    occupant: Some(test_uuid(lane + 1)),
                    epoch: self.epoch,
                }],
                body: serde_json::json!({
                    "fanout": test_uuid(id + 1),
                    "lane": test_uuid(lane),
                    "message": message,
                    "attachments": attachments,
                }),
            }
            .admit(
                &wire::Accepted {
                    wire: wire::WireVersion::CURRENT,
                    actor,
                    epoch: self.epoch,
                    release: "1.2.3".parse().expect("fixture release"),
                },
                &roles,
            )
            .expect("operator send admits"),
            &wire::intent::SteerSend {
                fanout: test_uuid(id + 1),
                recipients: Vec::new(),
                lane: Some(test_uuid(lane)),
                message: message.to_owned(),
                attachments: attachments.to_vec(),
            },
            &mut CommandEffects::default(),
        );
        let Outcome::Reply(Reply {
            body: CommandAnswer::Directive(direction),
            ..
        }) = sent
        else {
            panic!("operator direction is durable: {sent:?}");
        };
        direction.key
    }

    /// Read every record on a lane's tape, as a sweep would.
    fn records(&self, lane: u8) -> (CanonicalUuid, Vec<listen::Record>) {
        let root = self.paths.lane_tape(test_uuid(lane)).join("omp");
        let scan = listen::scan(&root, listen::Dialect::Omp, None).expect("the tape scans");
        let source = self
            .tape(lane)
            .source_id()
            .expect("the tape has a generation")
            .as_str()
            .parse()
            .expect("the generation is a UUID");
        (source, scan.records)
    }

    /// Hear every record on the lane's tape as authored by `author`.
    fn hear(
        &self,
        lane: u8,
        author: CanonicalUuid,
    ) -> (
        Vec<Result<steering::Published, steering::Unpublished>>,
        String,
    ) {
        self.hear_with(lane, author, steering::Refusals::Record)
    }

    /// Hear every record on the lane's tape as authored by `author`, telling
    /// the author privately under [`steering::Refusals::Notify`].
    fn hear_with(
        &self,
        lane: u8,
        author: CanonicalUuid,
        refusals: steering::Refusals,
    ) -> (
        Vec<Result<steering::Published, steering::Unpublished>>,
        String,
    ) {
        self.hear_config(lane, author, refusals, &steer::Config::default())
    }

    fn hear_config(
        &self,
        lane: u8,
        author: CanonicalUuid,
        refusals: steering::Refusals,
        config: &steer::Config,
    ) -> (
        Vec<Result<steering::Published, steering::Unpublished>>,
        String,
    ) {
        let (source, records) = self.records(lane);
        let mut outcomes = Vec::new();
        let mut diagnostics = Vec::new();
        for record in records {
            let listen::RecordPayload::Events(events) = record.payload else {
                continue;
            };
            let events = events
                .into_iter()
                .map(|sourced| sourced.event)
                .collect::<Vec<_>>();
            outcomes.extend(steering::publish_authored(
                &self.bus,
                &self.paths,
                self.epoch,
                config,
                &steering::AuthoredRecord {
                    lane: test_uuid(lane),
                    author,
                    source,
                    locus: record.locus,
                    events: &events,
                },
                refusals,
                &mut diagnostics,
            ));
        }
        (
            outcomes,
            String::from_utf8(diagnostics).expect("diagnostics are UTF-8"),
        )
    }

    fn messages(&self) -> Vec<(wire::Envelope<wire::AnyFact>, wire::body::SteerMessaged)> {
        self.bus
            .transact(|journal| {
                bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
            })
            .expect("facts read")
            .facts()
            .iter()
            .filter_map(|envelope| {
                let body = envelope.fact.read::<wire::body::SteerMessaged>()?;
                Some((envelope.clone(), body))
            })
            .collect()
    }

    fn queued_messages(&self) -> Vec<(wire::Envelope<wire::AnyFact>, wire::body::SteerQueued)> {
        self.bus
            .transact(|journal| {
                bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
            })
            .expect("facts read")
            .facts()
            .iter()
            .filter_map(|envelope| {
                let body = envelope.fact.read::<wire::body::SteerQueued>()?;
                Some((envelope.clone(), body))
            })
            .collect()
    }

    fn withheld(&self) -> Vec<wire::body::SteerWithheld> {
        self.bus
            .transact(|journal| {
                bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
            })
            .expect("facts read")
            .facts()
            .iter()
            .filter_map(|envelope| envelope.fact.read::<wire::body::SteerWithheld>())
            .collect()
    }

    fn pending(&self, lane: u8) -> Vec<wire::intent::PendingDirective> {
        self.inbox(lane).pending
    }

    /// The lane's own inbox row: its pending inputs and open peer lines.
    fn inbox(&self, lane: u8) -> wire::intent::LaneInbox {
        let request = wire::intent::SteerInbox { lane: None };
        let command = Command {
            v: wire::WireVersion::CURRENT,
            id: test_uuid(90),
            epoch: self.epoch,
            kind: CommandKind::SteerInbox,
            expected: Vec::new(),
            body: serde_json::to_value(&request).expect("inbox request encodes"),
        }
        .admit(
            &wire::Accepted {
                wire: wire::WireVersion::CURRENT,
                actor: Actor::Lane(test_uuid(lane)),
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
        let [row] = inbox.lanes.as_slice() else {
            panic!("a lane principal reads only its own lane: {inbox:?}")
        };
        row.clone()
    }

    /// Withdraw one live directive as the operator.
    fn withdraw(&self, directive: CanonicalUuid) {
        let request = wire::intent::DirectiveRef { directive };
        let command = Command {
            v: wire::WireVersion::CURRENT,
            id: new_uuid(),
            epoch: self.epoch,
            kind: CommandKind::SteerCancel,
            expected: vec![Witness::Directive {
                steer: directive,
                disposition: wire::Disposition::Live,
            }],
            body: serde_json::to_value(&request).expect("cancel body"),
        }
        .admit(
            &wire::Accepted {
                wire: wire::WireVersion::CURRENT,
                actor: Actor::TEST_PRINCIPAL,
                epoch: self.epoch,
                release: "1.2.3".parse().expect("release"),
            },
            &wire::principal::PrincipalRoles::default(),
        )
        .expect("operator cancel admits");
        assert!(matches!(
            steering::cancel(
                &self.bus,
                self.epoch,
                &command,
                &request,
                &mut CommandEffects::default()
            ),
            Outcome::Reply(_)
        ));
    }

    /// Spend every start `from` holds for its vacant peer `to`, which shares
    /// no round with it. Each start queues one SYN the operator withdraws, so
    /// no admitted body remains for a later one to join.
    fn spend_starts(&self, from: u8, to: u8) {
        let address = self.address(to);
        for remaining in (0..5).rev() {
            self.author(from, &format!("SYN {address}: start"));
            let (_, outcomes) = self.hear_newest(from);
            assert!(
                matches!(
                    outcomes.as_slice(),
                    [Ok(steering::Published::Sent { resuming, .. })] if *resuming == [address]
                ),
                "a start remains: {outcomes:?}"
            );
            let queued = self.queued_messages();
            let (_, body) = queued.last().expect("the start queued its body");
            assert!(matches!(
                body.admission,
                Some(wire::body::PeerAdmission::PairToken(spend)) if spend.remaining == remaining
            ));
            self.withdraw(body.directive);
        }
    }

    fn vacate(&self, lane: u8) {
        self.vacate_as(lane, true);
    }

    fn vacate_unlaunchable(&self, lane: u8) {
        self.vacate_as(lane, false);
    }

    fn vacate_as(&self, lane: u8, launchable: bool) {
        let lane = test_uuid(lane);
        let observed = current_residency(&self.bus, self.epoch).expect("residency reads");
        let mut vacant = observed.body().clone();
        let row = vacant
            .lanes
            .iter_mut()
            .find(|row| row.lane == lane)
            .expect("fixture lane exists");
        let occupant = row.occupant.take().expect("the lane is seated");
        if !launchable {
            row.capabilities.clear();
        }
        row.disposition = "vacant".to_owned();
        let mut ending = write(Actor::Spice, self.epoch, Producer::ResidentRecognition)
            .expect("ending write starts");
        crate::resident::shutdown::guard_residency(&mut ending, &observed);
        let exited = ending
            .fact(
                wire::body::LaneExited {
                    lane,
                    occupant,
                    code: 0,
                },
                FactCause::Existing(observed.position()),
            )
            .expect("exit fact is valid");
        ending.residency_head(exited, move |_| vacant);
        commit(&self.bus, ending).expect("vacancy commits");
    }

    /// Seat `occupant` on a vacated lane with no `lane::started` fact, so the
    /// journal proves no birth for it.
    fn seat_unproved(&self, lane: u8, occupant: CanonicalUuid) {
        let lane = test_uuid(lane);
        let observed = current_residency(&self.bus, self.epoch).expect("residency reads");
        let mut reseated = observed.body().clone();
        let row = reseated
            .lanes
            .iter_mut()
            .find(|row| row.lane == lane)
            .expect("fixture lane exists");
        assert!(row.occupant.is_none(), "the lane is vacant");
        row.occupant = Some(occupant);
        row.lineage.push(occupant);
        row.disposition = "occupied".to_owned();
        let mut seating = write(Actor::Spice, self.epoch, Producer::ResidentRecognition)
            .expect("seating write starts");
        crate::resident::shutdown::guard_residency(&mut seating, &observed);
        let seated = seating
            .fact(
                wire::body::LaneExited {
                    lane,
                    occupant: test_uuid(99),
                    code: 0,
                },
                FactCause::Existing(observed.position()),
            )
            .expect("the carrier fact is valid");
        seating.residency_head(seated, move |_| reseated);
        commit(&self.bus, seating).expect("unproved seat commits");
    }

    /// Start another occupant on a vacated lane, born there now.
    fn renew(&self, lane: u8, occupant: CanonicalUuid) {
        let lane = test_uuid(lane);
        let observed = current_residency(&self.bus, self.epoch).expect("residency reads");
        let mut renewed = observed.body().clone();
        let row = renewed
            .lanes
            .iter_mut()
            .find(|row| row.lane == lane)
            .expect("fixture lane exists");
        assert!(row.occupant.is_none(), "the lane is vacant");
        row.occupant = Some(occupant);
        row.lineage.push(occupant);
        row.disposition = "occupied".to_owned();
        let mut starting = write(Actor::Spice, self.epoch, Producer::ResidentRecognition)
            .expect("starting write starts");
        crate::resident::shutdown::guard_residency(&mut starting, &observed);
        let started = starting
            .fact(
                wire::body::LaneStarted {
                    lane,
                    occupant,
                    epoch: self.epoch,
                    group: 123,
                    fresh_session: false,
                },
                FactCause::Existing(observed.position()),
            )
            .expect("start fact is valid");
        starting.residency_head(started, move |_| renewed);
        commit(&self.bus, starting).expect("renewal commits");
    }

    /// Declare the lane's seated occupant's session at the tape's current end.
    fn declare(&self, lane: u8) -> listen::Locus {
        self.declare_identity(lane, test_uuid(lane.saturating_add(20)).to_string())
    }

    fn declare_identity(&self, lane: u8, identity: String) -> listen::Locus {
        let declaration = self
            .tape(lane)
            .tee(
                format!(
                    "{{\"type\":\"session\",\"id\":\"{identity}\",\"version\":3,\
                     \"timestamp\":\"2030-04-12T18:44:09.271Z\",\"cwd\":\"/\"}}\n"
                )
                .as_bytes(),
            )
            .expect("the declaration is durable");
        let observed = current_residency(&self.bus, self.epoch).expect("residency reads");
        let mut declared = observed.body().clone();
        let row = declared
            .lanes
            .iter_mut()
            .find(|row| row.lane == test_uuid(lane))
            .expect("declared lane exists");
        let session = wire::LaneSession {
            lane: row.lane,
            occupant: row.occupant.expect("the declaring lane is seated"),
            worktree: row.worktree.clone(),
            vendor: row.vendor.clone(),
            launch: wire::body::LaneLaunchIdentity::new("sonnet", "high"),
            epoch: self.epoch,
            stream: "stdout".to_owned(),
            identity,
            locus: wire::TapeLocus {
                member: declaration.member,
                offset: declaration.offset,
                length: declaration.length,
            },
        };
        row.session = Some(session.clone());
        let mut write = write(Actor::Spice, self.epoch, Producer::ResidentRecognition)
            .expect("declaration write starts");
        crate::resident::shutdown::guard_residency(&mut write, &observed);
        let (source, _) = self.records(lane);
        let fact = write
            .fact(
                crate::resident::lane_surface::session_fact(&session, source),
                FactCause::Existing(observed.position()),
            )
            .expect("declaration fact is valid");
        write.residency_head(fact, move |_| declared);
        commit(&self.bus, write).expect("declaration commits");
        declaration
    }

    fn recover(&self) {
        steering::publish_at_startup(
            &self.bus,
            &self.paths,
            self.epoch,
            &steer::Config::default(),
            &mut Vec::new(),
        )
        .expect("recovery reads");
    }

    /// Replay startup's transcript recovery, as a restarted resident does.
    fn restart(&self) {
        crate::resident::ingest_surface::recover_transcripts(
            &self.bus,
            &self.paths,
            (self.epoch, self.joining.joining()),
            &steer::Config::default(),
            &mut Vec::new(),
        )
        .expect("startup recovery replays");
    }

    /// Land one assistant message on a lane's tape and hear its answers.
    fn answer(&self, lane: u8, keys: &[&str], prose: &str) {
        self.land(
            lane,
            keys,
            serde_json::json!([{"type": "text", "text": prose}]),
            "stop",
        );
    }

    /// Land one assistant message whose tool calls declare `intents` and
    /// hear its answers.
    fn intend(&self, lane: u8, keys: &[&str], intents: &[&str]) {
        let calls = intents
            .iter()
            .enumerate()
            .map(|(index, intent)| {
                serde_json::json!({
                    "type": "toolCall",
                    "id": format!("call-{index}"),
                    "name": "read",
                    "arguments": {},
                    "intent": intent,
                })
            })
            .collect();
        self.land(lane, keys, serde_json::Value::Array(calls), "toolUse");
    }

    fn land(&self, lane: u8, keys: &[&str], content: serde_json::Value, stop: &str) {
        let message = serde_json::json!({
            "type": "message_end",
            "message": {
                "role": "assistant",
                "content": content,
                "stopReason": stop,
            },
        });
        self.tape(lane)
            .tee(format!("{message}\n").as_bytes())
            .expect("the answer is durable");
        let keys = keys
            .iter()
            .map(|key| key.parse().expect("fixture key"))
            .collect();
        let vendor: wire::Name = "omp".parse().expect("fixture vendor");
        steering::dispose_heard(
            &self.bus,
            &self.paths,
            (
                &steer::Config::default(),
                self.epoch,
                self.joining.joining(),
            ),
            test_uuid(lane),
            &vendor,
            &keys,
            &mut Vec::new(),
        );
    }

    /// Park removed lanes' peer duties, answering the resident log they wrote.
    fn dead_letter_removed_lanes(&self) -> String {
        let mut log = Vec::new();
        steering::dead_letter_removed_lanes(&self.bus, &self.paths, self.epoch, &mut log)
            .expect("removed lanes read");
        String::from_utf8(log).expect("dead-letter log is UTF-8")
    }

    /// Remove one vacant lane from the residency.
    fn remove(&self, lane: u8) {
        let lane = test_uuid(lane);
        let observed = current_residency(&self.bus, self.epoch).expect("residency reads");
        let mut remaining = observed.body().clone();
        let row = remaining
            .lanes
            .iter()
            .find(|row| row.lane == lane)
            .cloned()
            .expect("fixture lane exists");
        assert!(row.occupant.is_none(), "only a vacant lane is removed");
        remaining.lanes.retain(|row| row.lane != lane);
        let mut removal = write(
            Actor::TEST_PRINCIPAL,
            self.epoch,
            Producer::ResidentRecognition,
        )
        .expect("removal write starts");
        crate::resident::shutdown::guard_residency(&mut removal, &observed);
        let removed = removal
            .fact(
                wire::body::LaneRemoved {
                    lane,
                    team: row.team,
                    worktree: row.worktree,
                },
                FactCause::Existing(observed.position()),
            )
            .expect("removal fact is valid");
        removal.residency_head(removed, move |_| remaining);
        commit(&self.bus, removal).expect("removal commits");
    }

    /// Ask, as the operator, to recover one parked directive.
    fn recover_directive(&self, directive: CanonicalUuid) -> Outcome<CommandAnswer> {
        let recover = wire::intent::DirectiveRef { directive };
        let command = Command {
            v: wire::WireVersion::CURRENT,
            id: test_uuid(91),
            epoch: self.epoch,
            kind: CommandKind::SteerRecover,
            expected: vec![Witness::Recoverable { steer: directive }],
            body: serde_json::to_value(&recover).expect("recovery encodes"),
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
        .expect("operator recovery admits");
        steering::recover(
            &self.bus,
            self.epoch,
            &steer::Config::default(),
            &command,
            &recover,
            &mut CommandEffects::default(),
        )
    }

    /// Judge the lane's newest streamed record live, as its seated author's.
    fn hear_newest(
        &self,
        lane: u8,
    ) -> (
        listen::Locus,
        Vec<Result<steering::Published, steering::Unpublished>>,
    ) {
        let (source, records) = self.records(lane);
        let newest = &records[records.len() - 2];
        let listen::RecordPayload::Events(events) = &newest.payload else {
            panic!("the streamed record decodes");
        };
        let events = events
            .iter()
            .map(|sourced| sourced.event.clone())
            .collect::<Vec<_>>();
        let record = steering::AuthoredRecord {
            lane: test_uuid(lane),
            author: test_uuid(lane + 1),
            source,
            locus: newest.locus,
            events: &events,
        };
        let outcomes = steering::publish_authored(
            &self.bus,
            &self.paths,
            self.epoch,
            &steer::Config::default(),
            &record,
            steering::Refusals::Record,
            &mut Vec::new(),
        );
        (newest.locus, outcomes)
    }

    fn finish(self) {
        self.bus.verify().expect("the authority verifies");
        self.bus.shutdown().expect("fixture bus stops");
        crate::board_command::unregister_policy_repository(&self.root);
        fs::remove_dir_all(self.root).expect("fixture cleanup");
    }
}

/// The streamed block is the packet's one source: its settled twin, a reread
/// of the same record, and an identical body at another locus each behave
/// as what they are.
#[test]
fn a_packet_publishes_its_exact_body_once_to_every_named_seated_occupant() {
    let lanes = Lanes::new("syn-publish");
    let (first, second) = (lanes.address(FIRST), lanes.address(SECOND));
    let packet = format!("Status first.\nSYN {first} {second}: rebase onto main\nthen rerun.");
    let authored = lanes.author(SENDER, &packet);

    let (outcomes, diagnostics) = lanes.hear(SENDER, test_uuid(SENDER + 1));
    assert_eq!(
        outcomes,
        [Ok(sent_lines(&[FIRST, SECOND], 2))],
        "only the streamed block is a source: {diagnostics}"
    );
    let messages = lanes.messages();
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0].0.seq.get() + 1,
        messages[1].0.seq.get(),
        "both messages commit in one write"
    );
    for ((envelope, body), (lane, occupant)) in messages
        .iter()
        .zip([(FIRST, FIRST + 1), (SECOND, SECOND + 1)])
    {
        assert_eq!(envelope.actor, Actor::Lane(test_uuid(SENDER)));
        assert_eq!(body.lane, test_uuid(lane));
        assert_eq!(body.occupant, test_uuid(occupant));
        assert_eq!(body.from, test_uuid(SENDER));
        assert_eq!(body.sender, test_uuid(SENDER + 1));
        assert_eq!(body.text, "rebase onto main\nthen rerun.");
        assert_eq!(
            (body.locus.member, body.locus.offset, body.locus.line),
            (authored.member, authored.offset, 2)
        );
        let pending = lanes.pending(lane);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].text, "rebase onto main\nthen rerun.");
        assert_eq!(pending[0].from, Some(test_uuid(SENDER)));
    }

    let (reread, _) = lanes.hear(SENDER, test_uuid(SENDER + 1));
    assert_eq!(reread, [Ok(steering::Published::Already)]);
    assert_eq!(lanes.messages().len(), 2, "a reread sends nothing");

    lanes.author(SENDER, &packet);
    let (outcomes, _) = lanes.hear(SENDER, test_uuid(SENDER + 1));
    assert_eq!(
        outcomes,
        [
            Ok(steering::Published::Already),
            Ok(sent_lines(&[FIRST, SECOND], 2)),
        ],
        "the same body authored again is another message"
    );
    assert_eq!(lanes.pending(FIRST).len(), 2);
    lanes.finish();
}

fn sent(lanes: &[u8], unreached: &[&str]) -> steering::Published {
    sent_queued(lanes, &[], unreached)
}

/// A delivery to these lanes whose body carried `count` lines.
fn sent_lines(lanes: &[u8], count: usize) -> steering::Published {
    let mut published = sent(lanes, &[]);
    if let steering::Published::Sent { lines, .. } = &mut published {
        *lines = count;
    }
    published
}

fn sent_queued(
    lanes: &[u8],
    queued: &[wire::OccupantAddress],
    unreached: &[&str],
) -> steering::Published {
    steering::Published::Sent {
        lanes: lanes.iter().map(|lane| test_uuid(*lane)).collect(),
        queued: queued.to_vec(),
        resuming: queued.to_vec(),
        redirected: Vec::new(),
        unreached: unreached.iter().map(|entry| (*entry).to_owned()).collect(),
        spent: Vec::new(),
        rooms: Vec::new(),
        principals: 0,
        held_room: false,
        lines: 1,
    }
}

/// Plain direct routes to these addresses.
fn direct(addresses: &[wire::OccupantAddress]) -> Vec<steer::ack::scan::Route> {
    addresses
        .iter()
        .map(|address| steer::ack::scan::Route {
            audience: Some(steer::ack::scan::Audience::Occupant(*address)),
            context: None,
            direction: None,
            span: 0..0,
        })
        .collect()
}

/// The delivery a recipient hears names the exact body, the sending
/// occupant's address, and its own key, and the wake its admission committed.
fn delivered(lanes: &Lanes, lane: u8) -> wire::intent::PendingDirective {
    let [message] = lanes
        .pending(lane)
        .try_into()
        .unwrap_or_else(|pending| panic!("one SYN is pending: {pending:?}"));
    assert!(
        message.wakes,
        "an admitted peer message keeps the wake its admission committed"
    );
    assert_eq!(
        message.sender,
        Some(wire::intent::PeerSender {
            occupant: test_uuid(SENDER + 1),
            address: Some(lanes.address(SENDER)),
        })
    );
    assert_eq!(message.room, None, "a direct route freezes no room");
    let seal: steer::Key = "1kQnrzNZx".parse().expect("fixture seal");
    let rendered = steer::inbox::render_pending(
        &seal,
        std::slice::from_ref(&message),
        message.authored,
        |_, _| None,
    );
    for part in [
        message.key.as_str(),
        message.text.as_str(),
        &lanes.address(SENDER).to_string(),
    ] {
        assert!(rendered.contains(part), "{part} is delivered: {rendered}");
    }
    assert!(
        rendered.contains(&format!(
            "[PEER sender {} lane {}]",
            lanes.address(SENDER),
            test_uuid(SENDER)
        )),
        "a roomless peer header is unchanged: {rendered}"
    );
    message
}
