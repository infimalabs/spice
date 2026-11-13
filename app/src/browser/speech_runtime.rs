//! App speech playback: the queue and controller one page drives, the
//! controls agent panels speak and narrate through, and the page lifecycle
//! hooks that stop or abandon playback.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use gloo_timers::future::TimeoutFuture;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use super::{SurfaceData, media_session, toast};
use crate::AppRuntime;
use crate::runtime::LaneRow;
use crate::speech::{SystemObservers, SystemStart, VoiceRequest};
use crate::speech_queue::{
    Advance, AutomaticChange, Enqueue, NarrationCursorRow, NarrationCursors, NarrationMessage,
    SpeechEntry, SpeechMode, SpeechOwner, SpeechQueue, SpeechRequest,
};
use crate::voice_spread::{self, VoicePolicy};
use wire::CanonicalUuid;

const NARRATION_CURSOR_STORAGE: &str = "spice:narration-cursors:v1";

/// How long a system request waits for the browser to publish its voices.
///
/// Engines enumerate voices once per page and announce the list with
/// `voiceschanged`; the wait covers that first-load window. An engine with no
/// voices never announces anything, so the wait needs an end.
const VOICE_PUBLICATION_DEADLINE_MILLIS: u32 = 5_000;

#[derive(Clone, Default, PartialEq)]
pub(super) enum SpeechStatus {
    #[default]
    Idle,
    /// Speech started, and the automatic spread stepped down from premium
    /// voices; the notice names the gap and its remedy, once per page.
    Noticed(String),
    Failed(String),
}

#[derive(Clone, PartialEq)]
pub(super) struct NarrationBatch {
    pub(super) team: String,
    pub(super) materialization: bool,
    pub(super) mode: SpeechMode,
    pub(super) messages: Vec<NarrationMessage>,
}

#[derive(Clone)]
pub(super) struct AgentSpeechControls {
    pub(super) speak: Callback<SpeechRequest>,
    pub(super) narrate: Callback<NarrationBatch>,
    pub(super) set_automatic: Callback<(String, SpeechMode)>,
    pub(super) owner: UseStateHandle<Option<SpeechOwner>>,
}

#[derive(Clone)]
pub(super) struct SpeechRuntime {
    pub(super) queue: Rc<RefCell<SpeechQueue>>,
    pub(super) controller: Rc<RefCell<crate::speech::Controller>>,
    pub(super) status: UseStateHandle<SpeechStatus>,
    pub(super) owner: UseStateHandle<Option<SpeechOwner>>,
    pub(super) roster: Rc<RefCell<VoiceRoster>>,
}

/// The roster voice spreading and Media Session metadata read: every team's
/// members, plus each worktree's lane and current display label.
#[derive(Clone, Default, PartialEq)]
pub(super) struct VoiceRoster {
    teams: Vec<(CanonicalUuid, Vec<CanonicalUuid>)>,
    members: HashMap<String, (CanonicalUuid, String)>,
}

impl VoiceRoster {
    fn new(lanes: &[LaneRow]) -> Self {
        Self {
            teams: crate::watchband::topology(lanes),
            members: lanes
                .iter()
                .flat_map(|team| &team.members)
                .map(|member| {
                    let label = crate::watchband::agent_message_label(
                        &member.worktree,
                        member.name.as_deref(),
                    );
                    (member.worktree.clone(), (member.lane, label.to_owned()))
                })
                .collect(),
        }
    }
}

impl SpeechRuntime {
    fn enqueue(&self, request: SpeechRequest) {
        let decision = self.queue.borrow_mut().enqueue(request);
        if let Enqueue::Start(entry) = decision {
            self.start(entry);
        }
    }

    fn toggle_message(&self, request: SpeechRequest) {
        let was_playing = self.queue.borrow().active_owner() == request.owner();
        self.stop();
        if !was_playing {
            self.enqueue(request);
        }
    }

    fn start(&self, entry: SpeechEntry) {
        let generation = self.controller.borrow_mut().begin();
        self.owner.set(entry.owner().cloned());
        self.status.set(SpeechStatus::Idle);
        self.sync_media_session();
        match entry.playback().clone() {
            wire::SpeechPlayback::Disabled {} => self.finish(entry.id(), None),
            wire::SpeechPlayback::System {
                voices,
                words_per_minute,
            } => self.start_system(entry, generation, voices, words_per_minute),
        }
    }

