//! A packet's `#TASK` contexts record its exact source bytes once in each
//! named room, and a directed route freezes its room on the delivery.

use super::*;

impl Lanes {
    /// Seed one task a SYN context can name as its room.
    pub(super) fn room(&self, stamp: &str) -> wire::TaskHandle {
        self.seed_task(stamp, "Room", None, None, None)
    }

    /// Seed one room task `holder`'s lane holds an active claim on.
    fn held_room(&self, stamp: &str, holder: u8) -> wire::TaskHandle {
        self.seed_task(stamp, "Room", None, None, Some(test_uuid(holder)))
    }

    /// Seed one pending task with this statement, held by `holder` when named.
    pub(super) fn seed_task(
        &self,
        stamp: &str,
        title: &str,
        description: Option<&str>,
        intent: Option<wire::TaskIntent>,
        holder: Option<CanonicalUuid>,
    ) -> wire::TaskHandle {
        self.seed_task_with_term(stamp, title, description, intent, holder, &[])
    }

    /// Seed one pending task with explicit statement terms.
    pub(super) fn seed_task_with_term(
        &self,
        stamp: &str,
        title: &str,
        description: Option<&str>,
        intent: Option<wire::TaskIntent>,
        holder: Option<CanonicalUuid>,
        term: &[&str],
    ) -> wire::TaskHandle {
        self.seed_held_task(stamp, title, description, intent, holder.as_slice(), term)
    }

    /// Seed one pending todo task whose current round every lane in
    /// `holders` actively claims.
    pub(super) fn seed_round(&self, stamp: &str, holders: &[u8]) -> wire::TaskHandle {
        let holders = holders
            .iter()
            .map(|lane| test_uuid(*lane))
            .collect::<Vec<_>>();
        self.seed_held_task(stamp, "Round", None, None, &holders, &[])
    }

    fn seed_held_task(
        &self,
        stamp: &str,
        title: &str,
        description: Option<&str>,
        intent: Option<wire::TaskIntent>,
        holders: &[CanonicalUuid],
        term: &[&str],
    ) -> wire::TaskHandle {
        let task: wire::TaskStamp = stamp.parse().expect("fixture stamp");
        let phase: wire::Phase = if intent.is_some_and(wire::TaskIntent::is_design) {
            "design"
        } else {
            "todo"
        }
        .parse()
        .expect("fixture phase");
        let creation = wire::body::TaskCreationContext::unavailable(
            wire::body::TaskCreationUnavailable::SpiceActor,
        );
        let term = term
            .iter()
            .map(|item| (*item).to_owned())
            .collect::<Vec<_>>();
        let pitch = "This is enough.".to_owned();
        let mut seed =
            write(Actor::Spice, self.epoch, Producer::CommandDoor).expect("task write starts");
        let added = seed
            .fact(
                wire::body::TaskAdded {
                    task: task.clone(),
                    title: title.to_owned(),
                    pitch: pitch.clone(),
                    description: description.map(str::to_owned),
                    term: term.clone(),
                    requirements: wire::EnvironmentRequirements::default(),
                    topic: "/serve/rooms".parse().expect("fixture topic"),
                    submitted: false,
                    priority: None,
                    taste: None,
                    depends: Vec::new(),
                    intent,
                    preparation: None,
                    flow: vec![wire::Phase::Draft, phase],
                    phase,
                    revision: 1,
                    creation: creation.clone(),
                },
                FactCause::Root,
            )
            .expect("task fact is valid");
        let born = task.clone();
        let title = title.to_owned();
        let description = description.map(str::to_owned);
        seed.task_head(added, move |position| wire::TaskHead {
            preparation: None,
            intent,
            born,
            topic_origin: position,
            title,
            pitch,
            description,
            term,
            requirements: wire::EnvironmentRequirements::default(),
            resources: wire::ResourceRequirements::default(),
            topic: "/serve/rooms".parse().expect("fixture topic"),
            priority: None,
            taste: None,
            dependencies: Vec::new(),
            provenance: wire::TaskProvenance::OperatorDraft,
            creation,
            revision: position,
        });
        seed.workflow_head(task.clone(), added, move |_| wire::WorkflowHead {
            flow: vec![wire::Phase::Draft, phase],
            phase,
            status: wire::WorkflowStatus::pending(),
            deferral: None,
            submitted: false,
            generation: 0,
        });
        for lane in holders {
            self.claim(&mut seed, &task, added, *lane);
        }
        commit(&self.bus, seed).expect("task commits");
        format!("ROOMS-{task}").parse().expect("fixture handle")
    }

