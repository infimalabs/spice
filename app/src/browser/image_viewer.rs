//! One image-only top-layer viewer for Markdown, diagrams, and attachments.
//!
//! The viewer lives only while its opener could take focus back: it closes
//! when the opener leaves the document, stops rendering, or falls inside an
//! inert subtree, which is how a route change or a hidden owner ends it. An
//! image a message card or task retains opens through its durable address and
//! closes when the route leaves that address; every other image is local
//! viewer state the route never sees. Every close path converges on one
//! generation-checked clear, so stale close events and repeated dismissals
//! are no-ops.

use std::rc::Rc;

use wasm_bindgen::prelude::*;
use yew::prelude::*;

use super::{SurfaceData, card_target, navigation, overlay};
use crate::image_address::{self, Locator, Owner, Pick};
use crate::image_navigation::{self, Direction};
use crate::view::{AppRoute, ImageTarget};

mod actions;
mod editor;

use super::watchband::{ComposerAction, ComposerDrafts};

#[derive(Clone, PartialEq)]
struct DraftTarget {
    lane: String,
    quote: Option<String>,
    id: u64,
}

#[derive(Clone, PartialEq)]
struct Selection {
    generation: u64,
    source: AttrValue,
    alt: AttrValue,
    name: AttrValue,
    opener: web_sys::Element,
    draft: Option<DraftTarget>,
    target: Option<ImageTarget>,
}

#[derive(Default, PartialEq)]
struct Viewing {
    issued: u64,
    current: Option<Selection>,
    /// The addressed image the latest close left, until another opens.
    closed: Option<(u64, ImageTarget)>,
}

enum Change {
    Open {
        source: AttrValue,
        alt: AttrValue,
        name: AttrValue,
        opener: web_sys::Element,
        draft: Option<DraftTarget>,
        target: Option<ImageTarget>,
    },
    Close(u64),
}

impl Reducible for Viewing {
    type Action = Change;

    fn reduce(self: Rc<Self>, change: Change) -> Rc<Self> {
        match change {
            Change::Open {
                source,
                alt,
                name,
                opener,
                draft,
                target,
            } => {
                let generation = self.issued + 1;
                Rc::new(Self {
                    issued: generation,
                    current: Some(Selection {
                        generation,
                        source,
                        alt,
                        name,
                        opener,
                        draft,
                        target,
                    }),
                    closed: None,
                })
            }
            Change::Close(generation)
                if self
                    .current
                    .as_ref()
                    .is_some_and(|current| current.generation == generation) =>
            {
                Rc::new(Self {
                    issued: self.issued,
                    current: None,
                    closed: self
                        .current
                        .as_ref()
                        .and_then(|current| current.target.clone())
                        .map(|target| (generation, target)),
                })
            }
            Change::Close(_) => self,
        }
    }
}
const LANE_CARD_SELECTOR: &str = ".mosaic-message-card[data-agent-lane][data-mosaic-card-key]";
const LANE_CARD_KEY: &str = "data-mosaic-card-key";
/// An image a message card or task retains. A composer draft's image carries
/// `data-image-lane` and is never one, wherever its composer renders.
const IMAGE_BUTTON_SELECTOR: &str =
    "button[data-image-preview]:not([disabled]):not([data-image-lane])";
const TASK_OWNER_SELECTOR: &str = ".work-task-main[data-image-owner-task]";
/// The task detail its task's own address renders, where an address resolves.
const TASK_ADDRESSED_SELECTOR: &str =
    ".work-task-main[data-image-owner-task][data-image-owner-addressed]";
const TASK_OWNER_KEY: &str = "data-image-owner-task";
const TASK_ADDRESSED_KEY: &str = "data-image-owner-addressed";
/// One composer: its main draft strip and every quote shard's strip.
const COMPOSER_SELECTOR: &str = ".agent-composer-stack";
/// An image pasted into a composer draft, as its strip renders it.
const COMPOSER_IMAGE_SELECTOR: &str =
    "button.composer-shard-image-open[data-image-preview]:not([disabled])";

/// The rendered card or task retaining an image, with the key naming it.
struct Retainer {
    root: web_sys::Element,
    key: String,
    card: bool,
}

impl Retainer {
    fn of(opener: &web_sys::Element) -> Option<Self> {
        if let Some(root) = opener.closest(LANE_CARD_SELECTOR).ok().flatten() {
            let key = root.get_attribute(LANE_CARD_KEY)?;
            return Some(Self {
                root,
                key,
                card: true,
            });
        }
        let root = opener.closest(TASK_OWNER_SELECTOR).ok().flatten()?;
        let key = root.get_attribute(TASK_OWNER_KEY)?;
        Some(Self {
            root,
            key,
            card: false,
        })
    }

    fn owner(&self) -> Owner<'_> {
        if self.card {
            Owner::Card(&self.key)
        } else {
            Owner::Task(&self.key)
        }
    }
}

