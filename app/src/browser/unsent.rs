//! The one leave guard and the page's account of the unsent work it protects.
//!
//! A browser's leave dialog carries no page text, so a leave the guard holds
//! leaves the page listing what held it. Each item names its kind and owner,
//! reveals the work where it lives, and offers to discard it, so work that no
//! surface shows, such as a draft for a lane that left its console, can still
//! be found and settled.

use std::cell::RefCell;

use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use wire::{CanonicalUuid, TaskStamp};
use yew::prelude::*;

use super::watchband::{ComposerDrafts, clear_composer, focus_after_render, primary_textarea_id};
use super::{NoteComposers, UnloadGuard, draft_handoff};
use crate::AppRuntime;
use crate::unsent_work::UnsentWork;

thread_local! {
    /// The draft image whose annotation marks are not yet applied.
    static ANNOTATING: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The image whose unapplied annotation marks leaving would discard.
pub(super) fn annotating() -> Option<String> {
    ANNOTATING.with_borrow(Clone::clone)
}

/// Report `image`'s annotation marks as unapplied while `dirty` holds.
#[hook]
pub(super) fn use_annotation_guard(dirty: bool, image: AttrValue) {
    use_effect_with((dirty, image), |(dirty, image)| {
        let dirty = *dirty;
        if dirty {
            ANNOTATING.set(Some(image.to_string()));
        }
        move || {
            if dirty {
                ANNOTATING.set(None);
            }
        }
    });
}

/// Hold any leave that would discard unsent work, and keep what held it for
/// the page to list once the operator stays.
#[hook]
pub(super) fn use_leave_guard(guard: &UnloadGuard, held: &UseStateHandle<Option<Vec<UnsentWork>>>) {
    let guard = guard.clone();
    let held = held.clone();
    use_effect_with((), move |()| {
        let registration = web_sys::window().and_then(|window| {
            let callback =
                Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
                    if draft_handoff::leaving() {
                        return;
                    }
                    let unsent = guard.inventory();
                    if unsent.is_empty() {
                        return;
                    }
                    event.prevent_default();
                    let _ = js_sys::Reflect::set(
                        event.as_ref(),
                        &JsValue::from_str("returnValue"),
                        &JsValue::from_str(""),
                    );
                    held.set(Some(unsent));
                });
            window
                .add_event_listener_with_callback("beforeunload", callback.as_ref().unchecked_ref())
                .ok()?;
            Some((window, callback))
        });
        move || {
            if let Some((window, callback)) = registration {
                let _ = window.remove_event_listener_with_callback(
                    "beforeunload",
                    callback.as_ref().unchecked_ref(),
                );
            }
        }
    });
}

pub(super) struct UnsentRender<'a> {
    pub held: &'a UseStateHandle<Option<Vec<UnsentWork>>>,
    pub runtime: Option<&'a AppRuntime>,
    pub drafts: &'a UseReducerHandle<ComposerDrafts>,
    pub notes: &'a NoteComposers,
    pub select_task: &'a Callback<TaskStamp>,
}

/// The work a held leave protected, listed until the operator dismisses it.
pub(super) fn render(render: &UnsentRender<'_>) -> Html {
    let Some(items) = render.held.as_ref().filter(|items| !items.is_empty()) else {
        return Html::default();
    };
    let dismiss = {
        let held = render.held.clone();
        Callback::from(move |_: MouseEvent| held.set(None))
    };
    html! {
        <section
            class="unsent-work app-overlay-status"
            aria-labelledby="unsent-work-title"
            aria-live="polite"
        >
            <header class="unsent-work-header">
                <h2 id="unsent-work-title">{"Unsent work kept this page open"}</h2>
                <button type="button" class="app-compact-control" onclick={dismiss}>
                    {"Dismiss"}
                </button>
            </header>
            <ul class="unsent-work-items">
                {for items.iter().map(|item| render_item(render, items, item))}
            </ul>
        </section>
    }
}

/// What one item says and which controls it offers.
struct Described {
    kind: &'static str,
    label: String,
    excerpt: String,
    reveal: Option<Callback<MouseEvent>>,
    discard: Option<Callback<MouseEvent>>,
}

fn render_item(render: &UnsentRender<'_>, items: &[UnsentWork], item: &UnsentWork) -> Html {
    let described = describe(render, items, item);
    html! {
        <li class="unsent-work-item" data-unsent-kind={described.kind}>
            <span class="unsent-work-label">{described.label}</span>
            if !described.excerpt.is_empty() {
                <q class="unsent-work-excerpt">{described.excerpt}</q>
            }
            if described.reveal.is_some() || described.discard.is_some() {
                <span class="unsent-work-actions">
                    if let Some(reveal) = described.reveal {
                        <button type="button" class="app-compact-control" onclick={reveal}>
                            {"Show"}
                        </button>
                    }
                    if let Some(discard) = described.discard {
                        <button type="button" class="app-compact-control" onclick={discard}>
                            {"Discard"}
                        </button>
                    }
                </span>
            }
        </li>
    }
}

