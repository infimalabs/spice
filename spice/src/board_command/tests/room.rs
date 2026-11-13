use super::*;

use serde_json::Value;
use wire::intent::RoomLeave;
use wire::rooms::{
    ROOM_PUBLICATION_BYTES_LIMIT, RoomAuthorityStamp, RoomMessaged, RoomPost, RoomPostOutcome,
    RoomPublicationContent, RoomPublished, TranscriptHunk,
};

mod conversation;
pub(super) mod design_standing;

fn prove_seat_addresses(bus: &Bus, epoch: CanonicalUuid, seats: &[Seat]) {
    let origin = format!("resident:{epoch}")
        .parse()
        .expect("resident fixture origin");
    let at = Instant::new(jiff::Timestamp::now()).expect("fixture instant");
    let mut write = JournalWrite::new(Actor::Spice, origin, at, Producer::ResidentRecognition);
    for (group, seat) in seats.iter().enumerate() {
        write
            .fact(
                wire::body::LaneStarted {
                    lane: seat.lane,
                    occupant: seat.occupant,
                    epoch,
                    group: group as u32 + 1,
                    fresh_session: true,
                },
                FactCause::Root,
            )
            .expect("fixture occupant birth builds");
    }
    assert!(matches!(
        bus.commit(write).expect("fixture births reach writer"),
        CommitOutcome::Committed(_)
    ));
}

fn leave(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    room: &TaskHandle,
) -> Outcome<CommandAnswer> {
    let command = fixture_command(
        epoch,
        actor,
        CommandKind::RoomLeave,
        Vec::new(),
        &RoomLeave {
            room: room.clone(),
            clause: "my part is done".to_owned(),
        },
    );
    let wire::intent::Intent::RoomLeave(request) = command.body() else {
        panic!("fixture command carries room leave");
    };
    room_leave(bus, &command, request, None)
}

fn post_listeners(bus: &Bus, epoch: CanonicalUuid, room: &TaskHandle) -> Vec<CanonicalUuid> {
    let answer = assert_committed(
        dispatch(bus, &post_command(bus, epoch, room, "Recorded only."))
            .expect("room command dispatches"),
    );
    let CommandAnswer::RoomPost(answer) = answer else {
        panic!("room post returned another answer shape: {answer:?}");
    };
    match answer.outcome {
        RoomPostOutcome::RecordedWithListeners { listeners } => {
            listeners.iter().map(|listener| listener.lane).collect()
        }
        _ => Vec::new(),
    }
}

fn publication_page(bus: &Bus, room: &TaskHandle) -> journal::SelectedFactPage {
    let room = room.born().expect("fixture room has a birth stamp");
    bus.read(move |read| {
        read.room_publications(
            &room,
            None,
            journal::FactReadBudget::new(32, journal::MAX_RETAINED_FACT_PAGE_BYTES)
                .expect("fixture room page budget"),
        )
    })
    .expect("room publication page reads")
}

fn vacate(bus: &Bus, epoch: CanonicalUuid, seat: &Seat) {
    let observed = bus
        .task_snapshot()
        .expect("fixture residency snapshot reads")
        .residency()
        .expect("fixture residency exists")
        .clone();
    let mut vacant = observed.body().clone();
    let row = vacant
        .lanes
        .iter_mut()
        .find(|row| row.lane == seat.lane)
        .expect("fixture lane exists");
    row.occupant = None;
    row.disposition = "vacant".to_owned();
    let origin = format!("resident:{epoch}")
        .parse()
        .expect("resident origin");
    let at = Instant::new(jiff::Timestamp::now()).expect("fixture instant");
    let mut write = JournalWrite::new(Actor::Spice, origin, at, Producer::ResidentRecognition);
    let exited = write
        .fact(
            wire::body::LaneExited {
                lane: seat.lane,
                occupant: seat.occupant,
                code: 0,
            },
            FactCause::Existing(observed.position()),
        )
        .expect("fixture exit builds");
    write.residency_head(exited, move |_| vacant);
    assert!(matches!(
        bus.commit(write).expect("fixture exit reaches writer"),
        CommitOutcome::Committed(_)
    ));
}

