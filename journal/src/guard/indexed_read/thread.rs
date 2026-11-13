//! Bounded oldest-first pages of one cause thread through the thread index.
//!
//! A thread is every fact whose indexed root is the same fact. Its pages walk
//! `thread_index(root, seq)` and decode only the returned envelopes, so a read
//! costs its page rather than the journal or the whole thread.

use std::{error::Error, fmt};

use rusqlite::{OptionalExtension, params};

use super::{
    FactReadBudget, GuardError, JournalRead, admitted_envelope_length, decode_bounded_fact,
    read_metrics,
};

/// Thread lookup found no unambiguous reference, or its index could not be read.
#[derive(Debug)]
pub enum ThreadError {
    Read(GuardError),
    NotFound {
        reference: String,
    },
    Ambiguous {
        reference: String,
        roots: Vec<wire::Seq>,
    },
}

impl fmt::Display for ThreadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(error) => write!(formatter, "thread index read failed: {error}"),
            Self::NotFound { reference } => {
                write!(formatter, "thread reference {reference:?} was not found")
            }
            Self::Ambiguous { reference, roots } => write!(
                formatter,
                "thread reference {reference:?} names multiple roots {}",
                roots
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

impl Error for ThreadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read(error) => Some(error),
            Self::NotFound { .. } | Self::Ambiguous { .. } => None,
        }
    }
}

fn sql(value: u64, what: &str) -> Result<i64, GuardError> {
    i64::try_from(value).map_err(|_| GuardError::Invalid(format!("{what} exceeds SQLite")))
}

fn indexed_seq(value: i64) -> Result<wire::Seq, GuardError> {
    u64::try_from(value)
        .ok()
        .and_then(wire::Seq::new)
        .ok_or_else(|| {
            GuardError::Invalid(format!(
                "thread index sequence {value} is outside the wire range"
            ))
        })
}

impl JournalRead<'_> {
    fn thread_horizon(&self) -> Result<i64, GuardError> {
        sql(self.cursor.map_or(0, wire::Seq::get), "thread horizon")
    }

    /// Resolve one sequence or exact origin to the root of exactly one thread.
    pub fn thread_root(&self, reference: &str) -> Result<wire::Seq, ThreadError> {
        let not_found = || ThreadError::NotFound {
            reference: reference.to_owned(),
        };
        let horizon = self.thread_horizon().map_err(ThreadError::Read)?;
        let store = |error| ThreadError::Read(GuardError::Store(error));
        read_metrics::observe_query();
        let roots = if let Ok(seq) = reference.parse::<wire::Seq>() {
            let seq = i64::try_from(seq.get()).map_err(|_| not_found())?;
            self.connection
                .query_row(
                    "SELECT root FROM thread_index WHERE seq = ?1 AND seq <= ?2",
                    params![seq, horizon],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(store)?
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT DISTINCT root FROM thread_index \
                     WHERE origin = ?1 AND seq <= ?2 ORDER BY root",
                )
                .map_err(store)?;
            statement
                .query_map(params![reference, horizon], |row| row.get::<_, i64>(0))
                .map_err(store)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(store)?
        };
        match roots.as_slice() {
            [] => Err(not_found()),
            [root] => indexed_seq(*root).map_err(ThreadError::Read),
            _ => Err(ThreadError::Ambiguous {
                reference: reference.to_owned(),
                roots: roots
                    .iter()
                    .map(|root| indexed_seq(*root))
                    .collect::<Result<_, _>>()
                    .map_err(ThreadError::Read)?,
            }),
        }
    }

    /// Read one bounded oldest-first page of a thread starting at `at`.
    ///
    /// `at` is inclusive and must be a fact of this thread; the overflow names
    /// the first withheld fact as its continuation and counts every withheld
    /// fact, so the next call passes that continuation as `at`. `None` means
    /// `at` is not a fact of this thread, or there is no durable authority.
    pub fn thread_page(
        &self,
        root: wire::Seq,
        at: Option<wire::Seq>,
        budget: FactReadBudget,
    ) -> Result<Option<wire::Packed<wire::Envelope<wire::AnyFact>>>, GuardError> {
        let Some(horizon) = self.cursor else {
            return Ok(None);
        };
        let horizon_sql = sql(horizon.get(), "thread horizon")?;
        let root_sql = sql(root.get(), "thread root")?;
        let first = sql(at.map_or(0, wire::Seq::get), "thread cursor")?;
        if at.is_some() {
            read_metrics::observe_query();
            let member = self
                .connection
                .query_row(
                    "SELECT 1 FROM thread_index WHERE root = ?1 AND seq = ?2 AND seq <= ?3",
                    params![root_sql, first, horizon_sql],
                    |_| Ok(()),
                )
                .optional()
                .map_err(GuardError::Store)?;
            if member.is_none() {
                return Ok(None);
            }
        }
        read_metrics::observe_query();
        let total = self
            .connection
            .query_row(
                "SELECT count(*) FROM thread_index WHERE root = ?1 AND seq >= ?2 AND seq <= ?3",
                params![root_sql, first, horizon_sql],
                |row| row.get::<_, i64>(0),
            )
            .map_err(GuardError::Store)?;
        let total = u64::try_from(total)
            .map_err(|_| GuardError::Invalid(format!("thread count {total} is negative")))?;
        let query_limit = i64::try_from(budget.rows().saturating_add(1))
            .map_err(|_| GuardError::Invalid("thread page limit exceeds SQLite".to_owned()))?;
        read_metrics::observe_query();
        let mut statement = self
            .connection
            .prepare(
                "SELECT fact.seq, fact.kind, length(fact.envelope), fact.envelope \
                 FROM thread_index AS thread JOIN fact_envelopes AS fact ON fact.seq = thread.seq \
                 WHERE thread.root = ?1 AND thread.seq >= ?2 AND thread.seq <= ?3 \
                 ORDER BY thread.seq LIMIT ?4",
            )
            .map_err(GuardError::Store)?;
        let mut rows = statement
            .query(params![root_sql, first, horizon_sql, query_limit])
            .map_err(GuardError::Store)?;
        let mut facts: Vec<wire::Envelope<wire::AnyFact>> = Vec::with_capacity(budget.rows());
        let mut decoded_bytes = 0usize;
        let mut continuation = None;
        while let Some(row) = rows.next().map_err(GuardError::Store)? {
            let seq = row.get::<_, i64>(0).map_err(GuardError::Store)?;
            read_metrics::observe_row();
            let length =
                admitted_envelope_length(seq, row.get::<_, i64>(2).map_err(GuardError::Store)?)?;
            if facts.len() == budget.rows()
                || (!facts.is_empty()
                    && decoded_bytes.saturating_add(length) > budget.decoded_bytes())
            {
                continuation = Some(indexed_seq(seq)?);
                break;
            }
            let (fact, length) = decode_bounded_fact(row, seq, 1, 2, 3)?;
            decoded_bytes = decoded_bytes.saturating_add(length);
            facts.push(fact);
        }
        let returned = u64::try_from(facts.len()).unwrap_or(u64::MAX);
        Ok(Some(wire::Packed {
            rows: facts,
            overflow: continuation.map(|continuation| wire::Overflow {
                omitted: total.saturating_sub(returned),
                continuation,
            }),
            horizon,
        }))
    }
}
