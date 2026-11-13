use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::Closure;
use wire::CanonicalUuid;
use yew::prelude::*;

use crate::AppRuntime;

use super::SurfaceData;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Status {
    pub(super) target: String,
    pub(super) available: bool,
}

/// A card's plane and how far the card's top edge sits below the plane's.
fn plane_offset(card: &web_sys::Element) -> Option<(web_sys::Element, f64)> {
    let plane = card.closest(".lane-mosaic-plane").ok()??;
    let offset = card.get_bounding_client_rect().top() - plane.get_bounding_client_rect().top();
    Some((plane, offset))
}

/// Record where a plain quote link leaves its containing Chat card.
pub(super) fn remember_quote_return(event: &web_sys::MouseEvent) {
    let _ = remember_return(event, "a.message-quote-age[href]", ".mosaic-message-card");
}

/// Whether a native anchor click will navigate this tab.
pub(super) fn follows_in_tab(event: &web_sys::MouseEvent) -> bool {
    !event.default_prevented()
        && event.button() == 0
        && !event.meta_key()
        && !event.ctrl_key()
        && !event.shift_key()
        && !event.alt_key()
}

fn remember_return(
    event: &web_sys::MouseEvent,
    link_selector: &str,
    origin_selector: &str,
) -> bool {
    if !follows_in_tab(event) {
        return false;
    }
    let Some(link) = event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        .and_then(|target| target.closest(link_selector).ok().flatten())
    else {
        return false;
    };
    let Some(origin) = link.closest(origin_selector).ok().flatten() else {
        return false;
    };
    let Some(window) = web_sys::window() else {
        return false;
    };
    // A link to the current entry navigates nowhere, so nothing is left behind.
    if origin.id().is_empty() || link.get_attribute("href") == window.location().hash().ok() {
        return false;
    }
    let Some((plane, offset)) = plane_offset(&origin) else {
        return false;
    };
    let Some(key) = plane.get_attribute("data-agent-plane") else {
        return false;
    };
    super::navigation::presentation::record_departure(crate::view::ChatAnchor {
        plane: key,
        card: origin.id(),
        offset,
        focus: true,
    });
    true
}

/// Keeps a routed card where its route brought it while staged history grows
/// its lane plane around it. The plane disables browser scroll anchoring, so
/// without this a cold permalink load drifts off its card as older cards land
/// above it. The first scroll the pin did not make belongs to the reader and
/// releases it.
struct Pin {
    observer: web_sys::ResizeObserver,
    _aligned: Closure<dyn FnMut(js_sys::Array, web_sys::ResizeObserver)>,
}

impl Pin {
    fn hold(card: &web_sys::Element) -> Option<Self> {
        let (plane, offset) = plane_offset(card)?;
        let track = card.closest(".lane-mosaic-track").ok()??;
        let expected = Cell::new(plane.scroll_top());
        let card = card.clone();
        let aligned = Closure::<dyn FnMut(js_sys::Array, web_sys::ResizeObserver)>::new(
            move |_, observer: web_sys::ResizeObserver| {
                if plane.scroll_top() != expected.get() || !card.is_connected() {
                    observer.disconnect();
                    return;
                }
                if let Some((_, now)) = plane_offset(&card) {
                    plane.scroll_by_with_x_and_y(0.0, now - offset);
                }
                expected.set(plane.scroll_top());
            },
        );
        let observer = web_sys::ResizeObserver::new(aligned.as_ref().unchecked_ref()).ok()?;
        observer.observe(&track);
        Some(Self {
            observer,
            _aligned: aligned,
        })
    }
}

impl Drop for Pin {
    fn drop(&mut self) {
        self.observer.disconnect();
    }
}

/// Bring a route's element into view and focus, report it reached, and pin it.
/// Focus carries no visible ring: the arrival highlight already shows where the
/// route landed and fades, while keyboard navigation continues from the card.
fn reach(
    element: &web_sys::Element,
    target: &str,
    status: &UseStateHandle<Option<Status>>,
) -> Option<Pin> {
    element.scroll_into_view();
    if let Some(element) = element.dyn_ref::<web_sys::HtmlElement>() {
        let options = web_sys::FocusOptions::new();
        options.set_focus_visible(false);
        let _ = element.focus_with_options(&options);
    }
    status.set(Some(Status {
        target: target.to_owned(),
        available: true,
    }));
    Pin::hold(element)
}

/// Layout phase attribute; a `measuring` card is hidden until its lattice commits.
const LAYOUT_PHASE: &str = "data-layout-phase";

