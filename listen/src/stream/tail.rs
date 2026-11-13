//! Bounded newest-record windows over a generation-authenticated tape.

use std::num::{NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tape::{Extent, Locus, SourceId, Tape};

use super::{
    Checkpoint, Position, Scan, ScanError, ScanState, feed, unread_range, validate_position,
};
use crate::Dialect;

/// Bytes inspected at once while locating the preceding complete-record
/// boundary, and so the granularity of the pass allowance a retreat spends.
///
/// A caller sizing [`TailBudget`] passes cannot do it blind: retreating over
/// one long record costs one pass per chunk of it.
pub const BOUNDARY_CHUNK_BYTES: u64 = 64 * 1024;

/// Validated hard work allowance for one tail or history-window read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TailBudget {
    window_bytes: NonZeroU64,
    source_bytes: NonZeroU64,
    records: NonZeroUsize,
    passes: NonZeroUsize,
    duration: Duration,
}

impl TailBudget {
    /// Construct one positive allowance.
    ///
    /// # Errors
    ///
    /// Refuses any zero dimension, a zero wall duration, or a window larger
    /// than the total source-byte allowance.
    pub fn new(
        window_bytes: u64,
        source_bytes: u64,
        records: usize,
        passes: usize,
        duration: Duration,
    ) -> Result<Self, TailBudgetError> {
        let window_bytes = NonZeroU64::new(window_bytes)
            .ok_or(TailBudgetError("window bytes must be positive"))?;
        let source_bytes = NonZeroU64::new(source_bytes)
            .ok_or(TailBudgetError("source bytes must be positive"))?;
        let records = NonZeroUsize::new(records)
            .ok_or(TailBudgetError("decoded records must be positive"))?;
        let passes =
            NonZeroUsize::new(passes).ok_or(TailBudgetError("scan passes must be positive"))?;
        let duration = (!duration.is_zero())
            .then_some(duration)
            .ok_or(TailBudgetError("wall duration must be positive"))?;
        if window_bytes > source_bytes {
            return Err(TailBudgetError(
                "window bytes cannot exceed source-byte maximum",
            ));
        }
        Ok(Self {
            window_bytes,
            source_bytes,
            records,
            passes,
            duration,
        })
    }

    pub(super) const fn decode_window(self) -> u64 {
        self.window_bytes.get()
    }
}

/// Why a tail allowance could not be constructed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TailBudgetError(&'static str);

impl std::fmt::Display for TailBudgetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid tail scan allowance: {}", self.0)
    }
}

impl std::error::Error for TailBudgetError {}

/// Exact work consumed by one admitted tail scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TailWork {
    /// Source bytes read during boundary search and record decoding.
    pub source_bytes: u64,
    /// Complete records decoded.
    pub records: usize,
    /// Bounded tape reads performed.
    pub passes: usize,
    /// Wall duration observed around those bounded reads.
    pub duration: Duration,
}

/// One newest-record scan and the complete-record boundary where it began.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TailScan {
    /// Safe boundary selected at or before the requested byte window.
    pub start: Checkpoint,
    /// Complete records and resumable end checkpoint after that boundary.
    pub scan: Scan,
    /// Exact work consumed to produce this window.
    pub work: TailWork,
    /// Exact plaintext source bytes preceding this window.
    ///
    /// A nonzero value means older history exists even when every decoded
    /// record in this window fits the presentation budget.
    pub withheld_source_bytes: u64,
}

/// Scan the newest caller-bounded byte window without beginning inside a record.
///
/// Its start retreats toward the nearest preceding newline only while the
/// allowance retains bytes and passes, so the scanner never interprets a record
/// suffix as independent vendor evidence. For active members, the bytes that
/// retreat crosses are paid for once: the window reads them again, and pays
/// only from the byte it was asked for. Archived members charge every frame a
/// read decodes, less the retreat bytes already paid. The end checkpoint
/// remains before an unterminated tail, so a later forward scan cannot miss the
/// suffix that completes it.
///
/// # Errors
///
/// Returns [`ScanError`] when tape identity, extents, boundary bytes, or the
/// resulting generation-authenticated scan cannot be read within `budget`.
pub fn scan_tail(
    root: impl AsRef<Path>,
    dialect: Dialect,
    budget: TailBudget,
) -> Result<TailScan, ScanError> {
    let root = root.as_ref();
    let tape = Tape::open_existing(root).map_err(|source| tape_error(root, source))?;
    let source = tape
        .source_id()
        .map_err(|source| tape_error(root, source))?;
    let mut tracker = WorkTracker::new(budget);
    let extents = bounded_extents(&tape, &mut tracker)?;
    let candidate = tail_candidate(&extents, budget.decode_window(), &mut tracker)?;
    let next = preceding_boundary(&tape, &extents, candidate, &mut tracker)?;
    let start = Checkpoint {
        source: source.clone(),
        next,
    };
    let window = Window {
        start: next,
        paid: candidate,
        end: None,
    };
    let state = scan_window(&tape, dialect, &source, &extents, window, &mut tracker)?;
    let observed = tape
        .source_id()
        .map_err(|source| tape_error(root, source))?;
    if observed != source {
        return Err(ScanError::SourceChanged {
            expected: source,
            observed,
            next: state.position,
        });
    }
    let work = tracker.complete()?;
    Ok(TailScan {
        start,
        withheld_source_bytes: bytes_before(&extents, next),
        scan: Scan {
            records: state.records,
            checkpoint: Checkpoint {
                source,
                next: state.position,
            },
            pending: state.spans,
            extents_empty: extents.is_empty(),
        },
        work,
    })
}

