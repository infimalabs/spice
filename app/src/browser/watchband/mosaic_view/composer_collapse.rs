//! One team's composer exposure, carried by the Chat plane's own scroll.
//!
//! The composer rides in the plane's scroll content at the foot of a slot that
//! spans the content from its top, and sticks to the plane's top edge so
//! scrolling back can never open a gap above it. The browser's scroll moves
//! composer and cards together on every engine, in the same frame. This module
//! only moves the composer's rest, its place in the content, and only where the
//! move cannot be seen: while the composer is fully revealed the rest follows
//! the top edge, so scrolling on moves it at once, and while it is fully hidden
//! the rest follows one composer height and its painted continuation above, so
//! scrolling back reveals it at once. The continuation is the frost CSS paints
//! below the box; it counts as composer, so full collapse is never declared nor
//! the rest moved while any of it is on screen. Mosaic's own anchor correction,
//! which keeps cards visually still, moves the rest with it so the composer
//! stays still too.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{Element, Event, FocusEvent, HtmlElement, KeyboardEvent, Node, ResizeObserver};
use yew::prelude::*;

const COLLAPSE_EPSILON_PX: f64 = 0.25;
/// Moving the rest below this distance would only rewrite floating-point noise.
const REST_EPSILON_PX: f64 = 0.001;

struct CollapseState {
    offset: f64,
    /// The composer's own height: the plane's spacer and the exposed extent.
    maximum: f64,
    /// How far the composer's paint continues below its box.
    overhang: f64,
    /// The plane scroll position the offset last followed.
    scroll_top: f64,
    /// `scrollTop` read through JavaScript keeps subpixel positions that the
    /// integer `web_sys` getter truncates, so the rest never drifts off the
    /// cards on high-density displays.
    scroll_top_key: JsValue,
    /// The slot whose foot is the composer's rest.
    slot: Option<HtmlElement>,
    /// The slot height last written.
    rest: Option<f64>,
    /// Focus is inside the composer. Its fields then lie in the plane's scroll
    /// padding, which would send the browser's focus scrolling after them, so
    /// the padding stands aside until focus leaves.
    focused: bool,
}

impl CollapseState {
    fn new() -> Self {
        Self {
            offset: 0.0,
            maximum: 0.0,
            overhang: 0.0,
            scroll_top: 0.0,
            scroll_top_key: JsValue::from_str("scrollTop"),
            slot: None,
            rest: None,
            focused: false,
        }
    }

    /// The full collapse distance: the box and its painted continuation.
    fn travel(&self) -> f64 {
        self.maximum + self.overhang
    }

    fn plane_scroll_top(&self, scroller: &Element) -> f64 {
        js_sys::Reflect::get(scroller, &self.scroll_top_key)
            .ok()
            .and_then(|top| top.as_f64())
            .unwrap_or_else(|| f64::from(scroller.scroll_top()))
    }

    /// Return the plane to the position the composer last followed. A scroll
    /// the browser made since, before any scroll event reports it, is undone in
    /// the same task, so no frame paints it.
    fn hold_scroll(&self, scroller: &Element) {
        if (self.plane_scroll_top(scroller) - self.scroll_top).abs() < REST_EPSILON_PX {
            return;
        }
        js_sys::Reflect::set(
            scroller,
            &self.scroll_top_key,
            &JsValue::from_f64(self.scroll_top),
        )
        .expect("the plane accepts its scroll position");
    }

    /// Rest the composer `offset` above the plane's current top edge. While it
    /// travels with the cards its rest stays put, so scrolling writes nothing.
    fn place_rest(&mut self) {
        let Some(slot) = &self.slot else {
            return;
        };
        // An unmeasured composer keeps its natural place atop the content.
        if self.maximum <= COLLAPSE_EPSILON_PX {
            return;
        }
        let height = self.scroll_top - self.offset + self.maximum;
        if self
            .rest
            .is_some_and(|rest| (rest - height).abs() < REST_EPSILON_PX)
        {
            return;
        }
        slot.style()
            .set_property("height", &format!("{height}px"))
            .expect("the composer slot height is valid");
        self.rest = Some(height);
    }
}

type SharedCollapse = Rc<RefCell<CollapseState>>;

/// The composer exposure a plane's scroll owner consults before correcting it.
#[derive(Clone)]
pub(super) struct ComposerCollapse(SharedCollapse);

impl ComposerCollapse {
    /// Write a programmatic scroll position without moving the composer.
    pub(super) fn correct_scroll(&self, scroller: &Element, top: i32) {
        let before = self.0.borrow().plane_scroll_top(scroller);
        scroller.set_scroll_top(top);
        let mut state = self.0.borrow_mut();
        let applied = state.plane_scroll_top(scroller) - before;
        state.scroll_top += applied;
        // The rest moves in the same task as the scroll, so no frame shows the
        // composer carried along with the correction.
        state.place_rest();
    }
}

