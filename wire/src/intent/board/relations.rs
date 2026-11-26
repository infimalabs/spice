//! Bounded typed task relationship projections.

use serde::{Deserialize, Serialize};

use crate::Actor;

/// Most roots one relationship read admits.
pub const TASK_RELATION_ROOT_LIMIT: usize = 8;
/// Deepest bounded relationship expansion one request admits.
pub const TASK_RELATION_DEPTH_LIMIT: u8 = 4;
/// Most typed nodes one relationship answer admits.
pub const TASK_RELATION_NODE_LIMIT: u16 = 128;
/// Most typed edges one relationship answer emits.
pub const TASK_RELATION_EDGE_LIMIT: usize = 512;
/// Most omitted edges named explicitly at a truncation frontier.
pub const TASK_RELATION_FRONTIER_LIMIT: usize = 64;

/// One bounded on-demand relationship projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRelations {
    /// Durable task identities at the center of the projection.
    pub roots: Vec<crate::TaskStamp>,
    /// Explicit relation families to project.
    pub relations: Vec<TaskRelationKind>,
    /// Maximum edge distance from any root.
    pub depth: u8,
    /// Maximum number of returned nodes, including roots.
    pub node_limit: u16,
}

/// Closed relation families available to a task relationship read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskRelationKind {
    /// Immutable creation provenance, including task-parent origins.
    Origin,
    /// Current task dependency edges in both traversal directions.
    Dependency,
    /// Current preparation controller edges from controller to controlled task.
    Controller,
    /// Completed reviewer handoffs joined to their phase authors.
    Review,
    /// Durable task lifecycle facts.
    Lifecycle,
    /// Current active claim ownership.
    Claim,
    /// Declared unlabeled relations between tasks, in both directions.
    Related,
}

/// Stable identity of one relationship node.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum TaskRelationNodeRef {
    /// One durable task.
    Task { task: crate::TaskStamp },
    /// One non-task cause: a steering directive, document, or operator Draft.
    Origin { origin: crate::TaskProvenance },
    /// One durable completed review.
    Review {
        task: crate::TaskStamp,
        position: crate::Seq,
    },
    /// One durable lifecycle fact.
    Lifecycle {
        task: crate::TaskStamp,
        position: crate::Seq,
    },
    /// One current active claim head.
    Claim {
        task: crate::TaskStamp,
        position: crate::Seq,
    },
}

/// Closed lifecycle events exposed by relationship projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskLifecycleKind {
    Added,
    Edited,
    Phased,
    Completed,
    Assigned,
    Reviewed,
    Deferred,
    Woken,
    Submitted,
    Deleted,
}

/// One typed node in a bounded task relationship answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum TaskRelationNode {
    Task {
        task: crate::TaskStamp,
        handle: crate::TaskHandle,
        title: String,
        pitch: String,
        phase: crate::Phase,
        status: crate::WorkflowStatusKind,
    },
    /// One non-task cause. A steering origin carries the request it names when
    /// that key resolves.
    Origin {
        origin: crate::TaskProvenance,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request: Option<OriginRequest>,
    },
    /// One completed review. `phase` and `author` name the most recent
    /// completed phase before it and that phase's author; both are absent when
    /// no phase completed first, as in a flow that begins with review.
    Review {
        task: crate::TaskStamp,
        position: crate::Seq,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<crate::Phase>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        author: Option<Actor>,
        reviewer: Actor,
        verdict: crate::body::Verdict,
    },
    Lifecycle {
        task: crate::TaskStamp,
        position: crate::Seq,
        at: crate::Instant,
        actor: Actor,
        event: TaskLifecycleKind,
    },
    Claim {
        task: crate::TaskStamp,
        position: crate::Seq,
        actor: Actor,
        lane: crate::CanonicalUuid,
        occupant: crate::CanonicalUuid,
        disposition: crate::ClaimDisposition,
    },
}

impl TaskRelationNode {
    /// Stable identity used by edges and truncation metadata.
    #[must_use]
    pub fn identity(&self) -> TaskRelationNodeRef {
        match self {
            Self::Task { task, .. } => TaskRelationNodeRef::Task { task: task.clone() },
            Self::Origin { origin, .. } => TaskRelationNodeRef::Origin {
                origin: origin.clone(),
            },
            Self::Review { task, position, .. } => TaskRelationNodeRef::Review {
                task: task.clone(),
                position: *position,
            },
            Self::Lifecycle { task, position, .. } => TaskRelationNodeRef::Lifecycle {
                task: task.clone(),
                position: *position,
            },
            Self::Claim { task, position, .. } => TaskRelationNodeRef::Claim {
                task: task.clone(),
                position: *position,
            },
        }
    }
}

/// Characters of a steering request an origin node carries.
pub const ORIGIN_REQUEST_PREVIEW_LIMIT: usize = 600;

/// The published request behind a steering origin: the whole text when it
/// fits [`ORIGIN_REQUEST_PREVIEW_LIMIT`], otherwise that many leading
/// characters marked truncated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginRequest {
    pub text: String,
    pub truncated: bool,
}

impl OriginRequest {
    #[must_use]
    pub fn preview(text: &str) -> Self {
        match text.char_indices().nth(ORIGIN_REQUEST_PREVIEW_LIMIT) {
            Some((end, _)) => Self {
                text: text[..end].to_owned(),
                truncated: true,
            },
            None => Self {
                text: text.to_owned(),
                truncated: false,
            },
        }
    }
}

/// One stable typed relationship edge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRelationEdge {
    pub relation: TaskRelationKind,
    pub from: TaskRelationNodeRef,
    pub to: TaskRelationNodeRef,
}

/// One omitted edge at the bounded expansion frontier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRelationFrontier {
    /// Depth at which the omitted target would have appeared.
    pub depth: u8,
    /// Exact omitted relationship.
    pub edge: TaskRelationEdge,
}

/// Explicit metadata for a depth, node, edge, or frontier bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRelationTruncation {
    /// First omitted edges in stable order.
    pub frontier: Vec<TaskRelationFrontier>,
    /// Additional omitted edges beyond the bounded frontier list.
    pub omitted: u32,
}

/// One bounded typed relationship projection at a journal snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRelationsAnswer {
    pub snapshot: crate::Seq,
    pub roots: Vec<crate::TaskStamp>,
    pub nodes: Vec<TaskRelationNode>,
    pub edges: Vec<TaskRelationEdge>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub truncation: Option<TaskRelationTruncation>,
}
