use super::*;

use spice::rooms::{
    Declared, LaneAvailability, RoomDuty, RoomDutyKind, RoomDutyStanding, RoomProjectionBudget,
    RoomProjectionError, RoomProjectionSources, RoomWork, derive_room_attention,
};
use wire::rooms::{ROOM_PUBLICATION_BYTES_LIMIT, RoomAuthorityStamp};

const ROOM_PUBLICATION_PAGES: usize =
    spice::rooms::MAX_ROOM_ATTENTION_SIGNALS.div_ceil(journal::MAX_RETAINED_FACT_PAGE_ROWS);

pub(super) fn room_post(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    post: &RoomPost,
    repository: Option<&Repository>,
) -> Outcome<CommandAnswer> {
    let content = match authored_observation(&post.text, &post.room) {
        Ok(content) => content,
        Err(outcome) => return outcome,
    };
    let snapshot = match validated_working_snapshot(
        bus,
        repository,
        command.actor().clone(),
        std::slice::from_ref(&post.room),
    ) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let Some(anchor) = task_entry(&snapshot.entries, &post.room) else {
        return refusal(
            "room::anchor-unknown",
            Some(post.room.to_string()),
            "read the board and name one current task handle",
        );
    };
    if let Err(outcome) = publishable_room(anchor) {
        return outcome;
    }
    let room = anchor.born().clone();
    let publications = match room_publications(bus, &room, None) {
        Ok(publications) => publications,
        Err(outcome) => return outcome,
    };
    let now = jiff::Timestamp::now();
    let at = match Instant::new(now) {
        Ok(at) => at,
        Err(error) => return environment_fault("room::clock-invalid", error.to_string()),
    };
    let attention = match project_attention(
        bus,
        &snapshot.entries,
        &snapshot.answer.residency.body,
        &room,
        &publications,
    ) {
        Ok(attention) => attention,
        Err(outcome) => return outcome,
    };
    let publication = match mint_publication(bus, &room, now.as_microsecond()) {
        Ok(publication) => publication,
        Err(outcome) => return outcome,
    };
    let origin = match task_origin(&room) {
        Ok(origin) => origin,
        Err(outcome) => return outcome,
    };
    let answer = RoomPostAnswer {
        room: anchor.handle().clone(),
        publication: publication.clone(),
        outcome: post_outcome(attention),
    };
    let mut write = JournalWrite::new(command.actor().clone(), origin, at, Producer::CommandDoor);
    guard(&mut write, command.expected().to_vec());
    if let Err(error) = write.fact(
        RoomPublished {
            room,
            publication,
            content,
        },
        FactCause::Root,
    ) {
        return store_fault("room::post-build", error.to_string());
    }
    if let Err(error) = write.command_result(
        request(command, post.clone()),
        command.epoch(),
        answer.clone(),
    ) {
        return store_fault("room::post-receipt", error.to_string());
    }
    committed(bus, write, CommandAnswer::RoomPost(answer))
}

/// Refuse a publication into a deleted task's room; every other task's room
/// takes observations, whatever its standing.
pub(super) fn publishable_room(anchor: &Entry) -> Result<(), Outcome<CommandAnswer>> {
    if matches!(
        anchor.workflow().status.conclusion(),
        Some(wire::WorkflowConclusion::Negative(
            wire::WorkflowNegative::Deleted
        ))
    ) {
        return Err(refusal(
            "room::anchor-deleted",
            Some(format!("{} deleted", anchor.handle())),
            "a deleted task's room stays readable; publish to a task that is not deleted",
        ));
    }
    Ok(())
}

/// An authored observation of exactly `text`: refused when blank or over the
/// room publication bound, measured on the authored bytes.
pub(super) fn authored_observation(
    text: &str,
    room: &TaskHandle,
) -> Result<RoomPublicationContent, Outcome<CommandAnswer>> {
    if text.trim().is_empty() {
        return Err(refusal(
            "room::post-empty",
            Some(room.to_string()),
            "supply complete authored text",
        ));
    }
    if text.len() > ROOM_PUBLICATION_BYTES_LIMIT {
        return Err(refusal(
            "room::post-too-large",
            Some(text.len().to_string()),
            format!("shorten the observation to at most {ROOM_PUBLICATION_BYTES_LIMIT} bytes"),
        ));
    }
    Ok(RoomPublicationContent::Authored {
        text: text.to_owned(),
    })
}

/// One room's publications after `after`, refused past the bounded page budget.
pub(crate) fn room_publications(
    bus: &Bus,
    room: &wire::TaskStamp,
    after: Option<wire::Seq>,
) -> Result<Vec<wire::Envelope<wire::AnyFact>>, Outcome<CommandAnswer>> {
    let room = room.clone();
    let (rows, more) = bus
        .read(move |read| read_room_publications(read, &room, after))
        .map_err(|error| store_fault("room::publications-unreadable", error.to_string()))?;
    if more {
        return Err(refusal(
            "room::attention-budget",
            Some(format!(
                "room publications exceed {} rows or {} bounded pages",
                spice::rooms::MAX_ROOM_ATTENTION_SIGNALS,
                ROOM_PUBLICATION_PAGES
            )),
            "reduce retained room traffic before publishing another message",
        ));
    }
    Ok(rows)
}

