//! The production bridge from one live lane feed to the pure Mosaic engine.
//!
//! Browser rendering owns measurement and markup. This module owns the one
//! boundary that must not disappear during view rewrites: every lane submits
//! its measured candidates to [`crate::mosaic::pack`] and receives a plane
//! whose extent comes from the same engine result.

use crate::mosaic::{self, Candidate, Card, PlacementPolicy, TRACK_COUNT};
use std::collections::{BTreeMap, BTreeSet};

const CREATION_INDEX_STRIDE: usize = 1usize << (usize::BITS / 2);
const CREATION_INDEX_ORIGIN: usize = 1usize << (usize::BITS - 1);

/// Ephemeral browser geometry that determines one deterministic plane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlaneGeometry {
    width: i32,
    viewport_rows: i32,
}

impl PlaneGeometry {
    /// Pair the measurable content width with the visible vertical budget.
    #[must_use]
    pub const fn new(width: i32, viewport_rows: i32) -> Self {
        Self {
            width,
            viewport_rows: if viewport_rows > 0 { viewport_rows } else { 1 },
        }
    }

    /// Measurable content width.
    #[must_use]
    pub const fn width(self) -> i32 {
        self.width
    }

    /// Vertical modules visible in the plane.
    #[must_use]
    pub const fn viewport_rows(self) -> i32 {
        self.viewport_rows
    }

    const fn policy(self) -> PlacementPolicy {
        PlacementPolicy::new(self.viewport_rows)
    }
}

/// One measured card offered to a persistent lane plane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeasuredCard<K> {
    /// Stable message or directive identity.
    pub key: K,
    /// Policy candidates used by insertion and explicit full replay.
    pub candidates: Vec<Candidate>,
    /// Fresh measurement at the already-latched span, when distinct from the
    /// policy candidates.
    pub latched: Option<Candidate>,
    /// Semantic content epoch that must commit even when the resulting row
    /// reservation is unchanged, such as an image leaving its placeholder.
    pub content_revision: u64,
    /// Whether this card starts a new chronological placement epoch.
    pub boundary: bool,
}

/// Why the most recent committed plane changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transition {
    /// One or more newly observed cards were appended to the live tail.
    Append,
    /// Older retained cards were placed below the committed plane.
    Backfill,
    /// Existing content changed while horizontal placement stayed latched.
    ContentResize,
    /// Structural membership changed inside one or more stable boundaries.
    EpochReplay,
    /// Geometry or structure without a stable boundary required a complete replay.
    FullReplay,
}

impl Transition {
    /// Stable browser-instrumentation spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Append => "append",
            Self::Backfill => "backfill",
            Self::ContentResize => "content-resize",
            Self::EpochReplay => "epoch-replay",
            Self::FullReplay => "full-replay",
        }
    }
}

/// One keyed placement retained across browser renders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaneCard<K> {
    /// Stable message or directive identity.
    pub key: K,
    /// Fresh measurement at the latched span, if it is outside policy tiers.
    pub latched: Option<Candidate>,
    /// Last semantic content epoch incorporated into this placement.
    pub content_revision: u64,
    /// Whether this card starts a new chronological placement epoch.
    pub boundary: bool,
    /// Committed lattice placement.
    pub card: Card,
}

/// Stateful live plane layered over the pure Mosaic transition functions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistentLanePlane<K> {
    geometry: PlaneGeometry,
    cards: Vec<PlaneCard<K>>,
    max_row: i32,
    min_base: i32,
    commits: u64,
    transition: Transition,
}

impl<K> PersistentLanePlane<K> {
    /// Browser geometry for which this plane was committed.
    #[must_use]
    pub const fn geometry(&self) -> PlaneGeometry {
        self.geometry
    }

    /// Keyed cards in stable creation order.
    #[must_use]
    pub fn cards(&self) -> &[PlaneCard<K>] {
        &self.cards
    }

