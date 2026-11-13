//! One resumable, generation-authenticated stream over complete tape records.
//!
//! A checkpoint names the next complete-record boundary, never merely the last
//! byte read. Complete typed records and complete residue both advance it;
//! bytes after the final delimiter remain pending and leave it unchanged.

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tape::{Locus, SourceId, Tape};

use crate::{Decoded, Dialect, Event, LocusLengthError, RecordContext, Residue, decode};

mod search;
mod tail;

pub use search::{TailSearch, search_tail};

pub use tail::{
    BOUNDARY_CHUNK_BYTES, TailBudget, TailBudgetError, TailScan, TailWork, scan_before, scan_tail,
};

/// Member bytes one forward scan reads at once.
const SCAN_CHUNK_BYTES: usize = 1 << 20;

/// Serializable spelling of one durable tape span.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Span {
    /// Tape member.
    pub member: u64,
    /// First byte.
    pub offset: u64,
    /// Number of bytes.
    pub length: u64,
}

impl From<Locus> for Span {
    fn from(locus: Locus) -> Self {
        Self {
            member: locus.member,
            offset: locus.offset,
            length: locus.length,
        }
    }
}

impl From<Span> for Locus {
    fn from(span: Span) -> Self {
        Self::new(span.member, span.offset, span.length)
    }
}

impl fmt::Display for Span {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}:{}", self.member, self.offset, self.length)
    }
}

/// Position of the next complete record in one source generation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    /// Member containing the next record's first byte.
    pub member: u64,
    /// Offset of that byte within the member.
    pub offset: u64,
}

impl Position {
    /// First byte of a tape generation.
    #[must_use]
    pub const fn start() -> Self {
        Self {
            member: 1,
            offset: 0,
        }
    }

    const fn is_start(self) -> bool {
        self.member == 1 && self.offset == 0
    }
}

/// Durable resume checkpoint for one tape generation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    /// Opaque tape generation.
    pub source: SourceId,
    /// Next complete-record boundary.
    pub next: Position,
}

impl Checkpoint {
    /// Beginning of one source generation.
    #[must_use]
    pub const fn start(source: SourceId) -> Self {
        Self {
            source,
            next: Position::start(),
        }
    }
}

/// One public typed event with its durable identity coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourcedEvent {
    /// Zero-based event position within the decoded vendor record.
    pub ordinal: u64,
    /// Exact physical tape spans supplying the record.
    pub spans: Vec<Span>,
    /// Context shared by every event from this vendor record.
    pub context: RecordContext,
    /// Vendor-neutral event.
    pub event: Event,
}

/// One complete record that remained untyped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourcedResidue {
    /// Exact physical tape spans supplying the record.
    pub spans: Vec<Span>,
    /// Accounted decoder result.
    pub residue: Residue,
}

/// Public result of one complete record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordPayload {
    /// Zero or more typed events in vendor-record order.
    Events(Vec<SourcedEvent>),
    /// The complete record remained residue.
    Residue(SourcedResidue),
}

/// One complete record and the checkpoint immediately after it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Record {
    /// Context decoded from the complete record, including zero-event records.
    pub context: Option<RecordContext>,
    /// Stable vendor-record locus used by context-only consumers.
    pub locus: Locus,
    /// Typed or accounted record payload.
    pub payload: RecordPayload,
    /// Next complete-record boundary after this record.
    pub next: Checkpoint,
}

/// One resumable scan result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Scan {
    /// Every complete record after the supplied checkpoint.
    pub records: Vec<Record>,
    /// Boundary after the last complete record.
    pub checkpoint: Checkpoint,
    /// Exact trailing bytes awaiting a delimiter.
    pub pending: Vec<Span>,
    /// Whether the tape currently contains no readable member.
    pub extents_empty: bool,
}

/// Resume a tape scan from an optional durable checkpoint.
///
/// A checkpoint from another generation is admitted only at byte zero. A
/// same-generation boundary that retreats beyond observed bytes or skips a
/// member is refused. Missing, unreadable, or malformed source identity is a
/// tape error rather than an invitation to infer identity from the path.
///
/// # Errors
///
/// Returns [`ScanError`] when the tape cannot be opened/read, the checkpoint
/// does not continue the observed generation, or a decoder contract fails.
pub fn scan(
    root: impl AsRef<Path>,
    dialect: Dialect,
    checkpoint: Option<&Checkpoint>,
) -> Result<Scan, ScanError> {
    let mut records = Vec::new();
    let end = scan_each(root, dialect, checkpoint, |record| {
        records.push(record);
        Ok::<_, ScanError>(())
    })?;
    Ok(Scan {
        records,
        checkpoint: end.checkpoint,
        pending: end.pending,
        extents_empty: end.extents_empty,
    })
}