fn describe(render: &UnsentRender<'_>, items: &[UnsentWork], item: &UnsentWork) -> Described {
    match item {
        UnsentWork::Composer {
            target,
            excerpt,
            quotes,
            images,
        } => {
            let owner = composer_owner(render.runtime, target);
            let element_id = primary_textarea_id(target);
            let shown = element(&element_id).is_some();
            let mut label = owner.clone();
            for (count, one, many) in [
                (*quotes, "quoted reply", "quoted replies"),
                (*images, "image", "images"),
            ] {
                if count > 0 {
                    let noun = if count == 1 { one } else { many };
                    label.push_str(&format!(" · {count} {noun}"));
                }
            }
            if !shown {
                label.push_str(" · not shown on this page");
            }
            let discard = {
                let target = target.clone();
                let drafts = render.drafts.clone();
                settle(render.held, items, item, move || {
                    confirmed(&format!(
                        "Discard the unsent {}? Its text, quoted replies, and images cannot be recovered.",
                        lowercase_first(&owner)
                    ))
                    .then(|| clear_composer(&target, &drafts))
                    .is_some()
                })
            };
            Described {
                kind: "composer",
                label,
                excerpt: excerpt.clone(),
                reveal: shown.then(|| Callback::from(move |_: MouseEvent| reveal(&element_id))),
                discard: Some(discard),
            }
        }
        UnsentWork::Note { task, excerpt } => {
            let handle = task_handle(render.runtime, task);
            let element_id = format!("work-detail-note-{task}");
            let open = {
                let task = task.clone();
                let notes = render.notes.clone();
                let select_task = render.select_task.clone();
                Callback::from(move |_: MouseEvent| {
                    if element(&element_id).is_some() {
                        reveal(&element_id);
                    } else {
                        notes.update(&task, |composer| composer.open = true);
                        select_task.emit(task.clone());
                        focus_after_render(element_id.clone());
                    }
                })
            };
            let discard = {
                let task = task.clone();
                let notes = render.notes.clone();
                let handle = handle.clone();
                settle(render.held, items, item, move || {
                    confirmed(&format!(
                        "Discard the unsent observation on {handle}? Its text cannot be recovered."
                    ))
                    .then(|| notes.update(&task, |composer| composer.draft.clear()))
                    .is_some()
                })
            };
            Described {
                kind: "note",
                label: format!("Observation on {handle}"),
                excerpt: excerpt.clone(),
                reveal: Some(open),
                discard: Some(discard),
            }
        }
        UnsentWork::Awaiting { count } => Described {
            kind: "awaiting",
            label: if *count == 1 {
                "1 message the resident has not yet accepted".to_owned()
            } else {
                format!("{count} messages the resident has not yet accepted")
            },
            excerpt: String::new(),
            reveal: None,
            discard: None,
        },
        UnsentWork::Annotation { image } => Described {
            kind: "annotation",
            label: format!(
                "Annotation marks on {image}, not yet applied, in the open image viewer"
            ),
            excerpt: String::new(),
            reveal: None,
            discard: None,
        },
    }
}

/// Who a composer draft is addressed to: a lane and its team, or a design.
fn composer_owner(runtime: Option<&AppRuntime>, target: &str) -> String {
    if target == crate::composer_drafts::NEW_DRAFT_KEY {
        return "Message or task in the main composer".to_owned();
    }
    if let Ok(lane) = target.parse::<CanonicalUuid>() {
        let member = runtime
            .and_then(AppRuntime::lanes)
            .into_iter()
            .flat_map(|lanes| &lanes.lanes)
            .find_map(|team| {
                team.members
                    .iter()
                    .find(|member| member.lane == lane)
                    .map(|member| (team, member))
            });
        return match member {
            Some((team, member)) => {
                let agent =
                    crate::watchband::agent_message_label(&member.worktree, member.name.as_deref());
                match team.console {
                    Some(_) => format!(
                        "Message for {agent} in {}'s team",
                        crate::watchband::team_label(team)
                    ),
                    None => format!("Message for {agent}, a lane in no team console"),
                }
            }
            None => "Message for a lane this page no longer lists".to_owned(),
        };
    }
    "Draft in a composer this page no longer shows".to_owned()
}

fn task_handle(runtime: Option<&AppRuntime>, task: &TaskStamp) -> String {
    runtime
        .and_then(AppRuntime::board)
        .and_then(|board| board.row(task))
        .map_or_else(
            || task.to_string(),
            |row| row.statement.body.handle.to_string(),
        )
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}

/// A control that settles `item` when `act` reports it discarded, dropping it
/// from the held list and closing the list once nothing remains.
fn settle(
    held: &UseStateHandle<Option<Vec<UnsentWork>>>,
    items: &[UnsentWork],
    item: &UnsentWork,
    act: impl Fn() -> bool + 'static,
) -> Callback<MouseEvent> {
    let held = held.clone();
    let remaining = items
        .iter()
        .filter(|candidate| *candidate != item)
        .cloned()
        .collect::<Vec<_>>();
    Callback::from(move |_: MouseEvent| {
        if act() {
            held.set((!remaining.is_empty()).then(|| remaining.clone()));
        }
    })
}

fn confirmed(message: &str) -> bool {
    web_sys::window()
        .and_then(|window| window.confirm_with_message(message).ok())
        .unwrap_or(false)
}

fn element(id: &str) -> Option<web_sys::HtmlElement> {
    web_sys::window()?
        .document()?
        .get_element_by_id(id)?
        .dyn_into::<web_sys::HtmlElement>()
        .ok()
}

fn reveal(id: &str) {
    if id == primary_textarea_id(crate::composer_drafts::NEW_DRAFT_KEY) {
        if let Some(trigger) = element("app-compose-trigger") {
            trigger.click();
        }
        focus_after_render(id.to_owned());
        return;
    }
    if let Some(element) = element(id) {
        element.scroll_into_view();
        let _ = element.focus();
    }
}