    /// Highest occupied lattice row.
    #[must_use]
    pub const fn max_row(&self) -> i32 {
        self.max_row
    }

    /// Number of rows the CSS grid must expose.
    #[must_use]
    pub fn rows(&self) -> i32 {
        (self.max_row - self.min_base).max(1)
    }

    /// Number of non-noop plane commits since this component mounted.
    #[must_use]
    pub const fn commits(&self) -> u64 {
        self.commits
    }

    /// Transition used by the most recent commit.
    #[must_use]
    pub const fn transition(&self) -> Transition {
        self.transition
    }
}

impl<K: Clone + Ord> PersistentLanePlane<K> {
    /// Create the first committed plane at one measurable geometry.
    #[must_use]
    pub fn new(geometry: PlaneGeometry, measured: &[MeasuredCard<K>]) -> Self {
        Self::full_replay(geometry, measured, 1)
    }

    /// Reconcile a complete measured snapshot through the narrowest live path.
    #[must_use]
    pub fn reconcile(&self, geometry: PlaneGeometry, measured: &[MeasuredCard<K>]) -> Self {
        assert_unique(measured);
        if geometry != self.geometry {
            return self.geometry_replay(geometry, measured);
        }

        let Some((backfill_count, append_start)) = edge_additions(&self.cards, measured) else {
            return self.structural_replay(geometry, measured);
        };
        let retained = &measured[backfill_count..append_start];
        if self
            .cards
            .iter()
            .zip(retained)
            .any(|(entry, item)| entry.boundary != item.boundary)
        {
            return self.structural_replay(geometry, measured);
        }

        let mut next = self.clone();
        let changed_content = apply_content(&mut next.cards, retained);
        let mut transition = changed_content.then_some(Transition::ContentResize);
        if backfill_count > 0 {
            apply_backfill(
                &mut next.cards,
                &measured[..backfill_count],
                geometry.policy(),
            );
            transition = Some(Transition::Backfill);
        }
        if append_start < measured.len() {
            apply_append(
                &mut next.cards,
                &measured[append_start..],
                geometry.policy(),
            );
            transition = Some(Transition::Append);
        }
        let Some(transition) = transition else {
            return self.clone();
        };
        commit(next, transition)
            .unwrap_or_else(|| Self::full_replay(geometry, measured, self.commits + 1))
    }

    fn full_replay(geometry: PlaneGeometry, measured: &[MeasuredCard<K>], commits: u64) -> Self {
        assert_unique(measured);
        let cards = mosaic::pack(
            &measured
                .iter()
                .map(|item| item.candidates.clone())
                .collect::<Vec<_>>(),
            TRACK_COUNT,
            geometry.policy(),
        );
        let mut cards = measured
            .iter()
            .zip(cards)
            .enumerate()
            .map(|(index, (item, mut card))| {
                card.creation_index = creation_index(index);
                PlaneCard {
                    key: item.key.clone(),
                    latched: item.latched,
                    content_revision: item.content_revision,
                    boundary: item.boundary,
                    card,
                }
            })
            .collect::<Vec<_>>();
        freeze_completed_epochs(&mut cards);
        assert_boundary_floors(&cards);
        let (max_row, min_base) = mosaic::extent(&placed_cards(&cards));
        Self {
            geometry,
            cards,
            max_row,
            min_base,
            commits,
            transition: Transition::FullReplay,
        }
    }

    fn structural_replay(&self, geometry: PlaneGeometry, measured: &[MeasuredCard<K>]) -> Self {
        let Some(cards) = replay_changed_epochs(&self.cards, measured, geometry.policy()) else {
            return Self::full_replay(geometry, measured, self.commits + 1);
        };
        let mut next = self.clone();
        next.cards = cards;
        commit(next, Transition::EpochReplay)
            .unwrap_or_else(|| Self::full_replay(geometry, measured, self.commits + 1))
    }

