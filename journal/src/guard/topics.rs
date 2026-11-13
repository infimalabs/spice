//! Topic reads share the transaction that evaluates mutation guards.

use wire::topic::{TopicAnswer, TopicNode};

use super::{GuardError, JournalRead};

impl JournalRead<'_> {
    /// Public targets with descriptions and a bounded corpus of live tasks.
    pub fn topic_routing_nodes(&self) -> Result<Vec<crate::TopicRoutingNode>, GuardError> {
        crate::topic::routing::nodes(self.connection).map_err(GuardError::Invalid)
    }

    /// Inspect a stable node through its current absolute path.
    pub fn topic_node(&self, path: &wire::Topic) -> Result<Option<TopicNode>, GuardError> {
        crate::topic::metadata::node(self.connection, path).map_err(GuardError::Invalid)
    }

    /// Last change to node metadata, topology, or recorded task routing.
    pub fn topic_revision(&self) -> Result<Option<u64>, GuardError> {
        crate::topic::metadata::revision(self.connection).map_err(GuardError::Invalid)
    }

    /// Read direct routing and metadata facts, including ancestor subtree moves.
    pub fn topic_read(
        &self,
        path: &wire::Topic,
        after: Option<wire::Seq>,
    ) -> Result<TopicAnswer, GuardError> {
        let node = self.topic_node(path)?;
        let (history, next) = match &node {
            Some(node) => crate::topic::metadata::history(self.connection, node.id, after)
                .map_err(GuardError::Invalid)?,
            None => (Vec::new(), None),
        };
        Ok(TopicAnswer {
            node,
            history,
            next,
            preview: None,
            change: None,
        })
    }
}
