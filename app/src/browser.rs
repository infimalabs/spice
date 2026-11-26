use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use futures_channel::mpsc;
use futures_util::{FutureExt, SinkExt, StreamExt, select};
use gloo_net::http::Request;
use gloo_net::websocket::futures::WebSocket;
use gloo_net::websocket::{Message, WebSocketError};
use gloo_timers::future::TimeoutFuture;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use wire::Media;
use wire::intent::{
    BoardRead, GlobalSettingsRead, GlobalSettingsSet, LaneAction, LaneEffortRetune, LaneFastRetune,
    LaneRead, LaneRenewal, LaneRetune, LaneState, SteerInbox, SteerSend, TaskAnswer,
    TeamArrangement, TeamConfigure, VendorCatalog,
};
use wire::{
    CanonicalUuid, CommandKind, Response, StreamRequest, TaskHandle, TaskStamp, WireVersion,
    Witness,
};
use yew::prelude::*;

use crate::browser_branding::{ConnectionState, CountState, TabWork, use_browser_branding};
use crate::client_action::ClientAction;
use crate::compose::{self, Notice};
use crate::composer_submissions::{ComposerSource, Submission, SubmissionQueue, SubmissionStatus};
use crate::runtime::{AppSurface, topology_mutation_requires_refresh};
use crate::speech_queue::{
    NarrationMessage, SpeechControl, SpeechOwner, SpeechQueue, SpeechRequest,
};
use crate::view::{AppRoute, RouteEvidence};
use crate::work::WorkProjection;
use crate::{
    AppRuntime, Principal, Session, SteerPane, StreamDirective, TaskDetailState, stale_reads,
};

mod card_target;
mod clipboard;
mod clock;
mod code_copy;
mod connection;
mod deferral;
mod diagnostics;
mod draft_handoff;
mod drafts;
mod help;
mod image_media;
mod image_viewer;
mod input_history;
mod media_session;
mod mermaid;
mod navigation;
mod overlay;
mod rooms;
mod roster;
mod sender;
mod sidebar;
mod speech_runtime;
mod task_detail;
mod task_feedback;
mod task_relations;
mod toast;
mod touch_lift;
mod unsent;
mod view;
mod watchband;

use crate::PendingKind;
use crate::action_mailbox::{ActionLease, ActionQueue};
use connection::run_surface;
use sender::{
    SenderHandle, action_identity, admit_action, queue_submission, queue_submissions, random_uuid,
};
use speech_runtime::{
    AgentSpeechControls, NarrationBatch, SpeechRuntime, SpeechStatus, VoiceRoster,
    load_narration_cursors, use_agent_speech_controls, use_page_hide_speech_stop,
    use_speech_failure_toast, use_team_speech_abandonment, use_voice_roster,
};
use view::{AppRender, render_app};
use watchband::{ComposerDrafts, TeamControlAction, TeamControls, TeamRequest};

/// Identity of the compiled sources, injected by installed Spice `dev xtask -- app` and verified
/// before publish. The shell's `data-app-source` names every input, stylesheets included.
const APP_SOURCE_IDENTITY: &str = match option_env!("SPICE_APP_SOURCE_IDENTITY") {
    // env-policy: allow
    Some(identity) => identity,
    None => "unbound-development-build",
};
const CONNECTION_HANDSHAKE_TIMEOUT_MILLIS: u32 = 5_000;

/// How long the socket may stay silent before the resident is probed.
///
/// A quiet board sends nothing, so silence alone says nothing about the socket.
/// The probe is what makes the next interval meaningful: after it, silence is a
/// resident that did not answer rather than a resident with nothing to say.
const LIVENESS_PROBE_MILLIS: u32 = 15_000;

type SocketWriter = futures_util::stream::SplitSink<WebSocket, Message>;
type SocketReader = futures_util::stream::SplitStream<WebSocket>;

struct EstablishedConnection {
    session: Session,
    writer: SocketWriter,
    reader: SocketReader,
    principal: Principal,
    hydrating: bool,
}

#[derive(Clone)]
struct SubmissionHandle {
    queue: Rc<RefCell<SubmissionQueue>>,
    status: UseStateHandle<SubmissionStatus>,
}

impl SubmissionHandle {
    fn publish(&self) {
        let status = self.queue.borrow().status();
        // This handle is retained by the connection effect from its first
        // render. Its dereferenced value is therefore that render's snapshot,
        // not a live comparison point; comparing here suppresses transitions
        // back to the initial empty state. Publish the queue fold
        // unconditionally so every stage transition reaches the current tree.
        self.status.set(status);
    }
}
/// The durable submission one task's room composer is waiting on.
#[derive(Clone, Default, PartialEq, Eq)]
enum NoteSubmission {
    #[default]
    Idle,
    /// The composed text was sent as an observation in this task's room.
    Posting,
    /// The composed text was sent as this task's closing validation.
    Closing,
    /// The Design was sent to conclude done.
    Concluding,
    /// The composed text was sent as this Design's collapse reason.
    Collapsing,
    /// The composed text was sent as this task's withdrawal reason.
    Deleting,
    Failed(String),
}

