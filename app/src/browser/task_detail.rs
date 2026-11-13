//! Selection, focus, and observation lifecycle for contextual task detail.

use std::collections::BTreeSet;

use super::*;
use crate::browser::clock::{TaskTimePresentation, task_time};
use crate::work::TaskOverviewSelection;

#[derive(Clone)]
pub(super) struct TrustDecisionRequest {
    pub task: TaskStamp,
    pub revision: wire::Seq,
    pub digest: wire::Digest,
    pub decision: crate::client_action::TrustDecision,
}
pub(super) struct Controls {
    pub notes: NoteComposers,
    /// Ask again for one shown task whose read produced no answer.
    pub request: Callback<TaskStamp>,
    pub submit_note: Callback<(TaskStamp, SubmitEvent)>,
    pub decide_trust: Callback<TrustDecisionRequest>,
}

/// Own task-specific state for the task content mounted in the app sidebar.
#[hook]
pub(super) fn use_controls(
    sender: SenderHandle,
    surface: UseStateHandle<SurfaceData>,
    selection: Option<TaskOverviewSelection>,
    handed: &[crate::composer_drafts::HandedNote],
) -> Controls {
    let handed = || {
        handed
            .iter()
            .map(|note| {
                let composer = NoteComposer {
                    draft: note.text.clone(),
                    ..NoteComposer::default()
                };
                (note.task.clone(), composer)
            })
            .collect::<BTreeMap<_, _>>()
    };
    let notes = NoteComposers {
        state: use_mut_ref(handed),
        published: use_state(handed),
    };
    let request = request_callback(sender.clone(), surface.clone());
    {
        let sender = sender.clone();
        let surface = surface.clone();
        // A selection can precede the runtime (a cold permalink), and a
        // changed scope replaces it, so demand follows the runtime as well.
        let (demand, scope) = surface.read().as_ref().map_or_else(
            || (BTreeSet::new(), None),
            |runtime| {
                (
                    crate::decision_history::detail_demand(
                        selection.as_ref().map_or(&[], TaskOverviewSelection::tasks),
                        runtime.room_projection().preparation(),
                        runtime.board(),
                    ),
                    Some(runtime.principal().scope()),
                )
            },
        );
        use_effect_with((demand, scope), move |(demand, _)| {
            let tasks = demand.iter().cloned();
            let owed = surface
                .write()
                .as_mut()
                .is_some_and(|runtime| runtime.show_task_details(tasks));
            surface.set(surface.publication());
            if owed {
                let _admitted = admit_action(&sender, ClientAction::TaskDetails);
            }
        });
    }
    let submit_note = note_submit_callback(sender.clone(), surface.clone(), notes.clone());
    let decide_trust = trust_decision_callback(sender, surface);
    Controls {
        notes,
        request,
        submit_note,
        decide_trust,
    }
}

fn trust_decision_callback(
    sender: SenderHandle,
    surface: UseStateHandle<SurfaceData>,
) -> Callback<TrustDecisionRequest> {
    Callback::from(move |request: TrustDecisionRequest| {
        let expected = {
            let mut data = surface.write();
            data.as_mut().and_then(|runtime| {
                runtime.begin_trust_decision(
                    &request.task,
                    request.revision,
                    &request.digest,
                    request.decision,
                )
            })
        };
        let Some(expected) = expected else {
            return;
        };
        surface.set(surface.publication());
        if !admit_action(
            &sender,
            ClientAction::TrustDecision {
                task: request.task,
                revision: request.revision,
                digest: request.digest.clone(),
                decision: request.decision,
                expected,
            },
        ) {
            if let Some(runtime) = surface.write().as_mut() {
                runtime.settle_trust_decision(
                    request.digest,
                    request.decision,
                    Some(
                        "The browser could not queue this trust decision. Retry when connected."
                            .to_owned(),
                    ),
                );
            }
            surface.set(surface.publication());
        }
    })
}

