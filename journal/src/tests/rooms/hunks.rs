//! Room authority linked to exact transcript hunks: packets recorded in rooms
//! and per-lane catch-up cursors.

use wire::body::{SteerMessaged, SteeringStage, TranscriptLocus};
use wire::rooms::{RoomDelivered, RoomMessaged};
use wire::{Disposition, SteeringHead};

use super::*;

fn deliver(journal: &mut Journal, head: &TranscriptSourceHead, through: Seq) -> Commit {
    let mut write = JournalWrite::new(
        Actor::Spice,
        format!("lane:{}", head.lane)
            .parse()
            .expect("resident origin"),
        instant(),
        Producer::ResidentRecognition,
    );
    write
        .fact(
            RoomDelivered {
                through: SequenceCode::new(through.get()),
                hunk: hunk(head),
            },
            FactCause::Root,
        )
        .expect("delivery builds");
    journal.apply(write)
}

fn room_cursor(journal: &Journal, lane: CanonicalUuid) -> Option<(Seq, u64)> {
    journal
        .read(|read| read.room_cursor(lane))
        .expect("room cursor reads")
        .map(|head| (head.position(), head.body().through.get()))
}

fn published(journal: &mut Journal, room: &TaskStamp, micros: i64) -> Seq {
    commit(
        journal,
        publication(
            room,
            micros,
            RoomPublicationContent::Authored {
                text: format!("post {micros}"),
            },
        ),
    )
    .expect("publication commits");
    cursor(journal).expect("publication is the head")
}

/// A landed delivery strictly advances its own logical lane's cursor to an
/// earlier room publication; a renewed generation of that lane continues the
/// same cursor, another lane keeps its own, and replay rebuilds them all.
#[test]
fn landed_deliveries_advance_one_monotone_cursor_per_logical_lane() {
    let mut journal = Journal::in_memory().expect("journal");
    let room = task(&mut journal, 1, None);
    let origin = TranscriptPosition {
        member: 1,
        offset: 0,
    };
    let head = transcript_source_head();
    ingest(&mut journal, head.clone(), origin, None);
    let mut other = head.clone();
    other.lane = uuid("018f82d4-6f59-7d06-92e3-bce913116306");
    ingest(&mut journal, other.clone(), origin, None);
    let first = published(&mut journal, &room, 1);
    let second = published(&mut journal, &room, 2);
    let third = published(&mut journal, &room, 3);
    assert_eq!(room_cursor(&journal, head.lane), None, "no delivery landed");

    deliver(&mut journal, &head, first).expect("the first delivery lands");
    let landed = cursor(&journal).expect("delivery is the head");
    assert_eq!(
        room_cursor(&journal, head.lane),
        Some((landed, first.get()))
    );

    let mut unaccounted = head.clone();
    unaccounted.source = uuid("018f82d4-6f59-7d06-92e3-bce913116399");
    let task_fact = seq(1);
    let future = Seq::new(landed.get() + 1).expect("future position");
    for (from, through) in [
        (&head, first),
        (&head, task_fact),
        (&head, future),
        (&unaccounted, second),
    ] {
        let result = deliver(&mut journal, from, through);
        refused(&journal, result);
    }

    let mut renewed = head.clone();
    renewed.source = uuid("018f82d4-6f59-7d06-92e3-bce913116398");
    ingest(&mut journal, renewed.clone(), origin, Some(head.source));
    deliver(&mut journal, &renewed, second).expect("the renewed lane continues its cursor");
    let continued = cursor(&journal).expect("delivery is the head");
    let result = deliver(&mut journal, &renewed, first);
    refused(&journal, result);
    deliver(&mut journal, &other, third).expect("another lane keeps its own cursor");
    assert_eq!(
        room_cursor(&journal, head.lane),
        Some((continued, second.get()))
    );
    assert_eq!(
        room_cursor(&journal, other.lane).map(|(_, through)| through),
        Some(third.get())
    );

    let expected = projection(&journal);
    journal.verify().expect("cursors verify");
    journal.reindex().expect("cursors rebuild");
    assert_eq!(projection(&journal), expected);
}

