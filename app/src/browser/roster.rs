//! The global menu's roster of agent teams.
//!
//! Formed groups stand in order, each a card named for its member zero and
//! holding its members' tiles; the ungrouped list follows. A mouse drags a
//! whole tile or card; a finger or pen drags a tile by its handle or a card by
//! its header, and a lift near the sheet's edge scrolls it. A member released
//! on a card joins that group, on an ungrouped tile founds a group with that
//! member, on a tile in its own group takes that place, and on the ungrouped
//! list leaves its group. A card released on another card or its tiles takes
//! that place among the groups. A full group's card accepts no member. Every
//! release resolves through [`crate::roster::Roster::landing`] to at most one
//! change for the kind's authority to admit at the revision the roster was
//! drawn from; no release mounts, reorders, closes, or focuses a view.

use std::cell::{Cell, RefCell};
use std::fmt::Display;
use std::rc::Rc;
use std::str::FromStr;

use wasm_bindgen::JsCast;
use web_sys::{DragEvent, PointerEvent};
use yew::prelude::*;

use super::touch_lift;
use crate::roster::{Change, Lift, Roster, Side, Target};
use crate::watchband::PointerDragSession;

/// The attribute naming what a release over an element states.
const DROP: &str = "data-roster-drop";
/// The attribute naming what pressing a row lifts.
const LIFT: &str = "data-roster-lift";
/// The attribute marking a row's finger or pen handle.
const HANDLE: &str = "data-roster-handle";

thread_local! {
    /// The one roster lift a finger or pen holds.
    static TOUCH_DRAG: RefCell<PointerDragSession<String>> = RefCell::default();
    /// The one roster lift a mouse drag holds, from its start to its end.
    static MOUSE_LIFT: RefCell<Option<String>> = const { RefCell::new(None) };
    /// A lifted drag's release can still click a tile link; that click is swallowed.
    static SWALLOW_CLICK: Cell<bool> = const { Cell::new(false) };
    /// The last press went down on a form control a row carries, so a mouse
    /// drag from it moves that control rather than lifting the row.
    static CONTROL_PRESS: Cell<bool> = const { Cell::new(false) };
}

/// What one kind's roster says.
pub(super) struct Wording {
    /// The kind's name, which scopes every lift and drop value.
    pub(super) kind: &'static str,
    pub(super) ungrouped_title: &'static str,
    pub(super) new_group_title: &'static str,
    pub(super) ungrouped_hint: &'static str,
    /// Why a full group's card accepts no member.
    pub(super) full: &'static str,
    /// The base the ungrouped list wears; `None` keeps the page's.
    pub(super) drop_base: Option<usize>,
}

/// One roster render.
pub(super) struct RosterView<'a, G, M> {
    pub(super) wording: &'static Wording,
    pub(super) roster: Rc<Roster<G, M>>,
    /// The authority revision the roster was drawn from.
    pub(super) revision: u64,
    pub(super) enabled: bool,
    /// What the ungrouped list says while it holds nobody.
    pub(super) empty: &'a str,
    /// Send one change to the kind's authority.
    pub(super) submit: Rc<dyn Fn(Change<G, M>)>,
    /// A group card's header content, named for its member zero.
    pub(super) head: &'a dyn Fn(&G, &[M]) -> Html,
    /// A formed card's stable object base, shared with its corresponding console.
    pub(super) base: &'a dyn Fn(&G) -> Option<usize>,
    /// A member tile's content, grouped or not; it marks its handle with
    /// `data-roster-handle`.
    pub(super) member: &'a dyn Fn(&M, bool) -> Html,
}

/// What every gesture handler shares.
struct Gesture<G, M> {
    kind: &'static str,
    roster: Rc<Roster<G, M>>,
    revision: u64,
    enabled: bool,
    submit: Rc<dyn Fn(Change<G, M>)>,
}

