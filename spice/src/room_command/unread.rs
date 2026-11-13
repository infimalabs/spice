//! A lane's unread room activity, rendered when that lane reads `room inbox`.
//!
//! The page holds the publications after the lane's durable delivery cursor
//! in every room it listens to: through its own active work, a Design request
//! that asked it, or a live stop request. Its own packets and packets already
//! directed to it are passed over. Publications show oldest first within one
//! [`CatchUpBudget`], and only the publications a page shows have their
//! sources read. Reading writes nothing: a shown page closes with a delivery
//! line, and the resident moves the cursor past the page once that line
//! lands, as the result of the read, in the lane's accounted transcript.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::Range;

use serde::Serialize;
use spice::rooms::{
    AttentionReason, CatchUpBudget, CatchUpSource, RoomAttention, RoomLaneAttention, RoomWork,
    select_catch_up,
};
use wire::principal::PrincipalDirectory;
use wire::{
    Actor, CanonicalUuid, FaultClass, Outcome, Reply, ReplyPosition, Seq, TaskHandle, TaskStamp,
};

use super::{Failure, Publication, Receipt, RoomSources, Row, SourceOwners, fault, row, store};

/// Most listening reasons one room's line names before counting the rest.
const LISTENING_REASONS: usize = 4;
/// Opens the line that closes a shown page.
const DELIVERY: &str = "room-delivery lane=";

/// One page of a lane's unread room activity.
#[derive(Debug, Serialize)]
pub(crate) struct Unread {
    lane: CanonicalUuid,
    /// Where the lane's cursor stands once this page lands; absent when
    /// nothing is unread.
    #[serde(skip_serializing_if = "Option::is_none")]
    through: Option<Seq>,
    items: Vec<Item>,
    /// Each room an item belongs to, in first-shown order.
    rooms: Vec<Listened>,
    /// Whether unread publications remain behind this page.
    more: bool,
}

/// One unread publication under its room.
#[derive(Debug, Serialize)]
struct Item {
    room: TaskHandle,
    anchor: TaskStamp,
    publication: Publication,
}

/// One shown room and why the lane listens to it.
#[derive(Debug, Serialize)]
struct Listened {
    room: TaskHandle,
    title: String,
    pitch: String,
    /// The description collapsed to its count line.
    description: String,
    listening: Vec<String>,
}

/// What one journal snapshot holds unread for a lane, before any source is
/// read.
struct Gathered {
    after: Option<Seq>,
    /// The task the lane's active claim holds.
    work: Option<board::Entry>,
    rooms: Vec<Room>,
}

/// One listened room with the publications after the lane's cursor.
struct Room {
    task: board::Entry,
    attention: RoomAttention,
    rows: Vec<Row>,
}

/// The unread page of `lane`, read from `journal` and the authorities
/// `owners` names.
pub(crate) fn answer(
    journal: &journal::JournalReader,
    owners: SourceOwners<'_>,
    lane: CanonicalUuid,
) -> Outcome<Unread> {
    match page(journal, owners, lane) {
        Ok((position, body)) => Outcome::Reply(Reply { position, body }),
        Err(failure) => failure.outcome(),
    }
}

/// Whether `lane` has unread room activity, among `only` those rooms when
/// given. No publication source is read and nothing is rendered.
pub(crate) fn owed(
    read: &journal::JournalRead<'_>,
    lane: CanonicalUuid,
    only: Option<&BTreeSet<TaskStamp>>,
) -> Result<bool, String> {
    let detail = |failure: Failure| failure.guard().to_string();
    let gathered = gather(read, lane, only).map_err(detail)?;
    let sources = gathered.sources().map_err(detail)?;
    Ok(gathered
        .select(lane, sources)
        .map_err(detail)?
        .through
        .is_some())
}

