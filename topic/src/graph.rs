//! Cold construction from one window's document vectors. Names, charters and
//! placement scores are deliberately absent: none can change a graph cohort.
mod louvain;

use std::collections::HashMap;

use crate::evidence::SparseRow;

const LINK_FLOOR: f64 = 0.16;
const NEIGHBOURS: usize = 12;

#[derive(Debug, Clone, Copy)]
pub struct Edge {
    pub left: usize,
    pub right: usize,
    pub weight: f64,
}

#[derive(Debug)]
pub struct Construction {
    /// Original row indices at each distinct geometric point, in task ID order.
    pub points: Vec<Vec<usize>>,
    pub edges: Vec<Edge>,
    /// Point partitions before support filtering, finest level first.
    pub levels: Vec<Vec<Vec<usize>>>,
}

type Row = SparseRow;

fn distinct_points(rows: &[Row], ids: &[&str]) -> Vec<Vec<usize>> {
    let mut order: Vec<_> = (0..rows.len())
        .filter(|&i| !rows[i].entries().is_empty())
        .collect();
    order.sort_unstable_by_key(|&i| ids[i]);
    // Equal rows agree on their length and first weight; rows that agree
    // on both are compared whole.
    let mut buckets = HashMap::<(usize, u64), Vec<usize>>::new();
    let mut points: Vec<Vec<usize>> = Vec::new();
    for i in order {
        let row = rows[i].entries();
        let bucket = buckets.entry((row.len(), row[0].1.to_bits())).or_default();
        if let Some(p) = bucket
            .iter()
            .copied()
            .find(|&p| rows[points[p][0]] == rows[i])
        {
            points[p].push(i);
        } else {
            bucket.push(points.len());
            points.push(vec![i]);
        }
    }
    points
}

fn geometry(rows: &[Row], points: &[Vec<usize>]) -> Vec<Edge> {
    let n = points.len();
    let row = |point: &Vec<usize>| rows[point[0]].entries();
    let columns = (points.iter().flat_map(row))
        .map(|entry| entry.0.0 as usize + 1)
        .max()
        .unwrap_or(0);
    // Every column's postings lie together, in the order its points come.
    let mut next = vec![0; columns + 1];
    for &(term, _) in points.iter().flat_map(row) {
        next[term.0 as usize + 1] += 1;
    }
    for column in 0..columns {
        next[column + 1] += next[column];
    }
    let first = next.clone();
    let mut postings = vec![(0, 0.); next[columns]];
    // Pairs at or above the floor, by their later point and then the other.
    let mut pairs: Vec<(usize, usize, f64)> = Vec::new();
    let mut bound = vec![0; n + 1];
    let mut own = vec![0.; n];
    for (i, point) in points.iter().enumerate() {
        own[..i].fill(0.);
        let norm = row(point).iter().map(|e| e.1 * e.1).sum::<f64>().sqrt();
        for &(t, value) in row(point) {
            let t = t.0 as usize;
            let unit = value / norm;
            for &(j, weight) in &postings[first[t]..next[t]] {
                own[j] += unit * weight;
            }
            postings[next[t]] = (i, unit);
            next[t] += 1;
        }
        for (j, &dot) in own[..i].iter().enumerate() {
            if dot >= LINK_FLOOR {
                pairs.push((i, j, dot));
                bound[i + 1] += 1;
                bound[j + 1] += 1;
            }
        }
    }
    // Every point's candidates lie together, ascending by the other point.
    for point in 0..n {
        bound[point + 1] += bound[point];
    }
    let mut fill = bound.clone();
    let mut candidates = vec![(0, 0.); 2 * pairs.len()];
    for &(i, j, dot) in &pairs {
        candidates[fill[i]] = (j, dot);
        candidates[fill[j]] = (i, dot);
        fill[i] += 1;
        fill[j] += 1;
    }
    // A point nominates its nearest candidates, the earlier point first
    // among equals, and keeps them ascending.
    for point in 0..n {
        let near = &mut candidates[bound[point]..bound[point + 1]];
        if near.len() > NEIGHBOURS {
            near.select_nth_unstable_by(NEIGHBOURS, |a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        }
        let kept = near.len().min(NEIGHBOURS);
        near[..kept].sort_unstable_by_key(|candidate| candidate.0);
        fill[point] = bound[point] + kept;
    }
    // Two points link when each nominates the other.
    let nominated = |point: usize| &candidates[bound[point]..fill[point]];
    let mut edges = vec![];
    for left in 0..n {
        for &(right, dot) in nominated(left) {
            if right > left && nominated(right).iter().any(|back| back.0 == left) {
                edges.push(Edge {
                    left,
                    right,
                    weight: dot.min(1.),
                });
            }
        }
    }
    edges
}

/// Build candidate partitions of distinct points in stable task identity order.
pub fn construct(rows: &[Row], ids: &[&str]) -> Construction {
    assert_eq!(ids.len(), rows.len());
    let points = distinct_points(rows, ids);
    let edges = geometry(rows, &points);
    let keys: Vec<_> = points
        .iter()
        .map(|p| {
            let mut s = String::from(ids[p[0]]);
            for &i in &p[1..] {
                s.push('\0');
                s.push_str(ids[i]);
            }
            s
        })
        .collect();
    let levels = louvain::levels(&keys, &edges);
    Construction {
        points,
        edges,
        levels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::TermId;

    fn row(entries: &[(u32, f64)]) -> Row {
        entries.iter().map(|&(t, w)| (TermId(t), w)).collect()
    }

    #[test]
    fn identical_rows_share_a_point_and_empty_rows_have_none() {
        let mut rows = (0..8)
            .map(|i| row(&[(i / 4, 0.8), (i + 2, 0.6)]))
            .collect::<Vec<_>>();
        rows.extend([rows[0].clone(), row(&[]), row(&[(99, 1.)])]);
        let ids = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k"];
        let g = construct(&rows, &ids);
        assert_eq!(g.points[0], [0, 8]);
        assert!(!g.points.iter().flatten().any(|&row| row == 9));
    }

    #[test]
    fn exact_neighbor_cutoff_ties_use_identity_not_arrival_order() {
        let rows = (0..14)
            .map(|i| row(&[(0, 0.8), (1 + i, 0.6)]))
            .collect::<Vec<_>>();
        let ids = [
            "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n",
        ];
        let a = construct(&rows, &ids);
        let mut rows = rows;
        rows.reverse();
        let mut reversed = ids;
        reversed.reverse();
        let b = construct(&rows, &reversed);
        let pairs = |g: &Construction| {
            g.edges
                .iter()
                .map(|e| (e.left, e.right))
                .collect::<Vec<_>>()
        };
        assert_eq!(pairs(&a), pairs(&b));
        // Every point except the last belongs to the mutually nominated clique.
        let clique = ids.len() - 1;
        assert_eq!(a.edges.len(), clique * (clique - 1) / 2);
        assert!(a.edges.iter().all(|e| e.right < clique));
    }
}
