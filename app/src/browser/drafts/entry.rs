//! One persistent composition, with an explicit message or work destination.
//! It answers a team composer's strokes: Command- or Control-Enter submits
//! from any of its controls, and with Shift a message broadcasts to the whole
//! fleet.

use super::*;
use std::collections::BTreeSet;

use crate::browser::watchband::{
    BroadcastTarget, Composition, fleet_broadcast_targets, submit_broadcast,
};
use crate::browser::{Connection, SubmissionHandle, SurfaceData};
use crate::composer_drafts::{MediaPlacement, prepare_submission};
use crate::composer_submissions::{ComposerSource, FanoutOutcome};
use crate::watchband::SubmissionAuthority;

mod presentation;
use presentation::use_presentation;
use wire::CanonicalUuid;

/// The element describing the main composer's broadcast stroke.
pub(super) const BROADCAST_HELP_ID: &str = "app-compose-broadcast-help";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Message,
    Task,
    Plan,
    Design,
}

impl Mode {
    const ALL: [Self; 4] = [Self::Message, Self::Task, Self::Plan, Self::Design];

    const fn shortcut(self) -> &'static str {
        match self {
            Self::Message => "a",
            Self::Task => "t",
            Self::Plan => "p",
            Self::Design => "d",
        }
    }

    fn from_shortcut(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|choice| choice.shortcut() == key)
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Message => "Ask",
            Self::Task => "Task",
            Self::Plan => "Plan",
            Self::Design => "Design",
        }
    }

    /// The one visible composer's accessible name.
    const fn title_label(self) -> &'static str {
        match self {
            Self::Message => "Ask or steer",
            Self::Task => "Task statement",
            Self::Plan => "Plan statement",
            Self::Design => "Design statement",
        }
    }

    /// Work teaches the single composer decode: title, blank line, pitch,
    /// blank line, then optional context.
    const fn placeholder(self) -> &'static str {
        match self {
            Self::Message => "A plain question or steering for an agent (required)",
            Self::Task => {
                "A short name for the finished result (required)\n\nOne plain sentence on \
                 who can then do what, and why (required)\n\nThe durable need, mechanism, \
                 and invariants"
            }
            Self::Plan => {
                "A short name for the plan's outcome (required)\n\nOne plain sentence on \
                 what the finished plan lets someone do (required)\n\nIts scope and the \
                 tasks to construct"
            }
            Self::Design => {
                "The question to decide, as a short phrase (required)\n\nOne plain sentence \
                 on the end state if the answer holds (required)\n\nEvidence, options, and \
                 what would disprove it"
            }
        }
    }

    /// A quoted source adds only description context, never a title or pitch.
    const fn quote_placeholder(self) -> &'static str {
        match self {
            Self::Message => "What to ask or say about this source.",
            Self::Task => "What this source adds to the task's context.",
            Self::Plan => "What this source adds to the plan's scope.",
            Self::Design => "Evidence this source adds to the question.",
        }
    }

    /// The palette base the composer wears, as a team console wears its
    /// own: none for the operator's neutral message voice, otherwise the
    /// accent of the work's intent.
    const fn base_accent(self) -> Option<usize> {
        match self {
            Self::Message => None,
            Self::Task => Some(0),
            Self::Plan => Some(crate::watchband::PLAN_BASE),
            Self::Design => Some(crate::watchband::DESIGN_BASE),
        }
    }

    const fn intent(self) -> &'static str {
        match self {
            Self::Message | Self::Task => "",
            Self::Plan => "plan",
            Self::Design => "design",
        }
    }

    const fn stop(self) -> usize {
        match self {
            Self::Message => 0,
            Self::Task => 1,
            Self::Plan => 2,
            Self::Design => 3,
        }
    }
}

#[derive(Properties)]
pub(in crate::browser) struct Props {
    pub surface: UseStateHandle<SurfaceData>,
    pub connection: UseStateHandle<Connection>,
    pub controls: Controls,
    pub sender: SenderHandle,
    pub submissions: SubmissionHandle,
    pub open_task: Callback<TaskStamp>,
    pub navigate: Callback<crate::browser::navigation::Intent>,
    /// The principal section the expanded overlay carries below the
    /// composition: human messaging lives with the composer.
    #[prop_or_default]
    pub inbox: Html,
    /// Open principal inputs the strip announces while the sheet is closed.
    #[prop_or_default]
    pub inbox_waiting: usize,
}