    fn start_system(
        &self,
        entry: SpeechEntry,
        generation: u64,
        voices: Vec<String>,
        words_per_minute: u32,
    ) {
        let ended = self.clone();
        let failed = self.clone();
        let awaiting = self.clone();
        let id = entry.id();
        let resumed = entry.clone();
        let result = {
            let roster = self.roster.borrow();
            let member = roster.members.get(entry.worktree()).map(|(lane, _)| *lane);
            let (user_agent, max_touch_points) = if voices.is_empty() && member.is_some() {
                web_sys::window().map_or_else(Default::default, |window| {
                    let navigator = window.navigator();
                    (
                        navigator.user_agent().unwrap_or_default(),
                        navigator.max_touch_points(),
                    )
                })
            } else {
                Default::default()
            };
            let request = match voice_spread::policy(
                !voices.is_empty(),
                member.is_some(),
                &user_agent,
                max_touch_points,
            ) {
                VoicePolicy::Listed => VoiceRequest::Listed(&voices),
                VoicePolicy::DeviceDefault => VoiceRequest::Default,
                VoicePolicy::Spread { premium_expected } => VoiceRequest::Spread {
                    teams: &roster.teams,
                    member: member.expect("spreading requires a roster member"),
                    premium_expected,
                },
            };
            self.controller.borrow_mut().start_system(
                generation,
                entry.worktree(),
                entry.text(),
                request,
                words_per_minute,
                SystemObservers {
                    voices: {
                        let voices = voices.clone();
                        move || {
                            awaiting.resume_system(
                                resumed.clone(),
                                generation,
                                voices.clone(),
                                words_per_minute,
                            );
                        }
                    },
                    ended: move || ended.defer_finish(id, None),
                    failed: move |issue| {
                        failed.defer_finish(id, Some(issue));
                    },
                },
            )
        };
        match result {
            Ok(SystemStart::Speaking { notice: None }) => {}
            Ok(SystemStart::Speaking {
                notice: Some(notice),
            }) => self.status.set(SpeechStatus::Noticed(notice)),
            // The browser has published no voices yet. This entry stays the
            // loading queue head until `voiceschanged` resumes it, so the
            // first utterance of a page load speaks instead of refusing a
            // voice the browser had not enumerated.
            Ok(SystemStart::AwaitingVoices) => self.refuse_unpublished_voices(id, generation),
            Err(error) => self.finish(id, Some(error)),
        }
    }

    /// Continue system playback once the browser publishes its voice list.
    ///
    /// The first event consumes this generation's one-shot admission before
    /// deferring a task, so a burst of `voiceschanged` notifications schedules
    /// one continuation. Deferral avoids dropping the controller-owned
    /// observing closure while it is running. Stopping or superseding the
    /// request invalidates the later ownership check, so nothing stale starts.
    fn resume_system(
        &self,
        entry: SpeechEntry,
        generation: u64,
        voices: Vec<String>,
        words_per_minute: u32,
    ) {
        if !self.controller.borrow().claim_voice_publication(generation) {
            return;
        }
        let runtime = self.clone();
        spawn_local(async move {
            TimeoutFuture::new(0).await;
            if !runtime.is_current(entry.id(), generation) {
                return;
            }
            runtime.start_system(entry, generation, voices, words_per_minute);
        });
    }

    /// Refuse a request whose browser never publishes any voice.
    ///
    /// Waiting on `voiceschanged` is what lets a configured voice arrive
    /// late, but an engine carrying no voices at all never fires it. Without
    /// this deadline such a request would hold the queue head silently until
    /// a global stop, which reads as speech that vanished rather than speech
    /// this browser cannot give.
    fn refuse_unpublished_voices(&self, id: u64, generation: u64) {
        let runtime = self.clone();
        spawn_local(async move {
            TimeoutFuture::new(VOICE_PUBLICATION_DEADLINE_MILLIS).await;
            let awaiting = runtime.controller.borrow().is_awaiting_voices(generation);
            if !awaiting || !runtime.is_current(id, generation) {
                return;
            }
            runtime.finish(
                id,
                Some("this device's browser published no system voices".to_owned()),
            );
        });
    }

    fn defer_finish(&self, id: u64, failure: Option<String>) {
        let runtime = self.clone();
        spawn_local(async move {
            TimeoutFuture::new(0).await;
            runtime.finish(id, failure);
        });
    }

