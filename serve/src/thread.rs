//! Causal walks over the authority snapshot and current head positions.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use crate::view::pack_ranked;

/// Validated causal index derived from one immutable authority snapshot.
#[derive(Clone, Debug)]
pub struct ThreadIndex {
    entries: BTreeMap<wire::Seq, wire::Envelope<wire::AnyFact>>,
    horizon: Option<wire::Seq>,
}

impl ThreadIndex {
    /// Build an index, refusing dangling or cross-origin causes.
    pub fn from_snapshot(snapshot: &journal::FactSnapshot) -> Result<Self, ThreadError> {
        let mut entries = BTreeMap::<wire::Seq, wire::Envelope<wire::AnyFact>>::new();
        for envelope in snapshot.facts() {
            if let Some(cause) = envelope.cause {
                let parent = entries.get(&cause).ok_or(ThreadError::MissingCause {
                    seq: envelope.seq,
                    cause,
                })?;
                if parent.origin != envelope.origin {
                    return Err(ThreadError::CrossedOrigin {
                        seq: envelope.seq,
                        cause,
                    });
                }
            }
            entries.insert(envelope.seq, envelope.clone());
        }
        Ok(Self {
            entries,
            horizon: snapshot.horizon(),
        })
    }

    /// Pack one same-origin cause chain from `position` toward its root.
    pub fn walk(
        &self,
        position: wire::Seq,
        limit: usize,
    ) -> Result<wire::Packed<wire::Envelope<wire::AnyFact>>, ThreadError> {
        let horizon = self.horizon.ok_or(ThreadError::UnknownPosition(position))?;
        let mut rows = Vec::new();
        let mut cursor = Some(position);
        while let Some(current) = cursor {
            let envelope = self
                .entries
                .get(&current)
                .ok_or(ThreadError::UnknownPosition(current))?;
            rows.push(envelope.clone());
            cursor = envelope.cause;
        }
        Ok(pack_ranked(rows, limit, horizon, |envelope| envelope.seq))
    }
}

/// Bus-backed reader that takes each walk from one queued authority snapshot.
#[derive(Debug)]
pub struct ThreadReader<'a> {
    bus: &'a bus::Bus,
}

impl<'a> ThreadReader<'a> {
    #[must_use]
    pub const fn new(bus: &'a bus::Bus) -> Self {
        Self { bus }
    }

    pub fn walk(
        &self,
        position: wire::Seq,
        budget: journal::FactReadBudget,
    ) -> Result<wire::Packed<wire::Envelope<wire::AnyFact>>, ThreadReadError> {
        self.bus
            .app_thread(position, budget)
            .map_err(ThreadReadError::Bus)?
            .ok_or(ThreadReadError::Thread(ThreadError::UnknownPosition(
                position,
            )))
    }
}

/// A causal index is inconsistent with the immutable fact stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThreadError {
    UnknownPosition(wire::Seq),
    MissingCause { seq: wire::Seq, cause: wire::Seq },
    CrossedOrigin { seq: wire::Seq, cause: wire::Seq },
}

impl fmt::Display for ThreadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPosition(position) => {
                write!(formatter, "thread position {position} is absent")
            }
            Self::MissingCause { seq, cause } => {
                write!(
                    formatter,
                    "thread fact {seq} names absent prior cause {cause}"
                )
            }
            Self::CrossedOrigin { seq, cause } => {
                write!(
                    formatter,
                    "thread fact {seq} crosses the origin of cause {cause}"
                )
            }
        }
    }
}

impl Error for ThreadError {}

/// The bus could not provide a snapshot or the snapshot carried a bad thread.
#[derive(Debug)]
pub enum ThreadReadError {
    Bus(bus::BusReadError),
    Thread(ThreadError),
}

impl fmt::Display for ThreadReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bus(error) => error.fmt(formatter),
            Self::Thread(error) => error.fmt(formatter),
        }
    }
}

impl Error for ThreadReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Bus(error) => Some(error),
            Self::Thread(error) => Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The journal position of a fixture's `ordinal`th fact.
    fn position(ordinal: u64) -> wire::Seq {
        wire::Seq::new(wire::Seq::FIRST.get() + ordinal - 1).expect("fixture position")
    }

    fn envelope(seq: u64, cause: Option<u64>, origin: &str) -> wire::Envelope<wire::AnyFact> {
        let seq = position(seq).get();
        let cause = cause.map_or_else(String::new, |value| {
            format!(r#", "cause": {}"#, position(value).get())
        });
        let document = format!(
            r#"{{"v":1,"seq":{seq},"at":"2026-08-01T00:00:00.000Z","actor":"principal:00000000-0000-0000-0000-000000000000","origin":"{origin}"{cause},"fact":{{"kind":"task::woken","task":"1kKRRsDPx"}}}}"#
        );
        wire::decode(document.as_bytes()).expect("canonical envelope fixture")
    }

    fn snapshot(rows: Vec<wire::Envelope<wire::AnyFact>>) -> journal::FactSnapshot {
        journal::FactSnapshot::from_facts(rows).expect("contiguous snapshot")
    }

    #[test]
    fn a_walk_follows_one_cause_chain_and_names_overflow() {
        let snapshot = snapshot(vec![
            envelope(1, None, "task:1kKRRsDPx"),
            envelope(2, Some(1), "task:1kKRRsDPx"),
            envelope(3, Some(2), "task:1kKRRsDPx"),
            envelope(4, None, "task:1kKRRsDQx"),
        ]);
        let index = ThreadIndex::from_snapshot(&snapshot).expect("valid causal index");
        let walked = index.walk(position(3), 2).expect("walk");

        assert_eq!(
            walked.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
            [position(3), position(2)]
        );
        assert_eq!(
            walked.overflow,
            Some(wire::Overflow {
                omitted: 1,
                continuation: position(1),
            })
        );
        assert_eq!(walked.horizon, position(4));
    }

    #[test]
    fn a_cause_cannot_cross_origins() {
        let snapshot = snapshot(vec![
            envelope(1, None, "task:1kKRRsDPx"),
            envelope(2, Some(1), "task:1kKRRsDQx"),
        ]);

        assert_eq!(
            ThreadIndex::from_snapshot(&snapshot).expect_err("cross-origin cause refuses"),
            ThreadError::CrossedOrigin {
                seq: position(2),
                cause: position(1),
            }
        );
    }
}