impl PartialEq for Props {
    fn eq(&self, other: &Self) -> bool {
        self.surface == other.surface
            && self.connection == other.connection
            && self.controls.composer == other.controls.composer
            && self.controls.file_on_arrival == other.controls.file_on_arrival
            && Rc::ptr_eq(&self.sender.queue, &other.sender.queue)
            && self.submissions.status == other.submissions.status
            && self.open_task == other.open_task
            && self.navigate == other.navigate
            && self.inbox == other.inbox
            && self.inbox_waiting == other.inbox_waiting
    }
}

#[function_component]
pub(in crate::browser) fn ComposeEntry(props: &Props) -> Html {
    let expanded = use_state_eq(|| false);
    let mode = use_state_eq(|| Mode::Message);
    // Off unless the operator turns it on; it holds until the page reloads.
    let draft = use_state_eq(|| false);
    let sending = use_state_eq(|| false);
    // A fanout holds the composition in the submission queue until it settles.
    let held = props.submissions.status.holds(ComposerSource::Creator);
    let busy = *sending || held;
    let anchor = use_node_ref();
    let dialog = use_node_ref();
    let presentation = use_presentation(&anchor, &dialog, expanded.clone());
    let close = presentation.close.clone();
    let expand = presentation.open.clone();
    use_summons(props, &mode, &expanded, busy);
    use_mode_shortcuts(&mode, &expand, busy);
    // Whether a press would submit something, read again on every edit and
    // whenever quotes or images come or go without raising input.
    let form = use_node_ref();
    let applicable = use_state_eq(|| false);
    {
        let applicable = applicable.clone();
        let composer = props.controls.composer.clone();
        use_effect_with(
            (*mode, props.controls.composer.get(NEW_DRAFT_KEY).cloned()),
            move |(mode, _)| applicable.set(submits_something(*mode, &composer)),
        );
    }
    let edited = {
        let applicable = applicable.clone();
        let composer = props.controls.composer.clone();
        let mode = *mode;
        Callback::from(move |_: InputEvent| {
            applicable.set(submits_something(mode, &composer));
        })
    };
    let surface = props.surface.read();
    let runtime = surface.as_ref();
    super::use_file_on_arrival(runtime, &props.controls, &props.sender);
    let recipients = use_recipients(props, runtime);
    let available = |kind| props.connection.is_online() && runtime.is_some_and(|r| r.offers(kind));
    let message = *mode == Mode::Message;
    let ready = if message {
        available(CommandKind::SteerSend)
    } else {
        available(CommandKind::TaskAdd) && (*draft || available(CommandKind::TaskFile))
    };
    let created = created(props, &presentation);
    let accepted_ask = use_accepted_ask(
        props,
        close.clone(),
        presentation.finish.clone(),
        presentation.expanded,
    );
    let submit = submit(
        props,
        *mode,
        *draft,
        sending.clone(),
        created,
        accepted_ask,
        &recipients,
    );
    let targets = runtime.map_or_else(Vec::new, |runtime| {
        fleet_broadcast_targets(runtime, props.connection.is_online())
    });
    let full_broadcast = message && targets.len() > 1;
    let full_send = message && names_whole_roster(runtime, &recipients);
    let broadcast = broadcast(props, targets, busy);
    let keys = command_keys(*mode, expanded.clone(), busy, broadcast, form.clone());
    html! {
        <div ref={anchor} class="app-compose-entry"
            data-base-flavor={if mode.base_accent().is_some() { "accent" } else { "neutral" }}
            data-base-accent={mode.base_accent().map(|accent| accent.to_string())}>
            <dialog ref={dialog.clone()} open=true id="app-compose-dialog" popover="manual"
                class="app-modal app-compose-dialog app-glass-control-surface app-closed-edge app-closed-edge--block-end"
                data-expanded={presentation.expanded.to_string()}
                data-presented={presentation.visible.to_string()}
                data-inbox-waiting={props.inbox_waiting.to_string()}
                inert={presentation.closing.then_some("true")}
                data-admission-state={busy.then_some("pending")}
                aria-busy={busy.to_string()}
                aria-label="Compose"
                onkeydown={keys}>
                <div class="app-compose-surface" aria-hidden="true" />
                <form ref={form} class="draft-form app-compose-form"
                    data-draft-create={(!message).then_some("")} onsubmit={submit} oninput={edited}>
                    // Pending admission seals the content and mode selector;
                    // the utility controls live beside Help and state their
                    // own disabledness.
                    <fieldset disabled={busy}>
                        <legend class="app-compose-modes-row" onclick={expand.reform(|_: MouseEvent| ())}>
                            {mode_controls(&mode, busy)}
                        </legend>
                        <div id="app-compose-trigger" onfocusin={expand.reform(|_: FocusEvent| ())} onclick={expand.reform(|_: MouseEvent| ())}>
                            {render_context(NEW_DRAFT_KEY, &props.controls.composer, "10", None, mode.title_label(), (mode.placeholder(), mode.quote_placeholder()), true)}
                        </div>
                        {submit_action(*mode, ready, *applicable, busy, full_send, full_broadcast)}
                        {details(runtime, &recipients, *mode)}
                    </fieldset>
                    <button type="button" class="app-compose-pending-open"
                        aria-label="Open pending composer" hidden={!busy || *expanded}
                        onclick={expand.reform(|_: MouseEvent| ())} />
                    <span id={BROADCAST_HELP_ID} class="visually-hidden">
                        {"Control or Command plus Shift and Enter broadcasts this message and its direct images to every occupied agent in the fleet."}
                    </span>
                </form>
                {utility_tabs(&mode, &draft, busy, props.inbox_waiting)}
                // The Principals section names itself by its own heading.
                <div class="app-compose-inbox" hidden={!presentation.expanded}>
                    {props.inbox.clone()}
                </div>
            </dialog>
        </div>
    }
}

