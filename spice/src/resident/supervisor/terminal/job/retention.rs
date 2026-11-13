//! What a command's output may leave on disk.
//!
//! The keeper reads both of the command's streams and hands every byte to
//! one [`Log`]. A block of up to [`BLOCK_LINES`] complete lines that repeats
//! back to back keeps its first copy and one marker counting the repeats.
//! Bytes are written as they arrive except while they repeat lines the log
//! already holds, so a held byte never hides output the log does not show.
//! The log keeps at most [`RETAINED_CAP`] bytes, markers included; output
//! past the cap is counted and dropped after one notice saying so. Observed
//! output reaching [`TERMINATE_AT`] and [`KILL_AT`] is reported once each so
//! the keeper can end a runaway command.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant as Monotonic};

use serde::{Deserialize, Serialize};

/// Bytes one read of a command's stream, or of a job's log, takes.
pub(super) const CHUNK: usize = 8 * 1024;

pub(super) const MIB: u64 = 1024 * 1024;

/// Bytes a job's log may hold, across both streams.
pub(super) const RETAINED_CAP: u64 = 64 * MIB;

/// Observed output at which the keeper asks a command to leave.
pub(super) const TERMINATE_AT: u64 = 64 * MIB;

/// Observed output at which the keeper kills a command still running.
pub(super) const KILL_AT: u64 = 80 * MIB;

/// Longest block of lines whose consecutive repeats collapse.
const BLOCK_LINES: usize = 32;

/// Longest line, newline included, that a repeat is compared against.
const LINE_BYTES: usize = 4096;

/// Room under the cap kept for the notice that the cap was reached.
const NOTICE_RESERVE: u64 = 256;

/// The line the log ends with once it reached its cap.
pub(in crate::resident) const CAPPED: &str =
    "[spice: the log reached its 64 MiB cap; later output was counted, not kept]";

/// What became of a command's output, as the keeper accounted it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(in crate::resident) struct Output {
    /// Bytes the command wrote to stdout and stderr.
    pub(in crate::resident) observed: u64,
    /// Bytes the log holds, Spice's own markers included.
    pub(in crate::resident) retained: u64,
    /// Bytes of consecutive repeats that markers stand for.
    pub(in crate::resident) collapsed: u64,
    /// Bytes dropped once the log reached its cap.
    pub(in crate::resident) omitted: u64,
}

/// An observed-output threshold the command crossed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Limit {
    Terminate,
    Kill,
}

/// The bounded, collapsing writer of one log.
pub(super) struct Retention<W> {
    sink: W,
    cap: u64,
    output: Output,
    /// The complete lines most recently written, oldest first; a line too
    /// long to compare is `None`.
    recent: VecDeque<Option<Vec<u8>>>,
    /// The current line: bytes held while they repeat a candidate, or the
    /// start of a line already written, kept to remember it.
    line: Vec<u8>,
    /// Whether the current line is written as it arrives.
    flowing: bool,
    /// Whether the current written line outgrew [`LINE_BYTES`].
    long: bool,
    /// Indices into `recent` the held line still repeats.
    alive: Vec<usize>,
    run: Option<Run>,
    capped: bool,
}

/// A block of `recent` lines repeating back to back.
struct Run {
    start: usize,
    period: usize,
    /// Lines of the current repeat matched so far, held in `partial`.
    matched: usize,
    repeats: u64,
    partial: Vec<Vec<u8>>,
}

impl<W: Write> Retention<W> {
    pub(super) fn new(sink: W, cap: u64) -> Self {
        Self {
            sink,
            cap,
            output: Output::default(),
            recent: VecDeque::new(),
            line: Vec::new(),
            flowing: true,
            long: false,
            alive: Vec::new(),
            run: None,
            capped: false,
        }
    }

    pub(super) fn output(&self) -> Output {
        self.output
    }