    fn geometry_replay(&self, geometry: PlaneGeometry, measured: &[MeasuredCard<K>]) -> Self {
        let mut replayed = Self::full_replay(geometry, measured, self.commits + 1);
        if segment_identity_matches(&self.cards, measured) {
            for (current, retained) in replayed.cards.iter_mut().zip(&self.cards) {
                current.card.creation_index = retained.card.creation_index;
            }
        }
        replayed
    }
}

fn creation_index(index: usize) -> usize {
    index
        .checked_mul(CREATION_INDEX_STRIDE)
        .and_then(|offset| CREATION_INDEX_ORIGIN.checked_add(offset))
        .expect("Mosaic creation index space is exhausted")
}

fn stable_boundary_matches<K: Eq>(entry: &PlaneCard<K>, measured: &MeasuredCard<K>) -> bool {
    entry.key == measured.key
        && entry.boundary
        && measured.boundary
        && entry.card.candidates == measured.candidates
        && entry.latched == measured.latched
}

fn segment_identity_matches<K: Eq>(entries: &[PlaneCard<K>], measured: &[MeasuredCard<K>]) -> bool {
    entries.len() == measured.len()
        && entries.iter().zip(measured).all(|(entry, measured)| {
            entry.key == measured.key && entry.boundary == measured.boundary
        })
}

fn replay_changed_epochs<K: Clone + Ord>(
    entries: &[PlaneCard<K>],
    measured: &[MeasuredCard<K>],
    policy: PlacementPolicy,
) -> Option<Vec<PlaneCard<K>>> {
    let measured_by_key = measured
        .iter()
        .enumerate()
        .map(|(index, item)| (&item.key, (index, item)))
        .collect::<BTreeMap<_, _>>();
    let stable_boundaries = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.boundary)
        .filter_map(|(entry_index, entry)| {
            let (measured_index, item) = measured_by_key.get(&entry.key)?;
            stable_boundary_matches(entry, item).then_some((entry_index, *measured_index))
        })
        .collect::<Vec<_>>();
    if stable_boundaries.is_empty()
        || stable_boundaries
            .windows(2)
            .any(|pair| pair[0].1 >= pair[1].1)
    {
        return None;
    }

    let mut replayed_keys = BTreeSet::new();
    let mut reconciled = Vec::with_capacity(measured.len());
    let mut entry_start = 0;
    let mut measured_start = 0;
    for (entry_end, measured_end) in stable_boundaries
        .into_iter()
        .chain(std::iter::once((entries.len(), measured.len())))
    {
        let previous_epoch = &entries[entry_start..entry_end];
        let desired_epoch = &measured[measured_start..measured_end];
        if segment_identity_matches(previous_epoch, desired_epoch) {
            reconciled.extend_from_slice(previous_epoch);
        } else {
            let replayed = replay_epoch(&reconciled, desired_epoch, entries.get(entry_end), policy);
            replayed_keys.extend(replayed.iter().map(|entry| entry.key.clone()));
            reconciled.extend(replayed);
            if let Some(upper) = entries.get(entry_end) {
                rest_below(&mut reconciled, upper.card.base);
            }
        }
        if let Some(boundary) = entries.get(entry_end) {
            reconciled.push(boundary.clone());
            entry_start = entry_end + 1;
            measured_start = measured_end + 1;
        }
    }

    apply_content(&mut reconciled, measured);
    assert_eq!(reconciled.len(), measured.len());
    assert!(
        reconciled
            .iter()
            .zip(measured)
            .all(|(entry, item)| entry.key == item.key),
        "Mosaic epoch replay preserves the measured chronology"
    );
    if !epochs_are_disjoint(&reconciled, &replayed_keys) {
        return None;
    }
    Some(reconciled)
}