/// A packet's room record links only its exact decoded-event bytes inside the
/// accounted transcript, and pages beside operator publications in its room.
#[test]
fn messaged_packets_record_exact_transcript_hunks_beside_room_publications() {
    let mut journal = Journal::in_memory().expect("journal");
    let room = task(&mut journal, 1, None);
    let head = transcript_source_head();
    ingest(
        &mut journal,
        head.clone(),
        TranscriptPosition {
            member: 1,
            offset: 0,
        },
        None,
    );
    let messaged = |journal: &mut Journal, room: &TaskStamp, micros, event, start, end| {
        let mut write = JournalWrite::new(
            Actor::Lane(head.lane),
            "task:rooms".parse().expect("room origin"),
            instant(),
            Producer::AckScanner,
        );
        write
            .fact(
                RoomMessaged {
                    room: room.clone(),
                    publication: authority(micros),
                    hunk: TranscriptHunk {
                        head: head.clone(),
                        locus: wire::MessageLocus {
                            member: 1,
                            offset: 0,
                            length: 64,
                            event,
                        },
                        text: TextRange { start, end },
                    },
                    author: head.lane,
                    directed: Vec::new(),
                },
                FactCause::Root,
            )
            .expect("messaged fact builds");
        journal.apply(write)
    };

    commit(
        &mut journal,
        publication(
            &room,
            30,
            RoomPublicationContent::Authored {
                text: "operator".to_owned(),
            },
        ),
    )
    .expect("operator publication");
    messaged(&mut journal, &room, 31, 2, 4, 9).expect("the packet hunk records");
    let absent = TaskStamp::for_micros(99_999).expect("absent stamp");
    for result in [
        messaged(&mut journal, &room, 31, 2, 4, 9),
        messaged(&mut journal, &room, 32, 2, 9, 9),
        messaged(&mut journal, &room, 33, u32::MAX, 0, 1),
        messaged(&mut journal, &absent, 34, 2, 4, 9),
    ] {
        refused(&journal, result);
    }

    let page = journal
        .read(|read| read.room_publications(&room, None, fact_budget(8)))
        .expect("room publications read");
    assert_eq!(
        page.rows
            .iter()
            .map(|row| row.fact.kind())
            .collect::<Vec<_>>(),
        [FactKind::RoomPublished, FactKind::RoomMessaged]
    );
    journal.verify().expect("messaged publications verify");
}

/// One packet's write: the peer messages its directed routes open, then its
/// record in `room`.
struct Packet {
    writer: Actor,
    messages: Vec<SteerMessaged>,
    author: CanonicalUuid,
    directed: Vec<CanonicalUuid>,
}

fn peer_message(head: &TranscriptSourceHead, ordinal: u8, room: &TaskStamp) -> SteerMessaged {
    SteerMessaged {
        directive: uuid(&format!("018f82d4-6f59-7d06-92e3-bce9131164{ordinal:02}")),
        key: TaskStamp::for_micros(1_700_000_000_000_000 + i64::from(ordinal))
            .expect("fixture steering key")
            .to_string(),
        lane: uuid("018f82d4-6f59-7d06-92e3-bce913116307"),
        occupant: uuid("018f82d4-6f59-7d06-92e3-bce913116308"),
        from: head.lane,
        sender: uuid(AUTHOR),
        text: "for the room".to_owned(),
        locus: TranscriptLocus {
            source: Some(head.source),
            member: 1,
            offset: 0,
            length: 64,
            event: 0,
            line: 1,
            column: 1,
        },
        span: wire::body::BodySpan::new(0.."for the room".len(), "for the room"),
        room: Some(room.clone()),
        addressed: None,
        line: None,
        peer_body: None,
        covered: Vec::new(),
        admission: None,
        expires: "2030-04-12T19:44:09.271Z".parse().expect("fixture expiry"),
    }
}

const AUTHOR: &str = "018f82d4-6f59-7d06-92e3-bce913116309";

fn record_packet(
    journal: &mut Journal,
    head: &TranscriptSourceHead,
    room: &TaskStamp,
    micros: i64,
    packet: Packet,
) -> Commit {
    let origin: wire::Origin = "resident:018f82d4-6f59-7d06-92e3-bce913116305"
        .parse()
        .expect("resident origin");
    let at = instant();
    let mut write = JournalWrite::new(packet.writer, origin.clone(), at, Producer::AckScanner);
    for message in packet.messages {
        let minted_micros = message
            .key
            .parse::<wire::TaskStamp>()
            .expect("current steering key")
            .micros();
        let created = write
            .fact(message.clone(), FactCause::Root)
            .expect("peer message builds");
        let origin = origin.clone();
        write.steering_head(created, move |created| SteeringHead {
            directive: message.directive,
            fanout: None,
            recipients: Vec::new(),
            key: message.key,
            recipient: wire::Actor::Lane(message.lane),
            text: message.text,
            sender: wire::Actor::Lane(message.from),
            room: message.room,
            control: None,
            attachments: Vec::new(),
            published: at,
            expires: message.expires,
            stage: SteeringStage::Published,
            escalations: Vec::new(),
            disposition: Disposition::Live,
            origin,
            created,
            minted_micros,
        });
    }
    write
        .fact(
            RoomMessaged {
                room: room.clone(),
                publication: authority(micros),
                hunk: hunk(head),
                author: packet.author,
                directed: packet.directed,
            },
            FactCause::Root,
        )
        .expect("room packet builds");
    journal.apply(write)
}

