//! A kept tree with priced edits and compulsory seats for indivisible points.
//!
//! Each set weighs and links only its own tasks. Voluntary edits are priced in nats.
//!
//! - A grouping of a set is worth its links times the divergence between
//!   the share of them inside the groups and the share degrees alone would
//!   put there, less `ln(tasks)` for every group it names.
//! - A set with no groups takes the grouping that makes the whole tree
//!   below it worth most, among those worth more on their own than the
//!   price of taking them.
//! - A shared point takes its first occupied sibling seat. Conflicting
//!   prior seats transfer without a price. An unseated
//!   point follows its links or stands alone.
//! - A task changes group, two groups join under one, a group gives way
//!   to its own groups, or a set takes or loses groups, when that is
//!   worth more than restating the group of every task it moves,
//!   `ln(groups)` each. A set left with fewer than two groups loses them
//!   whatever that costs.
//!
//! A card is looked at in the sets it enters or leaves, from the window
//! down, and then in any set whose tasks changed because of what was done
//! there. No other set is looked at.

use std::collections::{BTreeMap, HashMap, VecDeque};

use crate::CAPACITY;
use crate::evidence::Prepared;
use crate::graph;
use crate::words::{Fits, Foreseen, Numbered, Words};

#[cfg(test)]
mod tests;

/// One set of tasks by arrival position, and the groups it contains.
#[derive(Clone)]
struct Node {
    id: usize,
    members: Vec<usize>,
    groups: Vec<Node>,
    /// Whether its members changed since it was last looked at.
    stale: bool,
}

struct Rows<'a> {
    fixed: usize,
    expired: usize,
    rows: &'a VecDeque<Prepared>,
    words: &'a Words,
    prospective: Option<(&'a Prepared, &'a Foreseen)>,
}

impl Rows<'_> {
    /// The identity and numbered evidence of the task at `index`.
    fn at(&self, index: usize) -> (&str, &Numbered) {
        let index = row_index(index, self.fixed, self.expired);
        if index == self.rows.len() {
            let (row, foreseen) = self.prospective.expect("hypothetical arrival is present");
            (&row.id, &foreseen.row)
        } else {
            (&self.rows[index].id, self.words.row(index))
        }
    }
}

// Fixed rows precede the rolling window. Only the interval between them
// and the oldest retained statement is absent from the arrival sequence.
fn row_index(position: usize, fixed: usize, expired: usize) -> usize {
    if position < fixed {
        position
    } else {
        position - expired
    }
}

struct World<'a> {
    tasks: Rows<'a>,
    fits: Fits,
    next: usize,
}

/// A set seen as its own world: its distinct points, the links between
/// them, and the groupings its own fit proposes.
struct Own {
    /// Point of every member, by member index. A task with no words is a
    /// point of its own with no links.
    point: Vec<usize>,
    /// Tasks at every point.
    weight: Vec<usize>,
    edges: Vec<(usize, usize)>,
    beside: Vec<Vec<usize>>,
    /// Group of every point in each proposed grouping.
    proposals: Vec<Vec<usize>>,
    /// What naming one group of this set costs.
    named: f64,
}

