//! Canonical reader destinations and owned browser history over mounted Chat.

mod history;
pub(in crate::browser) mod presentation;
mod selection;

pub(super) use selection::use_selection;

use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::Closure;
use yew::prelude::*;

use crate::view::{
    AppRoute, ImageTarget, NavigationCause, NormalizedAppLocation, TaskView, WorkLocation,
    WorkOptions, WorkPlace, WorkSelection, history_effect, normalize_app_location,
};
use crate::work::{TaskOverviewContext, TaskOverviewSelection, WorkScope};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Intent {
    Card(String),
    Work(WorkScope),
    RefineScope(WorkScope),
    Task(wire::TaskStamp),
    TaskView(wire::TaskStamp, Option<TaskView>),
    Overview(TaskOverviewSelection),
    Options(WorkOptions),
    Spice,
    ClosePanel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ImageRequest {
    pub(super) route: AppRoute,
    pub(super) target: ImageTarget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RouteRefusal {
    pub(super) route: AppRoute,
    pub(super) text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RouteNotice {
    id: u64,
    pub(super) text: String,
}

pub(super) struct Controls {
    pub(super) route: AppRoute,
    pub(super) image: Option<ImageTarget>,
    pub(super) notice: Option<RouteNotice>,
    pub(super) refuse: Callback<RouteRefusal>,
    pub(super) replace: Callback<AppRoute>,
    pub(super) navigate: Callback<Intent>,
    pub(super) open_image: Callback<ImageRequest>,
    pub(super) replace_image: Callback<ImageRequest>,
    pub(super) close_image: Callback<ImageTarget>,
    pub(super) refuse_image: Callback<String>,
}

fn work(place: WorkPlace, options: WorkOptions) -> AppRoute {
    AppRoute::Work(WorkLocation { place, options })
}

fn destination(current: &AppRoute, intent: Intent) -> Option<(AppRoute, NavigationCause)> {
    let mut cause = NavigationCause::Destination;
    let mut route = match intent {
        Intent::Card(target) => AppRoute::card(target)?,
        Intent::Work(scope) => AppRoute::work(scope.route()),
        Intent::RefineScope(scope) => {
            cause = NavigationCause::Adjustment;
            work(
                WorkPlace::Scope(scope.route()),
                current.options().cloned().unwrap_or_default(),
            )
        }
        Intent::Task(task) => work(WorkPlace::Task { task, view: None }, WorkOptions::default()),
        Intent::TaskView(task, view) => work(
            WorkPlace::Task {
                task,
                view: view.filter(|view| *view != TaskView::Room),
            },
            current.options().cloned().unwrap_or_default(),
        ),
        Intent::Overview(selection) => {
            let related = match selection.context() {
                TaskOverviewContext::Relationships(task) => Some(task.clone()),
                _ => None,
            };
            let selected = WorkSelection::new(selection.tasks().to_vec(), related)?;
            work(
                WorkPlace::Selection(selected),
                current.options().cloned().unwrap_or_default(),
            )
        }
        Intent::Options(options) => {
            cause = NavigationCause::Adjustment;
            current.clone().with_options(options)
        }
        Intent::Spice => AppRoute::Spice,
        Intent::ClosePanel => return None,
    };
    if matches!(cause, NavigationCause::Destination)
        && let (AppRoute::Work(current), AppRoute::Work(next)) = (current, &mut route)
        && current.place == next.place
    {
        next.options = current.options.clone();
    }
    Some((route, cause))
}

struct ImageControls {
    open: Callback<ImageRequest>,
    replace: Callback<ImageRequest>,
    close: Callback<ImageTarget>,
    refuse: Callback<String>,
}

fn image_controls(
    history: Rc<history::History>,
    arrive: Rc<dyn Fn()>,
    announce: Callback<String>,
) -> ImageControls {
    let open = {
        let history = history.clone();
        let arrive = arrive.clone();
        Callback::from(move |request: ImageRequest| {
            let current = location_route();
            history.open_image(
                current.route(),
                &request.route,
                &request.target,
                &presentation::departing(),
            );
            arrive();
        })
    };
    let replace = {
        let history = history.clone();
        let arrive = arrive.clone();
        Callback::from(move |request: ImageRequest| {
            history.replace_image(&request.route, &request.target);
            arrive();
        })
    };
    let close = {
        let history = history.clone();
        let arrive = arrive.clone();
        Callback::from(move |target: ImageTarget| {
            let current = location_route();
            if current.image() == Some(&target) && !history.close_image(current.route(), &target) {
                arrive();
            }
        })
    };
    let refuse = Callback::from(move |problem: String| {
        let current = location_route();
        history.replace(current.route());
        announce.emit(problem);
        arrive();
    });
    ImageControls {
        open,
        replace,
        close,
        refuse,
    }
}

#[hook]
fn use_navigation_listeners(
    arrive: Rc<dyn Fn()>,
    change: Callback<(AppRoute, NavigationCause)>,
    announce: Callback<String>,
    last_arrival: Rc<RefCell<String>>,
) {
    use_effect_with((), move |()| {
        presentation::install();
        arrive();
        let window = web_sys::window().expect("app navigation window");
        let last = last_arrival;
        let rehydrate = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            let identity = arrival_identity();
            if *last.borrow() != identity {
                arrive();
            }
        });
        let follow = Closure::<dyn FnMut(web_sys::MouseEvent)>::new(move |event| {
            let Some(next) = followed_location(&event) else {
                return;
            };
            event.prevent_default();
            if let Some(text) = next.notice() {
                announce.emit(text.to_owned());
            }
            change.emit((next.route().clone(), NavigationCause::Destination));
        });
        for event in ["popstate", "hashchange"] {
            window
                .add_event_listener_with_callback(event, rehydrate.as_ref().unchecked_ref())
                .expect("navigation history listener");
        }
        window
            .add_event_listener_with_callback("click", follow.as_ref().unchecked_ref())
            .expect("navigation link listener");
        move || {
            for event in ["popstate", "hashchange"] {
                let _ = window
                    .remove_event_listener_with_callback(event, rehydrate.as_ref().unchecked_ref());
            }
            let _ = window
                .remove_event_listener_with_callback("click", follow.as_ref().unchecked_ref());
        }
    });
}

#[hook]
pub(super) fn use_route() -> Controls {
    let location = use_state(location_route);
    let history = use_memo((), |()| history::History::new());
    let notice = use_state(|| None::<RouteNotice>);
    let announced = use_mut_ref(|| 0_u64);
    let last_arrival = use_mut_ref(String::new);
    let announce = {
        let notice = notice.clone();
        Callback::from(move |text: String| {
            let mut id = announced.borrow_mut();
            *id += 1;
            notice.set(Some(RouteNotice { id: *id, text }));
        })
    };
    let arrive: Rc<dyn Fn()> = {
        let location = location.clone();
        let history = history.clone();
        let announce = announce.clone();
        let last_arrival = last_arrival.clone();
        Rc::new(move || {
            let next = location_route();
            if next.needs_replace() || history.entry().is_none() {
                history.replace_location(next.route(), next.image());
            }
            if let Some(text) = next.notice() {
                announce.emit(text.to_owned());
            }
            presentation::arrive(next.route());
            *last_arrival.borrow_mut() = arrival_identity();
            location.set(next);
        })
    };
    let change = {
        let history = history.clone();
        let arrive = arrive.clone();
        Callback::from(move |(next, cause): (AppRoute, NavigationCause)| {
            let current = location_route();
            let effect = history_effect(current.route(), &next, cause);
            let departing = presentation::departing();
            history.navigate(current.route(), &next, effect, &departing);
            arrive();
        })
    };
    let image = image_controls(history.clone(), arrive.clone(), announce.clone());
    use_navigation_listeners(
        arrive.clone(),
        change.clone(),
        announce.clone(),
        last_arrival,
    );
    let replace = change.reform(|route| (route, NavigationCause::Normalization));
    let refuse = {
        let replace = replace.clone();
        Callback::from(move |refusal: RouteRefusal| {
            replace.emit(refusal.route);
            announce.emit(refusal.text);
        })
    };
    let navigate = Callback::from(move |intent: Intent| {
        let current = location_route();
        if intent == Intent::ClosePanel {
            if !history.close(current.route()) {
                arrive();
            }
        } else if let Some(target) = destination(current.route(), intent) {
            change.emit(target);
        }
    });
    Controls {
        route: location.route().clone(),
        image: location.image().cloned(),
        notice: (*notice).clone(),
        refuse,
        replace,
        navigate,
        open_image: image.open,
        replace_image: image.replace,
        close_image: image.close,
        refuse_image: image.refuse,
    }
}

fn followed_location(event: &web_sys::MouseEvent) -> Option<NormalizedAppLocation> {
    if !super::card_target::follows_in_tab(event) {
        return None;
    }
    let link = event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        .and_then(|target| target.closest("a[href]").ok().flatten())?;
    if link.has_attribute("download")
        || link
            .get_attribute("target")
            .is_some_and(|target| !target.is_empty() && target != "_self")
    {
        return None;
    }
    let href = link.get_attribute("href")?;
    href.starts_with("#/")
        .then(|| normalize_app_location("", &href))
}

fn arrival_identity() -> String {
    let window = web_sys::window().expect("navigation window");
    let state = window
        .history()
        .and_then(|history| history.state())
        .ok()
        .and_then(|state| js_sys::JSON::stringify(&state).ok())
        .and_then(|state| state.as_string())
        .unwrap_or_default();
    format!("{}\n{state}", window.location().href().unwrap_or_default())
}

fn location_route() -> NormalizedAppLocation {
    let window = web_sys::window().expect("app navigation requires a browser window");
    let location = window.location();
    normalize_app_location(
        &location.search().expect("navigation query"),
        &location.hash().expect("navigation fragment"),
    )
}

/// Consume authentication or replace a location before routing starts.
pub(super) fn replace_location(route: &AppRoute) {
    history::History::new().replace(route);
}
