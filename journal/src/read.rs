use std::error::Error;
use std::fmt;

use rusqlite::{Connection, params_from_iter, types::Value};

use crate::{Journal, JournalReader};

/// One immutable, contiguous view of the journal fact stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactSnapshot {
    facts: Vec<wire::Envelope<wire::AnyFact>>,
    horizon: Option<wire::Seq>,
}

/// One immutable view of only the facts that can change steering authority.
///
/// Positions remain their original journal positions and therefore need not be
/// contiguous. The horizon is the cursor of the complete journal generation
/// from which this projection was selected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteeringSnapshot {
    facts: Vec<wire::Envelope<wire::AnyFact>>,
    horizon: Option<wire::Seq>,
}

impl SteeringSnapshot {
    /// Steering-authority facts in ascending journal order.
    #[must_use]
    pub fn facts(&self) -> &[wire::Envelope<wire::AnyFact>] {
        &self.facts
    }

    /// Complete journal cursor covered by this selected projection.
    #[must_use]
    pub const fn horizon(&self) -> Option<wire::Seq> {
        self.horizon
    }
}

impl FactSnapshot {
    /// Validates a fact sequence for use by a disposable materialization.
    pub fn from_facts(facts: Vec<wire::Envelope<wire::AnyFact>>) -> Result<Self, SnapshotError> {
        let mut expected = wire::Seq::FIRST.get();
        for fact in &facts {
            if !fact.v.is_current() {
                return Err(SnapshotError::Invalid {
                    seq: Some(fact.seq.get()),
                    detail: "fact snapshot contains another wire version".to_owned(),
                });
            }
            if fact.seq.get() != expected {
                return Err(SnapshotError::Invalid {
                    seq: Some(fact.seq.get()),
                    detail: format!("expected contiguous seq {expected}"),
                });
            }
            expected = expected.saturating_add(1);
        }
        Ok(Self::from_verified(facts))
    }

    /// Adopt a stream verification already proved contiguous and current.
    pub(crate) fn from_verified(facts: Vec<wire::Envelope<wire::AnyFact>>) -> Self {
        let horizon = facts.last().map(|fact| fact.seq);
        Self { facts, horizon }
    }

    /// Facts in ascending journal order.
    #[must_use]
    pub fn facts(&self) -> &[wire::Envelope<wire::AnyFact>] {
        &self.facts
    }

    /// Greatest fact position visible in this snapshot.
    #[must_use]
    pub const fn horizon(&self) -> Option<wire::Seq> {
        self.horizon
    }
}

impl Journal {
    /// Reads a canonical, contiguous fact snapshot without mutating authority.
    pub fn fact_snapshot(&self) -> Result<FactSnapshot, SnapshotError> {
        read_snapshot(&self.connection)
    }

    /// Reads only facts capable of changing the bounded steering authority.
    ///
    /// This recovery surface deliberately excludes unrelated world history.
    /// The returned horizon still covers the complete journal generation so a
    /// rebuilt projection can advance atomically to the authoritative cursor.
    pub fn steering_snapshot(&self) -> Result<SteeringSnapshot, SnapshotError> {
        read_steering_snapshot(&self.connection)
    }
}

impl JournalReader {
    /// Reads the canonical authority stream without writer capability,
    /// creation, or migration.
    pub fn fact_snapshot(&self) -> Result<FactSnapshot, SnapshotError> {
        read_snapshot(&self.connection)
    }
}

pub(crate) fn read_snapshot(connection: &Connection) -> Result<FactSnapshot, SnapshotError> {
    let mut statement = connection
        .prepare("SELECT seq, kind, length(envelope), envelope FROM fact_envelopes ORDER BY seq")
        .map_err(SnapshotError::Sqlite)?;
    let mut rows = statement.query([]).map_err(SnapshotError::Sqlite)?;
    let mut facts = Vec::new();
    while let Some(row) = rows.next().map_err(SnapshotError::Sqlite)? {
        let (indexed_seq, indexed_kind, bytes) = read_fact_row(row)?;
        facts.push(decode_fact(indexed_seq, &indexed_kind, &bytes)?);
    }
    FactSnapshot::from_facts(facts)
}

