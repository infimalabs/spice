use super::*;
use wire::intent::{LaneJobAnswer, LaneJobPhase};

use crate::browser::clock::{
    TaskTimePresentation, local_full_millis, now_millis, task_time_millis,
};

const REFRESH_MILLIS: u32 = 3_000;
/// Pixels from the bottom edge that still count as reading the newest line.
const FOLLOW_SLACK_PX: i32 = 8;

#[derive(Properties, PartialEq)]
pub(super) struct InspectorProps {
    pub selection: Selection,
    pub position: Option<usize>,
    pub count: usize,
    pub in_list: Option<bool>,
    pub ready: bool,
    pub can_stop: bool,
    pub scope: Option<crate::socket::SessionScope>,
    pub on_action: Callback<ClientAction, bool>,
    pub on_back: Callback<()>,
    pub on_nav: Callback<i32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Summary,
    Output,
}

/// Text enters Yew ownership once; refreshing the clock or toggling a card
/// does not copy the command or bounded output again.
struct Evidence {
    phase: LaneJobPhase,
    command: AttrValue,
    output: AttrValue,
    summary: Html,
    truncated: bool,
    observed: AttrValue,
}

impl From<LaneJobAnswer> for Evidence {
    fn from(answer: LaneJobAnswer) -> Self {
        let summary = html! {
            <>
                <p class="app-job-status">
                    {answer.kind.as_str()}{" · "}{phase_label(answer.phase)}
                    if let Some(ending) = answer.ending { {" · "}{ending} }
                </p>
                <dl class="app-job-facts">
                    <dt>{"Job"}</dt><dd>{answer.target.id}</dd>
                    <dt>{"Scope"}</dt><dd>{answer.target.scope.topology.to_string()}{"/"}{answer.target.scope.lane.to_string()}</dd>
                    <dt>{"Cwd"}</dt><dd>{answer.cwd}</dd>
                    <dt>{"Started"}</dt><dd>{timestamp(answer.started_millis)}</dd>
                    <dt>{"Deadline"}</dt><dd>{answer.timeout_millis.map_or_else(
                        || "No deadline".to_owned(), |millis| format!("{:.1}s after start", millis as f64 / 1_000.0),
                    )}</dd>
                    if answer.phase == LaneJobPhase::Ended {
                        <dt>{"Ended"}</dt><dd>{timestamp(answer.ended_millis)}</dd>
                    }
                </dl>
            </>
        };
        Self {
            phase: answer.phase,
            command: answer.command.into(),
            output: answer.output.into(),
            summary,
            truncated: answer.output_truncated,
            observed: local_full_millis(now_millis()),
        }
    }
}

fn timestamp(millis: Option<u64>) -> Html {
    let Some(millis) = millis else {
        return html! { "Not observed" };
    };
    match i64::try_from(millis) {
        Ok(instant)
            if js_sys::Date::new(&JsValue::from_f64(millis as f64))
                .get_time()
                .is_finite() =>
        {
            task_time_millis(instant, TaskTimePresentation::Elapsed)
        }
        _ => html! { format!("{millis} Unix milliseconds") },
    }
}

fn phase_label(phase: LaneJobPhase) -> &'static str {
    match phase {
        LaneJobPhase::Starting => "starting",
        LaneJobPhase::Running => "running",
        LaneJobPhase::Stopping => "stopping",
        LaneJobPhase::Ended => "ended",
    }
}

