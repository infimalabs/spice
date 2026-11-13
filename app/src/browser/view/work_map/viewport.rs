//! The graph frame is manipulated directly. The diagram is drawn at a contain
//! fit that never enlarges; zoom alone grows it inside a scrollable stage.
//! Wheel and trackpad input stays native while the graph has range, then its
//! unconsumed vertical distance moves the side view even in browsers that keep
//! a wheel transaction pinned to its first scroller. Pinch, ctrl/cmd-wheel and
//! `+`/`-` zoom about a point; mouse drag pans; double-click or `0` fits.
//! Zoom writes custom properties synchronously, so neither the image nor its
//! node controls wait on a render.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use yew::prelude::*;

use wire::TaskStamp;

use super::overlay::{self, MapNodeTarget};
use crate::browser::mermaid::MermaidImage;
use crate::browser::navigation::presentation;
use crate::mermaid_geometry::Rect;

/// Largest drawn size relative to the diagram's natural size.
const MAX_NATURAL_SCALE: f64 = 3.0;
const KEY_ZOOM: f64 = 1.25;
const DRAG_THRESHOLD: f64 = 4.0;
/// Ctrl-wheel pixels per e-fold of zoom. A trackpad pinch streams small
/// deltas; one mouse notch is capped so it steps rather than leaps.
const WHEEL_ZOOM_RATE: f64 = 200.0;
const WHEEL_ZOOM_DELTA_LIMIT: f64 = 50.0;
const LINE_PIXELS: f64 = 16.0;
/// Frame pixels kept clear around the drawing at every zoom, so outer nodes
/// and the state badges straddling their top edges never touch the bounds.
const MARGIN: f64 = 16.0;

/// The zoom and diagram bounds one frame's gestures and node overlay share.
pub(super) struct Canvas {
    viewport: NodeRef,
    view: Cell<Option<Rect>>,
    zoom: Cell<f64>,
}

impl PartialEq for Canvas {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

#[derive(Clone, Copy)]
struct Frame {
    scale: f64,
    drawn: (f64, f64),
    stage: (f64, f64),
    offset: (f64, f64),
    max_zoom: f64,
}

/// Contain fit inside the margin, capped at natural size and enlarged by
/// `zoom` within its bounds; the stage covers the frame plus the margin and
/// the drawing sits centered in it.
fn frame(viewport: (f64, f64), view: Rect, zoom: f64) -> Option<Frame> {
    let room = (viewport.0 - 2.0 * MARGIN, viewport.1 - 2.0 * MARGIN);
    if room.0 <= 0.0 || room.1 <= 0.0 || view.width <= 0.0 || view.height <= 0.0 {
        return None;
    }
    let fit = (room.0 / view.width).min(room.1 / view.height).min(1.0);
    let max_zoom = (MAX_NATURAL_SCALE / fit).max(1.0);
    let scale = fit * zoom.clamp(1.0, max_zoom);
    let drawn = (view.width * scale, view.height * scale);
    let stage = (
        (drawn.0 + 2.0 * MARGIN).max(viewport.0),
        (drawn.1 + 2.0 * MARGIN).max(viewport.1),
    );
    Some(Frame {
        scale,
        drawn,
        stage,
        offset: ((stage.0 - drawn.0) / 2.0, (stage.1 - drawn.1) / 2.0),
        max_zoom,
    })
}

/// Block size the frame needs to show the whole drawing at its width-limited
/// fit, margin included. It depends on the frame's width alone, so sizing the
/// frame by it settles in one pass; a taller drawing scrolls inside the clamp.
fn content_block(inline: f64, view: Rect) -> Option<f64> {
    let room = inline - 2.0 * MARGIN;
    (room > 0.0 && view.width > 0.0 && view.height > 0.0)
        .then(|| view.height * (room / view.width).min(1.0) + 2.0 * MARGIN)
}

impl Canvas {
    /// The overlay reports the drawn diagram's bounds once its geometry lands.
    /// A returned-to entry's viewport applies with the geometry, when the frame
    /// can hold it, unless the reader's own motion since has cancelled it.
    pub(super) fn show(&self, view: Rect) {
        self.view.set(Some(view));
        if let Some(saved) = presentation::take(|pending| pending.graph.take()) {
            self.zoom.set(saved.zoom);
            self.apply();
            if let Some(element) = self.element() {
                element.set_scroll_left(saved.scroll.0.round() as i32);
                element.set_scroll_top(saved.scroll.1.round() as i32);
            }
            return;
        }
        self.apply();
    }