impl NoteSubmission {
    const fn busy(&self) -> bool {
        matches!(
            self,
            Self::Posting | Self::Closing | Self::Concluding | Self::Collapsing | Self::Deleting
        )
    }
}

/// One task's observation composer: the draft it retains, the local refusal
/// of that draft, the submission it waits on, and whether the operator opened
/// it. Every surface showing the task shows this composer, and no other
/// task's.
#[derive(Clone, Default, PartialEq, Eq)]
struct NoteComposer {
    draft: String,
    notice: Notice,
    submission: NoteSubmission,
    open: bool,
}

impl NoteComposer {
    /// Whether the composer shows its draft and submitters: once opened, and
    /// whenever it holds a draft, a refusal, or a submission, so collapsing
    /// never hides operator work.
    fn expanded(&self) -> bool {
        self.open
            || !self.draft.is_empty()
            || self.notice != Notice::none()
            || self.submission != NoteSubmission::default()
    }
}

/// Observation composers by task. The connection keeps this handle from its
/// first render, so the shared cell is the live value and `published` is the
/// snapshot each render reads.
#[derive(Clone)]
struct NoteComposers {
    state: Rc<RefCell<BTreeMap<TaskStamp, NoteComposer>>>,
    published: UseStateHandle<BTreeMap<TaskStamp, NoteComposer>>,
}

impl NoteComposers {
    fn update(&self, task: &TaskStamp, change: impl FnOnce(&mut NoteComposer)) {
        let mut state = self.state.borrow_mut();
        let composer = state.entry(task.clone()).or_default();
        change(composer);
        if *composer == NoteComposer::default() {
            state.remove(task);
        }
        self.published.set(state.clone());
    }

    fn draft_input(&self, task: &TaskStamp) -> Callback<InputEvent> {
        let notes = self.clone();
        let task = task.clone();
        Callback::from(move |event: InputEvent| {
            let Some(textarea) = event
                .target()
                .and_then(|target| target.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
            else {
                return;
            };
            let draft = textarea.value();
            notes.update(&task, move |composer| composer.draft = draft);
        })
    }

    /// Open `task`'s composer and focus its draft, `id`, once it renders.
    fn opener(&self, task: &TaskStamp, id: String) -> Callback<MouseEvent> {
        let notes = self.clone();
        let task = task.clone();
        Callback::from(move |_: MouseEvent| {
            notes.update(&task, |composer| composer.open = true);
            watchband::focus_after_render(id.clone());
        })
    }

    fn shown(&self, task: &TaskStamp) -> NoteComposer {
        self.published.get(task).cloned().unwrap_or_default()
    }
}

#[derive(Clone)]
struct SurfaceHandles {
    connection: UseStateHandle<Connection>,
    surface: UseStateHandle<SurfaceData>,
    identity: UseStateHandle<Option<AppIdentity>>,
    app_update: UseStateHandle<Option<AppUpdate>>,
    team_controls: UseReducerHandle<TeamControls>,
    composer_drafts: UseReducerHandle<ComposerDrafts>,
    submissions: SubmissionHandle,
    notes: NoteComposers,
    /// Refusals the resident reports for actions no control is waiting on.
    refusals: toast::Toaster,
    /// Whether leaving the document now would discard operator work.
    unload: UnloadGuard,
}

/// Live view of the operator work a document unload would discard.
///
/// The connection loop outlives every render, so it reads the current drafts
/// and submissions through these shared cells rather than a captured snapshot.
#[derive(Clone)]
struct UnloadGuard {
    /// Submissions the resident has not yet accepted. No handoff can carry
    /// them, since the next document cannot learn their fate.
    awaiting: Rc<Cell<usize>>,
    drafts: Rc<RefCell<UseReducerHandle<ComposerDrafts>>>,
    notes: NoteComposers,
}

impl UnloadGuard {
    /// Store every unsent draft for this tab's next document, returning
    /// whether leaving now discards no operator work.
    fn hand_off(&self) -> bool {
        self.awaiting.get() == 0 && self.store()
    }

    fn store(&self) -> bool {
        draft_handoff::store(self.drafts.borrow().store(), &self.note_drafts())
    }