/// Ask again for one shown task whose read produced no answer.
fn request_callback(
    sender: SenderHandle,
    surface: UseStateHandle<SurfaceData>,
) -> Callback<TaskStamp> {
    Callback::from(move |task: TaskStamp| {
        let owed = surface
            .write()
            .as_mut()
            .is_some_and(|runtime| runtime.retry_task_detail(&task));
        if owed {
            surface.set(surface.publication());
            let _admitted = admit_action(&sender, ClientAction::TaskDetails);
        }
    })
}

/// One authored task text field through the shared Markdown dialect and the
/// message body surface, so math, diagrams, images, and quoted sources render
/// as in cards.
///
/// Descriptions, acceptance criteria, observations, and closure validation all
/// render here. `quotes` presents the field's quote links; chips name only
/// tasks `resolves` admits, and the surrounding surface decides what
/// activating one opens.
pub(super) fn render_task_text(
    markdown: &str,
    quotes: &[wire::QuotePresentation],
    class: &'static str,
    resolves: impl Fn(&TaskStamp) -> bool,
) -> Html {
    let rendered =
        crate::composer_markdown::task_text::render_task_text(markdown, quotes, resolves);
    let sources = rendered
        .diagrams
        .into_iter()
        .map(AttrValue::from)
        .collect::<Vec<_>>();
    html! {
        <div class={classes!("task-text", class)}>
            <super::mermaid::MarkdownBody
                html={AttrValue::from(rendered.html)}
                sources={std::rc::Rc::new(sources)}
                final_answer=true
                quote_tasks={std::rc::Rc::default()}
            />
        </div>
    }
}

/// One authored task title as an inline Markdown line in the message dialect.
///
/// A live line chips the tasks its controls admit and leaves activating them
/// to the surrounding surface; a line inside another control renders handles
/// and links as text.
pub(super) fn render_task_title(
    title: &str,
    controls: &crate::composer_markdown::inline::LineControls<'_>,
) -> Html {
    let html = crate::composer_markdown::inline::render_line(title, controls);
    html! {
        <super::mermaid::MarkdownBody
            html={AttrValue::from(html)}
            sources={std::rc::Rc::default()}
            final_answer=true
            quote_tasks={std::rc::Rc::default()}
            inline=true
        />
    }
}

/// A task's authored description as task text, its quote links presented as
/// the resident resolved them in `quotes`.
pub(super) fn render_description(
    statement: &wire::intent::BoardStatement,
    quotes: &[wire::QuotePresentation],
    resolves: impl Fn(&TaskStamp) -> bool + Copy,
) -> Html {
    statement
        .description
        .as_deref()
        .map_or_else(Html::default, |description| {
            render_task_text(description, quotes, "work-detail-description", resolves)
        })
}

/// Every acceptance criterion with its statement-item handle.
pub(super) fn render_term(
    statement: &wire::intent::BoardStatement,
    resolves: impl Fn(&TaskStamp) -> bool + Copy,
) -> Html {
    let items = wire::StatementItem::each(&statement.term).collect::<Vec<_>>();
    html! {
        <ul data-task-contract={statement.born.to_string()}>
            {for statement.term.iter().zip(&items).map(|(criterion, item)| html! {
                <li>
                    <code>{item.handle(&items).to_owned()}</code>
                    {" "}
                    {render_task_text(criterion, &[], "work-detail-criterion", resolves)}
                </li>
            })}
        </ul>
    }
}

/// Open the task a task-text chip names in this detail surface.
pub(super) fn task_text_chip_click(select: &Callback<TaskStamp>) -> Callback<MouseEvent> {
    let select = select.clone();
    Callback::from(move |event: MouseEvent| {
        if let Some(task) = task_text_chip_task(event.target()) {
            event.prevent_default();
            select.emit(task);
        }
    })
}

