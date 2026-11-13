//! Crystallized image runs: a mosaic-only presentation of close image cards.
//!
//! An agent that captures several images in quick succession publishes one
//! single-image tool card per capture. In the mosaic such a run reads as a
//! stack of near-identical cards, so up to [`MAX_RUN`] of them, each at most
//! [`RUN_WINDOW_MILLIS`] after its predecessor and from the same speaker,
//! present as one card at the run's first place. Every other surface still
//! sees one card per image; this module only decides grouping and geometry.
//!
//! The crystal keeps one image card's footprint and partitions it into one
//! cell per image. The partition is a guillotine split that keeps capture
//! order, sizes each cell from the images' natural aspect ratios, and is the
//! candidate that crops the images least once stretched to the card's box.

use wire::{AgentActivityKind, AgentMessage};

/// Longest gap between consecutive captures that still joins one run.
pub(crate) const RUN_WINDOW_MILLIS: i64 = 2_000;
/// Most images one crystal holds; the next close image starts a new run.
pub(crate) const MAX_RUN: usize = 4;

/// Whether `message` is a single-image tool card, the only card a run joins.
pub(crate) fn crystallizable(message: &AgentMessage) -> bool {
    message.activity == AgentActivityKind::Tool && message.media.len() == 1
}

/// Partition chronologically ascending cards into runs, as run lengths that
/// sum to `cards.len()`. `None` marks a card that never joins a run.
///
/// Grouping is greedy from the oldest card, so a newer card can only extend
/// or follow the newest run; it never regroups older ones.
pub(crate) fn run_lengths(cards: &[Option<&AgentMessage>]) -> Vec<usize> {
    let mut lengths = Vec::new();
    let mut index = 0;
    while index < cards.len() {
        let mut length = 1;
        if let Some(mut previous) = cards[index] {
            while length < MAX_RUN
                && let Some(Some(next)) = cards.get(index + length)
                && joins(previous, next)
            {
                previous = next;
                length += 1;
            }
        }
        lengths.push(length);
        index += length;
    }
    lengths
}

fn joins(previous: &AgentMessage, next: &AgentMessage) -> bool {
    previous.lane == next.lane
        && previous.author == next.author
        && (0..=RUN_WINDOW_MILLIS).contains(&(next.display_millis() - previous.display_millis()))
}

/// One image's cell, as fractions of the crystal's box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Cell {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

/// A guillotine partition over a contiguous range of images.
#[derive(Clone, Debug)]
enum Node {
    Leaf(usize),
    /// Children side by side (`true`) or stacked (`false`).
    Split(bool, Vec<Node>),
}

/// The cells for images of the given natural aspect ratios (width / height)
/// inside a box of aspect `frame`, in image order.
///
/// Each candidate partition sizes its cells as if every image kept its own
/// aspect, then stretches to the box. The chosen candidate minimizes total
/// squared distortion: each cell's log distance from its image's aspect (how
/// much cover fill crops the image) and from an equal share of the box (so no
/// image shrinks to a sliver to spare another's crop). Squaring makes the
/// worst-served image dominate, so no single image is badly cropped to fit the
/// rest. Unknown or degenerate ratios read as square. Ties keep the first
/// candidate in a fixed enumeration order, so equal inputs always yield the
/// same cells.
pub(crate) fn arrange(aspects: &[f64], frame: f64) -> Vec<Cell> {
    let aspects: Vec<f64> = aspects.iter().copied().map(usable).collect();
    let frame = usable(frame);
    if aspects.is_empty() {
        return Vec::new();
    }
    let mut best: Option<(f64, Vec<Cell>)> = None;
    for orientation in [false, true] {
        for node in partitions(0, aspects.len(), orientation) {
            for fit in FITS {
                let mut cells = vec![
                    Cell {
                        x: 0.0,
                        y: 0.0,
                        width: 0.0,
                        height: 0.0,
                    };
                    aspects.len()
                ];
                place(&node, &aspects, fit, (0.0, 0.0, frame, 1.0), &mut cells);
                let share = aspects.len() as f64 / frame;
                let distortion: f64 = cells
                    .iter()
                    .zip(&aspects)
                    .map(|(cell, aspect)| {
                        (cell.width / cell.height / aspect).ln().powi(2)
                            + (cell.width * cell.height * share).ln().powi(2)
                    })
                    .sum();
                if best
                    .as_ref()
                    .is_none_or(|(least, _)| distortion < *least - 1e-9)
                {
                    for cell in &mut cells {
                        cell.x /= frame;
                        cell.width /= frame;
                    }
                    best = Some((distortion, cells));
                }
            }
        }
    }
    best.map(|(_, cells)| cells).unwrap_or_default()
}