    /// Everything leaving now would discard.
    fn inventory(&self) -> Vec<crate::unsent_work::UnsentWork> {
        crate::unsent_work::inventory(
            &draft_handoff::collect(self.drafts.borrow().store(), &self.note_drafts()),
            self.awaiting.get(),
            unsent::annotating().as_deref(),
        )
    }

    fn note_drafts(&self) -> BTreeMap<TaskStamp, String> {
        self.notes
            .state
            .borrow()
            .iter()
            .map(|(task, composer)| (task.clone(), composer.draft.clone()))
            .collect()
    }
}

#[derive(Clone, PartialEq, Eq)]
struct AppUpdate {
    version: wire::AppProtocolVersion,
    generation: wire::Digest,
    loaded_generation: wire::Digest,
}

#[derive(Clone, PartialEq, Eq)]
struct AppIdentity {
    version: wire::Version,
    loaded_app: wire::Digest,
}

#[derive(Clone, PartialEq)]
enum Connection {
    Starting,
    Connecting,
    Online { epoch: String },
    Offline(String),
}

impl Connection {
    /// The socket is admitted and the published runtime is the one the
    /// resident serves now. Only this state lends the runtime's offers to a
    /// composer: through an outage and through the hydration of a restarted
    /// resident the last runtime stays on screen, and a draft submitted
    /// against its remembered occupant and epoch would only be refused.
    const fn is_online(&self) -> bool {
        matches!(self, Self::Online { .. })
    }

    fn indicator_state(&self) -> &'static str {
        match self {
            Self::Online { .. } => "online",
            Self::Offline(_) => "reconnecting",
            Self::Starting => "starting",
            Self::Connecting => "connecting",
        }
    }

    const fn branding_state(&self) -> ConnectionState {
        match self {
            Self::Starting => ConnectionState::Starting,
            Self::Connecting => ConnectionState::Connecting,
            Self::Online { .. } => ConnectionState::Online,
            Self::Offline(_) => ConnectionState::Offline,
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Starting => "Loading spice".to_owned(),
            Self::Connecting => "Opening current resident".to_owned(),
            Self::Online { epoch } => format!("Connected to resident {epoch}"),
            Self::Offline(reason) => format!("Reconnecting / {reason}"),
        }
    }
}

type SurfaceData = AppSurface;

#[derive(Deserialize)]
struct ResponseIdentity {
    id: CanonicalUuid,
}

#[hook]
fn use_composer_drag(surface: &SurfaceData) -> watchband::PrimaryComposerDragState {
    let context = {
        let surface = surface.read();
        surface
            .as_ref()
            .and_then(|runtime| {
                runtime.lanes().map(|lanes| {
                    (
                        runtime.offers(CommandKind::TeamArrange),
                        Some(lanes.topology_revision),
                    )
                })
            })
            .unwrap_or((false, None))
    };
    watchband::use_primary_composer_drag_state(context.0, context.1)
}

#[hook]
fn use_team_drag(surface: &SurfaceData) -> watchband::TeamDragState {
    let context = {
        let surface = surface.read();
        surface
            .as_ref()
            .and_then(|runtime| {
                runtime.lanes().map(|lanes| {
                    (
                        runtime.offers(CommandKind::TeamArrange),
                        Some(lanes.topology_revision),
                    )
                })
            })
            .unwrap_or((false, None))
    };
    watchband::use_team_drag_state(context.0, context.1)
}

#[function_component(AppRouter)]
fn app_router() -> Html {
    let navigation::Controls {
        route,
        image,
        notice,
        refuse: navigate,
        replace: replace_route,
        navigate: reader_intent,
        open_image,
        replace_image,
        close_image,
        refuse_image,
    } = navigation::use_route();
    html! {
        <toast::ToastProvider>
            <RouteNotice {notice} />
            <App
                {route}
                {image}
                {navigate}
                {replace_route}
                {reader_intent}
                {open_image}
                {replace_image}
                {close_image}
                {refuse_image}
            />
        </toast::ToastProvider>
    }
}

#[derive(Properties, PartialEq)]
struct RouteNoticeProperties {
    notice: Option<navigation::RouteNotice>,
}

#[function_component(RouteNotice)]
fn route_notice(properties: &RouteNoticeProperties) -> Html {
    let toaster = toast::use_toaster();
    use_effect_with(properties.notice.clone(), move |notice| {
        if let Some(notice) = notice {
            toaster.settled(notice.text.clone());
        }
    });
    Html::default()
}

#[derive(Properties, PartialEq)]
struct AppProperties {
    route: AppRoute,
    image: Option<crate::view::ImageTarget>,
    navigate: Callback<navigation::RouteRefusal>,
    replace_route: Callback<AppRoute>,
    reader_intent: Callback<navigation::Intent>,
    open_image: Callback<navigation::ImageRequest>,
    replace_image: Callback<navigation::ImageRequest>,
    close_image: Callback<crate::view::ImageTarget>,
    refuse_image: Callback<String>,
}

