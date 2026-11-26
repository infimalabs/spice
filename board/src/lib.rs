//! Tasks and coordination as checked views over journal heads.

#![forbid(unsafe_code)]

use std::fmt;

#[cfg(not(target_arch = "wasm32"))]
pub mod allocate;
pub mod claim;
pub mod config;
pub mod control;
pub mod duplicate;
pub mod graph;
pub mod handout;
#[cfg(not(target_arch = "wasm32"))]
pub mod lapse;
pub mod next;
pub mod overlap;
pub mod ownership;
pub mod phase;
#[cfg(not(target_arch = "wasm32"))]
pub mod round;
pub mod routing;
pub mod similarity;
pub mod task;
pub mod team_policy;
pub mod urgency;

/// Mint the identity of one board document from its exact authored bytes.
#[must_use]
pub fn digest_of(input: &str) -> wire::Digest {
    wire::Digest::compute(wire::Domain::BoardDocument, input.as_bytes())
}

pub use claim::{Active, ClaimRefusal};
pub use config::{Config, NextConfig};
pub use control::{controlled_pending, controlled_pending_tasks};
pub use graph::{Diagram, View};
pub use next::{
    Claimant, HandoffPeer, ResolvedTasks, Selection, SelectionIndex, WorkWait,
    blocked_handoff_until, capturable_entry, dependencies_satisfied, design_cycle, design_cycles,
    design_held, design_hold, eligible_entry, environment_satisfied,
    independent_handoff_awaits_peer, is_independent_handoff, is_released_by, joinable_entry,
    offer_order, resolved_tasks, select, select_at, select_with_index_at, select_with_satisfied,
    select_with_satisfied_at, selectable_entry, selectable_entry_at, stealable_entry, work_waits,
    work_waits_resolved,
};
pub use overlap::{ClaimOverlap, claim_overlaps};
pub use ownership::{Allocation, OwnershipSnapshot};
pub use phase::{HANDOFF_SECONDS, PEER_HANDOFF_SECONDS, PhaseRefusal, Transition, TransitionKind};
pub use task::{Dependency, Entry, Family, PhaseHandoff, TaskClaim, TaskError};
pub use urgency::{
    Urgency, priority_from_authored, priority_label, priority_score, priority_to_document,
};

/// One read barrier over current journal task heads.
///
/// The schema identity comes from `journal`, completeness is stated with the
/// journal cursor, and rows are checked [`Entry`] values. This is a
/// disposable answer; none of it can update authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    schema_version: u32,
    cursor: wire::JournalCursor,
    entries: Vec<Entry>,
    environment: wire::Environment,
}

impl Snapshot {
    /// Admit a snapshot only for the journal schema this build owns.
    pub fn new(
        schema_version: u32,
        cursor: wire::JournalCursor,
        entries: Vec<Entry>,
    ) -> Result<Self, SnapshotRefusal> {
        if schema_version != journal::SCHEMA_VERSION {
            return Err(SnapshotRefusal {
                found: schema_version,
                current: journal::SCHEMA_VERSION,
            });
        }
        Ok(Self {
            schema_version,
            cursor,
            entries,
            environment: wire::Environment::journal_schema(schema_version)
                .expect("the admitted journal schema is positive"),
        })
    }

    /// Journal schema that supplied the heads.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Greatest journal position covered by this answer.
    #[must_use]
    pub const fn cursor(&self) -> wire::JournalCursor {
        self.cursor
    }

    /// Checked current head bundles.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Environmental authority captured at this snapshot barrier.
    #[must_use]
    pub const fn environment(&self) -> &wire::Environment {
        &self.environment
    }
}

/// Snapshot came from a journal schema this build does not own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotRefusal {
    pub found: u32,
    pub current: u32,
}

impl fmt::Display for SnapshotRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "board snapshot schema {} does not match current journal schema {}",
            self.found, self.current
        )
    }
}

impl std::error::Error for SnapshotRefusal {}
