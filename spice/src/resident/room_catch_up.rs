//! Unread room activity: marking it for occupied listeners, and recognizing
//! the page a lane read on its transcript.
//!
//! Nothing is rendered here. A transaction that leaves a seated lane with
//! unread activity only kicks that lane's driver, which queues one fixed
//! marker; the lane renders its page by reading `room inbox`.

use std::collections::BTreeSet;

use wire::rooms::{RoomDelivered, TextRange, TranscriptHunk};
use wire::{Actor, CanonicalUuid, Producer, Seq, SequenceCode, TaskStamp};

use super::*;

/// One read of the task board and residency that lane selection shares.
pub(in crate::resident) struct Planning {
    pub(in crate::resident) residency: wire::ResidencyHead,
    pub(in crate::resident) entries: Vec<board::Entry>,
    pub(in crate::resident) snapshot: bus::TaskSnapshot,
    pub(in crate::resident) horizon: Option<Seq>,
}

/// What one durable transaction changed.
#[derive(Debug, Default, Eq, PartialEq)]
pub(in crate::resident) struct Changes {
    /// Lanes whose delivery cursor moved past a page they read.
    pub(in crate::resident) lanes: BTreeSet<CanonicalUuid>,
    /// Rooms that gained a publication.
    pub(in crate::resident) rooms: BTreeSet<TaskStamp>,
    /// Each changed task and its preparation rooms.
    pub(in crate::resident) tasks: BTreeSet<TaskStamp>,
}

/// Kick the driver of each occupied lane `changes` reach, so it marks or
/// withdraws its unread notice. A vacant lane has no driver, so this never
/// starts or wakes a lane.
pub(in crate::resident) fn reconcile_commit(
    bus: &Bus,
    paths: &RuntimePaths,
    seats: &Seats,
    changes: &Changes,
    diagnostic: &mut dyn io::Write,
) {
    for lane in reached(bus, seats, changes, diagnostic) {
        super::supervisor::command::room_catch_up(paths, lane);
    }
}

/// The occupied lanes `changes` reach: a lane whose cursor moved, and a lane
/// a changed room now owes a publication. A lane that listens to none of the
/// changed rooms is not reached.
pub(in crate::resident) fn reached(
    bus: &Bus,
    seats: &Seats,
    changes: &Changes,
    diagnostic: &mut dyn io::Write,
) -> Vec<CanonicalUuid> {
    let mut reached = Vec::new();
    for lane in seats.lanes() {
        if seats.occupant(lane).is_none() {
            continue;
        }
        if changes.lanes.contains(&lane) {
            reached.push(lane);
            continue;
        }
        if changes.rooms.is_empty() {
            continue;
        }
        let rooms = changes.rooms.clone();
        let owed = bus
            .read(move |read| Ok(crate::room_command::unread::owed(read, lane, Some(&rooms))))
            .map_err(|error| error.to_string())
            .and_then(|owed| owed);
        match owed {
            Ok(true) => reached.push(lane),
            Ok(false) => {}
            Err(detail) => {
                let _ = writeln!(
                    diagnostic,
                    "lane {lane} unread room activity unavailable: {detail}"
                );
            }
        }
    }
    reached
}

impl Planning {
    /// The whole task board.
    pub(in crate::resident) fn load(bus: &Bus) -> CommandResult<Self> {
        Self::whole(task_snapshot(bus)?)
    }