struct AppState {
    connection: UseStateHandle<Connection>,
    surface: UseStateHandle<SurfaceData>,
    route: AppRoute,
    route_navigate: Callback<navigation::RouteRefusal>,
    replace_route: Callback<AppRoute>,
    card_target: Option<String>,
    card_target_status: UseStateHandle<Option<card_target::Status>>,
    navigate: Callback<navigation::Intent>,
    image: Option<crate::view::ImageTarget>,
    open_image: Callback<navigation::ImageRequest>,
    replace_image: Callback<navigation::ImageRequest>,
    close_image: Callback<crate::view::ImageTarget>,
    refuse_image: Callback<String>,
    identity: UseStateHandle<Option<AppIdentity>>,
    app_update: UseStateHandle<Option<AppUpdate>>,
    team_controls: UseReducerHandle<TeamControls>,
    team_fleet: NodeRef,
    composer_drafts: UseReducerHandle<ComposerDrafts>,
    submissions: SubmissionHandle,
    sender: SenderHandle,
    composer_drag: watchband::PrimaryComposerDragState,
    composer_slots: watchband::ComposerSlots,
    team_drag: watchband::TeamDragState,
    sidebar: sidebar::Controls,
    room_controls: rooms::Controls,
    draft_controls: drafts::Controls,
    task_detail: task_detail::Controls,
    work_list: view::WorkListControls,
    work_map: view::WorkMapState,
    speech_status: UseStateHandle<SpeechStatus>,
    agents: view::AgentControls,
    /// The unsent work a held leave protected, listed until dismissed.
    unsent: UseStateHandle<Option<Vec<crate::unsent_work::UnsentWork>>>,
    open_message_task: Callback<(wire::TaskStamp, web_sys::Element)>,
}

impl AppState {
    /// Open the Spice menu, returning focus to its launcher when it closes.
    /// The menu and a card route never stand together: a card arrival closes
    /// the menu, so opening it releases the card, and following the same card
    /// link again is a fresh arrival.
    fn open_spice_sidebar(&self) -> Callback<web_sys::Element> {
        let navigate = self.navigate.clone();
        let return_focus_to = self.sidebar.return_focus_to.clone();
        Callback::from(move |target: web_sys::Element| {
            return_focus_to.emit(target);
            navigate.emit(navigation::Intent::Spice);
        })
    }

    fn render(self, work: Option<&WorkProjection<'_>>) -> Html {
        let _trace = diagnostics::span("render.app", || diagnostics::Metadata {
            cards: self
                .surface
                .read()
                .as_ref()
                .map_or(0, |runtime| runtime.messages().len())
                .into(),
            ..diagnostics::Metadata::default()
        });
        let card_target_notice = (*self.card_target_status)
            .as_ref()
            .filter(|status| {
                status.available && self.card_target.as_deref() == Some(status.target.as_str())
            })
            .map(|_| "Linked card focused.".to_owned());
        let navigate = {
            let navigate = self.navigate.clone();
            let return_focus_to = self.sidebar.return_focus_to.clone();
            let sidebar_closed = self.sidebar.content.is_none();
            Callback::from(move |intent: navigation::Intent| {
                let opens_tasks = matches!(
                    intent,
                    navigation::Intent::Work(_)
                        | navigation::Intent::Task(_)
                        | navigation::Intent::TaskView(_, _)
                        | navigation::Intent::Overview(_)
                );
                if sidebar_closed
                    && opens_tasks
                    && let Some(target) = sidebar::active_focus_target()
                {
                    return_focus_to.emit(target);
                }
                navigate.emit(intent);
            })
        };
        let open_spice_sidebar = self.open_spice_sidebar();
        let select_task = navigate.reform(navigation::Intent::Task);
        let open_overview = navigate.reform(navigation::Intent::Overview);
        let open_message_task = self.open_message_task.clone();
        let selected_work = self
            .sidebar
            .content
            .as_ref()
            .and_then(sidebar::Content::tasks)
            .cloned();
        let retry_relations = task_relations::retry_callback(
            self.surface.clone(),
            selected_work,
            self.work_map.clone(),
            self.work_list.clone(),
        );
        let app = render_app(
            AppRender {
                connection: &self.connection,
                surface: &self.surface,
                card_target_notice,
                navigate: &navigate,
                spice_sidebar_open: matches!(self.route, AppRoute::Spice),
                open_spice_sidebar,
                identity: &self.identity,
                app_update: &self.app_update,
                team_controls: &self.team_controls,
                composer_drafts: &self.composer_drafts,
                submissions: &self.submissions,
                composer_drag: &self.composer_drag,
                composer_slots: &self.composer_slots,
                team_drag: &self.team_drag,
                team_fleet: &self.team_fleet,
                unsent: &self.unsent,
                sender: &self.sender,
                room_controls: &self.room_controls,
                draft_controls: &self.draft_controls,
                agents: &self.agents,
                sidebar_content: &self.sidebar.content,
                sidebar_presented: *self.sidebar.presented,
                sidebar_closing: *self.sidebar.closing,
                work_list: &self.work_list,
                work_map: &self.work_map,
                sidebar_close: &self.sidebar.close_ref,
                sidebar_sheet: &self.sidebar.sheet_ref,
                notes: &self.task_detail.notes,
                submit_note: self.task_detail.submit_note.clone(),
                decide_trust: self.task_detail.decide_trust.clone(),
                select_task,
                open_overview,
                open_message_task,
                request_task: self.task_detail.request.clone(),
                retry_relations,
                close_sidebar: self.sidebar.close.clone(),
            },
            work,
        );
        html! {
            <image_viewer::Provider
                drafts={self.composer_drafts}
                route={self.route}
                surface={(*self.surface).clone()}
                image={self.image}
                open={self.open_image}
                replace={self.replace_image}
                close={self.close_image}
                refuse={self.refuse_image}
            >
                <mermaid::Provider>{app}</mermaid::Provider>
            </image_viewer::Provider>
        }
    }
}