fn post_command(
    bus: &Bus,
    epoch: CanonicalUuid,
    room: &TaskHandle,
    text: &str,
) -> wire::AdmittedCommand {
    observe_command(bus, epoch, Actor::TEST_PRINCIPAL, room, text)
}

fn observe_command(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    room: &TaskHandle,
    text: &str,
) -> wire::AdmittedCommand {
    let current = board(bus, epoch, actor.clone());
    let heads = at(&current, room);
    fixture_command(
        epoch,
        actor,
        CommandKind::RoomPost,
        vec![statement_witness(heads)],
        &RoomPost {
            room: room.clone(),
            text: text.to_owned(),
        },
    )
}

fn publish_packet(bus: &Bus, room: &TaskHandle, seat: &Seat) -> RoomAuthorityStamp {
    publish_message(bus, room, seat.lane, seat.occupant, 1_790_000_000_000_001)
}

/// Record one SYN packet `lane` authored in `room`, from its own ingested
/// transcript source. A packet, unlike an observation, makes its author a
/// participant.
fn publish_message(
    bus: &Bus,
    room: &TaskHandle,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    micros: i64,
) -> RoomAuthorityStamp {
    let vendor: wire::Name = "omp".parse().expect("fixture vendor");
    let source = uuid(micros.unsigned_abs() % 0x1_0000_0000_0000);
    let next = wire::body::TranscriptPosition {
        member: 1,
        offset: 64,
    };
    let head = wire::TranscriptSourceHead {
        lane,
        vendor: vendor.clone(),
        stream: "session".to_owned(),
        source,
        next,
    };
    let origin = format!("lane:{lane}").parse().expect("fixture lane origin");
    let at = Instant::new(jiff::Timestamp::now()).expect("fixture instant");
    let mut ingest = JournalWrite::new(Actor::Spice, origin, at, Producer::ResidentRecognition);
    let fact = ingest
        .fact(
            wire::body::JournalIngested {
                lane,
                vendor,
                stream: "session".to_owned(),
                source,
                supersedes: None,
                from: wire::body::TranscriptPosition {
                    member: 1,
                    offset: 0,
                },
                to: next,
                usage: Vec::new(),
                non_usage_records: 1,
                residue_records: 0,
            },
            FactCause::Root,
        )
        .expect("fixture transcript ingestion builds");
    ingest.transcript_source_head(fact, {
        let head = head.clone();
        move |_| head
    });
    assert!(matches!(
        bus.commit(ingest)
            .expect("fixture ingestion reaches writer"),
        CommitOutcome::Committed(_)
    ));

    let publication = RoomAuthorityStamp::for_micros(micros).expect("fixture packet identity");
    let mut packet = JournalWrite::new(
        Actor::Lane(lane),
        "task:room-history".parse().expect("fixture packet origin"),
        at,
        Producer::AckScanner,
    );
    packet
        .fact(
            RoomMessaged {
                room: room.born().expect("fixture room has a birth"),
                publication: publication.clone(),
                hunk: TranscriptHunk {
                    head,
                    locus: wire::MessageLocus {
                        member: 1,
                        offset: 0,
                        length: 64,
                        event: 0,
                    },
                    text: wire::rooms::TextRange { start: 0, end: 6 },
                },
                author: occupant,
                directed: Vec::new(),
            },
            FactCause::Root,
        )
        .expect("fixture room packet builds");
    assert!(matches!(
        bus.commit(packet).expect("fixture packet reaches writer"),
        CommitOutcome::Committed(_)
    ));
    publication
}

fn history_page(root: &FixtureRoot, words: &[&str]) -> Value {
    let matches = crate::cli::command()
        .try_get_matches_from(["spice", "room", "history"].iter().chain(words))
        .expect("room history parses");
    let contract = crate::cli::selected_contract(&matches).expect("room history is registered");
    let journal = journal::JournalReader::open(root.as_ref().join("authority.sqlite3"))
        .expect("fixture journal opens to read");
    let artifacts = artifact::Store::new(
        root.as_ref().join("artifacts"),
        root.as_ref().join("locks/task-artifacts.lock"),
    );
    let tape = root.as_ref().join("tape");
    let outcome = crate::room_command::answer(
        &journal,
        crate::room_command::SourceOwners {
            tape: &tape,
            artifacts: &artifacts,
            git: &root.as_ref().join("repo"),
        },
        crate::cli::selected_matches(&matches),
        contract.path,
    );
    let Outcome::Reply(reply) = outcome else {
        panic!("room history returned another outcome: {outcome:?}");
    };
    serde_json::to_value(reply.body).expect("room history encodes")
}