    /// The whole task board as `snapshot` holds it.
    pub(in crate::resident) fn whole(snapshot: bus::TaskSnapshot) -> CommandResult<Self> {
        let residency = snapshot
            .residency()
            .ok_or_else(|| {
                store_fault(
                    "room::catch-up-residency-missing",
                    "the resident has no residency head",
                )
            })?
            .body()
            .clone();
        let entries = snapshot
            .tasks()
            .iter()
            .map(|heads| {
                let statement = heads.statement().body();
                board::Entry::new(
                    statement.clone(),
                    heads.workflow().body().clone(),
                    heads.completion().body().clone(),
                    heads.claim_bodies(),
                )
                .map(|entry| entry.with_design(snapshot.facts().design(statement)))
                .map_err(|error| store_fault("room::catch-up-task-invalid", &error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let horizon = snapshot.cursor();
        Ok(Self {
            residency,
            entries,
            snapshot,
            horizon,
        })
    }
}

/// The task heads every board read here observes at one barrier.
pub(in crate::resident) fn task_snapshot(bus: &Bus) -> CommandResult<bus::TaskSnapshot> {
    bus.task_snapshot()
        .map_err(|error| store_fault("room::catch-up-snapshot-unreadable", &error.to_string()))
}

pub(in crate::resident) fn changes(
    bus: &Bus,
    commit: bus::JournalCommit,
) -> CommandResult<Changes> {
    let first = Seq::new(commit.first()).expect("a committed position is positive");
    let last = Seq::new(commit.last()).expect("a committed position is positive");
    bus.read(move |read| {
        let mut changes = Changes {
            rooms: read
                .changed_room_publications(first, last)?
                .into_iter()
                .collect(),
            ..Changes::default()
        };
        let mut tasks = BTreeSet::new();
        let mut start = journal::FactPageStart::At(first);
        loop {
            let page = read.fact_page(start, last, journal::FactReadBudget::RETAINED)?;
            for envelope in &page.rows {
                let fact = &envelope.fact;
                if fact.kind() != wire::FactKind::ClaimRenewed
                    && let Some(task) = fact.task_subject()
                {
                    tasks.insert(task.clone());
                }
                tasks.extend(fact.mutated_tasks().cloned());
                if let Some(delivered) = fact.read::<RoomDelivered>() {
                    changes.lanes.insert(delivered.hunk.head.lane);
                }
            }
            let Some(overflow) = page.overflow else {
                break;
            };
            start = journal::FactPageStart::At(overflow.continuation);
        }
        for task in tasks {
            if let Some(head) = read.task_head(&task)?
                && let Some(preparation) = &head.body().preparation
            {
                changes.tasks.extend([
                    preparation.parent.clone(),
                    preparation.controller.clone(),
                    preparation.construction.clone(),
                ]);
            }
            changes.tasks.insert(task);
        }
        Ok(changes)
    })
    .map_err(|error| store_fault("room::catch-up-changes-unreadable", &error.to_string()))
}

/// The cursor position and closing-line bytes of a page `lane` read, when
/// `event` is the successful tool result that returned it.
fn landed(event: &listen::Event, lane: CanonicalUuid) -> Option<(Seq, std::ops::Range<usize>)> {
    let listen::Event::Tool(tool) = event else {
        return None;
    };
    let listen::ToolMoment::Result(outcome) = &tool.moment else {
        return None;
    };
    if outcome.result == listen::ToolResult::Failed {
        return None;
    }
    crate::room_command::unread::delivery(outcome.output.as_deref()?, lane)
}

/// Whether `record` carries the closing line of a page `lane` read, whose
/// recognition must cite the record's accounted bytes.
pub(in crate::resident) fn carries_delivery(record: &listen::Record, lane: CanonicalUuid) -> bool {
    match &record.payload {
        listen::RecordPayload::Events(events) => events
            .iter()
            .any(|sourced| landed(&sourced.event, lane).is_some()),
        listen::RecordPayload::Residue(_) => false,
    }
}

/// Advance a lane's cursor once the closing line of a page it read appears,
/// as a tool result, in its accounted transcript. A line at or behind the
/// cursor, in an unaccounted record, or naming a position the journal
/// refuses advances nothing.
pub(in crate::resident) fn recognize(
    bus: &Bus,
    record: &lane::capture::CaptureRecord,
    source: CanonicalUuid,
) -> CommandResult<Option<bus::JournalCommit>> {
    let lane = record.lane;
    let landed = record
        .decoded
        .events()
        .iter()
        .enumerate()
        .rev()
        .find_map(|(event, body)| {
            let (through, bytes) = landed(body, lane)?;
            Some((event, through, bytes))
        });
    let Some((event, through, bytes)) = landed else {
        return Ok(None);
    };
    let current = bus
        .read(move |read| read.room_cursor(lane))
        .map_err(|error| store_fault("room::catch-up-cursor-unreadable", &error.to_string()))?;
    if current.is_some_and(|head| head.body().through.get() >= through.get()) {
        return Ok(None);
    }
    let Some(hunk) = delivery_hunk(bus, lane, source, record.locus, event, bytes)? else {
        return Ok(None);
    };
    let at = current_instant()?;
    let mut write = JournalWrite::new(
        Actor::Spice,
        lane_origin(lane)?,
        at,
        Producer::ResidentRecognition,
    );
    write
        .fact(
            RoomDelivered {
                through: SequenceCode::new(through.get()),
                hunk,
            },
            FactCause::Root,
        )
        .map_err(|error| store_fault("room::catch-up-build", &error.to_string()))?;
    match bus
        .commit(write)
        .map_err(|error| store_fault("room::catch-up-commit", &error.to_string()))?
    {
        CommitOutcome::Committed(commit) => Ok(Some(commit)),
        // The line is text the lane's own tools returned, so a position the
        // journal refuses is not a delivery.
        CommitOutcome::Refused(_) => Ok(None),
        CommitOutcome::Fault(fault) => Err(store_fault("room::catch-up-fault", &fault.to_string())),
    }
}

fn delivery_hunk(
    bus: &Bus,
    lane: CanonicalUuid,
    source: CanonicalUuid,
    locus: listen::Locus,
    event: usize,
    bytes: std::ops::Range<usize>,
) -> CommandResult<Option<TranscriptHunk>> {
    let head = bus
        .read(move |read| read.transcript_source_heads(&lane))
        .map_err(|error| store_fault("room::catch-up-transcript-unreadable", &error.to_string()))?
        .into_iter()
        .map(journal::JournalHead::into_body)
        .find(|head| {
            let end = locus.offset + locus.length;
            head.source == source
                && (locus.member < head.next.member
                    || (locus.member == head.next.member && end <= head.next.offset))
        });
    let Some(head) = head else {
        return Ok(None);
    };
    Ok(Some(TranscriptHunk {
        head,
        locus: wire::MessageLocus {
            member: locus.member,
            offset: locus.offset,
            length: locus.length,
            event: u32::try_from(event)
                .map_err(|error| store_fault("room::catch-up-event-invalid", &error.to_string()))?,
        },
        text: TextRange {
            start: u64::try_from(bytes.start)
                .map_err(|error| store_fault("room::catch-up-text-invalid", &error.to_string()))?,
            end: u64::try_from(bytes.end)
                .map_err(|error| store_fault("room::catch-up-text-invalid", &error.to_string()))?,
        },
    }))
}