/// The durable address of `opener`'s image, reached from `current`. A
/// composer draft's image, an image nothing retains, and an image on a card
/// Chat does not yet retain have none: they open as local viewer state.
fn address(
    opener: &web_sys::Element,
    current: &AppRoute,
    surface: &SurfaceData,
) -> Option<navigation::ImageRequest> {
    if !opener.matches(IMAGE_BUTTON_SELECTOR).unwrap_or(false) {
        return None;
    }
    let retainer = Retainer::of(opener)?;
    let owner = retainer.owner();
    if let Owner::Card(card) = owner
        && !card_target::retains(surface, card)
    {
        return None;
    }
    let identity = opener
        .query_selector("img")
        .ok()
        .flatten()?
        .get_attribute("data-media-identity");
    let pick = if let Some(identity) = identity.as_deref() {
        Pick::Identity(identity)
    } else {
        let images = retainer
            .root
            .query_selector_all(IMAGE_BUTTON_SELECTOR)
            .ok()?;
        Pick::Ordinal(
            (0..images.length() as usize)
                .find(|index| element_at(&images, *index).is_some_and(|item| item == *opener))?,
        )
    };
    Some(navigation::ImageRequest {
        route: owner.route(current)?,
        target: Locator { owner, pick }.target()?,
    })
}

enum Resolution {
    Found(web_sys::Element),
    Pending,
    Missing,
}

/// The rendered image button `target` names: pending until its owner renders
/// at its own address, missing once that owner renders without it.
fn resolve_target(target: &ImageTarget) -> Resolution {
    locate(target, TASK_ADDRESSED_SELECTOR)
}

/// The image button `target` names among rendered cards and the task details
/// `tasks` selects.
fn locate(target: &ImageTarget, tasks: &'static str) -> Resolution {
    let Some(locator) = Locator::parse(target) else {
        return Resolution::Missing;
    };
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return Resolution::Pending;
    };
    let (selector, attribute, key) = match locator.owner {
        Owner::Card(card) => (LANE_CARD_SELECTOR, LANE_CARD_KEY, card),
        Owner::Task(task) => (tasks, TASK_OWNER_KEY, task),
    };
    let Ok(roots) = document.query_selector_all(selector) else {
        return Resolution::Pending;
    };
    let Some(root) = (0..roots.length() as usize)
        .filter_map(|index| element_at(&roots, index))
        .find(|root| root.get_attribute(attribute).as_deref() == Some(key))
    else {
        return Resolution::Pending;
    };
    let Ok(images) = root.query_selector_all(IMAGE_BUTTON_SELECTOR) else {
        return Resolution::Missing;
    };
    let found = match locator.pick {
        Pick::Identity(identity) => (0..images.length() as usize)
            .filter_map(|index| element_at(&images, index))
            .find(|button| {
                button
                    .query_selector("img")
                    .ok()
                    .flatten()
                    .and_then(|image| image.get_attribute("data-media-identity"))
                    .as_deref()
                    == Some(identity)
            }),
        Pick::Ordinal(ordinal) => element_at(&images, ordinal),
    };
    found.map_or(Resolution::Missing, Resolution::Found)
}

/// Opening `opener`'s image: at `target` when its durable address opened it,
/// else as local viewer state.
fn open_change(opener: web_sys::Element, target: Option<ImageTarget>) -> Option<Change> {
    let image = opener
        .query_selector("img")
        .ok()
        .flatten()?
        .dyn_into::<web_sys::HtmlImageElement>()
        .ok()?;
    Some(Change::Open {
        source: image.src().into(),
        alt: image.alt().into(),
        name: opener
            .get_attribute("data-image-name")
            .unwrap_or_default()
            .into(),
        draft: opener.get_attribute("data-image-lane").and_then(|lane| {
            Some(DraftTarget {
                lane,
                quote: opener.get_attribute("data-image-quote"),
                id: opener.get_attribute("data-image-id")?.parse().ok()?,
            })
        }),
        opener,
        target,
    })
}

fn element_at(nodes: &web_sys::NodeList, index: usize) -> Option<web_sys::Element> {
    nodes
        .item(u32::try_from(index).ok()?)?
        .dyn_into::<web_sys::Element>()
        .ok()
}

fn lane_media_count(cards: &web_sys::NodeList, index: usize, lane: &str) -> usize {
    let Some(card) = element_at(cards, index)
        .filter(|card| card.get_attribute("data-agent-lane").as_deref() == Some(lane))
    else {
        return 0;
    };
    card.query_selector_all(IMAGE_BUTTON_SELECTOR)
        .map_or(0, |images| images.length() as usize)
}

/// The next lane image in time. Cards are newest first in the DOM while each
/// card's images, including a crystallized run, are oldest first.
fn adjacent_lane_media(
    opener: &web_sys::Element,
    direction: Direction,
) -> Option<web_sys::Element> {
    let card = opener.closest(LANE_CARD_SELECTOR).ok().flatten()?;
    let lane = card.get_attribute("data-agent-lane")?;
    let plane = card.closest(".lane-mosaic-plane").ok().flatten()?;
    let cards = plane.query_selector_all(LANE_CARD_SELECTOR).ok()?;
    let card_count = cards.length() as usize;
    let current_card = (0..card_count)
        .find(|index| element_at(&cards, *index).is_some_and(|candidate| candidate == card))?;
    let images = card.query_selector_all(IMAGE_BUTTON_SELECTOR).ok()?;
    let image_count = images.length() as usize;
    let current_image = (0..image_count)
        .find(|index| element_at(&images, *index).is_some_and(|candidate| candidate == *opener))?;
    let (target_card, target_image) = image_navigation::adjacent_position(
        card_count,
        current_card,
        current_image,
        direction,
        |index| lane_media_count(&cards, index, &lane),
    )?;
    element_at(&cards, target_card)
        .and_then(|card| card.query_selector_all(IMAGE_BUTTON_SELECTOR).ok())
        .and_then(|images| element_at(&images, target_image))
}

