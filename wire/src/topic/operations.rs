//! Native topic inspection and digest-bound mutations.

use serde::{Deserialize, Serialize};

use crate::{AnyFact, Digest, Envelope, Seq, Topic};

/// Inspect one absolute topic and a page of its durable routing history, or
/// forecast where a fresh statement would be filed without writing anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum TopicRead {
    Show {
        topic: Topic,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<Seq>,
    },
    Preview {
        title: String,
        pitch: String,
    },
}

/// One durable topic description or operator-authored topology change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum TopicChange {
    Move { to: Topic },
    Describe { description: String },
}

/// Preview a change, or apply the exact preview digest against current authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopicEdit {
    pub topic: Topic,
    pub change: TopicChange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply: Option<Digest>,
}

/// A durable node, whose identity is independent of its current path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopicNode {
    pub id: i64,
    pub path: Topic,
    pub parent: Option<i64>,
    pub description: String,
    pub revision: Seq,
}

/// A node's direct placements and subtree movements, in durable fact order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopicAnswer {
    pub node: Option<TopicNode>,
    pub history: Vec<Envelope<AnyFact>>,
    pub next: Option<Seq>,
    pub preview: Option<Digest>,
    pub change: Option<TopicChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grouping: Option<super::Grouping>,
}

/// The description of a topic node was authored; missing nodes are created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopicDescribed {
    pub topic: Topic,
    pub description: String,
}