fn replay_epoch<K: Clone>(
    older: &[PlaneCard<K>],
    measured: &[MeasuredCard<K>],
    upper: Option<&PlaneCard<K>>,
    policy: PlacementPolicy,
) -> Vec<PlaneCard<K>> {
    if measured.is_empty() {
        return Vec::new();
    }
    let older_cards = placed_cards(older);
    let placed = mosaic::append(&older_cards, &candidate_sets(measured), TRACK_COUNT, policy)
        .split_off(older.len());
    let indexes = creation_indexes_between(
        older.last().map(|entry| entry.card.creation_index),
        upper.map(|entry| entry.card.creation_index),
        measured.len(),
    );
    measured
        .iter()
        .zip(placed)
        .zip(indexes)
        .map(|((item, mut card), creation_index)| {
            card.creation_index = creation_index;
            plane_card(item, &card)
        })
        .collect()
}

/// Rest everything beneath a retained boundary against it, as backfill rests
/// older history against the committed plane. A replayed epoch that shrank or
/// grew then leaves the same plane a full replay of its cards would, so cards
/// a retention horizon admitted and later evicted leave no reserved gap.
fn rest_below<K>(entries: &mut [PlaneCard<K>], boundary_base: i32) {
    let Some(top) = entries
        .iter()
        .map(|entry| entry.card.base + entry.card.rows)
        .max()
    else {
        return;
    };
    let shift = boundary_base - top;
    for entry in entries {
        entry.card.base += shift;
    }
}

fn creation_indexes_between(
    lower: Option<usize>,
    upper: Option<usize>,
    count: usize,
) -> Vec<usize> {
    if count == 0 {
        return Vec::new();
    }
    match (lower, upper) {
        (Some(lower), Some(upper)) => {
            let divisor = count
                .checked_add(1)
                .expect("Mosaic creation index space is exhausted");
            let gap = upper
                .checked_sub(lower)
                .expect("Mosaic stable boundaries retain chronological order");
            let step = gap / divisor;
            assert!(step > 0, "Mosaic stable epoch has no creation-index space");
            (1..=count)
                .map(|offset| {
                    lower
                        .checked_add(
                            step.checked_mul(offset)
                                .expect("Mosaic creation index space is exhausted"),
                        )
                        .expect("Mosaic creation index space is exhausted")
                })
                .collect()
        }
        (Some(lower), None) => (1..=count)
            .map(|offset| {
                lower
                    .checked_add(
                        CREATION_INDEX_STRIDE
                            .checked_mul(offset)
                            .expect("Mosaic creation index space is exhausted"),
                    )
                    .expect("Mosaic creation index space is exhausted")
            })
            .collect(),
        (None, Some(upper)) => (0..count)
            .map(|offset| {
                let distance = CREATION_INDEX_STRIDE
                    .checked_mul(count - offset)
                    .expect("Mosaic creation index space is exhausted");
                upper
                    .checked_sub(distance)
                    .expect("Mosaic creation index space is exhausted")
            })
            .collect(),
        (None, None) => (0..count).map(creation_index).collect(),
    }
}

fn epochs_are_disjoint<K: Ord>(entries: &[PlaneCard<K>], replayed: &BTreeSet<K>) -> bool {
    for (index, entry) in entries.iter().enumerate() {
        for other in &entries[index + 1..] {
            if replayed.contains(&entry.key) == replayed.contains(&other.key) {
                continue;
            }
            if cards_overlap(&entry.card, &other.card) {
                return false;
            }
        }
    }
    true
}

fn cards_overlap(left: &Card, right: &Card) -> bool {
    left.track < right.track + right.span
        && right.track < left.track + left.span
        && left.base < right.base + right.rows
        && right.base < left.base + left.rows
}