impl<G, M> Clone for Gesture<G, M> {
    fn clone(&self) -> Self {
        Self {
            kind: self.kind,
            roster: self.roster.clone(),
            revision: self.revision,
            enabled: self.enabled,
            submit: self.submit.clone(),
        }
    }
}

impl<G, M> Gesture<G, M>
where
    G: Clone + Eq + Display + FromStr + 'static,
    M: Clone + Eq + Display + FromStr + 'static,
{
    fn mime(&self) -> String {
        format!("application/x-spice-roster-{}", self.kind)
    }

    fn lift_value(&self, lift: &Lift<G, M>) -> String {
        match lift {
            Lift::Member(member) => format!("{}:member:{member}", self.kind),
            Lift::Group(group) => format!("{}:group:{group}", self.kind),
        }
    }

    fn lift(&self, value: &str) -> Option<Lift<G, M>> {
        let value = value.strip_prefix(self.kind)?.strip_prefix(':')?;
        if let Some(member) = value.strip_prefix("member:") {
            return member.parse().ok().map(Lift::Member);
        }
        value.strip_prefix("group:")?.parse().ok().map(Lift::Group)
    }

    fn drop_value(&self, target: &str) -> String {
        format!("{}:{target}", self.kind)
    }

    /// What an enabled drop element of this roster states for a release at
    /// `client_y`.
    fn target(&self, element: &web_sys::Element, client_y: i32) -> Option<Target<G, M>> {
        if element.get_attribute("data-drop-enabled").as_deref() != Some("true") {
            return None;
        }
        let value = element.get_attribute(DROP)?;
        let value = value.strip_prefix(self.kind)?.strip_prefix(':')?;
        let rect = element.get_bounding_client_rect();
        let side = if f64::from(client_y) < rect.top() + rect.height() / 2.0 {
            Side::Before
        } else {
            Side::After
        };
        match value {
            "ungrouped" => Some(Target::Ungrouped),
            "new" => Some(Target::NewGroup),
            _ => {
                if let Some(member) = value.strip_prefix("member:") {
                    return member
                        .parse()
                        .ok()
                        .map(|member| Target::Member(member, side));
                }
                let group = value.strip_prefix("group:")?.parse().ok()?;
                Some(Target::Group(group, side))
            }
        }
    }

    /// The change a release of `lift` at the contact asks for, beside the
    /// element that states it.
    fn landing_at(
        &self,
        lift: &Lift<G, M>,
        client_x: i32,
        client_y: i32,
    ) -> Option<(Change<G, M>, web_sys::Element)> {
        let element = touch_lift::target_at(DROP, client_x, client_y)?;
        let target = self.target(&element, client_y)?;
        Some((self.roster.landing(lift, &target)?, element))
    }

    fn land(&self, change: Change<G, M>) {
        (self.submit)(change);
    }
}

