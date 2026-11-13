//! The operator's editable prospective work, with an optional saved topic.
//! Author-held Drafts stay off allocation until explicit hand-off. Submitted
//! uncontrolled Drafts may be triaged; only the shared phase-zero validator
//! activates them for execution.
//!
//! A Draft's context is authored in the lane composer's own draft state under
//! a Draft key, so its text, image chooser, paste and drop, preview,
//! annotation, removal, bounds, and refusals are the lane composer's. Its text
//! is the description; its images are the Draft's image artifacts.

use std::cell::Cell;
use std::rc::Rc;

use wasm_bindgen::{JsCast, JsValue};
use wire::intent::{BoardTask, TaskAnswer, TaskFile};
use wire::{CommandKind, Media, TaskIntent, TaskStamp, Witness};
use yew::prelude::*;

use super::sender::{SenderHandle, admit_action};
use super::toast;
use super::watchband::{
    ComposerAction, ComposerDrafts, ComposerTextarea, clear_composer, current_composer_draft,
    drop_images, focus_after_render, paste_images, primary_textarea_id, quote_textarea_id,
    render_image_indicators, render_quote_shards,
};
use crate::AppRuntime;
use crate::client_action::{ClientAction, DraftRequest};
use crate::composer_drafts::{ComposerDraft, NEW_DRAFT_KEY};
use crate::draft_form::{
    DraftCreation, DraftField, DraftFields, DraftForm, NewDraft, composed_statement, creation,
    media_request, prepare_context,
};

mod entry;
pub(super) use entry::ComposeEntry;

thread_local! {
    static LAST_DRAFT_MICROS: Cell<i64> = const { Cell::new(0) };
}

/// The submit button value that files the Draft instead of saving it.
const FILE_SUBMITTER: &str = "file";

/// One Draft-off creation between its admitted Draft row and confirmed filing.
#[derive(Clone, PartialEq, Eq)]
enum AutomaticFiling {
    Attaching(TaskStamp),
    Waiting(TaskStamp),
    Sending(TaskStamp),
}

impl AutomaticFiling {
    fn task(&self) -> &TaskStamp {
        match self {
            Self::Attaching(task) | Self::Waiting(task) | Self::Sending(task) => task,
        }
    }
}

/// The composer key of one open Draft's context.
fn editor_key(born: &TaskStamp) -> String {
    format!("spice-draft:{born}")
}

#[derive(Clone)]
pub(super) struct Controls {
    /// Where every Draft control and composer send reports its progress and
    /// outcome: the root toast, so no result adds a line to the surface that
    /// issued it.
    results: toast::Toaster,
    /// The composer state every lane composer shares, which holds each
    /// Draft's context under its own key.
    composer: UseReducerHandle<ComposerDrafts>,
    /// A composer Create with Draft off stays read-only from the admitted
    /// Draft row through the confirmed file transition. `Attaching` holds it
    /// while its images land, `Waiting` needs the row's heads, and `Sending`
    /// prevents a second file until the board confirms ordinary work.
    file_on_arrival: UseStateHandle<Option<AutomaticFiling>>,
}

#[hook]
pub(super) fn use_controls(composer: UseReducerHandle<ComposerDrafts>) -> Controls {
    Controls {
        results: toast::use_toaster(),
        composer,
        file_on_arrival: use_state(|| None),
    }
}

impl Controls {
    fn send(&self, sender: &SenderHandle, request: DraftRequest, landed: Callback<TaskAnswer>) {
        self.send_settled(sender, request, landed, Callback::from(|_: bool| {}));
    }

    /// Send one Draft mutation and report both durable settlement and local
    /// admission, so the creator can hold its exact composition in place.
    fn send_settled(
        &self,
        sender: &SenderHandle,
        request: DraftRequest,
        landed: Callback<TaskAnswer>,
        settled: Callback<bool>,
    ) -> bool {
        let results = self.results.clone();
        let response_settled = settled.clone();
        let respond = std::rc::Rc::new(move |result: Result<TaskAnswer, String>| {
            response_settled.emit(result.is_ok());
            match result {
                Ok(answer) => landed.emit(answer),
                Err(error) => results.settled(error),
            }
        });
        if !admit_action(sender, ClientAction::Draft { request, respond }) {
            settled.emit(false);
            self.results.settled("The task change could not be queued.");
            return false;
        }
        true
    }