fn usable(aspect: f64) -> f64 {
    if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        1.0
    }
}

/// Every guillotine partition of `start..end` whose top split runs in
/// `across` (side by side when true); nested splits alternate orientation.
fn partitions(start: usize, end: usize, across: bool) -> Vec<Node> {
    if end - start == 1 {
        return vec![Node::Leaf(start)];
    }
    let mut nodes = Vec::new();
    for bounds in compositions(start, end) {
        let options: Vec<Vec<Node>> = bounds
            .windows(2)
            .map(|range| {
                if range[1] - range[0] == 1 {
                    vec![Node::Leaf(range[0])]
                } else {
                    partitions(range[0], range[1], !across)
                }
            })
            .collect();
        for children in product(&options) {
            nodes.push(Node::Split(across, children));
        }
    }
    nodes
}

/// Every split of `start..end` into two or more contiguous parts, as their
/// boundaries.
fn compositions(start: usize, end: usize) -> Vec<Vec<usize>> {
    let inner = end - start - 1;
    (1..(1_u32 << inner))
        .map(|mask| {
            let mut bounds = vec![start];
            bounds.extend(
                (0..inner)
                    .filter(|bit| mask & (1 << bit) != 0)
                    .map(|bit| start + bit + 1),
            );
            bounds.push(end);
            bounds
        })
        .collect()
}

fn product(options: &[Vec<Node>]) -> Vec<Vec<Node>> {
    options.iter().fold(vec![Vec::new()], |prefixes, choices| {
        prefixes
            .iter()
            .flat_map(|prefix| {
                choices.iter().map(move |choice| {
                    let mut next = prefix.clone();
                    next.push(choice.clone());
                    next
                })
            })
            .collect()
    })
}

/// How far each split sizes its parts by fit rather than by count: a part's
/// weight is `natural^fit * leaves^(1 - fit)`, so `1` sizes every image to its
/// own aspect, `0` gives every image an equal share, and `0.5` meets halfway.
const FITS: [f64; 3] = [1.0, 0.5, 0.0];

fn leaves(node: &Node) -> usize {
    match node {
        Node::Leaf(_) => 1,
        Node::Split(_, children) => children.iter().map(leaves).sum(),
    }
}

/// The aspect a node takes when every image keeps its own aspect.
fn natural(node: &Node, aspects: &[f64]) -> f64 {
    match node {
        Node::Leaf(index) => aspects[*index],
        Node::Split(true, children) => children.iter().map(|child| natural(child, aspects)).sum(),
        Node::Split(false, children) => {
            1.0 / children
                .iter()
                .map(|child| 1.0 / natural(child, aspects))
                .sum::<f64>()
        }
    }
}