#[hook]
pub(super) fn use_composer_collapse(host: NodeRef, slot: HtmlElement) -> ComposerCollapse {
    let state = use_mut_ref(CollapseState::new);
    {
        let state = Rc::clone(&state);
        use_effect_with((host, slot), move |(host, slot)| {
            let registration = host
                .cast::<Element>()
                .and_then(|scroller| Registration::new(scroller, slot.clone(), state));
            move || drop(registration)
        });
    }
    ComposerCollapse(state)
}

struct Registration {
    scroller: Element,
    composer: HtmlElement,
    observer: ResizeObserver,
    _resize: Closure<dyn FnMut(js_sys::Array, ResizeObserver)>,
    scroll: Closure<dyn FnMut(Event)>,
    key: Closure<dyn FnMut(KeyboardEvent)>,
    focus: Closure<dyn FnMut(Event)>,
    blur: Closure<dyn FnMut(FocusEvent)>,
}

impl Registration {
    fn new(scroller: Element, slot: HtmlElement, state: SharedCollapse) -> Option<Self> {
        let scroll_element = scroller.clone().dyn_into::<HtmlElement>().ok()?;
        let surface = scroller
            .closest(".team-compose")
            .ok()
            .flatten()?
            .dyn_into::<HtmlElement>()
            .ok()?;
        let composer = slot
            .query_selector(".team-composer")
            .ok()
            .flatten()?
            .dyn_into::<HtmlElement>()
            .ok()?;
        {
            let mut state = state.borrow_mut();
            state.scroll_top = state.plane_scroll_top(&scroller);
            state.slot = Some(slot);
            state.rest = None;
        }
        sync_metrics(&surface, &composer, &scroll_element, &state);

        let resized_surface = surface.clone();
        let resized_composer = composer.clone();
        let resized_scroller = scroll_element.clone();
        let resized_state = Rc::clone(&state);
        let resize = Closure::<dyn FnMut(js_sys::Array, ResizeObserver)>::new(move |_, _| {
            sync_metrics(
                &resized_surface,
                &resized_composer,
                &resized_scroller,
                &resized_state,
            );
        });
        let observer = ResizeObserver::new(resize.as_ref().unchecked_ref()).ok()?;
        observer.observe(&composer);

        let scroll_surface = surface.clone();
        let scroll_scroller = scroll_element.clone();
        let scroll_state = Rc::clone(&state);
        let scroll = Closure::<dyn FnMut(Event)>::new(move |_| {
            follow_scroll(&scroll_surface, &scroll_scroller, &scroll_state);
        });
        scroller
            .add_event_listener_with_callback("scroll", scroll.as_ref().unchecked_ref())
            .expect("the owned activity plane accepts its scroll listener");

        let key_surface = surface.clone();
        let key_composer = composer.clone();
        let key_scroller = scroll_element.clone();
        let key_state = Rc::clone(&state);
        let key = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
            // The composer's own fields keep Home and End for their caret.
            let in_composer = event
                .target()
                .and_then(|target| target.dyn_into::<Node>().ok())
                .is_some_and(|target| key_composer.contains(Some(&target)));
            let requested = match event.key().as_str() {
                _ if in_composer => return,
                "Home" => 0.0,
                "End" => key_state.borrow().travel(),
                _ => return,
            };
            set_collapse(&key_surface, &key_scroller, &key_state, requested);
        });
        scroller
            .add_event_listener_with_callback("keydown", key.as_ref().unchecked_ref())
            .expect("the owned activity plane accepts its keyboard listener");

        // Focus entering the composer, from Quote's new draft or the keyboard,
        // reveals it fully so the caret is never above the plane. Browsers may
        // already have scrolled the plane toward the field's rest when focus
        // events fire; that scroll is held back, so the composer comes to the
        // field and the cards stay.
        let focus_surface = surface.clone();
        let focus_scroller = scroll_element.clone();
        let focus_state = Rc::clone(&state);
        let focus = Closure::<dyn FnMut(Event)>::new(move |_| {
            focus_state.borrow_mut().focused = true;
            focus_state.borrow().hold_scroll(&focus_scroller);
            set_collapse(&focus_surface, &focus_scroller, &focus_state, 0.0);
            paint_padding(&focus_scroller, &focus_state);
        });
        composer
            .add_event_listener_with_callback("focusin", focus.as_ref().unchecked_ref())
            .expect("the team composer accepts its focus listener");
        let blur_composer = composer.clone();
        let blur_scroller = scroll_element.clone();
        let blur_state = Rc::clone(&state);
        let blur = Closure::<dyn FnMut(FocusEvent)>::new(move |event: FocusEvent| {
            let stays = event
                .related_target()
                .and_then(|target| target.dyn_into::<Node>().ok())
                .is_some_and(|target| blur_composer.contains(Some(&target)));
            if !stays {
                blur_state.borrow_mut().focused = false;
                paint_padding(&blur_scroller, &blur_state);
            }
        });
        composer
            .add_event_listener_with_callback("focusout", blur.as_ref().unchecked_ref())
            .expect("the team composer accepts its blur listener");

        Some(Self {
            scroller,
            composer,
            observer,
            _resize: resize,
            scroll,
            key,
            focus,
            blur,
        })
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.observer.disconnect();
        let _ = self
            .scroller
            .remove_event_listener_with_callback("scroll", self.scroll.as_ref().unchecked_ref());
        let _ = self
            .scroller
            .remove_event_listener_with_callback("keydown", self.key.as_ref().unchecked_ref());
        let _ = self
            .composer
            .remove_event_listener_with_callback("focusin", self.focus.as_ref().unchecked_ref());
        let _ = self
            .composer
            .remove_event_listener_with_callback("focusout", self.blur.as_ref().unchecked_ref());
    }
}