    /// Send `requests` in order, each once the one before it landed, so a
    /// refusal stops the rest and is the result reported.
    fn send_each(
        &self,
        sender: &SenderHandle,
        mut requests: Vec<DraftRequest>,
        landed: Callback<TaskAnswer>,
    ) {
        if requests.is_empty() {
            return;
        }
        let first = requests.remove(0);
        if requests.is_empty() {
            self.send(sender, first, landed);
            return;
        }
        let controls = self.clone();
        let next = sender.clone();
        self.send(
            sender,
            first,
            Callback::from(move |_: TaskAnswer| {
                controls.send_each(&next, requests.clone(), landed.clone());
            }),
        );
    }
}

/// Whether the form was submitted by its File control rather than Save.
fn files(event: &SubmitEvent) -> bool {
    event
        .submitter()
        .is_some_and(|button| button.get_attribute("value").as_deref() == Some(FILE_SUBMITTER))
}

/// Send a new Draft's creation, then its images once it lands. The creator
/// empties as soon as the Draft exists and transfers its exact context to that
/// Draft. Draft-on creation exposes the editor immediately. Draft-off creation
/// keeps it hidden through image attachment and filing; either refusal releases
/// the complete Draft for Save or Create.
fn create(
    controls: &Controls,
    sender: &SenderHandle,
    created: DraftCreation,
    form: web_sys::HtmlFormElement,
    open: &Callback<TaskStamp>,
    filed: bool,
    sending: UseStateHandle<bool>,
) {
    let DraftCreation {
        create,
        media,
        description,
    } = created;
    let controls_after = controls.clone();
    let sender_after = sender.clone();
    let open = open.clone();
    let file_on_arrival = controls.file_on_arrival.clone();
    let landed = Callback::from(move |answer: TaskAnswer| {
        let controls = &controls_after;
        form.reset();
        clear_composer(NEW_DRAFT_KEY, &controls.composer);
        if filed {
            controls
                .results
                .pending(format!("Creating {}…", answer.task));
        } else {
            controls
                .results
                .settled(format!("Recorded Draft {}.", answer.task));
        }
        let Ok(born) = answer.task.born() else {
            return;
        };
        if filed {
            let state = if media.is_empty() {
                AutomaticFiling::Waiting(born.clone())
            } else {
                AutomaticFiling::Attaching(born.clone())
            };
            file_on_arrival.set(Some(state));
        }
        controls.composer.dispatch(ComposerAction::Hydrate {
            lane: editor_key(&born),
            body: description.clone(),
            media: media.clone(),
        });
        open.emit(born.clone());
        if media.is_empty() {
            return;
        }
        match media_request(&answer.task, media.clone()) {
            Ok(request) => {
                let results = controls.results.clone();
                let count = media.len();
                let automatic = file_on_arrival.clone();
                let attached = Callback::from(move |answer: TaskAnswer| {
                    let images = if count == 1 { "image" } else { "images" };
                    if filed {
                        results.pending(format!("Creating {} with {count} {images}…", answer.task));
                    } else {
                        results.settled(format!(
                            "Recorded Draft {} with {count} {images}.",
                            answer.task
                        ));
                    }
                    if filed {
                        automatic.set(Some(AutomaticFiling::Waiting(born.clone())));
                    }
                });
                let automatic = file_on_arrival.clone();
                let settled = Callback::from(move |accepted: bool| {
                    if filed && !accepted {
                        automatic.set(None);
                    }
                });
                controls.send_settled(&sender_after, request, attached, settled);
            }
            Err(problem) => {
                if filed {
                    file_on_arrival.set(None);
                }
                controls.results.settled(problem);
            }
        }
    });
    let pending = sending.clone();
    let settled = Callback::from(move |accepted: bool| {
        pending.set(false);
        if !accepted {
            entry::refocus_if_open();
        }
    });
    if controls.send_settled(sender, create, landed, settled) {
        sending.set(true);
    }
}