/// Scan a caller-bounded complete-record window immediately before an authenticated boundary.
///
/// Unlike [`scan_tail`], this does not read records after `before`, so adjacent
/// history pages can move backward without rescanning a page already served.
///
/// # Errors
///
/// Returns [`ScanError`] when the source identity or boundary no longer belongs
/// to the observed tape generation, or when its records cannot be read within
/// `budget`.
pub fn scan_before(
    root: impl AsRef<Path>,
    dialect: Dialect,
    before: &Checkpoint,
    budget: TailBudget,
) -> Result<TailScan, ScanError> {
    let root = root.as_ref();
    let tape = Tape::open_existing(root).map_err(|source| tape_error(root, source))?;
    let source = tape
        .source_id()
        .map_err(|source| tape_error(root, source))?;
    if before.source != source {
        return Err(ScanError::SourceChanged {
            expected: before.source.clone(),
            observed: source,
            next: before.next,
        });
    }
    let mut tracker = WorkTracker::new(budget);
    let extents = bounded_extents(&tape, &mut tracker)?;
    let end = validate_position(before.next, &extents)?;
    let candidate = window_candidate(&extents, end, budget.decode_window(), &mut tracker)?;
    let next = preceding_boundary(&tape, &extents, candidate, &mut tracker)?;
    let start = Checkpoint {
        source: before.source.clone(),
        next,
    };
    let window = Window {
        start: next,
        paid: candidate,
        end: Some(end),
    };
    let state = scan_window(
        &tape,
        dialect,
        &before.source,
        &extents,
        window,
        &mut tracker,
    )?;
    let observed = tape
        .source_id()
        .map_err(|source| tape_error(root, source))?;
    if observed != before.source {
        return Err(ScanError::SourceChanged {
            expected: before.source.clone(),
            observed,
            next: state.position,
        });
    }
    let work = tracker.complete()?;
    Ok(TailScan {
        start,
        withheld_source_bytes: bytes_before(&extents, next),
        scan: Scan {
            records: state.records,
            checkpoint: Checkpoint {
                source: before.source.clone(),
                next: state.position,
            },
            pending: state.spans,
            extents_empty: extents.is_empty(),
        },
        work,
    })
}

fn bytes_before(extents: &[Extent], position: Position) -> u64 {
    extents
        .iter()
        .map(|extent| {
            if extent.member < position.member {
                extent.length
            } else if extent.member == position.member {
                position.offset.min(extent.length)
            } else {
                0
            }
        })
        .sum()
}

pub(super) struct WorkTracker {
    budget: TailBudget,
    started: Instant,
    source_bytes: u64,
    records: usize,
    passes: usize,
}

impl WorkTracker {
    pub(super) fn new(budget: TailBudget) -> Self {
        Self {
            budget,
            started: Instant::now(),
            source_bytes: 0,
            records: 0,
            passes: 0,
        }
    }

    pub(super) fn read(&mut self, bytes: u64) -> Result<(), ScanError> {
        self.check_duration()?;
        self.pass()?;
        self.source(bytes)
    }

    fn source(&mut self, bytes: u64) -> Result<(), ScanError> {
        let next_bytes = self.source_bytes.saturating_add(bytes);
        if next_bytes > self.budget.source_bytes.get() {
            return Err(self.exhausted_u64(
                "source bytes",
                self.budget.source_bytes.get(),
                self.source_bytes,
            ));
        }
        self.source_bytes = next_bytes;
        Ok(())
    }

    fn remaining_source_bytes(&self) -> u64 {
        self.budget
            .source_bytes
            .get()
            .saturating_sub(self.source_bytes)
    }

    fn remaining_passes(&self) -> usize {
        self.budget.passes.get().saturating_sub(self.passes)
    }

