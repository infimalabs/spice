//! How well a running occupant's lane matches each candidate: the supplier of
//! [`board::Claimant::match_score`] for `task next`'s read and for the claim
//! that follows it, so both rank the same board the same way.
//!
//! The score is [`spice::rooms::standing::standing`] for the running lane,
//! the one ranker `task next` and the best-match wake order share. The fit
//! and each candidate's vector come from the shared [`MatchCache`]; the board
//! asks for every row in the winning priority and weight group. A score below
//! the lowest Design invitation floor counts as zero, so incidental vocabulary
//! overlap never outranks age; a cold or absent profile scores zero for every
//! row, leaving the ladder exactly as it stands without a score.

use std::cell::RefCell;
use std::sync::{Arc, MutexGuard};

use board::Entry;
use board::similarity::{FrequencyProfile, MatchCache};
use spice::rooms::profile::LaneProfiles;
use spice::rooms::standing::standing;
use wire::CanonicalUuid;
use wire::rooms::DesignReach;

use super::OccupantProfiles;
use crate::resident::design_awareness::match_score_floor;
use crate::resident::message_surface::{self, MessageState};
use crate::typed_outcome;

/// The running occupant's matcher, holding the profile lock while it scores.
pub(crate) struct OccupantMatch<'a> {
    state: RefCell<MutexGuard<'a, MessageState>>,
    frequency: Arc<FrequencyProfile>,
    lane: CanonicalUuid,
}

impl<'a> OccupantMatch<'a> {
    /// The matcher for `principal`, fitted over the live rows of `entries`:
    /// none when the resident retains no profiles or the principal is not a
    /// seated, occupied lane.
    ///
    /// The fit is the ranking population. `task next` reads the live frontier
    /// and its claim reads that frontier plus settled context rows, so fitting
    /// only pending rows keeps both on one population and one ranking.
    pub(crate) fn open(
        profiles: OccupantProfiles<'a>,
        residency: &wire::ResidencyHead,
        principal: &wire::Actor,
        entries: &[Entry],
    ) -> Result<Option<Self>, wire::Outcome<wire::intent::CommandAnswer>> {
        let (Some(profiles), Some((lane, _))) = (profiles, running_occupant(residency, principal))
        else {
            return Ok(None);
        };
        let state = message_surface::lock_state(profiles).map_err(typed_outcome)?;
        Ok(Some(Self {
            frequency: MatchCache::shared().fit(
                entries
                    .iter()
                    .filter(|entry| entry.workflow().status.is_pending())
                    .map(Entry::statement),
                || state.profiles.conversation_background(residency),
            ),
            state: RefCell::new(state),
            lane,
        }))
    }

    /// The lane's standing for `entry` at or above the wide-reach floor;
    /// zero below it or when nothing scores.
    pub(crate) fn score(&self, entry: &Entry) -> f64 {
        let mut state = self.state.borrow_mut();
        floored_standing(&mut state.profiles, &self.frequency, entry, self.lane).unwrap_or(0.0)
    }
}

/// Every lane's retained work scored against the board, through the ranker
/// `task next` shares, under the shared [`MatchCache`] fit. One pass takes
/// the profile lock once and fits once. A lane's own cards of a task never
/// count.
pub(crate) struct RetainedMatch<'a> {
    state: RefCell<MutexGuard<'a, MessageState>>,
    frequency: Arc<FrequencyProfile>,
}

impl<'a> RetainedMatch<'a> {
    /// The pass's matcher; none before message startup restored profiles,
    /// which leaves every lane on the unscored ladder exactly.
    pub(crate) fn open(
        profiles: OccupantProfiles<'a>,
        residency: &wire::ResidencyHead,
        entries: &[Entry],
    ) -> Result<Option<Self>, wire::Outcome<wire::intent::CommandAnswer>> {
        let Some(profiles) = profiles else {
            return Ok(None);
        };
        let state = message_surface::lock_state(profiles).map_err(typed_outcome)?;
        Ok(Some(Self {
            frequency: MatchCache::shared().fit(entries.iter().map(Entry::statement), || {
                state.profiles.conversation_background(residency)
            }),
            state: RefCell::new(state),
        }))
    }

    /// `lane`'s standing for `entry` at or above the wide-reach floor; absent
    /// below it or when nothing scores.
    pub(crate) fn score(&self, lane: CanonicalUuid, entry: &Entry) -> Option<f64> {
        let mut state = self.state.borrow_mut();
        floored_standing(&mut state.profiles, &self.frequency, entry, lane)
    }
}

/// One lane's standing for `entry` under `frequency`. Absent when nothing
/// scores or the score sits below the wide-reach floor, the lowest Design
/// invitation floor, so incidental vocabulary overlap never counts as a match.
fn floored_standing(
    profiles: &mut LaneProfiles,
    frequency: &Arc<FrequencyProfile>,
    entry: &Entry,
    lane: CanonicalUuid,
) -> Option<f64> {
    let statement = MatchCache::shared().prepare(frequency, entry.statement());
    standing(
        profiles,
        frequency,
        statement.as_deref(),
        entry.born(),
        lane,
    )
    // The statement was prepared under this same frequency generation.
    .ok()
    .and_then(|standing| standing.score)
    .filter(|score| *score >= match_score_floor(DesignReach::Wide))
}

/// The lane and occupant seated for `principal`, when it is an occupied lane.
pub(super) fn running_occupant(
    residency: &wire::ResidencyHead,
    principal: &wire::Actor,
) -> Option<(CanonicalUuid, CanonicalUuid)> {
    let wire::Actor::Lane(lane) = principal else {
        return None;
    };
    residency
        .lanes
        .iter()
        .find(|row| row.lane == *lane && row.disposition == "occupied")
        .and_then(|row| row.occupant)
        .map(|occupant| (*lane, occupant))
}