/// The neighbouring image pasted into the opener's composer, in rendered order
/// across its main strip and its quote shards' strips, holding at either end.
fn adjacent_composer_media(
    opener: &web_sys::Element,
    direction: Direction,
) -> Option<web_sys::Element> {
    let composer = opener.closest(COMPOSER_SELECTOR).ok().flatten()?;
    let images = composer.query_selector_all(COMPOSER_IMAGE_SELECTOR).ok()?;
    let count = images.length() as usize;
    let current = (0..count)
        .find(|index| element_at(&images, *index).is_some_and(|candidate| candidate == *opener))?;
    let (_, target) = image_navigation::adjacent_position(1, 0, current, direction, |_| count)?;
    element_at(&images, target)
}

/// What Left and Right step through for the image a viewer opened from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Navigation {
    /// The opener's lane's message images, in time.
    Lane,
    /// The images pasted into the opener's composer.
    Composer,
}

impl Navigation {
    fn of(opener: &web_sys::Element) -> Option<Self> {
        if opener.closest(LANE_CARD_SELECTOR).ok().flatten().is_some() {
            Some(Self::Lane)
        } else if opener.matches(COMPOSER_IMAGE_SELECTOR).unwrap_or(false)
            && opener.closest(COMPOSER_SELECTOR).ok().flatten().is_some()
        {
            Some(Self::Composer)
        } else {
            None
        }
    }

    fn adjacent(self, opener: &web_sys::Element, direction: Direction) -> Option<web_sys::Element> {
        match self {
            Self::Lane => adjacent_lane_media(opener, direction),
            Self::Composer => adjacent_composer_media(opener, direction),
        }
    }

    const fn help(self) -> &'static str {
        match self {
            Self::Lane => {
                "Use Left Arrow or swipe right for the previous older image in this lane, and Right Arrow or swipe left for the next newer image."
            }
            Self::Composer => {
                "Use Left Arrow or swipe right for the previous image pasted into this composer, and Right Arrow or swipe left for the next."
            }
        }
    }
}

#[derive(Properties, PartialEq)]
pub(super) struct ProviderProps {
    pub children: Children,
    pub drafts: UseReducerHandle<ComposerDrafts>,
    pub route: AppRoute,
    pub surface: SurfaceData,
    pub image: Option<ImageTarget>,
    pub open: Callback<navigation::ImageRequest>,
    pub replace: Callback<navigation::ImageRequest>,
    pub close: Callback<ImageTarget>,
    pub refuse: Callback<String>,
}

fn unavailable(image: &ImageTarget) -> String {
    format!(
        "Linked image `{}` is unavailable; kept its page open.",
        image.as_str()
    )
}

/// Opens the image the route addresses once its owner renders it. An address
/// over any route but its owner's own names nothing and is refused at once;
/// one whose rendered owner lacks the image is refused after it settles.
#[hook]
fn use_addressed_image(
    image: Option<ImageTarget>,
    route: &AppRoute,
    viewing: UseReducerDispatcher<Viewing>,
    refuse: Callback<String>,
) {
    let owned = image
        .as_ref()
        .is_some_and(|image| image_address::addressed_over(route, image));
    use_effect_with((image, owned), move |(image, owned)| {
        const TARGET_SETTLE_MILLIS: u32 = 5_000;

        let settled = Rc::new(std::cell::Cell::new(false));
        let deadline = Rc::new(std::cell::RefCell::new(
            None::<gloo_timers::callback::Timeout>,
        ));
        if let Some(image) = image.as_ref().filter(|_| !*owned) {
            refuse.emit(unavailable(image));
        }
        let resolve = image.clone().filter(|_| *owned).map(|image| {
            let settled = settled.clone();
            let deadline = deadline.clone();
            let viewing = viewing.clone();
            let refuse = refuse.clone();
            Rc::new(move || {
                if settled.get() {
                    return;
                }
                match resolve_target(&image) {
                    Resolution::Found(opener) => {
                        settled.set(true);
                        deadline.borrow_mut().take();
                        if let Some(change) = open_change(opener, Some(image.clone())) {
                            viewing.dispatch(change);
                        }
                    }
                    Resolution::Missing if deadline.borrow().is_none() => {
                        let settled = settled.clone();
                        let image = image.clone();
                        let viewing = viewing.clone();
                        let refuse = refuse.clone();
                        *deadline.borrow_mut() = Some(gloo_timers::callback::Timeout::new(
                            TARGET_SETTLE_MILLIS,
                            move || {
                                if settled.replace(true) {
                                    return;
                                }
                                if let Resolution::Found(opener) = resolve_target(&image)
                                    && let Some(change) = open_change(opener, Some(image.clone()))
                                {
                                    viewing.dispatch(change);
                                } else {
                                    refuse.emit(unavailable(&image));
                                }
                            },
                        ));
                    }
                    Resolution::Pending | Resolution::Missing => {}
                }
            }) as Rc<dyn Fn()>
        });
        let observer = resolve.as_ref().and_then(|resolve| {
            let observed = resolve.clone();
            let callback = Closure::<dyn FnMut()>::new(move || observed());
            let observer =
                web_sys::MutationObserver::new(callback.as_ref().unchecked_ref()).ok()?;
            let document = web_sys::window()?.document()?;
            let options = web_sys::MutationObserverInit::new();
            options.set_child_list(true);
            options.set_subtree(true);
            // A detail already rendered becomes its task's own address in place.
            options.set_attributes(true);
            options.set_attribute_filter(&js_sys::Array::of1(&TASK_ADDRESSED_KEY.into()));
            observer.observe_with_options(&document, &options).ok()?;
            resolve();
            Some((observer, callback))
        });
        move || {
            deadline.borrow_mut().take();
            if let Some((observer, _callback)) = observer {
                observer.disconnect();
            }
        }
    });
}