impl World<'_> {
    fn own(&mut self, members: &[usize]) -> Own {
        let (ids, tasks): (Vec<&str>, Vec<&Numbered>) =
            members.iter().map(|&at| self.tasks.at(at)).unzip();
        let net = graph::construct(&self.fits.graph(&tasks), &ids);
        let mut point = vec![usize::MAX; members.len()];
        let mut weight: Vec<usize> = net.points.iter().map(Vec::len).collect();
        for (index, rows) in net.points.iter().enumerate() {
            for &row in rows {
                point[row] = index;
            }
        }
        for at in &mut point {
            if *at == usize::MAX {
                *at = weight.len();
                weight.push(1);
            }
        }
        let edges: Vec<(usize, usize)> = (net.edges.iter())
            .map(|edge| (edge.left, edge.right))
            .collect();
        let mut beside = vec![Vec::new(); weight.len()];
        for &(left, right) in &edges {
            beside[left].push(right);
            beside[right].push(left);
        }
        let proposals = (net.levels.iter())
            .map(|partition| {
                let mut label: Vec<usize> =
                    (0..weight.len()).map(|at| partition.len() + at).collect();
                for (group, points) in partition.iter().enumerate() {
                    for &at in points {
                        label[at] = group;
                    }
                }
                dense(&mut label);
                label
            })
            .collect();
        Own {
            point,
            weight,
            edges,
            beside,
            proposals,
            named: (members.len() as f64).ln(),
        }
    }

    fn fresh(&mut self, members: Vec<usize>) -> Node {
        self.next += 1;
        Node {
            id: self.next,
            members,
            groups: Vec::new(),
            stale: false,
        }
    }

    /// Gives a set with no groups the grouping that makes the whole tree
    /// below it worth most, and returns what that tree is worth.
    fn plant(&mut self, node: &mut Node, take: f64) -> f64 {
        node.stale = false;
        node.groups.clear();
        if node.members.len() < 2 {
            return 0.;
        }
        let own = self.own(&node.members);
        let mut best = 0.;
        for label in &own.proposals {
            let worth = own.worth(label);
            let groups = label.iter().copied().max().map_or(0, |most| most + 1);
            if worth <= 0. || groups < 2 {
                continue;
            }
            let mut members = vec![Vec::new(); groups];
            for (member, &at) in node.members.iter().enumerate() {
                members[label[own.point[member]]].push(at);
            }
            let largest = members.iter().map(Vec::len).max().expect("two groups");
            if worth <= take * restatement(node.members.len(), largest, groups) {
                continue;
            }
            let mut total = worth;
            let mut grown = Vec::new();
            for members in members {
                let mut group = self.fresh(members);
                total += self.plant(&mut group, take);
                grown.push(group);
            }
            if total > best {
                best = total;
                node.groups = grown;
            }
        }
        best
    }
}

/// The whole counts a grouping's worth is read from. Kept current through a
/// move, they give exactly the worth a recount would.
struct Tally {
    /// Links with both ends in one group.
    within: usize,
    /// Link ends in each group.
    ends: Vec<usize>,
    /// Sum of every group's squared link ends.
    squares: usize,
    /// Groups that hold a point.
    groups: usize,
}