/// The rendered element a route names. A card counts only once its layout
/// commits: while measuring it is hidden, so it can neither be scrolled to
/// meaningfully nor take focus, and its node may be replaced as the layout commits.
/// Route targets are ASCII alphanumerics and `-`, so they quote verbatim.
fn routed_element(document: &web_sys::Document, target: &str) -> Option<web_sys::Element> {
    let nodes = document
        .query_selector_all(&format!("[id=\"{target}\"]"))
        .ok()?;
    (0..nodes.length())
        .filter_map(|index| nodes.item(index)?.dyn_into::<web_sys::Element>().ok())
        .find(|element| element.get_attribute(LAYOUT_PHASE).as_deref() != Some("measuring"))
}

/// Watches the document for a routed element that has not rendered yet.
/// Staged hydration renders cards without advancing the stream cursor, so a
/// cold permalink otherwise waits on an unrelated live event to find its card.
struct Seek {
    observer: web_sys::MutationObserver,
    _found: Closure<dyn FnMut(js_sys::Array, web_sys::MutationObserver)>,
    /// The card this seek marked selected, outside the route effect's care.
    marked: Rc<RefCell<Option<web_sys::Element>>>,
}

impl Seek {
    fn start(
        document: &web_sys::Document,
        target: &str,
        status_target: &str,
        status: &UseStateHandle<Option<Status>>,
        pin: &Rc<RefCell<Option<Pin>>>,
    ) -> Option<Self> {
        let body = document.body()?;
        let marked = Rc::new(RefCell::new(None));
        let (document, target, status_target, status, pin, mark) = (
            document.clone(),
            target.to_owned(),
            status_target.to_owned(),
            status.clone(),
            Rc::clone(pin),
            Rc::clone(&marked),
        );
        let found = Closure::<dyn FnMut(js_sys::Array, web_sys::MutationObserver)>::new(
            move |_, observer: web_sys::MutationObserver| {
                let Some(element) = routed_element(&document, &target) else {
                    return;
                };
                observer.disconnect();
                if route_selection(&element) {
                    let _ = element.set_attribute("data-card-route-selected", "true");
                    *mark.borrow_mut() = Some(element.clone());
                }
                *pin.borrow_mut() = reach(&element, &status_target, &status);
            },
        );
        let observer = web_sys::MutationObserver::new(found.as_ref().unchecked_ref()).ok()?;
        let options = web_sys::MutationObserverInit::new();
        options.set_child_list(true);
        options.set_subtree(true);
        // A rendered card becomes reachable when its layout commits.
        options.set_attribute_filter(&js_sys::Array::of1(&LAYOUT_PHASE.into()));
        observer.observe_with_options(&body, &options).ok()?;
        Some(Self {
            observer,
            _found: found,
            marked,
        })
    }
}

impl Drop for Seek {
    fn drop(&mut self) {
        self.observer.disconnect();
        if let Some(element) = self.marked.borrow_mut().take() {
            let _ = element.remove_attribute("data-card-route-selected");
        }
    }
}

/// One route target being brought into view: sought until it renders, then
/// pinned until the reader scrolls. Dropped when the route moves on.
struct Hold {
    target: String,
    _seek: Option<Seek>,
    _pin: Rc<RefCell<Option<Pin>>>,
}

/// The latest visible card before one task-creation boundary.
fn creator_message_target(
    creator: &crate::view::CreatorContextTarget,
    runtime: &AppRuntime,
) -> Option<String> {
    runtime
        .messages()
        .iter()
        .filter(|message| creator.contains(message))
        .max_by_key(|message| {
            (
                message.locus.member,
                message.locus.offset,
                message.locus.event,
            )
        })
        .map(|message| super::watchband::agent_message_source_id(message))
}

#[derive(PartialEq)]
struct TargetResolution {
    ready: bool,
    routed: Option<String>,
    history: Option<wire::MessageHistoryCursor>,
    creator: bool,
    /// Both generations make another completed page re-run the route effect.
    _revision: u64,
    _completion: u64,
}

