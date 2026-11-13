use super::*;
use crate::speech_queue::SpeechMode;

mod frame;
pub(in crate::browser) use frame::{ControlsFrame, SendSlot};

const TEAM_SPEECH_STORAGE: &str = "spice:team-speech:v1";
const HIDDEN_TEAMS_STORAGE: &str = "spice:hidden-teams:v1";

/// The control that opened a team's arrangement, where dismissal returns focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::browser) enum ArrangementOpener {
    Lights,
    Menu,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct TeamControl {
    pub(super) arrangement: Option<ArrangementOpener>,
    pub(super) speech: SpeechMode,
    pub(super) drive: bool,
    pub(super) drain: bool,
    pending_drive: Option<bool>,
    pending_drain: Option<bool>,
    /// Team fast-mode flag requested and not yet answered by the resident.
    pending_fast: Option<bool>,
    issue: Option<String>,
}

impl Default for TeamControl {
    fn default() -> Self {
        Self {
            arrangement: None,
            speech: SpeechMode::Quiet,
            drive: false,
            drain: false,
            pending_drive: None,
            pending_drain: None,
            pending_fast: None,
            issue: None,
        }
    }
}

/// Per-team control state retained across Mosaic redraws.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct TeamControls {
    teams: BTreeMap<CanonicalUuid, TeamControl>,
    visibility: crate::watchband::visibility::TeamVisibility,
}

/// The selections one in-flight team configuration command carries, which
/// only that command's own answer settles.
#[derive(Clone, Copy)]
pub(crate) struct TeamRequest {
    pub(crate) team: CanonicalUuid,
    pub(crate) drive: Option<bool>,
    pub(crate) drain: Option<bool>,
    pub(crate) fast_mode: Option<bool>,
}

pub(crate) enum TeamControlAction {
    ToggleArrangement {
        team: CanonicalUuid,
        opener: ArrangementOpener,
    },
    HideTeam {
        team: CanonicalUuid,
    },
    ShowTeam {
        team: CanonicalUuid,
    },
    CloseArrangement,
    SetSpeech {
        team: CanonicalUuid,
        speech: SpeechMode,
    },
    RequestSwitches {
        team: CanonicalUuid,
        drive: Option<bool>,
        drain: Option<bool>,
    },
    RequestFast {
        team: CanonicalUuid,
        fast_mode: bool,
    },
    SyncLanes {
        answer: wire::intent::LaneAnswer,
        settle: Option<TeamRequest>,
    },
    SettleIssue {
        request: TeamRequest,
        issue: String,
    },
    /// The resident answered this request, but newer lane authority already
    /// arrived: settle the request to that authority, reporting the team issue
    /// only when that authority rolls the control back from what was asked.
    SettleSuperseded {
        request: TeamRequest,
        /// The team's held TURBO flag, absent when the team is no longer held.
        fast_mode: Option<bool>,
    },
}

impl Reducible for TeamControls {
    type Action = TeamControlAction;

    fn reduce(self: Rc<Self>, action: Self::Action) -> Rc<Self> {
        let mut next = (*self).clone();
        match action {
            TeamControlAction::ToggleArrangement { team, opener } => {
                next.toggle_arrangement(team, opener);
            }
            TeamControlAction::HideTeam { team } => next.set_hidden(team, true),
            TeamControlAction::ShowTeam { team } => next.set_hidden(team, false),
            TeamControlAction::CloseArrangement => next.close_arrangements(),
            TeamControlAction::SetSpeech { team, speech } => next.set_speech(team, speech),
            TeamControlAction::RequestSwitches { team, drive, drain } => {
                next.request_switches(team, drive, drain);
            }
            TeamControlAction::RequestFast { team, fast_mode } => {
                next.request_fast(team, fast_mode);
            }
            TeamControlAction::SyncLanes { answer, settle } => {
                next.sync_lanes(&answer, settle);
            }
            TeamControlAction::SettleIssue { request, issue } => {
                next.settle_issue(request, issue);
            }
            TeamControlAction::SettleSuperseded { request, fast_mode } => {
                next.settle_superseded(request, fast_mode);
            }
        }
        Rc::new(next)
    }
}