    fn element(&self) -> Option<web_sys::HtmlElement> {
        self.viewport.cast::<web_sys::HtmlElement>()
    }

    fn frame(&self, element: &web_sys::HtmlElement) -> Option<Frame> {
        frame(
            (
                f64::from(element.client_width()),
                f64::from(element.client_height()),
            ),
            self.view.get()?,
            self.zoom.get(),
        )
    }

    fn apply(&self) -> Option<Frame> {
        let element = self.element()?;
        let style = element.style();
        if let Some(block) = content_block(f64::from(element.client_width()), self.view.get()?) {
            style
                .set_property("--work-map-content-block", &format!("{block}px"))
                .expect("Work map writes --work-map-content-block");
        }
        let frame = self.frame(&element)?;
        self.zoom.set(self.zoom.get().clamp(1.0, frame.max_zoom));
        for (property, value) in [
            ("--work-map-scale", frame.scale.to_string()),
            ("--work-map-drawn-inline", format!("{}px", frame.drawn.0)),
            ("--work-map-drawn-block", format!("{}px", frame.drawn.1)),
            ("--work-map-stage-inline", format!("{}px", frame.stage.0)),
            ("--work-map-stage-block", format!("{}px", frame.stage.1)),
            ("--work-map-offset-inline", format!("{}px", frame.offset.0)),
            ("--work-map-offset-block", format!("{}px", frame.offset.1)),
        ] {
            style
                .set_property(property, &value)
                .unwrap_or_else(|_| panic!("Work map writes {property}"));
        }
        element
            .set_attribute(
                "data-work-map-zoomed",
                if self.zoom.get() > 1.0 {
                    "true"
                } else {
                    "false"
                },
            )
            .expect("the fixed Work map zoom attribute is valid");
        element
            .set_attribute(presentation::GRAPH_ZOOM, &self.zoom.get().to_string())
            .expect("the Work map zoom attribute is valid");
        Some(frame)
    }

    /// Zooms so the diagram point under `anchor` (frame-relative pixels) stays there.
    fn zoom_about(&self, zoom: f64, anchor: (f64, f64)) {
        let Some(element) = self.element() else {
            return;
        };
        let Some(before) = self.frame(&element) else {
            return;
        };
        let point = (
            (f64::from(element.scroll_left()) + anchor.0 - before.offset.0) / before.scale,
            (f64::from(element.scroll_top()) + anchor.1 - before.offset.1) / before.scale,
        );
        self.zoom.set(zoom.clamp(1.0, before.max_zoom));
        let Some(after) = self.apply() else {
            return;
        };
        element.set_scroll_left((after.offset.0 + point.0 * after.scale - anchor.0).round() as i32);
        element.set_scroll_top((after.offset.1 + point.1 * after.scale - anchor.1).round() as i32);
    }

    fn zoom_by(&self, factor: f64, anchor: (f64, f64)) {
        self.zoom_about(self.zoom.get() * factor, anchor);
    }

    fn fit(&self) {
        self.zoom.set(1.0);
        self.apply();
        if let Some(element) = self.element() {
            element.set_scroll_left(0);
            element.set_scroll_top(0);
        }
    }

