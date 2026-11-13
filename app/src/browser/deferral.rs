//! The operator's direct control over when filed work returns, in place on the
//! task detail's Deferral fact: Clear wakes a deferred task now, and the
//! calendar sets the instant its deferral wakes at. Both go through the same
//! `task::wake` and `task::defer` commands as the CLI, under the heads this
//! view was drawn from.

use std::rc::Rc;

use wasm_bindgen::{JsCast, JsValue};
use wire::intent::{BoardTask, TaskAnswer, TaskDefer, TaskWake};
use wire::{CommandKind, Instant, Witness};
use yew::prelude::*;

use super::sender::{SenderHandle, admit_action};
use crate::AppRuntime;
use crate::client_action::{ClientAction, DeferralRequest};

/// The Deferral fact of one task's detail, showing `shown`. A pending filed
/// task's fact also carries Clear and the calendar on the same line; a Draft,
/// which only leaving draft admits, and settled work show the bare fact.
pub(super) fn fact(
    runtime: Option<&AppRuntime>,
    sender: &SenderHandle,
    row: &BoardTask,
    shown: Html,
) -> Html {
    let workflow = &row.workflow.body;
    if !workflow.status.is_pending() || workflow.is_draft() {
        return html! { <div><dt>{"Deferral"}</dt><dd>{shown}</dd></div> };
    }
    html! {
        <DeferralFact
            key={row.statement.body.born.to_string()}
            row={row.clone()}
            shown={shown}
            wakes={runtime.is_some_and(|runtime| runtime.offers(CommandKind::TaskWake))}
            defers={runtime.is_some_and(|runtime| runtime.offers(CommandKind::TaskDefer))}
            sender={sender.clone()}
        />
    }
}

#[derive(Properties)]
struct DeferralProps {
    row: BoardTask,
    shown: Html,
    wakes: bool,
    defers: bool,
    sender: SenderHandle,
}

impl PartialEq for DeferralProps {
    fn eq(&self, other: &Self) -> bool {
        self.row == other.row
            && self.shown == other.shown
            && self.wakes == other.wakes
            && self.defers == other.defers
            && Rc::ptr_eq(&self.sender.queue, &other.sender.queue)
    }
}

/// One task's Deferral fact: the scheduled time, Clear while deferred, and the
/// calendar whose chosen time defers the task. The outcome of the last change
/// stays on the fact's line, in full on hover.
#[function_component]
fn DeferralFact(props: &DeferralProps) -> Html {
    let deferred = props.row.workflow.body.deferral;
    let picker = use_node_ref();
    let notice = use_state(|| None::<String>);
    let send = {
        let (row, sender, notice) = (props.row.clone(), props.sender.clone(), notice.clone());
        Rc::new(move |request: DeferralRequest, landed: &'static str| {
            send_change(&row, &sender, &notice, request, landed);
        })
    };
    let handle = props.row.statement.body.handle.clone();
    let clear = {
        let (send, handle) = (send.clone(), handle.clone());
        Callback::from(move |_: MouseEvent| {
            send(
                DeferralRequest::Wake(TaskWake {
                    task: handle.clone(),
                    claim: false,
                }),
                "Cleared.",
            );
        })
    };
    let open = {
        let (picker, notice) = (picker.clone(), notice.clone());
        Callback::from(move |_: MouseEvent| {
            let Some(input) = picker.cast::<web_sys::HtmlInputElement>() else {
                return;
            };
            // The horizon is a hold, not a date to edit.
            let scheduled = deferred.filter(|instant| !instant.is_indefinite());
            input.set_value(&scheduled.map(local_input).unwrap_or_default());
            if let Err(problem) = show_picker(&input) {
                notice.set(Some(problem));
            }
        })
    };
    let chosen = {
        let (send, notice) = (send, notice.clone());
        Callback::from(move |event: Event| {
            let Some(input) = event
                .target()
                .and_then(|target| target.dyn_into::<web_sys::HtmlInputElement>().ok())
            else {
                return;
            };
            match instant_of(&input.value()) {
                Ok(wait) => send(
                    DeferralRequest::Defer(TaskDefer {
                        task: handle.clone(),
                        wait,
                    }),
                    "Deferred.",
                ),
                Err(problem) => notice.set(Some(problem)),
            }
        })
    };
    let label = if deferred.is_some() {
        "Change deferral"
    } else {
        "Defer"
    };
    html! {
        <div>
            <dt>{"Deferral"}</dt>
            <dd class="task-deferral" data-task-deferral={props.row.statement.body.handle.to_string()}>
                <span class="task-deferral-time">{props.shown.clone()}</span>
                if deferred.is_some() {
                    <button
                        type="button"
                        class="app-compact-control"
                        onclick={clear}
                        disabled={!props.wakes}
                    >{"Clear"}</button>
                }
                <span class="task-deferral-calendar">
                    <button
                        type="button"
                        class="app-compact-control"
                        onclick={open}
                        disabled={!props.defers}
                    >
                        <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
                            <rect x="3.5" y="5" width="17" height="15.5" rx="2"></rect>
                            <path d="M3.5 10h17M8 3v4M16 3v4"></path>
                        </svg>
                        {label}
                    </button>
                    <input
                        ref={picker}
                        type="datetime-local"
                        tabindex="-1"
                        aria-hidden="true"
                        onchange={chosen}
                    />
                </span>
                if let Some(notice) = (*notice).clone() {
                    <span class="task-deferral-notice" role="status" title={notice.clone()}>{notice}</span>
                }
            </dd>
        </div>
    }
}

