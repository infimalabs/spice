//! The presentation of the entry the reader is on, written to that entry as
//! it changes, and the restoration a returned-to entry is owed until a newer
//! arrival or the reader's own motion cancels it.
//!
//! Explicit choices (an open row, a node budget, a passive Jobs read) are
//! recorded as they happen. Scroll places, the graph viewport and the focused
//! element are measured from the surfaces when the entry is written, so the
//! departure snapshot is what the reader last saw.

use std::cell::RefCell;

use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::Closure;
use yew::prelude::*;

use super::history::History;
use crate::view::{AppRoute, ChatAnchor, GraphView, Presentation};

/// A burst of scroll or focus events coalesces into one entry write at least
/// this long after the burst's first event.
const COALESCE_MILLIS: u32 = 250;
/// Least time between two presentation writes. WebKit refuses history writes
/// past 100 in 30 s; this spends at most a third of that on presentation and
/// leaves the rest to navigation.
const WRITE_INTERVAL_MILLIS: f64 = 1_000.0;
/// The side panel's scrolling frame; `data-sidebar-content` names what it shows.
const PANEL_SELECTOR: &str = ".app-sidebar-frame";
const PANEL_CONTENT: &str = "data-sidebar-content";
/// One Chat scrolling plane; its `data-agent-plane` names it across renders.
const PLANE_SELECTOR: &str = ".lane-mosaic-plane[data-agent-plane]";
const PLANE_KEY: &str = "data-agent-plane";
const CARD_SELECTOR: &str = ".mosaic-message-card[id]";
/// The graph frame; it carries its zoom as `data-work-map-zoom`.
const GRAPH_SELECTOR: &str = ".work-map-viewport";
pub(crate) const GRAPH_ZOOM: &str = "data-work-map-zoom";
/// The sidebar's stable focus key marks every element whose focus survives a
/// return to its entry.
const FOCUS_KEY: &str = "data-sidebar-focus-key";
/// How many animation frames after an arrival a surface still standing up,
/// like a reopened row's body, is watched for before an owed panel offset
/// clamps to what stands.
const SETTLE_FRAMES: u16 = 90;
/// Keys that move a scrolling surface and so belong to the reader.
const SCROLL_KEYS: [&str; 8] = [
    "ArrowUp",
    "ArrowDown",
    "PageUp",
    "PageDown",
    "Home",
    "End",
    " ",
    "Spacebar",
];

struct Store {
    history: History,
    current: Presentation,
    /// The card that held a link just followed; it leads its plane's anchor.
    departure: Option<ChatAnchor>,
    write: Option<gloo_timers::callback::Timeout>,
    /// When the last presentation write was issued, in epoch milliseconds.
    written: f64,
    /// The entry the current presentation belongs to.
    position: Option<(i32, String)>,
    generation: u64,
    pending: Presentation,
    /// Animation frames left to watch for the pending parts' surfaces after
    /// the latest arrival, and whether one is already requested.
    frames: u16,
    watching: bool,
    listeners: Vec<(&'static str, Listener)>,
}

/// One window listener the store keeps alive for the page's lifetime.
type Listener = Closure<dyn FnMut(web_sys::Event)>;

thread_local! {
    static STORE: RefCell<Option<Store>> = const { RefCell::new(None) };
}

fn with<T>(apply: impl FnOnce(&mut Store) -> T) -> T {
    STORE.with(|store| {
        let mut store = store.borrow_mut();
        let store = store.get_or_insert_with(|| Store {
            history: History::new(),
            current: Presentation::default(),
            departure: None,
            write: None,
            written: 0.0,
            position: None,
            generation: 0,
            pending: Presentation::default(),
            frames: 0,
            watching: false,
            listeners: Vec::new(),
        });
        apply(store)
    })
}

/// Listen once for the reader's own motion and for the page leaving, so a
/// pending restoration yields to the reader and a reload keeps the latest
/// place.
pub(super) fn install() {
    with(|store| {
        if !store.listeners.is_empty() {
            return;
        }
        let window = web_sys::window().expect("presentation requires a browser window");
        let options = web_sys::AddEventListenerOptions::new();
        options.set_capture(true);
        options.set_passive(true);
        let mut listen = |event: &'static str, handler: Listener| {
            window
                .add_event_listener_with_callback_and_add_event_listener_options(
                    event,
                    handler.as_ref().unchecked_ref(),
                    &options,
                )
                .expect("presentation listener");
            store.listeners.push((event, handler));
        };
        for event in ["wheel", "touchmove", "pointerdown"] {
            listen(event, Closure::new(|_| cancel_motion()));
        }
        listen(
            "keydown",
            Closure::new(|event: web_sys::Event| {
                let Some(key) = event
                    .dyn_ref::<web_sys::KeyboardEvent>()
                    .map(web_sys::KeyboardEvent::key)
                else {
                    return;
                };
                if SCROLL_KEYS.contains(&key.as_str()) || key.len() == 1 {
                    cancel_motion();
                }
            }),
        );
        for event in ["scroll", "focusin"] {
            listen(event, Closure::new(|_| schedule()));
        }
        listen("pagehide", Closure::new(|_| flush()));
    });
}