#[test]
fn room_post_records_once_and_retains_its_exact_retry_answer() {
    let (_root, bus, epoch, _seats) = fixture("room-post-retry", 0);
    let room = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "room-post-retry",
        "Room",
        None,
    );
    let command = post_command(&bus, epoch, &room, "Complete composer text.");
    let first = dispatch(&bus, &command).expect("room command dispatches");
    let answer = assert_committed(first.clone());
    let CommandAnswer::RoomPost(answer) = answer else {
        panic!("room post returned another answer shape: {answer:?}");
    };
    assert!(matches!(
        answer.outcome,
        RoomPostOutcome::RecordedNoCurrentListener { .. }
    ));

    let replay_command = command.clone();
    let replay = bus
        .read(move |read| read.command_replay(&replay_command))
        .expect("command retry receipt reads");
    assert_eq!(replay, Some(journal::CommandReplay::Exact(first)));

    let page = publication_page(&bus, &room);
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.continuation, None);
    assert_eq!(page.rows[0].actor, Actor::TEST_PRINCIPAL);
    let recorded = page.rows[0]
        .fact
        .read::<RoomPublished>()
        .expect("indexed publication has its typed body");
    assert_eq!(recorded.publication, answer.publication);
    assert_eq!(
        recorded.content,
        RoomPublicationContent::Authored {
            text: "Complete composer text.".to_owned()
        }
    );
}

#[test]
fn authored_observations_enforce_the_room_publication_byte_bound() {
    let (_root, bus, epoch, _seats) = fixture("room-post-byte-bound", 0);
    let room = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "room-post-byte-bound",
        "Room",
        None,
    );
    let boundary = format!(" {}\n", "x".repeat(ROOM_PUBLICATION_BYTES_LIMIT - 2));
    assert_committed(
        dispatch(&bus, &post_command(&bus, epoch, &room, &boundary))
            .expect("boundary observation dispatches"),
    );

    let oversized = format!("{boundary} ");
    let outcome = dispatch(&bus, &post_command(&bus, epoch, &room, &oversized))
        .expect("oversized observation dispatches");
    assert!(matches!(
        outcome,
        Outcome::Refusal(refusal) if refusal.reason() == "room::post-too-large"
    ));
    let blank = dispatch(&bus, &post_command(&bus, epoch, &room, " \n\t"))
        .expect("blank observation dispatches");
    assert!(matches!(
        blank,
        Outcome::Refusal(refusal) if refusal.reason() == "room::post-empty"
    ));

    let page = publication_page(&bus, &room);
    assert_eq!(page.rows.len(), 1);
    assert_eq!(
        page.rows[0]
            .fact
            .read::<RoomPublished>()
            .map(|publication| publication.content),
        Some(RoomPublicationContent::Authored { text: boundary })
    );
}

#[test]
fn a_lane_owing_no_design_request_cannot_leave_and_hears_which_claim_keeps_it_listening() {
    let (_root, bus, epoch, seats) = fixture("room-leave-native", 1);
    let lane = seats[0].lane;
    let room = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "room-leave-native",
        "Room",
        None,
    );
    super::resolution::claim_task(&bus, epoch, &seats[0], &Actor::Lane(lane), &room);

    let outcome = leave(&bus, epoch, Actor::Lane(lane), &room);
    let Outcome::Refusal(wire::Refusal::Bare { diagnostic }) = &outcome else {
        panic!("a leave owing nothing refuses: {outcome:?}");
    };
    assert_eq!(diagnostic.reason(), "room::leave-nothing-owed");
    assert!(
        diagnostic
            .observed()
            .is_some_and(|observed| observed.contains(&format!("work {room}"))),
        "the refusal names the claim that keeps the lane listening: {diagnostic:?}"
    );
    assert!(publication_page(&bus, &room).rows.is_empty());
    assert_eq!(post_listeners(&bus, epoch, &room), vec![lane]);
}