    /// Stage `lane`'s active claim, with its lease, on the task `added` adds.
    fn claim(
        &self,
        seed: &mut JournalWrite,
        task: &wire::TaskStamp,
        added: journal::PendingFact,
        lane: CanonicalUuid,
    ) {
        let occupant = current_residency(&self.bus, self.epoch)
            .expect("residency reads")
            .body()
            .lanes
            .iter()
            .find(|row| row.lane == lane)
            .and_then(|row| row.occupant)
            .expect("claimed lane is occupied");
        let cell = wire::ClaimCell {
            topic: Some("/serve/rooms".parse().expect("fixture topic")),
            phase: "todo".parse().expect("fixture phase"),
        };
        let until: wire::Instant = "2030-08-01T07:00:00.000Z"
            .parse()
            .expect("fixture claim expiry");
        let won = seed
            .fact(
                wire::body::ClaimWon {
                    task: task.clone(),
                    actor: Actor::Lane(lane),
                    lane,
                    occupant,
                    epoch: self.epoch,
                    cell: cell.clone(),
                    until,
                    git: None,
                    recovery: None,
                    source: wire::body::SourceBoundary::Proven { heads: Vec::new() },
                },
                FactCause::Pending(added),
            )
            .expect("claim fact is valid");
        let epoch = self.epoch;
        seed.claim_head(task.clone(), won, move |fact| wire::ClaimHead {
            actor: Actor::Lane(lane),
            fact,
            claimed: fact,
            cell,
            lane,
            occupant,
            epoch,
            recovery: None,
            disposition: wire::ClaimDisposition::Active,
            acknowledged: 0,
        });
        seed.lease_head(task.clone(), won, move |claimed| wire::LeaseHead {
            claimed,
            start: until,
            renewal: until,
            expiry: until,
            maximum: until,
            epoch,
        });
    }

    /// Account every record now on a lane's tape, as its sweep would.
    pub(super) fn account(&self, lane: u8) {
        let (source, records) = self.records(lane);
        let last = records.last().expect("the tape has records").locus;
        let head = wire::TranscriptSourceHead {
            lane: test_uuid(lane),
            vendor: "omp".parse().expect("fixture vendor"),
            stream: "session".to_owned(),
            source,
            next: wire::body::TranscriptPosition {
                member: last.member,
                offset: last.offset + last.length,
            },
        };
        let mut accounting = write(Actor::Spice, self.epoch, Producer::ResidentRecognition)
            .expect("accounting write starts");
        let ingested = accounting
            .fact(
                wire::body::JournalIngested {
                    lane: head.lane,
                    vendor: head.vendor.clone(),
                    stream: head.stream.clone(),
                    source,
                    supersedes: None,
                    from: wire::body::TranscriptPosition {
                        member: 0,
                        offset: 0,
                    },
                    to: head.next,
                    usage: Vec::new(),
                    non_usage_records: 0,
                    residue_records: 0,
                },
                FactCause::Root,
            )
            .expect("accounted batch is valid");
        accounting.transcript_source_head(ingested, move |_| head);
        commit(&self.bus, accounting).expect("accounting commits");
    }

    /// Replace one lane's accounted transcript source without changing its
    /// vendor session.
    pub(super) fn replace_transcript_source(&self, lane: u8) -> CanonicalUuid {
        let mut head = self
            .bus
            .read(move |read| read.transcript_source_heads(&test_uuid(lane)))
            .expect("transcript sources read")
            .into_iter()
            .next()
            .expect("declared lane has a transcript source")
            .into_body();
        let superseded = head.source;
        let source = test_uuid(99);
        head.source = source;
        let mut accounting = write(Actor::Spice, self.epoch, Producer::ResidentRecognition)
            .expect("replacement accounting write starts");
        let ingested = accounting
            .fact(
                wire::body::JournalIngested {
                    lane: head.lane,
                    vendor: head.vendor.clone(),
                    stream: head.stream.clone(),
                    source,
                    supersedes: Some(superseded),
                    from: wire::body::TranscriptPosition {
                        member: head.next.member,
                        offset: 0,
                    },
                    to: head.next,
                    usage: Vec::new(),
                    non_usage_records: 0,
                    residue_records: 0,
                },
                FactCause::Root,
            )
            .expect("replacement batch is valid");
        accounting.transcript_source_head(ingested, move |_| head);
        commit(&self.bus, accounting).expect("replacement accounting commits");
        source
    }

    pub(super) fn rooms(&self) -> Vec<wire::rooms::RoomMessaged> {
        self.bus
            .transact(|journal| {
                bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
            })
            .expect("facts read")
            .facts()
            .iter()
            .filter_map(|envelope| envelope.fact.read::<wire::rooms::RoomMessaged>())
            .collect()
    }
    /// `spice room WORDS...` read in process, as its JSON reply body.
    pub(super) fn room_read(&self, words: &[&str]) -> serde_json::Value {
        let matches = crate::cli::command()
            .try_get_matches_from(["spice", "room"].iter().chain(words))
            .expect("room read parses");
        let contract = crate::cli::selected_contract(&matches).expect("room read is registered");
        let leaf = crate::cli::selected_matches(&matches);
        let journal =
            journal::JournalReader::open(&self.paths.journal).expect("the journal opens to read");
        let body = match contract.path {
            ["room", "inbox"] => {
                match crate::room_command::inbox::answer(&journal, leaf, contract.path) {
                    Outcome::Reply(reply) => serde_json::to_value(&reply.body),
                    other => panic!("room inbox answered {other:?}"),
                }
            }
            ["room", "awareness"] => {
                match crate::room_command::awareness::answer(&journal, leaf, contract.path) {
                    Outcome::Reply(reply) => serde_json::to_value(&reply.body),
                    other => panic!("room awareness answered {other:?}"),
                }
            }
            other => panic!("not a room read: {other:?}"),
        };
        body.expect("the read encodes")
    }
}

