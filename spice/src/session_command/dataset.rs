use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::num::NonZeroUsize;
use std::path::Path;

use listen::{
    Decoded, Dialect, Event, ProseRole, RecordContext, RecordDecodeError, ResidueKind,
    SourcedEvent, Span, ToolMoment, ToolResult, TurnMoment,
};
use serde_json::Value;
use wire::Refusal;

use crate::message_shape::{UNRECOVERABLE_REHYDRATION_INTENT, classify, rehydration_intent};

use super::model::{
    CompactionRecord, FoldBuffer, MessageRecord, Residue, SourceView, TurnRecord, Vendor,
};
use super::source::ResolvedSource;

pub(crate) mod aggregate;
mod horizon;
pub(crate) mod stream;
mod work;
pub(crate) use aggregate::{WindowOptions, WindowScan, WindowTurn};
use horizon::{CompactionTracker, admit_source_order};
pub(crate) use horizon::{counted_compaction_horizon, recent_compaction_horizon};
pub(crate) use work::{ResidueLedger, SessionWork, SessionWorkBudget, text_bytes};

const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
const MAX_FRONTIER_RESULTS: usize = 4 * 1024;
pub(crate) const OPEN_TURN_LIMIT: usize = 64;
const OPEN_TOOL_LIMIT: usize = 1_024;

#[derive(Debug)]
pub(crate) struct TypedRecord {
    source: String,
    line: u64,
    vendor: Vendor,
    context: RecordContext,
    events: Vec<SourcedEvent>,
}