    fn record(&mut self) -> Result<(), ScanError> {
        self.check_duration()?;
        if self.records >= self.budget.records.get() {
            return Err(self.exhausted("decoded records", self.budget.records.get(), self.records));
        }
        self.records = self.records.saturating_add(1);
        Ok(())
    }

    fn pass(&mut self) -> Result<(), ScanError> {
        self.check_duration()?;
        let next_passes = self.passes.saturating_add(1);
        if next_passes > self.budget.passes.get() {
            return Err(self.exhausted("scan passes", self.budget.passes.get(), self.passes));
        }
        self.passes = next_passes;
        Ok(())
    }

    pub(super) fn check_duration(&self) -> Result<(), ScanError> {
        let elapsed = self.started.elapsed();
        if elapsed > self.budget.duration {
            return Err(ScanError::WorkLimit {
                dimension: "wall milliseconds",
                limit: u64::try_from(self.budget.duration.as_millis()).unwrap_or(u64::MAX),
                consumed: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
                work: self.work(),
            });
        }
        Ok(())
    }

    pub(super) fn complete(&self) -> Result<TailWork, ScanError> {
        self.check_duration()?;
        Ok(self.work())
    }

    fn work(&self) -> TailWork {
        TailWork {
            source_bytes: self.source_bytes,
            records: self.records,
            passes: self.passes,
            duration: self.started.elapsed(),
        }
    }

    fn exhausted(&self, dimension: &'static str, limit: usize, consumed: usize) -> ScanError {
        ScanError::WorkLimit {
            dimension,
            limit: u64::try_from(limit).unwrap_or(u64::MAX),
            consumed: u64::try_from(consumed).unwrap_or(u64::MAX),
            work: self.work(),
        }
    }

    fn exhausted_u64(&self, dimension: &'static str, limit: u64, consumed: u64) -> ScanError {
        ScanError::WorkLimit {
            dimension,
            limit,
            consumed,
            work: self.work(),
        }
    }
}

/// One complete-record window and the byte its allowance pays from.
///
/// The boundary search already paid for every byte from `start` up to
/// `paid`, the candidate it retreated from, so the window pays once.
#[derive(Clone, Copy)]
pub(super) struct Window {
    pub(super) start: Position,
    pub(super) paid: Position,
    pub(super) end: Option<Position>,
}

impl Window {
    /// Bytes of one extent's window the boundary search has not paid for.
    fn unpaid(self, extent: &Extent, start: u64, end: u64) -> u64 {
        if extent.member < self.paid.member {
            return 0;
        }
        let paid = if extent.member == self.paid.member {
            self.paid.offset
        } else {
            0
        };
        end.saturating_sub(start.max(paid))
    }
}

pub(super) fn scan_window(
    tape: &Tape,
    dialect: Dialect,
    source: &SourceId,
    extents: &[Extent],
    window: Window,
    tracker: &mut WorkTracker,
) -> Result<ScanState, ScanError> {
    let end = window.end;
    let mut state = ScanState {
        position: window.start,
        records: Vec::new(),
        bytes: Vec::new(),
        spans: Vec::new(),
    };
    for extent in extents {
        tracker.pass()?;
        if end.is_some_and(|end| extent.member > end.member) {
            break;
        }
        let Some((start, extent_end)) = unread_range(extent, end, &mut state) else {
            continue;
        };
        let length = extent_end - start;
        let credit = length - window.unpaid(extent, start, extent_end);
        let fragment = charged_extent(
            tape,
            Locus::new(extent.member, start, length),
            *extent,
            |work| match work.saturating_sub(credit) {
                0 => Ok(()),
                due => tracker.read(due),
            },
        )?;
        feed(
            extent,
            start,
            &fragment,
            dialect,
            source,
            &mut state,
            &mut || tracker.record(),
        )?;
        tracker.check_duration()?;
    }
    Ok(state)
}

fn tail_candidate(
    extents: &[Extent],
    bytes: u64,
    tracker: &mut WorkTracker,
) -> Result<Position, ScanError> {
    let mut remaining = bytes;
    for extent in extents.iter().rev() {
        tracker.pass()?;
        if remaining >= extent.length {
            remaining -= extent.length;
            continue;
        }
        return Ok(Position {
            member: extent.member,
            offset: extent.length - remaining,
        });
    }
    Ok(Position::start())
}

fn window_candidate(
    extents: &[Extent],
    end: Position,
    bytes: u64,
    tracker: &mut WorkTracker,
) -> Result<Position, ScanError> {
    let mut remaining = bytes;
    for extent in extents.iter().rev() {
        tracker.pass()?;
        if extent.member > end.member {
            continue;
        }
        let length = if extent.member == end.member {
            end.offset.min(extent.length)
        } else {
            extent.length
        };
        if length == 0 {
            continue;
        }
        if remaining < length {
            return Ok(Position {
                member: extent.member,
                offset: length - remaining,
            });
        }
        remaining -= length;
    }
    Ok(Position::start())
}

