//! Pure lattice used by the Mosaic view.
//!
//! Placement is derived only from creation order, measured candidates, and
//! the current geometry. Browser measurement and rendering remain outside
//! this module, which keeps replay deterministic and lets native tests prove
//! the same decisions the Wasm view applies.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

/// Number of tracks in every Mosaic lattice.
pub const TRACK_COUNT: usize = 12;
/// Number of later overlapping layers that latches a card as frozen.
pub const FREEZE_DEPTH: usize = 2;
/// Scroll distance after which incremental pushes preserve the viewport.
pub const NEAR_TOP_THRESHOLD_PX: f64 = 80.0;
/// Distance from the trailing edge that admits another retained-history page.
pub const HISTORY_EDGE_THRESHOLD_PX: f64 = 400.0;
/// Base-width height that admits wider measurements.
pub const TALL_THRESHOLD_PX: f64 = 230.0;
/// Smallest readable card height in vertical lattice modules.
pub const MIN_CARD_ROWS: i32 = 3;

const LEGAL_BASE_SPANS: [usize; 5] = [2, 3, 4, 6, 12];
const GAP_PX: i32 = 9;
const MIN_LANE_REM: f64 = 17.0;
const MODULE_LINE_HEIGHT_REM: f64 = 1.25;

/// Convert the DOM client box, which includes padding, into usable content width.
#[must_use]
pub fn content_box_width(client_width_px: f64, padding_left_px: f64, padding_right_px: f64) -> f64 {
    (client_width_px - padding_left_px - padding_right_px).max(0.0)
}

/// Geometry resolved from the live root type size and host width.
#[derive(Clone, Debug, PartialEq)]
pub struct Geometry {
    /// Integer seam shared by tracks and rows, matching the card content inset.
    pub gap: i32,
    /// Minimum usable lane width.
    pub min_lane: f64,
    /// Fractional width of one of the twelve tracks.
    pub column_width: f64,
    /// Smallest legal span that clears [`Self::min_lane`].
    pub base_span: usize,
    /// Number of base lanes available at this width.
    pub lane_count: usize,
    /// Type-ramp lines in one vertical module: coarse enough at wider lane
    /// counts that neighbouring card tops align and wide placements find room.
    pub module_lines: usize,
    /// Integer vertical module height.
    pub module: i32,
    /// Shared rounded track edges, including both outer edges.
    pub edges: Vec<i32>,
}

impl Geometry {
    /// Resolve one geometry pass.
    #[must_use]
    pub fn new(root_font_size_px: f64, container_width_px: f64) -> Self {
        let rem = positive_or(root_font_size_px, 16.0);
        let gap = GAP_PX;
        // The same seam also pads the lattice perimeter. Carve both edges from
        // the admitted width before resolving tracks; never grow the host.
        let width = (positive_or(container_width_px, 0.0) - 2.0 * f64::from(gap)).max(0.0);
        let min_lane = MIN_LANE_REM * rem;
        let column_width = (width - (TRACK_COUNT - 1) as f64 * f64::from(gap)) / TRACK_COUNT as f64;
        let base_span = base_span(column_width, f64::from(gap), min_lane);
        let lane_count = TRACK_COUNT / base_span;
        let module_lines = module_lines(lane_count);
        let module = (module_lines as f64 * MODULE_LINE_HEIGHT_REM * rem).round() as i32;
        let edges = (0..=TRACK_COUNT)
            .map(|track| (track as f64 * (column_width + f64::from(gap))).round() as i32)
            .collect();
        Self {
            gap,
            min_lane,
            column_width,
            base_span,
            lane_count,
            module_lines,
            module,
            edges,
        }
    }

    /// Normal card spans admitted at this width.
    ///
    /// A multi-lane plane always retains one lane beside a card. A single-lane
    /// plane necessarily admits its only lane.
    pub fn normal_spans(&self) -> impl ExactSizeIterator<Item = usize> + '_ {
        let admitted_lanes = self.lane_count.saturating_sub(1).max(1);
        (0..admitted_lanes).map(|index| self.base_span * (index + 1))
    }

    /// Rendered width for one anchored card, using the shared edge table.
    #[must_use]
    pub fn card_width(&self, track: usize, span: usize) -> i32 {
        let start = track.min(TRACK_COUNT);
        let end = track.saturating_add(span).min(TRACK_COUNT);
        self.edges[end] - self.edges[start] - self.gap
    }
}