#[derive(Debug)]
pub(crate) struct Marker {
    source: String,
    position: u64,
    ts: String,
    summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompactionHorizon {
    pub(crate) start: Option<String>,
    pub(crate) total: Option<usize>,
    sources: Vec<CompactionSourceHorizon>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompactionSourceHorizon {
    has_compaction: bool,
    total: usize,
    start_offset: Option<u64>,
    assistant_before: String,
    vendor: Vendor,
}

#[derive(Debug)]
pub(crate) struct PendingTool {
    pub(crate) turn: usize,
}

pub(crate) enum DecodedRow {
    Typed(Box<TypedRecord>),
    Residue(Residue),
    Ignored,
}

fn decode_row(
    source: &ResolvedSource,
    line: u64,
    buffer: &[u8],
    dialect: Option<Dialect>,
) -> Result<(Option<Dialect>, DecodedRow), Refusal> {
    if buffer.len() > MAX_LINE_BYTES {
        return Err(refusal(
            "session::transcript-record-oversized",
            Some(format!(
                "{}:{line} exceeds {MAX_LINE_BYTES} bytes",
                source.path.display()
            )),
            "restore a bounded transcript record or select an earlier intact copy",
        ));
    }
    let locus = listen::Locus::new(line, 0, buffer.len() as u64);
    let mut detected = match listen::decode_record(dialect, buffer, locus) {
        Ok(detected) => detected,
        Err(RecordDecodeError::Locus(error)) => {
            return Err(record_refusal(
                &source.path,
                line,
                &RecordDecodeError::Locus(error),
            ));
        }
        Err(RecordDecodeError::Invalid { kind }) => {
            return Ok((dialect, decode_residue(source, line, kind)));
        }
        Err(error @ RecordDecodeError::Unowned) => {
            return Ok((
                dialect,
                DecodedRow::Residue(transcript_residue(
                    source,
                    line,
                    "dialect-unowned",
                    error.to_string(),
                    "select the owning vendor decoder to interpret this record; surrounding typed history remains available",
                )),
            ));
        }
    };
    if let Err(error) = crate::session_media::resolve(&mut detected.decoded, &source.path) {
        let observed = detected.dialect;
        return Ok((
            Some(observed),
            DecodedRow::Residue(transcript_residue(
                source,
                line,
                error.kind(),
                error.to_string(),
                error.recovery(),
            )),
        ));
    }
    let observed = detected.dialect;
    let row = match detected.decoded {
        Decoded::Residue(residue) => decode_residue(source, line, residue.kind),
        Decoded::Events { context, events } => {
            typed_row(source, line, buffer, observed, *context, events)
        }
    };
    Ok((Some(observed), row))
}
pub(crate) fn captured_row(
    source: &str,
    position: u64,
    vendor: Vendor,
    record: listen::Record,
    prior_timestamp: Option<&str>,
) -> Result<DecodedRow, Refusal> {
    match record.payload {
        listen::RecordPayload::Residue(residue) => {
            Ok(captured_residue(source, position, residue.residue.kind))
        }
        listen::RecordPayload::Events(events) => {
            let mut context = record
                .context
                .or_else(|| events.first().map(|event| event.context.clone()))
                .unwrap_or_default();
            if context.timestamp.is_none() {
                context.timestamp = context
                    .message
                    .as_ref()
                    .and_then(|message| message.timestamp)
                    .and_then(|millis| i64::try_from(millis).ok())
                    .and_then(|millis| jiff::Timestamp::from_millisecond(millis).ok())
                    .map(|timestamp| timestamp.to_string())
                    .or_else(|| prior_timestamp.map(str::to_owned));
            }
            if events.is_empty() && context.turn.is_none() && context.timestamp.is_none() {
                return Ok(DecodedRow::Ignored);
            }
            let Some(raw_timestamp) = context.timestamp.as_deref() else {
                if events
                    .iter()
                    .all(|event| matches!(event.event, listen::Event::Session(_)))
                {
                    return Ok(DecodedRow::Ignored);
                }
                return Ok(DecodedRow::Residue(Residue {
                    source: source.to_owned(),
                    line: position,
                    kind: "timestamp-missing".to_owned(),
                    detail: "recognized capture record has no message instant or preceding ordered \
                             event"
                        .to_owned(),
                    recovery: "retain surrounding typed capture history; do not substitute provider ordering"
                        .to_owned(),
                }));
            };
            let Some(timestamp) = spice::session_meter::normalize_timestamp(raw_timestamp) else {
                return Ok(DecodedRow::Residue(Residue {
                    source: source.to_owned(),
                    line: position,
                    kind: "timestamp-invalid".to_owned(),
                    detail: format!(
                        "timestamp {raw_timestamp:?} cannot be normalized or ordered safely"
                    ),
                    recovery: "retain surrounding typed capture history; no substitute ordering key is used"
                        .to_owned(),
                }));
            };
            context.timestamp = Some(timestamp.to_string());
            Ok(DecodedRow::Typed(Box::new(TypedRecord {
                source: source.to_owned(),
                line: position,
                vendor,
                context,
                events,
            })))
        }
    }
}

fn captured_residue(source: &str, position: u64, kind: ResidueKind) -> DecodedRow {
    let (kind, detail) = residue_detail(kind);
    DecodedRow::Residue(Residue {
        source: source.to_owned(),
        line: position,
        kind: kind.to_owned(),
        detail,
        recovery: "upgrade the selected-record decoder; surrounding typed capture history remains available"
            .to_owned(),
    })
}

fn residue_detail(kind: ResidueKind) -> (&'static str, String) {
    match kind {
        ResidueKind::Absent { field } => (
            "record-field-absent",
            format!("required field {field:?} is absent"),
        ),
        ResidueKind::Unknown { discriminator } => ("unrecognized-record", discriminator),
        ResidueKind::Malformed { field, expected } => (
            "record-field-malformed",
            format!("field {field:?} must be {expected}"),
        ),
        ResidueKind::Truncated => (
            "record-truncated",
            "JSON ended before one complete record was available".to_owned(),
        ),
        ResidueKind::VersionDrift { discriminator } => (
            "record-version-drift",
            format!("unsupported discriminator {discriminator:?}"),
        ),
    }
}

fn decode_residue(source: &ResolvedSource, line: u64, kind: ResidueKind) -> DecodedRow {
    let (kind, detail) = residue_detail(kind);
    DecodedRow::Residue(transcript_residue(
        source,
        line,
        kind,
        detail,
        "upgrade the owning transcript decoder or recover this record from its original source; surrounding typed history remains available",
    ))
}

fn typed_row(
    source: &ResolvedSource,
    line: u64,
    buffer: &[u8],
    dialect: Dialect,
    mut context: RecordContext,
    events: Vec<Event>,
) -> DecodedRow {
    if events.is_empty() && context.turn.is_none() && context.timestamp.is_none() {
        return DecodedRow::Ignored;
    }
    let Some(raw_timestamp) = context.timestamp.as_deref() else {
        return DecodedRow::Residue(transcript_residue(
            source,
            line,
            "timestamp-missing",
            "recognized record has no timestamp and cannot be ordered safely".to_owned(),
            "recover the original timestamped record; surrounding typed history remains available",
        ));
    };
    let Some(timestamp) = spice::session_meter::normalize_timestamp(raw_timestamp) else {
        return DecodedRow::Residue(transcript_residue(
            source,
            line,
            "timestamp-invalid",
            format!("timestamp {raw_timestamp:?} cannot be normalized or ordered safely"),
            "recover the original timestamp; no substitute ordering key is used and surrounding typed history remains available",
        ));
    };
    context.timestamp = Some(timestamp.to_string());
    let span = Span {
        member: line,
        offset: 0,
        length: buffer.len() as u64,
    };
    let sourced = events
        .into_iter()
        .enumerate()
        .map(|(ordinal, event)| SourcedEvent {
            ordinal: ordinal as u64,
            spans: vec![span],
            context: context.clone(),
            event,
        })
        .collect();
    DecodedRow::Typed(Box::new(TypedRecord {
        source: source.path.display().to_string(),
        line,
        vendor: Vendor::inferred(dialect),
        context,
        events: sourced,
    }))
}

fn transcript_residue(
    source: &ResolvedSource,
    line: u64,
    kind: &str,
    detail: String,
    recovery: &str,
) -> Residue {
    Residue {
        source: source.path.display().to_string(),
        line,
        kind: kind.to_owned(),
        detail,
        recovery: recovery.to_owned(),
    }
}

fn record_refusal(path: &Path, line: u64, error: &RecordDecodeError) -> Refusal {
    let reason = match error {
        RecordDecodeError::Invalid { .. } => "session::transcript-evidence-invalid",
        RecordDecodeError::Unowned => "session::transcript-dialect-unowned",
        RecordDecodeError::Locus(_) => "session::transcript-locus-invalid",
    };
    refusal(
        reason,
        Some(format!("{}:{line}: {error}", path.display())),
        "select the transcript's owning vendor release and retry without a fallback decoder",
    )
}

fn read_residue(path: &Path, line: u64, error: &io::Error) -> Residue {
    Residue {
        source: path.display().to_string(),
        line,
        kind: "read-interrupted".to_owned(),
        detail: error.to_string(),
        recovery: "retain the readable prefix and restore an uninterrupted transcript copy"
            .to_owned(),
    }
}

#[derive(Default)]
pub(crate) struct FoldState {
    pub(crate) current: Option<usize>,
    pub(crate) pending: BTreeMap<String, PendingTool>,
}

impl FoldState {
    /// How many leading buffered turns no open turn or tool still points into.
    pub(crate) fn closed_prefix(&self, buffered: usize) -> usize {
        self.current
            .into_iter()
            .chain(self.pending.values().map(|pending| pending.turn))
            .min()
            .unwrap_or(buffered)
    }

    /// Rebase open indices after the first `drained` buffered turns left,
    /// refusing once more turns or tools stay open than their limits allow.
    pub(crate) fn release(
        &mut self,
        drained: usize,
        open_turns: usize,
        at: &str,
    ) -> Result<(), Refusal> {
        if drained > 0 {
            self.current = self.current.map(|index| index - drained);
            for pending in self.pending.values_mut() {
                pending.turn -= drained;
            }
        }
        if open_turns > OPEN_TURN_LIMIT {
            return Err(refusal(
                "session::aggregate-open-turn-limit",
                Some(format!("limit={OPEN_TURN_LIMIT} at={at:?}")),
                "repair unresolved tool spans or narrow the immutable transcript inputs",
            ));
        }
        if self.pending.len() > OPEN_TOOL_LIMIT {
            return Err(refusal(
                "session::aggregate-open-tool-limit",
                Some(format!("limit={OPEN_TOOL_LIMIT} at={at:?}")),
                "repair unresolved tool spans or narrow the immutable transcript inputs",
            ));
        }
        Ok(())
    }
}

pub(crate) struct FoldStorage<'a> {
    pub(crate) dataset: &'a mut FoldBuffer,
    pub(crate) markers: &'a mut Vec<Marker>,
    pub(crate) residues: &'a mut ResidueLedger,
    pub(crate) state: &'a mut FoldState,
}

#[derive(Clone, Copy)]
pub(crate) enum FrontierDirection {
    Oldest,
    Newest,
}

enum FrontierProgress<C> {
    More(C),
    Complete(C),
    Refused { continuation: C, error: Refusal },
}

pub(crate) struct ResultFrontier<T> {
    direction: FrontierDirection,
    limit: NonZeroUsize,
    rows: VecDeque<T>,
    offered: usize,
}

struct FrontierPage<T, C> {
    rows: Vec<T>,
    progress: FrontierProgress<C>,
}

impl<T> ResultFrontier<T> {
    pub(crate) fn new(limit: usize, direction: FrontierDirection) -> Result<Self, Refusal> {
        let limit = frontier_limit(limit)?;
        Ok(Self {
            direction,
            limit,
            rows: VecDeque::with_capacity(limit.get()),
            offered: 0,
        })
    }