/// The keyboard twin of [`task_text_chip_click`] for a chip's button role.
pub(super) fn task_text_chip_keydown(select: &Callback<TaskStamp>) -> Callback<KeyboardEvent> {
    let select = select.clone();
    Callback::from(move |event: KeyboardEvent| {
        if event.key() != "Enter" && event.key() != " " {
            return;
        }
        if let Some(task) = task_text_chip_task(event.target()) {
            event.prevent_default();
            select.emit(task);
        }
    })
}

fn task_text_chip_task(target: Option<web_sys::EventTarget>) -> Option<TaskStamp> {
    target?
        .dyn_into::<web_sys::Element>()
        .ok()?
        .closest(":is(.task-text, .markdown-line) .message-task-chip")
        .ok()??
        .get_attribute("data-task-handle")?
        .parse::<wire::TaskHandle>()
        .ok()?
        .born()
        .ok()
}

/// Render task-attached images with the same preview and lightbox contract as
/// message media. An empty task contributes no attachment section.
pub(super) fn render_media(media: &[wire::Media]) -> Html {
    if media.is_empty() {
        return Html::default();
    }
    html! {
        <section class="app-sheet-section app-sheet-media">
            <h3>{"Attachments"}</h3>
            <div class="message-attachments">
                {for media.iter().map(|media| {
                    let identity = media
                        .identity()
                        .expect("admitted task media has a content identity")
                        .to_string();
                    let action = format!("Open {} full size", media.name);
                    html! {
                        <button
                            key={identity.clone()}
                            type="button"
                            class="app-fact-link message-image message-image--openable"
                            title={action.clone()}
                            aria-label={action}
                            data-image-preview=""
                            data-image-name={media.name.clone()}
                        >
                            <img
                                src={media.data_url.clone()}
                                alt={media.name.clone()}
                                data-media-identity={identity}
                            />
                        </button>
                    }
                })}
            </div>
        </section>
    }
}

/// Command submits an observation; Shift+Command uses the composer's offered
/// completion, Close for a task or Conclude for a Design.
fn note_keyboard_submit(enabled: bool) -> Callback<KeyboardEvent> {
    Callback::from(move |event: KeyboardEvent| {
        if !enabled || event.key() != "Enter" || !(event.meta_key() || event.ctrl_key()) {
            return;
        }
        let Some(form) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .and_then(|target| target.closest("form").ok().flatten())
            .and_then(|form| form.dyn_into::<web_sys::HtmlFormElement>().ok())
        else {
            return;
        };
        event.prevent_default();
        if event.shift_key() {
            if event.repeat() {
                return;
            }
            let Some(button) = form
                .query_selector(COMPLETION_SUBMITTERS)
                .ok()
                .flatten()
                .and_then(|button| button.dyn_into::<web_sys::HtmlElement>().ok())
            else {
                return;
            };
            let _ = form.request_submit_with_submitter(Some(&button));
        } else {
            let _ = form.request_submit();
        }
    })
}

/// The link to the transcript offset recorded when this task was admitted,
/// labelled with that moment's live relative age.
///
/// The boundary is captured in the same admission that mints the task stamp,
/// so the stamp's moment is the linked offset's time whether or not that
/// transcript page is loaded.
pub(super) fn creator_context_link(
    class: &'static str,
    href: String,
    statement: &wire::intent::BoardStatement,
    onclick: Option<Callback<MouseEvent>>,
) -> Html {
    let created = statement.born.micros().div_euclid(1_000);
    html! {
        <a
            class={classes!(class, "creator-context-link")}
            data-creator-context="true"
            {href}
            title="Creator context at task creation"
            aria-label={format!(
                "Open creator context for {} in Chat, from when it was created",
                statement.handle,
            )}
            {onclick}
        >
            <super::clock::Age authored={created} class={classes!("creator-context-age")} />
        </a>
    }
}

/// Whether the operator may settle a task from its composer, whose live claim
/// that settlement takes, which controlled tasks it closes with it, and the
/// completion it offers beside Delete.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct TaskClosure {
    holder: Option<wire::Actor>,
    controlled: Vec<wire::TaskHandle>,
    completion: Option<ComposerSettlement>,
}

