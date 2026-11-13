//! Topic nodes form the stable tree referenced by live task heads.
//!
//! A live stored task head names integer nodes, so a move carries the task
//! without rewriting it. A settled head instead stores the last rendered paths:
//! history remains readable without retaining a node in the current namespace.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value};

pub(crate) mod metadata;
pub(crate) mod routing;

/// The node tree. A node's name is one topic component, unique among its
/// siblings; `head_seq` is the fact that last created, renamed, or moved it.
pub(crate) const SCHEMA_SQL: &str = "CREATE TABLE topic_heads (
    node INTEGER PRIMARY KEY,
    parent INTEGER REFERENCES topic_heads(node),
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    head_seq INTEGER NOT NULL REFERENCES fact_envelopes(seq)
) STRICT;
CREATE UNIQUE INDEX topic_heads_top ON topic_heads(name) WHERE parent IS NULL;
CREATE UNIQUE INDEX topic_heads_child ON topic_heads(parent, name) WHERE parent IS NOT NULL;
CREATE INDEX topic_heads_head_seq ON topic_heads(head_seq);";

/// The node at `path`, if the tree has one.
pub(crate) fn lookup(connection: &Connection, path: &wire::Topic) -> rusqlite::Result<Option<i64>> {
    let mut node = None;
    for name in path.components() {
        node = child(connection, node, name)?;
        if node.is_none() {
            return Ok(None);
        }
    }
    Ok(node)
}

/// The node at `path`, creating every missing node on the way under `seq`.
pub(crate) fn resolve(
    connection: &Connection,
    path: &wire::Topic,
    seq: i64,
) -> rusqlite::Result<i64> {
    let mut node = None;
    for name in path.components() {
        let found = match child(connection, node, name)? {
            Some(found) => found,
            None => {
                let created = metadata::allocate_node(connection)?;
                connection.execute(
                    "INSERT INTO topic_heads(node, parent, name, head_seq)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![created, node, name, seq],
                )?;
                metadata::record(connection, created, seq)?;
                created
            }
        };
        node = Some(found);
    }
    let node = node.expect("a topic path has at least one component");
    metadata::record(connection, node, seq)?;
    Ok(node)
}

fn child(
    connection: &Connection,
    parent: Option<i64>,
    name: &str,
) -> rusqlite::Result<Option<i64>> {
    connection
        .query_row(
            "SELECT node FROM topic_heads WHERE parent IS ?1 AND name = ?2",
            params![parent, name],
            |row| row.get(0),
        )
        .optional()
}

/// Renders nodes as their current paths, remembering each one it rendered.
pub(crate) struct Paths<'c> {
    connection: &'c Connection,
    rendered: HashMap<i64, wire::Topic>,
}

impl<'c> Paths<'c> {
    pub(crate) fn new(connection: &'c Connection) -> Self {
        Self {
            connection,
            rendered: HashMap::new(),
        }
    }

    pub(crate) fn path(&mut self, node: i64) -> Result<wire::Topic, String> {
        if let Some(path) = self.rendered.get(&node) {
            return Ok(path.clone());
        }
        let mut names = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut cursor = Some(node);
        while let Some(current) = cursor {
            if !visited.insert(current) {
                return Err(format!("topic node {node} sits on a parent cycle"));
            }
            let (parent, name) = self
                .connection
                .query_row(
                    "SELECT parent, name FROM topic_heads WHERE node = ?1",
                    [current],
                    |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("topic node {current} does not exist"))?;
            names.push(name);
            cursor = parent;
        }
        names.reverse();
        let private = names
            .first()
            .is_some_and(|root| root.parse::<wire::CanonicalUuid>().is_ok());
        let spelling = if private {
            names.join("/")
        } else {
            format!("/{}", names.join("/"))
        };
        let path: wire::Topic = spelling
            .parse()
            .map_err(|error| format!("topic node {node}: {error}"))?;
        if path.components().count() != names.len() {
            return Err(format!("topic node {node} names more than one component"));
        }
        self.rendered.insert(node, path.clone());
        Ok(path)
    }
}

