use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use tape::SourceId;
use wire::output::{Admission, Bounded, Budget, Resume};

use super::{Observation, SurfaceError, SurfaceEvent, SurfaceResidue, SurfaceToolMoment};
use crate::{
    Checkpoint, Dialect, EventKind, Position, Record, RecordPayload, Span, TailBudget, TailWork,
    scan_before, scan_tail,
};

/// Stable authenticated boundary used to continue toward older transcript records.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptCursor {
    /// Durable generation of the selected tape.
    pub source: SourceId,
    /// Exclusive end boundary for the next older page.
    pub before: Position,
}

impl fmt::Display for TranscriptCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}:{}",
            self.source, self.before.member, self.before.offset
        )
    }
}

impl FromStr for TranscriptCursor {
    type Err = TranscriptCursorError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (source, position) = text.split_once(':').ok_or(TranscriptCursorError)?;
        let (member, offset) = position.split_once(':').ok_or(TranscriptCursorError)?;
        if offset.contains(':') {
            return Err(TranscriptCursorError);
        }
        Ok(Self {
            source: source.parse().map_err(|_| TranscriptCursorError)?,
            before: Position {
                member: member.parse().map_err(|_| TranscriptCursorError)?,
                offset: offset.parse().map_err(|_| TranscriptCursorError)?,
            },
        })
    }
}

/// A transcript cursor was not in its one canonical spelling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TranscriptCursorError;

impl fmt::Display for TranscriptCursorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("transcript cursor must be SOURCE:MEMBER:OFFSET")
    }
}

impl std::error::Error for TranscriptCursorError {}

/// Which generation-authenticated transcript window a page starts from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TranscriptPageStart {
    /// Read the newest window, optionally requiring the generation previously observed.
    Current { expected: Option<SourceId> },
    /// Read the window immediately before this complete-record boundary.
    Before(TranscriptCursor),
}

/// Event selection for one transcript page. An empty set admits every event kind.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TranscriptFilter {
    kinds: BTreeSet<EventKind>,
}

impl TranscriptFilter {
    /// Select only the supplied event kinds.
    #[must_use]
    pub fn only(kinds: impl IntoIterator<Item = EventKind>) -> Self {
        Self {
            kinds: kinds.into_iter().collect(),
        }
    }

    /// Whether `observation` survives this selection. Only typed events carry
    /// a kind, so every other observation is admitted only by an empty set.
    #[must_use]
    pub fn admits(&self, observation: &Observation) -> bool {
        match observation {
            Observation::Event { event, .. } => {
                self.kinds.is_empty() || self.kinds.contains(&event.kind())
            }
            _ => self.kinds.is_empty(),
        }
    }
}

/// Caller-owned work and presentation limits for one transcript page.
#[derive(Clone, Debug)]
pub struct TranscriptPageRequest {
    /// Hard source-read, decode, pass, and wall-time allowance.
    pub scan: TailBudget,
    /// Aggregate row/encoded-byte and per-text-field allowance.
    pub output: Budget,
    /// Current or older generation-authenticated window to read.
    pub start: TranscriptPageStart,
    /// Event selection applied before presentation admission.
    pub filter: TranscriptFilter,
}

/// One complete vendor record and every visible observation it decoded to.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptRecord {
    /// Boundary immediately before this record.
    ///
    /// After exact export this continues with strictly older records rather
    /// than offering the exported record to presentation admission again.
    pub start: TranscriptCursor,
    /// Boundary that re-includes this record on the next older page.
    pub end: TranscriptCursor,
    /// Observations emitted from the record, kept atomically for pagination.
    pub observations: Vec<TranscriptObservation>,
}

/// Current non-record state admitted as one atomic page row.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptStatus {
    /// Unterminated bytes at this window's end.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<Span>,
    /// Event kinds absent from the decoded window.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub canaries: Vec<EventKind>,
}

/// One aggregate-budgeted row in a transcript page.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TranscriptPageRow {
    /// Current pending-byte and decoder-canary state.
    Status { status: TranscriptStatus },
    /// One complete vendor record.
    Record { record: TranscriptRecord },
}

