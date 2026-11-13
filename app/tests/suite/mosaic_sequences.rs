//! Composition checks over a small matrix of current Mosaic candidate shapes.
//!
//! Unit tests in `mosaic.rs` pin individual placement rules. These sequences
//! exercise those rules together without treating an external generator,
//! seed list, or recorded layout as authority. The inputs are ordinary typed
//! candidates accepted by the current Rust lattice.

use app::mosaic::{self, Candidate, Card, PlacementPolicy, TRACK_COUNT};

const BASE_SPANS: [usize; 5] = [2, 3, 4, 6, 12];

fn short(span: usize, rows: i32) -> Vec<Candidate> {
    vec![Candidate::synthetic(span, rows)]
}

fn tall(span: usize, rows: i32, wide_rows: i32) -> Vec<Candidate> {
    let wide = (span * 2).min(TRACK_COUNT);
    let mut candidates = short(span, rows);
    if wide > span {
        candidates.push(Candidate::synthetic(wide, wide_rows));
    }
    candidates
}

fn current_contract_sequences() -> Vec<(&'static str, Vec<Vec<Candidate>>)> {
    let ascending = BASE_SPANS
        .into_iter()
        .flat_map(|span| [short(span, 2), tall(span, 5, 3)])
        .collect();
    let descending = BASE_SPANS
        .into_iter()
        .rev()
        .flat_map(|span| [tall(span, 6, 4), short(span, 3)])
        .collect();
    let interleaved = [
        short(3, 2),
        tall(2, 6, 4),
        short(6, 3),
        tall(4, 5, 3),
        short(2, 1),
        short(12, 4),
        tall(3, 6, 4),
        short(4, 2),
        tall(6, 7, 5),
        short(2, 3),
    ]
    .into_iter()
    .collect();
    vec![
        ("ascending spans", ascending),
        ("descending spans", descending),
        ("interleaved spans", interleaved),
    ]
}

fn cells_overlap(left: &Card, right: &Card) -> bool {
    let tracks = left.track < right.track + right.span && right.track < left.track + left.span;
    let rows = left.base < right.base + right.rows && right.base < left.base + left.rows;
    tracks && rows
}

#[test]
fn no_two_cards_ever_occupy_the_same_cell() {
    for (name, candidates) in current_contract_sequences() {
        let cards = mosaic::pack(&candidates, TRACK_COUNT, PlacementPolicy::new(12));
        for (index, card) in cards.iter().enumerate() {
            for (other_index, other) in cards.iter().enumerate().skip(index + 1) {
                assert!(
                    !cells_overlap(card, other),
                    "cards {index} and {other_index} overlap in {name}: {card:?} {other:?}"
                );
            }
        }
    }
}

#[test]
fn a_run_places_every_card_it_was_given() {
    for (name, candidates) in current_contract_sequences() {
        let cards = mosaic::pack(&candidates, TRACK_COUNT, PlacementPolicy::new(12));
        assert_eq!(cards.len(), candidates.len(), "{name} lost a card");
        for (index, card) in cards.iter().enumerate() {
            assert_eq!(card.creation_index, index, "{name} reordered cards");
        }
    }
}

#[test]
fn a_later_card_never_moves_the_horizontal_placement_of_an_earlier_one() {
    for (name, candidates) in current_contract_sequences() {
        let complete = mosaic::pack(&candidates, TRACK_COUNT, PlacementPolicy::new(12));
        for prefix_len in 1..candidates.len() {
            let prefix = mosaic::pack(
                &candidates[..prefix_len],
                TRACK_COUNT,
                PlacementPolicy::new(12),
            );
            for (index, card) in prefix.iter().enumerate() {
                let later = &complete[index];
                assert_eq!(
                    (card.track, card.span),
                    (later.track, later.span),
                    "{name}: card {index} moved horizontally after {prefix_len} cards"
                );
            }
        }
    }
}
