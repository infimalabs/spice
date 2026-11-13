//! Peer-line projections checked against the fold of their facts.
//!
//! `peer_line_heads`, `peer_pair_heads` and `lane_session_declarations` are
//! derived inside each committing write by [`crate::peer_lines::record`]. The
//! verifier replays every retained fact through that same fold into a scratch
//! database and compares each stored row with the row it would derive, so a
//! lost terminal, a missing line or a row no fact derives is reported rather
//! than trusted. Facts stay the authority; nothing stored is rewritten.

use std::collections::BTreeMap;

use rusqlite::Connection;
use rusqlite::types::Value;

use super::{FactRecord, VerifyError, Violation, ViolationClass};

/// One derived table: its name, the key that identifies a row, and the
/// columns that must agree with the fold.
struct Projection {
    table: &'static str,
    key: &'static str,
    columns: &'static str,
}

const PROJECTIONS: [Projection; 3] = [
    Projection {
        table: "peer_line_heads",
        key: "line",
        columns: "actor_a, actor_b, terminal, head_seq, body",
    },
    Projection {
        table: "peer_pair_heads",
        key: "from_lane || '>' || to_lane",
        columns: "head_seq, body",
    },
    Projection {
        table: "lane_session_declarations",
        key: "CAST(seq AS TEXT)",
        columns: "lane, source, member, offset, occupant",
    },
];

pub(super) fn verify(
    connection: &Connection,
    facts: &BTreeMap<u64, FactRecord>,
    violations: &mut Vec<Violation>,
) -> Result<usize, VerifyError> {
    let expected = expected(facts, violations).map_err(VerifyError::Sqlite)?;
    let mut count = 0;
    for projection in &PROJECTIONS {
        let mut stored = rows(connection, projection).map_err(VerifyError::Sqlite)?;
        count += stored.len();
        for (key, row) in rows(&expected, projection).map_err(VerifyError::Sqlite)? {
            let detail = match stored.remove(&key) {
                Some(found) if found == row => continue,
                Some(_) => "row disagrees with the fold of its facts",
                None => "row is missing although its facts derive one",
            };
            violations.push(Violation::new(
                ViolationClass::Head,
                format!("{}:{key}", projection.table),
                detail,
            ));
        }
        for key in stored.into_keys() {
            violations.push(Violation::new(
                ViolationClass::Head,
                format!("{}:{key}", projection.table),
                "row has no fact deriving it",
            ));
        }
    }
    Ok(count)
}

/// Fold every decoded fact into a scratch database through the commit fold.
fn expected(
    facts: &BTreeMap<u64, FactRecord>,
    violations: &mut Vec<Violation>,
) -> Result<Connection, rusqlite::Error> {
    let scratch = Connection::open_in_memory()?;
    // The scratch holds no `fact_envelopes`, so the projections' references to
    // it cannot be enforced here; the stored database's own check covers them.
    scratch.execute_batch("PRAGMA foreign_keys = OFF;")?;
    scratch.execute_batch(crate::peer_lines::SCHEMA_SQL)?;
    scratch.execute_batch(crate::peer_lines::PAIR_SCHEMA_SQL)?;
    for record in facts.values() {
        let Some(envelope) = &record.envelope else {
            continue;
        };
        if let Err(error) =
            crate::peer_lines::record(&scratch, &envelope.actor, &envelope.fact, envelope.seq)
        {
            violations.push(Violation::new(
                ViolationClass::Head,
                format!("fact_envelopes:{}", envelope.seq),
                format!("fact does not fold into the peer-line projections: {error}"),
            ));
        }
    }
    Ok(scratch)
}

fn rows(
    connection: &Connection,
    projection: &Projection,
) -> Result<BTreeMap<String, Vec<Value>>, rusqlite::Error> {
    let mut statement = connection.prepare(&format!(
        "SELECT {}, {} FROM {}",
        projection.key, projection.columns, projection.table
    ))?;
    let width = statement.column_count();
    let rows = statement.query_map([], |row| {
        let key = row.get::<_, String>(0)?;
        let values = (1..width)
            .map(|index| row.get::<_, Value>(index))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((key, values))
    })?;
    rows.collect()
}