/// Bare mode keys outside text entry open the global composer at that
/// destination. The same modal guard as Help leaves every active sheet in
/// control of its own keyboard.
#[hook]
fn use_mode_shortcuts(mode: &UseStateHandle<Mode>, open: &Callback<()>, busy: bool) {
    let current = use_mut_ref(|| (mode.clone(), open.clone(), busy));
    *current.borrow_mut() = (mode.clone(), open.clone(), busy);
    use_effect_with((), move |()| {
        let registration = web_sys::window().and_then(|window| {
            let document = window.document()?;
            let observed = document.clone();
            let key = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(
                move |event: web_sys::KeyboardEvent| {
                    let Some(choice) = Mode::from_shortcut(&event.key()) else {
                        return;
                    };
                    let (mode, open, busy) = {
                        let current = current.borrow();
                        (current.0.clone(), current.1.clone(), current.2)
                    };
                    if busy
                        || event.ctrl_key()
                        || event.meta_key()
                        || event.alt_key()
                        || event.is_composing()
                        || event.repeat()
                        || event.default_prevented()
                        || crate::browser::overlay::typing(&event)
                        || crate::browser::overlay::native_modal_open(&observed)
                    {
                        return;
                    }
                    event.prevent_default();
                    mode.set(choice);
                    open.emit(());
                },
            );
            let _ =
                window.add_event_listener_with_callback("keydown", key.as_ref().unchecked_ref());
            Some((window, key))
        });
        move || {
            if let Some((window, key)) = registration {
                let _ = window
                    .remove_event_listener_with_callback("keydown", key.as_ref().unchecked_ref());
            }
        }
    });
}

/// Opening created work closes the composer without changing its destination.
fn created(props: &Props, presentation: &presentation::Presentation) -> Callback<TaskStamp> {
    let open = props.open_task.clone();
    let close = presentation.close.clone();
    let finish = presentation.finish.clone();
    Callback::from(move |task| {
        close.emit(());
        finish.emit(());
        open.emit(task);
    })
}

/// The agents an Ask names; none asks the best-matched agent.
#[hook]
fn use_recipients(
    props: &Props,
    runtime: Option<&AppRuntime>,
) -> UseStateHandle<BTreeSet<CanonicalUuid>> {
    let recipients = use_state_eq(BTreeSet::<CanonicalUuid>::new);
    use_current_recipients(runtime, &recipients);
    use_settled_ask(props, &recipients);
    recipients
}

