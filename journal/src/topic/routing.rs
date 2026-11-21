//! Public topology used to bind forecast destinations. Empty nodes also
//! reserve their spelling; an authored description makes its node a fixed
//! point of the board's topic tree.

use rusqlite::Connection;

#[derive(Debug, Eq, PartialEq)]
pub struct TopicRoutingNode {
    pub id: i64,
    pub path: wire::Topic,
    /// Operator-authored; empty when the node was never described.
    pub description: String,
}

pub(crate) fn nodes(connection: &Connection) -> Result<Vec<TopicRoutingNode>, String> {
    select(
        connection,
        "SELECT node, description FROM topic_heads ORDER BY node",
    )
}

pub(crate) fn described(connection: &Connection) -> Result<Vec<TopicRoutingNode>, String> {
    select(
        connection,
        "SELECT node, description FROM topic_heads WHERE description != '' ORDER BY node",
    )
}

fn select(connection: &Connection, query: &str) -> Result<Vec<TopicRoutingNode>, String> {
    let mut paths = super::Paths::new(connection);
    let mut statement = connection
        .prepare(query)
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| error.to_string())?;
    let mut nodes = Vec::new();
    for row in rows {
        let (id, description) = row.map_err(|error| error.to_string())?;
        let path = paths.path(id)?;
        if path.private_owner().is_none() && !wire::is_oops_topic(&path) {
            nodes.push(TopicRoutingNode {
                id,
                path,
                description,
            });
        }
    }
    Ok(nodes)
}
