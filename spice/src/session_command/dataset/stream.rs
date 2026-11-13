//! Bounded forward decoding into one canonical session fold.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use flate2::read::GzDecoder;
use wire::Refusal;

use super::horizon::start_offset_after_prior_timestamp;
use super::work::{SessionWork, decoded_row_events};
use super::{
    DecodedRow, MAX_LINE_BYTES, ResolvedSource, Vendor, decode_row, read_residue, record_timestamp,
    refusal,
};

enum RecordRead {
    Eof,
    Read { bytes: usize, terminated: bool },
    Interrupted(std::io::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ForwardContinuation {
    pub(crate) line: u64,
    pub(super) dialect: Option<listen::Dialect>,
}

pub(crate) enum ForwardProgress {
    More,
    Eof(ForwardContinuation),
}

pub(crate) struct ForwardWindow {
    pub(crate) row: Option<DecodedRow>,
    pub(crate) progress: ForwardProgress,
}

pub(crate) struct ForwardCursor<'a> {
    source: &'a ResolvedSource,
    reader: Box<dyn BufRead>,
    start: Option<String>,
    dialect: Option<listen::Dialect>,
    line: u64,
    within_horizon: bool,
    buffer: Vec<u8>,
}

impl<'a> ForwardCursor<'a> {
    pub(crate) fn new(
        source: &'a ResolvedSource,
        start: Option<&str>,
        work: &mut SessionWork,
    ) -> Result<Self, Refusal> {
        let file = File::open(&source.path).map_err(|error| source_unreadable(source, &error))?;
        let reader = transcript_reader(source, file, start, work)?;
        work.begin_pass(&source.path.display().to_string())?;
        Ok(Self {
            source,
            reader,
            start: start.map(str::to_owned),
            dialect: None,
            line: 0,
            within_horizon: start.is_none(),
            buffer: Vec::new(),
        })
    }

    pub(crate) fn advance(&mut self, work: &mut SessionWork) -> Result<ForwardWindow, Refusal> {
        self.buffer.clear();
        let at = self.next_record_locus();
        let (read, terminated) =
            match read_record(self.reader.as_mut(), &mut self.buffer, work, &at)? {
                RecordRead::Eof => return Ok(self.window(None, false)),
                RecordRead::Interrupted(error) => {
                    self.line = self.line.saturating_add(1);
                    let row =
                        DecodedRow::Residue(read_residue(&self.source.path, self.line, &error));
                    work.admit_decoded(decoded_row_events(&row), &at)?;
                    return Ok(self.window(Some(row), false));
                }
                RecordRead::Read { bytes, terminated } => (bytes, terminated),
            };
        self.line = self.line.saturating_add(1);
        debug_assert!(read > 0 && read <= MAX_LINE_BYTES.saturating_add(2));
        let (observed, row) = decode_window_row(
            self.source,
            self.line,
            &self.buffer,
            self.dialect,
            work,
            &at,
        )?;
        self.dialect = observed.or(self.dialect);
        let row = match row {
            None => None,
            Some(DecodedRow::Typed(record)) => {
                if !self.within_horizon
                    && self
                        .start
                        .as_deref()
                        .is_some_and(|start| record_timestamp(&record) >= start)
                {
                    self.within_horizon = true;
                }
                self.within_horizon.then_some(DecodedRow::Typed(record))
            }
            Some(row) => Some(row),
        };
        Ok(self.window(row, terminated))
    }

    fn window(&self, row: Option<DecodedRow>, more: bool) -> ForwardWindow {
        let progress = if more {
            ForwardProgress::More
        } else {
            ForwardProgress::Eof(self.continuation())
        };
        ForwardWindow { row, progress }
    }

    pub(crate) fn continuation(&self) -> ForwardContinuation {
        ForwardContinuation {
            line: self.line,
            dialect: self.dialect,
        }
    }

    fn next_record_locus(&self) -> String {
        format!(
            "{}:record {}",
            self.source.path.display(),
            self.line.saturating_add(1)
        )
    }

    /// The locus of the record the cursor read last.
    pub(crate) fn record_locus(&self) -> String {
        format!("{}:record {}", self.source.path.display(), self.line)
    }

    pub(crate) fn vendor(&self) -> Vendor {
        self.dialect.map_or(Vendor::UNKNOWN, Vendor::inferred)
    }
}

pub(super) fn decode_window_row(
    source: &ResolvedSource,
    line: u64,
    buffer: &[u8],
    dialect: Option<listen::Dialect>,
    work: &mut SessionWork,
    at: &str,
) -> Result<(Option<listen::Dialect>, Option<DecodedRow>), Refusal> {
    let end = buffer
        .iter()
        .rposition(|byte| !matches!(byte, b'\n' | b'\r'))
        .map_or(0, |last| last + 1);
    if end == 0 {
        return Ok((dialect, None));
    }
    let (observed, row) = decode_row(source, line, &buffer[..end], dialect)?;
    work.admit_decoded(decoded_row_events(&row), at)?;
    Ok((observed, Some(row)))
}

fn read_record(
    reader: &mut dyn BufRead,
    buffer: &mut Vec<u8>,
    work: &mut SessionWork,
    at: &str,
) -> Result<RecordRead, Refusal> {
    loop {
        work.check_deadline(at)?;
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(error) => return Ok(RecordRead::Interrupted(error)),
        };
        if available.is_empty() {
            return if buffer.is_empty() {
                Ok(RecordRead::Eof)
            } else {
                work.admit_record(at)?;
                Ok(RecordRead::Read {
                    bytes: buffer.len(),
                    terminated: false,
                })
            };
        }
        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |newline| newline + 1);
        if buffer.is_empty() {
            work.ensure_record(at)?;
        }
        work.admit_source_bytes(count as u64, at)?;
        if buffer.len().saturating_add(count) > MAX_LINE_BYTES.saturating_add(2) {
            return Err(refusal(
                "session::transcript-record-oversized",
                Some(format!("{at} exceeds {MAX_LINE_BYTES} bytes")),
                "restore a bounded transcript record or select an earlier intact copy",
            ));
        }
        buffer.extend_from_slice(&available[..count]);
        let terminated = available[count - 1] == b'\n';
        reader.consume(count);
        if terminated {
            work.admit_record(at)?;
            return Ok(RecordRead::Read {
                bytes: buffer.len(),
                terminated,
            });
        }
    }
}

/// The transcript reader, positioned after the last record before `start`
/// when the transcript is uncompressed and a start is set.
fn transcript_reader(
    source: &ResolvedSource,
    mut file: File,
    start: Option<&str>,
    work: &mut SessionWork,
) -> Result<Box<dyn BufRead>, Refusal> {
    if compressed(&source.path) {
        return Ok(Box::new(BufReader::new(GzDecoder::new(file))));
    }
    if let Some(start) = start {
        let transcript_length = file
            .metadata()
            .map_err(|error| source_unreadable(source, &error))?
            .len();
        work.begin_pass(&source.path.display().to_string())?;
        let locator = file
            .try_clone()
            .map_err(|error| source_unreadable(source, &error))?;
        let offset =
            start_offset_after_prior_timestamp(source, locator, transcript_length, start, work)?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| source_unreadable(source, &error))?;
    }
    Ok(Box::new(BufReader::new(file)))
}

fn source_unreadable(source: &ResolvedSource, error: &std::io::Error) -> Refusal {
    refusal(
        "session::transcript-unreadable",
        Some(format!("{}: {error}", source.path.display())),
        "restore read access to the transcript and retry",
    )
}

pub(crate) fn compressed(path: &Path) -> bool {
    path.to_string_lossy().ends_with(".gz")
}