/// One observation after per-field text clipping.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptObservation {
    /// Existing typed transcript evidence.
    pub observation: Observation,
    /// Exact UTF-8 bytes withheld across this observation's text fields.
    pub withheld_text_bytes: usize,
}

impl TranscriptObservation {
    /// `observation` with every text field cut to `budget`'s text limit.
    #[must_use]
    pub fn clipped(mut observation: Observation, budget: Budget) -> Self {
        let mut withheld = 0;
        match &mut observation {
            Observation::Event { event, .. } => clip_event(event, budget, &mut withheld),
            Observation::Residue { residue, .. } => clip_residue(residue, budget, &mut withheld),
            Observation::Missing { selector, detail } => {
                clip_field(selector, budget, &mut withheld);
                clip_field(detail, budget, &mut withheld);
            }
            Observation::Absent { selector } => clip_field(selector, budget, &mut withheld),
            Observation::Ambiguous { selector, .. } => {
                clip_field(selector, budget, &mut withheld);
            }
            Observation::Pending { .. } | Observation::Canary { .. } => {}
        }
        Self {
            observation,
            withheld_text_bytes: withheld,
        }
    }
}

/// How a presentation omission can be recovered without flooding stdout.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TranscriptContinuation {
    /// Re-read the newest window only while its tape generation remains current.
    Current { source: SourceId },
    /// Continue from an authenticated complete-record boundary.
    Cursor { cursor: TranscriptCursor },
    /// Export the complete oversized row, then follow its authenticated continuation.
    Export {
        spans: Vec<Span>,
        continuation: Box<TranscriptContinuation>,
    },
}

impl fmt::Display for TranscriptContinuation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Current { source } => write!(formatter, "current generation {source}"),
            Self::Cursor { cursor } => write!(formatter, "cursor {cursor}"),
            Self::Export {
                spans,
                continuation,
            } => write!(
                formatter,
                "exact export for spans {}; then {continuation}",
                spans
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        }
    }
}

/// Older source bytes outside the newest bounded scan window.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptHistory {
    /// Exact plaintext bytes preceding this page's scan window.
    pub withheld_source_bytes: u64,
    /// Authenticated exclusive boundary for the next older scan.
    pub continuation: TranscriptCursor,
}

/// Exact bounded work consumed by one page.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptWork {
    pub source_bytes: u64,
    pub records: usize,
    pub passes: usize,
    pub elapsed_millis: u64,
}

impl From<TailWork> for TranscriptWork {
    fn from(work: TailWork) -> Self {
        Self {
            source_bytes: work.source_bytes,
            records: work.records,
            passes: work.passes,
            elapsed_millis: u64::try_from(work.duration.as_millis()).unwrap_or(u64::MAX),
        }
    }
}

/// One bounded, newest-first page over an exact transcript tape.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptPage {
    /// Selected tape.
    pub tape: PathBuf,
    /// On a current page, its status; then complete records in newest-first
    /// order. An older page carries records only, so a continuation that
    /// widens the byte limit for one withheld record admits that record.
    pub rows: Bounded<TranscriptPageRow, TranscriptContinuation>,
    /// Older bytes outside the bounded source window, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<TranscriptHistory>,
    /// Work consumed before presentation admission.
    pub work: TranscriptWork,
}

