//! Transient result toasts: frosted status pills that show the latest owned
//! result, hold a pending one until its outcome replaces it, and fade a
//! settled one after a fixed dwell.
//!
//! The app mounts one shared lower feedback area with independently owned
//! general and task-lifecycle slots. Manual popovers show above an open modal
//! dialog; the lightbox instead renders its instance inside its dialog beside
//! the image its actions report on.

use std::rc::Rc;

use crate::toast_state::{ToastAction, ToastArea, ToastOwner};
use gloo_timers::callback::Timeout;
use yew::prelude::*;

/// How long a settled result stays up before the toast fades.
const DISMISS_MILLIS: u32 = 4_000;

/// Replaces the result a toast shows.
#[derive(Clone, PartialEq)]
pub(super) struct Toaster(Callback<(AttrValue, bool)>);

impl Toaster {
    /// Show a result that stays until another replaces it.
    pub(super) fn pending(&self, message: impl Into<AttrValue>) {
        self.0.emit((message.into(), false));
    }

    /// Show a final result, which fades on its own.
    pub(super) fn settled(&self, message: impl Into<AttrValue>) {
        self.0.emit((message.into(), true));
    }

    /// Remove the current result without announcing a replacement.
    pub(super) fn clear(&self) {
        self.0.emit((AttrValue::default(), false));
    }
}

#[derive(Clone, PartialEq)]
struct TaskToaster(Toaster);
impl Reducible for ToastArea<AttrValue> {
    type Action = ToastAction<AttrValue>;

    fn reduce(self: Rc<Self>, action: Self::Action) -> Rc<Self> {
        let mut next = (*self).clone();
        next.apply(action);
        Rc::new(next)
    }
}

/// The latest result, whether it is still showing, and the handle that
/// replaces it. A settled result arms its own dismissal; replacing it cancels
/// any earlier result's.
#[hook]
pub(super) fn use_toast() -> (UseStateHandle<AttrValue>, UseStateHandle<bool>, Toaster) {
    let text = use_state_eq(AttrValue::default);
    let shown = use_state_eq(|| false);
    let dismissal = use_mut_ref(|| None::<Timeout>);
    let notify = use_callback(
        (text.clone(), shown.clone()),
        move |(message, settled): (AttrValue, bool), (text, shown)| {
            text.set(message);
            shown.set(true);
            let shown = shown.clone();
            *dismissal.borrow_mut() =
                settled.then(|| Timeout::new(DISMISS_MILLIS, move || shown.set(false)));
        },
    );
    (text, shown, Toaster(notify))
}

/// The root toast's handle.
#[hook]
pub(super) fn use_toaster() -> Toaster {
    use_context::<Toaster>().expect("the app root mounts the toast")
}

/// The task lifecycle slot's independently owned handle.
#[hook]
pub(super) fn use_task_toaster() -> Toaster {
    use_context::<TaskToaster>()
        .expect("the app root mounts the task toast")
        .0
}

#[derive(Properties, PartialEq)]
pub(super) struct NoticeProps {
    /// The problem in force, or none.
    pub text: Option<AttrValue>,
}

/// Toast a real problem on the root toast when it begins. It renders nothing,
/// so a problem never adds an in-flow status line beside what it qualifies.
#[function_component(Notice)]
pub(super) fn notice(props: &NoticeProps) -> Html {
    let toaster = use_toaster();
    use_effect_with(props.text.clone(), move |text| {
        if let Some(text) = text {
            toaster.settled(text.clone());
        }
    });
    Html::default()
}

/// The toast pill. Hidden text stays in place so the pill fades out rather
/// than collapsing first. A `raised` toast is a manual popover in the top
/// layer; the one inside a dialog is already there.
pub(super) fn render_toast(message: AttrValue, shown: bool, raised: Option<NodeRef>) -> Html {
    let popover = raised.is_some().then_some("manual");
    html! {
        <p
            ref={raised.unwrap_or_default()} class="toast app-overlay-status" role="status" {popover}
            data-shown={shown.to_string()}
        >{message}</p>
    }
}

#[derive(Properties, PartialEq)]
pub(super) struct ToastProviderProps {
    pub children: Html,
}

/// Build one owner-scoped handle. Each owner has its own dismissal timer, and
/// every action names the one slot it may replace, hide, or clear.
#[hook]
fn use_owned_toaster(owner: ToastOwner, area: UseReducerHandle<ToastArea<AttrValue>>) -> Toaster {
    let dismissal = use_mut_ref(|| None::<Timeout>);
    Toaster(use_callback(
        area,
        move |(message, settled): (AttrValue, bool), area| {
            let empty = message.is_empty();
            area.dispatch(if empty {
                ToastAction::Clear { owner }
            } else {
                ToastAction::Show {
                    owner,
                    text: message,
                }
            });
            let area = area.clone();
            *dismissal.borrow_mut() = (settled && !empty).then(|| {
                Timeout::new(DISMISS_MILLIS, move || {
                    area.dispatch(ToastAction::Hide { owner });
                })
            });
        },
    ))
}

/// Raise the one common area whenever either owner reports.
#[hook]
fn use_raised_toaster(toaster: Toaster, node: NodeRef) -> Toaster {
    let raising = use_memo((toaster, node), |(toaster, node)| {
        let (inner, node) = (toaster.0.clone(), node.clone());
        Toaster(Callback::from(move |result| {
            if let Some(element) = node.cast::<web_sys::Element>() {
                raise(&element);
            }
            inner.emit(result);
        }))
    });
    (*raising).clone()
}

/// Mount the independently owned general-result and task-lifecycle slots in
/// one lower feedback area.
#[function_component(ToastProvider)]
pub(super) fn toast_provider(props: &ToastProviderProps) -> Html {
    let area = use_reducer(ToastArea::<AttrValue>::default);
    let node = use_node_ref();
    let toaster = use_owned_toaster(ToastOwner::General, area.clone());
    let toaster = use_raised_toaster(toaster, node.clone());
    let task_toaster = use_owned_toaster(ToastOwner::Task, area.clone());
    let task_toaster = use_raised_toaster(task_toaster, node.clone());
    let general = area.slot(ToastOwner::General).clone();
    let task = area.slot(ToastOwner::Task).clone();
    html! {
        <ContextProvider<Toaster> context={toaster}>
            <ContextProvider<TaskToaster> context={TaskToaster(task_toaster)}>
                {props.children.clone()}
                <div ref={node} class="toast-area" popover="manual">
                    {render_toast(
                        general.text.clone(),
                        general.shown && !general.text.is_empty(),
                        None,
                    )}
                    {render_toast(
                        task.text.clone(),
                        task.shown && !task.text.is_empty(),
                        None,
                    )}
                </div>
            </ContextProvider<TaskToaster>>
        </ContextProvider<Toaster>>
    }
}

/// Move a manual popover to the top of the top layer. The Popover API is
/// reached reflectively, as the clipboard is, so the build needs no unstable
/// `web-sys` flag.
fn raise(element: &web_sys::Element) {
    let call = |method: &str| {
        js_sys::Reflect::get(element, &wasm_bindgen::JsValue::from_str(method))
            .ok()
            .and_then(|function| wasm_bindgen::JsCast::dyn_into::<js_sys::Function>(function).ok())
            .map(|function| function.call0(element))
    };
    if element.matches(":popover-open").unwrap_or(false) {
        call("hidePopover");
    }
    call("showPopover");
}