/// A new entry is current: what it remembers is owed to its surfaces until
/// they take it or the reader moves on. Returning to the same entry, as a
/// replace does, changes nothing. Entries the app did not push all stand at
/// the chain's start, so their path tells them apart.
pub(super) fn arrive(route: &AppRoute) {
    with(|store| {
        store.history.own(route);
        let entry = store.history.entry();
        let position = entry
            .as_ref()
            .map(|entry| (entry.position, entry.path.clone()));
        if position.is_some() && position == store.position {
            return;
        }
        store.write.take();
        store.departure = None;
        store.position = position;
        store.generation += 1;
        store.frames = SETTLE_FRAMES;
        store.current = entry.map(|entry| entry.presentation).unwrap_or_default();
        store.pending = store.current.clone();
    });
}

/// The arrival every restoration belongs to; a later one supersedes it.
#[must_use]
pub(crate) fn generation() -> u64 {
    with(|store| store.generation)
}

/// Record one explicit choice on the current entry.
pub(crate) fn record(update: impl FnOnce(&mut Presentation)) {
    with(|store| update(&mut store.current));
    schedule();
}

/// The card holding a link the reader is following leads its plane's anchor
/// on the entry being left, and takes focus back on return.
pub(crate) fn record_departure(anchor: ChatAnchor) {
    with(|store| store.departure = Some(anchor));
}

/// Take one part of the restoration owed to the current entry. `None` once
/// it was taken, cancelled, or never recorded.
pub(crate) fn take<T>(part: impl FnOnce(&mut Presentation) -> Option<T>) -> Option<T> {
    with(|store| part(&mut store.pending))
}

/// Write the current entry now, measured as it stands. The page leaving needs
/// the latest place at once.
fn flush() {
    let presentation = measured();
    with(|store| {
        if store.history.present(&presentation) {
            store.written = js_sys::Date::now();
        }
    });
}

/// Measure the current entry's presentation as it stands into the store. A
/// part the entry is still owed is kept as recorded, so a write landing
/// before its surface restores it does not lose it.
fn measured() -> Presentation {
    with(|store| {
        store.write.take();
        let mut presentation = store.current.clone();
        measure(&mut presentation, store.departure.take());
        let owed = &store.pending;
        if owed.panel.is_some() {
            presentation.panel = owed.panel;
        }
        for anchor in &owed.chat {
            presentation.anchor(anchor.clone());
        }
        if owed.graph.is_some() {
            presentation.graph = owed.graph.clone();
        }
        if owed.focus.is_some() {
            presentation.focus = owed.focus.clone();
        }
        store.current = presentation.clone();
        presentation
    })
}

/// The presentation the entry being left keeps, measured at departure; the
/// push that follows writes it with the entry's chain place.
#[must_use]
pub(super) fn departing() -> Presentation {
    measured()
}

