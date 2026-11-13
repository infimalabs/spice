//! Renderer-independent admission for inert Mermaid node geometry.

use std::collections::BTreeSet;

pub(crate) const NODE_LIMIT: usize = 128;
pub(crate) const NODE_ID_BYTES: usize = 64;
const COORDINATE_LIMIT: f64 = 1_000_000.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rect {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Node {
    pub(crate) id: String,
    pub(crate) rect: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Geometry {
    pub(crate) view_box: Rect,
    pub(crate) nodes: Vec<Node>,
}

pub(crate) fn valid_node_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= NODE_ID_BYTES
        && bytes[0].is_ascii_alphabetic()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(crate) fn validate_ids(ids: &[String]) -> Result<(), &'static str> {
    if ids.len() > NODE_LIMIT {
        return Err("Diagram node geometry exceeds its count bound");
    }
    let mut unique = BTreeSet::new();
    for id in ids {
        if !valid_node_id(id) || !unique.insert(id.as_str()) {
            return Err("Invalid diagram node geometry ID");
        }
    }
    Ok(())
}

pub(crate) fn admit(
    expected: &[String],
    view_box: [f64; 4],
    nodes: Vec<(String, [f64; 4])>,
) -> Result<Geometry, &'static str> {
    validate_ids(expected)?;
    let view_box = rect(view_box, true)?;
    if nodes.len() != expected.len() || nodes.len() > NODE_LIMIT {
        return Err("Incomplete diagram node geometry");
    }
    let expected = expected.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let mut admitted = Vec::with_capacity(nodes.len());
    let mut seen = BTreeSet::new();
    for (id, raw) in nodes {
        if !valid_node_id(&id) || !expected.contains(id.as_str()) || !seen.insert(id.clone()) {
            return Err("Unknown or duplicate diagram node geometry ID");
        }
        let rect = rect(raw, false)?;
        if !contains(view_box, rect) {
            return Err("Diagram node geometry falls outside its view box");
        }
        admitted.push(Node { id, rect });
    }
    admitted.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(Geometry {
        view_box,
        nodes: admitted,
    })
}

fn rect([x, y, width, height]: [f64; 4], view_box: bool) -> Result<Rect, &'static str> {
    let values = [x, y, width, height];
    if values
        .iter()
        .any(|value| !value.is_finite() || value.abs() > COORDINATE_LIMIT)
        || width <= 0.0
        || height <= 0.0
    {
        return Err(if view_box {
            "Invalid diagram geometry view box"
        } else {
            "Invalid diagram node rectangle"
        });
    }
    Ok(Rect {
        x,
        y,
        width,
        height,
    })
}

fn contains(view_box: Rect, node: Rect) -> bool {
    let slack = (view_box.width.max(view_box.height) * 1.0e-6).max(0.01);
    node.x >= view_box.x - slack
        && node.y >= view_box.y - slack
        && node.x + node.width <= view_box.x + view_box.width + slack
        && node.y + node.height <= view_box.y + view_box.height + slack
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expected() -> Vec<String> {
        vec!["node-a".to_owned(), "node_b".to_owned()]
    }

    #[test]
    fn admits_complete_geometry_in_stable_caller_id_order() {
        let geometry = admit(
            &expected(),
            [0.0, 0.0, 200.0, 100.0],
            vec![
                ("node_b".to_owned(), [110.0, 20.0, 40.0, 30.0]),
                ("node-a".to_owned(), [10.0, 20.0, 40.0, 30.0]),
            ],
        )
        .expect("bounded geometry");

        assert_eq!(geometry.view_box.width, 200.0);
        assert_eq!(
            geometry
                .nodes
                .iter()
                .map(|node| node.id.as_str())
                .collect::<Vec<_>>(),
            ["node-a", "node_b"]
        );
        assert_eq!(geometry.nodes[0].rect.x, 10.0);
    }

    #[test]
    fn refuses_unknown_duplicate_and_missing_nodes() {
        let view_box = [0.0, 0.0, 200.0, 100.0];
        let rect = [10.0, 20.0, 40.0, 30.0];
        assert!(
            admit(
                &expected(),
                view_box,
                vec![("node-a".to_owned(), rect), ("other".to_owned(), rect)]
            )
            .is_err()
        );
        assert!(
            admit(
                &expected(),
                view_box,
                vec![("node-a".to_owned(), rect), ("node-a".to_owned(), rect),],
            )
            .is_err()
        );
        assert!(admit(&expected(), view_box, vec![("node-a".to_owned(), rect)]).is_err());
    }

    #[test]
    fn refuses_non_finite_oversized_and_out_of_view_geometry() {
        assert!(admit(&[], [0.0, 0.0, f64::INFINITY, 1.0], Vec::new()).is_err());
        assert!(
            admit(
                &["node-a".to_owned()],
                [0.0, 0.0, 100.0, 100.0],
                vec![("node-a".to_owned(), [90.0, 10.0, 20.0, 20.0])],
            )
            .is_err()
        );
        assert!(
            admit(
                &["node-a".to_owned()],
                [0.0, 0.0, 100.0, 100.0],
                vec![("node-a".to_owned(), [0.0, 0.0, 1_000_001.0, 1.0])],
            )
            .is_err()
        );
    }

    #[test]
    fn refuses_unbounded_or_ambiguous_caller_ids() {
        assert!(validate_ids(&vec!["node".to_owned(); NODE_LIMIT + 1]).is_err());
        assert!(validate_ids(&["node".to_owned(), "node".to_owned()]).is_err());
        assert!(validate_ids(&["1node".to_owned()]).is_err());
        assert!(validate_ids(&["node.dot".to_owned()]).is_err());
    }
}