/// Resolve acceptance after runtime publication, outside its mutable borrow.
#[hook]
fn use_accepted_ask(
    props: &Props,
    close: Callback<()>,
    finish: Callback<()>,
    expanded: bool,
) -> Callback<(Option<CanonicalUuid>, CanonicalUuid)> {
    let accepted_ask = use_state_eq(|| None::<(Option<CanonicalUuid>, CanonicalUuid)>);
    let focus_after_close = use_state_eq(|| None::<CanonicalUuid>);
    {
        let accepted_ask = accepted_ask.clone();
        let surface = props.surface.clone();
        let navigate = props.navigate.clone();
        let focus_after_close = focus_after_close.clone();
        use_effect_with(*accepted_ask, move |answer| {
            if let Some((requested, accepted)) = answer {
                accepted_ask.set(None);
                let lane = {
                    let surface = surface.read();
                    let lanes = surface.as_ref().and_then(AppRuntime::lanes);
                    crate::compose::accepted_ask_target(*requested, *accepted, lanes)
                };
                if let Some(lane) = lane {
                    focus_after_close.set(Some(lane));
                    close.emit(());
                    finish.emit(());
                    navigate.emit(crate::browser::navigation::Intent::Card(
                        crate::browser::watchband::primary_textarea_id(&lane.to_string()),
                    ));
                }
            }
        });
    }
    {
        let focus_after_close = focus_after_close.clone();
        use_effect_with((*focus_after_close, expanded), move |(lane, expanded)| {
            if !expanded && let Some(lane) = lane {
                focus_after_close.set(None);
                let target = crate::browser::watchband::primary_textarea_id(&lane.to_string());
                if let Some(element) = web_sys::window()
                    .and_then(|window| window.document())
                    .and_then(|document| document.get_element_by_id(&target))
                    .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
                {
                    let _ = element.focus();
                }
            }
        });
    }
    Callback::from(move |answer| accepted_ask.set(Some(answer)))
}

/// Keep only chosen agents the current roster still presents, so an Ask never
/// names an agent the operator can no longer see.
#[hook]
fn use_current_recipients(
    runtime: Option<&AppRuntime>,
    recipients: &UseStateHandle<BTreeSet<CanonicalUuid>>,
) {
    let current = runtime.and_then(AppRuntime::lanes).map(|lanes| {
        roster(lanes)
            .map(|member| member.lane)
            .collect::<BTreeSet<_>>()
    });
    let recipients = recipients.clone();
    use_effect_with(current, move |current| {
        if let Some(current) = current
            && !recipients.is_subset(current)
        {
            recipients.set(recipients.intersection(current).copied().collect());
        }
    });
}

/// Report each settled several-agent Ask once, every branch's outcome in one
/// toast. A refusal keeps the composition and narrows the chosen agents to
/// those it did not reach, so sending again retries only them.
#[hook]
fn use_settled_ask(props: &Props, recipients: &UseStateHandle<BTreeSet<CanonicalUuid>>) {
    let fanout = props
        .submissions
        .status
        .settled(ComposerSource::Creator)
        .map(|outcome| outcome.fanout);
    // An Ask that settled before this composer mounted was already reported.
    let reported = use_mut_ref(|| fanout);
    let status = props.submissions.status.clone();
    let surface = props.surface.clone();
    let results = props.controls.results.clone();
    let recipients = recipients.clone();
    use_effect_with(fanout, move |fanout| {
        if *reported.borrow() == *fanout {
            return;
        }
        *reported.borrow_mut() = *fanout;
        let Some(outcome) = status.settled(ComposerSource::Creator) else {
            return;
        };
        let surface = surface.read();
        results.settled(settlement_summary(
            outcome,
            surface.as_ref().and_then(AppRuntime::lanes),
        ));
        if !outcome.refused.is_empty() {
            recipients.set(outcome.refused.keys().copied().collect());
            refocus_if_open();
        }
    });
}

/// Every branch of a settled Ask by agent name: who was asked, and who was
/// not and why.
fn settlement_summary(outcome: &FanoutOutcome, lanes: Option<&crate::runtime::LaneView>) -> String {
    let name = |lane: &CanonicalUuid| {
        lanes
            .and_then(|lanes| lanes.member(*lane))
            .and_then(|member| member.name.clone())
            .unwrap_or_else(|| lane.to_string())
    };
    let mut parts = Vec::with_capacity(outcome.refused.len() + 2);
    if !outcome.admitted.is_empty() {
        let asked = outcome.admitted.iter().map(name).collect::<Vec<_>>();
        parts.push(format!("Asked {}.", asked.join(", ")));
    }
    for (lane, reason) in &outcome.refused {
        parts.push(format!("Not sent to {}: {reason}", name(lane)));
    }
    if !outcome.refused.is_empty() {
        parts.push("Send again to retry only those agents.".to_owned());
    }
    parts.join(" ")
}