/// Opens whichever image button is activated: through its durable address
/// when it has one, else as local viewer state.
#[hook]
fn use_image_opening(
    viewing: UseReducerDispatcher<Viewing>,
    open: Callback<navigation::ImageRequest>,
    route: AppRoute,
    surface: SurfaceData,
) {
    // The one document listener outlives renders; it reads the latest route.
    let latest = use_mut_ref(|| (open.clone(), route.clone()));
    *latest.borrow_mut() = (open, route);
    use_effect_with((viewing, surface), move |(viewing, surface)| {
        let document = web_sys::window()
            .expect("app window")
            .document()
            .expect("app document");
        let viewing = viewing.clone();
        let surface = surface.clone();
        let opened = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
            let Some(opener) = event
                .target()
                .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                .and_then(|target| target.closest("button[data-image-preview]").ok().flatten())
            else {
                return;
            };
            // Emitting re-renders, which rewrites `latest`: release it first.
            let (open, route) = latest.borrow().clone();
            if let Some(request) = address(&opener, &route, &surface) {
                event.prevent_default();
                open.emit(request);
            } else if let Some(change) = open_change(opener, None) {
                event.prevent_default();
                viewing.dispatch(change);
            }
        });
        document
            .add_event_listener_with_callback("click", opened.as_ref().unchecked_ref())
            .expect("the app document accepts image activation");
        move || {
            let _ = document
                .remove_event_listener_with_callback("click", opened.as_ref().unchecked_ref());
        }
    });
}

/// How long the page under a closed viewer may take to render its image again.
const RETURN_SETTLE_MILLIS: u32 = 2_000;

/// Gives focus back to an addressed image whose page renders its button anew
/// once the viewer closes: an image opened from a Work list row closes back
/// to that list, which stands the row up again. Focus the opener or the
/// reader already holds is left alone.
#[hook]
fn use_focus_return(closed: Option<(u64, ImageTarget)>) {
    use_effect_with(closed, |closed| {
        let watch = closed.as_ref().and_then(|(_, target)| {
            let document = web_sys::window()?.document()?;
            let target = target.clone();
            let sought = document.clone();
            let seek = Rc::new(move || {
                let unfocused = sought
                    .active_element()
                    .is_none_or(|active| active.tag_name() == "BODY");
                if unfocused && let Resolution::Found(image) = locate(&target, TASK_OWNER_SELECTOR)
                {
                    overlay::return_focus(&image);
                }
            });
            let observed = seek.clone();
            let callback = Closure::<dyn FnMut()>::new(move || observed());
            let observer =
                web_sys::MutationObserver::new(callback.as_ref().unchecked_ref()).ok()?;
            let options = web_sys::MutationObserverInit::new();
            options.set_child_list(true);
            options.set_subtree(true);
            observer.observe_with_options(&document, &options).ok()?;
            seek();
            let settled = observer.clone();
            let deadline = gloo_timers::callback::Timeout::new(RETURN_SETTLE_MILLIS, move || {
                settled.disconnect();
            });
            Some((observer, callback, deadline))
        });
        move || {
            if let Some((observer, _callback, _deadline)) = watch {
                observer.disconnect();
            }
        }
    });
}

#[function_component(Provider)]
pub(super) fn provider(props: &ProviderProps) -> Html {
    let viewing = use_reducer_eq(Viewing::default);
    use_addressed_image(
        props.image.clone(),
        &props.route,
        viewing.dispatcher(),
        props.refuse.clone(),
    );
    use_image_opening(
        viewing.dispatcher(),
        props.open.clone(),
        props.route.clone(),
        props.surface.clone(),
    );
    use_focus_return(viewing.closed.clone());
    html! {
        <>
            {props.children.clone()}
            if let Some(selection) = viewing.current.as_ref() {
                <Viewer
                    key={selection.generation.to_string()}
                    selection={selection.clone()}
                    viewing={viewing.dispatcher()}
                    drafts={props.drafts.clone()}
                    route={props.route.clone()}
                    surface={props.surface.clone()}
                    image={props.image.clone()}
                    open={props.open.clone()}
                    replace={props.replace.clone()}
                    dismiss={props.close.clone()}
                />
            }
        </>
    }
}

/// Raw-Markdown portals need listeners on physical nodes rather than delegated Yew events.
#[hook]
pub(super) fn use_button(action: Callback<()>) -> NodeRef {
    let button = use_node_ref();
    use_effect_with((button.clone(), action), move |(button, action)| {
        let registration = button.cast::<web_sys::HtmlElement>().map(|button| {
            let action = action.clone();
            let callback = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| action.emit(()));
            button
                .add_event_listener_with_callback("click", callback.as_ref().unchecked_ref())
                .expect("the owned button accepts its click listener");
            (button, callback)
        });
        move || {
            if let Some((button, callback)) = registration {
                let _ = button.remove_event_listener_with_callback(
                    "click",
                    callback.as_ref().unchecked_ref(),
                );
            }
        }
    });
    button
}