    pub(crate) fn push(&mut self, row: T) {
        self.offered += 1;
        if self.rows.len() < self.limit.get() {
            self.rows.push_back(row);
        } else if matches!(self.direction, FrontierDirection::Newest) {
            self.rows.pop_front();
            self.rows.push_back(row);
        }
    }

    /// Every row offered, retained or not: the population a page reports.
    pub(crate) const fn offered(&self) -> usize {
        self.offered
    }

    fn is_full(&self) -> bool {
        self.rows.len() == self.limit.get()
    }

    pub(crate) fn into_rows(self) -> Vec<T> {
        self.rows.into()
    }

    fn into_page<C>(self, progress: FrontierProgress<C>) -> FrontierPage<T, C> {
        FrontierPage {
            rows: self.rows.into(),
            progress,
        }
    }
}

pub(crate) fn frontier_limit(limit: usize) -> Result<NonZeroUsize, Refusal> {
    NonZeroUsize::new(limit)
        .filter(|limit| limit.get() <= MAX_FRONTIER_RESULTS)
        .ok_or_else(|| {
            refusal(
                "session::result-frontier-invalid",
                Some(format!("limit={limit} maximum={MAX_FRONTIER_RESULTS}")),
                "use one positive capped session result limit",
            )
        })
}

pub(crate) fn reduce_record(
    record: &TypedRecord,
    storage: &mut FoldStorage<'_>,
    work: &mut SessionWork,
    at: &str,
) -> Result<(), Refusal> {
    let timestamp = record_timestamp(record);
    match record.context.turn {
        Some(TurnMoment::Started) => {
            if let Some(index) = storage.state.current {
                finish_turn(storage.dataset, index, timestamp, false, work, at)?;
            }
            storage.state.current = Some(start_turn(
                storage.dataset,
                record,
                record.context.turn_id.as_deref(),
                work,
                at,
            )?);
        }
        Some(TurnMoment::Observed) => {
            storage.state.current = Some(observe_turn(
                storage.dataset,
                record,
                storage.state.current,
                work,
                at,
            )?);
        }
        Some(TurnMoment::Completed) | None => {}
    }
    for sourced in &record.events {
        reduce_event(record, &sourced.event, storage, work, at)?;
    }

    if record.context.turn == Some(TurnMoment::Completed) {
        let index = ensure_turn(
            storage.dataset,
            record,
            &mut storage.state.current,
            work,
            at,
        )?;
        finish_turn(storage.dataset, index, timestamp, true, work, at)?;
        storage.state.current = None;
    }
    Ok(())
}

fn reduce_event(
    record: &TypedRecord,
    event: &Event,
    storage: &mut FoldStorage<'_>,
    work: &mut SessionWork,
    at: &str,
) -> Result<(), Refusal> {
    let timestamp = record_timestamp(record);
    match event {
        Event::Session(_) | Event::SignOut(_) => {}
        Event::Prose(prose) => reduce_prose(record, prose, storage, work, at)?,
        Event::StreamedProse(_) => {}
        Event::Reasoning(_) => {
            let index = ensure_turn(
                storage.dataset,
                record,
                &mut storage.state.current,
                work,
                at,
            )?;
            touch_turn(storage.dataset, index, timestamp, work, at)?;
        }
        Event::Tool(tool) => reduce_tool(record, tool, storage, work, at)?,
        Event::Usage(_) => {}
        Event::Compaction(compaction) => {
            let marker = Marker {
                source: record.source.clone(),
                position: record.line,
                ts: timestamp.to_owned(),
                summary: compaction.summary.clone().unwrap_or_default(),
            };
            work.admit_retained_text(marker_text_bytes(&marker), at)?;
            storage.markers.push(marker);
        }
        Event::Failure(_) => {
            let index = ensure_turn(
                storage.dataset,
                record,
                &mut storage.state.current,
                work,
                at,
            )?;
            storage.dataset.turns[index].error_count =
                storage.dataset.turns[index].error_count.saturating_add(1);
            touch_turn(storage.dataset, index, timestamp, work, at)?;
        }
    }
    Ok(())
}

fn reduce_prose(
    record: &TypedRecord,
    prose: &listen::Prose,
    storage: &mut FoldStorage<'_>,
    work: &mut SessionWork,
    at: &str,
) -> Result<(), Refusal> {
    let index = ensure_turn(
        storage.dataset,
        record,
        &mut storage.state.current,
        work,
        at,
    )?;
    let side = match prose.role {
        ProseRole::Operator => "user",
        ProseRole::Assistant => "assistant",
    };
    let phase = if prose.role == ProseRole::Operator {
        "prompt"
    } else if prose.phase.as_deref() == Some("final_answer")
        || record.context.turn == Some(TurnMoment::Completed)
    {
        "final_answer"
    } else {
        prose.phase.as_deref().unwrap_or("commentary")
    };
    if phase != "final_answer" || !last_final_matches(storage.dataset, index, &prose.text) {
        push_message(
            storage.dataset,
            index,
            record,
            (side, phase, &prose.text),
            work,
            at,
        )?;
    }
    Ok(())
}

fn reduce_tool(
    record: &TypedRecord,
    tool: &listen::Tool,
    storage: &mut FoldStorage<'_>,
    work: &mut SessionWork,
    at: &str,
) -> Result<(), Refusal> {
    match &tool.moment {
        ToolMoment::Call { .. } => {
            let index = ensure_turn(
                storage.dataset,
                record,
                &mut storage.state.current,
                work,
                at,
            )?;
            register_tool_call(
                storage.dataset,
                index,
                record,
                tool,
                &mut storage.state.pending,
                work,
                at,
            )
        }
        ToolMoment::Update { .. } => {
            if let Some(pending) = storage.state.pending.get(&tool.call_id) {
                touch_turn(
                    storage.dataset,
                    pending.turn,
                    record_timestamp(record),
                    work,
                    at,
                )?;
            }
            Ok(())
        }
        ToolMoment::Result(_) => complete_tool_call(
            storage.dataset,
            record,
            tool,
            &mut storage.state.pending,
            storage.residues,
            work,
            at,
        ),
    }
}

pub(crate) fn finish_fold(
    dataset: &mut FoldBuffer,
    state: FoldState,
    work: &mut SessionWork,
    at: &str,
) -> Result<(), Refusal> {
    if let Some(index) = state.current {
        let end = dataset.turns[index].last_activity_ts.clone();
        finish_turn(dataset, index, &end, false, work, at)?;
    }
    Ok(())
}

fn record_timestamp(record: &TypedRecord) -> &str {
    record
        .context
        .timestamp
        .as_deref()
        .expect("typed session records require normalized timestamps")
}

pub(crate) fn source_view_text_bytes(source: &SourceView) -> u64 {
    text_bytes(
        [source.path.as_str(), source.name.as_str()]
            .into_iter()
            .chain(source.thread.iter().map(String::as_str)),
    )
}

fn marker_text_bytes(marker: &Marker) -> u64 {
    text_bytes([
        marker.source.as_str(),
        marker.ts.as_str(),
        marker.summary.as_str(),
    ])
}

fn turn_text_bytes(turn: &TurnRecord) -> u64 {
    text_bytes(
        [
            turn.source.as_str(),
            turn.start_ts.as_str(),
            turn.last_activity_ts.as_str(),
        ]
        .into_iter()
        .chain(turn.turn_id.iter().map(String::as_str))
        .chain(turn.end_ts.iter().map(String::as_str)),
    )
}

fn message_text_bytes(message: &MessageRecord) -> u64 {
    text_bytes(
        [
            message.source.as_str(),
            message.ts.as_str(),
            message.side.as_str(),
            message.phase.as_str(),
            message.text.as_str(),
        ]
        .into_iter()
        .chain(message.turn_id.iter().map(String::as_str))
        .chain(message.authored.iter().map(String::as_str)),
    )
}

fn compaction_text_bytes(compaction: &CompactionRecord) -> u64 {
    text_bytes([
        compaction.source.as_str(),
        compaction.ts.as_str(),
        compaction.assistant_before.as_str(),
        compaction.summary_after.as_str(),
        compaction.intent.as_str(),
        compaction.user_after.as_str(),
    ])
}

fn observe_turn(
    dataset: &mut FoldBuffer,
    record: &TypedRecord,
    current: Option<usize>,
    work: &mut SessionWork,
    at: &str,
) -> Result<usize, Refusal> {
    if let Some(turn_id) = record.context.turn_id.as_deref() {
        if let Some(index) = current
            && dataset.turns[index].turn_id.as_deref() == Some(turn_id)
        {
            return Ok(index);
        }
        if let Some(index) = current {
            finish_turn(dataset, index, record_timestamp(record), false, work, at)?;
        }
        if let Some(index) = dataset.turns.iter().rposition(|turn| {
            turn.source == record.source
                && turn.turn_id.as_deref() == Some(turn_id)
                && !turn.completed
        }) {
            return Ok(index);
        }
        return start_turn(dataset, record, Some(turn_id), work, at);
    }
    current.map_or_else(|| start_turn(dataset, record, None, work, at), Ok)
}

fn start_turn(
    dataset: &mut FoldBuffer,
    record: &TypedRecord,
    turn_id: Option<&str>,
    work: &mut SessionWork,
    at: &str,
) -> Result<usize, Refusal> {
    let timestamp = record_timestamp(record);
    let index = dataset.turns.len();
    let turn = TurnRecord {
        source: record.source.clone(),
        vendor: record.vendor,
        turn_id: turn_id.map(str::to_owned),
        start_ts: timestamp.to_owned(),
        end_ts: None,
        last_activity_ts: timestamp.to_owned(),
        completed: false,
        messages: Vec::new(),
        tool_calls: BTreeMap::new(),
        touched_files: Default::default(),
        command_count: 0,
        patch_count: 0,
        error_count: 0,
        web_search_count: 0,
        compaction_count: 0,
    };
    work.admit_retained_text(turn_text_bytes(&turn), at)?;
    dataset.turns.push(turn);
    Ok(index)
}

fn ensure_turn(
    dataset: &mut FoldBuffer,
    record: &TypedRecord,
    current: &mut Option<usize>,
    work: &mut SessionWork,
    at: &str,
) -> Result<usize, Refusal> {
    if let Some(turn_id) = record.context.turn_id.as_deref()
        && let Some(index) = dataset.turns.iter().rposition(|turn| {
            turn.source == record.source
                && turn.turn_id.as_deref() == Some(turn_id)
                && !turn.completed
        })
    {
        *current = Some(index);
        return Ok(index);
    }
    if let Some(index) = *current {
        return Ok(index);
    }
    let index = start_turn(dataset, record, record.context.turn_id.as_deref(), work, at)?;
    *current = Some(index);
    Ok(index)
}

fn finish_turn(
    dataset: &mut FoldBuffer,
    index: usize,
    ts: &str,
    completed: bool,
    work: &mut SessionWork,
    at: &str,
) -> Result<(), Refusal> {
    work.admit_retained_text(text_bytes([ts, ts]), at)?;
    let turn = &mut dataset.turns[index];
    turn.end_ts = Some(ts.to_owned());
    turn.last_activity_ts = ts.to_owned();
    turn.completed = completed;
    Ok(())
}

fn touch_turn(
    dataset: &mut FoldBuffer,
    index: usize,
    ts: &str,
    work: &mut SessionWork,
    at: &str,
) -> Result<(), Refusal> {
    work.admit_retained_text(text_bytes([ts]), at)?;
    dataset.turns[index].last_activity_ts = ts.to_owned();
    Ok(())
}

fn last_final_matches(dataset: &FoldBuffer, index: usize, text: &str) -> bool {
    dataset.turns[index]
        .messages
        .iter()
        .rev()
        .find(|message| message.side == "assistant" && message.phase == "final_answer")
        .is_some_and(|message| message.text == text)
}

fn push_message(
    dataset: &mut FoldBuffer,
    index: usize,
    record: &TypedRecord,
    message: (&str, &str, &str),
    work: &mut SessionWork,
    at: &str,
) -> Result<(), Refusal> {
    let (side, phase, text) = message;
    if text.trim().is_empty() {
        return Ok(());
    }
    let timestamp = record_timestamp(record);
    let classified = classify(text);
    let message = MessageRecord {
        source: record.source.clone(),
        position: record.line,
        ts: timestamp.to_owned(),
        turn_id: dataset.turns[index].turn_id.clone(),
        side: side.to_owned(),
        phase: phase.to_owned(),
        text: text.to_owned(),
        shape: classified.shape,
        authored: classified.authored,
    };
    if dataset.turns[index].messages.iter().any(|row| {
        row.ts == message.ts
            && row.side == message.side
            && row.phase == message.phase
            && row.text == message.text
    }) {
        return Ok(());
    }
    work.admit_retained_text(message_text_bytes(&message).saturating_mul(2), at)?;
    dataset.turns[index].messages.push(message.clone());
    dataset.messages.push(message);
    touch_turn(dataset, index, timestamp, work, at)
}

fn register_tool_call(
    dataset: &mut FoldBuffer,
    index: usize,
    record: &TypedRecord,
    tool: &listen::Tool,
    pending: &mut BTreeMap<String, PendingTool>,
    work: &mut SessionWork,
    at: &str,
) -> Result<(), Refusal> {
    let ToolMoment::Call {
        name, arguments, ..
    } = &tool.moment
    else {
        unreachable!("tool-call reduction passes only invocation events")
    };
    let name = name.as_str();
    let arguments = arguments.as_str();
    let call_id = tool.call_id.as_str();
    if pending.contains_key(call_id) {
        return Err(refusal(
            "session::tool-invocation-duplicate",
            Some(call_id.to_owned()),
            "restore one invocation per vendor call identity",
        ));
    }
    if !dataset.turns[index].tool_calls.contains_key(name) {
        work.admit_retained_text(text_bytes([name]), at)?;
    }
    *dataset.turns[index]
        .tool_calls
        .entry(name.to_owned())
        .or_default() += 1;
    if matches!(name, "edit" | "write") {
        dataset.turns[index].patch_count = dataset.turns[index].patch_count.saturating_add(1);
        for path in tool_paths(arguments) {
            if !dataset.turns[index].touched_files.contains(&path) {
                work.admit_retained_text(text_bytes([path.as_str()]), at)?;
            }
            dataset.turns[index].touched_files.insert(path);
        }
    }
    if name == "web_search" {
        dataset.turns[index].web_search_count =
            dataset.turns[index].web_search_count.saturating_add(1);
    }
    if matches!(name, "bash" | vendor::omp::SHELL_TOOL) {
        dataset.turns[index].command_count = dataset.turns[index].command_count.saturating_add(1);
    }
    work.admit_retained_text(text_bytes([call_id]), at)?;
    pending.insert(call_id.to_owned(), PendingTool { turn: index });
    touch_turn(dataset, index, record_timestamp(record), work, at)
}

fn tool_paths(arguments: &str) -> Vec<String> {
    let parsed = serde_json::from_str::<Value>(arguments).unwrap_or(Value::Null);
    parsed
        .get("path")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .into_iter()
        .collect()
}

fn complete_tool_call(
    dataset: &mut FoldBuffer,
    record: &TypedRecord,
    tool: &listen::Tool,
    pending: &mut BTreeMap<String, PendingTool>,
    residues: &mut ResidueLedger,
    work: &mut SessionWork,
    at: &str,
) -> Result<(), Refusal> {
    let ToolMoment::Result(outcome) = &tool.moment else {
        unreachable!("tool-result reduction passes only completion events")
    };
    let call_id = tool.call_id.as_str();
    let Some(pending) = pending.remove(call_id) else {
        return residues.push(
            Residue {
            source: record.source.clone(),
            line: record.line,
            kind: "tool-invocation-missing".to_owned(),
            detail: format!(
                "call_id={call_id} outcome was not attributed because no matching invocation preceded it"
            ),
            recovery: "restore the matching invocation to account this outcome; independent session evidence remains available"
                .to_owned(),
            },
            work,
            at,
        );
    };
    let failed =
        outcome.result == ToolResult::Failed || outcome.exit_code.is_some_and(|code| code != 0);
    if failed {
        dataset.turns[pending.turn].error_count =
            dataset.turns[pending.turn].error_count.saturating_add(1);
    }
    touch_turn(dataset, pending.turn, record_timestamp(record), work, at)
}

fn compaction_intent(summary: &str) -> String {
    if summary.is_empty() {
        return UNRECOVERABLE_REHYDRATION_INTENT.to_owned();
    }
    rehydration_intent(summary)
}

fn refusal(reason: &str, observed: Option<String>, detail: &str) -> Refusal {
    Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason.to_owned(),
            Some("session-transcript".to_owned()),
            Some("one current typed transcript evidence stream".to_owned()),
            observed,
        )
        .with_detail(detail),
    )
}