/// Where a forward scan stopped after handing over every complete record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanEnd {
    /// Boundary after the last complete record.
    pub checkpoint: Checkpoint,
    /// Exact trailing bytes awaiting a delimiter.
    pub pending: Vec<Span>,
    /// Whether the tape currently contains no readable member.
    pub extents_empty: bool,
}

/// Resume a tape scan and hand each complete record to `visit` as it decodes.
///
/// The checkpoint contract is [`scan`]'s. Member bytes are read forward in
/// fixed chunks, so memory holds one chunk, one partial record, and the
/// records one chunk completes, whatever the tape's size.
///
/// # Errors
///
/// Returns [`scan`]'s failures, converted into `E`, and the first failure
/// `visit` reports; records already visited stay visited.
pub fn scan_each<E: From<ScanError>>(
    root: impl AsRef<Path>,
    dialect: Dialect,
    checkpoint: Option<&Checkpoint>,
    mut visit: impl FnMut(Record) -> Result<(), E>,
) -> Result<ScanEnd, E> {
    let root = root.as_ref();
    let tape_error = |source| ScanError::Tape {
        path: root.to_path_buf(),
        source,
    };
    let tape = Tape::open_existing(root).map_err(tape_error)?;
    let source = tape.source_id().map_err(tape_error)?;
    let mut extents = tape.extents().map_err(tape_error)?;
    let mut position = resume_position(checkpoint, &source)?;
    position = match validate_position(position, &extents) {
        Ok(position) => position,
        Err(_) => {
            // A seal publishes its member and index before removing plaintext.
            // One extent observation can therefore miss the active file while
            // still holding the predecessor index. A fresh observation either
            // sees that completed seal or repeats the continuity violation.
            extents = tape.extents().map_err(tape_error)?;
            validate_position(position, &extents)?
        }
    };
    let mut state = ScanState {
        position,
        records: Vec::new(),
        bytes: Vec::new(),
        spans: Vec::new(),
    };
    let mut buffer = vec![0; SCAN_CHUNK_BYTES];
    for extent in &extents {
        let Some((start, end)) = unread_range(extent, None, &mut state) else {
            continue;
        };
        let mut offset = start;
        tape.stream_extent(
            Locus::new(extent.member, start, end - start),
            *extent,
            &mut buffer,
            |chunk| {
                feed(
                    extent,
                    offset,
                    chunk,
                    dialect,
                    &source,
                    &mut state,
                    &mut || Ok(()),
                )
                .map_err(|error| Streamed::Visit(error.into()))?;
                offset += chunk.len() as u64;
                state
                    .records
                    .drain(..)
                    .try_for_each(&mut visit)
                    .map_err(Streamed::Visit)
            },
        )
        .map_err(|stop| match stop {
            Streamed::Tape(source) => tape_error(source).into(),
            Streamed::Visit(error) => error,
        })?;
    }

    let observed_source = tape.source_id().map_err(tape_error)?;
    if observed_source != source {
        return Err(ScanError::SourceChanged {
            expected: source,
            observed: observed_source,
            next: state.position,
        }
        .into());
    }
    Ok(ScanEnd {
        checkpoint: Checkpoint {
            source,
            next: state.position,
        },
        pending: state.spans,
        extents_empty: extents.is_empty(),
    })
}

/// Why one streamed member read stopped early.
enum Streamed<E> {
    Tape(tape::Error),
    Visit(E),
}

impl<E> From<tape::Error> for Streamed<E> {
    fn from(error: tape::Error) -> Self {
        Self::Tape(error)
    }
}

struct ScanState {
    position: Position,
    records: Vec<Record>,
    bytes: Vec<u8>,
    spans: Vec<Span>,
}