impl TeamControls {
    pub(in crate::browser) fn restore() -> Self {
        let mut controls = Self::default();
        let Some(storage) =
            web_sys::window().and_then(|window| window.local_storage().ok().flatten())
        else {
            return controls;
        };
        if let Some(document) = storage.get_item(TEAM_SPEECH_STORAGE).ok().flatten()
            && let Ok(rows) = serde_json::from_str::<Vec<(String, u8)>>(&document)
        {
            for (team, value) in rows {
                let (Ok(team), Some(speech)) = (team.parse(), SpeechMode::from_storage(value))
                else {
                    continue;
                };
                controls.teams.entry(team).or_default().speech = speech;
            }
        }
        if let Some(document) = storage.get_item(HIDDEN_TEAMS_STORAGE).ok().flatten()
            && let Ok(choices) = serde_json::from_str::<BTreeMap<CanonicalUuid, bool>>(&document)
        {
            controls.visibility = crate::watchband::visibility::TeamVisibility::restore(choices);
        }
        controls
    }

    pub(in crate::browser) fn is_hidden(&self, team: CanonicalUuid) -> bool {
        self.visibility.is_hidden(team)
    }

    fn set_hidden(&mut self, team: CanonicalUuid, hidden: bool) {
        self.visibility.set_hidden(team, hidden);
        self.teams.entry(team).or_default().arrangement = None;
        self.store_hidden();
    }

    fn store_hidden(&self) {
        let Some(storage) =
            web_sys::window().and_then(|window| window.local_storage().ok().flatten())
        else {
            return;
        };
        let teams = self.visibility.choices();
        if teams.is_empty() {
            let _ = storage.remove_item(HIDDEN_TEAMS_STORAGE);
        } else if let Ok(document) = serde_json::to_string(&teams) {
            let _ = storage.set_item(HIDDEN_TEAMS_STORAGE, &document);
        }
    }

    pub(super) fn current(&self, row: &crate::runtime::LaneRow) -> TeamControl {
        let mut control = self.teams.get(&row.lane).cloned().unwrap_or_default();
        control.drive = row.drive;
        control.drain = row.drain;
        if let Some(drive) = control.pending_drive {
            control.drive = drive;
            if !drive {
                control.drain = false;
            }
        }
        if let Some(drain) = control.pending_drain {
            control.drain = drain;
            if drain {
                control.drive = true;
            }
        }
        control
    }

    /// The open arrangement's team and the control that opened it.
    pub(in crate::browser) fn open_arrangement(
        &self,
    ) -> Option<(CanonicalUuid, ArrangementOpener)> {
        self.teams
            .iter()
            .find_map(|(team, state)| state.arrangement.map(|opener| (*team, opener)))
    }

    fn toggle_arrangement(&mut self, team: CanonicalUuid, opener: ArrangementOpener) {
        let open = !self
            .teams
            .get(&team)
            .is_some_and(|state| state.arrangement.is_some());
        self.close_arrangements();
        self.teams.entry(team).or_default().arrangement = open.then_some(opener);
    }

    fn close_arrangements(&mut self) {
        for state in self.teams.values_mut() {
            state.arrangement = None;
        }
    }

    fn set_speech(&mut self, team: CanonicalUuid, speech: SpeechMode) {
        self.teams.entry(team).or_default().speech = speech;
        self.store_speech();
    }

    fn store_speech(&self) {
        let Some(storage) =
            web_sys::window().and_then(|window| window.local_storage().ok().flatten())
        else {
            return;
        };
        let rows = self
            .teams
            .iter()
            .map(|(team, control)| (team.to_string(), control.speech.storage_value()))
            .collect::<Vec<_>>();
        if let Ok(document) = serde_json::to_string(&rows) {
            let _ = storage.set_item(TEAM_SPEECH_STORAGE, &document);
        }
    }