/// The editor an unheld open Draft's task detail carries: save revises its
/// content, and file validates it as complete ordinary work and files it
/// atomically.
pub(super) fn render_editor(
    runtime: Option<&AppRuntime>,
    controls: &Controls,
    sender: &SenderHandle,
    row: &BoardTask,
    media: &[Media],
) -> Html {
    if controls
        .file_on_arrival
        .as_ref()
        .is_some_and(|filing| filing.task() == &row.statement.body.born)
        || !editable_draft(row)
    {
        return Html::default();
    }
    html! {
        <DraftEditor
            key={row.statement.body.born.to_string()}
            row={row.clone()}
            media={media.to_vec()}
            edits={runtime.is_some_and(|runtime| runtime.offers(CommandKind::TaskEdit))}
            files={runtime.is_some_and(|runtime| runtime.offers(CommandKind::TaskFile))}
            controls={controls.clone()}
            sender={sender.clone()}
        />
    }
}

#[derive(Properties)]
struct EditorProps {
    row: BoardTask,
    /// The image artifacts the Draft holds, as task detail returned them.
    media: Vec<Media>,
    edits: bool,
    files: bool,
    controls: Controls,
    sender: SenderHandle,
}

impl PartialEq for EditorProps {
    fn eq(&self, other: &Self) -> bool {
        self.row == other.row
            && self.media == other.media
            && self.edits == other.edits
            && self.files == other.files
            && self.controls.composer == other.controls.composer
            && Rc::ptr_eq(&self.sender.queue, &other.sender.queue)
    }
}

/// One open Draft's editor. Its fields are its own: each render only rebases
/// them onto a newer admitted revision, so live refreshes never take what the
/// operator typed, and its key reopens it for another Draft. Its context
/// composer is drawn from the Draft's admitted description and images unless
/// the operator already holds a draft of it.
#[function_component]
fn DraftEditor(props: &EditorProps) -> Html {
    let key = editor_key(&props.row.statement.body.born);
    let form = use_mut_ref(|| {
        let mut form = DraftForm::open(&props.row);
        if let Some(kept) = props.controls.composer.get(&key) {
            form.edit(DraftField::Description, kept.body.clone());
        }
        form
    });
    let redraw = use_force_update();
    form.borrow_mut().refresh(&props.row);
    let description = form
        .borrow()
        .fields()
        .get(DraftField::Description)
        .to_owned();
    use_context_composer(&key, props, &description);
    let edit = {
        let form = form.clone();
        Callback::from(move |(field, value): (DraftField, String)| {
            form.borrow_mut().edit(field, value);
            redraw.force_update();
        })
    };
    let described = {
        let form = form.clone();
        Callback::from(move |event: InputEvent| {
            if let Some(textarea) = event.target_dyn_into::<web_sys::HtmlTextAreaElement>() {
                form.borrow_mut()
                    .edit(DraftField::Description, textarea.value());
            }
        })
    };
    let submit = editor_submit(props, &key, &form);
    let context = render_context(
        &key,
        &props.controls.composer,
        "3",
        Some((described, &description)),
        "Description",
        "Write here. Paste, drop, or attach images.",
        true,
    );
    let fields = form.borrow();
    html! {
        <section class="app-sheet-section" data-draft-editor={props.row.statement.body.handle.to_string()}>
            <h3>{"Draft"}</h3>
            <p class="muted">{"Private to the operator and its origin thread. Save revises it; Create files it as ordinary work under current policy, or, when something it needs is missing, submits it for triage, after which it can no longer be edited here."}</p>
            <form class="draft-form" onsubmit={submit}>
                {content_fields(fields.fields(), &edit, context)}
                <div class="draft-actions">
                    <button type="submit" class="app-compact-control" disabled={!props.edits}>{"Save Draft"}</button>
                    <button type="submit" class="app-compact-control" value={FILE_SUBMITTER} disabled={!props.files}>{"Create"}</button>
                </div>
            </form>
        </section>
    }
}