#[derive(Properties, PartialEq)]
struct ViewerProps {
    selection: Selection,
    viewing: UseReducerDispatcher<Viewing>,
    drafts: UseReducerHandle<ComposerDrafts>,
    route: AppRoute,
    surface: SurfaceData,
    image: Option<ImageTarget>,
    open: Callback<navigation::ImageRequest>,
    replace: Callback<navigation::ImageRequest>,
    dismiss: Callback<ImageTarget>,
}

impl ViewerProps {
    /// The address this viewer opened at, while the route still names it.
    fn linked(&self) -> Option<&ImageTarget> {
        self.selection
            .target
            .as_ref()
            .filter(|target| self.image.as_ref() == Some(*target))
    }
}

fn allow_annotation_discard(dirty: bool, image: &str) -> bool {
    !dirty
        || web_sys::window()
            .and_then(|window| {
                window
                    .confirm_with_message(&format!(
                        "Discard the annotation marks not yet applied to {image}?"
                    ))
                    .ok()
            })
            .unwrap_or(false)
}

#[hook]
fn use_annotation_apply(
    target: Option<DraftTarget>,
    drafts: UseReducerHandle<ComposerDrafts>,
    editing: UseStateHandle<bool>,
    dirty: UseStateHandle<bool>,
    status: UseStateHandle<AttrValue>,
) -> Callback<editor::Applied> {
    let latest = use_mut_ref(|| drafts.clone());
    *latest.borrow_mut() = drafts;
    use_callback(
        (target, latest, editing, dirty, status),
        |applied: editor::Applied, (target, latest, editing, dirty, status)| {
            let Some(target) = target else {
                return;
            };
            let drafts = latest.borrow();
            if !drafts
                .image(&target.lane, target.quote.as_deref(), target.id)
                .is_some_and(|image| image.revision == applied.revision)
            {
                status
                    .set("This draft image changed or was removed. Reopen it to annotate.".into());
                dirty.set(false);
                editing.set(false);
                return;
            }
            drafts.dispatch(ComposerAction::ApplyImage {
                lane: target.lane.clone(),
                quote: target.quote.clone(),
                id: target.id,
                revision: applied.revision,
                media: applied.media,
                shapes: applied.shapes,
            });
            dirty.set(false);
            editing.set(false);
        },
    )
}

/// Steps the viewer to the opener's neighbouring image, holding at either end
/// and while annotating. Arrow keys and touch swipes both take this one step.
#[hook]
fn use_navigation(
    props: &ViewerProps,
    editing: UseStateHandle<bool>,
) -> (Option<Navigation>, Callback<Direction>) {
    let opener = props.selection.opener.clone();
    let navigation = Navigation::of(&opener);
    let step = use_callback(
        (
            opener,
            navigation,
            props.viewing.clone(),
            editing,
            (props.route.clone(), props.surface.clone()),
            props.linked().cloned(),
            (
                props.open.clone(),
                props.replace.clone(),
                props.dismiss.clone(),
            ),
        ),
        |direction: Direction,
         (
            opener,
            navigation,
            viewing,
            editing,
            (route, surface),
            linked,
            (open, replace, dismiss),
        )| {
            if **editing {
                return;
            }
            let Some(next) =
                navigation.and_then(|navigation| navigation.adjacent(opener, direction))
            else {
                return;
            };
            // One history entry holds the viewer's address: a step between
            // addressed images rewrites it, one onto an addressed image pushes
            // it, and one off to a local image returns through it.
            match (address(&next, route, surface), linked) {
                (Some(request), Some(_)) => replace.emit(request),
                (Some(request), None) => open.emit(request),
                (None, linked) => {
                    let Some(change) = open_change(next, None) else {
                        return;
                    };
                    viewing.dispatch(change);
                    if let Some(linked) = linked {
                        dismiss.emit(linked.clone());
                    }
                }
            }
        },
    );
    (navigation, step)
}

/// Left and Right Arrow without modifiers or composition; while annotating
/// they stay the editor's.
fn arrow_keys(step: Callback<Direction>, editing: bool) -> Callback<KeyboardEvent> {
    Callback::from(move |event: KeyboardEvent| {
        if editing
            || event.is_composing()
            || event.alt_key()
            || event.ctrl_key()
            || event.meta_key()
        {
            return;
        }
        let direction = match event.key().as_str() {
            "ArrowLeft" => Direction::Older,
            "ArrowRight" => Direction::Newer,
            _ => return,
        };
        event.prevent_default();
        event.stop_propagation();
        step.emit(direction);
    })
}

/// One primary touch in progress, until a second finger joins it.
#[derive(Clone, Copy)]
struct Touch {
    pointer: i32,
    x: f64,
    y: f64,
    multiple: bool,
}

/// The pointer listeners turning one-finger horizontal swipes into arrow
/// steps. Mouse and pen drags, any gesture another finger joins, and any
/// gesture begun while the page is pinch-zoomed never step; a gesture the
/// browser takes over for pinch or scroll is cancelled.
struct Swipes {
    down: Callback<web_sys::PointerEvent>,
    up: Callback<web_sys::PointerEvent>,
    cancel: Callback<web_sys::PointerEvent>,
}

/// Whether the page is pinch-zoomed, so one-finger travel pans the zoomed view.
fn zoomed() -> bool {
    web_sys::window()
        .and_then(|window| window.visual_viewport())
        .is_some_and(|viewport| viewport.scale() > 1.0)
}