/// Draw one roster.
pub(super) fn render<G, M>(view: RosterView<'_, G, M>) -> Html
where
    G: Clone + Eq + Display + FromStr + 'static,
    M: Clone + Eq + Display + FromStr + 'static,
{
    let gesture = Gesture {
        kind: view.wording.kind,
        roster: view.roster.clone(),
        revision: view.revision,
        enabled: view.enabled,
        submit: view.submit.clone(),
    };
    let wording = view.wording;
    let enabled = view.enabled.to_string();
    let ungrouped = view.roster.ungrouped();
    let drop_flavor = wording.drop_base.map(|_| "roster");
    let drop_accent = wording.drop_base.map(|base| base.to_string());
    html! {
        <div
            class="app-roster"
            data-roster={wording.kind}
            ondragover={allow_drop(&gesture)}
            ondrop={drop(&gesture)}
            ondragend={end_drag()}
        >
            <div class="app-roster-groups">
                {for view.roster.groups().map(|(group, members)| render_group(&view, &gesture, group, members))}
            </div>
            <section
                class="app-sheet-section app-roster-ungrouped"
                data-roster-drop={gesture.drop_value("new")}
                data-drop-enabled={enabled.clone()}
            >
                <header><h3>{wording.new_group_title}</h3></header>
                <p class="app-empty-state">{"Drop an agent here to start its own team"}</p>
            </section>
            <section
                class="app-sheet-section app-roster-ungrouped"
                data-base-flavor={drop_flavor}
                data-base-accent={drop_accent}
                data-roster-drop={gesture.drop_value("ungrouped")}
                data-drop-enabled={enabled}
            >
                <header>
                    <h3>{wording.ungrouped_title}</h3>
                    <span>{wording.ungrouped_hint}</span>
                </header>
                <div class="app-roster-members">
                    if ungrouped.is_empty() {
                        <p class="app-empty-state">{view.empty}</p>
                    } else {
                        {for ungrouped.iter().map(|member| render_member(&view, &gesture, member, false))}
                    }
                </div>
            </section>
        </div>
    }
}

fn render_group<G, M>(
    view: &RosterView<'_, G, M>,
    gesture: &Gesture<G, M>,
    group: &G,
    members: &[M],
) -> Html
where
    G: Clone + Eq + Display + FromStr + 'static,
    M: Clone + Eq + Display + FromStr + 'static,
{
    let accepts = view.enabled && view.roster.accepts(group);
    let lift = gesture.lift_value(&Lift::Group(group.clone()));
    let base = (view.base)(group);
    let base_flavor = base.map(|_| "roster");
    let base_accent = base.map(|base| base.to_string());
    html! {
        <article
            class="app-roster-group"
            key={group.to_string()}
            data-roster-group={group.to_string()}
            data-base-flavor={base_flavor}
            data-base-accent={base_accent}
            data-roster-drop={gesture.drop_value(&format!("group:{group}"))}
            data-drop-enabled={accepts.to_string()}
            data-roster-lift={lift.clone()}
            draggable={view.enabled.to_string()}
            title={(view.enabled && !accepts).then_some(view.wording.full)}
            ondragstart={begin_drag(gesture, lift.clone())}
            onpointerdown={touch_start(gesture, lift.clone())}
            onpointermove={touch_movement(gesture, lift.clone())}
            onpointerup={touch_finish(gesture, lift.clone())}
            onpointercancel={touch_cancel(lift.clone())}
            onlostpointercapture={touch_cancel(lift)}
            onclick={swallow_drag_click()}
        >
            <header data-roster-handle="">{(view.head)(group, members)}</header>
            <div class="app-roster-members">
                {for members.iter().map(|member| render_member(view, gesture, member, true))}
            </div>
        </article>
    }
}

fn render_member<G, M>(
    view: &RosterView<'_, G, M>,
    gesture: &Gesture<G, M>,
    member: &M,
    grouped: bool,
) -> Html
where
    G: Clone + Eq + Display + FromStr + 'static,
    M: Clone + Eq + Display + FromStr + 'static,
{
    let lift = gesture.lift_value(&Lift::Member(member.clone()));
    html! {
        <div
            class="app-roster-member"
            key={member.to_string()}
            draggable={view.enabled.to_string()}
            data-roster-member={member.to_string()}
            data-roster-lift={lift.clone()}
            data-roster-drop={gesture.drop_value(&format!("member:{member}"))}
            data-drop-enabled={view.enabled.to_string()}
            ondragstart={begin_drag(gesture, lift.clone())}
            onpointerdown={touch_start(gesture, lift.clone())}
            onpointermove={touch_movement(gesture, lift.clone())}
            onpointerup={touch_finish(gesture, lift.clone())}
            onpointercancel={touch_cancel(lift.clone())}
            onlostpointercapture={touch_cancel(lift)}
            onclick={swallow_drag_click()}
        >
            {(view.member)(member, grouped)}
        </div>
    }
}

fn begin_drag<G, M>(gesture: &Gesture<G, M>, lift: String) -> Callback<DragEvent>
where
    G: Clone + Eq + Display + FromStr + 'static,
    M: Clone + Eq + Display + FromStr + 'static,
{
    let (enabled, mime) = (gesture.enabled, gesture.mime());
    Callback::from(move |event: DragEvent| {
        // The innermost row lifts; a card holding the tile does not.
        event.stop_propagation();
        if !enabled || CONTROL_PRESS.get() {
            event.prevent_default();
            return;
        }
        // A browser starts a drag only once it carries data; the lift itself
        // stays in process, since a drag over a target cannot read that data.
        if let Some(transfer) = event.data_transfer()
            && transfer.set_data(&mime, &lift).is_ok()
        {
            transfer.set_effect_allowed("move");
        }
        MOUSE_LIFT.set(Some(lift.clone()));
    })
}

fn end_drag() -> Callback<DragEvent> {
    Callback::from(|_| MOUSE_LIFT.set(None))
}

/// The change the mouse lift would ask for if released at `event`.
fn mouse_landing<G, M>(gesture: &Gesture<G, M>, event: &DragEvent) -> Option<Change<G, M>>
where
    G: Clone + Eq + Display + FromStr + 'static,
    M: Clone + Eq + Display + FromStr + 'static,
{
    if !gesture.enabled {
        return None;
    }
    let lift = MOUSE_LIFT.with_borrow(|lift| gesture.lift(lift.as_deref()?))?;
    let element = event
        .target()?
        .dyn_into::<web_sys::Element>()
        .ok()?
        .closest(&format!("[{DROP}]"))
        .ok()??;
    let target = gesture.target(&element, event.client_y())?;
    gesture.roster.landing(&lift, &target)
}

fn allow_drop<G, M>(gesture: &Gesture<G, M>) -> Callback<DragEvent>
where
    G: Clone + Eq + Display + FromStr + 'static,
    M: Clone + Eq + Display + FromStr + 'static,
{
    let gesture = gesture.clone();
    Callback::from(move |event: DragEvent| {
        if mouse_landing(&gesture, &event).is_some() {
            event.prevent_default();
            if let Some(transfer) = event.data_transfer() {
                transfer.set_drop_effect("move");
            }
        }
    })
}

fn drop<G, M>(gesture: &Gesture<G, M>) -> Callback<DragEvent>
where
    G: Clone + Eq + Display + FromStr + 'static,
    M: Clone + Eq + Display + FromStr + 'static,
{
    let gesture = gesture.clone();
    Callback::from(move |event: DragEvent| {
        if let Some(change) = mouse_landing(&gesture, &event) {
            event.prevent_default();
            event.stop_propagation();
            MOUSE_LIFT.set(None);
            gesture.land(change);
        }
    })
}

/// Begin a finger or pen lift from a row's handle; a tap still activates it.
/// Every press also records whether it went down on a control.
fn touch_start<G, M>(gesture: &Gesture<G, M>, lift: String) -> Callback<PointerEvent>
where
    G: Clone + Eq + Display + FromStr + 'static,
    M: Clone + Eq + Display + FromStr + 'static,
{
    let (enabled, revision) = (gesture.enabled, gesture.revision);
    Callback::from(move |event: PointerEvent| {
        SWALLOW_CLICK.set(false);
        CONTROL_PRESS.set(on_control(&event));
        if event.pointer_type() == "mouse" || !event.is_primary() || !on_handle(&event, &lift) {
            return;
        }
        // A primary contact going down ends any lift whose release never arrived.
        if TOUCH_DRAG.with_borrow_mut(PointerDragSession::clear) {
            touch_lift::clear();
        }
        TOUCH_DRAG.with_borrow_mut(|session| {
            session.begin(
                enabled,
                lift.clone(),
                event.pointer_id(),
                event.client_x(),
                event.client_y(),
                revision,
            )
        });
    })
}

/// Whether the contact went down on a form control.
fn on_control(event: &PointerEvent) -> bool {
    event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        .and_then(|element| {
            element
                .closest("input, button, select, textarea")
                .ok()
                .flatten()
        })
        .is_some()
}

/// Whether the contact went down on the handle of the row lifting `lift`,
/// rather than on a row nested inside it or a control a handle carries.
fn on_handle(event: &PointerEvent, lift: &str) -> bool {
    if on_control(event) {
        return false;
    }
    let Some(handle) = event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        .and_then(|element| element.closest(&format!("[{HANDLE}]")).ok().flatten())
    else {
        return false;
    };
    handle
        .closest(&format!("[{LIFT}]"))
        .ok()
        .flatten()
        .and_then(|row| row.get_attribute(LIFT))
        .is_some_and(|row| row == lift)
}

fn touch_movement<G, M>(gesture: &Gesture<G, M>, lift: String) -> Callback<PointerEvent>
where
    G: Clone + Eq + Display + FromStr + 'static,
    M: Clone + Eq + Display + FromStr + 'static,
{
    let gesture = gesture.clone();
    Callback::from(move |event: PointerEvent| {
        if TOUCH_DRAG
            .with_borrow_mut(|session| session.reconcile(gesture.enabled, Some(gesture.revision)))
        {
            touch_lift::clear();
            return;
        }
        let Some(first) = TOUCH_DRAG.with_borrow_mut(|session| {
            let active = session.movement(
                &lift,
                event.pointer_id(),
                event.client_x(),
                event.client_y(),
                gesture.revision,
            )?;
            let first = !active.dragging;
            active.dragging = true;
            Some(first)
        }) else {
            return;
        };
        let Some(handle) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        else {
            return;
        };
        if first
            && (handle.set_pointer_capture(event.pointer_id()).is_err()
                || touch_lift::begin(
                    &handle,
                    &format!("[{LIFT}]"),
                    LIFT,
                    event.client_x(),
                    event.client_y(),
                )
                .is_none())
        {
            TOUCH_DRAG.with_borrow_mut(PointerDragSession::clear);
            touch_lift::clear();
            return;
        }
        event.stop_propagation();
        event.prevent_default();
        touch_lift::follow(&handle, event.client_x(), event.client_y());
        let landing = gesture
            .lift(&lift)
            .and_then(|lifted| gesture.landing_at(&lifted, event.client_x(), event.client_y()));
        touch_lift::preview(landing.as_ref().map(|(_, element)| element));
    })
}

fn touch_finish<G, M>(gesture: &Gesture<G, M>, lift: String) -> Callback<PointerEvent>
where
    G: Clone + Eq + Display + FromStr + 'static,
    M: Clone + Eq + Display + FromStr + 'static,
{
    let gesture = gesture.clone();
    Callback::from(move |event: PointerEvent| {
        let finished = TOUCH_DRAG.with_borrow_mut(|session| {
            if session.reconcile(gesture.enabled, Some(gesture.revision)) {
                return None;
            }
            session.finish(&lift, event.pointer_id(), gesture.revision)
        });
        if !finished.is_some_and(|active| active.dragging) {
            touch_lift::clear();
            return;
        }
        let landing = gesture
            .lift(&lift)
            .and_then(|lifted| gesture.landing_at(&lifted, event.client_x(), event.client_y()));
        touch_lift::clear();
        SWALLOW_CLICK.set(true);
        event.stop_propagation();
        event.prevent_default();
        if let Some((change, _)) = landing {
            gesture.land(change);
        }
    })
}

fn touch_cancel(lift: String) -> Callback<PointerEvent> {
    Callback::from(move |event: PointerEvent| {
        if TOUCH_DRAG.with_borrow_mut(|session| session.cancel(&lift, event.pointer_id())) {
            touch_lift::clear();
        }
    })
}
fn swallow_drag_click() -> Callback<MouseEvent> {
    Callback::from(|event: MouseEvent| {
        if SWALLOW_CLICK.replace(false) {
            event.prevent_default();
            event.stop_propagation();
        }
    })
}