/// The settlement a composer offers for `heads`: none without the capability
/// or once the task is settled. A task closes; an open Design concludes done
/// or collapses instead; either remains directly withdrawable.
pub(super) fn closure(
    can_close: bool,
    can_conclude: bool,
    heads: &wire::intent::BoardTask,
) -> Option<TaskClosure> {
    let settled = !heads.workflow.body.status.is_pending();
    if settled || !(can_close || can_conclude) {
        return None;
    }
    let completion = match heads.design_standing() {
        Some(standing) => {
            (can_conclude && standing.is_open()).then_some(ComposerSettlement::Conclude)
        }
        None => can_close.then_some(ComposerSettlement::Close),
    };
    Some(TaskClosure {
        holder: live_holder(heads),
        controlled: heads.controlled.clone(),
        completion,
    })
}

fn live_holder(heads: &wire::intent::BoardTask) -> Option<wire::Actor> {
    heads
        .active_claims()
        .next()
        .map(|held| held.claim.body.actor.clone())
}

/// The operator's closure or withdrawal of a task. Its text reads back here,
/// beside the observations; the composer also publishes it as one.
pub(super) fn render_settlement_record(
    record: Option<crate::work::SettlementRecord<'_>>,
    name: impl Fn(&wire::Actor) -> String,
    resolves: impl Fn(&TaskStamp) -> bool,
) -> Html {
    let Some(record) = record else {
        return Html::default();
    };
    let displaced = record
        .claimed
        .map(|actor| format!("Took the claim from {}.", name(actor)));
    html! {
        <article class="work-note-card work-closure-card" data-settlement-position={record.position.to_string()}>
            <header>
                <h4 title={record.author.to_string()}>{record.heading(&name(record.author))}</h4>
                {task_time(record.at, TaskTimePresentation::Elapsed)}
            </header>
            {render_task_text(record.text, &[], "work-note-body", resolves)}
            if let Some(displaced) = displaced {
                <p class="work-closure-claim">{displaced}</p>
            }
        </article>
    }
}

/// The disposition a Plan's lane recorded instead of constructing work, with
/// the evidence it rests on and what already answers the need.
pub(super) fn render_disposition_record(
    disposition: Option<&wire::intent::TaskDetailDisposition>,
    name: impl Fn(&wire::Actor) -> String,
    task_name: impl Fn(&TaskStamp) -> String,
    resolves: impl Fn(&TaskStamp) -> bool,
) -> Html {
    let Some(record) = disposition else {
        return Html::default();
    };
    let kind = record.disposition.kind();
    let cover = record.disposition.cover().map(|cover| match cover {
        wire::body::DispositionCover::Task(task) => task_name(task),
        wire::body::DispositionCover::Commit(commit) => format!("commit {commit}"),
    });
    html! {
        <article class="work-note-card work-closure-card" data-disposition={kind}>
            <header>
                <h4 title={record.author.to_string()}>
                    {format!("No work constructed ({kind}) by {}", name(&record.author))}
                </h4>
                {task_time(&record.at, TaskTimePresentation::Elapsed)}
            </header>
            if let Some(cover) = cover {
                {render_task_text(&format!("Answered by {cover}."), &[], "work-closure-claim", &resolves)}
            }
            {render_task_text(&record.validation, &[], "work-note-body", resolves)}
        </article>
    }
}

/// The heads a closure decides against, spelled as the resident's witnesses.
fn close_witnesses(heads: &wire::intent::BoardTask) -> Vec<wire::Witness> {
    let task = heads.statement.body.born.clone();
    vec![
        wire::Witness::Task {
            task: task.clone(),
            head: heads.statement.position,
        },
        wire::Witness::Workflow {
            task: task.clone(),
            head: heads.workflow.position,
            phase: heads.workflow.body.phase,
        },
        wire::intent::BoardClaim::plane_witness(&task, &heads.claims),
    ]
}

