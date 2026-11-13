use std::cell::Cell;

use app::mosaic::{
    Candidate, Card, FREEZE_DEPTH, Geometry, Lifecycle, MIN_CARD_ROWS, PlacementPolicy,
    TALL_THRESHOLD_PX, TRACK_COUNT, append, boundary_rows_for, candidate_for_height,
    content_box_width, extent, full_replay, history_edge_is_near, pack, pane_is_scrolled,
    replay_liquid, resolve_frozen_resize, ripple_rows, rows_for, span_candidates,
    viewport_rows_for,
};

fn candidates(span: usize, rows: i32) -> Vec<Candidate> {
    vec![Candidate::synthetic(span, rows)]
}

fn prose_candidates(geometry: &Geometry, heights: [f64; 5]) -> Vec<Candidate> {
    span_candidates(geometry, |span| match span {
        2 => heights[0],
        4 => heights[1],
        6 => heights[2],
        8 => heights[3],
        10 => heights[4],
        _ => panic!("unexpected prose span {span}"),
    })
}

fn policy() -> PlacementPolicy {
    PlacementPolicy::new(12)
}

/// Minimum later-card coverage across every track a card occupies: the
/// strict definition `recompute_frozen` answers in one pass.
fn burial_depth(card: &Card, cards: &[Card]) -> usize {
    (card.track..card.track + card.span)
        .map(|track| {
            cards
                .iter()
                .filter(|other| {
                    other.creation_index > card.creation_index
                        && track >= other.track
                        && track < other.track + other.span
                })
                .count()
        })
        .min()
        .unwrap_or(0)
}

fn card(index: usize, track: usize, span: usize, base: i32, rows: i32) -> Card {
    Card {
        creation_index: index,
        track,
        span,
        base,
        rows,
        lifecycle: Lifecycle::Liquid,
        candidates: candidates(span, rows),
    }
}

#[test]
fn geometry_uses_the_minimum_lane_inequality_and_shared_edges() {
    let four_lanes = Geometry::new(16.0, 1440.0);
    assert_eq!(four_lanes.gap, 9);
    assert_eq!(four_lanes.base_span, 3);
    assert_eq!(four_lanes.lane_count, 4);
    assert_eq!(four_lanes.module_lines, 3);
    assert_eq!(four_lanes.module, 60);
    assert_eq!(four_lanes.edges[0], 0);
    assert_eq!(four_lanes.edges[TRACK_COUNT], 1431);
    assert_eq!(four_lanes.card_width(0, TRACK_COUNT), 1422);

    assert_eq!(Geometry::new(16.0, 1000.0).base_span, 4);
    assert_eq!(Geometry::new(16.0, 700.0).base_span, 6);
    let one_lane = Geometry::new(16.0, 300.0);
    assert_eq!(one_lane.base_span, 12);
    assert_eq!((one_lane.module_lines, one_lane.module), (1, 20));
}

#[test]
fn geometry_uses_content_width_instead_of_the_padded_client_box() {
    let unpadded = Geometry::new(16.0, content_box_width(810.0, 0.0, 0.0));
    let padded = Geometry::new(16.0, content_box_width(840.0, 15.0, 15.0));
    let geometry_key =
        |geometry: &Geometry| (geometry.base_span, geometry.lane_count, geometry.module);

    assert_eq!(geometry_key(&unpadded), (6, 2, 40));
    assert_eq!(geometry_key(&padded), geometry_key(&unpadded));
}

#[test]
fn short_cards_keep_three_vertical_modules_without_rounding_down_long_cards() {
    let geometry = Geometry::new(16.0, 1440.0);

    assert_eq!(MIN_CARD_ROWS, 3);
    assert_eq!(rows_for(1.0, geometry.gap, geometry.module), 3);
    assert_eq!(rows_for(240.0, geometry.gap, geometry.module), 5);
}

#[test]
fn compact_boundaries_reserve_measured_height_without_the_card_minimum() {
    let narrow = Geometry::new(16.0, 300.0);
    let wide = Geometry::new(16.0, 1440.0);

    assert_eq!(boundary_rows_for(14.0, narrow.gap, narrow.module), 2);
    assert!(boundary_rows_for(14.0, wide.gap, wide.module) < MIN_CARD_ROWS);
    assert_eq!(rows_for(14.0, wide.gap, wide.module), MIN_CARD_ROWS);
}

#[test]
fn equivalent_choices_keep_the_smallest_span_then_the_leftmost_anchor() {
    let geometry = Geometry::new(16.0, 1440.0);
    let sets = vec![
        vec![Candidate::synthetic(3, 2), Candidate::synthetic(6, 2)],
        candidates(3, 2),
    ];
    let cards = pack(&sets, TRACK_COUNT, policy());
    assert_eq!((cards[0].track, cards[0].span, cards[0].base), (0, 3, 0));
    assert_eq!((cards[1].track, cards[1].span, cards[1].base), (3, 3, 0));
    assert_eq!(geometry.lane_count, 4);
}