/// One room's publications after `after` within the bounded pages, and
/// whether more remain beyond them.
pub(crate) fn read_room_publications(
    read: &journal::JournalRead<'_>,
    room: &wire::TaskStamp,
    after: Option<wire::Seq>,
) -> Result<(Vec<wire::Envelope<wire::AnyFact>>, bool), journal::GuardError> {
    let mut rows = Vec::new();
    let mut after = after;
    for _ in 0..ROOM_PUBLICATION_PAGES {
        let page = read.room_publications(room, after, journal::FactReadBudget::RETAINED)?;
        rows.extend(page.rows);
        let Some(continuation) = page.continuation else {
            return Ok((rows, false));
        };
        after = Some(continuation);
    }
    Ok((rows, true))
}

/// One room's listeners: lanes with native work or live stop requests in it,
/// and lanes a recorded Design request asked about it until
/// their next room publication answers or declines it. Authoring a message or
/// contribution, routing a packet into the room, an owed receipt, or a pending
/// Design-request result subscribes no one: steering delivers each directly. A
/// lane's leave ends requests it postdates, never its native work or duties.
pub(crate) fn project_attention(
    bus: &Bus,
    entries: &[Entry],
    residency: &wire::ResidencyHead,
    room: &wire::TaskStamp,
    publications: &[wire::Envelope<wire::AnyFact>],
) -> Result<spice::rooms::RoomAttention, Outcome<CommandAnswer>> {
    let work = entries
        .iter()
        .flat_map(|entry| RoomWork::from_task(room, entry))
        .collect::<Vec<_>>();
    let probe = room.clone();
    let rows = bus
        .read(move |read| read_room_duty_rows(read, &probe))
        .map_err(|error| store_fault("room::duties-unreadable", error.to_string()))?;
    let duties = room_duties(room, rows)?;
    derive_attention(room, publications, &work, &duties, residency)
}

/// [`project_attention`] for inputs already in hand: `work` and `duties` may
/// cover several rooms, since the projection filters by anchor.
pub(crate) fn derive_attention(
    room: &wire::TaskStamp,
    publications: &[wire::Envelope<wire::AnyFact>],
    work: &[RoomWork],
    duties: &[RoomDuty],
    residency: &wire::ResidencyHead,
) -> Result<spice::rooms::RoomAttention, Outcome<CommandAnswer>> {
    derive_room_attention(
        room,
        &RoomProjectionSources {
            publications: Declared::Available(publications),
            work: Declared::Available(work),
            duties: Declared::Available(duties),
            responsibilities: Declared::Available(&[]),
            residency: Declared::Available(residency),
        },
        RoomProjectionBudget::STANDARD,
    )
    .map_err(projection_refusal)
}

/// The indexed rows one room's duties are derived from, read inside one
/// journal transaction so a caller projecting many rooms pays one round trip.
pub(crate) struct RoomDutyRows {
    /// Design notice records naming the room, oldest first.
    asks: Vec<wire::Envelope<wire::AnyFact>>,
    /// Whether notices remained beyond the bounded pages read.
    asks_overflow: bool,
    /// Live stop requests bound to the room, in creation order.
    stops: Vec<journal::JournalHead<wire::SteeringHead>>,
}

/// Read the rows behind `room`'s duties through the per-Design notice index
/// and the steering index; a non-Design room has no notices, and a deferred
/// Design's room owes none, since deferral ends its outstanding invitations.
pub(crate) fn read_room_duty_rows(
    read: &journal::JournalRead<'_>,
    room: &wire::TaskStamp,
) -> Result<RoomDutyRows, journal::GuardError> {
    let mut asks = Vec::new();
    let mut after = None;
    let mut asks_overflow = false;
    let deferred = read
        .workflow_head(room)?
        .is_some_and(|head| head.body().deferral.is_some());
    if !deferred {
        asks_overflow = true;
        for _ in 0..ROOM_PUBLICATION_PAGES {
            let page = read.design_notices(room, after, journal::FactReadBudget::RETAINED)?;
            asks.extend(page.rows);
            let Some(continuation) = page.continuation else {
                asks_overflow = false;
                break;
            };
            after = Some(continuation);
        }
    }
    let stops = read.live_room_stops(room, spice::rooms::MAX_ROOM_ATTENTION_SIGNALS)?;
    Ok(RoomDutyRows {
        asks,
        asks_overflow,
        stops,
    })
}

