//! Consecutive duplicate collapsing for the resident's diagnostic stream.
//!
//! The resident writes its diagnostics as bare lines to the stream the daemon
//! points at `resident.log`, and a stuck lane says the same sentence on every
//! turn: one recorded log holds 4,135 consecutive copies of one app-server
//! refusal. Repetition is information about how often, not about what, so the
//! stream keeps the first telling and then one line that counts the rest.
//!
//! Duplicate identity is byte equality of the complete line, newline included.
//! Resident diagnostics carry no emission metadata of their own — no timestamp,
//! sequence, or level is added here, and the daemon appends the child's stream
//! verbatim — so two lines that read the same are the same telling. A line that
//! embeds a changing instant, lane, or reason therefore differs and begins its
//! own run, which is what keeps substantively different messages apart.

use std::io::{self, Write};

/// A diagnostic stream that says a repeated line twice: once when it starts,
/// once when it ends, carrying the number of copies in between.
///
/// Only the line under comparison and its repeat count are retained, so a run
/// of any length costs the same as a run of one. A run ends when a different
/// line arrives or when the stream is dropped; a line that never repeats is
/// passed straight through.
pub(super) struct Rollup<W: Write> {
    inner: W,
    /// The last line written, kept only to compare the next one against.
    held: Vec<u8>,
    /// Copies of `held` suppressed since it was written.
    repeats: u64,
    /// A line whose newline has not arrived yet.
    partial: Vec<u8>,
}

impl<W: Write> Rollup<W> {
    pub(super) const fn new(inner: W) -> Self {
        Self {
            inner,
            held: Vec::new(),
            repeats: 0,
            partial: Vec::new(),
        }
    }

    /// Account one complete line, newline included.
    fn line(&mut self, line: &[u8]) -> io::Result<()> {
        if line == self.held {
            self.repeats += 1;
            return Ok(());
        }
        self.rollup()?;
        self.inner.write_all(line)?;
        self.held.clear();
        self.held.extend_from_slice(line);
        Ok(())
    }

    /// Close the run the held line started, if it repeated at all.
    fn rollup(&mut self) -> io::Result<()> {
        let repeats = std::mem::take(&mut self.repeats);
        if repeats == 0 {
            return Ok(());
        }
        let body = self
            .held
            .strip_suffix(b"\n")
            .unwrap_or(self.held.as_slice());
        self.inner.write_all(body)?;
        let times = if repeats == 1 { "time" } else { "times" };
        write!(self.inner, " (repeated {repeats} more {times})")?;
        self.inner.write_all(b"\n")
    }

    /// Emit whatever the stream still owes: the pending rollup, then any line
    /// that never received its newline.
    fn settle(&mut self) -> io::Result<()> {
        self.rollup()?;
        if !self.partial.is_empty() {
            let partial = std::mem::take(&mut self.partial);
            self.inner.write_all(&partial)?;
        }
        self.inner.flush()
    }
}

impl<W: Write> Write for Rollup<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut rest = buf;
        while let Some(end) = rest.iter().position(|byte| *byte == b'\n') {
            let (line, tail) = rest.split_at(end + 1);
            if self.partial.is_empty() {
                self.line(line)?;
            } else {
                self.partial.extend_from_slice(line);
                let line = std::mem::take(&mut self.partial);
                self.line(&line)?;
            }
            rest = tail;
        }
        self.partial.extend_from_slice(rest);
        Ok(buf.len())
    }

    /// Flushing hands on what was written; it does not end a run, because a
    /// run ends at a different line and not at a moment.
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Write> Drop for Rollup<W> {
    fn drop(&mut self) {
        let _ = self.settle();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collapsed(writes: &[&str]) -> String {
        let mut sink = Vec::new();
        {
            let mut rollup = Rollup::new(&mut sink);
            for write in writes {
                rollup
                    .write_all(write.as_bytes())
                    .expect("a vector takes every write");
            }
        }
        String::from_utf8(sink).expect("the fixture writes text")
    }

    #[test]
    fn a_run_of_any_length_costs_its_first_telling_and_one_count() {
        assert_eq!(
            collapsed(&["stuck\n", "stuck\n", "stuck\n", "stuck\n", "moved on\n"]),
            "stuck\nstuck (repeated 3 more times)\nmoved on\n",
        );
        assert_eq!(
            collapsed(&["stuck\n", "stuck\n", "moved on\n"]),
            "stuck\nstuck (repeated 1 more time)\nmoved on\n",
        );
    }

    #[test]
    fn distinct_lines_keep_their_order_and_singletons_stay_single() {
        assert_eq!(
            collapsed(&["first\n", "second\n", "third\n"]),
            "first\nsecond\nthird\n",
        );
    }

    #[test]
    fn a_recurrence_after_another_line_is_a_new_run_not_a_total() {
        assert_eq!(
            collapsed(&["a\n", "a\n", "b\n", "a\n", "a\n", "c\n"]),
            "a\na (repeated 1 more time)\nb\na\na (repeated 1 more time)\nc\n",
        );
    }

    #[test]
    fn a_run_still_open_at_shutdown_is_counted_before_the_stream_closes() {
        assert_eq!(
            collapsed(&["stuck\n", "stuck\n", "stuck\n"]),
            "stuck\nstuck (repeated 2 more times)\n",
        );
    }

    #[test]
    fn a_line_split_across_writes_is_one_line_and_an_unfinished_one_survives() {
        assert_eq!(
            collapsed(&["par", "tial\n", "partial\n", "tail without a newline"]),
            "partial\npartial (repeated 1 more time)\ntail without a newline",
        );
        assert_eq!(
            collapsed(&["one\ntwo\ntwo\n", "two\nthree\n"]),
            "one\ntwo\ntwo (repeated 2 more times)\nthree\n",
        );
    }
}