/// File a Draft-off creation exactly once after its images and admitted row
/// arrive, keeping its transient Draft editor hidden through attachment and
/// filing. Either refusal releases the editor. Successful filing remains
/// hidden until the refreshed board row proves it is ordinary work.
#[hook]
pub(super) fn use_file_on_arrival(
    runtime: Option<&AppRuntime>,
    controls: &Controls,
    sender: &SenderHandle,
) {
    let filing = controls.file_on_arrival.as_ref().cloned();
    let row = runtime
        .filter(|runtime| runtime.offers(CommandKind::TaskFile))
        .and_then(AppRuntime::board)
        .and_then(|board| {
            filing.as_ref().and_then(|filing| {
                board
                    .tasks
                    .iter()
                    .find(|row| &row.statement.body.born == filing.task())
            })
        })
        .cloned();
    let controls = controls.clone();
    let sender = sender.clone();
    use_effect_with((filing, row), move |(filing, row)| match (filing, row) {
        (Some(AutomaticFiling::Waiting(_)), Some(row)) => {
            controls.file_on_arrival.set(Some(AutomaticFiling::Sending(
                row.statement.body.born.clone(),
            )));
            let automatic = controls.file_on_arrival.clone();
            let settled = Callback::from(move |accepted: bool| {
                if !accepted {
                    automatic.set(None);
                }
            });
            send_filing(&controls, &sender, row, settled);
        }
        (Some(AutomaticFiling::Sending(_)), Some(row))
            if !row.workflow.body.is_draft() || row.workflow.body.is_submitted_draft() =>
        {
            controls.file_on_arrival.set(None);
        }
        _ => {}
    });
}

/// File `row` under its current heads, reporting whether it became ordinary
/// work or a submitted Draft and whether the resident accepted the transition.
fn send_filing(
    controls: &Controls,
    sender: &SenderHandle,
    row: &BoardTask,
    settled: Callback<bool>,
) {
    let results = controls.results.clone();
    let landed = Callback::from(move |answer: TaskAnswer| {
        if answer.status == "submitted" {
            let message = answer.diagnostic.as_deref().map_or_else(
                || format!("Submitted Draft {} for triage.", answer.task),
                |diagnostic| format!("Submitted Draft {} for triage: {diagnostic}", answer.task),
            );
            results.settled(message);
        } else {
            results.settled(format!("Created {}.", answer.task));
        }
    });
    controls.send_settled(sender, filing_request(row), landed, settled);
}

/// Draw the editor's context composer once from the Draft's admitted
/// content, and redraw its text when a newer admitted description rebased the
/// form's `description`, which the operator had not changed. Both follow only
/// admitted text, so a local edit never becomes what the composer was drawn as.
#[hook]
fn use_context_composer(key: &str, props: &EditorProps, description: &str) {
    let admitted = props
        .row
        .statement
        .body
        .description
        .clone()
        .unwrap_or_default();
    {
        let composer = props.controls.composer.clone();
        let body = admitted.clone();
        let media = props.media.clone();
        use_effect_with(key.to_owned(), move |key| {
            composer.dispatch(ComposerAction::Hydrate {
                lane: key.clone(),
                body,
                media,
            });
        });
    }
    let composer = props.controls.composer.clone();
    let seen = use_mut_ref(|| None::<String>);
    let key = key.to_owned();
    let rebased = description == admitted;
    use_effect_with(admitted, move |admitted| {
        let previous = seen.borrow_mut().replace(admitted.clone());
        if rebased
            && previous.is_some_and(|previous| previous != *admitted)
            && current_composer_draft(&key, &composer).body != *admitted
        {
            composer.dispatch(ComposerAction::SetBody {
                lane: key,
                body: admitted.clone(),
            });
        }
    });
}

