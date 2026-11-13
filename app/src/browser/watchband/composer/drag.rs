use super::*;

/// Where a lifted primary composer would land if released now.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ComposerDrop {
    /// Exchange slots with this sibling composer in the same team.
    Swap(CanonicalUuid),
    /// Move the agent to the end of this other team.
    Transfer(CanonicalUuid),
}

#[derive(Clone, Default)]
pub(in crate::browser) struct PrimaryComposerDragState(
    std::rc::Rc<std::cell::RefCell<watchband::PointerDragSession>>,
    std::rc::Rc<std::cell::Cell<Option<ComposerDrop>>>,
);

impl PrimaryComposerDragState {
    fn clear(&self) {
        self.0.borrow_mut().clear();
        self.1.set(None);
        clear_composer_drag_presentation();
    }

    fn clear_preview(&self) {
        self.1.set(None);
        if let Some(document) = composer_document() {
            clear_composer_swap_preview(&document);
        }
    }

    fn restore_presentation(&self) {
        let Some(source) = self.0.borrow().dragging_source() else {
            return;
        };
        let Some(document) = composer_document() else {
            return;
        };
        let Some(source_element) = composer_drag_element(&document, source) else {
            return;
        };
        let _ = source_element.set_attribute("data-composer-drag-source", "true");
        if let Some(drop) = self.1.get() {
            apply_composer_drop(source, drop);
        }
    }
}

#[hook]
pub(in crate::browser) fn use_primary_composer_drag_state(
    enabled: bool,
    topology_revision: Option<u64>,
) -> PrimaryComposerDragState {
    let state = use_memo((), |()| PrimaryComposerDragState::default());
    use_effect_with((enabled, topology_revision), {
        let state = state.clone();
        move |(enabled, topology_revision)| {
            if state.0.borrow_mut().reconcile(*enabled, *topology_revision) {
                state.1.set(None);
                clear_composer_drag_presentation();
            }
        }
    });
    {
        let state = state.clone();
        use_effect(move || {
            let source_removed = state.0.borrow().source().is_some_and(|source| {
                composer_document()
                    .as_ref()
                    .and_then(|document| composer_drag_element(document, source))
                    .is_none()
            });
            if source_removed {
                state.clear();
            } else {
                state.restore_presentation();
            }
        });
    }
    use_effect_with((), {
        let state = state.clone();
        move |()| move || state.clear()
    });
    (*state).clone()
}

fn composer_document() -> Option<web_sys::Document> {
    web_sys::window()?.document()
}

fn composer_drag_element(
    document: &web_sys::Document,
    lane: CanonicalUuid,
) -> Option<web_sys::Element> {
    document
        .query_selector(&format!(
            ".team-agents > .team-agent[data-agent-lane=\"{lane}\"]"
        ))
        .ok()
        .flatten()
}

fn clear_composer_swap_preview(document: &web_sys::Document) {
    let Ok(nodes) = document.query_selector_all(".team-agents > .team-agent") else {
        return;
    };
    for index in 0..nodes.length() {
        let Some(element) = nodes
            .item(index)
            .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
        else {
            continue;
        };
        let _ = element.remove_attribute("data-composer-drag-target");
        if let Ok(element) = element.dyn_into::<web_sys::HtmlElement>() {
            let _ = element.style().remove_property("transform");
        }
    }
    if let Ok(nodes) = document.query_selector_all("[data-composer-transfer-target]") {
        for index in 0..nodes.length() {
            if let Some(element) = nodes
                .item(index)
                .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
            {
                let _ = element.remove_attribute("data-composer-transfer-target");
            }
        }
    }
}

fn clear_composer_drag_presentation() {
    let Some(document) = composer_document() else {
        return;
    };
    clear_composer_swap_preview(&document);
    if let Ok(nodes) = document.query_selector_all(".team-agents > .team-agent") {
        for index in 0..nodes.length() {
            let Some(element) = nodes
                .item(index)
                .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
            else {
                continue;
            };
            let _ = element.remove_attribute("data-composer-drag-source");
        }
    }
    if let Ok(nodes) = document.query_selector_all(".composer-drag-ghost") {
        for index in 0..nodes.length() {
            if let Some(element) = nodes
                .item(index)
                .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
            {
                element.remove();
            }
        }
    }
}