fn read_steering_snapshot(connection: &Connection) -> Result<SteeringSnapshot, SnapshotError> {
    let raw_horizon = connection
        .query_row(
            "SELECT seq FROM cursor_head WHERE singleton = 1",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(SnapshotError::Sqlite)?;
    let horizon = match raw_horizon {
        None => None,
        Some(found) => Some(
            u64::try_from(found)
                .ok()
                .and_then(wire::Seq::new)
                .ok_or_else(|| SnapshotError::Invalid {
                    seq: u64::try_from(found).ok(),
                    detail: format!("journal cursor {found} is not a fact position"),
                })?,
        ),
    };
    let Some(horizon) = horizon else {
        return Ok(SteeringSnapshot {
            facts: Vec::new(),
            horizon: None,
        });
    };

    let sql_horizon = i64::try_from(horizon.get()).map_err(|_| SnapshotError::Invalid {
        seq: Some(horizon.get()),
        detail: "journal cursor exceeds SQLite integer range".to_owned(),
    })?;
    let placeholders = (2..=wire::FactKind::STEERING_AUTHORITY.len() + 1)
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT seq, kind, length(envelope), envelope FROM fact_envelopes \
         WHERE seq <= ?1 AND kind IN ({placeholders}) ORDER BY seq"
    );
    let mut parameters = Vec::with_capacity(wire::FactKind::STEERING_AUTHORITY.len() + 1);
    parameters.push(Value::Integer(sql_horizon));
    parameters.extend(
        wire::FactKind::STEERING_AUTHORITY
            .iter()
            .map(|kind| Value::Text(kind.as_str().to_owned())),
    );
    let mut statement = connection.prepare(&sql).map_err(SnapshotError::Sqlite)?;
    let mut rows = statement
        .query(params_from_iter(&parameters))
        .map_err(SnapshotError::Sqlite)?;
    let mut facts = Vec::new();
    while let Some(row) = rows.next().map_err(SnapshotError::Sqlite)? {
        let (indexed_seq, indexed_kind, bytes) = read_fact_row(row)?;
        facts.push(decode_fact(indexed_seq, &indexed_kind, &bytes)?);
    }
    Ok(SteeringSnapshot {
        facts,
        horizon: Some(horizon),
    })
}

fn read_fact_row(row: &rusqlite::Row<'_>) -> Result<(i64, String, Vec<u8>), SnapshotError> {
    let indexed_seq = row.get::<_, i64>(0).map_err(SnapshotError::Sqlite)?;
    let length = row.get::<_, i64>(2).map_err(SnapshotError::Sqlite)?;
    let length = usize::try_from(length).map_err(|_| SnapshotError::Invalid {
        seq: u64::try_from(indexed_seq).ok(),
        detail: format!("fact envelope has invalid length {length}"),
    })?;
    if length > crate::MAX_DURABLE_FACT_ENVELOPE_BYTES {
        return Err(SnapshotError::Invalid {
            seq: u64::try_from(indexed_seq).ok(),
            detail: format!(
                "fact envelope has {length} bytes, exceeding the {}-byte durable limit",
                crate::MAX_DURABLE_FACT_ENVELOPE_BYTES,
            ),
        });
    }
    let indexed_kind = row.get::<_, String>(1).map_err(SnapshotError::Sqlite)?;
    let bytes = row.get::<_, Vec<u8>>(3).map_err(SnapshotError::Sqlite)?;
    Ok((indexed_seq, indexed_kind, bytes))
}

pub(crate) fn decode_fact(
    indexed_seq: i64,
    indexed_kind: &str,
    bytes: &[u8],
) -> Result<wire::Envelope<wire::AnyFact>, SnapshotError> {
    let seq = u64::try_from(indexed_seq).ok();
    let fact = wire::decode::<wire::Envelope<wire::AnyFact>>(bytes).map_err(|error| {
        SnapshotError::Invalid {
            seq,
            detail: format!("fact envelope does not decode: {error}"),
        }
    })?;
    let encoded = wire::encode(&fact).map_err(|error| SnapshotError::Invalid {
        seq,
        detail: format!("fact envelope does not re-encode: {error}"),
    })?;
    if encoded != bytes
        || seq != Some(fact.seq.get())
        || indexed_kind != fact.fact.kind().as_str()
        || !fact.v.is_current()
    {
        return Err(SnapshotError::Invalid {
            seq,
            detail: "fact envelope is noncanonical or disagrees with indexed columns".to_owned(),
        });
    }
    Ok(fact)
}

/// A read-only fact snapshot failure.
#[derive(Debug)]
pub enum SnapshotError {
    /// SQLite could not produce the snapshot.
    Sqlite(rusqlite::Error),
    /// A stored fact was not a canonical member of one contiguous stream.
    Invalid { seq: Option<u64>, detail: String },
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => write!(formatter, "journal fact snapshot failed: {error}"),
            Self::Invalid { seq, detail } => {
                write!(formatter, "journal fact {seq:?} is invalid: {detail}")
            }
        }
    }
}

impl Error for SnapshotError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
            Self::Invalid { .. } => None,
        }
    }
}