fn positive_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        fallback
    }
}

fn base_span(column_width: f64, gap: f64, min_lane: f64) -> usize {
    LEGAL_BASE_SPANS
        .into_iter()
        .find(|span| *span as f64 * column_width + (*span - 1) as f64 * gap >= min_lane)
        .unwrap_or(TRACK_COUNT)
}

fn module_lines(lane_count: usize) -> usize {
    match lane_count {
        3.. => 3,
        2 => 2,
        _ => 1,
    }
}

/// One measured width/height option offered to the placement key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Candidate {
    /// Track span measured by the caller.
    pub span: usize,
    /// Natural height converted into vertical modules.
    pub rows: i32,
    /// Distance from the preferred portrait ratio, in track-row units.
    pub shape_cost: i64,
}

impl Candidate {
    /// Build a synthetic candidate with no measured shape preference.
    ///
    /// Production measurements use [`candidate_for_height`].
    #[must_use]
    pub const fn synthetic(span: usize, rows: i32) -> Self {
        Self {
            span,
            rows,
            shape_cost: 0,
        }
    }
}

/// Ephemeral viewport policy applied while placing one deterministic plane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlacementPolicy {
    viewport_rows: i32,
}

impl PlacementPolicy {
    /// Build a policy from the number of vertical modules visible in the plane.
    #[must_use]
    pub const fn new(viewport_rows: i32) -> Self {
        Self {
            viewport_rows: if viewport_rows > 0 { viewport_rows } else { 1 },
        }
    }

    /// Vertical modules visible in the plane.
    #[must_use]
    pub const fn viewport_rows(self) -> i32 {
        self.viewport_rows
    }
}

/// The two card lifecycle states.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lifecycle {
    /// The card can still settle vertically as deferred content resolves.
    Liquid,
    /// The card has enough burial depth that its placement is latched.
    Frozen,
}

impl Lifecycle {
    /// Stable DOM/CSS spelling of this state.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Liquid => "liquid",
            Self::Frozen => "frozen",
        }
    }
}