/// Arm one entry write for the motion under way. It lands no sooner than the
/// write interval after the previous one, so continuous motion writes at that
/// pace and a return made mid-motion loses at most one interval of it.
fn schedule() {
    with(|store| {
        if store.write.is_some() {
            return;
        }
        let spacing = store.written + WRITE_INTERVAL_MILLIS - js_sys::Date::now();
        let delay = spacing.max(f64::from(COALESCE_MILLIS)).ceil() as u32;
        store.write = Some(gloo_timers::callback::Timeout::new(delay, flush));
    });
}

/// The reader moved: late scroll and focus restoration would fight them.
fn cancel_motion() {
    with(|store| {
        store.pending.panel = None;
        store.pending.chat.clear();
        store.pending.graph = None;
        store.pending.focus = None;
    });
}

/// Read every measured place from the surfaces as they stand.
fn measure(presentation: &mut Presentation, departure: Option<ChatAnchor>) {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    presentation.panel = document
        .query_selector(PANEL_SELECTOR)
        .ok()
        .flatten()
        .filter(|frame| frame.scroll_top() > 0)
        .map(|frame| f64::from(frame.scroll_top()));
    presentation.chat.clear();
    if let Ok(planes) = document.query_selector_all(PLANE_SELECTOR) {
        for index in 0..planes.length() {
            let Some(plane) = planes
                .get(index)
                .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
            else {
                continue;
            };
            if let Some(anchor) = plane_anchor(&plane) {
                presentation.anchor(anchor);
            }
        }
    }
    if let Some(departure) = departure {
        presentation.anchor(departure);
    }
    presentation.graph = document
        .query_selector(GRAPH_SELECTOR)
        .ok()
        .flatten()
        .and_then(|frame| {
            let zoom = frame
                .get_attribute(GRAPH_ZOOM)
                .and_then(|zoom| zoom.parse::<f64>().ok())
                .unwrap_or(1.0);
            let scroll = (
                f64::from(frame.scroll_left()),
                f64::from(frame.scroll_top()),
            );
            (zoom > 1.0 || scroll != (0.0, 0.0)).then_some(GraphView { zoom, scroll })
        });
    presentation.focus = document
        .active_element()
        .and_then(|active| active.closest(&format!("[{FOCUS_KEY}]")).ok().flatten())
        .and_then(|keyed| keyed.get_attribute(FOCUS_KEY));
}

/// The first card still reaching below one plane's top edge, and how far its
/// own top edge sits above or below that edge.
fn plane_anchor(plane: &web_sys::Element) -> Option<ChatAnchor> {
    let key = plane.get_attribute(PLANE_KEY)?;
    if plane.scroll_top() == 0 {
        return None;
    }
    let top = plane.get_bounding_client_rect().top();
    let cards = plane.query_selector_all(CARD_SELECTOR).ok()?;
    (0..cards.length())
        .filter_map(|index| cards.get(index)?.dyn_into::<web_sys::Element>().ok())
        .find(|card| card.get_bounding_client_rect().bottom() > top)
        .map(|card| ChatAnchor {
            plane: key,
            card: card.id(),
            offset: card.get_bounding_client_rect().top() - top,
            focus: false,
        })
}

/// Scroll one plane so its anchored card returns to its recorded line and,
/// when the anchor left through a link, give the card focus back. A card no
/// longer present leaves the plane where it is.
fn restore_anchor(document: &web_sys::Document, anchor: &ChatAnchor) {
    let Some(card) = document.get_element_by_id(&anchor.card) else {
        return;
    };
    let Some(plane) = card.closest(PLANE_SELECTOR).ok().flatten() else {
        return;
    };
    if plane.get_attribute(PLANE_KEY).as_deref() != Some(anchor.plane.as_str()) {
        return;
    }
    let offset = card.get_bounding_client_rect().top() - plane.get_bounding_client_rect().top();
    plane.scroll_by_with_x_and_y(0.0, offset - anchor.offset);
    if anchor.focus
        && let Ok(card) = card.dyn_into::<web_sys::HtmlElement>()
    {
        let options = web_sys::FocusOptions::new();
        options.set_prevent_scroll(true);
        let _ = card.focus_with_options(&options);
    }
}

/// Set a frame's scroll offset within its current extent.
fn clamp_scroll(frame: &web_sys::Element, top: f64) {
    let limit = f64::from((frame.scroll_height() - frame.client_height()).max(0));
    frame.set_scroll_top(top.clamp(0.0, limit).round() as i32);
}