/// Save sends the fields that moved and, when it changed, the image set;
/// File files the admitted Draft.
fn editor_submit(
    props: &EditorProps,
    key: &str,
    form: &Rc<std::cell::RefCell<DraftForm>>,
) -> Callback<SubmitEvent> {
    let form = form.clone();
    let controls = props.controls.clone();
    let sender = props.sender.clone();
    let row = props.row.clone();
    let admitted = props.media.clone();
    let key = key.to_owned();
    Callback::from(move |event: SubmitEvent| {
        event.prevent_default();
        if files(&event) {
            send_filing(&controls, &sender, &row, Callback::from(|_: bool| {}));
            return;
        }
        let results = controls.results.clone();
        let current = current_composer_draft(&key, &controls.composer);
        let requests = prepare_context(current.clone()).and_then(|context| {
            let media = context.attachments;
            form.borrow_mut()
                .edit(DraftField::Description, context.message);
            form.borrow().save(media, &admitted)
        });
        match requests {
            Ok(requests) => {
                let sent = form.borrow().fields().clone();
                let form = form.clone();
                let (composer, key) = (controls.composer.clone(), key.clone());
                let landed = Callback::from(move |answer: TaskAnswer| {
                    form.borrow_mut().saved(sent.clone());
                    composer.dispatch(ComposerAction::Saved {
                        lane: key.clone(),
                        draft: current.clone(),
                    });
                    results.settled(format!("Saved Draft {}.", answer.task));
                });
                controls.send_each(&sender, requests, landed);
            }
            Err(problem) => results.settled(problem),
        }
    })
}

/// A pending author-held Draft no lane claims. Submitted Drafts have already
/// left the operator's hands for triage, so they remain read-only even when
/// phase-zero validation leaves them at the Draft index.
fn editable_draft(task: &BoardTask) -> bool {
    task.workflow.body.is_draft()
        && !task.workflow.body.is_submitted_draft()
        && task.workflow.body.status.is_pending()
        && task.active_claims().next().is_none()
}

/// An open Draft's editable statement fields around its shared context.
fn content_fields(
    fields: &DraftFields,
    edit: &Callback<(DraftField, String)>,
    context: Html,
) -> Html {
    let value = |field: DraftField| fields.get(field).to_owned();
    let typed =
        |field: DraftField| edit.reform(move |event: InputEvent| (field, event_value(&event)));
    let chosen = edit.reform(|event: Event| (DraftField::Intent, event_value(&event)));
    let intent = fields.intent().ok().flatten();
    html! {
        <>
            <label class="draft-field-kind">
                <span>{"Kind"}</span>
                <select name="intent" onchange={chosen}>
                    <option value="" selected={intent.is_none()}>{"Task"}</option>
                    {for TaskIntent::ALL.into_iter().map(|choice| html! {
                        <option value={choice.as_str()} selected={intent == Some(choice)}>
                            {if choice == TaskIntent::Plan { "Plan" } else { "Design" }}
                        </option>
                    })}
                </select>
            </label>
            <label class="draft-field-title"><span>{"Title"}</span><input name="title" type="text" required=true value={value(DraftField::Title)} oninput={typed(DraftField::Title)} /></label>
            <label class="draft-field-pitch"><span>{"Pitch"}</span><input name="pitch" type="text" required=true maxlength="160" placeholder="One plain sentence saying why this work matters" value={value(DraftField::Pitch)} oninput={typed(DraftField::Pitch)} /></label>
            {context}
        </>
    }
}

/// A Draft's context composer: the lane composer's textarea with its image
/// chooser, previews, paste, and drop, held under `key`. Command-Enter submits
/// the form around it, as it sends from a lane. The global creator keeps its
/// quotes and images in its shared longer-context field instead.
///
/// An editor passes its description field: the handler that hears its text
/// and the text it holds. Until the store draws `key`, the textarea shows that
/// text, so its first mount never reports an empty description as an edit.
fn render_context(
    key: &str,
    composer: &UseReducerHandle<ComposerDrafts>,
    rows: &'static str,
    description: Option<(Callback<InputEvent>, &str)>,
    label: &'static str,
    placeholder: &str,
    attached: bool,
) -> Html {
    let (oninput, described) = description.unzip();
    let draft = composer.get(key).cloned().unwrap_or_else(|| ComposerDraft {
        body: described.unwrap_or_default().to_owned(),
        ..ComposerDraft::default()
    });
    let placeholder = placeholder.to_owned();
    let ondragover = Callback::from(|event: DragEvent| event.prevent_default());
    html! {
        <div
            class="draft-field-description draft-context"
            data-draft-context={key.to_owned()}
            {oninput}
            {ondragover}
            ondrop={drop_images(key.to_owned(), composer.clone())}
        >
            if attached && key != NEW_DRAFT_KEY {
                <div class="draft-context-heading">
                    <span>{label}</span>
                    {render_image_indicators(key, &draft.attachments, None, composer, false)}
                </div>
            }
            if attached && !draft.quotes.is_empty() {
                {render_quote_shards(
                    key,
                    &draft,
                    None,
                    composer,
                    Callback::from(submit_on_command_enter),
                    "Why this source matters to the work.",
                    false,
                )}
            }
            <div class="draft-context-editor">
                <ComposerTextarea
                    key={draft.body.clone()}
                    id={primary_textarea_id(key)}
                    lane={key.to_owned()}
                    drafts={composer.dispatcher()}
                    label={label.to_owned()}
                    body={draft.body.clone()}
                    {placeholder}
                    rows={rows}
                    primary_target={None::<String>}
                    quote_identity={None::<String>}
                    broadcast_help={(key == NEW_DRAFT_KEY && attached).then(|| entry::BROADCAST_HELP_ID.to_owned())}
                    onkeydown={Callback::from(submit_on_command_enter)}
                    onpaste={paste_images(key.to_owned(), None, composer.clone())}
                />
                if attached && key == NEW_DRAFT_KEY {
                    {render_image_indicators(key, &draft.attachments, None, composer, false)}
                }
            </div>
        </div>
    }
}