fn target_resolution(target: Option<&str>, surface: &SurfaceData) -> TargetResolution {
    let surface = surface.read();
    let runtime = surface.as_ref();
    let creator = target.and_then(crate::view::CreatorContextTarget::parse);
    let routed = target.and_then(|target| {
        creator.as_ref().map_or_else(
            || Some(target.to_owned()),
            |creator| runtime.and_then(|runtime| creator_message_target(creator, runtime)),
        )
    });
    let history = creator
        .as_ref()
        .filter(|_| routed.is_none())
        .and_then(|creator| {
            runtime.and_then(|runtime| runtime.message_history_target(creator.lane))
        });
    TargetResolution {
        ready: runtime.and_then(AppRuntime::lanes).is_some(),
        routed,
        history,
        creator: creator.is_some(),
        _revision: runtime.map_or(0, AppRuntime::message_history_revision),
        _completion: creator.as_ref().map_or(0, |creator| {
            runtime.map_or(0, |runtime| {
                runtime.message_history_completion(creator.lane)
            })
        }),
    }
}
/// A hidden team whose primary composer this route opens. Revealing from the
/// route effect covers every lane link and direct lane URL without coupling
/// each link renderer to browser-local team state.
fn hidden_team_target(
    target: Option<&str>,
    surface: &SurfaceData,
    controls: &super::TeamControls,
) -> Option<CanonicalUuid> {
    let lane = target?
        .strip_prefix(super::watchband::PRIMARY_TEXTAREA_PREFIX)?
        .parse()
        .ok()?;
    let surface = surface.read();
    let team = surface
        .as_ref()?
        .lanes()?
        .lanes
        .iter()
        .find(|team| team.members.iter().any(|member| member.lane == lane))?
        .lane;
    controls.is_hidden(team).then_some(team)
}

/// Whether retained Chat state holds the card a route names, once the lane
/// snapshot and a transcript generation are admitted. Every seated team has
/// a revealable Chat surface. A creator-context target stays unsettled while
/// the lane can still page older history.
fn retained(target: &str, runtime: &AppRuntime) -> Option<bool> {
    let lanes = runtime.lanes()?;
    runtime.message_generation()?;
    if let Some(creator) = crate::view::CreatorContextTarget::parse(target) {
        if creator_message_target(&creator, runtime).is_some() {
            return Some(true);
        }
        return runtime
            .message_history_target(creator.lane)
            .is_none()
            .then_some(false);
    }
    let chat_member = |lane: CanonicalUuid| {
        lanes
            .lanes
            .iter()
            .any(|team| team.members.iter().any(|member| member.lane == lane))
    };
    if let Some(team) = crate::view::team_stream_card_team(target) {
        return Some(lanes.chat_target_for_team(team).is_some());
    }
    if let Some(lane) = target.strip_prefix(super::watchband::PRIMARY_TEXTAREA_PREFIX) {
        return Some(lane.parse().is_ok_and(chat_member));
    }
    if let Some(seq) = target.strip_prefix(super::watchband::LIFECYCLE_CARD_PREFIX) {
        return Some(
            runtime
                .lifecycle_boundaries()
                .iter()
                .any(|boundary| chat_member(boundary.lane()) && boundary.seq().to_string() == seq),
        );
    }
    Some(runtime.messages().iter().any(|message| {
        chat_member(message.lane) && super::watchband::agent_message_source_id(message) == target
    }))
}

/// Whether retained Chat state already holds `card`, so its route resolves
/// now. A sent direction awaiting delivery renders a card Chat does not yet
/// retain.
pub(super) fn retains(surface: &SurfaceData, card: &str) -> bool {
    surface
        .read()
        .as_ref()
        .and_then(|runtime| retained(card, runtime))
        == Some(true)
}

/// Settled retention of the route's card target, recomputed only when the
/// retained Chat state it reads changes.
#[hook]
pub(super) fn use_retention(target: Option<&str>, surface: &SurfaceData) -> Option<bool> {
    let revision = {
        let surface = surface.read();
        surface.as_ref().map(|runtime| {
            let creator = target
                .and_then(crate::view::CreatorContextTarget::parse)
                .map_or(0, |creator| {
                    runtime.message_history_completion(creator.lane)
                });
            (
                runtime
                    .lanes()
                    .map(|lanes| (lanes.epoch, lanes.topology_revision)),
                runtime.message_generation(),
                runtime.message_history_revision(),
                runtime.messages().len(),
                runtime.lifecycle_boundaries().len(),
                creator,
            )
        })
    };
    let surface = surface.clone();
    *use_memo((target.map(str::to_owned), revision), move |(target, _)| {
        let surface = surface.read();
        target
            .as_deref()
            .zip(surface.as_ref())
            .and_then(|(target, runtime)| retained(target, runtime))
    })
}

