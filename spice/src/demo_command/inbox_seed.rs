use journal::{FactCause, JournalWrite};
use wire::{Actor, Producer, body};

use super::{
    DEMO_CLAIM_EXPIRY, DEMO_EPOCH, DEMO_FIRST_CONCERN, DEMO_FIRST_CONCERN_IMAGE,
    DEMO_FIRST_CONCERN_KEY, DEMO_FIRST_CONCERN_TEXT, DEMO_FIRST_PRINCIPAL_KEY,
    DEMO_INNER_DIRECTIVE, DEMO_INNER_KEY, DEMO_INNER_TEXT, DEMO_MESSAGE_LANE,
    DEMO_MESSAGE_OCCUPANT, DEMO_OUTER_DIRECTIVE, DEMO_OUTER_KEY, DEMO_PRINCIPAL, DEMO_RESIDENT_AT,
    DEMO_SECOND_CONCERN, DEMO_SECOND_CONCERN_KEY, DEMO_SECOND_CONCERN_TEXT,
    DEMO_SECOND_PRINCIPAL_KEY, DEMO_STAMP, DemoMessageLane, DemoResult, build_failure,
    commit_failure, environment_fault, instant, uuid,
};

pub(super) fn seed_demo_directions(journal: &mut journal::Journal) -> DemoResult<()> {
    let outer = demo_outer_text();
    let image = wire::Media::from_base64("rows.png", "image/png", DEMO_FIRST_CONCERN_IMAGE)
        .expect("the fixed demo image admits");
    let first = format!(
        "{DEMO_FIRST_CONCERN_TEXT}\n\n{}",
        image
            .reference_markdown()
            .expect("the fixed demo image has a reference")
    );
    for (directive, key, text, attachments) in [
        (
            DEMO_INNER_DIRECTIVE,
            DEMO_INNER_KEY,
            DEMO_INNER_TEXT,
            Vec::new(),
        ),
        (
            DEMO_OUTER_DIRECTIVE,
            DEMO_OUTER_KEY,
            outer.as_str(),
            Vec::new(),
        ),
        (
            DEMO_FIRST_CONCERN,
            DEMO_FIRST_CONCERN_KEY,
            first.as_str(),
            vec![image],
        ),
        (
            DEMO_SECOND_CONCERN,
            DEMO_SECOND_CONCERN_KEY,
            DEMO_SECOND_CONCERN_TEXT,
            Vec::new(),
        ),
    ] {
        seed_demo_direction(journal, directive, key, text, attachments)?;
    }
    Ok(())
}

fn seed_demo_direction(
    journal: &mut journal::Journal,
    directive: &str,
    key: &str,
    text: &str,
    attachments: Vec<wire::Media>,
) -> DemoResult<()> {
    let at = instant(DEMO_RESIDENT_AT);
    let origin: wire::Origin = format!("resident:{}", uuid(DEMO_EPOCH))
        .parse()
        .expect("demo direction origin is valid");
    let body = body::SteerSent {
        sender: Actor::Principal(uuid(DEMO_PRINCIPAL)),
        room: None,
        directive: uuid(directive),
        fanout: Some(uuid(directive)),
        recipients: Vec::new(),
        key: key.to_owned(),
        recipient: Actor::Lane(uuid(DEMO_MESSAGE_LANE)),
        text: text.to_owned(),
        control: None,
        attachments,
        expires: instant(DEMO_CLAIM_EXPIRY),
    };
    let mut write = JournalWrite::new(
        Actor::Principal(uuid(DEMO_PRINCIPAL)),
        origin.clone(),
        at,
        Producer::CommandDoor,
    );
    let sent = write
        .fact(body.clone(), FactCause::Root)
        .map_err(build_failure)?;
    write.steering_head(sent, move |created| {
        steer::Ledger::sent_head(&body, origin, at, created)
            .expect("the fixed demo direction is valid")
    });
    journal.apply(write).map_err(commit_failure)?;
    Ok(())
}