#[hook]
fn use_app_state(properties: &AppProperties) -> (AppState, SpeechRuntime) {
    let route = properties.route.clone();
    let connection = use_state(|| Connection::Starting);
    let surface = use_state(SurfaceData::default);
    let card_target_status = use_state(|| None::<card_target::Status>);
    let selection = navigation::use_selection(&route, &surface, properties.navigate.clone());
    let identity = use_state(|| None::<AppIdentity>);
    let app_update = use_state(|| None::<AppUpdate>);
    let team_controls = use_reducer(TeamControls::restore);
    let team_fleet = use_node_ref();
    let handoff = use_memo((), |()| draft_handoff::take());
    let composer_drafts = {
        let handoff = handoff.clone();
        use_reducer(move || ComposerDrafts::adopt(&handoff))
    };
    watchband::use_composer_issue_toast(&composer_drafts);
    let submission_status = use_state(SubmissionStatus::default);
    let submissions = SubmissionHandle {
        queue: use_mut_ref(SubmissionQueue::default),
        status: submission_status.clone(),
    };
    let sender = SenderHandle {
        queue: use_mut_ref(|| None::<ActionQueue>),
        refusals: toast::use_toaster(),
    };
    let composer_drag = use_composer_drag(&surface);
    let composer_slots = watchband::use_composer_slots();
    let team_drag = use_team_drag(&surface);
    let navigate = properties.reader_intent.clone();
    let (sidebar, selected_task) =
        use_task_sidebar(&route, &surface, selection.as_ref(), &navigate);
    let room_controls = rooms::use_controls();
    let (work_list, work_map) = use_work_controls(
        &sender,
        &surface,
        selected_task.as_ref(),
        selected_task == selection,
        &route,
        &navigate,
    );
    let task_detail = task_detail::use_controls(
        sender.clone(),
        surface.clone(),
        selected_task,
        match &*handoff {
            Ok(handoff) => handoff.notes.as_slice(),
            Err(_) => &[],
        },
    );
    let speech_status = use_state(SpeechStatus::default);
    let speech_owner = use_state(|| None::<SpeechOwner>);
    let speech_runtime = SpeechRuntime {
        queue: use_mut_ref(SpeechQueue::default),
        controller: use_mut_ref(crate::speech::Controller::default),
        status: speech_status.clone(),
        owner: speech_owner.clone(),
        roster: use_mut_ref(VoiceRoster::default),
    };
    let speech_ref = use_mut_ref(|| speech_runtime.clone());
    *speech_ref.borrow_mut() = speech_runtime.clone();
    let narration_cursors = use_mut_ref(load_narration_cursors);
    let agent_speech = use_agent_speech_controls(speech_ref, narration_cursors, speech_owner);
    let agents = use_agent_controls(agent_speech);
    let unsent = use_state(|| None::<Vec<crate::unsent_work::UnsentWork>>);
    let open_message_task = use_open_message_task(&navigate, &sidebar.return_focus_to);
    let draft_controls = drafts::use_controls(composer_drafts.clone());
    (
        AppState {
            connection,
            surface,
            card_target: route.card_target().map(str::to_owned),
            route_navigate: properties.navigate.clone(),
            replace_route: properties.replace_route.clone(),
            route,
            image: properties.image.clone(),
            open_image: properties.open_image.clone(),
            replace_image: properties.replace_image.clone(),
            close_image: properties.close_image.clone(),
            refuse_image: properties.refuse_image.clone(),
            navigate,
            card_target_status,
            identity,
            app_update,
            team_controls,
            team_fleet,
            composer_drafts,
            submissions,
            sender,
            composer_drag,
            composer_slots,
            team_drag,
            room_controls,
            draft_controls,
            sidebar,
            task_detail,
            work_list,
            work_map,
            speech_status,
            agents,
            unsent,
            open_message_task,
        },
        speech_runtime,
    )
}