/// Submitter values that settle the task with the composed text.
const CLOSE_SUBMITTER: &str = "close";
const CONCLUDE_SUBMITTER: &str = "conclude";
const COLLAPSE_SUBMITTER: &str = "collapse";
const DELETE_SUBMITTER: &str = "delete";
/// The enabled completion button the Shift+Command chord submits with.
const COMPLETION_SUBMITTERS: &str =
    "button[value=\"close\"]:enabled, button[value=\"conclude\"]:enabled";

#[derive(Clone, Copy, PartialEq, Eq)]
enum ComposerSettlement {
    Close,
    Conclude,
    Collapse,
    Delete,
}

impl ComposerSettlement {
    const fn submitter(self) -> &'static str {
        match self {
            Self::Close => CLOSE_SUBMITTER,
            Self::Conclude => CONCLUDE_SUBMITTER,
            Self::Collapse => COLLAPSE_SUBMITTER,
            Self::Delete => DELETE_SUBMITTER,
        }
    }

    const fn verb(self) -> &'static str {
        match self {
            Self::Close => "Close",
            Self::Conclude => "Conclude",
            Self::Collapse => "Collapse",
            Self::Delete => "Delete",
        }
    }

    const fn progress(self) -> &'static str {
        match self {
            Self::Close => "Closing…",
            Self::Conclude => "Concluding…",
            Self::Collapse => "Collapsing…",
            Self::Delete => "Deleting…",
        }
    }

    const fn submission(self) -> NoteSubmission {
        match self {
            Self::Close => NoteSubmission::Closing,
            Self::Conclude => NoteSubmission::Concluding,
            Self::Collapse => NoteSubmission::Collapsing,
            Self::Delete => NoteSubmission::Deleting,
        }
    }

    /// The buttons this completion offers: a Design concludes done or
    /// collapses with a reason; a task closes.
    const fn offered(self) -> &'static [Self] {
        match self {
            Self::Conclude | Self::Collapse => &[Self::Conclude, Self::Collapse],
            Self::Close | Self::Delete => &[Self::Close],
        }
    }
}

fn submitted_settlement(event: &SubmitEvent) -> Option<ComposerSettlement> {
    match event
        .submitter()
        .and_then(|button| button.get_attribute("value"))
        .as_deref()
    {
        Some(CLOSE_SUBMITTER) => Some(ComposerSettlement::Close),
        Some(CONCLUDE_SUBMITTER) => Some(ComposerSettlement::Conclude),
        Some(COLLAPSE_SUBMITTER) => Some(ComposerSettlement::Collapse),
        Some(DELETE_SUBMITTER) => Some(ComposerSettlement::Delete),
        _ => None,
    }
}

/// What the operator may do with a note composer: whether the socket is live,
/// whether this capability may publish, and the settlement the task admits.
#[derive(Clone, Copy)]
pub(super) struct NoteAccess<'a> {
    pub(super) connected: bool,
    pub(super) can_note: bool,
    pub(super) closure: Option<&'a TaskClosure>,
}

/// The surface callbacks a note composer wires: typed input, form submission,
/// and the opener that expands it from its Observe control.
pub(super) struct NoteCallbacks<'a> {
    pub(super) input: Callback<InputEvent>,
    pub(super) submit: &'a Callback<(TaskStamp, SubmitEvent)>,
    pub(super) open: Callback<MouseEvent>,
}