/// Vacate one transcript seat after its occupant's last record, as the
/// occupant's exit would.
pub(super) fn vacate_seat(journal: &mut journal::Journal, seat: DemoMessageLane) -> DemoResult<()> {
    let current = journal
        .read(|read| read.residency_head())
        .map_err(|error| environment_fault("demo::journal-invalid", error.to_string()))?
        .expect("demo lane seeding publishes residency");
    let mut residency = current.body().clone();
    let row = residency
        .lanes
        .iter_mut()
        .find(|row| row.lane == uuid(seat.lane))
        .expect("the demo transcript seat is seated");
    row.occupant = None;
    row.disposition = "vacant".to_owned();
    let epoch = uuid(DEMO_EPOCH);
    let mut write = JournalWrite::new(
        Actor::Spice,
        format!("resident:{epoch}")
            .parse()
            .expect("demo resident origin is valid"),
        instant(DEMO_RESIDENT_AT),
        Producer::ResidentRecognition,
    );
    let exited = write
        .fact(
            body::LaneExited {
                lane: uuid(seat.lane),
                occupant: uuid(seat.occupant),
                code: 0,
            },
            FactCause::Existing(current.position()),
        )
        .map_err(build_failure)?;
    write.residency_head(exited, move |_| residency);
    journal.apply(write).map_err(commit_failure)?;
    Ok(())
}

fn demo_outer_text() -> String {
    let inner = wire::QuoteTarget::Steering {
        key: DEMO_INNER_KEY.to_owned(),
        digest: wire::QuoteTarget::text_digest(DEMO_INNER_TEXT),
    };
    format!(
        "The status contract rests on this earlier operator direction:\n\n{}",
        inner.markdown()
    )
}

fn demo_message_text() -> String {
    let outer_text = demo_outer_text();
    let outer = wire::QuoteTarget::Steering {
        key: DEMO_OUTER_KEY.to_owned(),
        digest: wire::QuoteTarget::text_digest(&outer_text),
    }
    .markdown();
    format!(
        "Two operator concerns carry the same quoted status context.\n\n\
         SYN #{DEMO_FIRST_CONCERN_KEY}: Keep one row per lane.\n\n{outer}\n\n\
         SYN #{DEMO_SECOND_CONCERN_KEY}: Preserve diagnostics for failed lanes.\n\n{outer}"
    )
}

pub(super) fn seed_message_tape(state: &spice::repository::StatePaths) -> DemoResult<()> {
    let tape = tape::Tape::open(state.lane_tape(uuid(DEMO_MESSAGE_LANE)).join("omp"))
        .map_err(|error| environment_fault("demo::transcript-unwritable", error.to_string()))?;
    let text = demo_message_text();
    let settled = serde_json::json!({
        "timestamp": "2099-07-11T05:04:00.000Z",
        "type": "message_end",
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
        },
    });
    tape.tee(format!("{settled}\n").as_bytes())
        .map_err(|error| environment_fault("demo::transcript-unwritable", error.to_string()))?;
    Ok(())
}

fn ingest_message_transcript(
    state: &spice::repository::StatePaths,
    journal: &mut journal::Journal,
) -> DemoResult<()> {
    let ingestion = lane::ingest::Ingestion {
        lane: uuid(DEMO_MESSAGE_LANE),
        vendor: "omp".parse().expect("OMP demo vendor is valid"),
        stream: wire::body::TASK_CREATION_CONTEXT_STREAM.to_owned(),
        root: state.lane_tape(uuid(DEMO_MESSAGE_LANE)).join("omp"),
        dialect: listen::Dialect::Omp,
        actor: Actor::Spice,
        origin: format!("lane:{DEMO_MESSAGE_LANE}")
            .parse()
            .expect("demo transcript origin is valid"),
        at: instant("2026-07-11T05:04:00.000000Z"),
        occupancy: None,
    };
    match lane::ingest::ingest_bounded(
        journal,
        ingestion,
        &mut lane::ingest::Coalescing::new(),
        lane::ingest::Bound::IMMEDIATE,
    )
    .map_err(|error| environment_fault("demo::transcript-invalid", error.to_string()))?
    {
        lane::ingest::Ingested::Accounted { .. } => Ok(()),
        lane::ingest::Ingested::Unchanged | lane::ingest::Ingested::Deferred { .. } => {
            Err(environment_fault(
                "demo::transcript-invalid",
                "the fixed demo transcript did not produce an accounted batch",
            )
            .into())
        }
    }
}