    /// Each team's last configuration refusal, cleared by its next request.
    pub(super) fn refusals(&self) -> impl Iterator<Item = (CanonicalUuid, &str)> {
        self.teams
            .iter()
            .filter_map(|(team, control)| Some((*team, control.issue.as_deref()?)))
    }

    fn request_switches(&mut self, team: CanonicalUuid, drive: Option<bool>, drain: Option<bool>) {
        let state = self.teams.entry(team).or_default();
        state.pending_drive = drive;
        state.pending_drain = drain;
        state.issue = None;
    }

    fn request_fast(&mut self, team: CanonicalUuid, fast_mode: bool) {
        let state = self.teams.entry(team).or_default();
        state.pending_fast = Some(fast_mode);
        state.issue = None;
    }

    /// Reconcile a lane table. Ordinary reads cannot overtake an in-flight
    /// selection; the command's own authoritative reply can.
    fn sync_lanes(&mut self, answer: &wire::intent::LaneAnswer, settle: Option<TeamRequest>) {
        self.close_arrangements();
        if self
            .visibility
            .sync(answer.lanes.iter().map(|lane| (lane.lane, lane.team)))
        {
            self.store_hidden();
        }
        // A team that left the authoritative subscription set is gone, not
        // hidden. Drop its browser-local controls.
        let live_teams = answer
            .subscriptions
            .iter()
            .map(|subscription| subscription.team)
            .collect::<BTreeSet<_>>();
        self.teams.retain(|team, _| live_teams.contains(team));

        for subscription in &answer.subscriptions {
            let team = subscription.team;
            let state = self.teams.entry(team).or_default();
            state.drive = subscription.drive;
            state.drain = subscription.drain;
            let request = settle.filter(|request| request.team == team);
            let settles_switches = request.is_some_and(|request| {
                (request.drive.is_some() || request.drain.is_some())
                    && state.pending_drive == request.drive
                    && state.pending_drain == request.drain
            });
            if settles_switches {
                state.pending_drive = None;
                state.pending_drain = None;
            }
            let settles_fast = request.is_some_and(|request| {
                request.fast_mode.is_some() && state.pending_fast == request.fast_mode
            });
            if settles_fast {
                state.pending_fast = None;
            }
            if settles_switches || settles_fast {
                state.issue = None;
            }
        }
    }

    fn settle_issue(&mut self, request: TeamRequest, issue: String) {
        self.settle_stale(request);
        self.teams.entry(request.team).or_default().issue = Some(issue);
    }

    /// Settle a request a newer read already answered. Convergence on what it
    /// asked for is silent; a still-shown selection that the held authority
    /// replaces is a visible rollback and reports the team issue.
    fn settle_superseded(&mut self, request: TeamRequest, fast_mode: Option<bool>) {
        let Some(state) = self.teams.get(&request.team) else {
            return;
        };
        let switches_rolled_back = state.pending_drive == request.drive
            && state.pending_drain == request.drain
            && (request.drive.is_some_and(|drive| drive != state.drive)
                || request.drain.is_some_and(|drain| drain != state.drain));
        let fast_rolled_back = request.fast_mode.is_some()
            && state.pending_fast == request.fast_mode
            && fast_mode.is_some_and(|held| request.fast_mode != Some(held));
        if switches_rolled_back || fast_rolled_back {
            self.settle_issue(request, ROLLED_BACK.to_owned());
        } else {
            self.settle_stale(request);
        }
    }

    /// Restore authority for the selections this request carried, unless a
    /// later request has already replaced them.
    fn settle_stale(&mut self, request: TeamRequest) {
        let Some(state) = self.teams.get_mut(&request.team) else {
            return;
        };
        if state.pending_drive == request.drive && state.pending_drain == request.drain {
            state.pending_drive = None;
            state.pending_drain = None;
        }
        if request.fast_mode.is_some() && state.pending_fast == request.fast_mode {
            state.pending_fast = None;
        }
    }
}