    fn finish(&self, id: u64, failure: Option<String>) {
        let advance = self.queue.borrow_mut().finish(id);
        match advance {
            Advance::Ignored => {}
            Advance::Idle => {
                self.controller.borrow_mut().stop();
                self.owner.set(None);
                self.status
                    .set(failure.map_or(SpeechStatus::Idle, SpeechStatus::Failed));
                self.sync_media_session();
            }
            Advance::Start(entry) => self.start(entry),
        }
    }

    fn is_current(&self, id: u64, generation: u64) -> bool {
        self.queue.borrow().is_active(id) && self.controller.borrow().is_current(generation)
    }

    fn stop(&self) {
        self.queue.borrow_mut().stop();
        self.controller.borrow_mut().stop();
        self.owner.set(None);
        self.status.set(SpeechStatus::Idle);
        self.sync_media_session();
    }

    fn set_automatic(&self, team: &str, mode: SpeechMode) {
        let change = self.queue.borrow_mut().set_mode(team, mode);
        self.sync_media_session();
        match change {
            AutomaticChange::Preserved => {}
            AutomaticChange::Idle => {
                self.controller.borrow_mut().stop();
                self.owner.set(None);
                self.status.set(SpeechStatus::Idle);
            }
            AutomaticChange::Start(entry) => self.start(entry),
        }
    }

    fn abandon_lane(&self, lane: &str) {
        self.queue.borrow_mut().abandon_lane(lane);
        self.sync_media_session();
    }

    fn sync_media_session(&self) {
        let (active, worktree) = {
            let queue = self.queue.borrow();
            (
                queue.narration_session_active(),
                queue.active_worktree().map(str::to_owned),
            )
        };
        let speaker = worktree.as_deref().and_then(|worktree| {
            self.roster
                .borrow()
                .members
                .get(worktree)
                .map(|(_, label)| label.clone())
        });
        media_session::sync(active, speaker.as_deref());
    }
}

#[hook]
pub(super) fn use_speech_failure_toast(speech_status: &UseStateHandle<SpeechStatus>) {
    let toaster = toast::use_toaster();
    use_effect_with((**speech_status).clone(), move |status| match status {
        SpeechStatus::Failed(issue) | SpeechStatus::Noticed(issue) => {
            toaster.settled(issue.clone());
        }
        SpeechStatus::Idle => {}
    });
}

#[hook]
pub(super) fn use_agent_speech_controls(
    runtime: Rc<RefCell<SpeechRuntime>>,
    narration_cursors: Rc<RefCell<NarrationCursors>>,
    owner: UseStateHandle<Option<SpeechOwner>>,
) -> AgentSpeechControls {
    let media_controls_runtime = runtime.clone();
    let _media_controls = use_mut_ref(move || {
        media_session::Controls::install(move || media_controls_runtime.borrow().stop())
    });
    let enqueue = {
        let runtime = runtime.clone();
        use_memo((), move |_| {
            Callback::from(move |request: SpeechRequest| runtime.borrow().enqueue(request))
        })
    };
    let speak = {
        let runtime = runtime.clone();
        use_memo((), move |_| {
            Callback::from(move |request: SpeechRequest| runtime.borrow().toggle_message(request))
        })
    };
    let set_automatic = {
        let runtime = runtime.clone();
        use_memo((), move |_| {
            Callback::from(move |(team, mode): (String, SpeechMode)| {
                runtime.borrow().set_automatic(&team, mode);
            })
        })
    };
    let narrate = {
        let enqueue = (*enqueue).clone();
        let set_automatic = (*set_automatic).clone();
        use_memo((), move |_| {
            narration_callback(narration_cursors, enqueue, set_automatic)
        })
    };
    AgentSpeechControls {
        speak: (*speak).clone(),
        narrate: (*narrate).clone(),
        set_automatic: (*set_automatic).clone(),
        owner,
    }
}

