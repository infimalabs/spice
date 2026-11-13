//! Public routing documents are read under the same journal transaction as the tree.

use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::{Connection, params};
use serde::Deserialize;

/// Maximum live task statements contributing to one topic's routing flavor.
const MAX_CURRENT_TASK_DOCUMENTS_PER_TOPIC: i64 = 64;

/// Current public spelling plus authored descriptions and live task text.
#[derive(Debug)]
pub struct TopicRoutingNode {
    pub id: i64,
    pub path: wire::Topic,
    pub description: String,
    pub documents: Vec<Arc<String>>,
}

#[derive(Deserialize)]
struct Statement {
    topic: StoredTopic,
    title: String,
    pitch: String,
    description: Option<String>,
    #[serde(default)]
    term: Vec<String>,
}

#[derive(Deserialize)]
struct StoredTopic {
    node: i64,
}

fn document<'a>(fields: impl Iterator<Item = &'a str>) -> Arc<String> {
    let mut text = String::new();
    for field in fields {
        text.push_str(field);
        text.push('\n');
    }
    Arc::new(text)
}

pub(crate) fn nodes(connection: &Connection) -> Result<Vec<TopicRoutingNode>, String> {
    let mut paths = super::Paths::new(connection);
    let mut statement = connection
        .prepare(
            "SELECT q.node, q.description FROM topic_heads q
             WHERE q.description <> '' OR EXISTS (
                 SELECT 1 FROM task_heads t
                 JOIN workflow_heads w ON w.stamp = t.stamp
                 WHERE json_extract(CAST(w.body AS TEXT), '$.status') = 'pending'
                   AND (
                     json_extract(CAST(t.body AS TEXT), '$.topic.node') = q.node
                     OR json_extract(CAST(t.body AS TEXT), '$.topic.placed') = q.node
                   )
             ) ORDER BY q.node",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| error.to_string())?;
    let mut nodes = HashMap::new();
    for row in rows {
        let (id, description) = row.map_err(|error| error.to_string())?;
        let path = paths.path(id)?;
        if path.as_str().starts_with('/') {
            nodes.insert(
                id,
                TopicRoutingNode {
                    id,
                    path,
                    description,
                    documents: Vec::new(),
                },
            );
        }
    }
    current_documents(connection, &mut nodes)?;
    let mut nodes = nodes.into_values().collect::<Vec<_>>();
    nodes.sort_by_key(|node| node.id);
    Ok(nodes)
}

fn current_documents(
    connection: &Connection,
    nodes: &mut HashMap<i64, TopicRoutingNode>,
) -> Result<(), String> {
    let mut statement = connection
        .prepare(
            "WITH ranked AS (
                 SELECT t.body,
                        json_extract(CAST(t.body AS TEXT), '$.topic.node') AS node,
                        ROW_NUMBER() OVER (
                            PARTITION BY json_extract(CAST(t.body AS TEXT), '$.topic.node')
                            ORDER BY t.head_seq DESC, t.stamp
                        ) AS recency
                 FROM task_heads t
                 JOIN workflow_heads w ON w.stamp = t.stamp
                 WHERE json_extract(CAST(w.body AS TEXT), '$.status') = 'pending'
                   AND json_type(CAST(t.body AS TEXT), '$.topic.node') = 'integer'
                   AND COALESCE(
                     json_extract(CAST(t.body AS TEXT), '$.topic.unmatched'), 0
                   ) = 0
             )
             SELECT body FROM ranked WHERE recency <= ?1 ORDER BY node, recency",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![MAX_CURRENT_TASK_DOCUMENTS_PER_TOPIC], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .map_err(|error| error.to_string())?;
    for row in rows {
        let bytes = row.map_err(|error| error.to_string())?;
        let head: Statement = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        let Some(node) = nodes.get_mut(&head.topic.node) else {
            continue;
        };
        node.documents.push(document(
            [head.title.as_str(), head.pitch.as_str()]
                .into_iter()
                .chain(head.description.as_deref())
                .chain(head.term.iter().map(String::as_str)),
        ));
    }
    Ok(())
}