/// The member bytes a scan at `state.position` still has to read, up to
/// `end`, or `None` when this extent has nothing left.
///
/// A fully consumed sealed member advances the position to its successor.
fn unread_range(
    extent: &tape::Extent,
    end: Option<Position>,
    state: &mut ScanState,
) -> Option<(u64, u64)> {
    if extent.member < state.position.member {
        return None;
    }
    let start = if extent.member == state.position.member {
        state.position.offset
    } else {
        0
    };
    let extent_end = end
        .filter(|end| end.member == extent.member)
        .map_or(extent.length, |end| end.offset.min(extent.length));
    if start == extent_end {
        if end.is_none() && extent.sealed && state.bytes.is_empty() {
            state.position = Position {
                member: extent.member + 1,
                offset: 0,
            };
        }
        return None;
    }
    Some((start, extent_end))
}

/// Split one contiguous fragment starting at `start` into complete records,
/// holding bytes after the last delimiter for the next fragment.
fn feed(
    extent: &tape::Extent,
    start: u64,
    fragment: &[u8],
    dialect: Dialect,
    source: &SourceId,
    state: &mut ScanState,
    admit_record: &mut impl FnMut() -> Result<(), ScanError>,
) -> Result<(), ScanError> {
    let mut fragment_start = 0_usize;
    for (index, byte) in fragment.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        let end = index + 1;
        append_fragment(
            &mut state.bytes,
            &mut state.spans,
            extent.member,
            start + fragment_start as u64,
            &fragment[fragment_start..end],
        );
        state.position = next_position(extent, start + end as u64);
        state.finish_record(dialect, source, admit_record)?;
        fragment_start = end;
    }
    if fragment_start < fragment.len() {
        append_fragment(
            &mut state.bytes,
            &mut state.spans,
            extent.member,
            start + fragment_start as u64,
            &fragment[fragment_start..],
        );
    }
    Ok(())
}

impl ScanState {
    fn finish_record(
        &mut self,
        dialect: Dialect,
        source: &SourceId,
        admit_record: &mut impl FnMut() -> Result<(), ScanError>,
    ) -> Result<(), ScanError> {
        admit_record()?;
        let locus = synthetic_locus(&self.spans, self.bytes.len());
        let decoded = decode(dialect, &self.bytes, locus).map_err(ScanError::DecodeContract)?;
        self.records.push(Record {
            context: decoded.context().cloned(),
            locus,
            payload: source_decoded(decoded, &self.spans),
            next: Checkpoint {
                source: source.clone(),
                next: self.position,
            },
        });
        self.bytes.clear();
        self.spans.clear();
        Ok(())
    }
}

fn next_position(extent: &tape::Extent, offset: u64) -> Position {
    if extent.sealed && offset == extent.length {
        Position {
            member: extent.member + 1,
            offset: 0,
        }
    } else {
        Position {
            member: extent.member,
            offset,
        }
    }
}

fn resume_position(
    checkpoint: Option<&Checkpoint>,
    source: &SourceId,
) -> Result<Position, ScanError> {
    let Some(checkpoint) = checkpoint else {
        return Ok(Position::start());
    };
    if checkpoint.source == *source {
        return Ok(checkpoint.next);
    }
    if checkpoint.next.is_start() {
        return Ok(Position::start());
    }
    Err(ScanError::SourceChanged {
        expected: checkpoint.source.clone(),
        observed: source.clone(),
        next: checkpoint.next,
    })
}

fn validate_position(
    mut position: Position,
    extents: &[tape::Extent],
) -> Result<Position, ScanError> {
    let Some(last) = extents.last() else {
        return if position.is_start() {
            Ok(position)
        } else {
            Err(ScanError::CheckpointGap {
                next: position,
                observed_next_member: 1,
            })
        };
    };

    let extent = position
        .member
        .checked_sub(1)
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| extents.get(index))
        .filter(|extent| extent.member == position.member);
    if let Some(extent) = extent {
        if position.offset > extent.length {
            return Err(ScanError::CheckpointRetreated {
                next: position,
                observed_length: extent.length,
            });
        }
        if extent.sealed && position.offset == extent.length {
            position = Position {
                member: position.member + 1,
                offset: 0,
            };
        }
        return Ok(position);
    }

    let observed_next_member = last.member + u64::from(last.sealed);
    if last.sealed && position.member == observed_next_member && position.offset == 0 {
        Ok(position)
    } else {
        Err(ScanError::CheckpointGap {
            next: position,
            observed_next_member,
        })
    }
}