    fn center(&self) -> (f64, f64) {
        self.element().map_or((0.0, 0.0), |element| {
            (
                f64::from(element.client_width()) / 2.0,
                f64::from(element.client_height()) / 2.0,
            )
        })
    }
}

#[derive(Properties, PartialEq)]
pub(super) struct WorkMapViewportProps {
    pub(super) source: AttrValue,
    pub(super) targets: Rc<Vec<MapNodeTarget>>,
    pub(super) select: Callback<TaskStamp>,
    pub(super) pending: bool,
    pub(super) retry: Option<Callback<MouseEvent>>,
}

#[function_component(WorkMapViewport)]
pub(super) fn work_map_viewport(props: &WorkMapViewportProps) -> Html {
    let viewport = use_node_ref();
    let canvas = use_memo((), {
        let viewport = viewport.clone();
        move |()| Canvas {
            viewport,
            view: Cell::new(None),
            zoom: Cell::new(1.0),
        }
    });
    use_effect_with(canvas.clone(), |canvas| {
        let gestures = Gestures::attach(canvas.clone());
        move || drop(gestures)
    });
    let node_ids = Rc::new(
        props
            .targets
            .iter()
            .map(|target| target.id.clone())
            .collect(),
    );
    let render_geometry = Some(overlay::renderer(
        props.targets.clone(),
        props.select.clone(),
        canvas,
    ));
    html! {
        <div class="work-map-canvas">
        <div
            ref={viewport}
            class="work-map-viewport"
            tabindex="0"
            role="group"
            aria-label="Task graph: scroll or drag to pan; pinch, Ctrl-scroll, + or − to zoom; double-click or 0 to fit"
            aria-busy={props.pending.then_some("true")}
        >
            <MermaidImage
                source={props.source.clone()}
                {node_ids}
                {render_geometry}
                final_answer={true}
                canvas={true}
            />
            if let Some(retry) = props.retry.clone() {
                <button type="button" class="app-compact-control work-map-retry" onclick={retry}>{"Retry enrichment"}</button>
            }
        </div>
            <crate::browser::code_copy::SourceCopy source={props.source.clone()} />
        </div>
    }
}

struct Drag {
    pointer: i32,
    origin: (f64, f64),
    scroll: (f64, f64),
    moving: bool,
}

struct Pinch {
    distance: f64,
    zoom: f64,
}

#[derive(Default)]
struct Gesture {
    drag: Option<Drag>,
    pinch: Option<Pinch>,
    safari_zoom: Option<f64>,
}

type Listener = Closure<dyn FnMut(web_sys::Event)>;
type Handler = fn(&Canvas, &RefCell<Gesture>, &web_sys::Event);

/// Native listeners: wheel and touch zoom must cancel the browser's own
/// handling, which Yew's passive delegated listeners cannot.
struct Gestures {
    element: web_sys::HtmlElement,
    listeners: Vec<(&'static str, Listener)>,
    observer: web_sys::ResizeObserver,
    _resized: Closure<dyn FnMut(js_sys::Array, web_sys::ResizeObserver)>,
}

impl Drop for Gestures {
    fn drop(&mut self) {
        self.observer.disconnect();
        for (event, listener) in &self.listeners {
            let _ = self
                .element
                .remove_event_listener_with_callback(event, listener.as_ref().unchecked_ref());
        }
    }
}

impl Gestures {
    fn attach(canvas: Rc<Canvas>) -> Option<Self> {
        let element = canvas.element()?;
        let gesture = Rc::new(RefCell::new(Gesture::default()));
        let handlers: [(&'static str, Handler); 13] = [
            ("wheel", wheel),
            ("gesturestart", safari_gesture),
            ("gesturechange", safari_gesture),
            ("gestureend", safari_gesture),
            ("touchstart", touch),
            ("touchmove", touch),
            ("touchend", touch),
            ("touchcancel", touch),
            ("pointerdown", pointer_down),
            ("pointermove", pointer_move),
            ("pointerup", pointer_end),
            ("pointercancel", pointer_end),
            ("dblclick", double_click),
        ];
        let options = web_sys::AddEventListenerOptions::new();
        options.set_passive(false);
        let mut listeners: Vec<(&'static str, Listener)> = handlers
            .into_iter()
            .map(|(event, handler)| {
                let canvas = canvas.clone();
                let gesture = gesture.clone();
                let listener = Listener::new(move |event: web_sys::Event| {
                    handler(&canvas, &gesture, &event);
                });
                element
                    .add_event_listener_with_callback_and_add_event_listener_options(
                        event,
                        listener.as_ref().unchecked_ref(),
                        &options,
                    )
                    .expect("the Work map frame accepts its gesture listeners");
                (event, listener)
            })
            .collect();
        let keys = {
            let canvas = canvas.clone();
            Listener::new(move |event: web_sys::Event| key(&canvas, &event))
        };
        element
            .add_event_listener_with_callback("keydown", keys.as_ref().unchecked_ref())
            .expect("the Work map frame accepts its key listener");
        listeners.push(("keydown", keys));
        let resized = Closure::new(move |_: js_sys::Array, _: web_sys::ResizeObserver| {
            canvas.apply();
        });
        let observer = web_sys::ResizeObserver::new(resized.as_ref().unchecked_ref()).ok()?;
        observer.observe(&element);
        Some(Self {
            element,
            listeners,
            observer,
            _resized: resized,
        })
    }
}

/// Frame-relative pixels for a client point, inside the frame's border.
fn anchor(canvas: &Canvas, client: (f64, f64)) -> (f64, f64) {
    canvas.element().map_or((0.0, 0.0), |element| {
        let bounds = element.get_bounding_client_rect();
        (
            client.0 - bounds.left() - f64::from(element.client_left()),
            client.1 - bounds.top() - f64::from(element.client_top()),
        )
    })
}

fn wheel(canvas: &Canvas, _: &RefCell<Gesture>, event: &web_sys::Event) {
    let Some(event) = event.dyn_ref::<web_sys::WheelEvent>() else {
        return;
    };
    let unit = match event.delta_mode() {
        web_sys::WheelEvent::DOM_DELTA_LINE => LINE_PIXELS,
        web_sys::WheelEvent::DOM_DELTA_PAGE => canvas
            .element()
            .map_or(LINE_PIXELS, |element| f64::from(element.client_height())),
        _ => 1.0,
    };
    // Ctrl is also how browsers report a trackpad pinch.
    if event.ctrl_key() || event.meta_key() {
        event.prevent_default();
        let delta = (event.delta_y() * unit).clamp(-WHEEL_ZOOM_DELTA_LIMIT, WHEEL_ZOOM_DELTA_LIMIT);
        canvas.zoom_by(
            (-delta / WHEEL_ZOOM_RATE).exp(),
            anchor(
                canvas,
                (f64::from(event.client_x()), f64::from(event.client_y())),
            ),
        );
        return;
    }
    // Keep shift-wheel's browser-defined horizontal behavior. For ordinary
    // scrolling, intervene only when this delta crosses or starts at a
    // vertical boundary. Firefox otherwise pins the whole wheel transaction
    // to the graph and never gives its unconsumed distance to the side view.
    if event.shift_key() || event.delta_y() == 0.0 {
        return;
    }
    let Some(element) = canvas.element() else {
        return;
    };
    let delta_y = event.delta_y() * unit;
    let top = f64::from(element.scroll_top());
    let limit = f64::from((element.scroll_height() - element.client_height()).max(0));
    let graph_y = (top + delta_y).clamp(0.0, limit) - top;
    let remainder = delta_y - graph_y;
    if remainder.abs() < f64::EPSILON {
        return;
    }
    let Some(side_view) = element.closest(".app-sidebar-frame").ok().flatten() else {
        return;
    };
    event.prevent_default();
    element.scroll_by_with_x_and_y(event.delta_x() * unit, graph_y);
    side_view.scroll_by_with_x_and_y(0.0, remainder);
}

fn number(event: &web_sys::Event, property: &str) -> Option<f64> {
    js_sys::Reflect::get(event, &JsValue::from_str(property))
        .ok()?
        .as_f64()
}

/// Safari reports a trackpad pinch as gesture events rather than ctrl-wheel.
fn safari_gesture(canvas: &Canvas, gesture: &RefCell<Gesture>, event: &web_sys::Event) {
    event.prevent_default();
    let mut gesture = gesture.borrow_mut();
    if gesture.pinch.is_some() {
        return;
    }
    match event.type_().as_str() {
        "gesturestart" => gesture.safari_zoom = Some(canvas.zoom.get()),
        "gesturechange" => {
            let (Some(start), Some(scale), Some(x), Some(y)) = (
                gesture.safari_zoom,
                number(event, "scale"),
                number(event, "clientX"),
                number(event, "clientY"),
            ) else {
                return;
            };
            canvas.zoom_about(start * scale, anchor(canvas, (x, y)));
        }
        _ => gesture.safari_zoom = None,
    }
}

fn touch_pair(event: &web_sys::TouchEvent) -> Option<(f64, (f64, f64))> {
    let touches = event.touches();
    if touches.length() != 2 {
        return None;
    }
    let first = touches.get(0)?;
    let second = touches.get(1)?;
    let (ax, ay) = (f64::from(first.client_x()), f64::from(first.client_y()));
    let (bx, by) = (f64::from(second.client_x()), f64::from(second.client_y()));
    Some(((ax - bx).hypot(ay - by), ((ax + bx) / 2.0, (ay + by) / 2.0)))
}

/// One finger pans natively; two fingers pinch the graph instead of the page.
fn touch(canvas: &Canvas, gesture: &RefCell<Gesture>, event: &web_sys::Event) {
    let Some(event) = event.dyn_ref::<web_sys::TouchEvent>() else {
        return;
    };
    let mut gesture = gesture.borrow_mut();
    let Some((distance, middle)) = touch_pair(event) else {
        gesture.pinch = None;
        return;
    };
    match (event.type_().as_str(), &gesture.pinch) {
        ("touchmove", Some(pinch)) if pinch.distance > 0.0 => {
            event.prevent_default();
            canvas.zoom_about(
                pinch.zoom * distance / pinch.distance,
                anchor(canvas, middle),
            );
        }
        ("touchstart", _) => {
            gesture.pinch = Some(Pinch {
                distance,
                zoom: canvas.zoom.get(),
            });
        }
        _ => {}
    }
}

fn pointer_down(canvas: &Canvas, gesture: &RefCell<Gesture>, event: &web_sys::Event) {
    let Some(event) = event.dyn_ref::<web_sys::PointerEvent>() else {
        return;
    };
    if event.pointer_type() != "mouse" || event.button() != 0 {
        return;
    }
    let on_action = event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        .and_then(|target| {
            target
                .closest("button:not(.work-map-node-button)")
                .ok()
                .flatten()
        })
        .is_some();
    let Some(element) = canvas.element().filter(|_| !on_action) else {
        return;
    };
    gesture.borrow_mut().drag = Some(Drag {
        pointer: event.pointer_id(),
        origin: (f64::from(event.client_x()), f64::from(event.client_y())),
        scroll: (
            f64::from(element.scroll_left()),
            f64::from(element.scroll_top()),
        ),
        moving: false,
    });
}

fn pointer_move(canvas: &Canvas, gesture: &RefCell<Gesture>, event: &web_sys::Event) {
    let Some(event) = event.dyn_ref::<web_sys::PointerEvent>() else {
        return;
    };
    let mut gesture = gesture.borrow_mut();
    let Some(drag) = gesture
        .drag
        .as_mut()
        .filter(|drag| drag.pointer == event.pointer_id())
    else {
        return;
    };
    let Some(element) = canvas.element() else {
        return;
    };
    let delta = (
        f64::from(event.client_x()) - drag.origin.0,
        f64::from(event.client_y()) - drag.origin.1,
    );
    if !drag.moving {
        if delta.0.hypot(delta.1) < DRAG_THRESHOLD {
            return;
        }
        // Capturing retargets the release, so the click the drag ends in
        // lands on the frame rather than activating the node it began on.
        drag.moving = element.set_pointer_capture(drag.pointer).is_ok();
        element
            .set_attribute("data-work-map-dragging", "true")
            .expect("the fixed Work map drag attribute is valid");
    }
    element.set_scroll_left((drag.scroll.0 - delta.0).round() as i32);
    element.set_scroll_top((drag.scroll.1 - delta.1).round() as i32);
}

fn pointer_end(canvas: &Canvas, gesture: &RefCell<Gesture>, event: &web_sys::Event) {
    let Some(event) = event.dyn_ref::<web_sys::PointerEvent>() else {
        return;
    };
    let mut gesture = gesture.borrow_mut();
    if gesture
        .drag
        .as_ref()
        .is_some_and(|drag| drag.pointer == event.pointer_id())
    {
        gesture.drag = None;
        if let Some(element) = canvas.element() {
            let _ = element.remove_attribute("data-work-map-dragging");
        }
    }
}

fn double_click(canvas: &Canvas, _: &RefCell<Gesture>, event: &web_sys::Event) {
    let on_node = event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        .and_then(|target| target.closest("button").ok().flatten())
        .is_some();
    if !on_node {
        event.prevent_default();
        canvas.fit();
    }
}

fn key(canvas: &Canvas, event: &web_sys::Event) {
    let Some(event) = event.dyn_ref::<web_sys::KeyboardEvent>() else {
        return;
    };
    if event.ctrl_key() || event.meta_key() || event.alt_key() {
        return;
    }
    match event.key().as_str() {
        "+" | "=" => canvas.zoom_by(KEY_ZOOM, canvas.center()),
        "-" | "_" => canvas.zoom_by(1.0 / KEY_ZOOM, canvas.center()),
        "0" => canvas.fit(),
        _ => return,
    }
    event.prevent_default();
}