    /// Take the next bytes the command wrote.
    ///
    /// # Errors
    ///
    /// Returns the failure of writing the log.
    pub(super) fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.output.observed += bytes.len() as u64;
        if self.capped {
            self.output.omitted += bytes.len() as u64;
            return Ok(());
        }
        for piece in bytes.split_inclusive(|byte| *byte == b'\n') {
            self.piece(piece)?;
        }
        Ok(())
    }

    /// Write what is still held, closing any run, and account the log.
    ///
    /// # Errors
    ///
    /// Returns the failure of writing the log.
    pub(super) fn finish(mut self) -> io::Result<Output> {
        let held = if self.flowing {
            Vec::new()
        } else {
            std::mem::take(&mut self.line)
        };
        self.resolve()?;
        self.emit(&held)?;
        self.sink.flush()?;
        Ok(self.output)
    }

    /// One piece of a line, ending with its newline when it completes it.
    fn piece(&mut self, piece: &[u8]) -> io::Result<()> {
        let ended = piece.ends_with(b"\n");
        if self.flowing {
            self.emit(piece)?;
            if !self.long {
                self.long = self.line.len() + piece.len() > LINE_BYTES;
                if self.long {
                    self.line.clear();
                } else {
                    self.line.extend_from_slice(piece);
                }
            }
            if ended {
                let line = (!self.long).then(|| std::mem::take(&mut self.line));
                self.remember(line);
                self.next_line();
            }
            return Ok(());
        }
        let offset = self.line.len();
        self.line.extend_from_slice(piece);
        let recent = &self.recent;
        self.alive.retain(|&index| {
            recent[index]
                .as_ref()
                .is_some_and(|candidate| candidate.get(offset..offset + piece.len()) == Some(piece))
        });
        if self.alive.is_empty() {
            let line = std::mem::take(&mut self.line);
            self.resolve()?;
            self.flowing = true;
            return self.piece(&line);
        }
        if ended {
            self.repeated();
        }
        Ok(())
    }

    /// The held line completed as a copy of a candidate.
    fn repeated(&mut self) {
        let line = std::mem::take(&mut self.line);
        let run = match &mut self.run {
            Some(run) => run,
            None => {
                // The nearest copy is the shortest block.
                let start = self.alive.iter().copied().max().unwrap_or_default();
                self.run.insert(Run {
                    start,
                    period: self.recent.len() - start,
                    matched: 0,
                    repeats: 0,
                    partial: Vec::new(),
                })
            }
        };
        run.partial.push(line);
        run.matched += 1;
        if run.matched == run.period {
            run.repeats += 1;
            run.matched = 0;
            self.output.collapsed += run
                .partial
                .drain(..)
                .map(|line| line.len() as u64)
                .sum::<u64>();
        }
        self.next_line();
    }

    /// End any run: its marker, then the lines of an unfinished repeat.
    fn resolve(&mut self) -> io::Result<()> {
        let Some(run) = self.run.take() else {
            return Ok(());
        };
        if run.repeats > 0 {
            let marker = format!(
                "[spice: the {} line(s) above repeated {} more time(s)]\n",
                run.period, run.repeats
            );
            self.note(marker.as_bytes())?;
            self.recent.clear();
        }
        for line in run.partial {
            self.emit(&line)?;
            self.remember(Some(line));
        }
        Ok(())
    }

    fn remember(&mut self, line: Option<Vec<u8>>) {
        self.recent.push_back(line);
        if self.recent.len() > BLOCK_LINES {
            self.recent.pop_front();
        }
    }

    /// Choose what the next line may repeat: the run's next line, or any
    /// line just written.
    fn next_line(&mut self) {
        self.line.clear();
        self.long = false;
        self.alive.clear();
        match &self.run {
            Some(run) => self.alive.push(run.start + run.matched),
            None => self.alive.extend(
                self.recent
                    .iter()
                    .enumerate()
                    .filter_map(|(index, line)| line.as_ref().map(|_| index)),
            ),
        }
        self.flowing = self.alive.is_empty();
    }

    /// Write command bytes under the cap; the first that do not fit are
    /// replaced by the notice and everything after is only counted.
    fn emit(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.capped {
            self.output.omitted += bytes.len() as u64;
            return Ok(());
        }
        let room = (self.cap - NOTICE_RESERVE).saturating_sub(self.output.retained);
        let fits = usize::try_from(room).map_or(bytes.len(), |room| room.min(bytes.len()));
        self.sink.write_all(&bytes[..fits])?;
        self.output.retained += fits as u64;
        if fits < bytes.len() {
            self.output.omitted += (bytes.len() - fits) as u64;
            self.capped = true;
            let notice = format!("\n{CAPPED}\n");
            self.sink.write_all(notice.as_bytes())?;
            self.output.retained += notice.len() as u64;
        }
        Ok(())
    }

    /// Write one of Spice's own lines when it fits under the cap.
    fn note(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.capped || self.output.retained + bytes.len() as u64 > self.cap - NOTICE_RESERVE {
            return Ok(());
        }
        self.sink.write_all(bytes)?;
        self.output.retained += bytes.len() as u64;
        Ok(())
    }
}

/// One job's log, written by the threads reading the command's streams.
pub(super) struct Log {
    shared: Arc<Shared>,
    done: mpsc::Sender<()>,
    drained: mpsc::Receiver<()>,
    readers: usize,
}

struct Shared {
    /// `None` once closed: a stream still open past the command writes
    /// nothing more.
    retention: Mutex<Option<Retention<File>>>,
    observed: AtomicU64,
    limit: Box<dyn Fn(Limit) + Send + Sync>,
}

/// A writer into a [`Log`], for a reader the log does not run itself.
#[derive(Clone)]
pub(super) struct Writer(Arc<Shared>);