/// `#R` records the packet's exact bytes in R and tells only the author;
/// `A#R` also opens a room-bound delivery for A, queued while A's proved lane
/// is vacant; a packet with no seated route still records in R; equal text at
/// another locus records again; a reread writes nothing.
#[test]
fn room_routes_record_each_source_once_and_deliver_directed_routes() {
    let lanes = Lanes::new("syn-room");
    let room = lanes.room("dVrD244Jd");
    let born = room.born().expect("fixture handle carries its birth");
    let (sender, first, second) = (
        lanes.address(SENDER),
        lanes.address(FIRST),
        lanes.address(SECOND),
    );
    lanes.vacate(SECOND);
    let block =
        format!("SYN #{room}: for the room\n\nSYN {first}#{room} {second}#{room}: for first");
    lanes.author(SENDER, &block);
    lanes.author(SENDER, &format!("SYN #{room}: for the room"));
    lanes.author(SENDER, &format!("SYN {second}#{room}: for no one seated"));
    lanes.author(
        SENDER,
        &format!("SYN {first}#{room} {sender}#{room}: keep valid room fanout"),
    );
    lanes.account(SENDER);

    let (outcomes, diagnostics) = lanes.hear(SENDER, test_uuid(SENDER + 1));
    let recorded = |lanes: &[u8], queued: &[wire::OccupantAddress], unreached: &[String]| {
        Ok(steering::Published::Sent {
            lanes: lanes.iter().map(|lane| test_uuid(*lane)).collect(),
            queued: queued.to_vec(),
            resuming: queued.to_vec(),
            redirected: Vec::new(),
            unreached: unreached.to_vec(),
            spent: Vec::new(),
            rooms: vec![room.clone()],
            principals: 0,
            held_room: false,
            lines: 1,
        })
    };
    assert_eq!(
        outcomes,
        [
            recorded(&[], &[], &[]),
            recorded(&[FIRST], &[second], &[]),
            recorded(&[], &[], &[]),
            recorded(&[], &[second], &[]),
            recorded(
                &[FIRST],
                &[],
                &[format!(
                    "{sender} syn::recipient-self \
                     ({sender} is the authoring occupant's own address)"
                )]
            ),
        ],
        "{diagnostics}"
    );
    let messages = lanes.messages();
    assert_eq!(messages.len(), 2, "both valid seated routes deliver");
    assert!(
        messages
            .iter()
            .all(|(_, message)| message.lane == test_uuid(FIRST)
                && message.room == Some(born.clone()))
    );
    let queued = lanes.queued_messages();
    assert_eq!(queued.len(), 2);
    assert!(
        queued
            .iter()
            .all(|(_, message)| message.room == Some(born.clone()))
    );
    let pending = lanes.pending(FIRST);
    assert_eq!(pending.len(), 2, "both valid room messages are pending");
    assert!(
        pending
            .iter()
            .all(|message| message.room.as_ref() == Some(&room)),
        "every delivery retains its room"
    );
    let seal: steer::Key = "1kQnrzNZx".parse().expect("fixture seal");
    let now = pending[0].authored;
    let rendered = steer::inbox::render_pending(&seal, &pending[..1], now, |_, _| None);
    assert!(
        rendered.contains(&format!(
            "[PEER sender {} lane {} room {room}]",
            lanes.address(SENDER),
            test_uuid(SENDER)
        )),
        "the delivery header names the frozen room: {rendered}"
    );
    let records = lanes.rooms();
    assert_eq!(records.len(), 5, "each packet records once: {records:?}");
    assert!(records.iter().all(|record| record.room == born));
    let bodies = records[..2]
        .iter()
        .map(|record| &block[record.hunk.text.start as usize..record.hunk.text.end as usize])
        .collect::<Vec<_>>();
    assert_eq!(
        bodies,
        ["for the room", "for first"],
        "each hunk names its body"
    );
    assert_eq!(records[0].hunk.text, records[2].hunk.text);
    assert!(
        !records[0].hunk.same_bytes(&records[2].hunk),
        "equal text at another locus is another publication"
    );

    let (reread, _) = lanes.hear(SENDER, test_uuid(SENDER + 1));
    assert_eq!(reread, [const { Ok(steering::Published::Already) }; 5]);
    assert_eq!(lanes.rooms().len(), 5, "a reread records nothing");
    assert_eq!(lanes.messages().len(), 2, "a reread sends nothing");
    lanes.finish();
}