fn stage_inbox_conversation(
    write: &mut JournalWrite,
    residency: wire::Seq,
    room: &wire::rooms::RoomAnchor,
    publication: &wire::rooms::RoomAuthorityStamp,
    hunk: wire::rooms::TranscriptHunk,
    principal: wire::CanonicalUuid,
    route_key: &str,
) -> DemoResult<journal::PendingFact> {
    write
        .fact(
            wire::rooms::RoomMessaged {
                room: room.clone(),
                publication: publication.clone(),
                hunk: hunk.clone(),
                author: uuid(DEMO_MESSAGE_OCCUPANT),
                directed: Vec::new(),
            },
            FactCause::Existing(residency),
        )
        .map_err(build_failure)?;
    Ok(write
        .fact(
            wire::rooms::RoomConversed {
                publication: publication.clone(),
                hunk,
                operation: wire::rooms::RoomPublicationKind::Message {},
                routes: vec![wire::rooms::RoomRoute {
                    destination: wire::rooms::RoomDestination::Principal { principal },
                    context: Some(room.clone()),
                    direction: Some(route_key.to_owned()),
                }],
                directed: Vec::new(),
                expires: instant(DEMO_CLAIM_EXPIRY),
            },
            FactCause::Existing(residency),
        )
        .map_err(build_failure)?)
}