/// Encode `head` as stored: its topic paths become the nodes `node_of` names.
/// An unplaced live head stores a null node, which also marks it live.
/// Every object's keys are sorted, so the stored bytes do not depend on
/// whether this build's `serde_json` preserves insertion order.
pub(crate) fn store_task_head(
    head: &wire::TaskHead,
    mut node_of: impl FnMut(&wire::Topic) -> Result<i64, String>,
) -> Result<Vec<u8>, String> {
    let mut body = serde_json::to_value(head).map_err(|error| error.to_string())?;
    let Value::Object(fields) = &mut body else {
        return Err("a task head encodes as an object".to_owned());
    };
    let mut stored = Map::new();
    let node = head.topic.path.as_ref().map(&mut node_of).transpose()?;
    let placed = head.topic.placed.as_ref().map(&mut node_of).transpose()?;
    if node.is_none() && placed.is_some() {
        return Err("an unplaced task names no router-selected topic".to_owned());
    }
    if head.topic.nodes.is_some_and(|observed| {
        observed.node.is_some_and(|expected| Some(expected) != node)
            || observed
                .placed
                .is_some_and(|expected| Some(expected) != placed)
    }) {
        return Err(
            "task topic nodes changed; read the current statement before publishing".to_owned(),
        );
    }
    stored.insert("node".to_owned(), node.map_or(Value::Null, Value::from));
    if let Some(placed) = placed {
        stored.insert("placed".to_owned(), placed.into());
    }
    if let Some(named) = &head.topic.named {
        stored.insert("named".to_owned(), named.to_string().into());
    }
    if head.topic.unmatched {
        stored.insert("unmatched".to_owned(), true.into());
    }
    fields.insert("topic".to_owned(), Value::Object(stored));
    body.sort_all_objects();
    serde_json::to_vec(&body).map_err(|error| error.to_string())
}

/// Encode a settled task head with immutable rendered topic paths.
///
/// Settled history carries no observed node binding: reopening resolves the
/// rendered path against the current namespace and receives its current ID.
pub(crate) fn archive_task_head(head: &wire::TaskHead) -> Result<Vec<u8>, String> {
    let mut head = head.clone();
    head.topic.nodes = None;
    let mut body = serde_json::to_value(head).map_err(|error| error.to_string())?;
    body.sort_all_objects();
    serde_json::to_vec(&body).map_err(|error| error.to_string())
}

/// Decode a stored task head. Live heads carry a `node` key, null while the
/// task is unplaced, and render node references through the current tree;
/// settled heads already carry immutable rendered paths.
pub(crate) fn load_task_head(
    paths: &mut Paths<'_>,
    bytes: &[u8],
) -> Result<wire::TaskHead, String> {
    let mut body: Map<String, Value> =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let Some(Value::Object(stored)) = body.remove("topic") else {
        return Err("stored task head names no topic".to_owned());
    };
    if !stored.contains_key("node") {
        body.insert("topic".to_owned(), Value::Object(stored));
        let head: wire::TaskHead =
            serde_json::from_value(Value::Object(body)).map_err(|error| error.to_string())?;
        if head.topic.nodes.is_some() {
            return Err("settled task topic carries a live node binding".to_owned());
        }
        return Ok(head);
    }

    let mut node = None;
    let mut placed = None;
    let mut topic = Map::new();
    for (key, value) in stored {
        let rendered = match key.as_str() {
            "node" if value.is_null() => continue,
            "node" | "placed" => {
                let identity = value
                    .as_i64()
                    .ok_or_else(|| format!("stored task topic {key} is not a node"))?;
                if key == "node" {
                    node = Some(identity);
                } else {
                    placed = Some(identity);
                }
                Value::from(paths.path(identity)?.as_str())
            }
            "named" | "unmatched" => value,
            other => return Err(format!("stored task topic carries unknown field {other:?}")),
        };
        let key = if key == "node" {
            "path".to_owned()
        } else {
            key
        };
        topic.insert(key, rendered);
    }
    if node.is_none() && placed.is_some() {
        return Err("an unplaced task names no router-selected topic".to_owned());
    }
    body.insert("topic".to_owned(), Value::Object(topic));
    let mut head: wire::TaskHead =
        serde_json::from_value(Value::Object(body)).map_err(|error| error.to_string())?;
    head.topic.nodes = Some(wire::topic::TopicNodes { node, placed });
    Ok(head)
}