/// Each roster member once, in team and seat order.
fn roster(lanes: &crate::runtime::LaneView) -> impl Iterator<Item = &crate::runtime::LaneMember> {
    let mut seen = BTreeSet::new();
    lanes
        .lanes
        .iter()
        .flat_map(|team| &team.members)
        .filter(move |member| seen.insert(member.lane))
}

/// Whether an Ask names every agent on a roster of two or more, vacant or
/// occupied. Compare identities as well as counts so a roster replacement
/// cannot show FULL SEND during the recipient-pruning render.
fn names_whole_roster(runtime: Option<&AppRuntime>, recipients: &BTreeSet<CanonicalUuid>) -> bool {
    let Some(lanes) = runtime.and_then(AppRuntime::lanes) else {
        return false;
    };
    let mut members = 0;
    let all_named = roster(lanes).all(|member| {
        members += 1;
        recipients.contains(&member.lane)
    });
    all_named && members > 1 && recipients.len() == members
}

/// Ask keeps its recipient choice. Work's title, pitch and longer context stay
/// in the one composer above; only the intent remains a hidden form choice.
fn details(
    runtime: Option<&AppRuntime>,
    recipients: &UseStateHandle<BTreeSet<CanonicalUuid>>,
    mode: Mode,
) -> Html {
    let message = mode == Mode::Message;
    html! {
        <div class="app-compose-details" hidden={!message}>
            {recipient_field(runtime, recipients, message)}
            <input type="hidden" name="intent" value={mode.intent()} />
        </div>
    }
}

/// Command- or Control-Enter from any control submits, as the trailing
/// action would, unless the field it came from already submitted. It opens a
/// collapsed composer so a required field it lacks shows. With Shift, a
/// message broadcasts to the fleet; work has no broadcast, so the stroke
/// does nothing there.
fn command_keys(
    mode: Mode,
    expanded: UseStateHandle<bool>,
    busy: bool,
    broadcast: Callback<()>,
    form: NodeRef,
) -> Callback<KeyboardEvent> {
    Callback::from(move |event: KeyboardEvent| {
        if event.key() != "Enter" || !(event.meta_key() || event.ctrl_key()) {
            return;
        }
        // The principal inbox below the composition keeps its own strokes.
        if event
            .target_dyn_into::<web_sys::Element>()
            .and_then(|target| target.closest(".app-compose-inbox").ok().flatten())
            .is_some()
        {
            return;
        }
        if event.shift_key() {
            event.prevent_default();
            if mode == Mode::Message && !busy && !event.repeat() {
                broadcast.emit(());
            }
            return;
        }
        if event.default_prevented() {
            return;
        }
        event.prevent_default();
        expanded.set(true);
        if let Some(form) = form.cast::<web_sys::HtmlFormElement>() {
            let _ = form.request_submit();
        }
    })
}

/// Broadcast the message's primary text and direct images to every occupied
/// agent in the fleet, as a team broadcast reaches every agent in its team.
fn broadcast(props: &Props, targets: Vec<BroadcastTarget>, busy: bool) -> Callback<()> {
    let sender = props.sender.clone();
    let composer = props.controls.composer.clone();
    let submissions = props.submissions.clone();
    let results = props.controls.results.clone();
    Callback::from(move |()| {
        if busy {
            return;
        }
        if targets.is_empty() {
            results.settled("No occupied agent can receive a broadcast.");
            return;
        }
        if submit_broadcast(
            ComposerSource::Creator,
            Composition::Primary,
            &targets,
            &sender,
            &composer,
            &submissions,
        ) {
            // Several agents report once every branch settles.
            if targets.len() > 1 {
                results.pending(format!("Broadcasting to {} agents…", targets.len()));
            } else {
                results.settled("Broadcasting to 1 agent.");
            }
        }
    })
}

/// Whether pressing the trailing action would submit: an Ask holds text, a
/// quote or an image; work's one composer decodes to a non-empty title and
/// pitch.
fn submits_something(mode: Mode, composer: &ComposerDrafts) -> bool {
    let draft = current_composer_draft(NEW_DRAFT_KEY, composer);
    if mode == Mode::Message {
        return !draft.body.trim().is_empty()
            || !draft.quotes.is_empty()
            || !draft.attachments.is_empty();
    }
    let (title, pitch, _) = composed_statement(&draft.body);
    !title.is_empty() && !pitch.is_empty()
}