/// More room publications than one unread page holds.
const RETAINED_BACKLOG: usize = 18;
/// What a lane with nothing unread reads.
pub(super) const NOTHING_UNREAD: &str = "room inbox\nno unread room activity";

impl Lanes {
    /// The unread room page `lane` reads now, as its text.
    pub(super) fn unread(&self, lane: u8) -> String {
        let journal =
            journal::JournalReader::open(&self.paths.journal).expect("the journal opens to read");
        let owners = crate::room_command::SourceOwners {
            tape: &self.paths.tape,
            artifacts: &self.paths.artifacts(),
            git: &self.paths.common,
        };
        match crate::room_command::unread::answer(&journal, owners, test_uuid(lane)) {
            Outcome::Reply(reply) => reply.body.to_string(),
            other => panic!("the unread page answered {other:?}"),
        }
    }
}

fn seat(seats: &mut Seats, lane: u8) {
    seats
        .seat_detached(
            test_uuid(lane),
            test_uuid(lane + 1),
            Box::new(DetachedFixture {
                dropped: Arc::new(AtomicU64::new(0)),
            }),
        )
        .expect("listener seats");
}

/// A lane reads the publications after its cursor in the rooms it listens
/// to, oldest first within one page budget, and reading writes nothing. Its
/// cursor moves only when the page's closing line lands as a tool result in
/// its accounted transcript, and then only past what that page showed; until
/// then the same page reads again. A lane merely addressed in a room it holds
/// no work in reads nothing.
#[test]
fn a_lane_reads_unread_room_activity_and_only_a_landed_page_advances_its_cursor() {
    let lanes = Lanes::new("room-unread");
    let room = lanes.held_room("dVrD245Kc", FIRST);
    let first_lane = test_uuid(FIRST);
    let address = lanes.address(SECOND);
    lanes.author(SENDER, &format!("SYN {address}#{room}: second addressed"));
    for item in 0..RETAINED_BACKLOG {
        lanes.author(SENDER, &format!("SYN #{room}: retained {item}"));
    }
    lanes.account(SENDER);
    let _ = lanes.hear(SENDER, test_uuid(SENDER + 1));

    let before = lanes.cursor();
    let page = lanes.unread(FIRST);
    assert!(page.contains("second addressed"), "{page}");
    assert!(page.contains("retained 0"), "{page}");
    assert!(
        !page.contains(&format!("retained {}", RETAINED_BACKLOG - 1)),
        "one page stays within its budget: {page}"
    );
    assert!(page.contains("more unread activity remains"), "{page}");
    assert!(
        page.contains(&format!("listening {room}: work {room}")),
        "{page}"
    );
    assert_eq!(
        lanes.unread(SECOND),
        NOTHING_UNREAD,
        "a directed packet reaches its lane by steering, never as unread activity"
    );
    assert_eq!(lanes.unread(FIRST), page, "an unlanded page reads again");
    assert_eq!(lanes.cursor(), before, "reading writes nothing");

    let (through, closing) = crate::room_command::unread::delivery(&page, first_lane)
        .expect("a shown page closes with its delivery line");
    let quoted = lanes.tool_result(FIRST, &format!("  | {}", &page[closing]));
    // A `spice://` read reaches the lane with each line numbered.
    let numbered = page
        .lines()
        .enumerate()
        .map(|(line, text)| format!("{}:{text}\n", line + 1))
        .collect::<String>();
    let landed = lanes.tool_result(FIRST, &numbered);
    let source = lanes
        .tape(FIRST)
        .source_id()
        .expect("tape source reads")
        .as_str()
        .parse()
        .expect("tape source is a UUID");
    let cursor = || {
        lanes
            .bus
            .read(move |read| read.room_cursor(first_lane))
            .expect("cursor reads")
            .map(|head| head.body().through.get())
    };
    assert!(
        room_catch_up::recognize(&lanes.bus, &landed, source)
            .expect("recognition reads")
            .is_none(),
        "a page in an unaccounted record advances nothing"
    );
    let mut diagnostics = Vec::new();
    let _ = ingest_transcripts(
        &lanes.bus,
        &lanes.paths,
        Some(first_lane),
        lane::ingest::Bound::IMMEDIATE,
        lane::ingest::Coalescing::default(),
        &mut diagnostics,
    )
    .expect("the sweep accounts the lane's tape");
    assert!(
        room_catch_up::recognize(&lanes.bus, &quoted, source)
            .expect("recognition reads")
            .is_none(),
        "a quoted delivery line is not a page the lane read"
    );
    assert_eq!(cursor(), None);

    let delivered = room_catch_up::recognize(&lanes.bus, &landed, source)
        .expect("recognition reads")
        .expect("the landed page advances the cursor");
    assert_eq!(cursor(), Some(through.get()));
    assert_eq!(
        room_catch_up::changes(&lanes.bus, delivered).expect("delivery changes classify"),
        room_catch_up::Changes {
            lanes: BTreeSet::from([first_lane]),
            rooms: BTreeSet::new(),
            tasks: BTreeSet::new(),
        },
        "cursor progress names only its lane"
    );
    assert!(
        room_catch_up::recognize(&lanes.bus, &landed, source)
            .expect("recognition reads")
            .is_none(),
        "a page at the cursor advances nothing"
    );

    let rest = lanes.unread(FIRST);
    assert!(
        rest.contains(&format!("retained {}", RETAINED_BACKLOG - 1)),
        "{rest}"
    );
    assert!(
        !rest.contains("retained 0\n") && !rest.contains("second addressed"),
        "the cursor passed exactly what the page showed: {rest}"
    );
    assert!(
        diagnostics.is_empty(),
        "{}",
        String::from_utf8_lossy(&diagnostics)
    );
    lanes.finish();
}