#[test]
fn only_a_lane_leaves_a_room() {
    let (_root, bus, epoch, _seats) = fixture("room-leave-operator", 0);
    let room = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "room-leave-operatr",
        "Room",
        None,
    );
    assert!(matches!(
        &leave(&bus, epoch, Actor::TEST_PRINCIPAL, &room),
        Outcome::Refusal(refusal) if refusal.reason() == "room::leave-lane-only"
    ));
}

#[test]
fn room_post_refuses_unknown_anchor_without_recording() {
    let (_root, bus, epoch, _seats) = fixture("room-post-unknown", 0);
    let unknown_stamp = stamp("room-post-unknown-anchor");
    let unknown = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &unknown_stamp,
    );
    let command = fixture_command(
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::RoomPost,
        vec![Witness::Task {
            task: unknown_stamp,
            head: Seq::new(1).expect("fixture position"),
        }],
        &RoomPost {
            room: unknown.clone(),
            text: "Do not record.".to_owned(),
        },
    );
    let outcome = dispatch(&bus, &command).expect("room command dispatches");
    assert!(matches!(
        outcome,
        Outcome::Refusal(refusal) if refusal.reason() == "room::anchor-unknown"
    ));
    assert!(publication_page(&bus, &unknown).rows.is_empty());
}

/// An observation reaches every room whose anchor is not deleted: a closed or
/// completed room takes one from the operator or a lane, and a deleted anchor
/// refuses naming its standing, records nothing, and keeps its history.
#[test]
fn observations_reach_every_room_except_a_deleted_anchor() {
    let (_root, bus, epoch, seats) = fixture("room-post-settled", 2);
    let closed = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "room-post-closed",
        "Closed",
        None,
    );
    let deleted = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "room-post-deleted",
        "Deleted",
        None,
    );
    let completed = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "room-post-completed",
        "Completed",
        Some("todo"),
    );
    for room in [&closed, &deleted, &completed] {
        assert_committed(
            dispatch(&bus, &post_command(&bus, epoch, room, "While open."))
                .expect("room command dispatches"),
        );
    }

    let heads = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let witnesses = |task: &TaskHandle| {
        let heads = at(&heads, task);
        transition_witnesses(heads)
    };
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskClose,
        witnesses(&closed),
        &TaskClose {
            task: closed.clone(),
            validation: "settled on the operator's word".to_owned(),
            force_claimed: false,
        },
    ));
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDelete,
        witnesses(&deleted),
        &TaskDelete {
            task: deleted.clone(),
            reason: "withdrawn".to_owned(),
            force_claimed: false,
        },
    ));
    complete_through_claim(&bus, epoch, &seats[0], &completed);

    let lane = Actor::Lane(seats[1].lane);
    for room in [&closed, &completed] {
        for author in [Actor::TEST_PRINCIPAL, lane.clone()] {
            assert_committed(
                dispatch(
                    &bus,
                    &observe_command(&bus, epoch, author, room, "After settlement."),
                )
                .expect("room command dispatches"),
            );
        }
        assert_eq!(publication_page(&bus, room).rows.len(), 3);
    }

    for author in [Actor::TEST_PRINCIPAL, lane] {
        let outcome = dispatch(
            &bus,
            &observe_command(&bus, epoch, author, &deleted, "After deletion."),
        )
        .expect("room command dispatches");
        let Outcome::Refusal(refusal) = outcome else {
            panic!("an observation on a deleted anchor recorded: {outcome:?}");
        };
        assert_eq!(refusal.reason(), "room::anchor-deleted");
        assert_eq!(
            refusal
                .diagnostic()
                .and_then(wire::RefusalDiagnostic::observed),
            Some(format!("{deleted} deleted").as_str())
        );
    }
    let page = publication_page(&bus, &deleted);
    assert_eq!(page.rows.len(), 1, "deleted room history changed");
    assert_eq!(
        page.rows[0]
            .fact
            .read::<RoomPublished>()
            .map(|row| row.content),
        Some(RoomPublicationContent::Authored {
            text: "While open.".to_owned()
        })
    );
}

