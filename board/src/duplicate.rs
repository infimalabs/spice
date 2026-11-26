//! Independent siblings whose statement repeats a task being created.

use std::sync::Arc;

use wire::{TaskHead, TaskStamp};

use crate::similarity::{
    DUPLICATE_FLOOR, DUPLICATE_PREPARED_FLOOR, DUPLICATE_WARNING_FLOOR, FrequencyProfile,
    MatchCache, Prepared,
};

/// Siblings born at most this long before a creation refuse it outright.
pub const DUPLICATE_REFUSAL_MICROS: i64 = 60 * 1_000_000;
/// Siblings born at most this long before a creation refuse it unless the
/// creation names them as related.
pub const DUPLICATE_RELATION_MICROS: i64 = 15 * 60 * 1_000_000;

/// How a sibling stands to the task being created before any statement is
/// compared.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DuplicateKinship {
    /// Unrelated lineage: [`DUPLICATE_WARNING_FLOOR`] warns and
    /// [`DUPLICATE_FLOOR`] is the same work.
    Independent,
    /// Prepared under the same parent or controller, whose outputs echo each
    /// other by design: [`DUPLICATE_FLOOR`] only warns and
    /// [`DUPLICATE_PREPARED_FLOOR`] is the same work.
    Prepared,
}

/// How one duplicate sibling bears on the creation that repeats it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DuplicateStanding {
    /// Born within [`DUPLICATE_REFUSAL_MICROS`]: no relation admits the
    /// creation.
    Refused,
    /// Born within [`DUPLICATE_RELATION_MICROS`]: only a declared relation
    /// admits the creation.
    Unrelated,
    /// Older, or close without being the same work: the creation proceeds
    /// and carries a warning.
    Warned,
}

impl DuplicateStanding {
    /// The standing of a `kinship` sibling born `sibling` scoring `cosine`
    /// against a creation born `created`, or `None` when the score is beneath
    /// that kinship's notice. Only a sibling that is the same work can refuse;
    /// a sibling born later than the creation is as fresh as one born with it.
    #[must_use]
    pub fn of(
        created: &TaskStamp,
        sibling: &TaskStamp,
        cosine: f64,
        kinship: DuplicateKinship,
    ) -> Option<Self> {
        let (notice, same_work) = match kinship {
            DuplicateKinship::Independent => (DUPLICATE_WARNING_FLOOR, DUPLICATE_FLOOR),
            DuplicateKinship::Prepared => (DUPLICATE_FLOOR, DUPLICATE_PREPARED_FLOOR),
        };
        if cosine < notice {
            return None;
        }
        let age = created.micros().saturating_sub(sibling.micros());
        Some(if cosine < same_work || age > DUPLICATE_RELATION_MICROS {
            Self::Warned
        } else if age <= DUPLICATE_REFUSAL_MICROS {
            Self::Refused
        } else {
            Self::Unrelated
        })
    }
}

/// Most siblings of one duplicate band a creation's answer or refusal names;
/// the rest are counted, never rendered.
pub const DUPLICATE_PREVIEW: usize = 3;

/// The strongest [`DUPLICATE_PREVIEW`] siblings of one band, strongest first
/// and first-seen among equals, and how many matched in all. Admitting a
/// sibling never grows it past the preview.
#[derive(Clone, Debug)]
pub struct DuplicatePreview<T> {
    shown: Vec<(f64, T)>,
    matched: usize,
}

impl<T> Default for DuplicatePreview<T> {
    fn default() -> Self {
        Self {
            shown: Vec::with_capacity(DUPLICATE_PREVIEW),
            matched: 0,
        }
    }
}

impl<T> DuplicatePreview<T> {
    /// Count `sibling` scoring `cosine`, keeping it when it ranks among the
    /// strongest.
    pub fn admit(&mut self, cosine: f64, sibling: T) {
        self.matched += 1;
        let at = self.shown.partition_point(|(shown, _)| *shown >= cosine);
        if at >= DUPLICATE_PREVIEW {
            return;
        }
        if self.shown.len() == DUPLICATE_PREVIEW {
            self.shown.pop();
        }
        self.shown.insert(at, (cosine, sibling));
    }

    /// The kept siblings with their cosines, strongest first.
    #[must_use]
    pub fn shown(&self) -> &[(f64, T)] {
        &self.shown
    }

    /// How many matching siblings the preview leaves out.
    #[must_use]
    pub fn omitted(&self) -> usize {
        self.matched - self.shown.len()
    }
}

/// One created statement weighted under the shared fit, ready to score
/// siblings.
#[derive(Clone, Debug)]
pub struct DuplicateProbe {
    frequency: Arc<FrequencyProfile>,
    statement: Option<Arc<Prepared>>,
}