/// Read one exact transcript tape under hard source and presentation bounds.
///
/// Records are presented newest first. One vendor record is the atomic paging
/// row, so a record that emits several events cannot be split across pages or
/// replay a previously admitted event. Pending bytes and decoder canaries
/// describe the tape's end, so only a current page reports them. A
/// continuation authenticates the tape generation and is refused by the tail
/// scanner after replacement.
///
/// # Errors
///
/// Returns a typed surface error when the tape cannot be read, the source work
/// allowance is exhausted, or a supplied cursor does not belong to the tape.
pub fn transcript_page(
    tape: impl AsRef<Path>,
    lane: impl Into<String>,
    dialect: Dialect,
    request: TranscriptPageRequest,
) -> Result<TranscriptPage, SurfaceError> {
    let tape = tape.as_ref();
    let lane = lane.into();
    let current = matches!(request.start, TranscriptPageStart::Current { .. });
    let tail = match request.start {
        TranscriptPageStart::Before(cursor) => scan_before(
            tape,
            dialect,
            &Checkpoint {
                source: cursor.source,
                next: cursor.before,
            },
            request.scan,
        ),
        TranscriptPageStart::Current { expected } => scan_tail(tape, dialect, request.scan)
            .and_then(|tail| {
                if let Some(expected) = expected.filter(|expected| *expected != tail.start.source) {
                    Err(crate::ScanError::SourceChanged {
                        expected,
                        observed: tail.start.source.clone(),
                        next: tail.start.next,
                    })
                } else {
                    Ok(tail)
                }
            }),
    }
    .map_err(|source| SurfaceError::Scan {
        path: tape.to_path_buf(),
        source,
    })?;

    let source = tail.start.source.clone();
    let mut admission = Admission::new(request.output);
    if current {
        let status = window_status(&tail.scan);
        if !status.pending.is_empty() || !status.canaries.is_empty() {
            admission
                .offer(TranscriptPageRow::Status { status })
                .map_err(|error| SurfaceError::Encode(error.to_string()))?;
        }
    }
    for source_record in tail.scan.records.iter().rev() {
        let Some(record) = page_record(
            &lane,
            dialect,
            tape,
            source_record,
            &request.filter,
            request.output,
        ) else {
            continue;
        };
        admission
            .offer(TranscriptPageRow::Record { record })
            .map_err(|error| SurfaceError::Encode(error.to_string()))?;
    }
    let presentation_closed = admission.is_closed();
    let rows = admission.finish(|resume| resume_continuation(&resume, &source));
    let history =
        (!presentation_closed && tail.withheld_source_bytes > 0).then(|| TranscriptHistory {
            withheld_source_bytes: tail.withheld_source_bytes,
            continuation: TranscriptCursor {
                source: tail.start.source.clone(),
                before: tail.start.next,
            },
        });

    Ok(TranscriptPage {
        tape: tape.to_path_buf(),
        rows,
        history,
        work: tail.work.into(),
    })
}

/// Pending bytes at the window's end and the event kinds it never decoded.
fn window_status(scan: &crate::Scan) -> TranscriptStatus {
    let mut seen = BTreeSet::new();
    for record in &scan.records {
        if let RecordPayload::Events(events) = &record.payload {
            seen.extend(events.iter().map(|event| event.event.kind()));
        }
    }
    TranscriptStatus {
        pending: scan.pending.clone(),
        canaries: EventKind::ALL
            .into_iter()
            .filter(|kind| !seen.contains(kind))
            .collect(),
    }
}

/// One source record as a page row, or nothing when the filter admits none
/// of its observations.
fn page_record(
    lane: &str,
    dialect: Dialect,
    tape: &Path,
    source_record: &Record,
    filter: &TranscriptFilter,
    output: Budget,
) -> Option<TranscriptRecord> {
    let mut observations = record_observations(lane, dialect, tape, source_record);
    observations.retain(|observation| filter.admits(observation));
    if observations.is_empty() {
        return None;
    }
    Some(TranscriptRecord {
        start: TranscriptCursor {
            source: source_record.next.source.clone(),
            before: Position {
                member: source_record.locus.member,
                offset: source_record.locus.offset,
            },
        },
        end: TranscriptCursor {
            source: source_record.next.source.clone(),
            before: source_record.next.next,
        },
        observations: observations
            .into_iter()
            .map(|observation| TranscriptObservation::clipped(observation, output))
            .collect(),
    })
}