/// The cursor position a page's closing line in `text` names for `lane`, and
/// that line's bytes. The line counts only where it opens a line of `text`,
/// behind at most a reader's line number, so a quoted publication body can
/// never stand in for it; the last one wins.
pub(crate) fn delivery(text: &str, lane: CanonicalUuid) -> Option<(Seq, Range<usize>)> {
    let opening = format!("{DELIVERY}{lane} through=");
    text.rmatch_indices(&opening).find_map(|(start, _)| {
        let before =
            text[..start].trim_end_matches(|byte: char| byte.is_ascii_digit() || byte == ':');
        if !(before.is_empty() || before.ends_with('\n')) {
            return None;
        }
        let digits = start + opening.len();
        let end = digits
            + text[digits..]
                .bytes()
                .take_while(u8::is_ascii_digit)
                .count();
        let through = text[digits..end].parse().ok().and_then(Seq::new)?;
        Some((through, start..end))
    })
}

fn page(
    journal: &journal::JournalReader,
    owners: SourceOwners<'_>,
    lane: CanonicalUuid,
) -> Result<(ReplyPosition, Unread), Failure> {
    let (cursor, mut gathered) = journal
        .read_with(|read| -> Result<_, Failure> {
            let cursor = read.cursor().map_err(store)?.ok_or_else(|| {
                fault(
                    FaultClass::Store,
                    "room::cursor-missing",
                    "the admitted journal names no read position",
                )
            })?;
            Ok((cursor, gather(read, lane, None)?))
        })
        .map_err(|error| match error {
            journal::ReadError::Journal(error) => store(error),
            journal::ReadError::Answer(failure) => failure,
        })?;
    // The item budget alone bounds which publications can show, so only
    // those have their sources read before the byte budget is applied.
    let mut sources = gathered.sources()?;
    let candidates = gathered.select(lane, sources.clone())?;
    if let Some(withheld) = candidates.continuation {
        sources.retain(|source| source.seq <= withheld);
    }
    let mut shown = BTreeMap::new();
    for (index, room) in gathered.rooms.iter_mut().enumerate() {
        let mut reader = RoomSources::new(Some(owners), room.task.born());
        for row in std::mem::take(&mut room.rows) {
            if candidates.items.iter().any(|item| item.seq == row.seq) {
                let publication = row.project(owners.tape, &mut reader);
                shown.insert(publication.seq, (index, publication));
            }
        }
    }
    for source in &mut sources {
        if let Some((_, publication)) = shown.get(&source.seq) {
            source.bytes = publication.to_string().len();
        }
    }
    let selected = gathered.select(lane, sources)?;
    let mut rooms = Vec::<usize>::new();
    let mut items = Vec::with_capacity(selected.items.len());
    for item in &selected.items {
        let (index, publication) = shown
            .remove(&item.seq)
            .expect("a selected publication was projected");
        if !rooms.contains(&index) {
            rooms.push(index);
        }
        let task = &gathered.rooms[index].task;
        items.push(Item {
            room: task.handle().clone(),
            anchor: task.born().clone(),
            publication,
        });
    }
    let body = Unread {
        lane,
        through: selected.through,
        items,
        rooms: rooms
            .into_iter()
            .map(|index| gathered.listened(lane, &gathered.rooms[index]))
            .collect(),
        more: selected.continuation.is_some(),
    };
    Ok((ReplyPosition::Snapshot(cursor), body))
}