#[function_component(Inspector)]
pub(super) fn inspector(props: &InspectorProps) -> Html {
    let Controls {
        data,
        busy,
        view,
        confirm,
        copied,
        cancel,
        panel,
        nonce,
        request,
    } = use_controls(props);
    let (scroller, onscroll) = use_follow_output(*view);
    let evidence = data.as_ref().and_then(|result| result.as_ref().ok());
    let can_kill = props.ready
        && props.can_stop
        && props.in_list == Some(true)
        && !*busy
        && evidence.is_some_and(|evidence| {
            matches!(
                evidence.phase,
                LaneJobPhase::Starting | LaneJobPhase::Running
            )
        });
    let onkeydown = escape_action(&confirm, &props.on_back);
    let copy = copy_action(evidence, *view, &copied, &nonce);
    let previous = props.on_nav.reform(|_| -1);
    let next = props.on_nav.reform(|_| 1);
    let back = props.on_back.reform(|_| ());
    let refresh = request.reform(|_| false);
    let ask = {
        let confirm = confirm.clone();
        Callback::from(move |_| confirm.set(true))
    };
    let abort = {
        let confirm = confirm.clone();
        Callback::from(move |_| confirm.set(false))
    };
    let kill = {
        let confirm = confirm.clone();
        let request = request.clone();
        Callback::from(move |_| {
            confirm.set(false);
            request.emit(true);
        })
    };
    let summary = {
        let view = view.clone();
        Callback::from(move |_| view.set(View::Summary))
    };
    let output = {
        let view = view.clone();
        Callback::from(move |_| view.set(View::Output))
    };
    html! {
        <div class="app-job-inspector" ref={panel} data-accent={props.selection.accent.clone()}
            data-phase={evidence.map_or("unknown", |evidence| phase_label(evidence.phase))} {onkeydown}>
            <div class="app-job-toolbar" aria-label="Job controls">
                <button id="spice-job-back" type="button" onclick={back} title="Back to jobs (Escape)" aria-label="Back to jobs">{"←"}</button>
                if *confirm {
                    <span class="app-job-confirm" role="status" title={format!("Terminate {}’s command process group", props.selection.label)}>{"Kill "}{props.selection.target.id.clone()}{"?"}</span>
                    <button type="button" ref={cancel} onclick={abort}>{"Cancel"}</button>
                    <button type="button" class="app-job-kill" onclick={kill} disabled={!can_kill} aria-label="Confirm kill job">{"Kill"}</button>
                } else {
                    <button type="button" onclick={previous} disabled={props.count == 0 || props.count == 1 && props.position.is_some()}
                        title="Previous matching job" aria-label="Previous job">{"‹"}</button>
                    <span class="app-job-position" title={props.selection.label.to_string()}>{props.position.map_or_else(|| "—".to_owned(), |index| (index + 1).to_string())}{"/"}{props.count}</span>
                    <button type="button" onclick={next} disabled={props.count == 0 || props.count == 1 && props.position.is_some()}
                        title="Next matching job" aria-label="Next job">{"›"}</button>
                    <button type="button" onclick={summary} aria-label="Summary" aria-pressed={(*view == View::Summary).to_string()}>
                        <span class="app-job-control-full">{"Summary"}</span><span class="app-job-control-short" aria-hidden="true">{"Info"}</span>
                    </button>
                    <button type="button" onclick={output} aria-label="Output" aria-pressed={(*view == View::Output).to_string()}>
                        <span class="app-job-control-full">{"Output"}</span><span class="app-job-control-short" aria-hidden="true">{"Log"}</span>
                    </button>
                    <span class="app-job-toolbar-spacer" />
                    <button type="button" onclick={refresh} disabled={!props.ready || *busy} title="Refresh job evidence" aria-label="Refresh job">{"↻"}</button>
                    <button type="button" onclick={copy} disabled={evidence.is_none()} title={if *view == View::Summary { "Copy full command" } else { "Copy visible output" }}
                        aria-label={if *view == View::Summary { "Copy command" } else { "Copy output" }}>{"⧉"}</button>
                    <button type="button" class="app-job-kill" onclick={ask} disabled={!can_kill} title="Kill this job's process group">{"Kill"}</button>
                }
            </div>
            <div class="app-job-evidence" ref={scroller} {onscroll} aria-busy={busy.to_string()}>
                <div class="app-job-evidence-heading">
                    <strong>{props.selection.label.clone()}</strong>
                    if !props.ready { <span>{"Not live · cached"}</span> }
                    else if props.in_list == Some(false) { <span>{"Not in current catalog"}</span> }
                    else if props.in_list.is_none() { <span>{"Catalog unavailable"}</span> }
                    else if *busy { <span role="status">{"Reading…"}</span> }
                    else if let Some(evidence) = evidence { <span title={format!("Observed {}", evidence.observed)}>{phase_label(evidence.phase)}</span> }
                    if let Some(copied) = copied.as_ref() { <span role="status">{copied.clone()}</span> }
                </div>
                {match data.as_ref() {
                    None => html! { <p class="app-empty-state">{if props.ready { "Reading keeper evidence…" } else { "Reconnect to inspect this job." }}</p> },
                    Some(Err(error)) => html! { <p class="app-job-error" role="alert">{error.clone()}</p> },
                    Some(Ok(evidence)) if *view == View::Summary => html! {
                        <><pre class="app-job-full-command">{evidence.command.clone()}</pre>{evidence.summary.clone()}</>
                    },
                    Some(Ok(evidence)) => html! {
                        <>
                            if evidence.truncated { <p class="app-job-tail-note">{"Latest output only · earlier output omitted"}</p> }
                            if evidence.output.is_empty() { <p class="app-empty-state">{"No output recorded."}</p> }
                            else { <pre class="app-job-output">{evidence.output.clone()}</pre> }
                        </>
                    },
                }}
            </div>
        </div>
    }
}

