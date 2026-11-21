//! Undirected Louvain. Neighbor insertion order settles equal gains. Vertex visit
//! order is a digest of original point identities, including after contraction.
//! Each vertex starts in its own community.
use sha2::{Digest, Sha256};

use super::Edge;

const RESOLUTION: f64 = 0.5;
const ORDER_SALT: &[u8] = b"42\0";

type Adjacency = Vec<Vec<(usize, f64)>>;

fn degrees(g: &Adjacency) -> Vec<f64> {
    g.iter()
        .enumerate()
        .map(|(u, adj)| {
            adj.iter().map(|&(_, w)| w).sum::<f64>()
                + adj.iter().find(|&&(v, _)| v == u).map_or(0., |e| e.1)
        })
        .collect()
}

fn optimize(
    g: &Adjacency,
    degree: &[f64],
    order: &[usize],
    mass: f64,
    gamma: f64,
) -> (Vec<usize>, usize) {
    let n = g.len();
    let mut owner: Vec<_> = (0..n).collect();
    let mut totals = degree.to_vec();
    let mut strength = vec![0.; n];
    let mut touched = Vec::new();
    loop {
        let mut changed = false;
        for &u in order {
            let current = owner[u];
            for &(v, w) in &g[u] {
                if v != u {
                    let c = owner[v];
                    if strength[c] == 0. {
                        touched.push(c);
                    }
                    strength[c] += w;
                }
            }
            totals[current] -= degree[u];
            // gamma = resolution / 2; keep the reference arithmetic grouping.
            let gain = |c: usize| strength[c] * mass - gamma * (totals[c] * degree[u]);
            let mut best = current;
            let mut best_gain = gain(current);
            for &c in &touched {
                let score = gain(c);
                if score > best_gain {
                    best_gain = score;
                    best = c;
                }
            }
            totals[best] += degree[u];
            owner[u] = best;
            changed |= best != current;
            for c in touched.drain(..) {
                strength[c] = 0.;
            }
        }
        if !changed {
            break;
        }
    }
    let mut used = vec![false; n];
    for &c in &owner {
        used[c] = true;
    }
    let mut number = 0;
    let map: Vec<_> = used
        .into_iter()
        .map(|present| {
            let i = number;
            number += usize::from(present);
            i
        })
        .collect();
    for c in &mut owner {
        *c = map[*c];
    }
    (owner, number)
}

fn contract(g: &Adjacency, owner: &[usize], n: usize) -> Adjacency {
    let mut next = vec![vec![]; n];
    // Each unordered edge is visited once, in vertex then insertion order.
    // Record both slots on first insertion; never sort aggregate neighbors.
    let mut slots = std::collections::HashMap::new();
    for (u, adj) in g.iter().enumerate() {
        for &(v, weight) in adj {
            if v < u {
                continue;
            }
            let (a, b) = (owner[u].min(owner[v]), owner[u].max(owner[v]));
            let (i, j) = *slots.entry((a, b)).or_insert_with(|| {
                let pair = (next[a].len(), next[b].len());
                next[a].push((b, 0.));
                if a != b {
                    next[b].push((a, 0.));
                }
                pair
            });
            next[a][i].1 += weight;
            if a != b {
                next[b][j].1 += weight;
            }
        }
    }
    next
}

fn modularity(
    g: &Adjacency,
    degree: &[f64],
    owner: &[usize],
    count: usize,
    resolution: f64,
) -> f64 {
    let mut totals = vec![0.; count];
    let mut inside = vec![0.; count];
    let twice_mass = degree.iter().sum::<f64>();
    for (u, adj) in g.iter().enumerate() {
        let c = owner[u];
        totals[c] += degree[u];
        for &(v, w) in adj {
            if v >= u && owner[v] == c {
                inside[c] += w;
            }
        }
    }
    (0..count)
        .map(|c| 2. * inside[c] / twice_mass - resolution * (totals[c] / twice_mass).powi(2))
        .sum()
}

/// Each partition lists original point indices, smallest communities first in
/// the series. Point identities and initial edges are lexicographically ordered.
pub(super) fn levels(ids: &[String], edges: &[Edge]) -> Vec<Vec<Vec<usize>>> {
    if edges.is_empty() {
        return vec![];
    }
    let mut g = vec![vec![]; ids.len()];
    for &Edge {
        left,
        right,
        weight,
    } in edges
    {
        g[left].push((right, weight));
        g[right].push((left, weight));
    }
    let mut degree = degrees(&g);
    let mass = degree.iter().sum::<f64>() / 2.;
    let singletons: Vec<Vec<usize>> = (0..ids.len()).map(|i| vec![i]).collect();
    let mut previous = modularity(
        &g,
        &degree,
        &(0..g.len()).collect::<Vec<_>>(),
        g.len(),
        RESOLUTION,
    );
    let mut result = Vec::new();
    loop {
        let groups = result.last().unwrap_or(&singletons);
        let mut order: Vec<_> = groups
            .iter()
            .enumerate()
            .map(|(i, group)| {
                let mut h = Sha256::new();
                h.update(ORDER_SALT);
                for (j, &p) in group.iter().enumerate() {
                    if j != 0 {
                        h.update([0]);
                    }
                    h.update(ids[p].as_bytes());
                }
                (h.finalize(), i)
            })
            .collect();
        order.sort_unstable();
        let (owner, count) = optimize(
            &g,
            &degree,
            &order.into_iter().map(|(_, i)| i).collect::<Vec<_>>(),
            mass,
            RESOLUTION / 2.,
        );
        if count == g.len() && !result.is_empty() {
            break;
        }
        let mut merged = vec![vec![]; count];
        for (old, points) in groups.iter().enumerate() {
            merged[owner[old]].extend_from_slice(points);
        }
        for points in &mut merged {
            points.sort_unstable();
        }
        let q = modularity(&g, &degree, &owner, count, RESOLUTION);
        result.push(merged);
        if q - previous <= 1e-7 || count == g.len() {
            break;
        }
        previous = q;
        g = contract(&g, &owner, count);
        degree = degrees(&g);
    }
    result
}