#[hook]
fn use_work_controls(
    sender: &SenderHandle,
    surface: &UseStateHandle<SurfaceData>,
    selected: Option<&crate::work::TaskOverviewSelection>,
    settled: bool,
    route: &AppRoute,
    navigate: &Callback<navigation::Intent>,
) -> (view::WorkListControls, view::WorkMapState) {
    let map = view::use_work_map(route, navigate);
    let list = view::use_work_list(selected, settled, route, navigate);
    task_relations::use_request(sender, surface, selected.cloned(), &map, &list);
    (list, map)
}

#[hook]
fn use_task_sidebar(
    route: &AppRoute,
    surface: &UseStateHandle<SurfaceData>,
    selection: Option<&crate::work::TaskOverviewSelection>,
    navigate: &Callback<navigation::Intent>,
) -> (
    sidebar::Controls,
    Option<crate::work::TaskOverviewSelection>,
) {
    let requested_content = if matches!(route, AppRoute::Spice) {
        Some(sidebar::Content::Spice)
    } else {
        selection.cloned().map(sidebar::Content::Tasks)
    };
    let close_sidebar = {
        let navigate = navigate.clone();
        Callback::from(move |()| navigate.emit(navigation::Intent::ClosePanel))
    };
    let sidebar = sidebar::use_controls(
        matches!(route, AppRoute::Spice) || work_detail_mounted(surface),
        sidebar::Request {
            content: requested_content,
            leaving: route.card_target().is_some(),
        },
        close_sidebar,
    );
    let selected_task = sidebar
        .content
        .as_ref()
        .and_then(sidebar::Content::tasks)
        .cloned();
    (sidebar, selected_task)
}

/// Open a task linked from an agent message through one callback that keeps its
/// identity across renders, so every message card's props stay equal and its
/// parsed Markdown is not rebuilt each time anything else re-renders the app.
#[hook]
fn use_open_message_task(
    navigate: &Callback<navigation::Intent>,
    return_focus_to: &Callback<web_sys::Element>,
) -> Callback<(wire::TaskStamp, web_sys::Element)> {
    let current = use_mut_ref(|| (navigate.clone(), return_focus_to.clone()));
    *current.borrow_mut() = (navigate.clone(), return_focus_to.clone());
    (*use_memo((), move |()| {
        Callback::from(move |(task, target)| {
            let (navigate, return_focus_to) = current.borrow().clone();
            return_focus_to.emit(target);
            navigate.emit(navigation::Intent::Task(task));
        })
    }))
    .clone()
}

#[hook]
fn use_app_effects(
    state: &AppState,
    speech_runtime: SpeechRuntime,
    runtime: Option<&AppRuntime>,
    work: Option<&WorkProjection<'_>>,
) {
    let tab = runtime.zip(work).map(|(runtime, work)| {
        TabWork::select(
            work.scope_counts(&crate::work::WorkScope::AllOpen).into(),
            work.scope_counts(&crate::work::WorkScope::planning())
                .into(),
            view::chrome::waiting_messages(runtime) > 0,
        )
    });
    use_browser_branding(CountState::resolve(state.connection.branding_state(), tab));
    let unload = use_unload_guard(state);
    use_deferred_app_reload(state, &unload);
    use_surface_connection(
        state.sender.clone(),
        SurfaceHandles {
            connection: state.connection.clone(),
            surface: state.surface.clone(),
            identity: state.identity.clone(),
            app_update: state.app_update.clone(),
            team_controls: state.team_controls.clone(),
            composer_drafts: state.composer_drafts.clone(),
            submissions: state.submissions.clone(),
            notes: state.task_detail.notes.clone(),
            refusals: state.sender.refusals.clone(),
            unload,
        },
    );
    task_feedback::use_feedback(state);
    use_voice_roster(&state.surface, speech_runtime.roster.clone());
    use_team_speech_abandonment(&state.surface, speech_runtime.clone());
    use_page_hide_speech_stop(speech_runtime);
    use_speech_failure_toast(&state.speech_status);
    card_target::use_target(
        state.card_target.clone(),
        &state.surface,
        &state.sender,
        &state.team_controls,
        state.card_target_status.clone(),
        state.replace_route.clone(),
    );
    navigation::presentation::use_settlement(&state.route);
    use_mosaic_root_scroll();
    watchband::use_team_arrangement_focus(&state.team_controls, true);
    watchband::use_command_modifier_preview();
    watchband::use_composer_reading_focus();
    use_route_settlement(&state.route, &state.surface, state.route_navigate.clone());
}