fn update_composer_drag_ghost(client_x: i32, client_y: i32) {
    let Some(ghost) = composer_document()
        .and_then(|document| {
            document
                .query_selector(".composer-drag-ghost")
                .ok()
                .flatten()
        })
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return;
    };
    let offset_x = ghost
        .get_attribute("data-composer-drag-offset-x")
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or_default();
    let offset_y = ghost
        .get_attribute("data-composer-drag-offset-y")
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or_default();
    let _ = ghost
        .style()
        .set_property("left", &format!("{}px", f64::from(client_x) - offset_x));
    let _ = ghost
        .style()
        .set_property("top", &format!("{}px", f64::from(client_y) - offset_y));
}

fn begin_composer_drag_presentation(
    lane: CanonicalUuid,
    client_x: i32,
    client_y: i32,
) -> Option<()> {
    clear_composer_drag_presentation();
    let document = composer_document()?;
    let source = composer_drag_element(&document, lane)?;
    let rect = source.get_bounding_client_rect();
    source
        .set_attribute("data-composer-drag-source", "true")
        .ok()?;

    let ghost = source
        .clone_node_with_deep(true)
        .ok()?
        .dyn_into::<web_sys::HtmlElement>()
        .ok()?;
    ghost.set_class_name(&format!("{} composer-drag-ghost", ghost.class_name()));
    for attribute in ["data-composer-drag-source", "data-composer-drag-target"] {
        let _ = ghost.remove_attribute(attribute);
    }
    let _ = ghost.set_attribute("aria-hidden", "true");
    let _ = ghost.set_attribute("inert", "");
    let _ = ghost.set_attribute(
        "data-composer-drag-offset-x",
        &(f64::from(client_x) - rect.left()).to_string(),
    );
    let _ = ghost.set_attribute(
        "data-composer-drag-offset-y",
        &(f64::from(client_y) - rect.top()).to_string(),
    );
    let _ = ghost
        .style()
        .set_property("width", &format!("{}px", rect.width()));
    let _ = ghost
        .style()
        .set_property("height", &format!("{}px", rect.height()));
    if let Ok(ids) = ghost.query_selector_all("[id]") {
        for index in 0..ids.length() {
            if let Some(element) = ids
                .item(index)
                .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
            {
                let _ = element.remove_attribute("id");
            }
        }
    }
    document.body()?.append_child(&ghost).ok()?;
    update_composer_drag_ghost(client_x, client_y);
    Some(())
}

fn apply_composer_drop(source: CanonicalUuid, drop: ComposerDrop) -> bool {
    let Some(document) = composer_document() else {
        return false;
    };
    clear_composer_swap_preview(&document);
    let target = match drop {
        ComposerDrop::Swap(target) if target != source => target,
        ComposerDrop::Swap(_) => return false,
        ComposerDrop::Transfer(team) => {
            return team_surface(&document, team).is_some_and(|surface| {
                surface
                    .set_attribute("data-composer-transfer-target", "true")
                    .is_ok()
            });
        }
    };
    let Some(source) = composer_drag_element(&document, source)
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return false;
    };
    let Some(target) = composer_drag_element(&document, target)
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return false;
    };
    let (Some(source_rect), Some(target_rect)) = (layout_rect(&source), layout_rect(&target))
    else {
        return false;
    };
    let offset = target_rect.left - source_rect.left;
    let _ = source
        .style()
        .set_property("transform", &format!("translateX({offset}px)"));
    let _ = target
        .style()
        .set_property("transform", &format!("translateX({}px)", -offset));
    let _ = target.set_attribute("data-composer-drag-target", "true");
    true
}

fn preview_composer_drop(
    source: CanonicalUuid,
    drop: Option<ComposerDrop>,
    state: &PrimaryComposerDragState,
) {
    // Reapplying the drop already shown would restart its transition from
    // wherever the animation stands, so an unchanged drop leaves it alone.
    if drop.is_some() && drop == state.1.get() {
        return;
    }
    if drop.is_some_and(|drop| apply_composer_drop(source, drop)) {
        state.1.set(drop);
    } else {
        state.clear_preview();
    }
}