impl DuplicateProbe {
    /// Weigh `statement` under the shared [`MatchCache`] fit of the
    /// [`crate::similarity::FREQUENCY_WINDOW`] most recently revised of
    /// `corpus`, with the conversation background that fit carries.
    #[must_use]
    pub fn new<'a>(statement: &TaskHead, corpus: impl Iterator<Item = &'a TaskHead>) -> Self {
        let matcher = MatchCache::shared();
        let frequency = matcher.fit_carried(corpus);
        let statement = matcher.prepare(&frequency, statement);
        Self {
            frequency,
            statement,
        }
    }

    /// `sibling`'s cosine when it reaches [`DUPLICATE_WARNING_FLOOR`].
    #[must_use]
    pub fn cosine(&self, sibling: &TaskHead) -> Option<f64> {
        let created = self.statement.as_ref()?;
        let sibling = MatchCache::shared().prepare(&self.frequency, sibling)?;
        let cosine = created.cosine(&sibling).ok()?;
        (cosine.is_finite() && cosine >= DUPLICATE_WARNING_FLOOR).then_some(cosine)
    }

    /// The strongest weighted terms `sibling` shares with the created
    /// statement.
    #[must_use]
    pub fn surface(&self, sibling: &TaskHead) -> Vec<String> {
        let Some(created) = self.statement.as_ref() else {
            return Vec::new();
        };
        MatchCache::shared()
            .prepare(&self.frequency, sibling)
            .map_or_else(Vec::new, |sibling| created.explanation(&sibling))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(micros: i64) -> TaskStamp {
        TaskStamp::for_micros(micros).expect("fixture stamp")
    }

    #[test]
    fn standing_bounds_are_inclusive_of_each_window() {
        let created = stamp(DUPLICATE_RELATION_MICROS * 2);
        let born = |age: i64| stamp(created.micros() - age);
        let of = |sibling: &TaskStamp| {
            DuplicateStanding::of(
                &created,
                sibling,
                DUPLICATE_FLOOR,
                DuplicateKinship::Independent,
            )
        };
        assert_eq!(
            of(&born(DUPLICATE_REFUSAL_MICROS)),
            Some(DuplicateStanding::Refused)
        );
        assert_eq!(
            of(&born(DUPLICATE_REFUSAL_MICROS + 1)),
            Some(DuplicateStanding::Unrelated)
        );
        assert_eq!(
            of(&born(DUPLICATE_RELATION_MICROS)),
            Some(DuplicateStanding::Unrelated)
        );
        assert_eq!(
            of(&born(DUPLICATE_RELATION_MICROS + 1)),
            Some(DuplicateStanding::Warned)
        );
        assert_eq!(
            DuplicateStanding::of(
                &born(DUPLICATE_RELATION_MICROS + 1),
                &created,
                DUPLICATE_FLOOR,
                DuplicateKinship::Independent,
            ),
            Some(DuplicateStanding::Refused),
            "a sibling born after the creation is as fresh as one born with it"
        );
    }

    #[test]
    fn each_kinship_bands_a_fresh_sibling_by_its_own_floors() {
        let created = stamp(DUPLICATE_RELATION_MICROS * 2);
        let fresh = |cosine: f64, kinship: DuplicateKinship| {
            DuplicateStanding::of(&created, &created, cosine, kinship)
        };
        let independent = DuplicateKinship::Independent;
        let prepared = DuplicateKinship::Prepared;
        assert_eq!(fresh(DUPLICATE_WARNING_FLOOR - 0.01, independent), None);
        assert_eq!(
            fresh(DUPLICATE_WARNING_FLOOR, independent),
            Some(DuplicateStanding::Warned)
        );
        assert_eq!(
            fresh(DUPLICATE_FLOOR - 0.01, independent),
            Some(DuplicateStanding::Warned)
        );
        assert_eq!(
            fresh(DUPLICATE_FLOOR, independent),
            Some(DuplicateStanding::Refused)
        );
        assert_eq!(fresh(DUPLICATE_FLOOR - 0.01, prepared), None);
        assert_eq!(
            fresh(DUPLICATE_FLOOR, prepared),
            Some(DuplicateStanding::Warned)
        );
        assert_eq!(
            fresh(DUPLICATE_PREPARED_FLOOR - 0.01, prepared),
            Some(DuplicateStanding::Warned)
        );
        assert_eq!(
            fresh(DUPLICATE_PREPARED_FLOOR, prepared),
            Some(DuplicateStanding::Refused)
        );
    }

    #[test]
    fn a_preview_keeps_the_strongest_siblings_and_counts_the_rest() {
        let mut preview = DuplicatePreview::default();
        for (cosine, sibling) in [(0.4, 'a'), (0.9, 'b'), (0.5, 'c'), (0.9, 'd'), (0.3, 'e')] {
            preview.admit(cosine, sibling);
        }
        assert_eq!(preview.shown(), &[(0.9, 'b'), (0.9, 'd'), (0.5, 'c')]);
        assert_eq!(preview.omitted(), 2);
        assert!(preview.shown.capacity() <= DUPLICATE_PREVIEW);
    }
}