/// Marks the dialog `data-zoomed` while the page is pinch-zoomed, where its
/// touch-action gives horizontal panning back to the browser.
#[hook]
fn use_zoom_mark(host: NodeRef) {
    use_effect_with(host, |host| {
        let viewport = web_sys::window().and_then(|window| window.visual_viewport());
        let listening = host
            .cast::<web_sys::Element>()
            .zip(viewport)
            .map(|(dialog, viewport)| {
                let mark = move || {
                    let _ = dialog.toggle_attribute_with_force("data-zoomed", zoomed());
                };
                mark();
                let resized = Closure::<dyn FnMut()>::new(mark);
                let _ = viewport
                    .add_event_listener_with_callback("resize", resized.as_ref().unchecked_ref());
                (viewport, resized)
            });
        move || {
            if let Some((viewport, resized)) = listening {
                let _ = viewport.remove_event_listener_with_callback(
                    "resize",
                    resized.as_ref().unchecked_ref(),
                );
            }
        }
    });
}

#[hook]
fn use_swipes(step: Callback<Direction>, host: NodeRef) -> Swipes {
    use_zoom_mark(host);
    let touch = use_mut_ref(|| None::<Touch>);
    let down = {
        let touch = touch.clone();
        Callback::from(move |event: web_sys::PointerEvent| {
            if event.pointer_type() != "touch" {
                return;
            }
            let mut touch = touch.borrow_mut();
            if event.is_primary() {
                *touch = (!zoomed()).then(|| Touch {
                    pointer: event.pointer_id(),
                    x: f64::from(event.client_x()),
                    y: f64::from(event.client_y()),
                    multiple: false,
                });
            } else if let Some(touch) = touch.as_mut() {
                touch.multiple = true;
            }
        })
    };
    let up = {
        let touch = touch.clone();
        Callback::from(move |event: web_sys::PointerEvent| {
            let started = *touch.borrow();
            let Some(started) = started.filter(|started| started.pointer == event.pointer_id())
            else {
                return;
            };
            touch.borrow_mut().take();
            if !started.multiple
                && let Some(direction) = image_navigation::swipe(
                    f64::from(event.client_x()) - started.x,
                    f64::from(event.client_y()) - started.y,
                )
            {
                step.emit(direction);
            }
        })
    };
    let cancel = Callback::from(move |event: web_sys::PointerEvent| {
        let mut touch = touch.borrow_mut();
        if touch.is_some_and(|touch| touch.pointer == event.pointer_id()) {
            *touch = None;
        }
    });
    Swipes { down, up, cancel }
}

/// The lightbox's single-letter shortcuts: A annotates and D downloads by
/// activating the matching button, so each key does exactly what clicking
/// does and nothing when that button is absent or disabled. The editor keeps
/// its own letters, and typing in a field or with a modifier never triggers.
#[hook]
fn use_action_keys(host: NodeRef, editing: UseStateHandle<bool>) -> Callback<KeyboardEvent> {
    use_callback((host, editing), |event: KeyboardEvent, (host, editing)| {
        if **editing
            || event.repeat()
            || event.is_composing()
            || event.alt_key()
            || event.ctrl_key()
            || event.meta_key()
            || event.shift_key()
            || overlay::typing(&event)
        {
            return;
        }
        let selector = match event.key().as_str() {
            "a" | "A" => "[data-image-edit]",
            "d" | "D" => "[data-image-download]",
            _ => return,
        };
        let Some(button) = host
            .cast::<web_sys::HtmlDialogElement>()
            .and_then(|dialog| dialog.query_selector(selector).ok().flatten())
            .and_then(|button| button.dyn_into::<web_sys::HtmlElement>().ok())
        else {
            return;
        };
        event.prevent_default();
        event.stop_propagation();
        button.click();
    })
}

/// The dialog's keyboard and touch: lane or composer navigation by arrows or
/// swipes, and the action letters. Says which navigation is offered, for its
/// description.
#[hook]
fn use_viewer_keys(
    props: &ViewerProps,
    host: NodeRef,
    editing: UseStateHandle<bool>,
) -> (Option<Navigation>, Callback<KeyboardEvent>, Swipes) {
    let (navigation, step) = use_navigation(props, editing.clone());
    let navigate = arrow_keys(step.clone(), *editing);
    let swipes = use_swipes(step, host.clone());
    let action_keys = use_action_keys(host, editing);
    let keydown = Callback::from(move |event: KeyboardEvent| {
        navigate.emit(event.clone());
        action_keys.emit(event);
    });
    (navigation, keydown, swipes)
}

/// Closes a viewer whose address the route has left: Back, a refused route,
/// or its own dismissal. It runs after the dialog shows, so a viewer mounted
/// off its address still closes through the one close path.
#[hook]
fn use_address_alignment(host: &NodeRef, stray: bool) {
    use_effect_with((host.clone(), stray), |(host, stray)| {
        if *stray && let Some(dialog) = host.cast::<web_sys::HtmlDialogElement>() {
            dialog.close();
        }
    });
}