/// A packet's room record is written by its hunk's lane and names exactly the
/// directed routes its own write opened into that room, each sent from that
/// lane by its author at its record; replay rebuilds what was admitted.
#[test]
fn a_room_packet_names_exactly_the_routes_its_write_opened_there() {
    let mut journal = Journal::in_memory().expect("journal");
    let room = task(&mut journal, 1, None);
    let other_room = task(&mut journal, 2, None);
    let head = transcript_source_head();
    ingest(
        &mut journal,
        head.clone(),
        TranscriptPosition {
            member: 1,
            offset: 0,
        },
        None,
    );
    let lane = Actor::Lane(head.lane);
    let author = uuid(AUTHOR);
    let packet = |messages: Vec<SteerMessaged>, directed: Vec<CanonicalUuid>| Packet {
        writer: lane.clone(),
        messages,
        author,
        directed,
    };
    let directives = |messages: &[SteerMessaged]| {
        messages
            .iter()
            .map(|message| message.directive)
            .collect::<Vec<_>>()
    };

    let routes = [peer_message(&head, 1, &room), peer_message(&head, 2, &room)];
    let elsewhere = peer_message(&head, 3, &other_room);
    let mut messages = routes.to_vec();
    messages.push(elsewhere.clone());
    record_packet(
        &mut journal,
        &head,
        &room,
        1,
        packet(messages, directives(&routes)),
    )
    .expect("a packet names its routes into the room in write order");
    record_packet(
        &mut journal,
        &head,
        &room,
        2,
        packet(Vec::new(), Vec::new()),
    )
    .expect("a room-only packet names no route");

    let fresh = [peer_message(&head, 4, &room), peer_message(&head, 5, &room)];
    let mut reordered = directives(&fresh);
    reordered.reverse();
    let mut from_elsewhere = peer_message(&head, 6, &room);
    from_elsewhere.from = uuid("018f82d4-6f59-7d06-92e3-bce913116306");
    let mut other_sender = peer_message(&head, 7, &room);
    other_sender.sender = uuid("018f82d4-6f59-7d06-92e3-bce913116310");
    let mut other_record = peer_message(&head, 8, &room);
    other_record.locus.offset = 64;
    let mut other_generation = peer_message(&head, 9, &room);
    other_generation.locus.source = Some(uuid("018f82d4-6f59-7d06-92e3-bce913116311"));
    let unrouted = peer_message(&head, 10, &other_room);
    let mut foreign_writer = packet(Vec::new(), Vec::new());
    foreign_writer.writer = Actor::Lane(uuid("018f82d4-6f59-7d06-92e3-bce913116306"));
    for (micros, refused_packet) in (10..).zip([
        foreign_writer,
        packet(fresh.to_vec(), Vec::new()),
        packet(Vec::new(), directives(&fresh)),
        packet(fresh.to_vec(), reordered),
        packet(fresh[..1].to_vec(), directives(&fresh)),
        packet(vec![unrouted.clone()], directives(&[unrouted])),
        packet(vec![from_elsewhere.clone()], directives(&[from_elsewhere])),
        packet(vec![other_sender.clone()], directives(&[other_sender])),
        packet(vec![other_record.clone()], directives(&[other_record])),
        packet(
            vec![other_generation.clone()],
            directives(&[other_generation]),
        ),
    ]) {
        let result = record_packet(&mut journal, &head, &room, micros, refused_packet);
        let Err(CommitError::Refused(refusal)) = &result else {
            panic!("the packet record was admitted: {result:?}");
        };
        let reason = refusal.to_string();
        assert!(
            reason.contains("room packet") || reason.contains("not sent from this packet's lane"),
            "refused for another reason: {reason}"
        );
        refused(&journal, result);
    }

    let page = journal
        .read(|read| read.room_publications(&room, None, fact_budget(8)))
        .expect("room publications read");
    assert_eq!(
        page.rows
            .iter()
            .filter_map(|row| row.fact.read::<RoomMessaged>())
            .map(|packet| packet.directed)
            .collect::<Vec<_>>(),
        [directives(&routes), Vec::new()]
    );
    journal.verify().expect("admitted packets verify");
}