/// The To row's trailing action: Send in Ask mode, Create otherwise. Both words
/// fill SEND's fixed label box; Create's glyphs compress to fit it. It takes the
/// team SEND's backlit glass only once a press would submit, and its label: an
/// Ask naming every agent on the roster, or a held broadcast stroke that would
/// reach the whole fleet, compresses it into FULL SEND. While a submission is
/// admitted it keeps its word and reads busy, as the team SEND does.
fn submit_action(
    mode: Mode,
    ready: bool,
    applicable: bool,
    sending: bool,
    full_send: bool,
    full_broadcast: bool,
) -> Html {
    html! {
        <button type="submit" class="app-compose-submit" data-lit={(ready && applicable).to_string()}
            data-full-send={full_send.to_string()}
            data-full-broadcast={full_broadcast.to_string()} aria-busy={sending.then_some("true")}
            disabled={!ready || sending}>
            if mode == Mode::Message {
                <span class="team-send-label">
                    <span class="team-send-full" aria-hidden="true">{"Full"}</span>
                    <span class="team-send-word">{"Send"}</span>
                </span>
            } else {
                <span class="team-send-label">
                    <span class="app-compose-create-word">{"Create"}</span>
                </span>
            }
        </button>
    }
}

/// The strip's horizontal destination slider has the same four controls in
/// every mode, so the strip never resizes under its moving thumb.
fn mode_controls(mode: &UseStateHandle<Mode>, sending: bool) -> Html {
    html! {
        <div class="app-control-rail app-segment-slider app-compose-modes" role="group" aria-label="Compose as"
            data-stop={mode.stop().to_string()}>
            {for Mode::ALL.map(|choice| {
                let mode = mode.clone();
                html! {
                    <button type="button" class="app-choice-chip" disabled={sending}
                        aria-pressed={(*mode == choice).to_string()}
                        aria-keyshortcuts={choice.shortcut()}
                        onclick={Callback::from(move |_| mode.set(choice))}>{choice.label()}</button>
                }
            })}
        </div>
    }
}

/// Draft shares the rail's top pair of tabs with Help. Pressed, Create records
/// a Draft. It has no meaning for an Ask and seals itself while a submission
/// is being admitted.
fn draft_control(mode: &UseStateHandle<Mode>, draft: &UseStateHandle<bool>, sending: bool) -> Html {
    let toggle_draft = {
        let draft = draft.clone();
        Callback::from(move |_: MouseEvent| draft.set(!*draft))
    };
    html! {
        <button type="button" class="app-horn-tab app-horn-tab--down app-compose-utility-tab app-compose-draft"
            aria-pressed={draft.to_string()} disabled={sending || **mode == Mode::Message}
            title={match (**mode, **draft) {
                (Mode::Message, _) => "Draft applies to Task, Plan and Design",
                (_, true) => "Draft on: Create records a Draft to finish later",
                (_, false) => "Draft off: Create makes the task available now",
            }}
            onclick={toggle_draft}>
            {"Draft"}
        </button>
    }
}

/// The agents an Ask goes to: one fixed row of choices that scrolls inline, so
/// no choice resizes the sheet. With none chosen the resident picks the
/// best-matched agent; each chosen agent is asked once.
fn recipient_field(
    runtime: Option<&AppRuntime>,
    recipients: &UseStateHandle<BTreeSet<CanonicalUuid>>,
    visible: bool,
) -> Html {
    let best = recipients.is_empty();
    let clear = {
        let recipients = recipients.clone();
        Callback::from(move |_: MouseEvent| recipients.set(BTreeSet::new()))
    };
    // The checked boxes are the choice, so changes landing between renders
    // compose instead of overwriting each other.
    let choose = {
        let recipients = recipients.clone();
        Callback::from(move |event: Event| {
            use wasm_bindgen::JsCast;
            let input: web_sys::Element = event.target_unchecked_into();
            let Some(checked) = input
                .closest(".app-compose-recipients")
                .ok()
                .flatten()
                .and_then(|row| row.query_selector_all("input:checked").ok())
            else {
                return;
            };
            recipients.set(
                (0..checked.length())
                    .filter_map(|index| checked.item(index))
                    .filter_map(|node| node.dyn_into::<web_sys::HtmlInputElement>().ok())
                    .filter_map(|input| input.value().parse().ok())
                    .collect(),
            );
        })
    };
    html! {
        <div class="app-compose-recipient" role="group" aria-labelledby="app-compose-to"
            hidden={!visible}>
            <span id="app-compose-to">{"To"}</span>
            <div class="app-compose-recipients">
                <button type="button" class="app-compose-recipient-choice" data-best-match=""
                    aria-pressed={best.to_string()} onclick={clear}
                    title="The resident picks the best-matched agent, preferring a vacant one">
                    {"Best-matched agent"}
                </button>
                {for runtime.and_then(AppRuntime::lanes).into_iter().flat_map(roster).map(|member| {
                    let lane = member.lane.to_string();
                    html! {
                        <label class="app-compose-recipient-choice" key={lane.clone()}>
                            <input type="checkbox" value={lane.clone()}
                                checked={recipients.contains(&member.lane)}
                                onchange={choose.clone()} />
                            {member.name.clone().unwrap_or(lane)}
                        </label>
                    }
                })}
            </div>
        </div>
    }
}