/// Focus a card named by the current route once retained Chat state is ready.
/// A returned-to entry's reading positions are the presentation store's to
/// restore; a target route alone reaches.
///
/// Creator-context targets resolve to the latest card before their captured
/// transcript boundary. If that card predates the loaded window, request the
/// owning lane's history until a page crosses the boundary.
#[hook]
pub(super) fn use_target(
    target: Option<String>,
    surface: &SurfaceData,
    sender: &super::SenderHandle,
    team_controls: &UseReducerHandle<super::TeamControls>,
    status: UseStateHandle<Option<Status>>,
    replace_route: Callback<super::AppRoute>,
) {
    let held = use_mut_ref(|| None::<Hold>);
    let resolution = target_resolution(target.as_deref(), surface);
    let hidden_team = hidden_team_target(target.as_deref(), surface, team_controls);
    let sender = sender.clone();
    let team_controls = team_controls.clone();
    use_effect_with(
        (target, resolution, hidden_team),
        move |(target, resolution, hidden_team)| {
            let mut selected = None;
            let window = web_sys::window();
            let document = window.as_ref().and_then(web_sys::Window::document);
            if held
                .borrow()
                .as_ref()
                .is_some_and(|hold| Some(hold.target.as_str()) != target.as_deref())
            {
                held.borrow_mut().take();
            }
            if let Some(requested) = target.as_deref() {
                if let Some(team) = hidden_team {
                    if status
                        .as_ref()
                        .is_some_and(|known| known.target == requested && known.available)
                    {
                        // A reached permalink must not undo a later deliberate hide.
                        // Release it so following the lane link again is a new reveal.
                        replace_route.emit(super::AppRoute::Chat { card: None });
                    } else {
                        team_controls.dispatch(super::TeamControlAction::ShowTeam { team: *team });
                    }
                }
                if resolution.ready {
                    if let Some(target) = resolution.routed.as_deref() {
                        let route = Route {
                            target,
                            status_target: requested,
                            document: document.as_ref(),
                            status: &status,
                        };
                        selected = route.reach(&held);
                    } else if resolution.creator {
                        held.borrow_mut().take();
                        if let Some(cursor) = &resolution.history {
                            if status
                                .as_ref()
                                .is_some_and(|known| known.target == requested)
                            {
                                status.set(None);
                            }
                            let _ = super::admit_action(
                                &sender,
                                super::ClientAction::MessageHistory {
                                    generation: cursor.generation,
                                    lane: cursor.lane,
                                    before: cursor.before,
                                },
                            );
                        } else if status
                            .as_ref()
                            .is_none_or(|known| known.target != requested || known.available)
                        {
                            status.set(Some(Status {
                                target: requested.to_owned(),
                                available: false,
                            }));
                        }
                    }
                }
            } else if status.is_some() {
                status.set(None);
            }
            move || {
                if let Some(element) = selected {
                    let _ = element.remove_attribute("data-card-route-selected");
                }
            }
        },
    );
}

/// One visible route target once its destination is ready.
struct Route<'a> {
    /// Concrete rendered card ID.
    target: &'a str,
    /// Stable URL target, including a creator-context boundary.
    status_target: &'a str,
    document: Option<&'a web_sys::Document>,
    status: &'a UseStateHandle<Option<Status>>,
}

impl Route<'_> {
    /// Reach, seek, or settle the route's element. Returns an element marked
    /// as the route's selection.
    fn reach(&self, held: &Rc<RefCell<Option<Hold>>>) -> Option<web_sys::Element> {
        let (target, status_target, status) = (self.target, self.status_target, self.status);
        let element = self
            .document
            .and_then(|document| routed_element(document, target));
        let selected = element
            .as_ref()
            .filter(|element| route_selection(element))
            .cloned();
        if let Some(selected) = &selected {
            let _ = selected.set_attribute("data-card-route-selected", "true");
        }
        let settled = status
            .as_ref()
            .is_some_and(|settled| settled.target == status_target && settled.available);
        if !settled {
            if let Some(element) = element {
                let pin = Rc::new(RefCell::new(reach(&element, status_target, status)));
                *held.borrow_mut() = Some(Hold {
                    target: status_target.to_owned(),
                    _seek: None,
                    _pin: pin,
                });
            } else {
                status.set(Some(Status {
                    target: status_target.to_owned(),
                    available: false,
                }));
                if held.borrow().is_none()
                    && let Some(document) = self.document
                {
                    let pin = Rc::new(RefCell::new(None));
                    let seek = Seek::start(document, target, status_target, status, &pin);
                    *held.borrow_mut() = Some(Hold {
                        target: status_target.to_owned(),
                        _seek: seek,
                        _pin: pin,
                    });
                }
            }
        }
        selected
    }
}

fn route_selection(element: &web_sys::Element) -> bool {
    element
        .matches(".mosaic-message-card, .team-direction-ledger-row")
        .unwrap_or(false)
}