/// Command- or Control-Enter submits the form the composer sits in.
fn submit_on_command_enter(event: KeyboardEvent) {
    if event.key() != "Enter" || !(event.meta_key() || event.ctrl_key()) || event.shift_key() {
        return;
    }
    let form = event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        .and_then(|element| element.closest("form").ok().flatten())
        .and_then(|form| form.dyn_into::<web_sys::HtmlFormElement>().ok());
    if let Some(form) = form {
        event.prevent_default();
        let _ = form.request_submit();
    }
}

/// The value of the control an event fired on.
fn event_value(event: &Event) -> String {
    event
        .target()
        .and_then(|target| js_sys::Reflect::get(&target, &JsValue::from_str("value")).ok())
        .and_then(|value| value.as_string())
        .unwrap_or_default()
}

fn submitted_form(event: &SubmitEvent) -> Option<web_sys::HtmlFormElement> {
    event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::HtmlFormElement>().ok())
}

/// One named control's current value.
fn field(form: &web_sys::HtmlFormElement, name: &str) -> String {
    form.query_selector(&format!("[name=\"{name}\"]"))
        .ok()
        .flatten()
        .and_then(|element| js_sys::Reflect::get(&element, &JsValue::from_str("value")).ok())
        .and_then(|value| value.as_string())
        .unwrap_or_default()
}

/// The creator's non-text choice; its one visible composer carries the
/// statement fields.
fn new_draft(form: &web_sys::HtmlFormElement) -> NewDraft {
    NewDraft {
        intent: field(form, "intent"),
    }
}

/// The guarded file transition for `row`: the advance out of draft, placed by
/// the validator from the saved topic, the router or the team's destination.
fn filing_request(row: &BoardTask) -> DraftRequest {
    let task = row.statement.body.born.clone();
    DraftRequest::File {
        request: TaskFile {
            task: row.statement.body.handle.clone(),
            topic: None,
            claim: false,
        },
        expected: vec![
            Witness::Task {
                task: task.clone(),
                head: row.statement.position,
            },
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
        ],
    }
}

/// A batch-ordered birth stamp after this page's high-water and absent from its
/// current tasks.
fn mint_stamp<'a>(taken: impl Iterator<Item = &'a TaskStamp> + Clone) -> Result<TaskStamp, String> {
    let now = (js_sys::Date::now() * 1_000.0) as i64;
    let page_high_water = taken.clone().map(TaskStamp::micros).max().unwrap_or(0);
    LAST_DRAFT_MICROS.with(|last| {
        let high_water = page_high_water.max(last.get());
        let start = now.max(high_water.saturating_add(1));
        let stamp = TaskStamp::allocate(start, 1, taken)
            .map_err(|error| error.to_string())?
            .pop()
            .expect("one requested task stamp is allocated");
        last.set(stamp.micros());
        Ok(stamp)
    })
}