struct Controls {
    data: UseStateHandle<Option<Result<Rc<Evidence>, String>>>,
    busy: UseStateHandle<bool>,
    view: UseStateHandle<View>,
    confirm: UseStateHandle<bool>,
    copied: UseStateHandle<Option<String>>,
    cancel: NodeRef,
    panel: NodeRef,
    nonce: Rc<RefCell<u64>>,
    request: Callback<bool>,
}

#[hook]
fn use_controls(props: &InspectorProps) -> Controls {
    let data = use_state(|| None::<Result<Rc<Evidence>, String>>);
    let busy = use_state(|| false);
    let live_busy = use_mut_ref(|| false);
    let phase = use_mut_ref(|| None::<LaneJobPhase>);
    let nonce = use_mut_ref(|| 0_u64);
    let view = use_state(|| View::Summary);
    let confirm = use_state(|| false);
    let copied = use_state(|| None::<String>);
    let cancel = use_node_ref();
    let panel = use_node_ref();
    let request = job_request(props, &data, &busy, &live_busy, &phase, &nonce);
    {
        let data = data.clone();
        let confirm = confirm.clone();
        let copied = copied.clone();
        let view = view.clone();
        use_effect_with(props.selection.target.clone(), move |_| {
            data.set(None);
            confirm.set(false);
            copied.set(None);
            view.set(View::Summary);
            || ()
        });
    }
    {
        let nonce = nonce.clone();
        let live_busy = live_busy.clone();
        let busy = busy.clone();
        let phase = phase.clone();
        let request = request.clone();
        let ready = props.ready;
        use_effect_with(
            (props.selection.target.clone(), props.ready, props.scope),
            move |_| {
                *nonce.borrow_mut() += 1;
                *live_busy.borrow_mut() = false;
                *phase.borrow_mut() = None;
                busy.set(false);
                if ready {
                    request.emit(false);
                }
                move || {
                    *nonce.borrow_mut() += 1;
                }
            },
        );
    }
    {
        let phase = phase.clone();
        let request = request.clone();
        let panel = panel.clone();
        let ready = props.ready;
        use_effect_with(
            (props.selection.target.clone(), props.ready, props.scope),
            move |_| {
                let timer = ready.then(|| {
                    gloo_timers::callback::Interval::new(REFRESH_MILLIS, move || {
                        if phase
                            .borrow()
                            .is_some_and(|phase| phase != LaneJobPhase::Ended)
                            && panel
                                .cast::<web_sys::HtmlElement>()
                                .is_some_and(|panel| panel.get_bounding_client_rect().width() > 0.0)
                        {
                            request.emit(false);
                        }
                    })
                });
                move || drop(timer)
            },
        );
    }
    {
        let cancel = cancel.clone();
        use_effect_with(*confirm, move |confirm| {
            if *confirm && let Some(cancel) = cancel.cast::<web_sys::HtmlElement>() {
                let _ = cancel.focus();
            }
            || ()
        });
    }
    Controls {
        data,
        busy,
        view,
        confirm,
        copied,
        cancel,
        panel,
        nonce,
        request,
    }
}