impl Own {
    /// Settle individual point moves at the current cost of changing groups.
    fn settle(&self, label: &mut [usize]) -> (f64, f64) {
        let groups = dense(label);
        let mut restated = (groups as f64).ln();
        let mut live = groups;
        let mut counts = vec![0usize; groups];
        for &group in label.iter() {
            counts[group] += 1;
        }
        let mut tally = self.tally(label);
        let mut worth = self.value(tally.within, tally.squares, live);
        let (mut beside, mut links) = (Vec::new(), Vec::<(usize, usize)>::new());
        // A point changes group when that is worth more than restating
        // the group of each of its tasks.
        loop {
            let mut changed = false;
            for point in 0..label.len() {
                let price = self.weight[point] as f64 * restated;
                let home = label[point];
                // The groups this point links to, ascending, and its links
                // into each.
                beside.clear();
                beside.extend(self.beside[point].iter().map(|&other| label[other]));
                beside.sort_unstable();
                links.clear();
                for &group in &beside {
                    match links.last_mut() {
                        Some((last, count)) if *last == group => *count += 1,
                        _ => links.push((group, 1)),
                    }
                }
                let own = beside.len();
                let stay = (links.iter().find(|link| link.0 == home)).map_or(0, |link| link.1);
                let moved = |tally: &Tally, group: usize, join: usize| {
                    let (from, to) = (tally.ends[home], tally.ends[group]);
                    let squares =
                        tally.squares + (from - own) * (from - own) + (to + own) * (to + own)
                            - from * from
                            - to * to;
                    (tally.within + join - stay, squares)
                };
                let mut best = (worth + price, home, 0);
                for &(group, join) in &links {
                    if group == home {
                        continue;
                    }
                    let (within, squares) = moved(&tally, group, join);
                    let there = self.value(within, squares, live - usize::from(counts[home] == 1));
                    if there > best.0 {
                        best = (there, group, join);
                    }
                }
                let (there, group, join) = best;
                if group != home {
                    (tally.within, tally.squares) = moved(&tally, group, join);
                    tally.ends[home] -= own;
                    tally.ends[group] += own;
                    label[point] = group;
                    counts[home] -= 1;
                    counts[group] += 1;
                    if counts[home] == 0 {
                        live -= 1;
                        restated = (live as f64).ln();
                    }
                    worth = there;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        (worth, restated)
    }

    fn tally(&self, label: &[usize]) -> Tally {
        let most = label.iter().copied().max().map_or(0, |most| most + 1);
        let mut ends = vec![0; most];
        let mut used = vec![false; most];
        for &group in label {
            used[group] = true;
        }
        let mut within = 0;
        for &(left, right) in &self.edges {
            ends[label[left]] += 1;
            ends[label[right]] += 1;
            within += usize::from(label[left] == label[right]);
        }
        Tally {
            within,
            squares: ends.iter().map(|ends| ends * ends).sum(),
            ends,
            groups: used.iter().filter(|&&used| used).count(),
        }
    }

    /// What grouping the points by `label` is worth, in nats.
    fn worth(&self, label: &[usize]) -> f64 {
        let tally = self.tally(label);
        self.value(tally.within, tally.squares, tally.groups)
    }

    /// What a grouping is worth, in nats, by its links inside one group, the
    /// sum of its groups' squared link ends, and how many groups it names.
    fn value(&self, within: usize, squares: usize, groups: usize) -> f64 {
        let mass = self.edges.len() as f64;
        let evidence = if mass == 0. {
            0.
        } else {
            let (p, q) = (within as f64 / mass, squares as f64 / (4. * mass * mass));
            let part = |observed: f64, expected: f64| {
                if observed == 0. {
                    0.
                } else {
                    observed * (observed / expected).ln()
                }
            };
            let divergence = mass * (part(p, q) + part(1. - p, 1. - q));
            if p >= q { divergence } else { -divergence }
        };
        evidence - groups as f64 * self.named
    }
}

/// Renumbers `label` so its groups are 0, 1, 2, … in order of first use.
fn dense(label: &mut [usize]) -> usize {
    let mut seen = HashMap::new();
    for group in label.iter_mut() {
        let next = seen.len();
        *group = *seen.entry(*group).or_insert(next);
    }
    seen.len()
}

fn restatement(members: usize, largest: usize, groups: usize) -> f64 {
    (members - largest) as f64 * (groups as f64).ln()
}

impl Node {
    fn advance(&mut self, arriving: usize, world: &mut World<'_>) {
        self.members.push(arriving);
        self.stale = true;
        self.refresh(world);
    }

    fn remove(&mut self, at: usize) {
        let Ok(index) = self.members.binary_search(&at) else {
            return;
        };
        self.members.remove(index);
        self.stale = true;
        for group in &mut self.groups {
            group.remove(at);
        }
        self.groups.retain(|group| !group.members.is_empty());
    }

    /// Brings this set up to date after its members changed.
    fn refresh(&mut self, world: &mut World<'_>) {
        if !self.stale {
            return;
        }
        self.stale = false;
        if self.members.len() < 2 {
            self.groups.clear();
            return;
        }
        if self.groups.is_empty() {
            world.plant(self, 1.);
            return;
        }
        let own = world.own(&self.members);
        loop {
            let mut label = self.seats(&own);
            let (worth, restated) = own.settle(&mut label);
            let groups = dense(&mut label);
            self.adopt(world, &own, &label, groups);
            if self.groups.len() < 2 {
                break;
            }
            let size: Vec<f64> = self
                .groups
                .iter()
                .map(|node| node.members.len() as f64)
                .collect();
            // Two linked groups join under one when that is worth more than
            // restating the group of the smaller one's tasks.
            let mut linked: Vec<(usize, usize)> = (own.edges.iter())
                .map(|&(left, right)| {
                    (label[left].min(label[right]), label[left].max(label[right]))
                })
                .filter(|(a, b)| a != b)
                .collect();
            linked.sort_unstable();
            let tally = own.tally(&label);
            let mut join: Option<(f64, usize, usize)> = None;
            for links in linked.chunk_by(|left, right| left == right) {
                let (a, b) = links[0];
                let price = size[a].min(size[b]) * restated;
                let joined = own.value(
                    tally.within + links.len(),
                    tally.squares + 2 * tally.ends[a] * tally.ends[b],
                    tally.groups - 1,
                );
                let gain = joined - worth - price;
                if gain > 0. && join.is_none_or(|best| gain > best.0) {
                    join = Some((gain, a, b));
                }
            }
            if let Some((_, a, b)) = join {
                let (keep, give) = if size[a] >= size[b] { (a, b) } else { (b, a) };
                let given = self.groups.remove(give);
                let kept = &mut self.groups[keep - usize::from(give < keep)];
                if kept.groups.is_empty() {
                    // The joined set continues the larger group; what that
                    // group was continues inside it.
                    let inner = world.fresh(kept.members.clone());
                    kept.groups.push(inner);
                }
                kept.members.extend(&given.members);
                kept.members.sort_unstable();
                kept.groups.push(given);
                kept.stale = true;
                continue;
            }
            // A group gives way to its own groups when this set's grouping
            // is worth more with them than with it, by more than restating
            // the group of all but its largest one's tasks.
            let mut promotion: Option<(f64, usize, Vec<usize>)> = None;
            for (group, node) in self.groups.iter().enumerate() {
                let Some((opened, restated_tasks)) =
                    node.promotion(&self.members, &own, &label, group, groups)
                else {
                    continue;
                };
                let price = restated_tasks as f64 * restated;
                let gain = own.worth(&opened) - worth - price;
                if gain > 0. && promotion.as_ref().is_none_or(|best| gain > best.0) {
                    promotion = Some((gain, group, opened));
                }
            }
            let Some((_, group, label)) = promotion else {
                break;
            };
            self.apply_promotion(&own, &label, group);
        }
        let mut label = self.seats(&own);
        dense(&mut label);
        let forced = self.groups.len() < 2;
        let should_collapse = if forced {
            true
        } else {
            let largest = self
                .groups
                .iter()
                .map(|g| g.members.len())
                .max()
                .expect("two groups");
            own.worth(&label) <= -restatement(self.members.len(), largest, self.groups.len())
        };
        if should_collapse {
            world.plant(self, 1.);
            return;
        }
        for group in &mut self.groups {
            group.refresh(world);
        }
    }

    /// Keep promoted child memberships. A direct task goes with its point's
    /// child, or with the first child when that point has no child yet.
    fn promotion(
        &self,
        parent: &[usize],
        own: &Own,
        label: &[usize],
        group: usize,
        groups: usize,
    ) -> Option<(Vec<usize>, usize)> {
        if self.groups.len() < 2 {
            return None;
        }
        let point = |at| own.point[parent.binary_search(&at).expect("a member")];
        let mut opened = label.to_vec();
        for seat in &mut opened {
            if *seat == group {
                *seat = usize::MAX;
            }
        }
        for (index, child) in self.groups.iter().enumerate() {
            let seat = if index == 0 {
                group
            } else {
                groups + index - 1
            };
            for &at in &child.members {
                let current = &mut opened[point(at)];
                if *current != usize::MAX && *current != seat {
                    // One point's tasks are spread over several children.
                    // Promoting them intact is not a partition on this
                    // parent's graph.
                    return None;
                }
                *current = seat;
            }
        }
        let mut sizes = vec![0; self.groups.len()];
        for &at in &self.members {
            let seat = &mut opened[point(at)];
            if *seat == usize::MAX {
                *seat = group;
            }
            sizes[if *seat == group {
                0
            } else {
                *seat - groups + 1
            }] += 1;
        }
        let restated = self.members.len() - sizes.into_iter().max().expect("promoted children");
        Some((opened, restated))
    }

    fn apply_promotion(&mut self, own: &Own, label: &[usize], group: usize) {
        // Preserve the same label order used to score the proposed partition.
        let opened = &mut self.groups[group];
        let mut children = std::mem::take(&mut opened.groups).into_iter();
        let previous = std::mem::replace(opened, children.next().expect("first promoted child"));
        self.groups.extend(children);
        for at in previous.members {
            let point = own.point[self.members.binary_search(&at).expect("a member")];
            let child = &mut self.groups[label[point]];
            if let Err(index) = child.members.binary_search(&at) {
                child.members.insert(index, at);
                child.stale = true;
            }
        }
    }

    /// The group of every point: that of its placed tasks, and for a point
    /// with none the group most of its links go to, or one of its own.
    fn seats(&self, own: &Own) -> Vec<usize> {
        let index = |at: usize| self.members.binary_search(&at).expect("a member");
        let mut seat: Vec<Option<usize>> = vec![None; own.weight.len()];
        for (group, node) in self.groups.iter().enumerate() {
            for &at in &node.members {
                let point = own.point[index(at)];
                seat[point].get_or_insert(group);
            }
        }
        let mut groups = self.groups.len();
        for point in 0..seat.len() {
            if seat[point].is_some() {
                continue;
            }
            let mut links: BTreeMap<usize, usize> = BTreeMap::new();
            for group in own.beside[point].iter().filter_map(|&other| seat[other]) {
                *links.entry(group).or_insert(0) += 1;
            }
            let most = links.values().copied().max();
            seat[point] = Some(match most {
                Some(most) => {
                    *links
                        .iter()
                        .find(|&(_, &count)| count == most)
                        .expect("a group")
                        .0
                }
                None => {
                    groups += 1;
                    groups - 1
                }
            });
        }
        seat.into_iter()
            .map(|group| group.expect("seated"))
            .collect()
    }

    /// Makes the groups of this set those `label` gives its points, in
    /// label order, keeping each existing group that continues.
    fn adopt(&mut self, world: &mut World<'_>, own: &Own, label: &[usize], groups: usize) {
        let mut members: Vec<Vec<usize>> = vec![Vec::new(); groups];
        for (member, &at) in self.members.iter().enumerate() {
            members[label[own.point[member]]].push(at);
        }
        // Which existing group each new group continues: the one it shares
        // most tasks with, if that one shares most with it too.
        let old: HashMap<usize, usize> = (self.groups.iter().enumerate())
            .flat_map(|(group, node)| node.members.iter().map(move |&at| (at, group)))
            .collect();
        let mut shared: BTreeMap<(usize, usize), usize> = BTreeMap::new();
        for (new, tasks) in members.iter().enumerate() {
            for at in tasks {
                if let Some(&was) = old.get(at) {
                    *shared.entry((was, new)).or_insert(0) += 1;
                }
            }
        }
        let (mut onward, mut back) = (HashMap::new(), HashMap::new());
        for (&(was, new), &count) in &shared {
            let best = onward.entry(was).or_insert((0, new));
            if count > best.0 {
                *best = (count, new);
            }
            let best = back.entry(new).or_insert((0, was));
            if count > best.0 {
                *best = (count, was);
            }
        }
        let mut existing: Vec<Option<Node>> = std::mem::take(&mut self.groups)
            .into_iter()
            .map(Some)
            .collect();
        for (new, tasks) in members.into_iter().enumerate() {
            let continues = back
                .get(&new)
                .map(|&(_, was)| was)
                .filter(|was| onward[was].1 == new);
            let node = match continues.and_then(|was| existing[was].take()) {
                Some(mut node) => {
                    let gone: Vec<usize> = (node.members.iter().copied())
                        .filter(|at| tasks.binary_search(at).is_err())
                        .collect();
                    for at in gone {
                        node.remove(at);
                    }
                    if node.members != tasks {
                        node.members = tasks;
                        node.stale = true;
                    }
                    node
                }
                None => {
                    let mut node = world.fresh(tasks);
                    node.stale = true;
                    node
                }
            };
            self.groups.push(node);
        }
    }

    /// Every node of this set's tree by identity path, with the tasks it
    /// holds directly; a node that holds none has an empty row. A task
    /// alone in its group is held by the set above it.
    fn held(&self, path: &mut Vec<usize>, out: &mut BTreeMap<Vec<usize>, Vec<usize>>) {
        let direct = out.entry(path.clone()).or_default();
        if self.groups.is_empty() {
            direct.extend(&self.members);
            return;
        }
        direct.extend(
            self.groups
                .iter()
                .filter(|group| group.members.len() == 1)
                .map(|group| group.members[0]),
        );
        direct.sort_unstable();
        for group in self.groups.iter().filter(|group| group.members.len() > 1) {
            path.push(group.id);
            group.held(path, out);
            path.pop();
        }
    }
}

/// Seats one arrival, evicting the oldest once the window is full. While
/// `unopened`, the root has never had groups and is offered the unpriced
/// split; from the arrival that takes one, every edit is priced.
fn arrive(
    root: &mut Node,
    unopened: &mut bool,
    world: &mut World<'_>,
    arriving: usize,
    evict: Option<usize>,
) {
    if let Some(oldest) = evict {
        root.remove(oldest);
    }
    if !*unopened {
        return root.advance(arriving, world);
    }
    root.members.push(arriving);
    world.plant(root, 0.);
    *unopened = root.groups.is_empty();
}

/// A kept tree between arrivals: everything growth carries forward. The
/// first-ever split is free; later edits are priced.
#[derive(Clone)]
struct Kept {
    root: Node,
    /// The last node ID given out.
    next: usize,
    /// Whether the root has never had groups.
    unopened: bool,
    arrived: usize,
}

impl Default for Kept {
    /// The empty tree.
    fn default() -> Self {
        Self {
            root: Node {
                id: 1,
                members: Vec::new(),
                groups: Vec::new(),
                stale: false,
            },
            next: 1,
            unopened: true,
            arrived: 0,
        }
    }
}

impl Kept {
    /// Direct membership in the visible tree. Rows are absolute arrival
    /// positions; node IDs persist across arrivals, not independent snapshots.
    #[must_use]
    pub fn held(&self) -> BTreeMap<Vec<usize>, Vec<usize>> {
        let mut held = BTreeMap::new();
        self.root.held(&mut Vec::new(), &mut held);
        held
    }
}

/// Online growth over fixed evidence and at most `CAPACITY` rolling statements.
/// This is local derived state; the engine does not validate authority.
#[derive(Clone, Default)]
pub struct Rolling {
    tree: Kept,
    fixed: usize,
    rows: VecDeque<Prepared>,
    /// The rows' words as numbers, derived from the rows.
    words: Words,
}

impl Rolling {
    /// Seed the tree with rows that participate in every fit and never age out.
    pub fn with_fixed(fixed: impl IntoIterator<Item = Prepared>) -> Self {
        let mut tree = Self::default();
        for row in fixed {
            tree.push(row);
            tree.fixed += 1;
        }
        tree
    }

    fn evicted_by_arrival(&self) -> Option<usize> {
        (self.rows.len() - self.fixed == CAPACITY).then(|| self.tree.arrived - CAPACITY)
    }

    /// Admit one new statement, evicting the oldest before fitting.
    pub fn push(&mut self, row: Prepared) {
        let evict = self.evicted_by_arrival();
        if evict.is_some() {
            let oldest = self.rows.remove(self.fixed).expect("full statement window");
            self.words.remove(self.fixed, &oldest);
        }
        self.words.push(&row);
        self.rows.push_back(row);
        let arriving = self.tree.arrived;
        let mut world = World {
            tasks: Rows {
                fixed: self.fixed,
                expired: arriving + 1 - self.rows.len(),
                rows: &self.rows,
                words: &self.words,
                prospective: None,
            },
            fits: self.words.fits(0),
            next: self.tree.next,
        };
        arrive(
            &mut self.tree.root,
            &mut self.tree.unopened,
            &mut world,
            arriving,
            evict,
        );
        self.tree.next = world.next;
        self.tree.arrived = arriving + 1;
    }

    /// Seat one hypothetical arrival on a copy of the tree, borrowing all
    /// historical evidence. The resident's working window stays untouched.
    /// Each direct seat lists its absolute arrival positions in ascending order.
    #[must_use]
    pub fn forecast(&self, row: &Prepared) -> BTreeMap<Vec<usize>, Vec<usize>> {
        let mut tree = self.tree.clone();
        let foreseen = self.words.foresee(row);
        let mut world = World {
            tasks: Rows {
                fixed: self.fixed,
                expired: tree.arrived - self.rows.len(),
                rows: &self.rows,
                words: &self.words,
                prospective: Some((row, &foreseen)),
            },
            fits: self.words.fits(foreseen.beyond),
            next: tree.next,
        };
        arrive(
            &mut tree.root,
            &mut tree.unopened,
            &mut world,
            tree.arrived,
            self.evicted_by_arrival(),
        );
        tree.held()
    }

    /// Count of arrivals in this lineage, including rows no longer held.
    #[must_use]
    pub fn arrived(&self) -> usize {
        self.tree.arrived
    }

    /// Prepared evidence in arrival order, oldest first.
    #[must_use]
    pub fn rows(&self) -> &VecDeque<Prepared> {
        &self.rows
    }

    /// Index of a retained arrival (from `held` or `forecast`) in `rows()`.
    pub fn row_index(&self, position: usize) -> usize {
        row_index(position, self.fixed, self.arrived() - self.rows.len())
    }

    /// Direct seats, with ascending absolute arrival positions in this lineage.
    #[must_use]
    pub fn held(&self) -> BTreeMap<Vec<usize>, Vec<usize>> {
        self.tree.held()
    }

    /// The words that name the seat `forecast` gave `row` on this tree: one
    /// group for each level of the seat's path, outermost first. Each group
    /// above the seat is everything beneath it, and the seat itself is what
    /// it holds directly unless it is a first-level group; each is fitted
    /// within the group above it, the first within everything the forecast
    /// holds. An arrival that joined no group is read alone within everything
    /// held. Each is the fit that seated it: the arrival is in it and a
    /// predecessor it evicted is not.
    #[must_use]
    pub fn seat_words<'a>(
        &'a self,
        row: &'a Prepared,
        forecast: &BTreeMap<Vec<usize>, Vec<usize>>,
    ) -> Vec<Named<'a>> {
        let arriving = self.tree.arrived;
        let foreseen = self.words.foresee(row);
        let mut fits = self.words.fits(foreseen.beyond);
        let mut read = |members: Vec<usize>, within: &[usize]| {
            let set: Vec<Fitted<'_, 'a>> = (within.iter())
                .map(|&at| {
                    let (numbered, evidence) = if at == arriving {
                        (&foreseen.row, row)
                    } else {
                        let index = self.row_index(at);
                        (self.words.row(index), &self.rows[index])
                    };
                    Fitted {
                        numbered,
                        row: evidence,
                        counted: members.binary_search(&at).is_ok(),
                    }
                })
                .collect();
            let words = heaviest(&mut fits, &set);
            Named { members, words }
        };
        let beneath = |group: &[usize]| -> Vec<usize> {
            let mut members: Vec<usize> = (forecast.iter())
                .filter(|(path, _)| path.starts_with(group))
                .flat_map(|(_, members)| members.iter().copied())
                .collect();
            members.sort_unstable();
            members
        };
        let everything = beneath(&[]);
        let Some((path, direct)) = (forecast.iter())
            .find(|(_, members)| members.contains(&arriving))
            .filter(|(path, _)| !path.is_empty())
        else {
            return vec![read(vec![arriving], &everything)];
        };
        let mut named: Vec<Named<'a>> = Vec::with_capacity(path.len());
        for depth in 1..=path.len() {
            let members = if depth > 1 && depth == path.len() {
                direct.clone()
            } else {
                beneath(&path[..depth])
            };
            let group = match named.last() {
                Some(above) => read(members, &above.members),
                None => read(members, &everything),
            };
            named.push(group);
        }
        named
    }

    /// Cosine to prior rows in the parent that formed `seat`. All rows
    /// beneath that parent contribute to the fit, including the arrival;
    /// `compare` selects which prior rows to score, not which to fit.
    pub fn likeness<'a>(
        &self,
        row: &Prepared,
        forecast: &'a BTreeMap<Vec<usize>, Vec<usize>>,
        seat: &'a [usize],
        compare: impl Fn(usize) -> bool + 'a,
    ) -> impl Iterator<Item = (usize, f64)> + 'a {
        let arriving = self.tree.arrived;
        let foreseen = self.words.foresee(row);
        let parent = seat.split_last().map_or(&[][..], |(_, parent)| parent);
        let held = forecast
            .iter()
            .filter(move |(path, _)| path.starts_with(parent))
            .flat_map(|(_, members)| members)
            .copied()
            .filter(move |&at| at != arriving);
        let mut tasks: Vec<&Numbered> = held
            .clone()
            .map(|at| self.words.row(self.row_index(at)))
            .collect();
        tasks.push(&foreseen.row);
        let mut fitted = self.words.fits(foreseen.beyond).graph(&tasks);
        let arrival = fitted.pop().expect("the arrival is fitted last");
        let columns = (arrival.entries().iter())
            .map(|&(term, _)| term.0 as usize + 1)
            .max()
            .unwrap_or(0);
        let mut own = vec![0.; columns];
        for &(term, weight) in arrival.entries() {
            own[term.0 as usize] = weight;
        }
        held.zip(fitted)
            .filter(move |(at, _)| compare(*at))
            .map(move |(at, other)| {
                // An identical nonzero unit vector has cosine one, even when the
                // sum of its squared coordinates rounds just below one.
                let likeness = if !own.is_empty() && arrival == other {
                    1.
                } else {
                    other
                        .entries()
                        .iter()
                        .filter_map(|&(term, weight)| Some(own.get(term.0 as usize)? * weight))
                        .sum::<f64>()
                        .min(1.)
                };
                (at, likeness)
            })
    }
}