#[test]
fn repeated_base_cards_fill_columns_before_advancing_the_skyline() {
    let sets = vec![candidates(3, 2); 5];
    let cards = pack(&sets, TRACK_COUNT, policy());
    let positions: Vec<_> = cards.iter().map(|card| (card.track, card.base)).collect();
    assert_eq!(positions, vec![(0, 0), (3, 0), (6, 0), (9, 0), (0, 2)]);
}

#[test]
fn equal_height_anchors_choose_the_least_waste_before_the_leftmost_track() {
    let cards = pack(
        &[candidates(2, 2), candidates(6, 3), candidates(6, 2)],
        TRACK_COUNT,
        policy(),
    );
    assert_eq!(
        cards
            .iter()
            .map(|card| (card.track, card.base))
            .collect::<Vec<_>>(),
        [(0, 0), (2, 0), (2, 3)]
    );
}

#[test]
fn a_small_gap_wins_when_it_avoids_moving_the_committed_stack() {
    let committed = vec![card(0, 3, 3, 0, 1), card(1, 6, 6, 0, 4)];
    let before = committed.clone();
    let placed = append(
        &committed,
        &[vec![Candidate::synthetic(3, 5), Candidate::synthetic(6, 3)]],
        TRACK_COUNT,
        PlacementPolicy::new(12),
    );

    assert_eq!(&placed[..2], before);
    assert_eq!((placed[2].track, placed[2].span, placed[2].base), (0, 6, 1));
}

#[test]
fn forward_placement_never_descends_into_an_older_elevation() {
    let geometry = Geometry::new(16.0, 1920.0);
    let candidate_sets = vec![
        prose_candidates(&geometry, [1880.0, 960.0, 660.0, 520.0, 440.0]),
        prose_candidates(&geometry, [6360.0, 3140.0, 2100.0, 1600.0, 1300.0]),
        prose_candidates(&geometry, [200.0, 140.0, 120.0, 120.0, 120.0]),
    ];

    let packed = pack(&candidate_sets, TRACK_COUNT, policy());
    assert_eq!(
        packed
            .iter()
            .map(|card| (card.track, card.span, card.base, card.rows))
            .collect::<Vec<_>>(),
        [(0, 6, 0, 12), (0, 10, 12, 22), (10, 2, 12, 4)]
    );

    let appended = append(&packed[..2], &candidate_sets[2..], TRACK_COUNT, policy());
    assert_eq!(appended[2].base, 12);

    let replayed = full_replay(&packed, TRACK_COUNT, FREEZE_DEPTH, policy());
    assert_eq!(
        replayed
            .windows(2)
            .map(|pair| pair[0].base <= pair[1].base)
            .collect::<Vec<_>>(),
        [true, true]
    );

    let settled = replay_liquid(&packed, TRACK_COUNT, usize::MAX);
    assert_eq!(
        settled.iter().map(|card| card.base).collect::<Vec<_>>(),
        [0, 12, 12]
    );
}

#[test]
fn an_empty_plane_uses_viewport_debt_to_avoid_a_mega_tower() {
    let choices = vec![vec![
        Candidate::synthetic(2, 40),
        Candidate::synthetic(4, 24),
        Candidate::synthetic(6, 18),
        Candidate::synthetic(8, 15),
        Candidate::synthetic(10, 13),
    ]];
    let cards = pack(&choices, TRACK_COUNT, PlacementPolicy::new(12));

    assert_eq!(cards[0].span, 10);
    assert_eq!(cards[0].rows, 13);
}

#[test]
fn normal_spans_stop_one_lane_short_and_single_lane_keeps_its_width() {
    for (width, expected) in [
        (1920.0, vec![2, 4, 6, 8, 10]),
        (1440.0, vec![3, 6, 9]),
        (1000.0, vec![4, 8]),
        (700.0, vec![6]),
        (300.0, vec![12]),
    ] {
        assert_eq!(
            Geometry::new(16.0, width)
                .normal_spans()
                .collect::<Vec<_>>(),
            expected
        );
    }
}

#[test]
fn measured_shape_cost_accepts_square_through_golden_portrait() {
    let geometry = Geometry::new(16.0, 1440.0);
    let span = geometry.base_span;
    let width = f64::from(geometry.card_width(0, span));
    let portrait = candidate_for_height(
        &geometry,
        span,
        width * app::mosaic::MAX_PREFERRED_CARD_HEIGHT_TO_WIDTH,
    );
    let square = candidate_for_height(&geometry, span, width);
    let flat = candidate_for_height(&geometry, span, width / 2.0);
    let tower = candidate_for_height(&geometry, span, width * 2.0);

    assert_eq!((square.shape_cost, portrait.shape_cost), (0, 0));
    assert!(flat.shape_cost > 0);
    assert!(tower.shape_cost > 0);
    assert_eq!(viewport_rows_for(600.0, geometry.gap, geometry.module), 10);
}