/// Stop every clip and clear its Media Session when the page leaves.
///
/// System synthesis runs in the operating system speech server, so an
/// utterance queued by this page keeps playing after the window that queued
/// it is gone unless the page cancels it on the way out. `pagehide` is the
/// last event every browser dispatches before a document is unloaded or
/// frozen, including tab and window close. A document restored from the
/// back-forward cache reapplies its still-selected Narrate posture on `pageshow`.
#[hook]
pub(super) fn use_page_hide_speech_stop(speech_runtime: SpeechRuntime) {
    use_effect_with((), move |()| {
        let registration = web_sys::window().and_then(|window| {
            let hidden_runtime = speech_runtime.clone();
            let pagehide =
                Closure::<dyn FnMut(web_sys::Event)>::new(move |_event: web_sys::Event| {
                    hidden_runtime.stop();
                    media_session::sync(false, None);
                });
            let shown_runtime = speech_runtime.clone();
            let pageshow =
                Closure::<dyn FnMut(web_sys::Event)>::new(move |_event: web_sys::Event| {
                    shown_runtime.sync_media_session();
                });
            window
                .add_event_listener_with_callback("pagehide", pagehide.as_ref().unchecked_ref())
                .ok()?;
            if window
                .add_event_listener_with_callback("pageshow", pageshow.as_ref().unchecked_ref())
                .is_err()
            {
                let _ = window.remove_event_listener_with_callback(
                    "pagehide",
                    pagehide.as_ref().unchecked_ref(),
                );
                return None;
            }
            Some((window, pagehide, pageshow))
        });
        move || {
            if let Some((window, pagehide, pageshow)) = registration {
                let _ = window.remove_event_listener_with_callback(
                    "pagehide",
                    pagehide.as_ref().unchecked_ref(),
                );
                let _ = window.remove_event_listener_with_callback(
                    "pageshow",
                    pageshow.as_ref().unchecked_ref(),
                );
            }
        }
    });
}

/// Keep the roster voice spreading reads in step with the rendered lanes.
#[hook]
pub(super) fn use_voice_roster(
    surface: &UseStateHandle<SurfaceData>,
    roster: Rc<RefCell<VoiceRoster>>,
) {
    let current = surface
        .read()
        .as_ref()
        .and_then(AppRuntime::lanes)
        .map(|lanes| VoiceRoster::new(&lanes.lanes));
    use_effect_with(current, move |current| {
        if let Some(current) = current {
            *roster.borrow_mut() = current.clone();
        }
    });
}

#[hook]
pub(super) fn use_team_speech_abandonment(
    surface: &UseStateHandle<SurfaceData>,
    speech_runtime: SpeechRuntime,
) {
    let teams = surface
        .read()
        .as_ref()
        .and_then(AppRuntime::lanes)
        .map(|lanes| {
            lanes
                .lanes
                .iter()
                .map(|team| team.lane.to_string())
                .collect::<BTreeSet<_>>()
        });
    let previous = use_mut_ref(|| None::<BTreeSet<String>>);
    use_effect_with(teams, move |teams| {
        let Some(current) = teams else {
            return;
        };
        if let Some(previous) = previous.borrow().as_ref() {
            for removed in previous.difference(current) {
                speech_runtime.abandon_lane(removed);
            }
        }
        *previous.borrow_mut() = Some(current.clone());
    });
}

pub(super) fn load_narration_cursors() -> NarrationCursors {
    web_sys::window()
        .and_then(|window| window.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item(NARRATION_CURSOR_STORAGE).ok().flatten())
        .and_then(|value| serde_json::from_str::<Vec<NarrationCursorRow>>(&value).ok())
        .map_or_else(NarrationCursors::default, NarrationCursors::restore)
}

fn store_narration_cursors(cursors: &NarrationCursors) {
    let Some(storage) = web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    else {
        return;
    };
    let Ok(value) = serde_json::to_string(&cursors.rows()) else {
        return;
    };
    let _ = storage.set_item(NARRATION_CURSOR_STORAGE, &value);
}

fn narration_callback(
    cursors: Rc<RefCell<NarrationCursors>>,
    speak: Callback<SpeechRequest>,
    set_automatic: Callback<(String, SpeechMode)>,
) -> Callback<NarrationBatch> {
    Callback::from(move |batch: NarrationBatch| {
        set_automatic.emit((batch.team.clone(), batch.mode));
        let lines = {
            let mut cursors = cursors.borrow_mut();
            let lines = cursors.observe(batch.materialization, &batch.messages);
            store_narration_cursors(&cursors);
            lines
        };
        for line in lines {
            let text = match batch.mode {
                SpeechMode::Quiet => None,
                SpeechMode::Speak => line.automatic_speech,
                SpeechMode::Narrate => line.narration,
            };
            if let Some(text) = text {
                speak.emit(SpeechRequest::narration(
                    line.worktree,
                    line.playback,
                    batch.team.clone(),
                    line.key,
                    text,
                ));
            }
        }
    })
}