/// A refusal hands the composition back for editing. Focus follows only when
/// the operator is still in the dialog; one who dismissed it and moved on is
/// never pulled back, and finds the composition and its error on reopening.
pub(super) fn refocus_if_open() {
    let open = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id("app-compose-dialog"))
        .is_some_and(|dialog| dialog.get_attribute("data-expanded").as_deref() == Some("true"));
    if open {
        focus_after_render(primary_textarea_id(NEW_DRAFT_KEY));
    }
}

#[hook]
fn use_summons(
    props: &Props,
    mode: &UseStateHandle<Mode>,
    expanded: &UseStateHandle<bool>,
    sending: bool,
) {
    let summons = props.controls.composer.creator_summons().cloned();
    let handled = use_mut_ref(|| None);
    let mode = mode.clone();
    let expanded = expanded.clone();
    use_effect_with(
        (sending, summons.as_ref().map(|summons| summons.serial)),
        move |(sending, _)| {
            if !sending
                && let Some(summons) = summons
                && *handled.borrow() != Some(summons.serial)
            {
                // Settlement is not another quote arrival.
                *handled.borrow_mut() = Some(summons.serial);
                mode.set(Mode::Task);
                expanded.set(true);
                crate::browser::watchband::focus_after_render(quote_textarea_id(
                    NEW_DRAFT_KEY,
                    &summons.identity,
                ));
            }
        },
    );
}

fn submit(
    props: &Props,
    mode: Mode,
    draft: bool,
    sending: UseStateHandle<bool>,
    open: Callback<TaskStamp>,
    accepted_ask: Callback<(Option<CanonicalUuid>, CanonicalUuid)>,
    recipients: &UseStateHandle<BTreeSet<CanonicalUuid>>,
) -> Callback<SubmitEvent> {
    let controls = props.controls.clone();
    let sender = props.sender.clone();
    let surface = props.surface.clone();
    let submissions = props.submissions.clone();
    let online = props.connection.is_online();
    let recipients = recipients.clone();
    Callback::from(move |event: SubmitEvent| {
        event.prevent_default();
        if *sending {
            return;
        }
        let Some(form) = submitted_form(&event) else {
            return;
        };
        let context = current_composer_draft(NEW_DRAFT_KEY, &controls.composer);
        if mode == Mode::Message {
            let mut chosen = recipients.iter().copied();
            match (chosen.next(), chosen.next()) {
                (None, _) => {
                    send_message(&controls, &sender, None, context, &sending, &accepted_ask)
                }
                (Some(lane), None) => {
                    send_message(
                        &controls,
                        &sender,
                        Some(lane),
                        context,
                        &sending,
                        &accepted_ask,
                    );
                }
                (Some(_), Some(_)) => {
                    ask_several(
                        &controls,
                        &sender,
                        &submissions,
                        &surface,
                        online,
                        &recipients,
                    );
                }
            }
        } else {
            let surface = surface.read();
            let born = if let Some(board) = surface.as_ref().and_then(AppRuntime::board) {
                mint_stamp(board.tasks.iter().map(|task| &task.statement.body.born))
            } else {
                mint_stamp(std::iter::empty())
            };
            let fields = new_draft(&form);
            match born.and_then(|born| creation(&fields, context, born)) {
                Ok(created) => create(
                    &controls,
                    &sender,
                    created,
                    form,
                    &open,
                    !draft,
                    sending.clone(),
                ),
                Err(problem) => controls.results.settled(problem),
            }
        }
    })
}