/// Recording a Design request admits its room from that request on: earlier
/// room context stays excluded, while a later room post reaches the asked
/// lane and names the request it listens through.
#[test]
fn a_design_request_admits_later_room_activity_for_the_asked_lane() {
    let lanes = Lanes::new("room-unread-design-request");
    let prior = lanes.held_room("dVrD245Kc", FIRST);
    let design = lanes.seed_task(
        "dVrD245Kd",
        "Design",
        Some("Decide how room context should reach an asked lane."),
        Some(wire::TaskIntent::Design),
        None,
    );
    let first_lane = test_uuid(FIRST);
    let mut seats = Seats::new();
    seat(&mut seats, FIRST);
    let mut diagnostics = Vec::new();
    let mut reached = |commit| {
        let changes = room_catch_up::changes(&lanes.bus, commit).expect("changes classify");
        room_catch_up::reached(&lanes.bus, &seats, &changes, &mut diagnostics)
    };

    append_room_publication(&lanes, &design);
    let design_stamp = design.born().expect("fixture Design has a birth");
    let noticed = record_design_request(&lanes, &design_stamp, &prior, first_lane);
    assert!(
        reached(noticed).is_empty(),
        "the request cannot retroactively admit room context"
    );
    assert_eq!(lanes.unread(FIRST), NOTHING_UNREAD);

    assert_eq!(
        reached(append_room_publication(&lanes, &design)),
        [first_lane]
    );
    let page = lanes.unread(FIRST);
    assert_eq!(
        page.matches("appended before accounting").count(),
        1,
        "{page}"
    );
    assert!(page.contains("asked from"), "{page}");
    assert!(
        diagnostics.is_empty(),
        "{}",
        String::from_utf8_lossy(&diagnostics)
    );
    seats.release_all();
    lanes.finish();
}

/// The match current selection records, as a fixture notice carries it.
fn fixture_admission() -> wire::rooms::DesignNoticeMatch {
    wire::rooms::DesignNoticeMatch::new(wire::rooms::DesignReach::Wide, 0.2, 0.1, [1; 32])
        .expect("fixture match clears its floor")
}

fn record_design_request(
    lanes: &Lanes,
    design: &wire::TaskStamp,
    prior: &wire::TaskHandle,
    lane: CanonicalUuid,
) -> bus::JournalCommit {
    let statement = lanes
        .bus
        .read({
            let design = design.clone();
            move |read| read.task_head(&design)
        })
        .expect("Design statement reads")
        .expect("Design statement exists")
        .body()
        .revision;
    let prior = prior.born().expect("fixture prior task has a birth");
    let mut request = write(Actor::Spice, lanes.epoch, Producer::ResidentRecognition)
        .expect("request write starts");
    request
        .fact(
            wire::rooms::DesignNoticed {
                design: design.clone(),
                statement,
                lane,
                occupant: test_uuid(FIRST + 1),
                session: wire::rooms::DesignSession {
                    vendor: "omp".parse().expect("vendor"),
                    identity: "fixture-session".to_owned(),
                },
                compaction: None,
                opportunity: wire::rooms::DesignNoticeOpportunity::TaskCompletion {
                    task: prior.clone(),
                    completed: statement,
                },
                evidence: wire::rooms::DesignNoticeEvidence::Task {
                    task: prior,
                    statement,
                    completed: statement,
                    support: wire::rooms::DesignSupport::Implemented,
                },
                excerpt: "context room".to_owned(),
                terms: vec!["context".to_owned(), "room".to_owned()],
                admission: fixture_admission(),
                asks: Vec::new(),
            },
            FactCause::Root,
        )
        .expect("Design request is valid");
    commit(&lanes.bus, request).expect("Design request commits")
}

