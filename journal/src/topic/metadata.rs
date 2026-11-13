//! Node-bound descriptions and history survive renames and subtree moves.

use rusqlite::{Connection, params};

pub(crate) const HISTORY_SQL: &str = "CREATE TABLE topic_history (
    node INTEGER NOT NULL REFERENCES topic_heads(node),
    seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    PRIMARY KEY (node, seq)
) STRICT;
CREATE INDEX topic_history_seq ON topic_history(seq);";

pub(crate) const REVISION_SQL: &str = "CREATE TABLE IF NOT EXISTS topic_revision (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    seq INTEGER REFERENCES fact_envelopes(seq),
    next_node INTEGER NOT NULL CHECK (next_node > 0)
) STRICT;
INSERT OR IGNORE INTO topic_revision(singleton, seq, next_node) VALUES (1, NULL, 1);";

fn advance_revision(connection: &Connection, seq: i64) -> rusqlite::Result<()> {
    connection.execute(
        "UPDATE topic_revision SET seq = MAX(COALESCE(seq, 0), ?1) WHERE singleton = 1",
        [seq],
    )?;
    Ok(())
}

/// Advance the routing generation when a task gains or releases live nodes.
pub(crate) fn task_changed(connection: &Connection, seq: i64) -> rusqlite::Result<()> {
    advance_revision(connection, seq)
}

pub(super) fn allocate_node(connection: &Connection) -> rusqlite::Result<i64> {
    connection.query_row(
        "UPDATE topic_revision SET next_node = next_node + 1 WHERE singleton = 1 \
         RETURNING next_node - 1",
        [],
        |row| row.get(0),
    )
}

pub(super) fn record(connection: &Connection, node: i64, seq: i64) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT OR IGNORE INTO topic_history(node, seq) VALUES (?1, ?2)",
        params![node, seq],
    )?;
    advance_revision(connection, seq)
}

pub(super) fn moved(connection: &Connection, node: i64, seq: i64) -> rusqlite::Result<()> {
    connection.execute(
        "WITH RECURSIVE subtree(node) AS (
            SELECT ?1 UNION SELECT q.node FROM topic_heads q JOIN subtree s ON q.parent = s.node
        ) INSERT OR IGNORE INTO topic_history(node, seq) SELECT node, ?2 FROM subtree",
        params![node, seq],
    )?;
    advance_revision(connection, seq)
}

pub(crate) fn describe(
    connection: &Connection,
    described: &wire::topic::TopicDescribed,
    seq: i64,
) -> rusqlite::Result<()> {
    let node = if described.description.is_empty() {
        let Some(node) = super::lookup(connection, &described.topic)? else {
            return advance_revision(connection, seq);
        };
        record(connection, node, seq)?;
        node
    } else {
        super::resolve(connection, &described.topic, seq)?
    };
    connection.execute(
        "UPDATE topic_heads SET description = ?1, head_seq = ?2 WHERE node = ?3",
        params![described.description, seq, node],
    )?;
    Ok(())
}

pub(crate) fn node(
    connection: &Connection,
    path: &wire::Topic,
) -> Result<Option<wire::topic::TopicNode>, String> {
    let Some(id) = super::lookup(connection, path).map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    connection
        .query_row(
            "SELECT parent, description, head_seq FROM topic_heads WHERE node = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .map_err(|error| error.to_string())
        .and_then(|(parent, description, revision)| {
            Ok(Some(wire::topic::TopicNode {
                id,
                path: path.clone(),
                parent,
                description,
                revision: u64::try_from(revision)
                    .ok()
                    .and_then(wire::Seq::new)
                    .ok_or_else(|| "topic revision is not positive".to_owned())?,
            }))
        })
}

pub(crate) fn revision(connection: &Connection) -> Result<Option<u64>, String> {
    let value: Option<i64> = connection
        .query_row(
            "SELECT seq FROM topic_revision WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    value
        .map(|value| u64::try_from(value).map_err(|error| error.to_string()))
        .transpose()
}

/// Read a page by durable sequence; no task head or current path is its key.
pub(crate) fn history(
    connection: &Connection,
    node: i64,
    after: Option<wire::Seq>,
) -> Result<(Vec<wire::Envelope<wire::AnyFact>>, Option<wire::Seq>), String> {
    const PAGE: usize = 64;
    let mut statement = connection
        .prepare(
            "SELECT f.envelope FROM topic_history h JOIN fact_envelopes f ON f.seq = h.seq
         WHERE h.node = ?1 AND h.seq > ?2 ORDER BY h.seq LIMIT ?3",
        )
        .map_err(|error| error.to_string())?;
    let after =
        i64::try_from(after.map_or(0, wire::Seq::get)).map_err(|error| error.to_string())?;
    let limit = i64::try_from(PAGE + 1).expect("history page fits SQLite");
    let rows = statement
        .query_map(params![node, after, limit], |row| row.get::<_, Vec<u8>>(0))
        .map_err(|error| error.to_string())?;
    let mut facts = rows
        .map(|row| {
            wire::decode(&row.map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<wire::Envelope<wire::AnyFact>>, String>>()?;
    let next = if facts.len() > PAGE {
        facts.pop();
        facts.last().map(|fact| fact.seq)
    } else {
        None
    };
    Ok((facts, next))
}