fn apply_content<K: Clone + Eq>(
    entries: &mut [PlaneCard<K>],
    measured: &[MeasuredCard<K>],
) -> bool {
    assert_eq!(entries.len(), measured.len());
    assert!(
        entries
            .iter()
            .zip(measured)
            .all(|(entry, item)| entry.key == item.key),
        "Mosaic content reconciliation receives the committed key sequence"
    );
    let mut cards = placed_cards(entries);
    let mut changed = false;
    for (index, (entry, current)) in entries.iter_mut().zip(measured).enumerate() {
        if current.candidates == entry.card.candidates
            && current.latched == entry.latched
            && current.content_revision == entry.content_revision
        {
            continue;
        }
        changed = true;
        entry.latched = current.latched;
        entry.content_revision = current.content_revision;
        let desired = current
            .latched
            .filter(|candidate| candidate.span == cards[index].span)
            .or_else(|| {
                current
                    .candidates
                    .iter()
                    .copied()
                    .find(|candidate| candidate.span == cards[index].span)
            })
            .expect("a content resize measures the committed Mosaic span");
        cards[index].candidates = current.candidates.clone();
        if cards[index].lifecycle == mosaic::Lifecycle::Frozen {
            cards =
                mosaic::resolve_frozen_resize(&cards, cards[index].creation_index, desired.rows);
        } else {
            cards[index].rows = desired.rows;
        }
    }
    if changed {
        cards = mosaic::replay_liquid(&cards, TRACK_COUNT, mosaic::FREEZE_DEPTH);
        replace_cards(entries, &cards);
    }
    changed
}

/// Return the contiguous older prefix and newer suffix around the complete
/// committed key sequence. An unseen middle key or retained-order change is a
/// structural insertion and must replay the affected compaction epochs.
fn edge_additions<K: Eq>(
    committed: &[PlaneCard<K>],
    measured: &[MeasuredCard<K>],
) -> Option<(usize, usize)> {
    if committed.is_empty() {
        return Some((0, 0));
    }
    let backfill_count = measured
        .iter()
        .position(|item| item.key == committed[0].key)?;
    let append_start = backfill_count.checked_add(committed.len())?;
    (append_start <= measured.len()
        && measured[backfill_count..append_start]
            .iter()
            .zip(committed)
            .all(|(item, entry)| item.key == entry.key))
    .then_some((backfill_count, append_start))
}

fn apply_backfill<K: Clone>(
    entries: &mut Vec<PlaneCard<K>>,
    older: &[MeasuredCard<K>],
    policy: PlacementPolicy,
) {
    let current = placed_cards(entries);
    let placed = mosaic::backfill(&current, &candidate_sets(older), TRACK_COUNT, policy);
    let indexes = creation_indexes_between(
        None,
        entries.first().map(|entry| entry.card.creation_index),
        older.len(),
    );
    let mut introduced = older
        .iter()
        .zip(placed.iter())
        .zip(indexes)
        .map(|((item, card), creation_index)| {
            let mut card = card.clone();
            card.creation_index = creation_index;
            plane_card(item, &card)
        })
        .collect::<Vec<_>>();
    for (entry, card) in entries.iter_mut().zip(&placed[older.len()..]) {
        let creation_index = entry.card.creation_index;
        entry.card = card.clone();
        entry.card.creation_index = creation_index;
    }
    introduced.append(entries);
    *entries = introduced;
}

fn apply_append<K: Clone>(
    entries: &mut Vec<PlaneCard<K>>,
    appended: &[MeasuredCard<K>],
    policy: PlacementPolicy,
) {
    let current = placed_cards(entries);
    let placed = mosaic::append(&current, &candidate_sets(appended), TRACK_COUNT, policy);
    let indexes = creation_indexes_between(
        entries.last().map(|entry| entry.card.creation_index),
        None,
        appended.len(),
    );
    replace_cards(entries, &placed[..current.len()]);
    entries.extend(
        appended
            .iter()
            .zip(&placed[current.len()..])
            .zip(indexes)
            .map(|((item, card), creation_index)| {
                let mut card = card.clone();
                card.creation_index = creation_index;
                plane_card(item, &card)
            }),
    );
}

fn candidate_sets<K>(measured: &[MeasuredCard<K>]) -> Vec<Vec<Candidate>> {
    measured
        .iter()
        .map(|item| item.candidates.clone())
        .collect()
}