/// Read what `lane` listens to and every publication after its cursor
/// there, among `only` those rooms when given.
fn gather(
    read: &journal::JournalRead<'_>,
    lane: CanonicalUuid,
    only: Option<&BTreeSet<TaskStamp>>,
) -> Result<Gathered, Failure> {
    let after = read
        .room_cursor(lane)
        .map_err(store)?
        .map(|head| Seq::new(head.body().through.get()).expect("a cursor code is positive"));
    let actor = Actor::Lane(lane);
    let work = read
        .active_claim_for_actor(&actor)
        .map_err(store)?
        .map(|heads| entry(&heads))
        .transpose()?;
    let mut gathered = Gathered {
        after,
        work,
        rooms: Vec::new(),
    };
    let Some(residency) = read.residency_head().map_err(store)? else {
        return Ok(gathered);
    };
    let residency = residency.into_body();
    // A lane listens only through its own active work, a Design request, or
    // a live stop request, so no other room is projected.
    let mut candidates = read.design_notice_rooms().map_err(store)?;
    candidates.extend(read.live_stop_rooms().map_err(store)?);
    if let Some(task) = &gathered.work {
        candidates.insert(task.born().clone());
        if let Some(preparation) = &task.statement().preparation {
            candidates.extend([
                preparation.parent.clone(),
                preparation.controller.clone(),
                preparation.construction.clone(),
            ]);
        }
    }
    if let Some(only) = only {
        candidates.retain(|room| only.contains(room));
    }
    let roles = read.principal_roles().map_err(store)?;
    let principals = PrincipalDirectory::codes(&roles);
    for anchor in candidates {
        let duties = crate::board_command::room_duties(
            &anchor,
            crate::board_command::read_room_duty_rows(read, &anchor).map_err(store)?,
        )
        .map_err(refused)?;
        let latest = read
            .latest_room_publication(&anchor, &actor)
            .map_err(store)?;
        let native = gathered
            .work
            .as_ref()
            .map(|task| RoomWork::from_task(&anchor, task))
            .unwrap_or_default();
        let attention = crate::board_command::derive_attention(
            &anchor,
            latest.as_slice(),
            &native,
            &duties,
            &residency,
        )
        .map_err(refused)?;
        let Some(authority) = attention
            .lanes
            .iter()
            .find(|row| row.lane == lane)
            .and_then(RoomLaneAttention::catch_up_after)
        else {
            continue;
        };
        let start = after.map_or(authority, |cursor| cursor.max(authority));
        let (envelopes, more) =
            crate::board_command::read_room_publications(read, &anchor, Some(start))
                .map_err(store)?;
        if more {
            return Err(Failure::Refusal(crate::read_page::refusal(
                "room",
                "attention-budget",
                Some(format!(
                    "room {anchor} holds more unread publications than one read gathers"
                )),
                "reduce retained room traffic before retrying",
            )));
        }
        if envelopes.is_empty() {
            continue;
        }
        let heads = read
            .task_heads_for(&anchor)
            .map_err(store)?
            .ok_or_else(|| {
                fault(
                    FaultClass::Store,
                    "room::task-missing",
                    format!("room {anchor} has publications but no task"),
                )
            })?;
        gathered.rooms.push(Room {
            task: entry(&heads)?,
            attention,
            rows: envelopes
                .into_iter()
                .map(|envelope| row(read, &principals, envelope))
                .collect::<Result<_, _>>()?,
        });
    }
    Ok(gathered)
}

impl Gathered {
    /// Every gathered publication as a catch-up source that renders no bytes.
    fn sources(&self) -> Result<Vec<CatchUpSource>, Failure> {
        let mut sources = Vec::new();
        for room in &self.rooms {
            for row in &room.rows {
                // A principal route reaches no lane, so it owes no lane a read.
                let recipients = row
                    .directed
                    .iter()
                    .filter_map(|route| match &route.receipt {
                        Receipt::Steering { recipient, .. } => recipient.actor.lane().map(Ok),
                        Receipt::Missing => Some(Err(fault(
                            FaultClass::Store,
                            "room::route-invalid",
                            format!(
                                "room publication {} has no steering head for directive {}",
                                row.publication, route.directive
                            ),
                        ))),
                    })
                    .collect::<Result<_, _>>()?;
                sources.push(CatchUpSource {
                    seq: row.seq,
                    room: room.task.born().clone(),
                    author: match row.actor.actor {
                        Actor::Lane(author) => Some(author),
                        _ => None,
                    },
                    recipients,
                    bytes: 0,
                });
            }
        }
        Ok(sources)
    }