/// The one room composer every surface showing `task` renders, bound to that
/// task and preserving only that task's draft while feedback uses its
/// task-owned toast. It is a single Observe control until opened, unless it
/// already holds a draft, a refusal, or a submission. Typed input enters that
/// retained draft immediately, so a connection loss can leave the same
/// composer visible and read-only until it returns. Add publishes the draft as
/// an observation in the task's room; with a `closure`, Close or Delete
/// publishes it the same way and then settles the task on it.
pub(super) fn render_note_composer(
    id: String,
    handle: &str,
    task: &TaskStamp,
    access: NoteAccess<'_>,
    composer: &NoteComposer,
    callbacks: NoteCallbacks<'_>,
) -> Html {
    let NoteAccess {
        connected,
        can_note,
        closure,
    } = access;
    let NoteCallbacks {
        input,
        submit,
        open,
    } = callbacks;
    if connected && !can_note {
        return html! {
            <p class="work-note-capability">{"This capability can read this room but cannot publish observations."}</p>
        };
    }
    let form_key = task.to_string();
    if !composer.expanded() {
        return html! {
            <div key={form_key} class="work-note-observe">
                <button
                    type="button"
                    class="app-compact-control"
                    aria-label={format!("Observe {handle}")}
                    onclick={open}
                >{"Observe"}</button>
            </div>
        };
    }
    let busy = composer.submission.busy();
    let locked = busy || !connected;
    let posting = composer.submission == NoteSubmission::Posting;
    let settle_button = |settlement: ComposerSettlement| {
        let title = closure
            .and_then(|closure| closure.holder.as_ref())
            .map(|holder| match settlement {
                ComposerSettlement::Close => format!("Closing takes this task from {holder}"),
                ComposerSettlement::Conclude | ComposerSettlement::Collapse => format!(
                    "{holder} holds this Design; the resident refuses conclusion until it frees"
                ),
                ComposerSettlement::Delete => format!("Deleting takes this task from {holder}"),
            });
        let label = if composer.submission == settlement.submission() {
            settlement.progress()
        } else {
            settlement.verb()
        };
        html! {
            <button type="submit" class="app-compact-control" value={settlement.submitter()} disabled={locked} {title}>
                {label}
            </button>
        }
    };
    let task = task.clone();
    let label_id = id.clone();
    html! {
        <form
            key={form_key.clone()}
            class="form work-note-composer"
            onsubmit={submit.reform(move |event| (task.clone(), event))}
        >
            <label for={label_id}>{format!("Observation for {handle}")}</label>
            <textarea
                id={id}
                data-note-task={form_key}
                value={composer.draft.clone()}
                readonly={locked}
                oninput={input}
                onkeydown={note_keyboard_submit(!locked)}
            />
            <div class="team-composer-controls work-note-actions">
                <button type="submit" class="app-compact-control" disabled={locked}>
                    {if posting { "Posting…" } else { "Add" }}
                </button>
                if let Some(closure) = closure {
                    if let Some(completion) = closure.completion {
                        {for completion.offered().iter().map(|offer| settle_button(*offer))}
                    }
                    {settle_button(ComposerSettlement::Delete)}
                }
            </div>
        </form>
    }
}

