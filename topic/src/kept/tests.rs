use std::collections::HashSet;

use super::{Fitted, Node, Rolling, heaviest};
use crate::CAPACITY;
use crate::evidence::{Prepared, prepare_fields};

const SUBJECTS: [[&str; 4]; 3] = [
    [
        "Store the release signing key in the hardware vault",
        "Verify release signatures before the installer runs",
        "Publish the public half of the release signing key",
        "Record which signing key signed each release",
    ],
    [
        "Migrate the orders table to the partitioned schema",
        "Backfill the partitioned orders schema from the old table",
        "Add a rollback step to the orders schema migration",
        "Run the orders migration in batches to limit table locks",
    ],
    [
        "Let the dashboard switch between light and dark theme",
        "Persist the chosen dashboard theme per operator",
        "Fix contrast of chart labels in the dark dashboard theme",
        "Follow the system setting for the dashboard theme by default",
    ],
];

/// Whether this set's tasks ascend without repeats, its groups divide
/// exactly those tasks between them, and every ID in its tree is one
/// `last` covers that is not yet in `seen`.
fn divided(node: &Node, last: usize, seen: &mut HashSet<usize>) -> bool {
    if node.id > last
        || !seen.insert(node.id)
        || !node.members.is_sorted_by(|left, right| left < right)
    {
        return false;
    }
    if node.groups.is_empty() {
        return true;
    }
    let mut members: Vec<usize> = (node.groups.iter())
        .flat_map(|group| group.members.iter().copied())
        .collect();
    members.sort_unstable();
    members == node.members
        && (node.groups.iter()).all(|group| !group.members.is_empty() && divided(group, last, seen))
}

/// Whether the next arrival can extend this tree: growth indexes rows by
/// the positions the tree holds and numbers new groups after the last ID
/// given out.
fn sound(rolling: &Rolling) -> bool {
    let tree = &rolling.tree;
    let held = rolling.rows.len();
    held == tree.arrived.min(CAPACITY)
        && (tree.root.members.iter().copied()).eq(tree.arrived - held..tree.arrived)
        && (!tree.unopened || tree.root.groups.is_empty())
        && divided(&tree.root, tree.next, &mut HashSet::new())
}

/// Arrival `n` is on subject `n % 3`.
#[test]
fn interleaved_subjects_group_apart_and_every_arrival_leaves_a_sound_tree() {
    let mut tree = Rolling::default();
    for round in 0..4 {
        for (subject, titles) in SUBJECTS.iter().enumerate() {
            let title = titles[round].to_owned();
            let id = format!("task{round}{subject}");
            tree.push(prepare_fields(
                &id,
                [std::slice::from_ref(&title), &[], &[], &[]],
            ));
            assert!(sound(&tree), "growth left an unsound tree at {id}");
        }
    }
    let held = tree.held();
    assert!(held.len() > 1, "the subjects formed no groups");
    for (path, members) in held.iter().filter(|(path, _)| !path.is_empty()) {
        assert!(
            members.iter().all(|task| task % 3 == members[0] % 3),
            "group {path:?} mixes subjects: {members:?}"
        );
    }
}

#[test]
fn nested_direct_seats_preserve_arrival_order_across_singleton_groups() {
    let node = |id, members, groups| Node {
        id,
        members,
        groups,
        stale: false,
    };
    let tree = super::Kept {
        root: node(
            1,
            vec![0, 1, 2, 3, 4, 5],
            vec![
                node(
                    2,
                    vec![0, 2, 3, 4, 5],
                    vec![
                        node(3, vec![5], vec![]),
                        node(4, vec![2, 3], vec![]),
                        node(5, vec![0], vec![]),
                        node(6, vec![4], vec![]),
                    ],
                ),
                node(7, vec![1], vec![]),
            ],
        ),
        arrived: 6,
        next: 7,
        unopened: false,
    };
    let seats = tree.held();
    assert_eq!(seats[&vec![2]], [0, 4, 5]);
    assert_eq!(seats[&vec![2, 4]], [2, 3]);
}

#[test]
fn a_forecast_evicts_the_oldest_statement_but_not_a_fixed_row() {
    let row = |id: usize| {
        let title = String::from("Inspect the manifest");
        prepare_fields(
            &id.to_string(),
            [std::slice::from_ref(&title), &[], &[], &[]],
        )
    };
    let mut tree = Rolling::with_fixed([row(0), row(1)]);
    for id in 2..CAPACITY + 4 {
        tree.push(row(id));
    }
    let next = tree.arrived();
    let forecast = tree.forecast(&row(next));
    let mut positions: Vec<_> = forecast.values().flatten().copied().collect();
    positions.sort_unstable();
    assert!(
        positions
            .iter()
            .copied()
            .eq([0, 1].into_iter().chain(next + 1 - CAPACITY..=next))
    );
    for &position in &positions[..positions.len() - 1] {
        assert_eq!(
            tree.rows()[tree.row_index(position)].id,
            position.to_string()
        );
    }
    let arrival = row(next);
    let names = tree.seat_words(&arrival, &forecast);
    let mut after = tree.clone();
    after.push(arrival.clone());
    for depth in names {
        assert_eq!(depth.words, ranked_held(&after, &depth.members, &positions));
    }
    assert_eq!(after.held(), forecast);
}

