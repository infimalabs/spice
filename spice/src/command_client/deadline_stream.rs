//! One socket that cannot outlive the caller's bound.
//!
//! Every read, write, and flush is configured from the same deadline the
//! connect was bounded by, so a resident that stops answering ends the command
//! at the bound rather than stalling a person's terminal. The stream also
//! remembers what ended its last operation: a bound that elapsed is not a peer
//! that failed, and only the stream is in a position to tell them apart.

use super::*;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum IoDirection {
    Read,
    Write,
}

/// The last transport failure, and whether the bound is what ended it.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct IoFailure {
    pub(super) direction: IoDirection,
    pub(super) deadline_elapsed: bool,
}

pub(super) struct DeadlineStream {
    stream: TcpStream,
    /// The caller's declared bound, which faults name; `deadline` may be
    /// nearer when an earlier attempt already spent part of it.
    pub(super) bound: Duration,
    deadline: Instant,
    pub(super) last_failure: Option<IoFailure>,
}

impl DeadlineStream {
    pub(super) fn connect(
        address: &SocketAddr,
        deadline: Instant,
        bound: Duration,
    ) -> io::Result<Self> {
        let stream = TcpStream::connect_timeout(address, remaining(deadline)?)?;
        Ok(Self {
            stream,
            bound,
            deadline,
            last_failure: None,
        })
    }

    pub(super) const fn deadline(&self) -> Instant {
        self.deadline
    }

    fn record(&mut self, direction: IoDirection, error: &io::Error) {
        self.last_failure = Some(IoFailure {
            direction,
            // A socket that reaches its configured timeout reports it as a
            // would-block on Unix and as a timeout elsewhere; `remaining`
            // raises the same timeout once the whole bound is spent.
            deadline_elapsed: matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ),
        });
    }

    fn read_timeout(&mut self) -> io::Result<Duration> {
        let timeout = remaining(self.deadline)?;
        self.stream.set_read_timeout(Some(timeout))?;
        Ok(timeout)
    }

    fn write_timeout(&mut self) -> io::Result<Duration> {
        let timeout = remaining(self.deadline)?;
        self.stream.set_write_timeout(Some(timeout))?;
        Ok(timeout)
    }
}

impl Read for DeadlineStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        self.last_failure = None;
        if let Err(error) = self.read_timeout() {
            self.record(IoDirection::Read, &error);
            return Err(error);
        }
        match self.stream.read(bytes) {
            Ok(read) => Ok(read),
            Err(error) => {
                self.record(IoDirection::Read, &error);
                Err(error)
            }
        }
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        self.last_failure = None;
        if let Err(error) = self.write_timeout() {
            self.record(IoDirection::Write, &error);
            return Err(error);
        }
        match self.stream.write(bytes) {
            Ok(written) => Ok(written),
            Err(error) => {
                self.record(IoDirection::Write, &error);
                Err(error)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.last_failure = None;
        if let Err(error) = self.write_timeout() {
            self.record(IoDirection::Write, &error);
            return Err(error);
        }
        match self.stream.flush() {
            Ok(()) => Ok(()),
            Err(error) => {
                self.record(IoDirection::Write, &error);
                Err(error)
            }
        }
    }
}

pub(super) fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "resident command deadline elapsed"))
}