/// Queue one deferral command under `row`'s heads; `notice` carries `landed`
/// once it lands, or the refusal.
fn send_change(
    row: &BoardTask,
    sender: &SenderHandle,
    notice: &UseStateHandle<Option<String>>,
    request: DeferralRequest,
    landed: &'static str,
) {
    let respond = {
        let notice = notice.clone();
        Rc::new(move |answer: Result<TaskAnswer, String>| {
            notice.set(Some(
                answer.map_or_else(|refusal| refusal, |_| landed.to_owned()),
            ));
        })
    };
    let action = ClientAction::Deferral {
        request,
        expected: witnesses(row),
        respond,
    };
    notice.set(None);
    if !admit_action(sender, action) {
        notice.set(Some("The deferral change could not be queued.".to_owned()));
    }
}

/// Open the browser's own date-time picker for `input`.
fn show_picker(input: &web_sys::HtmlInputElement) -> Result<(), String> {
    const UNAVAILABLE: &str = "This browser cannot open a date picker.";
    let open = js_sys::Reflect::get(input, &JsValue::from_str("showPicker"))
        .ok()
        .and_then(|open| open.dyn_into::<js_sys::Function>().ok())
        .ok_or_else(|| UNAVAILABLE.to_owned())?;
    open.call0(input)
        .map(|_| ())
        .map_err(|_| UNAVAILABLE.to_owned())
}

/// The exact workflow and deferral heads both commands require.
fn witnesses(row: &BoardTask) -> Vec<Witness> {
    let task = row.statement.body.born.clone();
    vec![
        Witness::Workflow {
            task: task.clone(),
            head: row.workflow.position,
            phase: row.workflow.body.phase,
        },
        Witness::Deferral {
            task,
            head: row.workflow.position,
            deferred: row.workflow.body.deferral.is_some(),
        },
    ]
}

/// The browser's local wall-clock spelling of `instant` for a
/// `datetime-local` input, to the minute.
fn local_input(instant: Instant) -> String {
    let date = js_sys::Date::new(&JsValue::from_f64(instant.get().as_millisecond() as f64));
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}",
        date.get_full_year(),
        date.get_month() + 1,
        date.get_date(),
        date.get_hours(),
        date.get_minutes(),
    )
}

/// The instant a `datetime-local` value names in the browser's time zone.
fn instant_of(local: &str) -> Result<Instant, String> {
    let date = js_sys::Date::new(&JsValue::from_str(local));
    if local.is_empty() || date.get_time().is_nan() {
        return Err("Choose a valid time.".to_owned());
    }
    String::from(date.to_iso_string())
        .parse()
        .map_err(|error: wire::IdentError| error.to_string())
}
