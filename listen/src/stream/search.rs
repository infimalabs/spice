//! Newest-first search for the complete records that carry any byte needle.

use std::ops::ControlFlow;
use std::path::Path;

use memchr::{memmem, memrchr_iter};
use tape::{Extent, Locus, SourceId, Tape};

use super::tail::{
    TailBudget, TailWork, Window, WorkTracker, bounded_extents, charged_window, scan_window,
    tape_error,
};
use super::{Position, Record, ScanError};
use crate::Dialect;

/// Bytes one backward read asks for; an archived read widens to the start of
/// the frame holding its first byte.
const SEARCH_CHUNK_BYTES: u64 = 1024 * 1024;

/// How far one backward search covered its tape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TailSearch {
    /// Generation the search authenticated.
    pub source: SourceId,
    /// Every complete record beginning at or after this boundary was searched.
    ///
    /// [`Position::start`] means the whole tape was searched. After the
    /// visitor stops the search, this is the start of the record it stopped on.
    pub start: Position,
    /// Exact work consumed.
    pub work: TailWork,
}

/// Visit, newest first, each complete record whose bytes contain any needle.
///
/// Raw bytes are searched backward, so only matching records are decoded and
/// records that never mention a needle cost a byte comparison, not a parse.
/// The budget's window is how many tape bytes the search reaches back from
/// the end; a matching record the window enters is still completed. Its source
/// allowance also pays for decoding matches, its record allowance counts only
/// decoded matches, and one pass is one bounded read. An unterminated tail is
/// never visited.
///
/// # Errors
///
/// Returns [`ScanError`] when tape identity, extents, or bytes cannot be read
/// within `budget`, or a matching record breaks the decoder contract.
///
/// # Panics
///
/// Panics when `needles` is empty, or any needle is empty or spans a record
/// delimiter.
pub fn search_tail(
    root: impl AsRef<Path>,
    dialect: Dialect,
    needles: &[&[u8]],
    budget: TailBudget,
    mut visit: impl FnMut(Record) -> ControlFlow<()>,
) -> Result<TailSearch, ScanError> {
    assert!(
        !needles.is_empty()
            && needles
                .iter()
                .all(|needle| !needle.is_empty() && !needle.contains(&b'\n')),
        "every search needle lies within one record"
    );
    let root = root.as_ref();
    let tape = Tape::open_existing(root).map_err(|source| tape_error(root, source))?;
    let source = tape
        .source_id()
        .map_err(|source| tape_error(root, source))?;
    let mut tracker = WorkTracker::new(budget);
    let extents = bounded_extents(&tape, &mut tracker)?;
    let mut search = Search {
        tape: &tape,
        dialect,
        source: &source,
        extents: &extents,
        finders: needles.iter().map(memmem::Finder::new).collect(),
        overlap: Vec::new(),
        record_end: None,
        matched: false,
        remaining: budget.decode_window(),
    };
    let start = search.run(&mut tracker, &mut visit)?;
    let observed = tape
        .source_id()
        .map_err(|source| tape_error(root, source))?;
    if observed != source {
        return Err(ScanError::SourceChanged {
            expected: source,
            observed,
            next: start,
        });
    }
    let work = tracker.complete()?;
    Ok(TailSearch {
        source,
        start,
        work,
    })
}

struct Search<'a> {
    tape: &'a Tape,
    dialect: Dialect,
    source: &'a SourceId,
    extents: &'a [Extent],
    finders: Vec<memmem::Finder<'a>>,
    /// Leading bytes of the newer chunk a needle may continue into.
    overlap: Vec<u8>,
    /// End of the complete record the walk is inside; `None` in the
    /// unterminated tail.
    record_end: Option<Position>,
    /// Whether that record contains the needle.
    matched: bool,
    /// Window bytes not yet searched.
    remaining: u64,
}

impl Search<'_> {
    fn run(
        &mut self,
        tracker: &mut WorkTracker,
        visit: &mut impl FnMut(Record) -> ControlFlow<()>,
    ) -> Result<Position, ScanError> {
        for extent in self.extents.iter().rev() {
            let mut end = extent.length;
            while end > 0 {
                if self.remaining == 0 && !self.matched {
                    return Ok(self.record_end.unwrap_or(Position {
                        member: extent.member,
                        offset: end,
                    }));
                }
                let wanted = end.saturating_sub(SEARCH_CHUNK_BYTES);
                let window = charged_window(
                    self.tape,
                    Locus::new(extent.member, wanted, end - wanted),
                    *extent,
                    |work| tracker.read(work),
                )?;
                tracker.check_duration()?;
                let start = window.offset;
                self.remaining = self.remaining.saturating_sub(end - start);
                if let ControlFlow::Break(stop) =
                    self.walk(extent.member, start, window.bytes, tracker, visit)?
                {
                    return Ok(stop);
                }
                end = start;
            }
        }
        if let ControlFlow::Break(stop) = self.close(Position::start(), tracker, visit)? {
            return Ok(stop);
        }
        Ok(Position::start())
    }

    /// Walk one chunk newest byte first, closing each record at its delimiter.
    fn walk(
        &mut self,
        member: u64,
        start: u64,
        mut bytes: Vec<u8>,
        tracker: &mut WorkTracker,
        visit: &mut impl FnMut(Record) -> ControlFlow<()>,
    ) -> Result<ControlFlow<Position>, ScanError> {
        let length = bytes.len();
        let carried = self
            .finders
            .iter()
            .map(|finder| finder.needle().len() - 1)
            .max()
            .unwrap_or_default();
        let overlap = bytes[..length.min(carried)].to_vec();
        bytes.extend_from_slice(&self.overlap);
        let mut matches = self
            .finders
            .iter()
            .flat_map(|finder| finder.find_iter(&bytes).take_while(|at| *at < length))
            .collect::<Vec<_>>();
        matches.sort_unstable();
        bytes.truncate(length);
        self.overlap = overlap;
        for newline in memrchr_iter(b'\n', &bytes) {
            if matches.last().is_some_and(|at| *at > newline) {
                matches.retain(|at| *at < newline);
                self.matched = self.record_end.is_some();
            }
            let boundary = Position {
                member,
                offset: start + newline as u64 + 1,
            };
            if let ControlFlow::Break(stop) = self.close(boundary, tracker, visit)? {
                return Ok(ControlFlow::Break(stop));
            }
        }
        if !matches.is_empty() {
            self.matched = self.record_end.is_some();
        }
        Ok(ControlFlow::Continue(()))
    }

    /// Enter the record that ends at `boundary`, visiting the one it began.
    fn close(
        &mut self,
        boundary: Position,
        tracker: &mut WorkTracker,
        visit: &mut impl FnMut(Record) -> ControlFlow<()>,
    ) -> Result<ControlFlow<Position>, ScanError> {
        let end = self.record_end.replace(boundary);
        if !std::mem::take(&mut self.matched) {
            return Ok(ControlFlow::Continue(()));
        }
        let end = end.expect("only a terminated record can match");
        let first = usize::try_from(boundary.member.max(1) - 1).expect("member index fits");
        let last = usize::try_from(end.member - 1).expect("member index fits");
        let window = Window {
            start: boundary,
            paid: boundary,
            end: Some(end),
        };
        let state = scan_window(
            self.tape,
            self.dialect,
            self.source,
            &self.extents[first..=last],
            window,
            tracker,
        )?;
        for record in state.records.into_iter().rev() {
            if visit(record).is_break() {
                return Ok(ControlFlow::Break(boundary));
            }
        }
        Ok(ControlFlow::Continue(()))
    }
}