/// How a caller recovers the first row a page withheld. A status row resumes
/// against the generation it observed; a record resumes at its own end, or,
/// after exporting it, strictly before it.
fn resume_continuation(
    resume: &Resume<'_, TranscriptPageRow>,
    source: &SourceId,
) -> TranscriptContinuation {
    let current = || TranscriptContinuation::Current {
        source: source.clone(),
    };
    match resume {
        Resume::Export(TranscriptPageRow::Status { status }) => TranscriptContinuation::Export {
            spans: status.pending.clone(),
            continuation: Box::new(current()),
        },
        Resume::Export(TranscriptPageRow::Record { record }) => TranscriptContinuation::Export {
            spans: transcript_record_spans(record),
            continuation: Box::new(TranscriptContinuation::Cursor {
                cursor: record.start.clone(),
            }),
        },
        Resume::At(row) | Resume::Widened { record: row, .. } => match row {
            TranscriptPageRow::Status { .. } => current(),
            TranscriptPageRow::Record { record } => TranscriptContinuation::Cursor {
                cursor: record.end.clone(),
            },
        },
    }
}

fn record_observations(
    lane: &str,
    dialect: Dialect,
    tape: &Path,
    record: &Record,
) -> Vec<Observation> {
    match &record.payload {
        RecordPayload::Events(events) => events
            .iter()
            .map(|sourced| Observation::Event {
                lane: lane.to_owned(),
                dialect,
                tape: tape.to_path_buf(),
                spans: sourced.spans.clone(),
                event: (&sourced.event).into(),
            })
            .collect(),
        RecordPayload::Residue(sourced) => vec![Observation::Residue {
            lane: lane.to_owned(),
            dialect,
            tape: tape.to_path_buf(),
            spans: sourced.spans.clone(),
            residue: (&sourced.residue.kind).into(),
        }],
    }
}

fn transcript_record_spans(record: &TranscriptRecord) -> Vec<Span> {
    record
        .observations
        .iter()
        .map(|observation| observation_spans(&observation.observation))
        .find(|spans| !spans.is_empty())
        .unwrap_or_default()
}

fn clip_event(event: &mut SurfaceEvent, budget: Budget, withheld: &mut usize) {
    match event {
        SurfaceEvent::Session { identity } => clip_field(identity, budget, withheld),
        SurfaceEvent::Prose { text, phase, .. } => {
            clip_field(text, budget, withheld);
            if let Some(phase) = phase {
                clip_field(phase, budget, withheld);
            }
        }
        SurfaceEvent::Reasoning { .. } => {}
        SurfaceEvent::Tool { call_id, moment } => {
            clip_field(call_id, budget, withheld);
            match moment {
                SurfaceToolMoment::Call { name, arguments }
                | SurfaceToolMoment::Update { name, arguments } => {
                    clip_field(name, budget, withheld);
                    clip_field(arguments, budget, withheld);
                }
                SurfaceToolMoment::Result { output, .. } => {
                    if let Some(output) = output {
                        clip_field(output, budget, withheld);
                    }
                }
            }
        }
        SurfaceEvent::Failure { message } => clip_field(message, budget, withheld),
        SurfaceEvent::SignOut { message, login } => {
            clip_field(message, budget, withheld);
            clip_field(login, budget, withheld);
        }
        SurfaceEvent::Usage { .. } | SurfaceEvent::Compaction {} => {}
    }
}

fn clip_residue(residue: &mut SurfaceResidue, budget: Budget, withheld: &mut usize) {
    match residue {
        SurfaceResidue::Absent { field } => clip_field(field, budget, withheld),
        SurfaceResidue::Unknown { discriminator }
        | SurfaceResidue::VersionDrift { discriminator } => {
            clip_field(discriminator, budget, withheld);
        }
        SurfaceResidue::Malformed { field, expected } => {
            clip_field(field, budget, withheld);
            clip_field(expected, budget, withheld);
        }
        SurfaceResidue::Truncated {} => {}
    }
}

fn clip_field(field: &mut String, budget: Budget, withheld: &mut usize) {
    let clipped = budget.clip(field);
    *withheld = withheld.saturating_add(clipped.withheld_bytes.map_or(0, |count| count.get()));
    *field = clipped.text;
}

fn observation_spans(observation: &Observation) -> Vec<Span> {
    match observation {
        Observation::Event { spans, .. }
        | Observation::Residue { spans, .. }
        | Observation::Pending { spans, .. } => spans.clone(),
        Observation::Absent { .. }
        | Observation::Missing { .. }
        | Observation::Ambiguous { .. }
        | Observation::Canary { .. } => Vec::new(),
    }
}