#[function_component(App)]
fn app(properties: &AppProperties) -> Html {
    let (state, speech_runtime) = use_app_state(properties);
    let surface = state.surface.clone();
    let surface_view = surface.read();
    let work = project_work(surface_view.as_ref());
    use_app_effects(&state, speech_runtime, surface_view.as_ref(), work.as_ref());
    code_copy::use_code_copy();
    state.render(work.as_ref())
}

/// The detail layer renders over Chat once a board is available.
fn work_detail_mounted(surface: &SurfaceData) -> bool {
    surface
        .read()
        .as_ref()
        .and_then(AppRuntime::board)
        .is_some()
}

fn project_work(runtime: Option<&AppRuntime>) -> Option<WorkProjection<'_>> {
    let runtime = runtime?;
    runtime
        .board()
        .map(|board| WorkProjection::from_board_and_lanes(board, runtime.lanes()))
}

/// Validate the route's data-backed segments once authoritative state
/// settles them, replacing a refused route with its containing view.
#[hook]
fn use_route_settlement(
    route: &AppRoute,
    surface: &SurfaceData,
    navigate: Callback<navigation::RouteRefusal>,
) {
    let evidence = RouteEvidence {
        card: card_target::use_retention(route.card_target(), surface),
    };
    use_effect_with((route.clone(), evidence), move |(route, evidence)| {
        if let Some((route, text)) = route.unavailable(*evidence) {
            navigate.emit(navigation::RouteRefusal { route, text });
        }
    });
}

#[hook]
fn use_surface_connection(sender: SenderHandle, handles: SurfaceHandles) {
    use_effect_with((), move |()| {
        let active = Rc::new(Cell::new(true));
        let (queue, wakes) = ActionQueue::new();
        *sender.queue.borrow_mut() = Some(queue.clone());
        spawn_local(run_surface(active.clone(), handles, queue, wakes));
        move || {
            active.set(false);
            sender.queue.borrow_mut().take();
        }
    });
}

/// Retry a generation handoff once the submission that made it unsafe settles.
///
/// Bootstrap attempts the same handoff before opening a session. When an
/// unknown result blocks it, the compatible stale session stays online long
/// enough to learn that result; this effect closes the loop without waiting
/// for another connection failure.
#[hook]
fn use_deferred_app_reload(state: &AppState, unload: &UnloadGuard) {
    let served = (*state.app_update)
        .as_ref()
        .filter(|update| update.generation != update.loaded_generation)
        .map(|update| update.generation.clone());
    let unsafe_submission = state.submissions.status.awaiting() != 0;
    let unload = unload.clone();
    use_effect_with(
        (served, unsafe_submission),
        move |(served, unsafe_submission)| {
            if !unsafe_submission && let Some(served) = served {
                connection::reload_into_served_app(served, &unload);
            }
        },
    );
}

#[hook]
fn use_unload_guard(state: &AppState) -> UnloadGuard {
    let guard = (*use_memo((), {
        let drafts = state.composer_drafts.clone();
        let notes = state.task_detail.notes.clone();
        move |()| UnloadGuard {
            awaiting: Rc::default(),
            drafts: Rc::new(RefCell::new(drafts)),
            notes,
        }
    }))
    .clone();
    guard.awaiting.set(state.submissions.status.awaiting());
    *guard.drafts.borrow_mut() = state.composer_drafts.clone();
    let leaving = guard.clone();
    // Any reload of this tab, the operator's included, keeps unsent drafts.
    use_effect_with((), move |()| {
        let registration = web_sys::window().and_then(|window| {
            let callback =
                Closure::<dyn FnMut(web_sys::Event)>::new(move |_event: web_sys::Event| {
                    leaving.store();
                });
            window
                .add_event_listener_with_callback("pagehide", callback.as_ref().unchecked_ref())
                .ok()
                .map(|()| (window, callback))
        });
        move || {
            if let Some((window, callback)) = registration {
                let _ = window.remove_event_listener_with_callback(
                    "pagehide",
                    callback.as_ref().unchecked_ref(),
                );
            }
        }
    });
    unsent::use_leave_guard(&guard, &state.unsent);
    guard
}