pub(super) fn seed_inbox_conversations(state: &spice::repository::StatePaths) -> DemoResult<()> {
    let (head, locus, ranges) = message_source(state)?;
    let mut journal = journal::Journal::open(state.journal())
        .map_err(|error| environment_fault("demo::journal-unavailable", error.to_string()))?;
    ingest_message_transcript(state, &mut journal)?;
    let residency = journal
        .read(|read| read.residency_head())
        .map_err(|error| environment_fault("demo::journal-invalid", error.to_string()))?
        .expect("demo lane seeding publishes residency");
    let at = instant("2026-07-11T05:04:00.000000Z");
    let origin: wire::Origin = format!("resident:{}", uuid(DEMO_EPOCH))
        .parse()
        .expect("demo resident origin is valid");
    let mut write = JournalWrite::new(
        Actor::Lane(uuid(DEMO_MESSAGE_LANE)),
        origin.clone(),
        at,
        Producer::AckScanner,
    );
    let room: wire::rooms::RoomAnchor = DEMO_STAMP.parse().expect("the demo task names a room");
    let principal = uuid(DEMO_PRINCIPAL);
    for (offset, (route_key, input_key, range, text)) in [
        (
            DEMO_FIRST_CONCERN_KEY,
            DEMO_FIRST_PRINCIPAL_KEY,
            ranges[0],
            "Keep one row per lane.",
        ),
        (
            DEMO_SECOND_CONCERN_KEY,
            DEMO_SECOND_PRINCIPAL_KEY,
            ranges[1],
            "Preserve diagnostics for failed lanes.",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let micros = at
            .get()
            .as_microsecond()
            .saturating_add(i64::try_from(offset).expect("two demo items fit"));
        let publication = wire::rooms::RoomAuthorityStamp::for_micros(micros)
            .expect("demo conversation instant names a publication");
        let hunk = wire::rooms::TranscriptHunk {
            head: head.clone(),
            locus,
            text: range,
        };
        let conversed = stage_inbox_conversation(
            &mut write,
            residency.position(),
            &room,
            &publication,
            hunk,
            principal,
            route_key,
        )?;
        let directive =
            wire::rooms::PrincipalInputRef::published(&publication, principal).directive;
        let body = body::SteerSent {
            directive,
            fanout: None,
            key: input_key.to_owned(),
            recipient: Actor::Principal(principal),
            text: text.to_owned(),
            sender: Actor::Lane(uuid(DEMO_MESSAGE_LANE)),
            room: Some(room.clone()),
            control: Some(wire::peer_line_control(
                body::PeerLineRef {
                    root: directive,
                    parent: None,
                    opening: directive,
                },
                body::PeerBodyKind::Open,
                &[],
            )),
            attachments: Vec::new(),
            recipients: Vec::new(),
            expires: instant(DEMO_CLAIM_EXPIRY),
        };
        let sent = write
            .fact(body.clone(), FactCause::Pending(conversed))
            .map_err(build_failure)?;
        let (head_origin, head_at) = (origin.clone(), at);
        write.steering_head(sent, move |created| {
            steer::Ledger::sent_head(&body, head_origin, head_at, created)
                .expect("the fixed principal input is valid")
        });
    }
    journal.apply(write).map_err(commit_failure)?;
    let second_publication =
        wire::rooms::RoomAuthorityStamp::for_micros(at.get().as_microsecond().saturating_add(1))
            .expect("the second demo conversation instant names a publication");
    lapse_principal_input(
        &mut journal,
        wire::rooms::PrincipalInputRef::published(&second_publication, uuid(DEMO_PRINCIPAL))
            .directive,
    )?;
    Ok(())
}

fn lapse_principal_input(
    journal: &mut journal::Journal,
    directive: wire::CanonicalUuid,
) -> DemoResult<()> {
    let current = journal
        .read(|read| read.steering_head(directive))
        .map_err(|error| environment_fault("demo::journal-invalid", error.to_string()))?
        .expect("the fixed principal input has a steering head");
    let at = instant(DEMO_CLAIM_EXPIRY);
    let body = body::SteerLapsed {
        key: current.body().key.clone(),
        recipient: current.body().recipient.clone(),
        expires: current.body().expires,
    };
    let next = steer::Ledger::transition_head(current.body(), steer::Steering::Lapsed(&body), at)
        .expect("the fixed principal input can lapse");
    let mut write = JournalWrite::new(
        Actor::Spice,
        current.body().origin.clone(),
        at,
        Producer::BusTimer,
    );
    let lapsed = write
        .fact(body, FactCause::Existing(current.body().created))
        .map_err(build_failure)?;
    write
        .fact(
            body::SteerLineTransition {
                line: directive,
                input: directive,
                action: body::PeerLineAction::Unanswered,
                body: None,
            },
            FactCause::Pending(lapsed),
        )
        .map_err(build_failure)?;
    write.steering_head(lapsed, move |_| next);
    journal.apply(write).map_err(commit_failure)?;
    Ok(())
}

fn message_source(
    state: &spice::repository::StatePaths,
) -> DemoResult<(
    wire::TranscriptSourceHead,
    wire::MessageLocus,
    [wire::rooms::TextRange; 2],
)> {
    let scan = listen::scan(
        state.lane_tape(uuid(DEMO_MESSAGE_LANE)).join("omp"),
        listen::Dialect::Omp,
        None,
    )
    .map_err(|error| environment_fault("demo::transcript-invalid", error.to_string()))?;
    let (ordinal, locus, text) = scan
        .records
        .iter()
        .filter_map(|record| match &record.payload {
            listen::RecordPayload::Events(events) => Some(events),
            listen::RecordPayload::Residue(_) => None,
        })
        .flatten()
        .find_map(|event| match &event.event {
            listen::Event::Prose(prose) if prose.role == listen::ProseRole::Assistant => {
                Some((event.ordinal, prose.locus, prose.text.as_str()))
            }
            _ => None,
        })
        .expect("the fixed demo tape carries settled assistant prose");
    let source = scan
        .checkpoint
        .source
        .as_str()
        .parse()
        .expect("tape source identities are UUIDs");
    let head = wire::TranscriptSourceHead {
        lane: uuid(DEMO_MESSAGE_LANE),
        vendor: "omp".parse().expect("OMP demo vendor is valid"),
        stream: wire::body::TASK_CREATION_CONTEXT_STREAM.to_owned(),
        source,
        next: wire::body::TranscriptPosition {
            member: scan.checkpoint.next.member,
            offset: scan.checkpoint.next.offset,
        },
    };
    let locus = wire::MessageLocus {
        member: locus.member,
        offset: locus.offset,
        length: locus.length,
        event: u32::try_from(ordinal).expect("the demo event ordinal fits"),
    };
    Ok((head, locus, demo_message_ranges(text)))
}

fn demo_message_ranges(text: &str) -> [wire::rooms::TextRange; 2] {
    let first_header = format!("SYN #{DEMO_FIRST_CONCERN_KEY}: ");
    let second_header = format!("SYN #{DEMO_SECOND_CONCERN_KEY}: ");
    let first_start = text
        .find(&first_header)
        .expect("the fixed demo message has its first packet")
        + first_header.len();
    let second_header_start = text
        .find(&second_header)
        .expect("the fixed demo message has its second packet");
    let first_end = text[..second_header_start].trim_end().len();
    let second_start = second_header_start + second_header.len();
    [
        wire::rooms::TextRange {
            start: u64::try_from(first_start).expect("demo message range fits"),
            end: u64::try_from(first_end).expect("demo message range fits"),
        },
        wire::rooms::TextRange {
            start: u64::try_from(second_start).expect("demo message range fits"),
            end: u64::try_from(text.len()).expect("demo message range fits"),
        },
    ]
}
