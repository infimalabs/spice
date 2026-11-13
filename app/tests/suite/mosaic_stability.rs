use std::collections::{BTreeMap, BTreeSet};

use app::mosaic::{Candidate, Lifecycle};
use app::mosaic_plane::{MeasuredCard, PersistentLanePlane, PlaneGeometry, Transition};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PlacementTuple {
    track: usize,
    base_row: i32,
    span: usize,
    extent: i32,
    lifecycle: Lifecycle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlacementSnapshot {
    cards: BTreeMap<&'static str, PlacementTuple>,
    max_row: i32,
    rows: i32,
}

#[derive(Clone, Copy)]
struct ExpectedMovement {
    key: &'static str,
    placement: PlacementTuple,
}

struct MovementDeclaration {
    targets: &'static [ExpectedMovement],
    cause: &'static str,
}

fn geometry() -> PlaneGeometry {
    PlaneGeometry::new(900, 12)
}

fn measured(key: &'static str, span: usize, extent: i32) -> MeasuredCard<&'static str> {
    MeasuredCard {
        key,
        candidates: vec![Candidate::synthetic(span, extent)],
        latched: None,
        content_revision: 0,
        boundary: false,
    }
}

fn boundary(key: &'static str) -> MeasuredCard<&'static str> {
    MeasuredCard {
        boundary: true,
        ..measured(key, 12, 1)
    }
}

fn placements(
    plane: &PersistentLanePlane<&'static str>,
    selected: &[&'static str],
) -> PlacementSnapshot {
    let selected = selected.iter().copied().collect::<BTreeSet<_>>();
    let cards = plane
        .cards()
        .iter()
        .filter(|entry| selected.is_empty() || selected.contains(entry.key))
        .map(|entry| {
            (
                entry.key,
                PlacementTuple {
                    track: entry.card.track,
                    base_row: entry.card.base,
                    span: entry.card.span,
                    extent: entry.card.rows,
                    lifecycle: entry.card.lifecycle,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    PlacementSnapshot {
        cards,
        max_row: plane.max_row(),
        rows: plane.rows(),
    }
}

fn assert_stable(
    before: &PlacementSnapshot,
    after: &PlacementSnapshot,
    added: &[&'static str],
    label: &str,
) {
    let additions = added.iter().copied().collect::<BTreeSet<_>>();
    for (key, prior) in &before.cards {
        let Some(current) = after.cards.get(key) else {
            panic!("{label}: card {key:?} disappeared; before={prior:?}");
        };
        assert_eq!(
            current, prior,
            "{label}: card {key:?} moved unexpectedly; before={prior:?} after={current:?}",
        );
    }
    for (key, current) in &after.cards {
        assert!(
            before.cards.contains_key(key) || additions.contains(key),
            "{label}: undeclared card {key:?} appeared; after={current:?}",
        );
    }
    for key in additions {
        assert!(
            after.cards.contains_key(key),
            "{label}: addition {key:?} did not appear"
        );
    }
    if added.is_empty() {
        assert_eq!(
            (after.max_row, after.rows),
            (before.max_row, before.rows),
            "{label}: plane extent changed without a declared addition"
        );
    }
}

fn assert_declared_movement(
    before: &PlacementSnapshot,
    after: &PlacementSnapshot,
    declaration: &MovementDeclaration,
) {
    let targets = declaration
        .targets
        .iter()
        .map(|target| (target.key, target.placement))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        before.cards.keys().collect::<Vec<_>>(),
        after.cards.keys().collect::<Vec<_>>()
    );
    for (key, prior) in &before.cards {
        let current = &after.cards[key];
        if let Some(expected) = targets.get(key) {
            assert_eq!(
                current, expected,
                "target {key:?} moved to an unexpected placement for {}; expected={expected:?} actual={current:?}",
                declaration.cause,
            );
        } else {
            assert_eq!(
                current, prior,
                "card {key:?} moved unexpectedly for {}; before={prior:?} after={current:?}",
                declaration.cause,
            );
        }
    }
    assert_ne!(
        (after.max_row, after.rows),
        (before.max_row, before.rows),
        "declared card movement also changes plane extent for {}",
        declaration.cause,
    );
}

#[test]
fn noops_content_only_updates_and_sibling_activity_preserve_committed_tuples() {
    let initial = [
        measured("a", 12, 2),
        measured("b", 12, 2),
        measured("c", 12, 2),
        measured("d", 12, 2),
    ];
    let plane = PersistentLanePlane::new(geometry(), &initial);
    let baseline = placements(&plane, &[]);

    let noop = plane.reconcile(geometry(), &initial);
    assert_stable(&baseline, &placements(&noop, &[]), &[], "a no-op render");

    let revised = noop.reconcile(
        geometry(),
        &[
            initial[0].clone(),
            initial[1].clone(),
            initial[2].clone(),
            MeasuredCard {
                content_revision: 1,
                ..initial[3].clone()
            },
        ],
    );
    assert_eq!(revised.transition(), Transition::ContentResize);
    assert_stable(
        &baseline,
        &placements(&revised, &[]),
        &[],
        "a content-only revision with unchanged extent",
    );

    let sibling = measured("sibling-agent-message", 6, 3);
    let appended = revised.reconcile(
        geometry(),
        &[
            initial[0].clone(),
            initial[1].clone(),
            initial[2].clone(),
            MeasuredCard {
                content_revision: 1,
                ..initial[3].clone()
            },
            sibling,
        ],
    );
    let before_append = placements(&revised, &[]);
    let after_append = placements(&appended, &[]);
    assert!(
        after_append.rows > before_append.rows,
        "the declared sibling addition grows the plane extent"
    );
    assert_stable(
        &before_append,
        &after_append,
        &["sibling-agent-message"],
        "activity from a sibling agent",
    );
}

#[test]
fn pagination_outside_an_epoch_preserves_the_selected_committed_tuples() {
    let initial = [
        measured("old", 12, 2),
        boundary("compact-a"),
        measured("prior-epoch", 12, 4),
        boundary("compact-b"),
        measured("recent", 6, 3),
    ];
    let plane = PersistentLanePlane::new(geometry(), &initial);
    // Older history rests against the retained boundary, so only the boundary
    // and the newer side keep their committed tuples.
    let outside = ["compact-b", "recent"];
    let before = placements(&plane, &outside);
    let replaced = plane.reconcile(
        geometry(),
        &[
            initial[0].clone(),
            initial[1].clone(),
            measured("epoch-left", 6, 2),
            measured("epoch-right", 6, 2),
            initial[3].clone(),
            initial[4].clone(),
        ],
    );
    assert_eq!(replaced.transition(), Transition::EpochReplay);
    assert_stable(
        &before,
        &placements(&replaced, &outside),
        &[],
        "an epoch replay beneath its retained boundary",
    );

    let first_page = replaced.reconcile(
        geometry(),
        &[
            measured("older", 12, 2),
            initial[0].clone(),
            initial[1].clone(),
            measured("epoch-left", 6, 2),
            measured("epoch-right", 6, 2),
            initial[3].clone(),
            initial[4].clone(),
        ],
    );
    let committed = placements(&first_page, &[]);
    let second_page = first_page.reconcile(
        geometry(),
        &[
            measured("oldest", 3, 1),
            measured("older", 12, 2),
            initial[0].clone(),
            initial[1].clone(),
            measured("epoch-left", 6, 2),
            measured("epoch-right", 6, 2),
            initial[3].clone(),
            initial[4].clone(),
        ],
    );
    let paged = placements(&second_page, &[]);
    assert_eq!(
        paged.max_row, committed.max_row,
        "backfill grows below the committed maximum row"
    );
    assert!(
        paged.rows > committed.rows,
        "the declared backfill addition grows the plane extent"
    );
    assert_stable(
        &committed,
        &paged,
        &["oldest"],
        "pagination before the affected epoch",
    );
}

#[test]
fn required_growth_declares_every_moving_card_and_preserves_the_rest() {
    let initial = [
        measured("a", 12, 2),
        measured("b", 12, 2),
        measured("c", 12, 2),
        measured("d", 12, 2),
    ];
    let plane = PersistentLanePlane::new(geometry(), &initial);
    let before = placements(&plane, &[]);
    let movement = MovementDeclaration {
        targets: &[
            ExpectedMovement {
                key: "a",
                placement: PlacementTuple {
                    track: 0,
                    base_row: -2,
                    span: 12,
                    extent: 2,
                    lifecycle: Lifecycle::Frozen,
                },
            },
            ExpectedMovement {
                key: "b",
                placement: PlacementTuple {
                    track: 0,
                    base_row: 0,
                    span: 12,
                    extent: 4,
                    lifecycle: Lifecycle::Frozen,
                },
            },
        ],
        cause: "frozen card b grows downward through overlapping card a",
    };
    let grown = plane.reconcile(
        geometry(),
        &[
            initial[0].clone(),
            measured("b", 12, 4),
            initial[2].clone(),
            initial[3].clone(),
        ],
    );
    let after = placements(&grown, &[]);
    assert_declared_movement(&before, &after, &movement);
}