impl Log {
    /// A log appending to `file`, telling `limit` each observed-output
    /// threshold the command crosses.
    pub(super) fn open(file: File, limit: Box<dyn Fn(Limit) + Send + Sync>) -> Self {
        let (done, drained) = mpsc::channel();
        Self {
            shared: Arc::new(Shared {
                retention: Mutex::new(Some(Retention::new(file, RETAINED_CAP))),
                observed: AtomicU64::new(0),
                limit,
            }),
            done,
            drained,
            readers: 0,
        }
    }

    pub(super) fn writer(&self) -> Writer {
        Writer(Arc::clone(&self.shared))
    }

    /// Bytes the command has written so far.
    pub(super) fn observed(&self) -> u64 {
        self.shared.observed.load(Ordering::Relaxed)
    }

    /// Read `stream` into the log on a thread of its own until it ends.
    ///
    /// # Errors
    ///
    /// Returns the failure of starting the thread.
    pub(super) fn drain(&mut self, stream: impl Read + Send + 'static) -> io::Result<()> {
        let writer = self.writer();
        let done = self.done.clone();
        thread::Builder::new()
            .name("spice-job-output".to_owned())
            .spawn(move || {
                let mut stream = stream;
                let mut chunk = [0u8; CHUNK];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(length) => writer.write(&chunk[..length]),
                        Err(error) if error.kind() == ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
                let _ = done.send(());
            })?;
        self.readers += 1;
        Ok(())
    }

    /// Wait up to `bound` for every drained stream to end, then close the
    /// log: a descendant can hold a stream open past the command.
    pub(super) fn close(self, bound: Duration) -> Output {
        let deadline = Monotonic::now() + bound;
        for _ in 0..self.readers {
            let remaining = deadline.saturating_duration_since(Monotonic::now());
            if self.drained.recv_timeout(remaining).is_err() {
                break;
            }
        }
        let retention = self
            .shared
            .retention
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        retention.map_or_else(Output::default, |retention| {
            let output = retention.output();
            retention.finish().unwrap_or(output)
        })
    }
}

impl Writer {
    pub(super) fn write(&self, bytes: &[u8]) {
        let (before, after) = {
            let mut retention = self
                .0
                .retention
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let Some(retention) = retention.as_mut() else {
                return;
            };
            let before = retention.output().observed;
            let _ = retention.write(bytes);
            (before, retention.output().observed)
        };
        self.0.observed.store(after, Ordering::Relaxed);
        for (threshold, limit) in [(TERMINATE_AT, Limit::Terminate), (KILL_AT, Limit::Kill)] {
            if before < threshold && after >= threshold {
                (self.0.limit)(limit);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finished(cap: u64, writes: &[&[u8]]) -> (String, Output) {
        let mut log = Vec::new();
        let mut retention = Retention::new(&mut log, cap);
        for bytes in writes {
            retention.write(bytes).expect("in memory");
        }
        let output = retention.finish().expect("in memory");
        (String::from_utf8(log).expect("utf-8"), output)
    }

    #[test]
    fn consecutive_repeats_of_a_block_keep_one_copy_and_a_count() {
        let mut writes: Vec<&[u8]> = vec![b"start\n"];
        writes.extend(std::iter::repeat_n(
            b"warning: retry\n1) retry 2) abort\n".as_slice(),
            1000,
        ));
        writes.push(b"done\n");

        let (log, output) = finished(RETAINED_CAP, &writes);

        assert_eq!(
            log,
            "start\nwarning: retry\n1) retry 2) abort\n\
             [spice: the 2 line(s) above repeated 999 more time(s)]\ndone\n"
        );
        assert_eq!(output.collapsed, 999 * 33);
        assert_eq!(output.omitted, 0);
        assert_eq!(output.retained, log.len() as u64);
    }

    #[test]
    fn a_repeat_split_across_writes_still_collapses_and_an_unfinished_one_is_kept() {
        let (log, output) = finished(RETAINED_CAP, &[b"tick\nti", b"ck\ntick\nti", b"ck\nti"]);

        assert_eq!(
            log,
            "tick\n[spice: the 1 line(s) above repeated 3 more time(s)]\nti"
        );
        assert_eq!(output.observed, 22);
        assert_eq!(output.collapsed, 15);
    }

    #[test]
    fn distinct_output_is_written_in_order_and_cut_at_the_cap() {
        let lines: Vec<String> = (0..200).map(|line| format!("line {line:04}\n")).collect();
        let writes: Vec<&[u8]> = lines.iter().map(String::as_bytes).collect();
        let cap = NOTICE_RESERVE + 100;

        let (log, output) = finished(cap, &writes);

        assert!(log.starts_with("line 0000\nline 0001\n"), "{log}");
        assert!(log.ends_with(&format!("\n{CAPPED}\n")), "{log}");
        assert!(output.retained <= cap);
        assert_eq!(output.retained, log.len() as u64);
        assert_eq!(output.observed, 200 * 10);
        assert_eq!(output.omitted, 200 * 10 - 100);
    }
}