/// The team issue when newer resident state replaces a requested selection.
const ROLLED_BACK: &str = "Newer team state replaced this request; showing the resident's setting.";

/// Whether some slot can take an explicit send, independent of drive/drain.
pub(super) struct SendReadiness {
    pub(super) ready: bool,
    pub(super) dormant: bool,
    pub(super) slots: Vec<SendSlot>,
}

pub(super) fn render_team_controls(
    render: &super::fleet::FleetRender<'_, '_>,
    team_row: &crate::runtime::LaneRow,
    control: &TeamControl,
    readiness: SendReadiness,
) -> Html {
    let team = team_row.lane;
    let (topology_revision, can_configure) = (render.topology_revision, render.can_configure);
    let (team_controls, sender) = (render.team_controls, render.sender);
    let set_automatic_speech = &render.controls.set_automatic_speech;
    let SendReadiness {
        ready,
        dormant,
        slots,
    } = readiness;
    let speech = control.speech;
    let global_fast = render
        .runtime
        .settings()
        .is_some_and(|settings| settings.fast_mode);
    let turbo = turbo_button(
        team,
        topology_revision,
        control.pending_fast.unwrap_or(team_row.fast_mode),
        team_controls.clone(),
        sender.clone(),
        can_configure,
        global_fast,
    );
    let menu = {
        let controls = team_controls.clone();
        html! {
            <button
                class="team-menu"
                type="button"
                aria-label="Team menu"
                aria-haspopup="menu"
                aria-expanded={control.arrangement.is_some().to_string()}
                title="Team menu"
                onclick={Callback::from(move |event: MouseEvent| {
                    event.stop_propagation();
                    // Safari does not focus a clicked button; closing from here keeps focus here.
                    if let Some(button) = event
                        .current_target()
                        .and_then(|target| target.dyn_into::<web_sys::HtmlElement>().ok())
                    {
                        let _ = button.focus();
                    }
                    controls.dispatch(TeamControlAction::ToggleArrangement {
                        team,
                        opener: ArrangementOpener::Menu,
                    });
                })}
            >
                <span class="composer-menu-icon" aria-hidden="true">
                    <span></span><span></span><span></span>
                </span>
            </button>
        }
    };
    html! {
        <ControlsFrame
            {dormant}
            {ready}
            {topology_revision}
            {slots}
            verb="Send"
            noun="agent"
            speech={Some(speech.label())}
            drive={control.drive}
            drain={control.drain}
            policy_pending={control.pending_drive.is_some() || control.pending_drain.is_some()}
            {turbo}
            {menu}
        >
            {speech_slider(
                team,
                speech,
                team_controls.clone(),
                set_automatic_speech.clone(),
            )}
            {policy_switches(render, team_row, control)}
        </ControlsFrame>
    }
}

/// The team's fast-mode toggle retains TURBO lettering in both states.
/// Fast mode is additive: switching this flag off does not turn off a member
/// whose own flag or the global setting keeps it fast. Running OMP agents
/// whose effective value changes take it at once. A press shows its request
/// until the resident answers that command.
fn turbo_button(
    team: CanonicalUuid,
    topology_revision: u64,
    team_engaged: bool,
    controls: UseReducerHandle<TeamControls>,
    sender: SenderHandle,
    can_configure: bool,
    global_fast: bool,
) -> Html {
    let onclick = Callback::from(move |_: MouseEvent| {
        if admit_action(
            &sender,
            ClientAction::ConfigureTeam {
                topology_revision,
                team,
                member: None,
                new_team: false,
                drive: None,
                drain: None,
                fast_mode: Some(!team_engaged),
            },
        ) {
            controls.dispatch(TeamControlAction::RequestFast {
                team,
                fast_mode: !team_engaged,
            });
        }
    });
    let engaged = global_fast || team_engaged;
    let title = if global_fast {
        "Turbo: global fast mode runs every agent in this team in fast mode"
    } else if engaged {
        "Turbo: every agent in this team runs in fast mode"
    } else {
        "Turbo off: press to run every agent in this team in fast mode"
    };
    html! {
        <crate::browser::view::chrome::TurboButton
            class={classes!("team-turbo", global_fast.then_some("team-turbo--inherited"))}
            {engaged}
            disabled={!can_configure || global_fast}
            {title}
            {onclick}
        />
    }
}

