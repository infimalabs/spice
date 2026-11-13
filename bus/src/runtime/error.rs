//! Typed failures of the dedicated writer and its queued reads.

use std::error::Error;
use std::fmt;

use journal::{GuardError, VerifyError};

/// The dedicated writer could not be reached or did not end normally.
#[derive(Debug)]
pub enum BusError {
    /// The operating system refused the writer thread.
    Start(std::io::Error),
    /// The writer queue is closed.
    Closed,
    /// The writer thread panicked while owning authority.
    WriterPanicked,
}

impl fmt::Display for BusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start(error) => write!(formatter, "bus writer did not start: {error}"),
            Self::Closed => formatter.write_str("bus writer is closed"),
            Self::WriterPanicked => formatter.write_str("bus writer panicked"),
        }
    }
}

impl Error for BusError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Start(error) => Some(error),
            Self::Closed | Self::WriterPanicked => None,
        }
    }
}

/// A queued read could not be returned.
#[derive(Debug)]
pub enum BusReadError {
    Bus(BusError),
    Head(GuardError),
}

impl fmt::Display for BusReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bus(error) => error.fmt(formatter),
            Self::Head(error) => error.fmt(formatter),
        }
    }
}

impl Error for BusReadError {}

/// A queued authority verification could not be returned.
#[derive(Debug)]
pub enum BusVerifyError {
    Bus(BusError),
    Journal(VerifyError),
}

impl fmt::Display for BusVerifyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bus(error) => error.fmt(formatter),
            Self::Journal(error) => error.fmt(formatter),
        }
    }
}

impl Error for BusVerifyError {}
