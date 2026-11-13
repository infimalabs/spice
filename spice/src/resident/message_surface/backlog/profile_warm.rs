//! Bounded warming of lane evidence profiles from admitted history.
//!
//! Warming walks two histories newest first: one completion page shared by
//! every owing profile, and per profile, older windows of its lane's tape.
//! Startup takes the first step; each later commit turn takes at most one more
//! step of each, resuming from the recorded cursors, until a profile finds both
//! beginnings or already holds its limit of newer cards in a class. Reads run
//! outside the message-state lock; only folding the found cards holds it.

use spice::rooms::evidence::{Conversation, Evidence};
use spice::rooms::profile::{EvidenceKey, LaneProfiles, TranscriptCursor};

use super::*;

/// Completion facts one task-history step reads.
const TASK_ROWS: usize = 256;
/// Encoded completion bytes one task-history step reads.
const TASK_BYTES: usize = 1024 * 1024;

/// Completion evidence one task-history page found, newest first.
pub(super) struct TaskPage {
    cards: Vec<(CanonicalUuid, Evidence<EvidenceKey>)>,
    next: Option<wire::Seq>,
    complete: bool,
}

/// Conversation evidence and turn boundaries one tape window found for one
/// lane, newest first.
pub(super) struct TranscriptWindow {
    lane: CanonicalUuid,
    rows: Vec<TranscriptRow>,
    crossed: TranscriptCursor,
    complete: bool,
}

/// One newest-first step of a backward tape walk.
enum TranscriptRow {
    /// The record that completed a turn: every card held so far belongs to
    /// the newer turn, which begins after this record.
    TurnEnd,
    Conversation(Evidence<EvidenceKey>),
    /// A record of the current turn whose operator prose carried a room
    /// catch-up, with the Designs it requested: the prompt that opened the
    /// turn or an aside that landed inside it.
    Requested(Vec<wire::TaskStamp>),
}

/// Take at most one further completion page and one older tape window for
/// profiles that still owe history.
pub(in crate::resident) fn advance(
    bus: &bus::BusReader,
    paths: &RuntimePaths,
    state: &Mutex<MessageState>,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    let (facts, transcript) = {
        let mut state = lock_state(state)?;
        state.profiles.settle_full();
        let facts = state.profiles.owed_facts();
        let transcript = state
            .profiles
            .owed_transcript()
            .map(|(lane, vendor, cursor)| (lane, vendor.clone(), cursor));
        (facts, transcript)
    };
    if let Some(before) = facts {
        let page = read_task_page(bus, before);
        let mut state = lock_state(state)?;
        match page {
            Ok(page) => fold_task_page(&mut state.profiles, page),
            Err(error) => fail_task_history(&mut state.profiles, &error, diagnostic),
        }
    }
    if let Some((lane, vendor, cursor)) = transcript {
        let window = read_older_window(paths, lane, &vendor, cursor);
        let mut state = lock_state(state)?;
        match window {
            Ok(window) => fold_transcript_window(&mut state.profiles, window),
            Err(detail) => {
                let _ = writeln!(
                    diagnostic,
                    "lane profile transcript unavailable for lane {lane}: {detail}"
                );
                state.profiles.warm_failed(lane, detail);
            }
        }
    }
    Ok(())
}

pub(super) fn fail_task_history(
    profiles: &mut LaneProfiles,
    error: &Outcome<()>,
    diagnostic: &mut dyn io::Write,
) {
    profiles.facts_failed(format!("task history: {error:?}"));
    let _ = writeln!(
        diagnostic,
        "lane profile task history unavailable: {error:?}"
    );
}

/// Read one newest-first completion page strictly before `before`, attributing
/// each completion to the lane its won claim held.
///
/// A task whose statement was revised after it completed is skipped: its card
/// must carry the statement it was observed with, not a later edit. So is a
/// completion whose terminal operation names no won claim, exactly as live
/// retention admits only claim-owned completions: the card's support is the
/// phase that claim held.
pub(super) fn read_task_page(
    bus: &bus::BusReader,
    before: Option<wire::Seq>,
) -> CommandResult<TaskPage> {
    let budget = journal::FactReadBudget::new(TASK_ROWS, TASK_BYTES).map_err(|error| {
        store_fault(
            "room::profile-task-history-budget-invalid",
            &error.to_string(),
        )
    })?;
    let page = bus
        .recent_fact_page(wire::FactKind::TaskDone, before, budget)
        .map_err(|error| {
            store_fault("room::profile-task-history-unreadable", &error.to_string())
        })?;
    let snapshot = bus
        .task_snapshot()
        .map_err(|error| store_fault("room::profile-task-heads-unreadable", &error.to_string()))?;
    let mut cards = Vec::new();
    for envelope in page.rows {
        let completion = envelope.seq;
        let Some(done) = envelope.fact.read::<wire::body::TaskDone>() else {
            continue;
        };
        let Some(statement) = snapshot
            .task_heads(&done.task)
            .map(|heads| heads.statement().body())
        else {
            continue;
        };
        if statement.revision > completion {
            continue;
        }
        let Some(won) = done
            .terminal
            .as_ref()
            .and_then(|terminal| wire::intent::TaskDone::claim_for_operation(terminal.operation))
            .and_then(|claim| snapshot.facts().at(claim))
            .and_then(|claimed| claimed.fact.read::<wire::body::ClaimWon>())
        else {
            continue;
        };
        let support = wire::rooms::DesignSupport::for_phase(&won.cell.phase);
        cards.push((
            won.lane,
            Evidence::task(
                EvidenceKey::Task {
                    task: done.task,
                    statement: statement.revision,
                    completion,
                    support,
                },
                &statement.title,
                &statement.pitch,
                statement.description.as_deref(),
                &statement.term,
                &done.validation,
            ),
        ));
    }
    Ok(TaskPage {
        cards,
        next: page.continuation,
        complete: page.continuation.is_none(),
    })
}