/// What the current route lets settle: which panel content is its own, and
/// whether its Chat planes are free to take their anchors back (a routed
/// card owns its plane).
#[derive(Clone, Copy, PartialEq)]
struct Settling {
    content: Option<&'static str>,
    planes: bool,
}

/// After every render, give a returned-to entry its places back once their
/// surfaces stand. Surfaces that stand up without re-rendering the app, like
/// the row a list reopens, are watched a bounded run of animation frames
/// further; when that run ends, a panel still owed its offset clamps to the
/// extent it has.
#[hook]
pub(in crate::browser) fn use_settlement(route: &AppRoute) {
    let settling = Settling {
        content: match route {
            AppRoute::Chat { .. } => None,
            AppRoute::Work(_) => Some("work"),
            AppRoute::Spice => Some("spice"),
        },
        planes: route.card_target().is_none(),
    };
    use_effect(move || settle(settling));
}

fn settle(settling: Settling) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    let last_frame = with(|store| store.frames == 0);
    // The panel takes its offset back once its content stands tall enough to
    // hold it, or at the run's end clamped to what stands.
    if let Some(frame) = document.query_selector(PANEL_SELECTOR).ok().flatten()
        && frame.get_attribute(PANEL_CONTENT).as_deref() == settling.content
        && let Some(top) = take(|pending| {
            let limit = f64::from((frame.scroll_height() - frame.client_height()).max(0));
            (pending.row.is_none() && pending.panel.is_some_and(|top| last_frame || top <= limit))
                .then(|| pending.panel.take())
                .flatten()
        })
    {
        clamp_scroll(&frame, top);
    }
    // Planes take their anchors back as their cards render, each once; a
    // plane whose card never returns stays where it is.
    if settling.planes {
        let present = take(|pending| {
            let mut present = Vec::new();
            pending.chat.retain(|anchor| {
                let found = document.get_element_by_id(&anchor.card).is_some();
                if found {
                    present.push(anchor.clone());
                }
                !found
            });
            (!present.is_empty()).then_some(present)
        });
        for anchor in present.unwrap_or_default() {
            restore_anchor(&document, &anchor);
        }
    }
    // Focus returns once its keyed element is rendered and reachable.
    let focus = with(|store| store.pending.focus.clone());
    if let Some(key) = focus
        && let Ok(candidates) = document.query_selector_all(&format!("[{FOCUS_KEY}]"))
        && let Some(element) = (0..candidates.length())
            .filter_map(|index| candidates.get(index)?.dyn_into::<web_sys::Element>().ok())
            .find(|candidate| {
                candidate.get_attribute(FOCUS_KEY).as_deref() == Some(key.as_str())
                    && super::super::overlay::returnable(candidate)
            })
        && sidebar_settled(&element)
        && let Some(element) = element.dyn_ref::<web_sys::HtmlElement>()
    {
        with(|store| store.pending.focus = None);
        let options = web_sys::FocusOptions::new();
        options.set_prevent_scroll(true);
        let _ = element.focus_with_options(&options);
    }
    let watch = with(|store| {
        let owed = store.pending.panel.is_some()
            || !store.pending.chat.is_empty()
            || store.pending.focus.is_some();
        if owed && store.frames > 0 && !store.watching {
            store.watching = true;
            store.frames -= 1;
            true
        } else {
            false
        }
    });
    if watch {
        let generation = with(|store| store.generation);
        let frame = Closure::once_into_js(move |_: f64| {
            let current = with(|store| {
                store.watching = false;
                store.generation == generation
            });
            if current {
                settle(settling);
            }
        });
        let _ = window.request_animation_frame(frame.unchecked_ref());
    }
}

/// An element inside the sidebar takes focus back only once the sidebar has
/// finished opening and given its Close button the opening focus; outside it,
/// at once.
fn sidebar_settled(element: &web_sys::Element) -> bool {
    element
        .closest(".app-sidebar-layer")
        .ok()
        .flatten()
        .is_none_or(|layer| layer.get_attribute("data-sidebar-state").as_deref() == Some("open"))
}