#[test]
fn tall_cards_measure_every_admitted_tier_in_bounded_batches() {
    let geometry = Geometry::new(16.0, 1440.0);
    let calls = Cell::new(0);
    let measured = span_candidates(&geometry, |span| {
        calls.set(calls.get() + 1);
        if span == geometry.base_span {
            240.0
        } else {
            180.0
        }
    });
    assert_eq!(calls.get(), geometry.normal_spans().len());
    assert_eq!(
        measured
            .iter()
            .map(|candidate| (candidate.span, candidate.rows))
            .collect::<Vec<_>>(),
        vec![(3, 5), (6, 4)]
    );

    let calls = Cell::new(0);
    let measured = span_candidates(&geometry, |_| {
        calls.set(calls.get() + 1);
        120.0
    });
    assert_eq!(calls.get(), 1);
    assert_eq!(measured.len(), 1);
}

#[test]
fn a_tall_card_whose_width_buys_no_height_keeps_one_base_candidate() {
    let geometry = Geometry::new(16.0, 1440.0);
    let calls = Cell::new(0);
    let letterbox = span_candidates(&geometry, |_| {
        calls.set(calls.get() + 1);
        252.0
    });

    assert_eq!(calls.get(), geometry.normal_spans().len());
    assert_eq!(
        letterbox
            .iter()
            .map(|candidate| (candidate.span, candidate.rows))
            .collect::<Vec<_>>(),
        vec![(3, 5)]
    );

    let rewrapped_header = span_candidates(&geometry, |span| {
        if span == geometry.base_span {
            262.0
        } else {
            255.0
        }
    });

    assert_eq!(
        rewrapped_header
            .iter()
            .map(|candidate| (candidate.span, candidate.rows))
            .collect::<Vec<_>>(),
        vec![(3, 5)]
    );
}

#[test]
fn candidate_policy_depends_on_measured_height_not_card_class() {
    let geometry = Geometry::new(16.0, 1440.0);
    let candidates_for = |_: &str, base_height: f64| {
        let calls = Cell::new(0);
        let candidates = span_candidates(&geometry, |span| {
            calls.set(calls.get() + 1);
            if span == geometry.base_span {
                base_height
            } else {
                180.0
            }
        });
        (candidates, calls.get())
    };

    let agent = candidates_for("agent-message", TALL_THRESHOLD_PX);
    let directive = candidates_for("directive-with-attachment", TALL_THRESHOLD_PX);
    assert_eq!(agent, directive);
    assert_eq!(agent.1, geometry.normal_spans().len());

    let subthreshold = candidates_for("directive", TALL_THRESHOLD_PX - 1.0);
    assert_eq!(subthreshold.1, 1);
    assert_eq!(subthreshold.0.len(), 1);
    assert_eq!(subthreshold.0[0].span, geometry.base_span);
}

#[test]
fn freeze_is_a_latch_driven_by_minimum_burial_depth() {
    let cards = pack(&vec![candidates(12, 2); 3], TRACK_COUNT, policy());
    assert_eq!(burial_depth(&cards[0], &cards), 2);
    assert_eq!(cards[0].lifecycle, Lifecycle::Frozen);
    assert_eq!(cards[1].lifecycle, Lifecycle::Liquid);
    assert_eq!(cards[2].lifecycle, Lifecycle::Liquid);
    assert_eq!(Lifecycle::Liquid.as_str(), "liquid");
    assert_eq!(Lifecycle::Frozen.as_str(), "frozen");
}

#[test]
fn append_rests_on_the_skyline_of_a_backfilled_plane() {
    // Backfill carries older cards above row zero; a card appended after them
    // rests on their skyline rather than leaving a gap down to the origin.
    let backfilled = vec![card(0, 0, 12, -6, 3), card(1, 0, 6, -3, 2)];

    let appended = append(&backfilled, &[candidates(6, 4)], TRACK_COUNT, policy());

    assert_eq!(
        (appended[2].track, appended[2].base),
        (6, -3),
        "an uncovered track rests on the boundary above it"
    );
}

#[test]
fn one_reverse_coverage_pass_matches_strict_later_card_depth() {
    let mut cards = vec![
        card(4, 0, 3, 0, 2),
        card(1, 0, 6, 0, 2),
        card(4, 3, 3, 0, 2),
        card(2, 0, 3, 0, 2),
        card(3, 0, 6, 0, 2),
    ];
    cards[1].lifecycle = Lifecycle::Frozen;
    let expected = cards
        .iter()
        .map(|candidate| {
            candidate.lifecycle == Lifecycle::Frozen || burial_depth(candidate, &cards) >= 2
        })
        .collect::<Vec<_>>();

    let frozen = app::mosaic::recompute_frozen(&cards, 2);

    assert_eq!(
        frozen
            .iter()
            .map(|candidate| candidate.lifecycle == Lifecycle::Frozen)
            .collect::<Vec<_>>(),
        expected,
    );
}