fn placed_cards<K>(entries: &[PlaneCard<K>]) -> Vec<Card> {
    entries.iter().map(|entry| entry.card.clone()).collect()
}

fn plane_card<K: Clone>(item: &MeasuredCard<K>, card: &Card) -> PlaneCard<K> {
    PlaneCard {
        key: item.key.clone(),
        latched: item.latched,
        content_revision: item.content_revision,
        boundary: item.boundary,
        card: card.clone(),
    }
}

/// Admit one incremental result only while every retained boundary still
/// separates its chronological epochs. A candidate that cannot honor those
/// fixed floors belongs to the deterministic full-replay path.
fn commit<K>(
    mut plane: PersistentLanePlane<K>,
    transition: Transition,
) -> Option<PersistentLanePlane<K>> {
    freeze_completed_epochs(&mut plane.cards);
    if !boundary_floors_hold(&plane.cards) {
        return None;
    }
    let (max_row, min_base) = mosaic::extent(&placed_cards(&plane.cards));
    plane.max_row = max_row;
    plane.min_base = min_base;
    plane.commits += 1;
    plane.transition = transition;
    Some(plane)
}

fn freeze_completed_epochs<K>(entries: &mut [PlaneCard<K>]) {
    let Some(boundary) = entries.iter().rposition(|entry| entry.boundary) else {
        return;
    };
    for entry in &mut entries[..=boundary] {
        entry.card.lifecycle = mosaic::Lifecycle::Frozen;
    }
}

fn boundary_floors_hold<K>(entries: &[PlaneCard<K>]) -> bool {
    let mut highest_top = None::<i32>;
    let mut epoch_floor = None::<i32>;
    for entry in entries {
        if epoch_floor.is_some_and(|floor| entry.card.base < floor) {
            return false;
        }
        if entry.boundary {
            if (entry.card.track, entry.card.span) != (0, TRACK_COUNT)
                || highest_top.is_some_and(|top| top > entry.card.base)
            {
                return false;
            }
            epoch_floor = Some(entry.card.base + entry.card.rows);
        }
        let top = entry.card.base + entry.card.rows;
        highest_top = Some(highest_top.map_or(top, |highest| highest.max(top)));
    }
    true
}

fn assert_boundary_floors<K>(entries: &[PlaneCard<K>]) {
    assert!(
        boundary_floors_hold(entries),
        "Mosaic placement boundaries divide chronological epochs"
    );
}

fn assert_unique<K: Ord>(measured: &[MeasuredCard<K>]) {
    let mut keys = BTreeSet::new();
    for item in measured {
        assert!(
            keys.insert(&item.key),
            "persistent Mosaic input contains a duplicate card identity"
        );
        assert!(
            !item.candidates.is_empty(),
            "persistent Mosaic card has no measurement candidate"
        );
    }
}

fn replace_cards<K>(entries: &mut [PlaneCard<K>], cards: &[Card]) {
    assert_eq!(entries.len(), cards.len());
    for (entry, card) in entries.iter_mut().zip(cards) {
        entry.card = card.clone();
    }
}

/// One independently packed lane plane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LanePlane {
    /// Cards placed in stream creation order.
    pub cards: Vec<Card>,
    /// Highest occupied lattice row.
    pub max_row: i32,
    /// Lowest occupied lattice base.
    pub min_base: i32,
}

impl LanePlane {
    /// Number of rows the CSS grid must expose, including a negative ripple.
    #[must_use]
    pub fn rows(&self) -> i32 {
        (self.max_row - self.min_base).max(1)
    }
}

/// Pack one lane without carrying a skyline in from any sibling lane.
#[must_use]
pub fn arrange(candidate_sets: &[Vec<Candidate>], policy: PlacementPolicy) -> LanePlane {
    let cards = mosaic::pack(candidate_sets, TRACK_COUNT, policy);
    let (max_row, min_base) = mosaic::extent(&cards);
    LanePlane {
        cards,
        max_row,
        min_base,
    }
}
