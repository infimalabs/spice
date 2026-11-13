//! Compact predicates for bounded taste evidence and lane boundary reads.

use rusqlite::{Connection, OptionalExtension, params};

pub(crate) fn record(
    connection: &Connection,
    fact: &wire::AnyFact,
    seq: wire::Seq,
) -> Result<(), rusqlite::Error> {
    if let Some(body) = fact.read::<wire::body::LaneSeated>() {
        connection.execute(
            "INSERT INTO lane_seats(lane, vendor, seq) VALUES (?1, ?2, ?3) \
             ON CONFLICT(lane) DO UPDATE SET vendor = excluded.vendor, seq = excluded.seq",
            params![body.lane.to_string(), body.vendor.to_string(), sql_seq(seq)],
        )?;
        return Ok(());
    }
    if let Some(body) = fact.read::<wire::body::LaneCheckpoint>() {
        let turn_seq =
            (body.boundary == wire::body::CheckpointBoundary::Turn).then_some(sql_seq(seq));
        connection.execute(
            "INSERT INTO lane_checkpoints(lane, seq, turn_seq) VALUES (?1, ?2, ?3) \
             ON CONFLICT(lane) DO UPDATE SET seq = excluded.seq, \
             turn_seq = COALESCE(excluded.turn_seq, lane_checkpoints.turn_seq)",
            params![body.lane.to_string(), sql_seq(seq), turn_seq],
        )?;
        return Ok(());
    }
    if let Some(body) = fact.read::<wire::body::TasteReminded>() {
        insert_fire(connection, &body.taste, seq)?;
        return Ok(());
    }
    if let Some(body) = fact.read::<wire::body::TasteJudged>() {
        let confirmed = matches!(body.decision, wire::body::TasteDecision::Confirmed { .. });
        insert_judgment(connection, &body.taste, seq, confirmed)?;
        if confirmed {
            insert_fire(connection, &body.taste, seq)?;
        }
        return Ok(());
    }
    Ok(())
}

fn insert_fire(
    connection: &Connection,
    taste: &str,
    seq: wire::Seq,
) -> Result<(), rusqlite::Error> {
    let ordinal = connection
        .query_row(
            "SELECT ordinal FROM taste_fires WHERE taste = ?1 ORDER BY seq DESC LIMIT 1",
            [taste],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0);
    connection.execute(
        "INSERT INTO taste_fires(seq, taste, ordinal) VALUES (?1, ?2, ?3)",
        params![sql_seq(seq), taste, ordinal + 1],
    )?;
    Ok(())
}

fn insert_judgment(
    connection: &Connection,
    taste: &str,
    seq: wire::Seq,
    is_confirmed: bool,
) -> Result<(), rusqlite::Error> {
    let previous = connection
        .query_row(
            "SELECT ordinal, confirmed, cleared FROM taste_judgments \
             WHERE taste = ?1 ORDER BY ordinal DESC LIMIT 1",
            [taste],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?
        .unwrap_or((0, 0, 0));
    connection.execute(
        "INSERT INTO taste_judgments(seq, taste, ordinal, confirmed, cleared) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            sql_seq(seq),
            taste,
            previous.0 + 1,
            previous.1 + i64::from(is_confirmed),
            previous.2 + i64::from(!is_confirmed),
        ],
    )?;
    Ok(())
}

fn sql_seq(seq: wire::Seq) -> i64 {
    i64::try_from(seq.get()).expect("journal positions fit SQLite")
}