/// Keep the Output view on its newest line while the reader stays at the
/// bottom. Scrolling away releases it and scrolling back resumes it; Summary
/// opens at its top.
#[hook]
fn use_follow_output(view: View) -> (NodeRef, Callback<Event>) {
    let scroller = use_node_ref();
    let follow = use_mut_ref(|| false);
    {
        let scroller = scroller.clone();
        let follow = follow.clone();
        use_effect_with(view, move |view| {
            *follow.borrow_mut() = *view == View::Output;
            if *view == View::Summary
                && let Some(scroller) = scroller.cast::<web_sys::Element>()
            {
                scroller.set_scroll_top(0);
            }
            || ()
        });
    }
    {
        let scroller = scroller.clone();
        let follow = follow.clone();
        use_effect(move || {
            if view == View::Output
                && *follow.borrow()
                && let Some(scroller) = scroller.cast::<web_sys::Element>()
            {
                scroller.set_scroll_top(scroller.scroll_height());
            }
            || ()
        });
    }
    let onscroll = Callback::from(move |event: Event| {
        if let Some(scroller) = event.target_dyn_into::<web_sys::Element>() {
            let gap = scroller.scroll_height() - scroller.scroll_top() - scroller.client_height();
            *follow.borrow_mut() = gap <= FOLLOW_SLACK_PX;
        }
    });
    (scroller, onscroll)
}

fn escape_action(
    confirm: &UseStateHandle<bool>,
    on_back: &Callback<()>,
) -> Callback<KeyboardEvent> {
    {
        let confirm = confirm.clone();
        let on_back = on_back.clone();
        Callback::from(move |event: KeyboardEvent| {
            if event.key() == "Escape" && !event.is_composing() {
                event.prevent_default();
                event.stop_propagation();
                if *confirm {
                    confirm.set(false);
                } else {
                    on_back.emit(());
                }
            }
        })
    }
}

fn job_request(
    props: &InspectorProps,
    data: &UseStateHandle<Option<Result<Rc<Evidence>, String>>>,
    busy: &UseStateHandle<bool>,
    live_busy: &Rc<RefCell<bool>>,
    phase: &Rc<RefCell<Option<LaneJobPhase>>>,
    nonce: &Rc<RefCell<u64>>,
) -> Callback<bool> {
    {
        let target = props.selection.target.clone();
        let ready = props.ready;
        let on_action = props.on_action.clone();
        let data = data.clone();
        let busy = busy.clone();
        let live_busy = live_busy.clone();
        let phase = phase.clone();
        let nonce = nonce.clone();
        Callback::from(move |stop: bool| {
            if !ready || *live_busy.borrow() {
                return;
            }
            *live_busy.borrow_mut() = true;
            busy.set(true);
            *nonce.borrow_mut() += 1;
            let sent = *nonce.borrow();
            let respond: crate::client_action::JobResponse = {
                let nonce = nonce.clone();
                let busy = busy.clone();
                let live_busy = live_busy.clone();
                let phase = phase.clone();
                let data = data.clone();
                Rc::new(move |result| {
                    if *nonce.borrow() != sent {
                        return;
                    }
                    *live_busy.borrow_mut() = false;
                    busy.set(false);
                    *phase.borrow_mut() = result.as_ref().ok().map(|answer| answer.phase);
                    data.set(Some(result.map(|answer| Rc::new(Evidence::from(answer)))));
                })
            };
            if !on_action.emit(ClientAction::Job {
                target: target.clone(),
                stop,
                respond: respond.clone(),
            }) {
                respond(Err("The job request could not be queued.".to_owned()));
            }
        })
    }
}

fn copy_action(
    evidence: Option<&Rc<Evidence>>,
    view: View,
    copied: &UseStateHandle<Option<String>>,
    nonce: &Rc<RefCell<u64>>,
) -> Callback<MouseEvent> {
    let text = evidence.map(|evidence| match view {
        View::Summary => evidence.command.clone(),
        View::Output => evidence.output.clone(),
    });
    let copied = copied.clone();
    let nonce = nonce.clone();
    Callback::from(move |_| {
        let Some(text) = &text else {
            return;
        };
        let sent = *nonce.borrow();
        let nonce = nonce.clone();
        let copied = copied.clone();
        super::super::super::clipboard::copy_text(text, move |result| {
            if *nonce.borrow() == sent {
                copied.set(Some(
                    result.map_or_else(|error| error, |()| "Copied".to_owned()),
                ));
            }
        });
    })
}