fn place(
    node: &Node,
    aspects: &[f64],
    fit: f64,
    (x, y, width, height): (f64, f64, f64, f64),
    cells: &mut [Cell],
) {
    match node {
        Node::Leaf(index) => {
            cells[*index] = Cell {
                x,
                y,
                width,
                height,
            };
        }
        Node::Split(across, children) => {
            let weights: Vec<f64> = children
                .iter()
                .map(|child| {
                    let aspect = natural(child, aspects);
                    let natural = if *across { aspect } else { 1.0 / aspect };
                    natural.powf(fit) * (leaves(child) as f64).powf(1.0 - fit)
                })
                .collect();
            let total: f64 = weights.iter().sum();
            let mut offset = 0.0;
            for (child, weight) in children.iter().zip(weights) {
                let share = weight / total;
                if *across {
                    place(
                        child,
                        aspects,
                        fit,
                        (x + offset * width, y, share * width, height),
                        cells,
                    );
                } else {
                    place(
                        child,
                        aspects,
                        fit,
                        (x, y + offset * height, width, share * height),
                        cells,
                    );
                }
                offset += share;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Cell, MAX_RUN, arrange, crystallizable, run_lengths};
    use wire::{AgentActivityKind, AgentMessage, Media};

    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
    const LANE: &str = "018f82d4-6f59-7d06-92e3-bce9131163e5";
    const AUTHOR: &str = "018f82d4-6f59-7d06-92e3-bce9131163ef";
    const OTHER: &str = "018f82d4-6f59-7d06-92e3-bce913116300";

    fn card(author: &str, millis: i64, activity: AgentActivityKind, images: usize) -> AgentMessage {
        let mut message: AgentMessage = serde_json::from_value(serde_json::json!({
            "lane": LANE,
            "author": author,
            "locus": {"member": 1, "offset": millis, "length": 1, "event": 0},
            "authored": format!("2030-04-12T18:44:{:02}.{:03}Z", millis / 1000, millis % 1000),
            "text": "",
        }))
        .expect("fixture message");
        message.activity = activity;
        message.media = vec![
            Media::from_base64("capture.png", "image/png", PNG)
                .expect("fixture media");
            images
        ];
        message
    }

    fn image(millis: i64) -> AgentMessage {
        card(AUTHOR, millis, AgentActivityKind::Tool, 1)
    }

    fn lengths(cards: &[AgentMessage]) -> Vec<usize> {
        let eligible: Vec<_> = cards
            .iter()
            .map(|card| crystallizable(card).then_some(card))
            .collect();
        run_lengths(&eligible)
    }

    #[test]
    fn close_images_join_until_a_gap_exceeds_two_seconds() {
        let cards = [image(0), image(2_000), image(4_001), image(5_000)];
        assert_eq!(lengths(&cards), [2, 2]);
        assert_eq!(lengths(&[image(0)]), [1]);
    }

    #[test]
    fn a_run_holds_at_most_four_images() {
        let cards: Vec<_> = (0..9).map(|n| image(n * 500)).collect();
        assert_eq!(lengths(&cards), [MAX_RUN, MAX_RUN, 1]);
    }

    #[test]
    fn another_speaker_or_card_kind_breaks_a_run() {
        let cards = [
            image(0),
            card(OTHER, 100, AgentActivityKind::Tool, 1),
            image(200),
            card(AUTHOR, 300, AgentActivityKind::Prose, 1),
            image(400),
            card(AUTHOR, 500, AgentActivityKind::Tool, 2),
            image(600),
            image(700),
        ];
        assert_eq!(lengths(&cards), [1, 1, 1, 1, 1, 1, 2]);
    }

    #[test]
    fn a_newer_image_extends_only_the_newest_run() {
        let mut cards: Vec<_> = (0..5).map(|n| image(n * 100)).collect();
        assert_eq!(lengths(&cards), [4, 1]);
        cards.push(image(600));
        assert_eq!(lengths(&cards), [4, 2]);
    }

    fn covers_frame(cells: &[Cell]) {
        let area: f64 = cells.iter().map(|cell| cell.width * cell.height).sum();
        assert!((area - 1.0).abs() < 1e-9, "cells cover {area} of the frame");
        for cell in cells {
            assert!(cell.x >= -1e-9 && cell.y >= -1e-9);
            assert!(cell.x + cell.width <= 1.0 + 1e-9 && cell.y + cell.height <= 1.0 + 1e-9);
        }
    }

    #[test]
    fn wide_images_stack_and_tall_images_stand_side_by_side() {
        let wide = arrange(&[2.0, 2.0], 1.0);
        covers_frame(&wide);
        assert!(wide.iter().all(|cell| cell.width > cell.height), "{wide:?}");
        let tall = arrange(&[0.5, 0.5], 1.0);
        covers_frame(&tall);
        assert!(tall.iter().all(|cell| cell.height > cell.width), "{tall:?}");
    }

    #[test]
    fn mixed_images_give_each_its_own_orientation() {
        let cells = arrange(&[0.5, 2.0, 2.0], 1.0);
        covers_frame(&cells);
        assert!(cells[0].height > cells[0].width, "{cells:?}");
        assert!(cells[1].width > cells[1].height, "{cells:?}");
        assert!(cells[2].width > cells[2].height, "{cells:?}");
    }

    #[test]
    fn four_square_images_form_a_grid_in_a_square_box() {
        let cells = arrange(&[1.0; 4], 1.0);
        covers_frame(&cells);
        for cell in &cells {
            assert!(
                (cell.width - 0.5).abs() < 1e-9 && (cell.height - 0.5).abs() < 1e-9,
                "{cells:?}"
            );
        }
    }

    #[test]
    fn no_image_shrinks_to_a_sliver() {
        for aspects in [
            [1.6, 1.6, 0.53, 0.53],
            [1.44, 0.46, 0.46, 0.46],
            [4.5, 4.5, 0.25, 1.0],
        ] {
            let cells = arrange(&aspects, 0.96);
            covers_frame(&cells);
            for cell in &cells {
                assert!(
                    cell.width * cell.height >= 1.0 / 8.0,
                    "{aspects:?} gave {cells:?}"
                );
            }
        }
    }

    #[test]
    fn unknown_ratios_read_as_square_and_the_choice_is_stable() {
        let cells = arrange(&[f64::NAN, 0.0, 1.0], 1.0);
        covers_frame(&cells);
        assert_eq!(cells, arrange(&[1.0, 1.0, 1.0], 1.0));
        assert_eq!(
            arrange(&[1.6, 0.7, 1.2, 0.9], 1.3),
            arrange(&[1.6, 0.7, 1.2, 0.9], 1.3)
        );
    }
}