/// Publish one observation in the room of the task named by the submitting
/// surface and, when a destructive submitter sent the form, then settle that
/// task on the same text as its validation or reason.
pub(super) fn note_submit_callback(
    sender: SenderHandle,
    surface: UseStateHandle<SurfaceData>,
    notes: NoteComposers,
) -> Callback<(TaskStamp, SubmitEvent)> {
    Callback::from(move |(task, event): (TaskStamp, SubmitEvent)| {
        event.prevent_default();
        let text = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .and_then(|form| form.query_selector("textarea").ok().flatten())
            .and_then(|element| element.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
            .map(|textarea| textarea.value())
            .unwrap_or_default();
        notes.update(&task, |composer| composer.draft.clone_from(&text));
        let requested_settlement = submitted_settlement(&event);
        let (handle, head, settlement) = {
            let data = surface.read();
            let Some(detail) = data
                .as_ref()
                .and_then(|runtime| runtime.task_detail(&task))
                .and_then(TaskDetailState::answer)
            else {
                notes.update(&task, |composer| {
                    composer.notice = Notice::none();
                    composer.submission = NoteSubmission::Failed(
                        "Task detail changed before the observation could be queued; the draft was retained."
                            .to_owned(),
                    );
                });
                return;
            };
            (
                detail.task.statement.body.handle.clone(),
                detail.task.statement.position,
                requested_settlement.map(|kind| WitnessedSettlement {
                    kind,
                    expected: close_witnesses(&detail.task),
                    force_claimed: live_holder(&detail.task).is_some(),
                    controlled: detail.task.controlled.clone(),
                }),
            )
        };
        if let Some(settlement) = settlement {
            queue_settlement(&sender, &notes, &task, handle, head, &text, settlement);
            return;
        }
        let text = match compose::observation(&text) {
            Ok(text) => text,
            Err(refusal) => {
                notes.update(&task, |composer| composer.notice = refusal.into());
                return;
            }
        };
        let respond = {
            let notes = notes.clone();
            let task = task.clone();
            Rc::new(
                move |result: Result<wire::rooms::RoomPostAnswer, String>| match result {
                    Ok(_) => notes.update(&task, |composer| *composer = NoteComposer::default()),
                    Err(issue) => notes.update(&task, |composer| {
                        composer.submission = NoteSubmission::Failed(issue);
                    }),
                },
            )
        };
        if admit_action(
            &sender,
            ClientAction::RoomPost(crate::client_action::RoomPostAction {
                room: handle,
                head,
                text,
                respond,
            }),
        ) {
            notes.update(&task, |composer| {
                composer.notice = Notice::none();
                composer.submission = NoteSubmission::Posting;
            });
        } else {
            retain_note_failure(&notes, &task);
        }
    })
}

/// A settlement the operator requested, with the heads and controlled closure
/// set of the detail it was requested from.
struct WitnessedSettlement {
    kind: ComposerSettlement,
    expected: Vec<wire::Witness>,
    force_claimed: bool,
    controlled: Vec<wire::TaskHandle>,
}

/// Validate and confirm one task settlement against the exact heads and
/// controlled closure set the operator saw. Close, Delete, and a Conclude
/// with a draft publish the draft as the task's observation and only once
/// the room records it settle: Close and Delete on that same text as
/// validation or reason, Conclude done. A blank Conclude and a Collapse,
/// whose draft is the conclusion's own reason, settle directly. A refused
/// observation settles nothing and keeps the draft.
fn queue_settlement(
    sender: &SenderHandle,
    notes: &NoteComposers,
    task: &TaskStamp,
    handle: wire::TaskHandle,
    head: wire::Seq,
    text: &str,
    settlement: WitnessedSettlement,
) {
    let kind = settlement.kind;
    let composed = match kind {
        ComposerSettlement::Conclude => Ok(text.trim().to_owned()),
        ComposerSettlement::Collapse => compose::collapse(text),
        ComposerSettlement::Close | ComposerSettlement::Delete => compose::close(text),
    };
    let text = match composed {
        Ok(text) => text,
        Err(refusal) => {
            notes.update(task, |composer| composer.notice = refusal.into());
            return;
        }
    };
    if !confirm_settlement(kind, &handle, &text, &settlement.controlled) {
        return;
    }
    let direct = match kind {
        ComposerSettlement::Conclude => text.is_empty(),
        ComposerSettlement::Collapse => true,
        ComposerSettlement::Close | ComposerSettlement::Delete => false,
    };
    let queued = if direct {
        admit_settlement(sender, notes, task, handle, text, settlement)
    } else {
        let settle = {
            let sender = sender.clone();
            let notes = notes.clone();
            let task = task.clone();
            let handle = handle.clone();
            let text = text.clone();
            move || {
                if !admit_settlement(&sender, &notes, &task, handle, text, settlement) {
                    notes.update(&task, |composer| {
                        composer.submission = NoteSubmission::Failed(
                            "The observation was published, but the settlement could not be \
                             queued; the draft was retained."
                                .to_owned(),
                        );
                    });
                }
            }
        };
        let refused = {
            let notes = notes.clone();
            let task = task.clone();
            move |issue| {
                notes.update(&task, |composer| {
                    composer.submission = NoteSubmission::Failed(issue);
                });
            }
        };
        let post = crate::client_action::RoomPostAction::then(handle, head, text, settle, refused);
        admit_action(sender, ClientAction::RoomPost(post))
    };
    notes.update(task, |composer| {
        composer.notice = Notice::none();
        composer.submission = if queued {
            kind.submission()
        } else {
            NoteSubmission::Failed(
                "Settlement could not be queued; the draft was retained.".to_owned(),
            )
        };
    });
}

/// Admit the settling action itself, carrying `text` as a close's validation,
/// a deletion's reason, or a collapse's reason; a conclusion done carries none.
fn admit_settlement(
    sender: &SenderHandle,
    notes: &NoteComposers,
    task: &TaskStamp,
    handle: wire::TaskHandle,
    text: String,
    settlement: WitnessedSettlement,
) -> bool {
    let WitnessedSettlement {
        kind,
        expected,
        force_claimed,
        ..
    } = settlement;
    action_identity(sender).is_some_and(|id| {
        let action = match kind {
            ComposerSettlement::Close => ClientAction::Close {
                id,
                task: handle,
                validation: text,
                expected,
                force_claimed,
            },
            ComposerSettlement::Delete => ClientAction::Delete {
                id,
                task: handle,
                reason: text,
                expected,
                force_claimed,
            },
            ComposerSettlement::Conclude | ComposerSettlement::Collapse => {
                let conclusion = match kind {
                    ComposerSettlement::Collapse => {
                        wire::body::DesignConclusion::Collapsed { reason: text }
                    }
                    _ => wire::body::DesignConclusion::Done {},
                };
                ClientAction::Conclude {
                    request: wire::intent::TaskConclude {
                        task: handle,
                        conclusion,
                    },
                    expected,
                    respond: conclusion_response(notes.clone(), task.clone()),
                }
            }
        };
        admit_action(sender, action)
    })
}

/// A landed conclusion clears the composer as a close does; a refusal keeps
/// it with the resident's reason.
fn conclusion_response(
    notes: NoteComposers,
    task: TaskStamp,
) -> crate::client_action::TaskResponse {
    Rc::new(
        move |answer: Result<wire::intent::TaskAnswer, String>| match answer {
            Ok(_) => notes.update(&task, |composer| *composer = NoteComposer::default()),
            Err(issue) => notes.update(&task, |composer| {
                composer.submission = NoteSubmission::Failed(issue);
            }),
        },
    )
}

fn confirm_settlement(
    settlement: ComposerSettlement,
    handle: &wire::TaskHandle,
    text: &str,
    controlled: &[wire::TaskHandle],
) -> bool {
    let publish = |fate: &str| {
        format!(
            "{} {handle}?\n\nThis publishes the text below as an observation in the task's \
             room, then {fate}:\n{text}",
            settlement.verb()
        )
    };
    let mut message = match settlement {
        ComposerSettlement::Conclude if text.is_empty() => format!("Conclude {handle} done?"),
        ComposerSettlement::Conclude => publish("concludes the Design done"),
        ComposerSettlement::Collapse => {
            format!("Collapse {handle}?\n\nThis records the text below as its reason:\n{text}")
        }
        ComposerSettlement::Close => publish("closes the task with it as its validation"),
        ComposerSettlement::Delete => publish("deletes the task with it as its reason"),
    };
    if !controlled.is_empty() {
        message.push_str("\n\nThis also closes these controlled tasks:");
        for task in controlled {
            message.push_str("\n- ");
            message.push_str(task.as_str());
        }
    }
    web_sys::window()
        .and_then(|window| window.confirm_with_message(&message).ok())
        .unwrap_or(false)
}

fn retain_note_failure(notes: &NoteComposers, task: &TaskStamp) {
    notes.update(task, |composer| {
        composer.notice = Notice::none();
        composer.submission = NoteSubmission::Failed(
            "Observation could not be queued; the draft was retained.".to_owned(),
        );
    });
}