fn source_decoded(decoded: Decoded, spans: &[Span]) -> RecordPayload {
    match decoded {
        Decoded::Events { context, events } => RecordPayload::Events(
            events
                .into_iter()
                .enumerate()
                .map(|(ordinal, event)| SourcedEvent {
                    ordinal: ordinal as u64,
                    spans: spans.to_vec(),
                    context: RecordContext::clone(&context),
                    event,
                })
                .collect(),
        ),
        Decoded::Residue(residue) => RecordPayload::Residue(SourcedResidue {
            spans: spans.to_vec(),
            residue,
        }),
    }
}

fn append_fragment(
    record: &mut Vec<u8>,
    spans: &mut Vec<Span>,
    member: u64,
    offset: u64,
    fragment: &[u8],
) {
    if fragment.is_empty() {
        return;
    }
    record.extend_from_slice(fragment);
    let length = fragment.len() as u64;
    if let Some(last) = spans.last_mut()
        && last.member == member
        && last.offset.checked_add(last.length) == Some(offset)
    {
        last.length += length;
    } else {
        spans.push(Span {
            member,
            offset,
            length,
        });
    }
}

fn synthetic_locus(spans: &[Span], length: usize) -> Locus {
    let first = spans
        .first()
        .expect("a record with bytes has at least one source span");
    Locus::new(first.member, first.offset, length as u64)
}

/// A resumable scan could not continue from its durable contract.
#[derive(Debug)]
pub enum ScanError {
    /// Tape identity, extents, or member bytes could not be read.
    Tape {
        /// Tape involved.
        path: PathBuf,
        /// Typed tape failure.
        source: tape::Error,
    },
    /// Another source generation was observed after a nonzero checkpoint.
    SourceChanged {
        /// Generation named by the checkpoint or observed before scanning.
        expected: SourceId,
        /// Generation now published at the path.
        observed: SourceId,
        /// Nonzero position that cannot be transferred between generations.
        next: Position,
    },
    /// Same-generation bytes retreated behind the checkpoint.
    CheckpointRetreated {
        /// Boundary being resumed.
        next: Position,
        /// Bytes now observed in that member.
        observed_length: u64,
    },
    /// The checkpoint skips the next member the tape can supply.
    CheckpointGap {
        /// Boundary being resumed.
        next: Position,
        /// Next member the observed tape can supply.
        observed_next_member: u64,
    },
    /// A caller-owned scan allowance was exhausted before more work began.
    WorkLimit {
        /// Dimension whose allowance was exhausted.
        dimension: &'static str,
        /// Maximum admitted work in this dimension.
        limit: u64,
        /// Work consumed before refusal.
        consumed: u64,
        /// Complete work counters at the refusal boundary.
        work: TailWork,
    },
    /// Decoder received a synthetic locus of the wrong length.
    DecodeContract(LocusLengthError),
}

impl fmt::Display for ScanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tape { path, source } => {
                write!(
                    formatter,
                    "failed to read tape at {}: {source}",
                    path.display()
                )
            }
            Self::SourceChanged {
                expected,
                observed,
                next,
            } => write!(
                formatter,
                "tape source changed from {expected} to {observed} after checkpoint {}:{}",
                next.member, next.offset
            ),
            Self::CheckpointRetreated {
                next,
                observed_length,
            } => write!(
                formatter,
                "tape member {} retreated from checkpoint {} to {observed_length}",
                next.member, next.offset
            ),
            Self::CheckpointGap {
                next,
                observed_next_member,
            } => write!(
                formatter,
                "checkpoint {}:{} skips observed next member {observed_next_member}",
                next.member, next.offset
            ),
            Self::WorkLimit {
                dimension,
                limit,
                consumed,
                work,
            } => write!(
                formatter,
                "tape scan exhausted {dimension} allowance {limit} after consuming {consumed}; work: {} source bytes, {} records, {} passes, {:?}",
                work.source_bytes, work.records, work.passes, work.duration,
            ),
            Self::DecodeContract(error) => write!(formatter, "decoder contract failed: {error}"),
        }
    }
}

impl ScanError {
    /// Complete tail work counters when a caller-owned allowance was exhausted.
    #[must_use]
    pub const fn tail_work(&self) -> Option<&TailWork> {
        match self {
            Self::WorkLimit { work, .. } => Some(work),
            _ => None,
        }
    }
}

impl Error for ScanError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Tape { source, .. } => Some(source),
            Self::DecodeContract(error) => Some(error),
            Self::SourceChanged { .. }
            | Self::CheckpointRetreated { .. }
            | Self::CheckpointGap { .. }
            | Self::WorkLimit { .. } => None,
        }
    }
}