/// Shows the dialog once for its selection. Its dismissal requests read the
/// latest route-bound callbacks, so a route change never reopens it.
#[hook]
fn use_presentation(props: &ViewerProps, host: &NodeRef, close: Callback<()>) {
    let lapse = {
        let linked = props.linked().cloned();
        let dismiss = props.dismiss.clone();
        Callback::from(move |()| {
            if let Some(target) = linked.as_ref() {
                dismiss.emit(target.clone());
            }
        })
    };
    let latest = use_mut_ref(|| (close.clone(), lapse.clone()));
    *latest.borrow_mut() = (close, lapse);
    use_effect_with(
        (host.clone(), props.selection.clone(), props.viewing.clone()),
        move |(host, selection, viewing)| {
            let dialog = host
                .cast::<web_sys::HtmlDialogElement>()
                .expect("the mounted viewer owns its dialog");
            let generation = selection.generation;
            let viewing = viewing.clone();
            // Emitting re-renders, which rewrites `latest`: release it first.
            let requested = {
                let latest = latest.clone();
                Callback::from(move |()| {
                    let (close, _) = latest.borrow().clone();
                    close.emit(());
                })
            };
            let lapsed = Callback::from(move |()| {
                let (_, lapse) = latest.borrow().clone();
                lapse.emit(());
            });
            let presentation = Presentation::show(
                dialog,
                selection.opener.clone(),
                Callback::from(move |()| viewing.dispatch(Change::Close(generation))),
                requested,
                lapsed,
            );
            move || drop(presentation)
        },
    );
}

#[hook]
fn use_editor_focus(host: &NodeRef, editing: bool) {
    use_effect_with((host.clone(), editing), |(host, editing)| {
        if !editing
            && let Some(dialog) = host.cast::<web_sys::HtmlDialogElement>()
            && dialog.open()
            && let Ok(Some(button)) = dialog.query_selector("[data-image-edit]")
            && let Some(button) = button.dyn_ref::<web_sys::HtmlElement>()
        {
            let _ = button.focus();
        }
    });
}

#[function_component(Viewer)]
fn viewer(props: &ViewerProps) -> Html {
    let host = use_node_ref();
    let image_status = use_state_eq(|| AttrValue::from("Loading image"));
    let editing = use_state_eq(|| false);
    let dirty = use_state_eq(|| false);
    let image = props
        .selection
        .draft
        .as_ref()
        .and_then(|target| {
            props
                .drafts
                .image(&target.lane, target.quote.as_deref(), target.id)
        })
        .cloned();
    let source = use_memo(
        (
            props.selection.source.clone(),
            image.as_ref().map(|image| image.media.clone()),
        ),
        |(fallback, media)| {
            media.as_ref().map_or_else(
                || fallback.clone(),
                |media| AttrValue::from(media.data_url.clone()),
            )
        },
    );
    let ondirty = use_callback(dirty.clone(), |value: bool, dirty| dirty.set(value));
    let edit = use_callback((editing.clone(), dirty.clone()), |(), (editing, dirty)| {
        dirty.set(false);
        editing.set(true);
    });
    let (cancel, close) = use_annotation_discard(props, &host, &editing, &dirty);
    let apply = use_annotation_apply(
        props.selection.draft.clone(),
        props.drafts.clone(),
        editing.clone(),
        dirty.clone(),
        image_status.clone(),
    );
    let loaded = use_callback(image_status.clone(), |_: Event, status| {
        status.set(AttrValue::default())
    });
    let failed = use_callback(image_status.clone(), |_: Event, status| {
        status.set("Image unavailable".into())
    });
    let (navigation, keydown, swipes) = use_viewer_keys(props, host.clone(), editing.clone());
    let link = props
        .linked()
        .and_then(|_| web_sys::window()?.location().href().ok())
        .map(AttrValue::from);
    let stray = props.selection.target.is_some() && props.linked().is_none();
    use_presentation(props, &host, close.clone());
    use_address_alignment(&host, stray);
    use_editor_focus(&host, *editing);
    html! {
        <dialog
            ref={host}
            class="image-lightbox app-modal"
            aria-label="Full-size image"
            aria-describedby={navigation.is_some().then_some("image-lightbox-navigation-help")}
            onkeydown={keydown}
            onpointerdown={swipes.down}
            onpointerup={swipes.up}
            onpointercancel={swipes.cancel}
        >
            if let Some(navigation) = navigation {
                <span id="image-lightbox-navigation-help" class="visually-hidden">
                    {navigation.help()}
                </span>
            }
            if *editing && let Some(image) = image {
                <editor::Editor {image} onapply={apply} oncancel={cancel} {ondirty} />
            } else {
                <actions::Actions source={(*source).clone()} name={props.selection.name.clone()} link={link} onclose={close} image_status={(*image_status).clone()} onedit={image.is_some().then_some(edit)} />
                <img class="image-lightbox-image" src={(*source).clone()} alt={props.selection.alt.clone()} onload={loaded} onerror={failed} />
            }
        </dialog>
    }
}