/// One group of a forecast seat and the words that name it.
pub struct Named<'a> {
    /// Arrival positions of the group, the arrival among them, ascending.
    pub members: Vec<usize>,
    /// Its words by the weight they carry for it in the fit, heaviest first,
    /// equal weights in spelling order.
    pub words: Vec<&'a str>,
}

/// One task of a set being read for its words.
struct Fitted<'n, 'a> {
    numbered: &'n Numbered,
    row: &'a Prepared,
    /// Whether its words count toward the group being named.
    counted: bool,
}

/// Words by the weight they carry for the counted tasks when `set` is fitted
/// as a set of its own: the fit that seats tasks, read for its words.
fn heaviest<'a>(fits: &mut Fits, set: &[Fitted<'_, 'a>]) -> Vec<&'a str> {
    let tasks: Vec<&Numbered> = set.iter().map(|task| task.numbered).collect();
    let fitted = fits.graph(&tasks);
    let mut weights = BTreeMap::new();
    for (task, fitted) in set.iter().zip(&fitted).filter(|(task, _)| task.counted) {
        for (word, (_, weight)) in task.row.admitted.iter().zip(fitted.entries()) {
            *weights.entry(word.as_str()).or_default() += weight;
        }
    }
    ranked(weights)
}

fn ranked(weights: BTreeMap<&str, f64>) -> Vec<&str> {
    let mut order: Vec<(&str, f64)> = weights.into_iter().collect();
    // Stable, so equal weights keep the map's spelling order.
    order.sort_by(|left, right| right.1.total_cmp(&left.1));
    order.into_iter().map(|(word, _)| word).collect()
}