fn preceding_boundary(
    tape: &Tape,
    extents: &[Extent],
    candidate: Position,
    tracker: &mut WorkTracker,
) -> Result<Position, ScanError> {
    if candidate == Position::start() {
        return Ok(candidate);
    }
    let candidate_index = usize::try_from(candidate.member.saturating_sub(1))
        .ok()
        .filter(|index| {
            extents
                .get(*index)
                .is_some_and(|extent| extent.member == candidate.member)
        })
        .expect("a tail candidate names one observed extent");
    for index in (0..=candidate_index).rev() {
        tracker.pass()?;
        let extent = extents[index];
        let mut end = if index == candidate_index {
            candidate.offset
        } else {
            extent.length
        };
        while end > 0 {
            let start = end.saturating_sub(BOUNDARY_CHUNK_BYTES);
            let window = charged_window(
                tape,
                Locus::new(extent.member, start, end - start),
                extent,
                |work| tracker.read(work),
            )?;
            tracker.check_duration()?;
            if let Some(newline) = window.bytes.iter().rposition(|byte| *byte == b'\n') {
                let offset = window.offset + newline as u64 + 1;
                return Ok(if extent.sealed && offset == extent.length {
                    Position {
                        member: extent.member + 1,
                        offset: 0,
                    }
                } else {
                    Position {
                        member: extent.member,
                        offset,
                    }
                });
            }
            end = window.offset;
        }
    }
    // Every extent back to the first byte carried no newline, so the record
    // the candidate sits inside is the tape's first. Its own beginning is the
    // boundary; the allowance that paid for the search already bounded it.
    Ok(Position::start())
}

pub(super) fn bounded_extents(
    tape: &Tape,
    tracker: &mut WorkTracker,
) -> Result<Vec<Extent>, ScanError> {
    tracker.pass()?;
    let bytes = tracker.remaining_source_bytes();
    let extents = tracker.remaining_passes();
    match tape.extents_bounded(bytes, extents) {
        Ok(snapshot) => {
            tracker.source(snapshot.index_bytes)?;
            for _ in &snapshot.extents {
                tracker.pass()?;
            }
            Ok(snapshot.extents)
        }
        Err(tape::Error::Refused(tape::Refusal::IndexReadLimit { .. })) => Err(tracker
            .exhausted_u64(
                "source bytes",
                tracker.budget.source_bytes.get(),
                tracker.source_bytes,
            )),
        Err(tape::Error::Refused(tape::Refusal::ExtentReadLimit {
            limit, index_bytes, ..
        })) => {
            tracker.source(index_bytes)?;
            for _ in 0..limit {
                tracker.pass()?;
            }
            Err(tracker.exhausted("scan passes", tracker.budget.passes.get(), tracker.passes))
        }
        Err(source) => Err(tape_error(tape.root(), source)),
    }
}

pub(super) fn tape_error(path: &Path, source: tape::Error) -> ScanError {
    ScanError::Tape {
        path: PathBuf::from(path),
        source,
    }
}

/// A charged read's failure: the work budget's refusal or tape's own.
enum Charged {
    Budget(ScanError),
    Tape(tape::Error),
}

impl From<tape::Error> for Charged {
    fn from(source: tape::Error) -> Self {
        Self::Tape(source)
    }
}

impl Charged {
    fn settle(self, tape: &Tape) -> ScanError {
        match self {
            Self::Budget(error) => error,
            Self::Tape(source) => tape_error(tape.root(), source),
        }
    }
}

/// Read exactly `locus`, charging the plaintext tape decodes to produce it
/// before any of it is decoded.
pub(super) fn charged_extent(
    tape: &Tape,
    locus: Locus,
    extent: Extent,
    charge: impl FnOnce(u64) -> Result<(), ScanError>,
) -> Result<Vec<u8>, ScanError> {
    tape.read_extent(locus, extent, |work| charge(work).map_err(Charged::Budget))
        .map_err(|error| error.settle(tape))
}

/// Read every decoded byte from where storage lets `locus` begin through its
/// end, charging that work before any of it is decoded.
pub(super) fn charged_window(
    tape: &Tape,
    locus: Locus,
    extent: Extent,
    charge: impl FnOnce(u64) -> Result<(), ScanError>,
) -> Result<tape::Window, ScanError> {
    tape.read_window(locus, extent, |work| charge(work).map_err(Charged::Budget))
        .map_err(|error| error.settle(tape))
}
