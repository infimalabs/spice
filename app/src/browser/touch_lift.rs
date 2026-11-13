//! Finger and pen lift presentation shared by the global menu's draggable
//! rows: roster lanes and Design tasks. A mouse uses HTML5 drag and drop.

use wasm_bindgen::JsCast;

/// Marks the lifted row while its ghost follows the contact.
const SOURCE: &str = "data-topology-drag-source";
/// Marks the drop target a release would land on.
const TARGET: &str = "data-topology-drop-target";
/// The fixed-position copy of the lifted row.
const GHOST: &str = "app-topology-drag-ghost";
/// How near a scrolling container's visible edge a lifted finger scrolls it.
const SCROLL_EDGE_PX: f64 = 48.0;
/// Each move scrolls this fraction of the finger's depth into the edge band, so
/// pressing deeper scrolls faster without outrunning the target.
const SCROLL_DEPTH_RATE: f64 = 0.25;

/// Lift the row `handle` sits in: dim it and float an inert ghost under the
/// contact. `identity` names the attribute that makes the row a drag source,
/// which the ghost must not carry.
pub(super) fn begin(
    handle: &web_sys::Element,
    row: &str,
    identity: &str,
    client_x: i32,
    client_y: i32,
) -> Option<()> {
    clear();
    let source = handle.closest(row).ok()??;
    let rect = source.get_bounding_client_rect();
    source.set_attribute(SOURCE, "true").ok()?;
    let ghost = source
        .clone_node_with_deep(true)
        .ok()?
        .dyn_into::<web_sys::HtmlElement>()
        .ok()?;
    ghost.set_class_name(&format!("{} {GHOST}", source.class_name()));
    for attribute in [SOURCE, "draggable", identity] {
        let _ = ghost.remove_attribute(attribute);
    }
    let _ = ghost.set_attribute("aria-hidden", "true");
    let _ = ghost.set_attribute("inert", "");
    let _ = ghost.set_attribute(
        "data-touch-offset-x",
        &(f64::from(client_x) - rect.left()).to_string(),
    );
    let _ = ghost.set_attribute(
        "data-touch-offset-y",
        &(f64::from(client_y) - rect.top()).to_string(),
    );
    let _ = ghost
        .style()
        .set_property("width", &format!("{}px", rect.width()));
    web_sys::window()?
        .document()?
        .body()?
        .append_child(&ghost)
        .ok()?;
    Some(())
}

/// Move the ghost under the contact, scrolling the sheet near its edges.
pub(super) fn follow(handle: &web_sys::Element, client_x: i32, client_y: i32) {
    scroll_toward_edge(handle, client_y);
    let Some(ghost) = document()
        .and_then(|document| document.query_selector(&format!(".{GHOST}")).ok().flatten())
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return;
    };
    let offset = |name| {
        ghost
            .get_attribute(name)
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or_default()
    };
    let left = f64::from(client_x) - offset("data-touch-offset-x");
    let top = f64::from(client_y) - offset("data-touch-offset-y");
    let _ = ghost.style().set_property("left", &format!("{left}px"));
    let _ = ghost.style().set_property("top", &format!("{top}px"));
}

/// The innermost element carrying `attribute` under the contact, when it
/// accepts drops; a disabled target under the contact accepts nothing.
pub(super) fn target_at(attribute: &str, client_x: i32, client_y: i32) -> Option<web_sys::Element> {
    let (x, y) = (f64::from(client_x), f64::from(client_y));
    let targets = document()?
        .query_selector_all(&format!("[{attribute}]"))
        .ok()?;
    // Document order lists an ancestor before its descendants, so the first
    // hit from the end is the innermost.
    let target = (0..targets.length())
        .rev()
        .filter_map(|index| targets.item(index)?.dyn_into::<web_sys::Element>().ok())
        .find(|element| {
            let rect = element.get_bounding_client_rect();
            x >= rect.left() && x <= rect.right() && y >= rect.top() && y <= rect.bottom()
        })?;
    (target.get_attribute("data-drop-enabled").as_deref() == Some("true")).then_some(target)
}

/// Outline the target a release would land on, or none.
pub(super) fn preview(target: Option<&web_sys::Element>) {
    remove_attribute(TARGET);
    if let Some(target) = target {
        let _ = target.set_attribute(TARGET, "true");
    }
}

/// End every lift presentation: the dimmed source, the outlined target and the
/// ghost.
pub(super) fn clear() {
    remove_attribute(SOURCE);
    remove_attribute(TARGET);
    if let Some(document) = document()
        && let Ok(ghosts) = document.query_selector_all(&format!(".{GHOST}"))
    {
        for index in 0..ghosts.length() {
            if let Some(ghost) = ghosts
                .item(index)
                .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
            {
                ghost.remove();
            }
        }
    }
}

fn document() -> Option<web_sys::Document> {
    web_sys::window()?.document()
}

/// Scroll the nearest scrolling ancestor while the finger rests near its
/// visible edge, so a target outside the visible sheet stays reachable.
fn scroll_toward_edge(handle: &web_sys::Element, client_y: i32) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let mut ancestor = handle.parent_element();
    while let Some(element) = ancestor {
        let scrolls = element.scroll_height() > element.client_height()
            && window
                .get_computed_style(&element)
                .ok()
                .flatten()
                .and_then(|style| style.get_property_value("overflow-y").ok())
                .is_some_and(|overflow| overflow == "auto" || overflow == "scroll");
        if scrolls {
            let rect = element.get_bounding_client_rect();
            let viewport = window
                .inner_height()
                .ok()
                .and_then(|height| height.as_f64())
                .unwrap_or(rect.bottom());
            let top = rect.top().max(0.0) + SCROLL_EDGE_PX;
            let bottom = rect.bottom().min(viewport) - SCROLL_EDGE_PX;
            let y = f64::from(client_y);
            let depth = if y < top {
                y - top
            } else if y > bottom {
                y - bottom
            } else {
                return;
            };
            element.scroll_by_with_x_and_y(0.0, depth * SCROLL_DEPTH_RATE);
            return;
        }
        ancestor = element.parent_element();
    }
}

fn remove_attribute(attribute: &str) {
    let Some(document) = document() else {
        return;
    };
    let Ok(nodes) = document.query_selector_all(&format!("[{attribute}]")) else {
        return;
    };
    for index in 0..nodes.length() {
        if let Some(element) = nodes
            .item(index)
            .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
        {
            let _ = element.remove_attribute(attribute);
        }
    }
}
