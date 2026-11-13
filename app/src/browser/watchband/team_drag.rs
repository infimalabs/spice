use super::*;

const TEAM_SURFACE_SELECTOR: &str = ".team-surface[data-team]";
const TEAM_GHOST_SELECTOR: &str = ".team-drag-ghost";
const TEAM_GHOST_MAX_WIDTH_PX: f64 = 480.0;
const TEAM_GHOST_MAX_HEIGHT_PX: f64 = 360.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JoinEdge {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TeamPreview {
    target: CanonicalUuid,
    drop: watchband::TeamDrop,
    edge: Option<JoinEdge>,
    admitted: bool,
}

#[derive(Clone, Default)]
pub(in crate::browser) struct TeamDragState(
    std::rc::Rc<std::cell::RefCell<watchband::PointerDragSession>>,
    std::rc::Rc<std::cell::Cell<Option<TeamPreview>>>,
);

impl TeamDragState {
    fn clear(&self) {
        self.0.borrow_mut().clear();
        self.1.set(None);
        clear_team_drag_presentation();
    }

    fn restore_presentation(&self) {
        let Some(source) = self.0.borrow().dragging_source() else {
            return;
        };
        let Some(document) = team_document() else {
            return;
        };
        let Some(source_element) = team_surface(&document, source) else {
            return;
        };
        let _ = source_element.set_attribute("data-team-drag-source", "true");
        paint_team_preview(&document, source, self.1.get());
    }
}

#[hook]
pub(in crate::browser) fn use_team_drag_state(
    enabled: bool,
    topology_revision: Option<u64>,
) -> TeamDragState {
    let state = use_memo((), |()| TeamDragState::default());
    use_effect_with((enabled, topology_revision), {
        let state = state.clone();
        move |(enabled, topology_revision)| {
            if state.0.borrow_mut().reconcile(*enabled, *topology_revision) {
                state.1.set(None);
                clear_team_drag_presentation();
            }
        }
    });
    {
        let state = state.clone();
        use_effect(move || {
            let source_removed = state.0.borrow().source().is_some_and(|source| {
                team_document()
                    .as_ref()
                    .and_then(|document| team_surface(document, source))
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

fn team_document() -> Option<web_sys::Document> {
    web_sys::window()?.document()
}

fn team_surface(document: &web_sys::Document, team: CanonicalUuid) -> Option<web_sys::Element> {
    document
        .query_selector(&format!("{TEAM_SURFACE_SELECTOR}[data-team=\"{team}\"]"))
        .ok()
        .flatten()
}

fn team_elements(document: &web_sys::Document) -> Option<web_sys::NodeList> {
    document.query_selector_all(TEAM_SURFACE_SELECTOR).ok()
}

fn clear_team_preview(document: &web_sys::Document) {
    let Some(nodes) = team_elements(document) else {
        return;
    };
    for index in 0..nodes.length() {
        let Some(element) = nodes
            .item(index)
            .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
        else {
            continue;
        };
        let _ = element.remove_attribute("data-team-drop-target");
        let _ = element.remove_attribute("data-team-drop-blocked");
    }
}

fn clear_team_drag_presentation() {
    let Some(document) = team_document() else {
        return;
    };
    clear_team_preview(&document);
    if let Some(nodes) = team_elements(&document) {
        for index in 0..nodes.length() {
            if let Some(element) = nodes
                .item(index)
                .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
            {
                let _ = element.remove_attribute("data-team-drag-source");
            }
        }
    }
    if let Ok(nodes) = document.query_selector_all(TEAM_GHOST_SELECTOR) {
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

fn begin_team_drag_presentation(team: CanonicalUuid, client_x: i32, client_y: i32) -> Option<()> {
    clear_team_drag_presentation();
    let document = team_document()?;
    let source = team_surface(&document, team)?;
    let rect = source.get_bounding_client_rect();
    source.set_attribute("data-team-drag-source", "true").ok()?;
    let ghost = source
        .clone_node_with_deep(true)
        .ok()?
        .dyn_into::<web_sys::HtmlElement>()
        .ok()?;
    ghost.set_class_name(&format!("{} team-drag-ghost", ghost.class_name()));
    for attribute in [
        "data-team",
        "data-team-drag-source",
        "data-team-drop-target",
        "data-team-drop-blocked",
    ] {
        let _ = ghost.remove_attribute(attribute);
    }
    let width = rect.width().clamp(1.0, TEAM_GHOST_MAX_WIDTH_PX);
    let height = rect.height().clamp(1.0, TEAM_GHOST_MAX_HEIGHT_PX);
    let _ = ghost.set_attribute("aria-hidden", "true");
    let _ = ghost.set_attribute("inert", "");
    let _ = ghost.set_attribute(
        "data-team-drag-offset-x",
        &(f64::from(client_x) - rect.left())
            .clamp(0.0, width)
            .to_string(),
    );
    let _ = ghost.set_attribute(
        "data-team-drag-offset-y",
        &(f64::from(client_y) - rect.top())
            .clamp(0.0, height)
            .to_string(),
    );
    let _ = ghost.style().set_property("width", &format!("{width}px"));
    let _ = ghost.style().set_property("height", &format!("{height}px"));
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
    update_team_drag_ghost(client_x, client_y);
    Some(())
}

fn update_team_drag_ghost(client_x: i32, client_y: i32) {
    let Some(ghost) = team_document()
        .and_then(|document| document.query_selector(TEAM_GHOST_SELECTOR).ok().flatten())
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return;
    };
    let offset_x = ghost
        .get_attribute("data-team-drag-offset-x")
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or_default();
    let offset_y = ghost
        .get_attribute("data-team-drag-offset-y")
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or_default();
    let _ = ghost
        .style()
        .set_property("left", &format!("{}px", f64::from(client_x) - offset_x));
    let _ = ghost
        .style()
        .set_property("top", &format!("{}px", f64::from(client_y) - offset_y));
}

fn team_preview_at(
    source: CanonicalUuid,
    topology: &[(CanonicalUuid, Vec<CanonicalUuid>)],
    client_x: i32,
    client_y: i32,
) -> Option<TeamPreview> {
    let document = team_document()?;
    let surface = document
        .element_from_point(client_x as f32, client_y as f32)?
        .closest(TEAM_SURFACE_SELECTOR)
        .ok()
        .flatten()?;
    let target = surface.get_attribute("data-team")?.parse().ok()?;
    if target == source {
        return None;
    }
    let rect = surface.get_bounding_client_rect();
    let offset = f64::from(client_x) - rect.left();
    let drop = watchband::TeamDrop::at(offset, rect.width());
    let edge = (drop == watchband::TeamDrop::Join).then(|| {
        if offset <= rect.width() / 2.0 {
            JoinEdge::Left
        } else {
            JoinEdge::Right
        }
    });
    Some(TeamPreview {
        target,
        drop,
        edge,
        admitted: watchband::arrangement(topology, source, target, drop).is_some(),
    })
}

fn preview_label(preview: TeamPreview) -> &'static str {
    match (preview.drop, preview.edge) {
        (watchband::TeamDrop::Swap, _) => "swap",
        (watchband::TeamDrop::Join, Some(JoinEdge::Left)) => "join-left",
        (watchband::TeamDrop::Join, Some(JoinEdge::Right)) => "join-right",
        (watchband::TeamDrop::Join, None) => "join",
    }
}

fn paint_team_preview(
    document: &web_sys::Document,
    source: CanonicalUuid,
    preview: Option<TeamPreview>,
) {
    clear_team_preview(document);
    let Some(source_element) = team_surface(document, source) else {
        return;
    };
    let _ = source_element.set_attribute("data-team-drag-source", "true");
    let Some(preview) = preview else {
        return;
    };
    let Some(target) = team_surface(document, preview.target) else {
        return;
    };
    let attribute = if preview.admitted {
        "data-team-drop-target"
    } else {
        "data-team-drop-blocked"
    };
    let _ = target.set_attribute(attribute, preview_label(preview));
}

fn set_team_preview(source: CanonicalUuid, preview: Option<TeamPreview>, state: &TeamDragState) {
    state.1.set(preview);
    if let Some(document) = team_document() {
        paint_team_preview(&document, source, preview);
    }
}

fn release_team_pointer(event: &PointerEvent) {
    if let Some(element) = event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        .and_then(|element| element.closest("[data-team-drag-id]").ok().flatten())
    {
        let _ = element.release_pointer_capture(event.pointer_id());
    }
}

fn team_drag_start(
    team: CanonicalUuid,
    enabled: bool,
    topology_revision: u64,
    state: &TeamDragState,
) -> Callback<PointerEvent> {
    let state = state.clone();
    Callback::from(move |event: PointerEvent| {
        if event.button() != 0
            || !state.0.borrow_mut().begin(
                enabled,
                team,
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
            .and_then(|element| element.closest("[data-team-drag-id]").ok().flatten())
        else {
            state.clear();
            return;
        };
        if element.set_pointer_capture(event.pointer_id()).is_err() {
            state.clear();
            return;
        }
        clear_team_drag_presentation();
        state.1.set(None);
        event.prevent_default();
        event.stop_propagation();
    })
}

fn team_drag_movement(
    team: CanonicalUuid,
    topology_revision: u64,
    topology: Rc<Vec<(CanonicalUuid, Vec<CanonicalUuid>)>>,
    state: &TeamDragState,
) -> Callback<PointerEvent> {
    let state = state.clone();
    Callback::from(move |event: PointerEvent| {
        let lift = {
            let mut session = state.0.borrow_mut();
            let Some(active) = session.movement(
                &team,
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
        if lift && begin_team_drag_presentation(team, event.client_x(), event.client_y()).is_none()
        {
            state.clear();
            return;
        }
        update_team_drag_ghost(event.client_x(), event.client_y());
        let preview = team_preview_at(team, &topology, event.client_x(), event.client_y());
        set_team_preview(team, preview, &state);
        event.prevent_default();
        event.stop_propagation();
    })
}

fn team_drag_finish(
    team: CanonicalUuid,
    topology_revision: u64,
    topology: Rc<Vec<(CanonicalUuid, Vec<CanonicalUuid>)>>,
    sender: SenderHandle,
    state: &TeamDragState,
) -> Callback<PointerEvent> {
    let state = state.clone();
    Callback::from(move |event: PointerEvent| {
        if state.0.borrow().dragging_source() == Some(team) {
            let preview = team_preview_at(team, &topology, event.client_x(), event.client_y());
            set_team_preview(team, preview, &state);
        }
        let Some(active) =
            state
                .0
                .borrow_mut()
                .finish(&team, event.pointer_id(), topology_revision)
        else {
            return;
        };
        release_team_pointer(&event);
        let arranged = active
            .dragging
            .then(|| state.1.get())
            .flatten()
            .filter(|preview| preview.admitted)
            .and_then(|preview| {
                watchband::arrangement(&topology, team, preview.target, preview.drop)
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

fn team_drag_cancel(team: CanonicalUuid, state: &TeamDragState) -> Callback<PointerEvent> {
    let state = state.clone();
    Callback::from(move |event: PointerEvent| {
        if state.0.borrow_mut().cancel(&team, event.pointer_id()) {
            release_team_pointer(&event);
            state.clear();
            event.prevent_default();
            event.stop_propagation();
        }
    })
}

pub(super) struct TeamDragging {
    pub(super) start: Callback<PointerEvent>,
    pub(super) movement: Callback<PointerEvent>,
    pub(super) finish: Callback<PointerEvent>,
    pub(super) cancel: Callback<PointerEvent>,
}

pub(super) fn team_dragging(
    team: CanonicalUuid,
    enabled: bool,
    topology_revision: u64,
    topology: &Rc<Vec<(CanonicalUuid, Vec<CanonicalUuid>)>>,
    sender: &SenderHandle,
    state: &TeamDragState,
) -> TeamDragging {
    let cancel = team_drag_cancel(team, state);
    let topology = Rc::clone(topology);
    TeamDragging {
        start: team_drag_start(team, enabled, topology_revision, state),
        movement: team_drag_movement(team, topology_revision, Rc::clone(&topology), state),
        finish: team_drag_finish(team, topology_revision, topology, sender.clone(), state),
        cancel,
    }
}