#[test]
fn liquid_replay_moves_only_the_vertical_base_and_ignores_input_order() {
    let mut frozen = card(0, 0, 6, 0, 4);
    frozen.lifecycle = Lifecycle::Frozen;
    let first = card(1, 0, 3, 0, 2);
    let second = card(2, 3, 3, 0, 2);
    let replayed = replay_liquid(
        &[second.clone(), frozen.clone(), first.clone()],
        TRACK_COUNT,
        9,
    );
    let by_index = |index| {
        replayed
            .iter()
            .find(|card| card.creation_index == index)
            .unwrap()
    };
    assert_eq!(
        (by_index(1).track, by_index(1).span, by_index(1).base),
        (0, 3, 4)
    );
    assert_eq!(
        (by_index(2).track, by_index(2).span, by_index(2).base),
        (3, 3, 4)
    );
    assert_eq!(by_index(0), &frozen);
}

#[test]
fn frozen_growth_ripples_transitively_over_the_original_layout() {
    let mut growing = card(0, 0, 3, 4, 2);
    growing.lifecycle = Lifecycle::Frozen;
    let below = card(1, 1, 3, 2, 2);
    let further = card(2, 3, 3, 1, 2);
    let separate = card(3, 9, 3, 0, 2);
    let cards = vec![growing, below, further, separate.clone()];
    let resized = ripple_rows(&cards, 0, 2);
    assert_eq!((resized[0].base, resized[0].rows), (2, 4));
    assert_eq!(resized[1].base, 0);
    assert_eq!(resized[2].base, -1);
    assert_eq!(resized[3], separate);
    assert_eq!(resolve_frozen_resize(&cards, 0, 1), cards);

    let grown = resolve_frozen_resize(&cards, 0, 5);
    assert_eq!((grown[0].base, grown[0].rows), (1, 5));
    assert_eq!(grown[1].base, -1);
    assert_eq!(grown[2].base, -2);
    assert_eq!(grown[3], separate);
}

#[test]
#[should_panic(expected = "Mosaic creation index 99 is absent from the card set")]
fn a_ripple_for_an_unknown_creation_index_fails_loudly() {
    let _ = ripple_rows(&[card(0, 0, 3, 0, 2)], 99, 1);
}

#[test]
#[should_panic(expected = "Mosaic creation index 99 is absent from the card set")]
fn a_frozen_resize_for_an_unknown_creation_index_fails_loudly() {
    let _ = resolve_frozen_resize(&[card(0, 0, 3, 0, 2)], 99, 3);
}

#[test]
fn full_replay_depends_on_creation_order_and_current_candidates_only() {
    let mut cards = pack(&vec![candidates(3, 2); 5], TRACK_COUNT, policy());
    cards.reverse();
    for card in &mut cards {
        card.track = 9;
        card.base = -30;
        card.lifecycle = Lifecycle::Frozen;
    }
    let replayed = full_replay(&cards, TRACK_COUNT, FREEZE_DEPTH, policy());
    let replayed_again = full_replay(&replayed, TRACK_COUNT, FREEZE_DEPTH, policy());
    assert_eq!(replayed, replayed_again);
    assert!(
        replayed
            .iter()
            .all(|card| card.lifecycle == Lifecycle::Liquid),
        "full replay clears every stale freeze latch before recomputing burial"
    );
    let positions: Vec<_> = replayed
        .iter()
        .map(|card| (card.creation_index, card.track, card.base))
        .collect();
    assert_eq!(
        positions,
        vec![(4, 0, 2), (3, 9, 0), (2, 6, 0), (1, 3, 0), (0, 0, 0)]
    );
}

#[test]
fn extent_spans_every_card_and_scroll_preservation_starts_past_the_threshold() {
    let cards = vec![card(0, 0, 3, -1, 3), card(1, 3, 3, 2, 4)];
    assert_eq!(extent(&cards), (6, -1));
    assert!(!pane_is_scrolled(80.0, None));
    assert!(pane_is_scrolled(80.1, None));
}

#[test]
fn history_paging_admits_the_trailing_scroll_edge_without_a_cursor_card() {
    assert!(!history_edge_is_near(199.0, 400.0, 1_000.0, None));
    assert!(history_edge_is_near(200.0, 400.0, 1_000.0, None));
    assert!(history_edge_is_near(0.0, 400.0, 400.0, None));
}