fn append_room_publication(lanes: &Lanes, room: &wire::TaskHandle) -> bus::JournalCommit {
    let publication = wire::rooms::RoomAuthorityStamp::for_micros(
        1_790_000_000_000_000
            + i64::try_from(lanes.cursor()).expect("fixture cursor fits a timestamp"),
    )
    .expect("fixture publication stamp");
    let mut append = write(Actor::TEST_PRINCIPAL, lanes.epoch, Producer::CommandDoor)
        .expect("post write starts");
    append
        .fact(
            wire::rooms::RoomPublished {
                room: room.born().expect("fixture room has a birth"),
                publication,
                content: wire::rooms::RoomPublicationContent::Authored {
                    text: "appended before accounting".to_owned(),
                },
            },
            FactCause::Root,
        )
        .expect("intervening publication is valid");
    commit(&lanes.bus, append).expect("intervening publication commits")
}

/// A context naming no task refuses the whole packet: no route delivers and
/// no room records.
#[test]
fn an_unknown_context_refuses_every_route() {
    let lanes = Lanes::new("syn-room-unknown");
    let room = lanes.room("dVrD244Jd");
    let (first, second) = (lanes.address(FIRST), lanes.address(SECOND));
    lanes.author(
        SENDER,
        &format!("SYN {first}#{room} {second}#ROOMS-dVs4wZ9Q3: lost"),
    );
    lanes.account(SENDER);

    let (outcomes, _) = lanes.hear(SENDER, test_uuid(SENDER + 1));
    assert!(
        matches!(
            outcomes.as_slice(),
            [Err(steering::Unpublished::Recorded { reason, .. })]
                if reason == "syn::context-unknown"
        ),
        "{outcomes:?}"
    );
    assert!(lanes.messages().is_empty());
    assert!(lanes.rooms().is_empty());
    assert!(lanes.pending(FIRST).is_empty());
    assert!(lanes.pending(SECOND).is_empty());
    lanes.finish();
}

fn read_awareness(lanes: &Lanes, design: &wire::TaskHandle, words: &[&str]) -> serde_json::Value {
    lanes.room_read(
        &["awareness", design.as_str()]
            .into_iter()
            .chain(words.iter().copied())
            .collect::<Vec<_>>(),
    )
}