/// Encode `head` exactly as this build stores it: placed live heads through
/// their topic nodes, archived heads by path.
pub(crate) fn restore_task_head(
    connection: &Connection,
    head: &wire::TaskHead,
) -> Result<Vec<u8>, String> {
    if head.topic.nodes.is_some() {
        store_task_head(head, |path| {
            lookup(connection, path)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("topic {path} has no node"))
        })
    } else {
        archive_task_head(head)
    }
}

/// Decode a stored task head and refuse bytes this build would not store.
pub(crate) fn decode_stored_task_head(
    connection: &Connection,
    bytes: &[u8],
) -> Result<wire::TaskHead, String> {
    let head = load_task_head(&mut Paths::new(connection), bytes)?;
    let canonical = restore_task_head(connection, &head)?;
    if canonical != bytes {
        return Err("task head is not stored in canonical form".to_owned());
    }
    Ok(head)
}

/// Every node renders as an admitted path: its parent exists, its ancestor
/// walk ends at a root, and its name is one topic component.
pub(crate) fn verify_tree(
    connection: &Connection,
    violations: &mut Vec<crate::Violation>,
) -> Result<usize, crate::VerifyError> {
    let nodes = {
        let mut statement = connection
            .prepare("SELECT node FROM topic_heads ORDER BY node")
            .map_err(crate::VerifyError::Sqlite)?;
        statement
            .query_map([], |row| row.get::<_, i64>(0))
            .map_err(crate::VerifyError::Sqlite)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(crate::VerifyError::Sqlite)?
    };
    let mut paths = Paths::new(connection);
    for node in &nodes {
        if let Err(detail) = paths.path(*node) {
            violations.push(crate::Violation::new(
                crate::ViolationClass::Head,
                format!("topic_heads:{node}"),
                detail,
            ));
        }
    }
    Ok(nodes.len())
}

/// Current workflow liveness and task topic retention must agree exactly.
pub(crate) fn verify_task_liveness(
    connection: &Connection,
    violations: &mut Vec<crate::Violation>,
) -> Result<(), crate::VerifyError> {
    let mut statement = connection
        .prepare(
            "SELECT t.stamp, t.body, w.body FROM task_heads t
             JOIN workflow_heads w ON w.stamp = t.stamp ORDER BY t.stamp",
        )
        .map_err(crate::VerifyError::Sqlite)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(crate::VerifyError::Sqlite)?;
    for row in rows {
        let (stamp, task_body, workflow_body) = row.map_err(crate::VerifyError::Sqlite)?;
        let Ok(task) = decode_stored_task_head(connection, &task_body) else {
            continue;
        };
        let Ok(workflow) = wire::decode::<wire::WorkflowHead>(&workflow_body) else {
            continue;
        };
        if workflow.status.is_pending() != task.topic.nodes.is_some() {
            violations.push(crate::Violation::new(
                crate::ViolationClass::Head,
                format!("task_heads:{stamp}"),
                "task topic node bindings disagree with workflow liveness".to_owned(),
            ));
        }
        if unplaced_past_first_phase(&task, &workflow) {
            violations.push(crate::Violation::new(
                crate::ViolationClass::Head,
                format!("task_heads:{stamp}"),
                UNPLACED_PAST_FIRST_PHASE.to_owned(),
            ));
        }
    }
    Ok(())
}

