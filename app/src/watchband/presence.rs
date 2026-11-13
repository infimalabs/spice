//! Per-member presence projected from resident state and retained chronology.

use crate::runtime::{AppRuntime, LaneMember};

use super::Standing;

/// One truthful, mutually exclusive presence state for a team member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberPresenceState {
    /// The current occupant is active, or its lane owns active work.
    Working,
    /// The current occupant remains supervised after the quiet horizon.
    Idle,
    /// The current occupant is seated but has not produced activity yet.
    Starting,
    /// No occupant sits in the lane.
    Vacant,
}

impl MemberPresenceState {
    /// Stable machine spelling used by browser presentation hooks.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Idle => "idle",
            Self::Starting => "starting",
            Self::Vacant => "vacant",
        }
    }

    /// Human-facing state independent of task detail.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Idle => "idle in seat",
            Self::Starting => "starting",
            Self::Vacant => "vacant",
        }
    }
}

/// Current presence plus the wall-clock instant at which that state began.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberPresence {
    pub state: MemberPresenceState,
    pub since_millis: Option<i64>,
}

/// Join lane supervision, the active claim, current-occupant activity, and
/// durable lifecycle chronology into exactly one member presence.
///
/// A missing occupant is authoritatively vacant even if its claim remains
/// active. Otherwise only activity authored by the currently named occupant
/// can prove activity; predecessor records never animate a successor.
#[must_use]
pub fn member_presence(
    runtime: &AppRuntime,
    member: &LaneMember,
    standing: &Standing,
) -> MemberPresence {
    let activity = member.occupant.and_then(|occupant| {
        runtime
            .activities()
            .iter()
            .find(|activity| activity.lane() == member.lane && activity.author() == occupant)
    });
    let transition = runtime
        .newest_lane_presence_transition(member.lane)
        .filter(|transition| transition.occupant() == member.occupant);
    let activity_since = activity.map(crate::runtime::LaneActivity::display_millis);
    let transition_since = transition.map(crate::runtime::LanePresenceTransition::display_millis);

    let Some(_) = member.occupant else {
        return MemberPresence {
            state: MemberPresenceState::Vacant,
            since_millis: transition_since,
        };
    };
    if standing.handle.is_some() {
        return MemberPresence {
            state: MemberPresenceState::Working,
            since_millis: standing.held_since.or(activity_since).or(transition_since),
        };
    }
    if member.liveness == wire::intent::LaneLiveness::Vacant {
        return MemberPresence {
            state: if activity.is_some() {
                MemberPresenceState::Working
            } else {
                MemberPresenceState::Starting
            },
            since_millis: activity_since.or(transition_since),
        };
    }
    if activity.is_none() {
        return MemberPresence {
            state: MemberPresenceState::Starting,
            since_millis: transition_since,
        };
    }
    let quiet_horizon =
        i64::try_from(wire::intent::LANE_QUIET_HORIZON_MILLIS).expect("quiet horizon fits i64");
    if member.liveness == wire::intent::LaneLiveness::Working {
        return MemberPresence {
            state: MemberPresenceState::Working,
            since_millis: activity_since.or(transition_since),
        };
    }
    let activity_since = activity_since.expect("activity presence was checked");
    MemberPresence {
        state: MemberPresenceState::Idle,
        since_millis: Some(activity_since.saturating_add(quiet_horizon)),
    }
}