pub(super) fn fold_task_page(profiles: &mut LaneProfiles, page: TaskPage) {
    for (lane, card) in page.cards {
        profiles.retain_older(lane, card);
    }
    profiles.warmed_facts(page.next, page.complete);
}

/// Read the lane's `vendor` tape window just older than `cursor`, or the
/// newest window when the walk has not begun.
fn read_older_window(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    vendor: &wire::Name,
    cursor: Option<TranscriptCursor>,
) -> Result<TranscriptWindow, String> {
    let dialect = ingest_vendor(vendor)
        .ok_or_else(|| format!("vendor {vendor} has no transcript dialect"))?
        .dialect();
    let tape = paths.lane_tape(lane).join(vendor.as_str());
    let tail = match cursor {
        None => listen::scan_tail(&tape, dialect, startup_budget()),
        Some(cursor) => {
            let before = listen::Checkpoint {
                source: cursor
                    .source
                    .to_string()
                    .parse()
                    .map_err(|error| format!("{error}"))?,
                next: listen::Position {
                    member: cursor.member,
                    offset: cursor.offset,
                },
            };
            listen::scan_before(&tape, dialect, &before, startup_budget())
        }
    }
    .map_err(|error| error.to_string())?;
    read_transcript_window(lane, &tail.scan, &tail.start).map_err(|error| format!("{error:?}"))
}

/// Collect the conversation evidence one scanned window of `lane`'s tape
/// holds, newest first, whichever session wrote it.
pub(super) fn read_transcript_window(
    lane: CanonicalUuid,
    scan: &listen::Scan,
    start: &listen::Checkpoint,
) -> CommandResult<TranscriptWindow> {
    let source = scan_source(scan)?;
    let mut rows = Vec::new();
    for record in scan.records.iter().rev() {
        let listen::RecordPayload::Events(events) = &record.payload else {
            continue;
        };
        let turn = record.context.as_ref().and_then(|context| context.turn);
        if turn == Some(listen::TurnMoment::Completed) {
            rows.push(TranscriptRow::TurnEnd);
        }
        for sourced in events.iter().rev() {
            let listen::Event::Prose(prose) = &sourced.event else {
                continue;
            };
            if prose.role != listen::ProseRole::Assistant {
                continue;
            }
            let Some(conversation) = Conversation::read(&prose.text) else {
                continue;
            };
            rows.push(TranscriptRow::Conversation(Evidence::conversation(
                EvidenceKey::conversation(
                    source,
                    prose.locus.member,
                    prose.locus.offset,
                    prose.locus.length,
                    Vec::new(),
                ),
                &conversation,
            )));
        }
        let requested = spice::rooms::profile::design_request_causes(
            events.iter().map(|sourced| &sourced.event),
        );
        if !requested.is_empty() {
            rows.push(TranscriptRow::Requested(requested));
        }
    }
    Ok(TranscriptWindow {
        lane,
        rows,
        crossed: TranscriptCursor {
            source,
            member: start.next.member,
            offset: start.next.offset,
        },
        complete: start.next == listen::Position::start(),
    })
}

pub(super) fn fold_transcript_window(profiles: &mut LaneProfiles, window: TranscriptWindow) {
    for row in window.rows {
        match row {
            TranscriptRow::TurnEnd => profiles.resolve_older_conversation_turn(window.lane),
            TranscriptRow::Conversation(card) => {
                profiles.hold_older_conversation(window.lane, card);
            }
            TranscriptRow::Requested(caused_by) => {
                profiles.older_turn_causes(window.lane, &caused_by);
            }
        }
    }
    if window.complete {
        profiles.resolve_older_conversation_turn(window.lane);
    }
    profiles.warmed_transcript(window.lane, window.crossed, window.complete);
}