pub(crate) const UNPLACED_PAST_FIRST_PHASE: &str =
    "an unplaced task cannot stay pending outside its draft phase";

/// Only a pending task's draft phase may leave it unplaced: substantive work
/// always carries a resolved topic.
pub(crate) fn unplaced_past_first_phase(
    task: &wire::TaskHead,
    workflow: &wire::WorkflowHead,
) -> bool {
    task.topic.path.is_none() && workflow.status.is_pending() && !workflow.is_draft()
}

/// Re-parent and rename the node at `moved.from` to `moved.to`, creating
/// `to`'s missing ancestors under `seq`.
pub(crate) fn apply_move(
    connection: &Connection,
    moved: &wire::topic::TopicMoved,
    seq: i64,
) -> Result<(), MoveError> {
    let (from, to) = (&moved.from, &moved.to);
    if wire::is_oops_topic(to) {
        return Err(MoveError::ReservedOops(to.to_string()));
    }
    let node = lookup(connection, from)?
        .ok_or_else(|| MoveError::Refused(format!("topic {from} does not exist")))?;
    if from.contains(to) {
        return Err(MoveError::Refused(format!(
            "topic {from} cannot move to {to}, inside its own subtree"
        )));
    }
    if lookup(connection, to)?.is_some() {
        return Err(MoveError::Refused(format!("topic {to} already exists")));
    }
    let parent = to
        .parent()
        .map(|parent| resolve(connection, &parent, seq))
        .transpose()?;
    connection.execute(
        "UPDATE topic_heads SET parent = ?1, name = ?2, head_seq = ?3 WHERE node = ?4",
        params![parent, to.name(), seq, node],
    )?;
    metadata::moved(connection, node, seq)?;
    Ok(())
}

/// Remove leaf nodes whose last identity-bearing association disappeared.
///
/// History rows are projection data and are deleted before their node. The
/// immutable facts they index remain in `fact_envelopes`.
pub(crate) fn prune_anonymous(connection: &Connection) -> rusqlite::Result<usize> {
    let removable = {
        let mut statement = connection.prepare(
            "WITH RECURSIVE
             protected(node) AS (
                 SELECT node FROM topic_heads WHERE description <> ''
                 UNION
                 SELECT json_extract(CAST(body AS TEXT), '$.topic.node')
                 FROM task_heads
                 UNION
                 SELECT json_extract(CAST(body AS TEXT), '$.topic.placed')
                 FROM task_heads
                 WHERE json_extract(CAST(body AS TEXT), '$.topic.placed') IS NOT NULL
             ),
             retained(node) AS (
                 SELECT node FROM protected
                 UNION
                 SELECT q.parent FROM topic_heads q
                 JOIN retained r ON r.node = q.node
                 WHERE q.parent IS NOT NULL
             ),
             tree(node, depth) AS (
                 SELECT node, 0 FROM topic_heads WHERE parent IS NULL
                 UNION ALL
                 SELECT q.node, tree.depth + 1 FROM topic_heads q
                 JOIN tree ON q.parent = tree.node
             )
             SELECT tree.node FROM tree
             LEFT JOIN retained ON retained.node = tree.node
             WHERE retained.node IS NULL
             ORDER BY tree.depth DESC, tree.node",
        )?;
        statement
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    for node in &removable {
        connection.execute("DELETE FROM topic_history WHERE node = ?1", [node])?;
    }
    for node in &removable {
        connection.execute("DELETE FROM topic_heads WHERE node = ?1", [node])?;
    }
    Ok(removable.len())
}

#[derive(Debug)]
pub(crate) enum MoveError {
    Refused(String),
    ReservedOops(String),
    Sqlite(rusqlite::Error),
}

impl From<rusqlite::Error> for MoveError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}
