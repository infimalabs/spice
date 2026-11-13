//! How many distinct lanes complete each phase, and who completed this one.

use std::collections::BTreeMap;
use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

use crate::envelope::{Actor, Seq};
use crate::ident::CanonicalUuid;
use crate::witness::Phase;

/// One task's completion staffing, orthogonal to its workflow.
///
/// The flow stays one unique ladder; this head says how many distinct lanes
/// must complete each phase of it and records who completed the phase the
/// task occupies now. Changing a target is not a workflow mutation, so it
/// advances no generation and stales no holder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionHead {
    /// Completion target of each phase that needs more than one lane.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub targets: BTreeMap<Phase, NonZeroU32>,
    /// The part of each target the operator asked for. A target above its
    /// ask grew by joins, and each grown slot leaves with the participant
    /// that filled it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub asked: BTreeMap<Phase, NonZeroU32>,
    /// Fact that opened the current round: the task's creation, or its
    /// latest entry into the phase and status it occupies now. The journal
    /// assigns it at commit.
    pub round: Seq,
    /// Claims that completed the current round's phase, in arrival order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arrivals: Vec<Arrival>,
}

impl CompletionHead {
    /// An empty round opened at `round` with every target at one.
    #[must_use]
    pub const fn opened(round: Seq) -> Self {
        Self {
            targets: BTreeMap::new(),
            asked: BTreeMap::new(),
            round,
            arrivals: Vec::new(),
        }
    }

    /// An empty round opened at `round` keeping every target and ask.
    #[must_use]
    pub fn reopened(self, round: Seq) -> Self {
        Self {
            round,
            arrivals: Vec::new(),
            ..self
        }
    }

    /// Distinct lanes `phase` needs; a phase without a stored target needs one.
    #[must_use]
    pub fn target(&self, phase: &Phase) -> NonZeroU32 {
        self.targets.get(phase).copied().unwrap_or(NonZeroU32::MIN)
    }

    /// Distinct lanes the operator asked `phase` for; one when never asked.
    #[must_use]
    pub fn asked(&self, phase: &Phase) -> NonZeroU32 {
        self.asked.get(phase).copied().unwrap_or(NonZeroU32::MIN)
    }

    /// Set `phase`'s target, storing only targets above one.
    pub fn set_target(&mut self, phase: Phase, target: NonZeroU32) {
        if target == NonZeroU32::MIN {
            self.targets.remove(&phase);
        } else {
            self.targets.insert(phase, target);
        }
    }

    /// Set `phase`'s target as the operator's ask, storing only asks above one.
    pub fn ask(&mut self, phase: Phase, target: NonZeroU32) {
        if target == NonZeroU32::MIN {
            self.asked.remove(&phase);
        } else {
            self.asked.insert(phase, target);
        }
        self.set_target(phase, target);
    }

    /// The target `phase` falls back to once only `occupied` claims and
    /// arrivals remain: the operator's ask or the occupancy, whichever is
    /// higher. None when the current target already stands there.
    #[must_use]
    pub fn retraction(&self, phase: &Phase, occupied: usize) -> Option<NonZeroU32> {
        let occupied = u32::try_from(occupied).ok()?;
        let floor = self
            .asked(phase)
            .max(NonZeroU32::new(occupied).unwrap_or(NonZeroU32::MIN));
        (floor < self.target(phase)).then_some(floor)
    }

    /// Whether `lane` already completed the current round.
    #[must_use]
    pub fn arrived(&self, lane: CanonicalUuid) -> bool {
        self.arrivals.iter().any(|arrival| arrival.lane == lane)
    }
}

/// One claim's completion of its round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Arrival {
    /// Identity of the completing claim: the fact that won it.
    pub claimed: Seq,
    /// Actor that held the claim.
    pub actor: Actor,
    /// Lane bound to the claim.
    pub lane: CanonicalUuid,
    /// Fact that recorded the completion.
    pub fact: Seq,
}