/// Ask every chosen agent once, as one fanout that settles as one. Each chosen
/// agent must still be reachable, so the Ask goes to exactly the chosen agents
/// or to none of them.
fn ask_several(
    controls: &Controls,
    sender: &SenderHandle,
    submissions: &SubmissionHandle,
    surface: &UseStateHandle<SurfaceData>,
    online: bool,
    recipients: &BTreeSet<CanonicalUuid>,
) {
    if !submits_something(Mode::Message, &controls.composer) {
        controls
            .results
            .settled("Write a message or attach an image.");
        return;
    }
    let surface = surface.read();
    let Some((runtime, lanes)) = surface
        .as_ref()
        .and_then(|runtime| runtime.lanes().map(|lanes| (runtime, lanes)))
    else {
        controls
            .results
            .settled("The agent roster has not loaded yet.");
        return;
    };
    let authority = SubmissionAuthority::derive(online, |kind| runtime.offers(kind));
    let mut targets = Vec::with_capacity(recipients.len());
    for lane in recipients {
        let member = lanes.member(*lane);
        let Some(target) = member
            .and_then(|member| authority.target(member.occupant, member.wakeable, lanes.epoch))
        else {
            let name = member
                .and_then(|member| member.name.clone())
                .unwrap_or_else(|| lane.to_string());
            controls.results.settled(format!(
                "{name} cannot receive an Ask now; deselect it to ask the others."
            ));
            return;
        };
        targets.push(BroadcastTarget::chosen(*lane, target));
    }
    if submit_broadcast(
        ComposerSource::Creator,
        Composition::Whole,
        &targets,
        sender,
        &controls.composer,
        submissions,
    ) {
        // The settled Ask reports every agent's outcome in place of this.
        controls
            .results
            .pending(format!("Asking {} agents…", targets.len()));
    }
}

fn send_message(
    controls: &Controls,
    sender: &SenderHandle,
    lane: Option<CanonicalUuid>,
    context: crate::composer_drafts::ComposerDraft,
    sending: &UseStateHandle<bool>,
    accepted_ask: &Callback<(Option<CanonicalUuid>, CanonicalUuid)>,
) {
    let prepared = match prepare_submission(context, MediaPlacement::Markdown) {
        Ok(prepared) if !prepared.message.trim().is_empty() => prepared,
        Ok(_) => {
            controls
                .results
                .settled("Write a message or attach an image.");
            return;
        }
        Err(error) => {
            controls.results.settled(error.to_string());
            return;
        }
    };
    let results = controls.results.clone();
    let composer = controls.composer.clone();
    let pending = sending.clone();
    let sent = Rc::new(prepared.snapshot);
    let accepted_ask = accepted_ask.clone();
    let respond = Rc::new(
        move |result: Result<wire::intent::DirectiveAnswer, String>| {
            pending.set(false);
            match result {
                Ok(answer) => {
                    crate::browser::watchband::clear_sent_composer(
                        NEW_DRAFT_KEY,
                        &composer,
                        sent.clone(),
                    );
                    let Some(answered) = answer.recipient.lane() else {
                        results.settled(format!(
                            "Message accepted for {}, which is not an agent.",
                            answer.recipient
                        ));
                        return;
                    };
                    results.settled(format!("Message accepted for agent {answered}."));
                    accepted_ask.emit((lane, answered));
                }
                Err(error) => {
                    results.settled(error);
                    refocus_if_open();
                }
            }
        },
    );
    if admit_action(
        sender,
        ClientAction::ComposeMessage {
            lane,
            message: prepared.message,
            attachments: prepared.attachments,
            respond,
        },
    ) {
        sending.set(true);
    }
}

/// Outside the masked form so this layer clears both accents. Tabs run from
/// the inline end inward; Help remains available during admission, while Draft
/// explicitly seals itself. The count of open principal inputs rides beside
/// them so the closed strip announces it.
fn utility_tabs(
    mode: &UseStateHandle<Mode>,
    draft: &UseStateHandle<bool>,
    busy: bool,
    waiting: usize,
) -> Html {
    let label = match waiting {
        0 => None,
        1 => Some("1 message waiting for you".to_owned()),
        count => Some(format!("{count} messages waiting for you")),
    };
    html! {
        <div class="app-compose-utility-tabs">
            <crate::browser::help::HelpTrigger />
            {draft_control(mode, draft, busy)}
            if let Some(label) = label {
                <span class="app-count-badge app-compose-inbox-count" role="status"
                    aria-label={label.clone()} title={label}>
                    {waiting.to_string()}
                </span>
            }
        </div>
    }
}