/// Client-space box of an element's layout slot, without the translation a
/// swap preview (or its running transition) has moved it by.
///
/// Hit-testing the moved box would read the preview back as input: the target
/// slides out from under the pointer, the drop flips, the preview clears, and
/// the target slides back, forever.
struct LayoutRect {
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
}

fn layout_rect(element: &web_sys::Element) -> Option<LayoutRect> {
    let rect = element.get_bounding_client_rect();
    let transform = web_sys::window()?
        .get_computed_style(element)
        .ok()??
        .get_property_value("transform")
        .ok()?;
    let (dx, dy) = computed_translation(&transform)?;
    Some(LayoutRect {
        left: rect.left() - dx,
        right: rect.right() - dx,
        top: rect.top() - dy,
        bottom: rect.bottom() - dy,
    })
}

/// Translation carried by a computed `transform` value: `none`,
/// `matrix(a, b, c, d, e, f)`, or `matrix3d(...)` with its offsets at 13 and 14.
fn computed_translation(transform: &str) -> Option<(f64, f64)> {
    if transform == "none" {
        return Some((0.0, 0.0));
    }
    let (values, x, y) = if let Some(values) = transform.strip_prefix("matrix3d(") {
        (values, 12, 13)
    } else {
        (transform.strip_prefix("matrix(")?, 4, 5)
    };
    let values = values
        .strip_suffix(')')?
        .split(',')
        .map(|value| value.trim().parse::<f64>().ok())
        .collect::<Option<Vec<_>>>()?;
    Some((*values.get(x)?, *values.get(y)?))
}

fn team_surface(document: &web_sys::Document, team: CanonicalUuid) -> Option<web_sys::Element> {
    document
        .query_selector(&format!(
            ".team-fleet > .team-surface[data-team=\"{team}\"]"
        ))
        .ok()
        .flatten()
}

/// The identity carried by the first element under the point, read from `attribute`.
fn identity_at_point(selector: &str, attribute: &str, x: f64, y: f64) -> Option<CanonicalUuid> {
    let nodes = composer_document()?.query_selector_all(selector).ok()?;
    (0..nodes.length())
        .filter_map(|index| {
            nodes
                .item(index)
                .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
        })
        .find(|element| {
            layout_rect(element).is_some_and(|rect| {
                x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom
            })
        })?
        .get_attribute(attribute)?
        .parse()
        .ok()
}

/// Read the drop a lifted composer states at one pointer position.
///
/// A sibling composer in the same team swaps; anywhere on another team's
/// surface transfers the agent there. Everything else states nothing.
fn composer_drop_at(
    source_team: Option<CanonicalUuid>,
    members: &[CanonicalUuid],
    client_x: i32,
    client_y: i32,
) -> Option<ComposerDrop> {
    let (x, y) = (f64::from(client_x), f64::from(client_y));
    let team = identity_at_point(".team-fleet > .team-surface[data-team]", "data-team", x, y)?;
    if Some(team) != source_team {
        return Some(ComposerDrop::Transfer(team));
    }
    identity_at_point(".team-agents > .team-agent", "data-agent-lane", x, y)
        .filter(|lane| members.contains(lane))
        .map(ComposerDrop::Swap)
}

fn release_composer_pointer(event: &PointerEvent) {
    if let Some(element) = event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    {
        let _ = element.release_pointer_capture(event.pointer_id());
    }
}

fn composer_drag_start(
    lane: CanonicalUuid,
    enabled: bool,
    topology_revision: u64,
    state: &PrimaryComposerDragState,
) -> Callback<PointerEvent> {
    let state = state.clone();
    Callback::from(move |event: PointerEvent| {
        if event.button() != 0
            || !state.0.borrow_mut().begin(
                enabled,
                lane,
                event.pointer_id(),
                event.client_x(),
                event.client_y(),
                topology_revision,
            )
        {
            return;
        }
        let Some(element) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        else {
            state.clear();
            return;
        };
        if element.set_pointer_capture(event.pointer_id()).is_err() {
            state.clear();
            return;
        }
        clear_composer_drag_presentation();
        state.1.set(None);
        event.prevent_default();
        event.stop_propagation();
    })
}

