//! Automatic operator-message recipient selection.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::sync::Mutex;

use board::similarity::{MatchCache, TermCounts};
use spice::rooms::standing::relevance;
use wire::{CanonicalUuid, Outcome, intent::CommandAnswer};

use super::*;
use crate::resident::message_surface::{self, MessageState};
use crate::resident::room_catch_up::Planning;

/// Select the best lane for authored operator text. Candidates rank by cohort
/// first: running agents with no active claim, then vacant lanes with no active
/// claim, and only then any lane holding one. Within a cohort, each lane
/// scores its one retained profile across every occupant and session it
/// hosted, and exact ties fall back to stable lane identity.
pub(super) fn select(
    bus: &Bus,
    residency: &wire::ResidencyHead,
    profiles: Option<&Mutex<MessageState>>,
    text: &str,
) -> Result<(CanonicalUuid, Option<CanonicalUuid>), Outcome<CommandAnswer>> {
    let planning = Planning::load(bus).map_err(typed_outcome)?;
    let tasked = planning
        .snapshot
        .tasks()
        .iter()
        .flat_map(|heads| heads.claims())
        .map(|held| held.claim().body())
        .filter(|claim| claim.disposition == wire::ClaimDisposition::Active)
        .map(|claim| claim.lane)
        .collect::<BTreeSet<_>>();
    let mut candidates = residency
        .lanes
        .iter()
        .filter(|row| eligible(&planning.snapshot, row))
        .map(|row| Candidate {
            lane: row.lane,
            occupant: row.occupant,
            cohort: if tasked.contains(&row.lane) {
                Cohort::Tasked
            } else if row.occupant.is_some() {
                Cohort::Idle
            } else {
                Cohort::Vacant
            },
            score: None,
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Err(refusal(
            "steer::recipient-unavailable",
            Some("one occupied or launchable vacant lane".to_owned()),
            None,
            "seat an agent before sending an automatic message",
        ));
    }

    if let Some(profiles) = profiles {
        let mut state = message_surface::lock_state(profiles).map_err(typed_outcome)?;
        let frequency = MatchCache::shared()
            .fit(planning.entries.iter().map(board::Entry::statement), || {
                state.profiles.conversation_background(residency)
            });
        let document = frequency.prepare(&TermCounts::from_texts([text]));
        for candidate in &mut candidates {
            candidate.score = relevance(
                &mut state.profiles,
                &frequency,
                document.as_ref(),
                candidate.lane,
            )
            .map_err(|error| {
                protocol_fault("steer::profile-generation-mismatch", &error.to_string())
            })?
            .filter(|score| *score > 0.0);
        }
    }

    candidates.sort_by(Candidate::order);
    let selected = candidates
        .into_iter()
        .next()
        .expect("a nonempty candidate list remains nonempty after ranking");
    Ok((selected.lane, selected.occupant))
}

fn eligible(snapshot: &bus::TaskSnapshot, row: &wire::LaneResidency) -> bool {
    match row.occupant {
        Some(_) => row.disposition == "occupied",
        None => row.disposition != "occupied" && !snapshot.facts().lane_held(row.lane),
    }
}

/// Delivery cost order: an idle running agent reads the message now, a vacant
/// lane must launch first, and a tasked lane would interrupt claimed work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Cohort {
    Idle,
    Vacant,
    Tasked,
}

#[derive(Clone, Copy, Debug)]
struct Candidate {
    lane: CanonicalUuid,
    occupant: Option<CanonicalUuid>,
    cohort: Cohort,
    score: Option<f64>,
}

impl Candidate {
    fn order(left: &Self, right: &Self) -> Ordering {
        let score = |candidate: &Self| candidate.score.unwrap_or(f64::NEG_INFINITY);
        left.cohort
            .cmp(&right.cohort)
            .then_with(|| score(right).total_cmp(&score(left)))
            .then_with(|| left.lane.cmp(&right.lane))
    }
}

#[cfg(test)]
mod tests;