/// Likeness is read in the fit, not counted in shared words. The arrival's
/// description restates an orders statement, more distinct words than its
/// title shares with any signing statement, and the fit weighs a title
/// above a description.
#[test]
fn likeness_follows_the_fit_where_shared_word_counts_point_elsewhere() {
    let mut tree = Rolling::default();
    for round in 0..4 {
        for (subject, titles) in SUBJECTS.iter().enumerate() {
            let title = titles[round].to_owned();
            tree.push(prepare_fields(
                &format!("task{round}{subject}"),
                [std::slice::from_ref(&title), &[], &[], &[]],
            ));
        }
    }
    let arrival = |title: &str, description: &str| {
        prepare_fields(
            "arrival",
            [&[title.to_owned()], &[], &[description.to_owned()], &[]],
        )
    };
    let signing = arrival(
        "Store the release signing key",
        "Migrate the orders table to the partitioned schema",
    );
    let forecast = tree.forecast(&signing);
    let (path, seat) = (forecast.iter())
        .find(|(path, members)| !path.is_empty() && members.contains(&tree.arrived()))
        .map(|(path, members)| (path, members.as_slice()))
        .expect("the arrival joined a group");
    assert!(
        seat.iter()
            .filter(|&&at| at != tree.arrived())
            .all(|at| at % 3 == 0),
        "seated off its subject"
    );
    let shared = |subject: usize| {
        (tree.rows().iter().enumerate())
            .filter(|(at, _)| at % 3 == subject)
            .map(|(_, held)| {
                (held.admitted.keys())
                    .filter(|&word| signing.admitted.contains_key(word))
                    .count()
            })
            .max()
    };
    assert!(shared(1) > shared(0), "the description shares too little");
    let (mut inside, mut outside) = (0_f64, 0_f64);
    for (at, score) in tree.likeness(&signing, &forecast, path, |_| true) {
        let best = if seat.contains(&at) {
            &mut inside
        } else {
            &mut outside
        };
        *best = best.max(score);
    }
    assert!(inside > outside);
}

#[test]
fn nested_seats_compare_rivals_only_in_their_own_parent_fit() {
    // Maple is common inside this parent; plum is common outside it. The
    // arriving statement matches one of each, so the fit scope decides
    // whether its plum seat clears a ten-point lead over its maple rivals.
    let prepare = |at: usize, text: &str| {
        prepare_fields(&format!("task{at}"), [&[text.into()], &[], &[], &[]])
    };
    let mut tree = Rolling::default();
    for (at, text) in [
        "plum", "maple", "maple", "maple", "plum", "plum", "plum", "plum",
    ]
    .into_iter()
    .enumerate()
    {
        tree.push(prepare(at, text));
    }
    let at = tree.arrived();
    let row = prepare(at, "plum maple");
    let path = vec![1, 2, 3];
    let partition = std::collections::BTreeMap::from([
        (path.clone(), vec![0, at]),
        (vec![1, 2], vec![1]),
        (vec![1, 2, 4], vec![2, 3]),
        (vec![1, 5], vec![4, 5, 6, 7]),
    ]);
    let mut scores = tree
        .likeness(&row, &partition, &path, |_| true)
        .collect::<Vec<_>>();
    scores.sort_by_key(|&(at, _)| at);
    let [(0, inside), (1, parent), (2, sibling), (3, other)] = scores.as_slice() else {
        panic!("only the other rows within the immediate parent compete: {scores:?}")
    };
    assert_eq!(parent, sibling);
    assert_eq!(sibling, other);
    assert!(
        inside - sibling >= 0.10,
        "plum carries the greater local weight"
    );
}