/// Apply the team's speech mode in the input turn that selects it, so the
/// queue never narrates under a mode the control no longer shows.
fn speech_slider(
    team: CanonicalUuid,
    speech: SpeechMode,
    controls: UseReducerHandle<TeamControls>,
    set_automatic_speech: Callback<(String, SpeechMode)>,
) -> Html {
    let automatic_team = team.to_string();
    let oninput = Callback::from(move |event: InputEvent| {
        let input: HtmlInputElement = event.target_unchecked_into();
        let speech = SpeechMode::at(input.value().parse::<usize>().unwrap_or(1));
        set_automatic_speech.emit((automatic_team.clone(), speech));
        controls.dispatch(TeamControlAction::SetSpeech { team, speech });
    });
    html! {
        <label
            class="team-slider team-speech"
            data-stop={speech.index().to_string()}
        >
            <input
                type="range"
                min="0"
                max="2"
                step="1"
                value={speech.index().to_string()}
                aria-label={format!("Speech mode: {}", speech.label())}
                {oninput}
            />
            {slider_words(SpeechMode::ALL.map(SpeechMode::label), speech.index())}
        </label>
    }
}

fn policy_switches(
    render: &super::fleet::FleetRender<'_, '_>,
    row: &crate::runtime::LaneRow,
    control: &TeamControl,
) -> Html {
    let pending = control.pending_drive.is_some() || control.pending_drain.is_some();
    html! {
        <div class="team-policy-switches" role="group" aria-label="Team work switches">
            {for [
                ("drive", "Drive", control.drive, Some(!control.drive), None, control.drain,
                    "Drive: allow creating work under this team's configured rules",
                    "Turn Drain off before turning Drive off"),
                ("drain", "Drain", control.drain, None, Some(!control.drain), !control.drive,
                    "Drain: automatically take eligible work",
                    "Turn Drive on before turning Drain on"),
            ].into_iter().map(|(policy, label, on, drive, drain, locked, help, locked_help)| {
                let controls = render.team_controls.clone();
                let sender = render.sender.clone();
                let team = row.lane;
                let topology_revision = render.topology_revision;
                let onclick = Callback::from(move |_: MouseEvent| {
                    if admit_action(&sender, ClientAction::ConfigureTeam {
                        topology_revision,
                        team,
                        member: None,
                        new_team: false,
                        drive,
                        drain,
                        fast_mode: None,
                    }) {
                        controls.dispatch(TeamControlAction::RequestSwitches { team, drive, drain });
                    }
                });
                html! {
                    <button
                        type="button"
                        class="team-policy-switch"
                        role="switch"
                        aria-label={format!("Team {label}")}
                        data-team-policy={policy}
                        data-policy-locked={locked.to_string()}
                        aria-checked={on.to_string()}
                        title={if locked { locked_help } else { help }}
                        disabled={!render.can_configure || pending || locked}
                        {onclick}
                    >{crate::browser::view::chrome::fit_word(label)}</button>
                }
            })}
        </div>
    }
}

fn slider_words<const N: usize>(words: [&'static str; N], selected: usize) -> Html {
    let word = words[selected.min(words.len() - 1)];
    html! {
        <span class="team-slider-words" aria-hidden="true">
            <span>{crate::browser::view::chrome::fit_word(word)}</span>
        </span>
    }
}