fn awareness_rows(page: &serde_json::Value) -> Vec<serde_json::Value> {
    page["page"]["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| row["seq"].clone())
        .collect()
}

/// Awareness pages a Design's notices over their exact population, though
/// every record's envelope outweighs the row it renders: each first page
/// reports the whole population at any `--limit`, admits whole rows, and its
/// continuations visit every notice once in journal order.
#[test]
fn design_awareness_pages_heavy_notices_over_their_exact_population() {
    const NOTICES: usize = 34;
    let lanes = Lanes::new("design-awareness-paging");
    let prior = lanes.held_room("dVrD245Kc", FIRST);
    let design = lanes.seed_task(
        "dVrD245Kd",
        "Design",
        Some("Decide how awareness pages."),
        Some(wire::TaskIntent::Design),
        None,
    );
    let design_stamp = design.born().expect("fixture Design has a birth");
    let statement = lanes
        .bus
        .read({
            let design = design_stamp.clone();
            move |read| read.task_head(&design)
        })
        .expect("Design statement reads")
        .expect("Design statement exists")
        .body()
        .revision;
    // Rows show six distinct terms; the six long ones the record also keeps
    // make each envelope several times its row.
    let terms = ('a'..='f')
        .map(|letter| format!("a{letter}"))
        .chain(('a'..='f').map(|letter| format!("b{letter}{}", "x".repeat(400))))
        .collect::<Vec<_>>();
    let prior = prior.born().expect("fixture prior task has a birth");
    let mut noticed = Vec::with_capacity(NOTICES);
    for _ in 1..=NOTICES {
        let mut request = write(Actor::Spice, lanes.epoch, Producer::ResidentRecognition)
            .expect("notice write starts");
        request
            .fact(
                wire::rooms::DesignNoticed {
                    design: design_stamp.clone(),
                    statement,
                    lane: test_uuid(FIRST),
                    occupant: test_uuid(FIRST + 1),
                    session: wire::rooms::DesignSession {
                        vendor: "omp".parse().expect("vendor"),
                        identity: "fixture-session".to_owned(),
                    },
                    compaction: None,
                    opportunity: wire::rooms::DesignNoticeOpportunity::TaskCompletion {
                        task: prior.clone(),
                        completed: statement,
                    },
                    evidence: wire::rooms::DesignNoticeEvidence::Task {
                        task: prior.clone(),
                        statement,
                        completed: statement,
                        support: wire::rooms::DesignSupport::Implemented,
                    },
                    excerpt: "fixture notice".to_owned(),
                    terms: terms.clone(),
                    admission: fixture_admission(),
                    asks: Vec::new(),
                },
                FactCause::Root,
            )
            .expect("notice is valid");
        noticed.push(serde_json::json!(
            commit(&lanes.bus, request).expect("notice commits").first()
        ));
    }
    let read = |words: &[&str]| read_awareness(&lanes, &design, words);

    for limit in [
        &[][..],
        &["--limit", "1"],
        &["--limit", "5"],
        &["--limit", "50"],
    ] {
        let first = read(limit);
        let shown = awareness_rows(&first);
        assert!(!shown.is_empty(), "{limit:?} admits whole rows");
        assert_eq!(shown[..], noticed[..shown.len()], "{limit:?}");
        let omission = &first["page"]["omission"];
        if omission.is_null() {
            assert_eq!(shown.len(), NOTICES, "{limit:?} shows every notice");
        } else {
            assert_eq!(omission["population"], NOTICES, "{limit:?}");
            assert_eq!(omission["withheld"], NOTICES - shown.len(), "{limit:?}");
        }
    }

    let mut page = read(&["--limit", "5"]);
    let mut seen = awareness_rows(&page);
    while !page["page"]["omission"].is_null() {
        let resume =
            serde_json::from_value::<Vec<String>>(page["page"]["omission"]["continuation"].clone())
                .expect("a withheld notice names its continuation")
                .split_off(2);
        page = lanes.room_read(
            &["awareness"]
                .into_iter()
                .chain(resume.iter().map(String::as_str))
                .collect::<Vec<_>>(),
        );
        let next = awareness_rows(&page);
        assert!(!next.is_empty(), "each continuation admits a whole row");
        assert_eq!(
            page["page"]["omission"]["population"]
                .as_u64()
                .unwrap_or(next.len() as u64),
            (NOTICES - seen.len()) as u64,
            "a continuation counts exactly the notices from its cursor"
        );
        seen.extend(next);
    }
    assert_eq!(
        seen, noticed,
        "continuations visit every notice once in order"
    );
    lanes.finish();
}

/// Awareness answers only for a Design: an ordinary task refuses rather than
/// rendering a fabricated Design readout, while a Design still answers.
#[test]
fn design_awareness_refuses_a_task_that_is_not_a_design() {
    let lanes = Lanes::new("design-awareness-not-design");
    let ordinary = lanes.seed_task("dVrD245Kc", "Ordinary", None, None, None);
    let design = lanes.seed_task(
        "dVrD245Kd",
        "Design",
        Some("Decide something."),
        Some(wire::TaskIntent::Design),
        None,
    );
    let answer = |task: &wire::TaskHandle| {
        let matches = crate::cli::command()
            .try_get_matches_from(["spice", "room", "awareness", task.as_str()])
            .expect("room awareness parses");
        let contract = crate::cli::selected_contract(&matches).expect("awareness is registered");
        let journal =
            journal::JournalReader::open(&lanes.paths.journal).expect("the journal opens to read");
        crate::room_command::awareness::answer(
            &journal,
            crate::cli::selected_matches(&matches),
            contract.path,
        )
    };

    match answer(&ordinary) {
        Outcome::Refusal(refusal) => assert_eq!(refusal.reason(), "room::not-design"),
        other => panic!("an ordinary task did not refuse: {other:?}"),
    }
    assert!(
        matches!(answer(&design), Outcome::Reply(_)),
        "a Design still answers"
    );
    lanes.finish();
}

/// A transaction reaches only occupied lanes it leaves with unread activity:
/// a publication reaches its room's listeners, an unrelated task reaches no
/// one, and a vacant listener is never reached. Listening follows the claim
/// and the stop request that hold it.
#[test]
fn a_commit_reaches_only_occupied_lanes_with_unread_activity() {
    let lanes = Lanes::new("room-unread-reach");
    let first_room = lanes.held_room("dVrD245Kc", FIRST);
    let second_room = lanes.held_room("dVrD245Kd", SECOND);
    let (first_lane, second_lane) = (test_uuid(FIRST), test_uuid(SECOND));
    let mut seats = Seats::new();
    seat(&mut seats, FIRST);
    let mut diagnostics = Vec::new();
    let mut reached = |seats: &Seats, commit| {
        let changes = room_catch_up::changes(&lanes.bus, commit).expect("changes classify");
        room_catch_up::reached(&lanes.bus, seats, &changes, &mut diagnostics)
    };

    let subscriber = lanes.bus.subscribe().expect("commits subscribe");
    let _ = lanes.room("dVrD244Jd");
    assert!(
        reached(
            &seats,
            subscriber.recv().expect("the unrelated task commits")
        )
        .is_empty(),
        "an unrelated task reaches no lane"
    );
    assert_eq!(
        reached(&seats, append_room_publication(&lanes, &first_room)),
        [first_lane]
    );
    assert!(
        reached(&seats, append_room_publication(&lanes, &second_room)).is_empty(),
        "a vacant listener is never reached, and no other room's listener is"
    );
    seat(&mut seats, SECOND);
    assert_eq!(
        reached(&seats, append_room_publication(&lanes, &second_room)),
        [second_lane],
        "a publication reaches only its room's listeners"
    );

    free_claim(&lanes, &second_room, SECOND);
    assert_eq!(
        lanes.unread(SECOND),
        NOTHING_UNREAD,
        "a freed claim ends the holder's listening"
    );
    stop_room(&lanes, &first_room, FIRST);
    let stopped = lanes.unread(FIRST);
    assert!(
        stopped.contains("stop request"),
        "the page names the stop request it listens through: {stopped}"
    );
    assert!(
        diagnostics.is_empty(),
        "{}",
        String::from_utf8_lossy(&diagnostics)
    );
    seats.release_all();
    lanes.finish();
}

/// Release `lane`'s active claim on `room`'s task.
fn free_claim(lanes: &Lanes, room: &wire::TaskHandle, lane: u8) -> bus::JournalCommit {
    let task = room.born().expect("fixture room has a birth");
    let claim = active_claim(lanes, &task, lane);
    let mut free =
        write(claim.actor.clone(), lanes.epoch, Producer::CommandDoor).expect("free write starts");
    let freed = free
        .fact(
            wire::body::ClaimFreed {
                task: task.clone(),
                actor: claim.actor.clone(),
                parked: None,
            },
            FactCause::Existing(claim.claimed),
        )
        .expect("claim release fact is valid");
    free.claim_head(task, freed, move |position| wire::ClaimHead {
        fact: position,
        disposition: wire::ClaimDisposition::Freed,
        ..claim
    });
    commit(&lanes.bus, free).expect("claim release commits")
}

/// Ask `lane`, as a deferral does, to stop its claimed work in `room`.
fn stop_room(lanes: &Lanes, room: &wire::TaskHandle, lane: u8) -> bus::JournalCommit {
    let task = room.born().expect("fixture room has a birth");
    let claim = active_claim(lanes, &task, lane);
    let control = wire::rooms::StopControl {
        room: task.clone(),
        claim: wire::SequenceCode::new(claim.claimed.get()),
    }
    .to_string();
    let origin: wire::Origin = format!("task:{task}").parse().expect("task origin");
    let at = Instant::new(jiff::Timestamp::now()).expect("fixture instant");
    let mut stop = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin.clone(),
        at,
        Producer::CommandDoor,
    );
    crate::resident::append_operator_directive(
        &lanes.bus,
        origin,
        &steer::Config::default(),
        &mut stop,
        crate::resident::OperatorDirective {
            directive: test_uuid(90),
            fanout: None,
            lane: test_uuid(lane),
            sender: Actor::TEST_PRINCIPAL,
            text: "Checkpoint and free.",
            control: Some(&control),
            room: None,
        },
        at,
        FactCause::Root,
    )
    .expect("stop request stages");
    commit(&lanes.bus, stop).expect("stop request commits")
}