#[test]
fn a_seat_three_groups_deep_is_named_at_every_level_within_the_group_above() {
    // Storage sets one first-level group apart, disk one group inside it,
    // and latency the seat inside that.
    let prepare = |at: usize, text: &str| {
        prepare_fields(&format!("task{at}"), [&[text.into()], &[], &[], &[]])
    };
    let mut tree = Rolling::default();
    for (at, text) in [
        "compiler parser tokens",
        "compiler parser syntax",
        "storage pages budget",
        "storage pages ledger",
        "storage segments budget",
        "storage segments ledger",
        "storage disk audit receipts",
        "storage disk audit ledger",
        "storage disk latency batches",
        "storage disk latency budget",
    ]
    .into_iter()
    .enumerate()
    {
        tree.push(prepare(at, text));
    }
    let at = tree.arrived();
    let row = prepare(at, "storage disk latency recovery");
    let partition = std::collections::BTreeMap::from([
        (vec![9], vec![0, 1]),
        (vec![1, 8], vec![2, 3]),
        (vec![1, 6], vec![4, 5]),
        (vec![1, 2, 7], vec![6, 7]),
        (vec![1, 2, 3], vec![8, 9, at]),
    ]);
    let named = tree.seat_words(&row, &partition);
    let levels: Vec<(&[usize], &str)> = (named.iter())
        .map(|group| (group.members.as_slice(), group.words[0]))
        .collect();
    assert_eq!(
        levels,
        [
            (&[2, 3, 4, 5, 6, 7, 8, 9, at][..], "storage"),
            (&[6, 7, 8, 9, at][..], "disk"),
            (&[8, 9, at][..], "latency"),
        ]
    );
}

const AREAS: [[&str; 4]; 4] = [
    ["compiler", "parser", "syntax", "tokens"],
    ["storage", "disk", "pages", "segments"],
    ["dashboard", "theme", "charts", "panels"],
    ["release", "signing", "keys", "installer"],
];
const FACETS: [[&str; 3]; 3] = [
    ["errors", "recovery", "diagnostics"],
    ["latency", "batches", "budget"],
    ["audit", "ledger", "receipts"],
];
const VERBS: [&str; 7] = [
    "Tighten", "Measure", "Rework", "Document", "Guard", "Simplify", "Extend",
];

/// Arrival `n` is on area `n % 4`, seen through facet `n / 4 % 3`.
fn statement(n: usize) -> Prepared {
    let (area, facet, turn) = (AREAS[n % 4], FACETS[n / 4 % 3], n / 12);
    let title = format!(
        "{} {} {} {} {} {}",
        VERBS[n % 7],
        area[turn % 4],
        area[(turn + 1 + n % 3) % 4],
        facet[turn % 3],
        facet[(turn + 1) % 3],
        AREAS[(n + turn) % 4][n % 4],
    );
    prepare_fields(
        &format!("task{n}"),
        [std::slice::from_ref(&title), &[], &[], &[]],
    )
}

/// What `tree`, holding every row of `within`, ranks for `group`.
fn ranked_held(tree: &Rolling, group: &[usize], within: &[usize]) -> Vec<String> {
    let set: Vec<Fitted<'_, '_>> = (within.iter())
        .map(|&at| Fitted {
            numbered: tree.words.row(tree.row_index(at)),
            row: &tree.rows[tree.row_index(at)],
            counted: group.contains(&at),
        })
        .collect();
    (heaviest(&mut tree.words.fits(0), &set).into_iter())
        .map(str::to_owned)
        .collect()
}

/// A name is read from the fit that seats the arrival. The stream is one
/// where the same rows ranked before it arrived, an evicted predecessor
/// still among them, name the group otherwise: while the window fills, and
/// at each depth once it is full.
#[test]
fn a_forecast_seat_is_named_as_the_arrived_fit_names_it_at_every_depth() {
    let mut tree = Rolling::default();
    let (mut filling, mut full) = (0, [0; 2]);
    for n in 0..CAPACITY + 32 {
        let row = statement(n);
        if (32..CAPACITY).contains(&n) {
            tree.push(row);
            continue;
        }
        let forecast = tree.forecast(&row);
        let named = tree.seat_words(&row, &forecast);
        let mut after = tree.clone();
        after.push(row.clone());
        assert_eq!(forecast, after.held(), "arrival {n} was seated elsewhere");
        let prior = |members: &[usize]| members[..members.len() - 1].to_vec();
        for (depth, group) in named.iter().enumerate() {
            let (within, before) = match depth.checked_sub(1) {
                None => (&after.tree.root.members, tree.tree.root.members.clone()),
                Some(above) => (&named[above].members, prior(&named[above].members)),
            };
            let arrived = ranked_held(&after, &group.members, within);
            assert_eq!(group.words, arrived, "arrival {n}, depth {depth}");
            let beside = prior(&group.members);
            let renamed = !beside.is_empty()
                && ranked_held(&tree, &beside, &before).first() != arrived.first();
            match tree.rows.len() == CAPACITY {
                true => full[depth] += usize::from(renamed),
                false => filling += usize::from(renamed),
            }
        }
        tree = after;
    }
    assert!(
        filling > 0 && full.iter().all(|&arrivals| arrivals > 0),
        "no arrival changed its group's heaviest word: {filling} filling, {full:?} full"
    );
}