/// One placed card in the lattice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Card {
    /// Stable stream creation order.
    pub creation_index: usize,
    /// Zero-based anchor track.
    pub track: usize,
    /// Number of tracks occupied.
    pub span: usize,
    /// Base row; rows increase toward the leading edge.
    pub base: i32,
    /// Number of vertical modules occupied.
    pub rows: i32,
    /// Current lifecycle latch.
    pub lifecycle: Lifecycle,
    /// Measurements used when a full replay re-decides placement.
    pub candidates: Vec<Candidate>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Anchor {
    base: i32,
    waste: i32,
    track: usize,
    span: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Placement {
    track: usize,
    span: usize,
    base: i32,
    rows: i32,
}

/// Convert a pixel height into vertical modules.
#[must_use]
pub fn rows_for(px: f64, gap: i32, module: i32) -> i32 {
    let module = module.max(1);
    MIN_CARD_ROWS.max(((px + f64::from(gap)) / f64::from(module)).ceil() as i32)
}

/// Convert a viewport height into the count of complete visible modules.
#[must_use]
pub fn viewport_rows_for(px: f64, gap: i32, module: i32) -> i32 {
    let module = module.max(1);
    1.max(((px + f64::from(gap)) / f64::from(module)).floor() as i32)
}

/// Convert a measured chronological divider into compact vertical modules.
///
/// Dividers are labels rather than cards, so they need their measured height
/// but not the ordinary three-module reading floor.
#[must_use]
pub fn boundary_rows_for(px: f64, gap: i32, module: i32) -> i32 {
    let module = module.max(1);
    1.max(((px + f64::from(gap)) / f64::from(module)).ceil() as i32)
}

/// Upper edge of the preferred compact-reading range. Square through golden
/// portrait shapes pay no aspect cost; flatter or taller shapes pay their
/// distance from that range.
pub const MAX_PREFERRED_CARD_HEIGHT_TO_WIDTH: f64 = 1.618_033_988_749_895;

/// Convert one rendered normal-card measurement into a placement candidate.
#[must_use]
pub fn candidate_for_height(geometry: &Geometry, span: usize, height_px: f64) -> Candidate {
    let rows = rows_for(height_px, geometry.gap, geometry.module);
    let width_px = f64::from(geometry.card_width(0, span).max(1));
    let max_height = width_px * MAX_PREFERRED_CARD_HEIGHT_TO_WIDTH;
    let deviation_px = if height_px < width_px {
        width_px - height_px
    } else {
        (height_px - max_height).max(0.0)
    };
    let deviation_rows = (deviation_px / f64::from(geometry.module.max(1))).ceil() as i64;
    Candidate {
        span,
        rows,
        shape_cost: deviation_rows.saturating_mul(span as i64),
    }
}

/// Build the non-dominated width/height choices for one normal card.
pub fn span_candidates(
    geometry: &Geometry,
    mut measure: impl FnMut(usize) -> f64,
) -> Vec<Candidate> {
    span_candidates_with_threshold(geometry, &mut measure, TALL_THRESHOLD_PX)
}

fn span_candidates_with_threshold(
    geometry: &Geometry,
    measure: &mut impl FnMut(usize) -> f64,
    threshold: f64,
) -> Vec<Candidate> {
    let base_px = measure(geometry.base_span);
    let base = candidate_for_height(geometry, geometry.base_span, base_px);
    let mut candidates = vec![base];
    if base_px < threshold || base.rows == MIN_CARD_ROWS {
        return candidates;
    }

    let mut shortest = base.rows;
    let mut best_shape = base.shape_cost;
    for span in geometry.normal_spans().skip(1) {
        let candidate = candidate_for_height(geometry, span, measure(span));
        // A wider tier earns admission when it improves either the height or
        // reading-shape frontier. A plateau cannot stop measurement: row
        // quantization can hide an improvement until a later tier.
        if candidate.rows < shortest || candidate.shape_cost < best_shape {
            shortest = shortest.min(candidate.rows);
            best_shape = best_shape.min(candidate.shape_cost);
            candidates.push(candidate);
            if shortest == MIN_CARD_ROWS && best_shape == 0 {
                break;
            }
        }
    }
    candidates
}

fn normalize_tracks(track_count: usize) -> usize {
    track_count.max(1)
}

fn clamp_span(span: usize, tracks: usize) -> usize {
    span.max(1).min(normalize_tracks(tracks))
}

/// Derive the skyline from a placed card set.
///
/// A track no card covers rests at `origin`, the top of the plane the cards
/// belong to: row zero until backfill carries a plane above it.
#[must_use]
pub fn derive_row_floor(cards: &[Card], origin: i32, track_count: usize) -> Vec<i32> {
    let tracks = normalize_tracks(track_count);
    let mut floor = vec![origin; tracks];
    for card in cards {
        let top = card.base + card.rows;
        let end = card.track.saturating_add(card.span).min(tracks);
        for value in floor.iter_mut().take(end).skip(card.track.min(tracks)) {
            *value = (*value).max(top);
        }
    }
    floor
}

fn anchor_at(track: usize, span: usize, floor: &[i32], chronological_floor: i32) -> Anchor {
    let covered = &floor[track..track + span];
    let base = covered
        .iter()
        .copied()
        .max()
        .unwrap_or(chronological_floor)
        .max(chronological_floor);
    let waste = covered.iter().map(|value| base - value).sum();
    Anchor {
        base,
        waste,
        track,
        span,
    }
}

fn anchor_for(span: usize, floor: &[i32], track_count: usize, chronological_floor: i32) -> Anchor {
    let tracks = normalize_tracks(track_count);
    let span = clamp_span(span, tracks);
    let mut best = anchor_at(0, span, floor, chronological_floor);
    for track in 1..=tracks - span {
        let candidate = anchor_at(track, span, floor, chronological_floor);
        if (candidate.base, candidate.waste, candidate.track) < (best.base, best.waste, best.track)
        {
            best = candidate;
        }
    }
    best
}

fn skyline_debt(
    floor: &[i32],
    anchor: Anchor,
    rows: i32,
    next_max: i32,
    policy: PlacementPolicy,
) -> i64 {
    let top = anchor.base + rows;
    let end = anchor.track + anchor.span;
    floor
        .iter()
        .enumerate()
        .map(|(track, current)| {
            let next = if (anchor.track..end).contains(&track) {
                top.max(*current)
            } else {
                *current
            };
            i64::from((next_max - next - policy.viewport_rows()).max(0))
        })
        .sum()
}

fn decide(
    candidates: &[Candidate],
    floor: &[i32],
    track_count: usize,
    movement_weight: i64,
    chronological_floor: i32,
    policy: PlacementPolicy,
) -> Placement {
    let current_max = floor.iter().copied().max().unwrap_or(0);
    let mut decisions = candidates.iter().map(|candidate| {
        let anchor = anchor_for(candidate.span, floor, track_count, chronological_floor);
        let top = anchor.base + candidate.rows;
        let next_max = current_max.max(top);
        let growth = i64::from(next_max - current_max);
        let movement = growth.saturating_mul(movement_weight);
        let debt = skyline_debt(floor, anchor, candidate.rows, next_max, policy);
        let waste = i64::from(anchor.waste);
        let total = movement
            .saturating_add(debt)
            .saturating_add(waste)
            .saturating_add(candidate.shape_cost);
        let score = (
            total,
            movement,
            debt,
            candidate.shape_cost,
            waste,
            anchor.span,
            anchor.track,
        );
        (
            score,
            Placement {
                track: anchor.track,
                span: anchor.span,
                base: anchor.base,
                rows: candidate.rows,
            },
        )
    });
    let first = decisions
        .next()
        .expect("a Mosaic card has at least one measured candidate");
    decisions
        .fold(first, |best, candidate| {
            if candidate.0 < best.0 {
                candidate
            } else {
                best
            }
        })
        .1
}

fn commit(floor: &[i32], card: &Card, track_count: usize) -> Vec<i32> {
    let tracks = normalize_tracks(track_count);
    let mut next = floor.to_vec();
    let top = card.base + card.rows;
    let end = card.track.saturating_add(card.span).min(tracks);
    for value in next.iter_mut().take(end).skip(card.track.min(tracks)) {
        *value = (*value).max(top);
    }
    next
}

/// Insert measured cards in creation order and latch frozen cards after placement.
#[must_use]
pub fn pack(
    candidate_sets: &[Vec<Candidate>],
    track_count: usize,
    policy: PlacementPolicy,
) -> Vec<Card> {
    let tracks = normalize_tracks(track_count);
    let mut cards = Vec::new();
    let mut floor = vec![0; tracks];
    let mut movement_weight = 0_i64;
    let mut chronological_floor = 0;
    for (creation_index, candidates) in candidate_sets.iter().enumerate() {
        let placement = decide(
            candidates,
            &floor,
            tracks,
            movement_weight,
            chronological_floor,
            policy,
        );
        let card = Card {
            creation_index,
            track: placement.track,
            span: placement.span,
            base: placement.base,
            rows: placement.rows,
            lifecycle: Lifecycle::Liquid,
            candidates: candidates.clone(),
        };
        movement_weight = movement_weight.saturating_add(card.span as i64);
        chronological_floor = card.base;
        floor = commit(&floor, &card, tracks);
        cards.push(card);
    }
    recompute_frozen(&cards, FREEZE_DEPTH)
}

/// Append newly measured cards without re-deciding any committed placement.
///
/// This is the live-arrival path. Re-running [`pack`] over the complete stream
/// happens to reproduce many simple prefixes, but it discards the lifecycle
/// latches and content-resize history that make a live plane stable.
#[must_use]
pub fn append(
    cards: &[Card],
    candidate_sets: &[Vec<Candidate>],
    track_count: usize,
    policy: PlacementPolicy,
) -> Vec<Card> {
    if candidate_sets.is_empty() {
        return cards.to_vec();
    }
    let tracks = normalize_tracks(track_count);
    let mut appended = cards.to_vec();
    let mut floor = derive_row_floor(cards, extent(cards).1, tracks);
    let mut movement_weight = cards.iter().fold(0_i64, |weight, card| {
        weight.saturating_add(card.span as i64)
    });
    let mut chronological_floor = cards
        .iter()
        .max_by_key(|card| card.creation_index)
        .map_or(0, |card| card.base);
    let first_index = cards
        .iter()
        .map(|card| card.creation_index)
        .max()
        .map_or(0, |index| {
            index
                .checked_add(1)
                .expect("Mosaic creation index space is exhausted")
        });
    for (offset, candidates) in candidate_sets.iter().enumerate() {
        let placement = decide(
            candidates,
            &floor,
            tracks,
            movement_weight,
            chronological_floor,
            policy,
        );
        let card = Card {
            creation_index: first_index
                .checked_add(offset)
                .expect("Mosaic creation index space is exhausted"),
            track: placement.track,
            span: placement.span,
            base: placement.base,
            rows: placement.rows,
            lifecycle: Lifecycle::Liquid,
            candidates: candidates.clone(),
        };
        movement_weight = movement_weight.saturating_add(card.span as i64);
        chronological_floor = card.base;
        floor = commit(&floor, &card, tracks);
        appended.push(card);
    }
    recompute_frozen(&appended, FREEZE_DEPTH)
}

/// Place older backfill below a committed plane without moving its cards.
///
/// Creation indices are labels rather than coordinates.  Renumbering them is
/// therefore safe here: the backfill receives the earlier labels, while every
/// existing card retains its exact track, span, base, and row reservation.
#[must_use]
pub fn backfill(
    cards: &[Card],
    candidate_sets: &[Vec<Candidate>],
    track_count: usize,
    policy: PlacementPolicy,
) -> Vec<Card> {
    if candidate_sets.is_empty() {
        return cards.to_vec();
    }
    let mut older = pack(candidate_sets, track_count, policy);
    let (older_max, _) = extent(&older);
    let (_, committed_min) = extent(cards);
    let shift = committed_min - older_max;
    for card in &mut older {
        card.base += shift;
    }
    let offset = older.len();
    let mut committed = cards.to_vec();
    for card in &mut committed {
        card.creation_index = card
            .creation_index
            .checked_add(offset)
            .expect("Mosaic creation index space is exhausted");
    }
    older.extend(committed);
    recompute_frozen(&older, FREEZE_DEPTH)
}

fn tracks_overlap(left: &Card, right: &Card) -> bool {
    left.track < right.track + right.span && right.track < left.track + left.span
}

/// Latch cards whose burial depth reaches the configured threshold.
#[must_use]
pub fn recompute_frozen(cards: &[Card], freeze_depth: usize) -> Vec<Card> {
    let mut order = (0..cards.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| Reverse(cards[*index].creation_index));
    let track_count = cards
        .iter()
        .map(|card| card.track.saturating_add(card.span))
        .max()
        .unwrap_or(0);
    let mut later_coverage = vec![0_usize; track_count];
    let mut depths = vec![0_usize; cards.len()];
    let mut group_start = 0;
    while group_start < order.len() {
        let creation_index = cards[order[group_start]].creation_index;
        let group_end = order[group_start..]
            .iter()
            .position(|index| cards[*index].creation_index != creation_index)
            .map_or(order.len(), |offset| group_start + offset);
        for index in &order[group_start..group_end] {
            let card = &cards[*index];
            depths[*index] = later_coverage[card.track..card.track + card.span]
                .iter()
                .copied()
                .min()
                .unwrap_or(0);
        }
        for index in &order[group_start..group_end] {
            let card = &cards[*index];
            for coverage in &mut later_coverage[card.track..card.track + card.span] {
                *coverage += 1;
            }
        }
        group_start = group_end;
    }
    cards
        .iter()
        .enumerate()
        .map(|(index, card)| {
            if card.lifecycle == Lifecycle::Frozen || depths[index] >= freeze_depth {
                let mut frozen = card.clone();
                frozen.lifecycle = Lifecycle::Frozen;
                frozen
            } else {
                card.clone()
            }
        })
        .collect()
}

fn rest_liquid(
    card: &Card,
    floor: &[i32],
    track_count: usize,
    chronological_floor: i32,
) -> (Card, Vec<i32>) {
    let tracks = normalize_tracks(track_count);
    let end = card.track.saturating_add(card.span).min(tracks);
    let base = floor
        .iter()
        .take(end)
        .skip(card.track.min(tracks))
        .copied()
        .max()
        .unwrap_or(chronological_floor)
        .max(chronological_floor);
    let mut placed = card.clone();
    placed.base = base;
    let floor = commit(floor, &placed, tracks);
    (placed, floor)
}

/// Re-rest liquid cards vertically while retaining their insert-time track and span.
#[must_use]
pub fn replay_liquid(cards: &[Card], track_count: usize, freeze_depth: usize) -> Vec<Card> {
    let frozen: Vec<_> = cards
        .iter()
        .filter(|card| card.lifecycle == Lifecycle::Frozen)
        .cloned()
        .collect();
    let mut floor = derive_row_floor(&frozen, extent(cards).1, track_count);
    let mut ordered: Vec<_> = cards
        .iter()
        .filter(|card| card.lifecycle == Lifecycle::Liquid)
        .cloned()
        .collect();
    ordered.sort_by_key(|card| card.creation_index);
    let mut placed = BTreeMap::new();
    let mut chronological_floor = extent(cards).1;
    for card in ordered {
        let frozen_floor = frozen
            .iter()
            .filter(|prior| prior.creation_index < card.creation_index)
            .map(|prior| prior.base)
            .max()
            .unwrap_or(chronological_floor);
        let (card, next) = rest_liquid(
            &card,
            &floor,
            track_count,
            chronological_floor.max(frozen_floor),
        );
        chronological_floor = card.base;
        floor = next;
        placed.insert(card.creation_index, card);
    }
    let replayed = cards
        .iter()
        .map(|card| {
            placed
                .get(&card.creation_index)
                .cloned()
                .unwrap_or_else(|| card.clone())
        })
        .collect::<Vec<_>>();
    recompute_frozen(&replayed, freeze_depth)
}

/// Grow one card downward and move every transitively lower overlapping card.
#[must_use]
pub fn ripple_rows(cards: &[Card], creation_index: usize, delta: i32) -> Vec<Card> {
    let growing = cards
        .iter()
        .find(|card| card.creation_index == creation_index)
        .unwrap_or_else(|| {
            panic!("Mosaic creation index {creation_index} is absent from the card set")
        });
    let mut shifted = BTreeMap::new();
    let mut seen = BTreeSet::from([creation_index]);
    let mut stack = vec![growing.creation_index];
    while let Some(frontier_index) = stack.pop() {
        let frontier = cards
            .iter()
            .find(|card| card.creation_index == frontier_index)
            .expect("ripple frontier originated in the card set");
        for candidate in cards {
            if seen.contains(&candidate.creation_index) {
                continue;
            }
            if tracks_overlap(candidate, frontier) && candidate.base < frontier.base {
                shifted.insert(candidate.creation_index, candidate.base - delta);
                seen.insert(candidate.creation_index);
                stack.push(candidate.creation_index);
            }
        }
    }
    cards
        .iter()
        .map(|card| {
            let mut next = card.clone();
            if card.creation_index == creation_index {
                next.base -= delta;
                next.rows += delta;
            } else if let Some(base) = shifted.get(&card.creation_index) {
                next.base = *base;
            }
            next
        })
        .collect()
}

/// Resolve a frozen card resize, retaining its reservation for shrinkage.
#[must_use]
pub fn resolve_frozen_resize(cards: &[Card], creation_index: usize, new_rows: i32) -> Vec<Card> {
    let card = cards
        .iter()
        .find(|card| card.creation_index == creation_index)
        .unwrap_or_else(|| {
            panic!("Mosaic creation index {creation_index} is absent from the card set")
        });
    if new_rows <= card.rows {
        cards.to_vec()
    } else {
        ripple_rows(cards, creation_index, new_rows - card.rows)
    }
}

/// Clear lifecycle latches and re-decide every card from current measurements.
#[must_use]
pub fn full_replay(
    cards: &[Card],
    track_count: usize,
    freeze_depth: usize,
    policy: PlacementPolicy,
) -> Vec<Card> {
    let tracks = normalize_tracks(track_count);
    let mut ordered = cards.to_vec();
    ordered.sort_by_key(|card| card.creation_index);
    let mut floor = vec![0; tracks];
    let mut movement_weight = 0_i64;
    let mut chronological_floor = 0;
    let mut placed = BTreeMap::new();
    for card in ordered {
        let decision = decide(
            &card.candidates,
            &floor,
            tracks,
            movement_weight,
            chronological_floor,
            policy,
        );
        let card = Card {
            creation_index: card.creation_index,
            track: decision.track,
            span: decision.span,
            base: decision.base,
            rows: decision.rows,
            lifecycle: Lifecycle::Liquid,
            candidates: card.candidates,
        };
        movement_weight = movement_weight.saturating_add(card.span as i64);
        chronological_floor = card.base;
        floor = commit(&floor, &card, tracks);
        placed.insert(card.creation_index, card);
    }
    let replayed = cards
        .iter()
        .map(|card| {
            placed
                .get(&card.creation_index)
                .cloned()
                .expect("full replay placed every input card")
        })
        .collect::<Vec<_>>();
    recompute_frozen(&replayed, freeze_depth)
}

/// Highest and lowest occupied lattice rows.
#[must_use]
pub fn extent(cards: &[Card]) -> (i32, i32) {
    cards.iter().fold((0, 0), |(max_row, min_base), card| {
        (max_row.max(card.base + card.rows), min_base.min(card.base))
    })
}

/// Move composer exposure with the plane's scroll motion.
///
/// The composer overlays the plane above a spacer of its own height, so moving
/// it exactly as far as the cards scrolled keeps it fused to the stream. It
/// never hides more than the plane has scrolled, so reaching the top always
/// reveals it rather than exposing the empty spacer.
#[cfg(any(target_arch = "wasm32", test))]
#[must_use]
pub(crate) fn composer_collapse_offset(
    current: f64,
    maximum: f64,
    scrolled: f64,
    scroll_top: f64,
) -> f64 {
    (current + scrolled).clamp(0.0, maximum.min(scroll_top).max(0.0))
}

/// Whether the pane is far enough from its leading edge to preserve its viewport.
#[must_use]
pub fn pane_is_scrolled(scroll_top: f64, threshold: Option<f64>) -> bool {
    scroll_top
        > threshold
            .filter(|value| value.is_finite())
            .unwrap_or(NEAR_TOP_THRESHOLD_PX)
}

/// Whether a scroller is close enough to its trailing edge to page older history.
#[must_use]
pub fn history_edge_is_near(
    scroll_top: f64,
    client_height: f64,
    scroll_height: f64,
    threshold: Option<f64>,
) -> bool {
    scroll_height - client_height - scroll_top
        <= threshold
            .filter(|value| value.is_finite())
            .unwrap_or(HISTORY_EDGE_THRESHOLD_PX)
}

#[cfg(test)]
mod tests {
    use super::composer_collapse_offset;

    #[test]
    fn composer_moves_exactly_as_far_as_the_cards_until_an_exposure_edge() {
        // Scrolling down from the top hides the composer pixel for pixel.
        assert_eq!(composer_collapse_offset(0.0, 140.0, 37.5, 37.5), 37.5);
        // Scrolling back up mid-stream reveals it by the same distance.
        assert_eq!(composer_collapse_offset(140.0, 140.0, -25.0, 975.0), 115.0);
        // Strong motion stops at either edge without leaving a remainder.
        assert_eq!(composer_collapse_offset(20.0, 140.0, 900.0, 5_000.0), 140.0);
        assert_eq!(composer_collapse_offset(120.0, 140.0, -900.0, 2_000.0), 0.0);
        // A composer hidden further than the plane is scrolled reveals at the top.
        assert_eq!(composer_collapse_offset(140.0, 140.0, -60.0, 0.0), 0.0);
    }
}
