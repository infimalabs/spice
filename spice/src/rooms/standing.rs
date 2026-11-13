//! How well one lane's retained work matches a task statement: the one ranker
//! every choice of a lane for a task shares, `task next` and the lifecycle's
//! best-match wake order alike.

use board::similarity::{FrequencyProfile, GenerationMismatch, Prepared};
use wire::{CanonicalUuid, TaskStamp};

use super::evidence::Evidence;
use super::profile::{EvidenceKey, LaneProfiles};

/// One lane's match against a task statement.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Standing {
    /// Best cosine of the lane's retained cards, less the task's own, against
    /// the statement; absent when no card scores.
    pub score: Option<f64>,
    /// Whether a scored profile holds one of the task's own cards: the lane
    /// already worked the task, so it takes it only as a last resort.
    pub worked: bool,
}

/// The standing of `lane` for the task born `task`, whose statement weighs as
/// `statement`; absent when the statement weighs no term, so nothing scores.
///
/// The lane's one profile scores, whether it is seated or vacant and whichever
/// sessions retained its cards. The task's own cards never count toward the
/// score, so it measures only other related work.
///
/// # Errors
///
/// Refuses a `statement` weighted under a generation other than `frequency`'s.
pub fn standing(
    profiles: &mut LaneProfiles,
    frequency: &FrequencyProfile,
    statement: Option<&Prepared>,
    task: &TaskStamp,
    lane: CanonicalUuid,
) -> Result<Standing, GenerationMismatch> {
    let own =
        |key: &EvidenceKey| matches!(key, EvidenceKey::Task { task: card, .. } if card == task);
    let foreign = |card: &Evidence<EvidenceKey>| !own(card.key());
    let mut standing = Standing::default();
    let Some(profile) = profiles.profile_mut(lane) else {
        return Ok(standing);
    };
    let evidence = profile.evidence_mut();
    standing.worked = evidence.holds(own);
    if let Some(statement) = statement {
        standing.score = evidence.best_score_where(frequency, statement, foreign)?;
    }
    Ok(standing)
}

/// The strongest retained evidence match for arbitrary authored text on one
/// lane's profile.
///
/// # Errors
///
/// Refuses a `document` weighted under a generation other than `frequency`'s.
pub fn relevance(
    profiles: &mut LaneProfiles,
    frequency: &FrequencyProfile,
    document: Option<&Prepared>,
    lane: CanonicalUuid,
) -> Result<Option<f64>, GenerationMismatch> {
    let (Some(document), Some(profile)) = (document, profiles.profile_mut(lane)) else {
        return Ok(None);
    };
    profile.evidence_mut().best_score(frequency, document)
}