/// A lane's observation reaches the task's holder as a current listener, but
/// its author gains no listening from writing it.
#[test]
fn a_lane_observation_reaches_the_holder_without_making_its_author_listen() {
    let (_root, bus, epoch, seats) = fixture("room-observation-listeners", 2);
    let room = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "room-observation-held",
        "Held",
        Some("todo"),
    );
    let holder = Actor::Lane(seats[0].lane);
    let available = board(&bus, epoch, holder.clone());
    assert_committed(issue(
        &bus,
        epoch,
        holder,
        CommandKind::ClaimWin,
        claim_expected(&available, &room, &seats[0]),
        &next_rule(),
    ));

    let author = Actor::Lane(seats[1].lane);
    for text in ["First observation.", "Second observation."] {
        let answer = assert_committed(
            dispatch(
                &bus,
                &observe_command(&bus, epoch, author.clone(), &room, text),
            )
            .expect("room command dispatches"),
        );
        let CommandAnswer::RoomPost(answer) = answer else {
            panic!("room post returned another answer shape: {answer:?}");
        };
        let RoomPostOutcome::RecordedWithListeners { listeners } = answer.outcome else {
            panic!("the holder was not reported as a listener");
        };
        assert_eq!(
            listeners
                .iter()
                .map(|listener| listener.lane)
                .collect::<Vec<_>>(),
            vec![seats[0].lane],
            "{text}"
        );
    }

    assert_eq!(publication_page(&bus, &room).rows.len(), 2);
}

/// Claim `task` from the allocator under `seat`'s lane and complete it.
fn complete_through_claim(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, task: &TaskHandle) {
    let lane = Actor::Lane(seat.lane);
    let available = board(bus, epoch, lane.clone());
    assert_committed(issue(
        bus,
        epoch,
        lane.clone(),
        CommandKind::ClaimWin,
        claim_expected(&available, task, seat),
        &next_rule(),
    ));
    let held = board(bus, epoch, lane.clone());
    assert_committed(issue(
        bus,
        epoch,
        lane,
        CommandKind::TaskDone,
        progress_witnesses(&held, at(&held, task), seat.lane, seat.occupant, false)
            .expect("held task has completion witnesses"),
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(task, "done")
        },
    ));
}

#[test]
fn room_history_pages_posts_and_packets_in_one_order() {
    let (root, bus, epoch, seats) = fixture("room-history-order", 1);
    prove_seat_addresses(&bus, epoch, &seats);
    let room = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "room-history-order",
        "Room history",
        None,
    );
    assert_committed(
        dispatch(&bus, &post_command(&bus, epoch, &room, "before")).expect("room post dispatches"),
    );
    publish_packet(&bus, &room, &seats[0]);
    assert_committed(
        dispatch(&bus, &post_command(&bus, epoch, &room, "after")).expect("room post dispatches"),
    );
    publish_message(
        &bus,
        &room,
        seats[0].lane,
        seats[0].occupant,
        1_790_000_000_000_002,
    );

    let first = history_page(&root, &[room.as_str(), "--limit", "3"]);
    assert_eq!(first["page"]["omission"]["population"], 4);
    assert_eq!(first["page"]["omission"]["shown"], 3);
    assert_eq!(first["page"]["omission"]["withheld"], 1);
    let first_rows = first["page"]["rows"].as_array().expect("first page rows");
    assert_eq!(
        first_rows
            .iter()
            .map(|row| row["content"]["state"].as_str())
            .collect::<Vec<_>>(),
        [Some("text"), Some("unavailable"), Some("text")]
    );
    let continuation: Vec<String> =
        serde_json::from_value(first["page"]["omission"]["continuation"].clone())
            .expect("continuation is one command");
    let words = continuation[2..]
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let second = history_page(&root, &words);
    assert!(second["page"]["omission"].is_null());
    let second_rows = second["page"]["rows"].as_array().expect("second page rows");
    assert_eq!(
        second_rows
            .iter()
            .map(|row| row["content"]["state"].as_str())
            .collect::<Vec<_>>(),
        [Some("unavailable")]
    );

    let shown = first_rows
        .iter()
        .chain(second_rows)
        .map(|row| row["seq"].clone())
        .collect::<Vec<_>>();
    let indexed = publication_page(&bus, &room)
        .rows
        .into_iter()
        .map(|row| serde_json::to_value(row.seq).expect("sequence encodes"))
        .collect::<Vec<_>>();
    assert_eq!(shown, indexed, "history and publication index order agree");
}