/// Every duty bound to `room`: the newest recorded Design request that asked
/// each lane about it, and each live stop request, which no leave can end.
pub(crate) fn room_duties(
    room: &wire::TaskStamp,
    rows: RoomDutyRows,
) -> Result<Vec<RoomDuty>, Outcome<CommandAnswer>> {
    if rows.asks_overflow {
        return Err(refusal(
            "room::attention-budget",
            Some(format!(
                "Design requests for {room} exceed {} bounded pages",
                ROOM_PUBLICATION_PAGES
            )),
            "reduce retained room traffic before retrying",
        ));
    }
    let limit = spice::rooms::MAX_ROOM_ATTENTION_SIGNALS;
    if rows.stops.len() >= limit {
        return Err(refusal(
            "room::attention-budget",
            Some(format!(
                "room {room} holds {limit} or more live stop requests"
            )),
            "settle or cancel live room stop requests before retrying",
        ));
    }
    let mut newest = std::collections::BTreeMap::new();
    for envelope in rows.asks {
        if let Some(noticed) = envelope.fact.read::<wire::rooms::DesignNoticed>()
            && noticed.design == *room
        {
            newest.insert(
                noticed.lane,
                (notice_source(&noticed.evidence), envelope.seq),
            );
        }
    }
    let asked = newest
        .into_iter()
        .map(|(lane, (source, position))| RoomDuty {
            room: room.clone(),
            lane,
            kind: RoomDutyKind::Asked { source, position },
            standing: RoomDutyStanding::Owed,
        });
    let stops = rows.stops.into_iter().filter_map(|head| {
        let position = head.body().created;
        let head = head.into_body();
        Some(RoomDuty {
            room: room.clone(),
            lane: head.recipient.lane()?,
            kind: RoomDutyKind::Stop {
                key: head.key,
                position,
            },
            standing: RoomDutyStanding::Owed,
        })
    });
    Ok(asked.chain(stops).collect())
}

fn notice_source(evidence: &wire::rooms::DesignNoticeEvidence) -> String {
    match evidence {
        wire::rooms::DesignNoticeEvidence::Task { task, .. }
        | wire::rooms::DesignNoticeEvidence::TaskClaim { task, .. } => format!("task {task}"),
        wire::rooms::DesignNoticeEvidence::Conversation { source } => {
            let locus = &source.locus;
            format!(
                "conversation {}:{}:{}",
                locus.member, locus.offset, locus.event
            )
        }
    }
}

fn projection_refusal(error: RoomProjectionError) -> Outcome<CommandAnswer> {
    refusal(
        "room::attention-unavailable",
        Some(error.to_string()),
        "repair the named room authority source, then retry the post",
    )
}

/// The first unused publication identity in `room` at or after `micros`.
///
/// Each occupied candidate is a distinct earlier publication, so the probe
/// ends within the room's size; the clock range is the only other limit.
pub(super) fn mint_publication(
    bus: &Bus,
    room: &wire::TaskStamp,
    mut micros: i64,
) -> Result<RoomAuthorityStamp, Outcome<CommandAnswer>> {
    loop {
        let candidate = RoomAuthorityStamp::for_micros(micros).map_err(|error| {
            environment_fault("room::publication-clock-invalid", error.to_string())
        })?;
        let (probe_room, probe) = (room.clone(), candidate.clone());
        let occupied = bus
            .read(move |read| read.room_publication_seq(&probe_room, &probe))
            .map_err(|error| store_fault("room::publication-unreadable", error.to_string()))?;
        if occupied.is_none() {
            return Ok(candidate);
        }
        micros = micros.checked_add(1).ok_or_else(|| {
            environment_fault(
                "room::publication-clock-exhausted",
                "room publication clock exceeded the native stamp range",
            )
        })?;
    }
}

fn post_outcome(attention: spice::rooms::RoomAttention) -> RoomPostOutcome {
    let listeners = attention
        .lanes
        .into_iter()
        .filter(|lane| lane.listens())
        .map(|lane| {
            let (availability, delivery) = match lane.availability {
                LaneAvailability::Occupied => (
                    RoomListenerAvailability::Occupied,
                    RoomListenerDelivery::AtSafeBoundary,
                ),
                LaneAvailability::Vacant => (
                    RoomListenerAvailability::Vacant,
                    RoomListenerDelivery::WaitsForNextStart,
                ),
                LaneAvailability::Unavailable { disposition } => (
                    RoomListenerAvailability::Unavailable { disposition },
                    RoomListenerDelivery::PendingAvailability,
                ),
                LaneAvailability::Unknown => (
                    RoomListenerAvailability::Unknown,
                    RoomListenerDelivery::PendingAvailability,
                ),
            };
            RoomPostListener {
                lane: lane.lane,
                availability,
                delivery,
            }
        })
        .collect::<Vec<_>>();
    if listeners.is_empty() {
        RoomPostOutcome::RecordedNoCurrentListener {
            next: RoomPostNext::StartOrDirect,
        }
    } else {
        RoomPostOutcome::RecordedWithListeners { listeners }
    }
}