/// The edit guard's discard path: the unsent inventory learns of unapplied
/// marks, and leaving the editor or the viewer confirms against the image
/// they belong to. Returns the editor's cancel and the viewer's close.
#[hook]
fn use_annotation_discard(
    props: &ViewerProps,
    host: &NodeRef,
    editing: &UseStateHandle<bool>,
    dirty: &UseStateHandle<bool>,
) -> (Callback<()>, Callback<()>) {
    let latest_dirty = use_mut_ref(|| false);
    *latest_dirty.borrow_mut() = **dirty;
    let image = if props.selection.name.is_empty() {
        AttrValue::from("this image")
    } else {
        props.selection.name.clone()
    };
    super::unsent::use_annotation_guard(**dirty, image.clone());
    let cancel = use_callback(
        (
            editing.clone(),
            dirty.clone(),
            latest_dirty.clone(),
            image.clone(),
        ),
        |(), (editing, dirty, latest_dirty, image)| {
            if allow_annotation_discard(*latest_dirty.borrow(), image) {
                dirty.set(false);
                editing.set(false);
            }
        },
    );
    let close = use_callback(
        (
            host.clone(),
            latest_dirty,
            dirty.clone(),
            image,
            props.linked().cloned(),
            props.dismiss.clone(),
        ),
        |(), (host, latest_dirty, dirty, image, linked, dismiss)| {
            if allow_annotation_discard(*latest_dirty.borrow(), image) {
                dirty.set(false);
                // An addressed viewer closes by leaving its address; any
                // other closes itself.
                if let Some(linked) = linked.as_ref() {
                    dismiss.emit(linked.clone());
                } else if let Some(dialog) = host.cast::<web_sys::HtmlDialogElement>() {
                    dialog.close();
                }
            }
        },
    );
    (cancel, close)
}

/// One modal showing and the listeners that request or finish dismissal.
/// Native cancel and backdrop requests pass through the current edit guard;
/// an opener that lapses must close immediately. The single `close` event
/// returns focus and clears the selection. Dropping the presentation detaches
/// every listener first, so teardown closes silently.
struct Presentation {
    dialog: web_sys::HtmlDialogElement,
    closed: Closure<dyn FnMut(web_sys::Event)>,
    cancelled: Closure<dyn FnMut(web_sys::Event)>,
    backdrop: Closure<dyn FnMut(web_sys::Event)>,
    _ownership: Ownership,
}

/// Closes the dialog as soon as its opener can no longer take focus back.
/// The opener's own box reports detachment and every kind of hiding; `inert`
/// changes nothing about layout, so ancestors' `inert` is watched separately.
struct Ownership {
    resized: web_sys::ResizeObserver,
    inerted: web_sys::MutationObserver,
    _lapsed: Closure<dyn FnMut()>,
}

impl Ownership {
    fn watch(
        dialog: web_sys::HtmlDialogElement,
        opener: web_sys::Element,
        departed: Callback<()>,
    ) -> Self {
        let document = opener.owner_document().expect("preview document");
        let observed = opener.clone();
        let lapsed = Closure::<dyn FnMut()>::new(move || {
            if !overlay::returnable(&observed) {
                departed.emit(());
                dialog.close();
            }
        });
        let resized = web_sys::ResizeObserver::new(lapsed.as_ref().unchecked_ref())
            .expect("viewer opener box observer");
        resized.observe(&opener);
        let inerted = web_sys::MutationObserver::new(lapsed.as_ref().unchecked_ref())
            .expect("viewer opener inertness observer");
        let options = web_sys::MutationObserverInit::new();
        options.set_subtree(true);
        options.set_attribute_filter(&js_sys::Array::of1(&JsValue::from_str("inert")));
        inerted
            .observe_with_options(&document, &options)
            .expect("observe opener inertness");
        Self {
            resized,
            inerted,
            _lapsed: lapsed,
        }
    }
}

impl Drop for Ownership {
    fn drop(&mut self) {
        self.resized.disconnect();
        self.inerted.disconnect();
    }
}

impl Presentation {
    fn show(
        dialog: web_sys::HtmlDialogElement,
        opener: web_sys::Element,
        dismissed: Callback<()>,
        requested: Callback<()>,
        departed: Callback<()>,
    ) -> Self {
        let returned = opener.clone();
        let closed = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            overlay::return_focus(&returned);
            dismissed.emit(());
        });
        let cancel_request = requested.clone();
        let cancelled = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
            event.prevent_default();
            cancel_request.emit(());
        });
        let backdrop = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
            if event
                .target()
                .and_then(|target| target.dyn_into::<web_sys::HtmlDialogElement>().ok())
                .is_some()
            {
                requested.emit(());
            }
        });
        dialog
            .add_event_listener_with_callback("close", closed.as_ref().unchecked_ref())
            .expect("dialog close listener");
        dialog
            .add_event_listener_with_callback("cancel", cancelled.as_ref().unchecked_ref())
            .expect("dialog cancel listener");
        dialog
            .add_event_listener_with_callback("click", backdrop.as_ref().unchecked_ref())
            .expect("dialog backdrop listener");
        let ownership = Ownership::watch(dialog.clone(), opener.clone(), departed);
        // Focus returns only to an eligible opener, never to whatever the
        // browser remembers as focused before the modal opened.
        if let Some(active) = opener
            .owner_document()
            .and_then(|document| document.active_element())
            .and_then(|active| active.dyn_into::<web_sys::HtmlElement>().ok())
        {
            let _ = active.blur();
        }
        dialog
            .show_modal()
            .expect("the connected viewer opens in the top layer");
        Self {
            dialog,
            closed,
            cancelled,
            backdrop,
            _ownership: ownership,
        }
    }
}

impl Drop for Presentation {
    fn drop(&mut self) {
        let _ = self
            .dialog
            .remove_event_listener_with_callback("close", self.closed.as_ref().unchecked_ref());
        let _ = self
            .dialog
            .remove_event_listener_with_callback("cancel", self.cancelled.as_ref().unchecked_ref());
        let _ = self
            .dialog
            .remove_event_listener_with_callback("click", self.backdrop.as_ref().unchecked_ref());
        self.dialog.close();
    }
}