fn active_claim(lanes: &Lanes, task: &wire::TaskStamp, lane: u8) -> wire::ClaimHead {
    lanes
        .bus
        .task_snapshot()
        .expect("snapshot reads")
        .task_heads(task)
        .expect("task heads read")
        .claims()
        .iter()
        .map(|held| held.claim().body())
        .find(|claim| claim.lane == test_uuid(lane) && claim.is_active())
        .expect("active claim")
        .clone()
}

/// A page the lane reads advances its cursor in the live sweep that hands the
/// read's result over: that pass accounts the record before recognizing it.
#[test]
fn a_page_read_live_advances_the_cursor_as_its_result_is_swept() {
    let lanes = Lanes::new("room-unread-live");
    let room = lanes.held_room("dVrD245Kc", FIRST);
    let first_lane = test_uuid(FIRST);
    lanes.author(SENDER, &format!("SYN #{room}: retained"));
    lanes.account(SENDER);
    let _ = lanes.hear(SENDER, test_uuid(SENDER + 1));
    let page = lanes.unread(FIRST);
    let (through, _) = crate::room_command::unread::delivery(&page, first_lane)
        .expect("a shown page closes with its delivery line");
    let landed = lanes.tool_result(FIRST, &page);
    let mut seats = Seats::new();
    seat(&mut seats, FIRST);
    let source = message_source(lanes.bus.reader(), lanes.epoch);
    let publisher = source.message_publisher();
    let generation = publisher.batch().expect("message generation begins");
    let tastes = taste::Bag::new(Vec::new()).expect("empty taste bag");
    let steering = steer::Config::default();
    let surfaces = CaptureSurfaces::new(
        &lanes.paths,
        &tastes,
        &steering,
        &generation,
        lanes.joining.joining(),
    );
    let state = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());
    let mut diagnostics = Vec::new();
    accept_capture_record(
        &lanes.bus,
        lanes.epoch,
        &mut seats,
        &surfaces,
        crate::resident::message_surface::TasteRecognition::Direct,
        &landed,
        CaptureViews::new(&state, &mut diagnostics),
    )
    .expect("the record is accepted");
    let cursor = lanes
        .bus
        .read(move |read| read.room_cursor(first_lane))
        .expect("cursor reads")
        .map(|head| head.body().through.get());
    assert_eq!(
        cursor,
        Some(through.get()),
        "{}",
        String::from_utf8_lossy(&diagnostics)
    );
    seats.release_all();
    lanes.finish();
}