    /// The next page `sources` admit after the lane's cursor.
    fn select(
        &self,
        lane: CanonicalUuid,
        sources: Vec<CatchUpSource>,
    ) -> Result<spice::rooms::CatchUp, Failure> {
        let attention = self
            .rooms
            .iter()
            .map(|room| room.attention.clone())
            .collect::<Vec<_>>();
        select_catch_up(
            lane,
            self.after,
            &attention,
            sources,
            CatchUpBudget::STANDARD,
        )
        .map_err(|error| fault(FaultClass::Store, "room::unread-authority-invalid", error))
    }

    /// `room` as a page names it, with why `lane` listens there.
    fn listened(&self, lane: CanonicalUuid, room: &Room) -> Listened {
        let reasons = room
            .attention
            .lanes
            .iter()
            .find(|row| row.lane == lane)
            .map_or(&[][..], |row| row.attention.as_slice());
        let task = |task: &TaskStamp| match &self.work {
            Some(work) if work.born() == task => work.handle().to_string(),
            _ => task.to_string(),
        };
        let mut listening = reasons
            .iter()
            .take(LISTENING_REASONS)
            .map(|reason| match reason {
                AttentionReason::Work { task: work, .. } => format!("work {}", task(work)),
                AttentionReason::Responsibility { task: work, .. } => {
                    format!("responsibility {}", task(work))
                }
                AttentionReason::Asked { source, .. } => format!("asked from {source}"),
                AttentionReason::Stop { key, .. } => format!("stop request {key}"),
            })
            .collect::<Vec<_>>();
        if reasons.len() > LISTENING_REASONS {
            listening.push(format!("{} more", reasons.len() - LISTENING_REASONS));
        }
        let statement = room.task.statement();
        Listened {
            room: room.task.handle().clone(),
            title: statement.title.clone(),
            pitch: statement.pitch.clone(),
            description: crate::board_command::collapsed_description(
                room.task.handle(),
                statement.description.as_deref(),
            ),
            listening,
        }
    }
}

/// One task's heads as a board entry.
fn entry(heads: &journal::TaskHeads) -> Result<board::Entry, Failure> {
    board::Entry::new(
        heads.statement().body().clone(),
        heads.workflow().body().clone(),
        heads.completion().body().clone(),
        heads.claim_bodies(),
    )
    .map_err(|error| fault(FaultClass::Store, "room::task-head-invalid", error))
}

/// A room projection's refusal or fault as this read's failure.
fn refused(outcome: Outcome<wire::intent::CommandAnswer>) -> Failure {
    match outcome {
        Outcome::Refusal(refusal) => Failure::Refusal(refusal),
        Outcome::Fault(fault) => Failure::Fault(fault),
        Outcome::Reply(_) => fault(
            FaultClass::Store,
            "room::attention-invalid",
            "a room projection failed with a reply",
        ),
    }
}

impl fmt::Display for Unread {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("room inbox")?;
        let Some(through) = self.through else {
            return formatter.write_str("\nno unread room activity");
        };
        formatter.write_str("\nunread activity in the rooms this lane listens to:")?;
        for item in &self.items {
            write!(
                formatter,
                "\n\nroom {} anchor {}\n{}",
                item.room, item.anchor, item.publication
            )?;
        }
        for room in &self.rooms {
            write!(
                formatter,
                "\n\nroom {}: {}\npitch: {}\n{}\nlistening {}: {}",
                room.room,
                room.title,
                room.pitch,
                room.description,
                room.room,
                room.listening.join(", ")
            )?;
        }
        if self.more {
            formatter.write_str(
                "\n\nmore unread activity remains: read the room inbox again after this page",
            )?;
        }
        write!(
            formatter,
            "\n\n{DELIVERY}{} through={}",
            self.lane,
            through.get()
        )
    }
}
