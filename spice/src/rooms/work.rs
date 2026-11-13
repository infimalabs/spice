//! Current native work that makes a lane listen to a room.

use wire::{CanonicalUuid, Seq, TaskStamp};

/// Current native work associated with one room and held by one lane.
///
/// Construction is derived from the current task/preparation and active-claim
/// projection. It is an input reference, not a claim or membership grant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoomWork {
    pub(super) room: TaskStamp,
    pub(super) task: TaskStamp,
    pub(super) lane: CanonicalUuid,
    pub(super) since: Seq,
}

impl RoomWork {
    /// Derive relevant current work from one native task-head bundle, one row
    /// per active participant.
    ///
    /// The anchor itself and tasks whose explicit preparation relationship
    /// names the anchor are relevant only while a claim remains active.
    #[must_use]
    pub fn from_task(room: &TaskStamp, task: &board::Entry) -> Vec<Self> {
        let statement = task.statement();
        let related = task.born() == room
            || statement.preparation.as_ref().is_some_and(|preparation| {
                preparation.parent == *room
                    || preparation.controller == *room
                    || preparation.construction == *room
            });
        if !related {
            return Vec::new();
        }
        task.active_claims()
            .map(|claim| Self {
                room: room.clone(),
                task: task.born().clone(),
                lane: claim.claim().lane,
                since: claim.claimed().max(statement.topic_origin),
            })
            .collect()
    }

    /// The lane whose active claim makes this work current.
    #[must_use]
    pub const fn lane(&self) -> CanonicalUuid {
        self.lane
    }
}