fn follow_scroll(surface: &HtmlElement, scroller: &HtmlElement, state: &SharedCollapse) {
    let requested = {
        let mut state = state.borrow_mut();
        let top = state.plane_scroll_top(scroller);
        let scrolled = top - state.scroll_top;
        state.scroll_top = top;
        crate::mosaic::composer_collapse_offset(state.offset, state.travel(), scrolled, top)
    };
    set_collapse(surface, scroller, state, requested);
}

fn sync_metrics(
    surface: &HtmlElement,
    composer: &HtmlElement,
    scroller: &HtmlElement,
    state: &SharedCollapse,
) {
    // A Chat surface hidden behind Work stays mounted, and its unrendered
    // composer measures zero. Collapsing the spacer then would shift every card
    // on return, so the last rendered metrics stand.
    if composer.offset_parent().is_none() {
        return;
    }
    let maximum = f64::from(composer.offset_height());
    let overhang = painted_overhang(composer);
    surface
        .style()
        .set_property("--team-composer-block", &format!("{maximum}px"))
        .expect("the composer block CSS property is valid");
    let requested = {
        let mut state = state.borrow_mut();
        let was_fully_collapsed = state.maximum > COLLAPSE_EPSILON_PX
            && state.offset >= state.travel() - COLLAPSE_EPSILON_PX;
        state.maximum = maximum;
        state.overhang = overhang;
        if was_fully_collapsed {
            state.travel()
        } else {
            state.offset.min(state.travel())
        }
    };
    if !set_collapse(surface, scroller, state, requested) {
        // A resize changes the rest even when a fully revealed offset stays zero.
        state.borrow_mut().place_rest();
    }
}

/// The depth CSS declares for the composer's continuation below its box.
fn painted_overhang(composer: &HtmlElement) -> f64 {
    web_sys::window()
        .and_then(|window| window.get_computed_style(composer).ok().flatten())
        .and_then(|style| style.get_property_value("--team-composer-overhang").ok())
        .and_then(|value| value.trim().strip_suffix("px")?.parse::<f64>().ok())
        .filter(|depth| depth.is_finite() && *depth >= 0.0)
        .unwrap_or(0.0)
}

/// Settle the offset. The absolute rest re-anchors only while exposure changes;
/// once either edge is reached, CSS sticky positioning owns continued motion
/// without a layout write for every scroll event.
fn set_collapse(
    surface: &HtmlElement,
    scroller: &HtmlElement,
    state: &SharedCollapse,
    requested: f64,
) -> bool {
    let changed = update_collapse(state, requested);
    let uninitialized = !surface.has_attribute("data-composer-collapse");
    if changed || uninitialized {
        paint_collapse(surface, scroller, state);
        state.borrow_mut().place_rest();
        true
    } else {
        false
    }
}

fn update_collapse(state: &SharedCollapse, requested: f64) -> bool {
    let mut state = state.borrow_mut();
    let next = requested.clamp(0.0, state.travel());
    if (next - state.offset).abs() <= f64::EPSILON {
        return false;
    }
    state.offset = next;
    true
}

/// Keep browser scrolls' targets clear of the composer's exposed part.
fn paint_padding(scroller: &HtmlElement, state: &SharedCollapse) {
    let padding = {
        let state = state.borrow();
        if state.focused {
            0.0
        } else {
            (state.maximum - state.offset).max(0.0)
        }
    };
    scroller
        .style()
        .set_property("scroll-padding-top", &format!("{padding}px"))
        .expect("the plane scroll padding is valid");
}

fn paint_collapse(surface: &HtmlElement, scroller: &HtmlElement, state: &SharedCollapse) {
    paint_padding(scroller, state);
    let (offset, maximum, travel) = {
        let state = state.borrow();
        (state.offset, state.maximum, state.travel())
    };
    surface
        .set_attribute("data-composer-collapse", &offset.to_string())
        .expect("the composer collapse diagnostic attribute is valid");
    let fully_collapsed = if maximum > COLLAPSE_EPSILON_PX && offset >= travel - COLLAPSE_EPSILON_PX
    {
        "true"
    } else {
        "false"
    };
    if surface
        .get_attribute("data-composer-fully-collapsed")
        .as_deref()
        != Some(fully_collapsed)
    {
        surface
            .set_attribute("data-composer-fully-collapsed", fully_collapsed)
            .expect("the composer collapse state attribute is valid");
    }
}