fn clamp_fixed_mosaic_root(window: &web_sys::Window) {
    let Some(document) = window.document() else {
        return;
    };
    let Ok(Some(shell)) = document.query_selector(".shell") else {
        return;
    };
    let fixed = window
        .get_computed_style(&shell)
        .ok()
        .flatten()
        .is_some_and(|style| {
            style
                .get_property_value("position")
                .is_ok_and(|value| value == "fixed")
        });
    if fixed && window.scroll_y().unwrap_or_default() != 0.0 {
        window.scroll_to_with_x_and_y(0.0, 0.0);
    }
}

#[hook]
fn use_mosaic_root_scroll() {
    use_effect_with((), move |()| {
        let registration = web_sys::window().and_then(|window| {
            clamp_fixed_mosaic_root(&window);
            let callback_window = window.clone();
            let callback = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
                clamp_fixed_mosaic_root(&callback_window);
            });
            window
                .add_event_listener_with_callback("resize", callback.as_ref().unchecked_ref())
                .ok()?;
            if window
                .add_event_listener_with_callback("scroll", callback.as_ref().unchecked_ref())
                .is_err()
            {
                let _ = window.remove_event_listener_with_callback(
                    "resize",
                    callback.as_ref().unchecked_ref(),
                );
                return None;
            }
            Some((window, callback))
        });
        move || {
            if let Some((window, callback)) = registration {
                for event in ["resize", "scroll"] {
                    let _ = window.remove_event_listener_with_callback(
                        event,
                        callback.as_ref().unchecked_ref(),
                    );
                }
            }
        }
    });
}

#[hook]
fn use_agent_controls(speech: AgentSpeechControls) -> view::AgentControls {
    view::AgentControls {
        speak: speech.speak,
        narrate: speech.narrate,
        set_automatic_speech: speech.set_automatic,
        speech_owner: speech.owner,
    }
}

fn apply_team_configuration(
    session: &Session,
    team_controls: &UseReducerHandle<TeamControls>,
    runtime: &mut AppRuntime,
    request: TeamRequest,
    bytes: &[u8],
) -> Result<(bool, bool), String> {
    let admitted: crate::Admitted<Response<wire::intent::LaneAnswer>> =
        admit_response(session, bytes)?;
    let refresh = topology_mutation_requires_refresh(&admitted.get().outcome);
    let applied = runtime.apply_lanes(&admitted);
    match &admitted.get().outcome {
        wire::Outcome::Reply(reply) if applied => {
            team_controls.dispatch(TeamControlAction::SyncLanes {
                answer: reply.body.clone(),
                settle: Some(request),
            });
        }
        wire::Outcome::Reply(_) if runtime.lane_reply_superseded(&admitted) => {
            let fast_mode = runtime.lanes().and_then(|lanes| {
                lanes
                    .lanes
                    .iter()
                    .find(|row| row.lane == request.team)
                    .map(|row| row.fast_mode)
            });
            team_controls.dispatch(TeamControlAction::SettleSuperseded { request, fast_mode });
        }
        wire::Outcome::Reply(_) => {
            team_controls.dispatch(TeamControlAction::SettleIssue {
                request,
                issue: "Team configuration reply did not apply to current lane authority."
                    .to_owned(),
            });
        }
        wire::Outcome::Refusal(refusal) => {
            team_controls.dispatch(TeamControlAction::SettleIssue {
                request,
                issue: format!("{}: {}", refusal.reason(), refusal.explanation()),
            });
        }
        wire::Outcome::Fault(fault) => {
            team_controls.dispatch(TeamControlAction::SettleIssue {
                request,
                issue: format!("{}: {}", fault.reason, fault.detail),
            });
        }
    }
    Ok((refresh, applied))
}

fn admit_response<T: for<'de> Deserialize<'de>>(
    session: &Session,
    bytes: &[u8],
) -> Result<crate::Admitted<Response<T>>, String> {
    session
        .admit_frame(bytes)
        .map_err(|fault| format!("{}: {}", fault.reason, fault.detail))
}

fn websocket_error(error: WebSocketError) -> String {
    format!("socket failed: {error}")
}

fn binary_message(message: Message) -> Result<Vec<u8>, String> {
    match message {
        Message::Bytes(bytes) => Ok(bytes),
        Message::Text(_) => Err("resident sent a text frame".to_owned()),
    }
}

fn empty(message: &'static str) -> Html {
    html! { <div class="empty">{message}</div> }
}

#[wasm_bindgen(start)]
pub fn start() {
    if connection::capture_opening_auth_token().expect("capture app opening auth token") {
        navigation::replace_location(&crate::view::AppRoute::chat());
    }
    diagnostics::install();
    let root = web_sys::window()
        .expect("browser window")
        .document()
        .expect("browser document")
        .get_element_by_id("app")
        .expect("app root");
    yew::Renderer::<AppRouter>::with_root(root).render();
}