fn composer_drag_movement(
    lane: CanonicalUuid,
    topology_revision: u64,
    team: Option<CanonicalUuid>,
    members: Vec<CanonicalUuid>,
    state: &PrimaryComposerDragState,
) -> Callback<PointerEvent> {
    let state = state.clone();
    Callback::from(move |event: PointerEvent| {
        let lift = {
            let mut session = state.0.borrow_mut();
            let Some(active) = session.movement(
                &lane,
                event.pointer_id(),
                event.client_x(),
                event.client_y(),
                topology_revision,
            ) else {
                return;
            };
            let lift = !active.dragging;
            active.dragging = true;
            lift
        };
        if lift
            && begin_composer_drag_presentation(lane, event.client_x(), event.client_y()).is_none()
        {
            state.clear();
            return;
        }
        update_composer_drag_ghost(event.client_x(), event.client_y());
        let drop = composer_drop_at(team, &members, event.client_x(), event.client_y());
        preview_composer_drop(lane, drop, &state);
        event.prevent_default();
        event.stop_propagation();
    })
}

fn composer_drag_finish(
    lane: CanonicalUuid,
    topology_revision: u64,
    topology: Vec<(CanonicalUuid, Vec<CanonicalUuid>)>,
    sender: SenderHandle,
    state: &PrimaryComposerDragState,
) -> Callback<PointerEvent> {
    let state = state.clone();
    Callback::from(move |event: PointerEvent| {
        let Some(active) =
            state
                .0
                .borrow_mut()
                .finish(&lane, event.pointer_id(), topology_revision)
        else {
            return;
        };
        release_composer_pointer(&event);
        let arranged =
            active
                .dragging
                .then(|| state.1.get())
                .flatten()
                .and_then(|drop| match drop {
                    ComposerDrop::Swap(target) => watchband::swap_members(&topology, lane, target),
                    ComposerDrop::Transfer(team) => {
                        watchband::transfer_member(&topology, lane, team)
                    }
                });
        state.clear();
        let Some(teams) = arranged else {
            return;
        };
        let _ = admit_action(
            &sender,
            ClientAction::ArrangeTeams {
                topology_revision,
                teams: teams
                    .into_iter()
                    .map(|(team, lanes)| TeamArrangement { team, lanes })
                    .collect(),
                respond: None,
            },
        );
        event.prevent_default();
        event.stop_propagation();
    })
}

fn composer_drag_cancel(
    lane: CanonicalUuid,
    state: &PrimaryComposerDragState,
) -> Callback<PointerEvent> {
    let state = state.clone();
    Callback::from(move |event: PointerEvent| {
        if state.0.borrow_mut().cancel(&lane, event.pointer_id()) {
            release_composer_pointer(&event);
            state.clear();
        }
    })
}

pub(in crate::browser::watchband) fn primary_composer_dragging(
    lane: CanonicalUuid,
    enabled: bool,
    topology_revision: u64,
    topology: &[(CanonicalUuid, Vec<CanonicalUuid>)],
    sender: &SenderHandle,
    state: &PrimaryComposerDragState,
) -> PrimaryComposerDragging {
    let (team, members) = topology
        .iter()
        .find(|(_, members)| members.contains(&lane))
        .map_or((None, Vec::new()), |(team, members)| {
            (Some(*team), members.clone())
        });
    let cancel = composer_drag_cancel(lane, state);
    PrimaryComposerDragging {
        enabled,
        start: composer_drag_start(lane, enabled, topology_revision, state),
        movement: composer_drag_movement(lane, topology_revision, team, members, state),
        finish: composer_drag_finish(
            lane,
            topology_revision,
            topology.to_vec(),
            sender.clone(),
            state,
        ),
        cancel: cancel.clone(),
        lost_capture: cancel,
    }
}
