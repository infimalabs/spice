//! Read-only inferred groups. Their numeric identifiers are scoped to one replay,
//! not durable topics, names, or authorities.
use crate::{Seq, TaskStamp, Topic, intent::TaskRoute};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupMember {
    pub id: TaskStamp,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub path: Vec<usize>,
    pub label: String,
    pub subtree: usize,
    pub direct: Vec<GroupMember>,
    /// Described topics seated directly here; a statement seated with one is
    /// proposed that topic.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixed: Vec<Topic>,
}

/// Direct seat after inserting only the supplied title and pitch. Companions
/// are historical rows in that seat after the insertion, not prior witnesses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NextSeat {
    /// Beside no held statement.
    Alone,
    Group {
        path: Vec<usize>,
        path_existed_before: bool,
        companions: Vec<GroupMember>,
    },
}

/// Positions used when a previously completed tree supplies grouping advice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeLag {
    pub lineage: crate::CanonicalUuid,
    pub projection: Option<Seq>,
    pub source: Seq,
}

/// The completed generation used for this view, not the authority for a filing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupingFreshness {
    pub lineage: crate::CanonicalUuid,
    pub projection: Option<Seq>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_against: Option<Seq>,
    /// Creation position of the oldest seat, inclusive. None for an empty window.
    pub oldest_origin: Option<Seq>,
    pub capacity: usize,
    pub catch_up_limit: usize,
    pub recovery: GroupingRecovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum GroupingRecovery {
    Cold,
    Rebuilt { reason: GroupingRebuild },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GroupingRebuild {
    CatchUpLimit,
    InputChanged,
}

/// One tree over the board's newest eligible statements, and where a fresh
/// statement would sit in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grouping {
    pub cursor: Seq,
    /// Eligible statements on the board, including those older than the window.
    pub retained: usize,
    pub window: usize,
    pub freshness: GroupingFreshness,
    pub proposal: TaskRoute,
    pub groups: Vec<Group>,
    pub next_seat: NextSeat,
}

impl fmt::Display for Grouping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write_proposal(f)?;
        writeln!(
            f,
            "Cursor {}: {} eligible statements, {} in window. Group IDs belong to this replay only.",
            self.cursor.get(),
            self.retained,
            self.window
        )?;
        let fresh = &self.freshness;
        if let Some(source) = fresh.checked_against {
            writeln!(
                f,
                "Earlier tree at {}, checked against task projection {}.",
                fresh.projection.map_or(0, Seq::get),
                source.get()
            )?;
        }
        writeln!(
            f,
            "Tree lineage {} ({} seats; catch-up ceiling {}; {:?}).",
            fresh.lineage, fresh.capacity, fresh.catch_up_limit, fresh.recovery
        )?;
        match fresh.oldest_origin {
            Some(origin) => writeln!(f, "Oldest included arrival: {}.", origin.get())?,
            None => writeln!(f, "No included arrivals.")?,
        }
        self.write_tree(f)
    }
}

impl Grouping {
    fn write_proposal(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.next_seat {
            NextSeat::Alone => writeln!(
                f,
                "Preview only — next seat (title and pitch): beside no prior task"
            )?,
            NextSeat::Group {
                path,
                path_existed_before,
                companions,
            } => {
                let status = if *path_existed_before {
                    "existing path"
                } else {
                    "new path"
                };
                writeln!(
                    f,
                    "Preview only — next seat (title and pitch): group {path:?}, {status}; {} prior tasks directly beside it",
                    companions.len()
                )?;
                for member in companions.iter().take(3) {
                    writeln!(f, "  {}  {}", member.id, member.title)?;
                }
            }
        }
        match &self.proposal {
            TaskRoute::Pick { topic, .. } => writeln!(f, "proposed filing: {topic}"),
            TaskRoute::Unavailable { reason, detail } => {
                writeln!(f, "no filing proposed: {reason}: {detail}")
            }
        }
    }

    fn write_tree(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for g in &self.groups {
            for _ in &g.path {
                write!(f, "  ")?;
            }
            if let Some(id) = g.path.last() {
                write!(f, "g{id}, e.g. ")?;
            } else {
                write!(f, "root: ")?;
            }
            writeln!(
                f,
                "{} ({} direct, {} total)",
                g.label,
                g.direct.len(),
                g.subtree
            )?;
            for topic in &g.fixed {
                for _ in &g.path {
                    write!(f, "  ")?;
                }
                writeln!(f, "  fixed point {topic}")?;
            }
            for member in g.direct.iter().take(3) {
                for _ in &g.path {
                    write!(f, "  ")?;
                }
                writeln!(f, "  {}  {}", member.id, member.title)?;
            }
            if g.direct.len() > 3 {
                writeln!(
                    f,
                    "  … {} more direct members in --json",
                    g.direct.len() - 3
                )?;
            }
        }
        Ok(())
    }
}
