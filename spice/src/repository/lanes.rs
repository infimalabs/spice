//! Runtime paths keyed by one lane identity.

use std::path::{Path, PathBuf};

use wire::CanonicalUuid;

use super::StatePaths;

/// Tape root for one lane below the canonical lane-lifetime tree.
#[must_use]
pub fn lane_tape(lanes: &Path, lane: CanonicalUuid) -> PathBuf {
    lanes.join(lane.to_string()).join("tape")
}

/// Agent occupant records for one lane below the canonical lane-lifetime tree.
#[must_use]
pub fn lane_occupants(lanes: &Path, lane: CanonicalUuid) -> PathBuf {
    lanes.join(lane.to_string()).join("occupants")
}

/// Shell command jobs for one lane below the canonical lane-lifetime tree.
#[must_use]
pub fn lane_jobs(lanes: &Path, lane: CanonicalUuid) -> PathBuf {
    lanes.join(lane.to_string()).join("jobs")
}

/// Steering media root for one lane below the canonical lane-lifetime tree.
#[must_use]
pub fn lane_media(lanes: &Path, lane: CanonicalUuid) -> PathBuf {
    lanes.join(lane.to_string()).join("media")
}

impl StatePaths {
    /// Parent of all state whose lifetime is one lane.
    #[must_use]
    pub fn lanes(&self) -> &Path {
        &self.tape
    }

    /// Complete state root for one lane.
    #[must_use]
    pub fn lane(&self, lane: CanonicalUuid) -> PathBuf {
        self.lanes().join(lane.to_string())
    }

    /// Tape root for one lane.
    pub fn lane_tape(&self, lane: CanonicalUuid) -> PathBuf {
        lane_tape(self.lanes(), lane)
    }

    /// Agent occupant records for one lane.
    pub fn lane_occupants(&self, lane: CanonicalUuid) -> PathBuf {
        lane_occupants(self.lanes(), lane)
    }

    /// Shell command jobs for one lane.
    pub fn lane_jobs(&self, lane: CanonicalUuid) -> PathBuf {
        lane_jobs(self.lanes(), lane)
    }

    /// Steering media projections for one lane.
    pub fn lane_media(&self, lane: CanonicalUuid) -> PathBuf {
        lane_media(self.lanes(), lane)
    }

    /// Current RTK history database for one lane.
    #[must_use]
    pub fn rtk_history(&self, lane: CanonicalUuid) -> PathBuf {
        self.lane(lane).join("rtk").join("history.db")
    }

    /// Parent of per-lane app-server discovery files.
    #[must_use]
    pub fn lane_discovery(&self) -> &Path {
        self.lane_serve()
    }
}
