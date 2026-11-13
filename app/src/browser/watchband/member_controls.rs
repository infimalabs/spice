//! One seat's lane controls, drawn in its lane menu.
//!
//! Every control is drawn only when the resident offers its command to this
//! session and the seat's current state admits it, so a missing control means
//! the action is unavailable here rather than disabled for an unstated reason.
//! Ending an occupancy or removing the seat confirms first.

use super::*;
use crate::runtime::{MemberCommand, MemberControlState};

/// Issue one member command: reserve it on the member, then queue it.
pub(super) fn command_callback(
    sender: &SenderHandle,
    surface: &UseStateHandle<SurfaceData>,
    team: CanonicalUuid,
) -> Callback<(CanonicalUuid, MemberCommand)> {
    let sender = sender.clone();
    let surface = surface.clone();
    Callback::from(move |(lane, command): (CanonicalUuid, MemberCommand)| {
        issue(&sender, &surface, team, lane, command);
    })
}

fn issue(
    sender: &SenderHandle,
    surface: &UseStateHandle<SurfaceData>,
    team: CanonicalUuid,
    lane: CanonicalUuid,
    command: MemberCommand,
) {
    let reserved = {
        let mut data = surface.write();
        let Some(runtime) = data.as_mut() else {
            return;
        };
        let Some(topology_revision) = runtime.lanes().map(|lanes| lanes.topology_revision) else {
            return;
        };
        runtime
            .begin_member_command(lane, command)
            .map(|request| (request, topology_revision))
    };
    let Some((request, topology_revision)) = reserved else {
        return;
    };
    surface.set(surface.publication());
    let action = match request.command {
        MemberCommand::Wake | MemberCommand::Drain | MemberCommand::Remove => {
            ClientAction::LaneState {
                request: request.clone(),
            }
        }
        MemberCommand::Renew { requested } => ClientAction::RenewLane {
            topology_revision,
            team,
            lane,
            requested,
            member: Some(request.clone()),
        },
        MemberCommand::Retune { .. }
        | MemberCommand::RetuneEffort { .. }
        | MemberCommand::RetuneFast { .. } => ClientAction::RetuneLane {
            topology_revision,
            request: request.clone(),
        },
    };
    if !admit_action(sender, action) {
        let mut data = surface.write();
        if let Some(runtime) = data.as_mut() {
            runtime.settle_member_command(
                &request,
                Some(format!(
                    "{}: spice could not queue the command.",
                    request.command.failure()
                )),
            );
        }
        drop(data);
        surface.set(surface.publication());
    }
}

/// Which member controls the session's authority admits.
struct Offered {
    lifecycle: bool,
    renew: bool,
    model: bool,
    effort: bool,
    fast: bool,
}

impl Offered {
    fn of(runtime: &AppRuntime) -> Self {
        Self {
            lifecycle: runtime.offers(CommandKind::LaneState),
            renew: runtime.offers(CommandKind::LaneRenewal),
            model: runtime.offers(CommandKind::LaneRetune),
            effort: runtime.offers(CommandKind::LaneEffortRetune),
            fast: runtime.offers(CommandKind::LaneFastRetune),
        }
    }
}

fn send(
    command: &Callback<(CanonicalUuid, MemberCommand)>,
    lane: CanonicalUuid,
    next: MemberCommand,
) -> Callback<MouseEvent> {
    let command = command.clone();
    Callback::from(move |event: MouseEvent| {
        crate::browser::overlay::dismiss_disclosure_from_click(&event);
        command.emit((lane, next.clone()));
    })
}

/// One control that asks before it acts.
fn confirmed(
    key: &'static str,
    label: &str,
    question: String,
    action: String,
    busy: bool,
    onclick: Callback<MouseEvent>,
) -> Html {
    html! {
        <details class="member-confirm" data-app-overlay-disclosure="" data-member-control={key}>
            <summary class="agent-menu-action">{label.to_owned()}</summary>
            <div class="member-confirm-body">
                <p>{question}</p>
                <button type="button" class="agent-menu-action" disabled={busy} {onclick}>{action}</button>
            </div>
        </details>
    }
}

#[derive(Properties, PartialEq)]
struct ModelFormProps {
    lane: CanonicalUuid,
    /// The seat's authoritative model spelling; absent follows the vendor.
    model: Option<String>,
    placeholder: String,
    busy: bool,
    command: Callback<(CanonicalUuid, MemberCommand)>,
}

/// The next-launch model editor.
///
/// The typed spelling is component state, so a surface re-render cannot
/// reset it; only a change to the seat's authoritative model replaces it.
#[function_component(ModelForm)]
fn model_form(props: &ModelFormProps) -> Html {
    let draft = use_state(|| props.model.clone().unwrap_or_default());
    {
        let draft = draft.clone();
        use_effect_with(props.model.clone(), move |model| {
            draft.set(model.clone().unwrap_or_default());
        });
    }
    let lane = props.lane;
    let id = format!("member-model-{lane}");
    let oninput = {
        let draft = draft.clone();
        Callback::from(move |event: InputEvent| {
            let input: HtmlInputElement = event.target_unchecked_into();
            draft.set(input.value());
        })
    };
    let submit = {
        let command = props.command.clone();
        let draft = draft.clone();
        Callback::from(move |event: SubmitEvent| {
            // The lane menu sits inside the team composer form, whose own
            // submit sends a message; the model form must not reach it.
            event.prevent_default();
            event.stop_propagation();
            let model = Some(draft.trim().to_owned()).filter(|model| !model.is_empty());
            command.emit((lane, MemberCommand::Retune { model }));
        })
    };
    let reset = props.model.is_some().then(|| {
        let command = props.command.clone();
        Callback::from(move |_: MouseEvent| {
            command.emit((lane, MemberCommand::Retune { model: None }));
        })
    });
    let busy = props.busy;
    html! {
        <form class="member-model" data-member-control="model" onsubmit={submit}>
            <label for={id.clone()}>{"Next launch model"}</label>
            <div class="app-control-rail">
                <input
                    {id}
                    type="text"
                    spellcheck="false"
                    autocomplete="off"
                    value={(*draft).clone()}
                    placeholder={props.placeholder.clone()}
                    disabled={busy}
                    {oninput}
                />
                <button type="submit" class="agent-menu-action" disabled={busy}>{"Set model"}</button>
                if let Some(reset) = reset {
                    <button type="button" class="agent-menu-action" disabled={busy} onclick={reset}>
                        {"Use default"}
                    </button>
                }
            </div>
        </form>
    }
}
#[derive(Properties, PartialEq)]
struct EffortFormProps {
    lane: CanonicalUuid,
    /// Current running effort, shown separately from launch authority.
    current: Option<String>,
    /// The seat's authoritative next-launch effort.
    effort: Option<String>,
    default_effort: String,
    choices: Vec<String>,
    busy: bool,
    command: Callback<(CanonicalUuid, MemberCommand)>,
}

#[function_component(EffortForm)]
fn effort_form(props: &EffortFormProps) -> Html {
    let draft = use_state(|| props.effort.clone().unwrap_or_default());
    {
        let draft = draft.clone();
        use_effect_with(props.effort.clone(), move |effort| {
            draft.set(effort.clone().unwrap_or_default());
        });
    }
    let onchange = {
        let draft = draft.clone();
        Callback::from(move |event: Event| {
            let select: HtmlSelectElement = event.target_unchecked_into();
            draft.set(select.value());
        })
    };
    let lane = props.lane;
    let submit = {
        let command = props.command.clone();
        let draft = draft.clone();
        Callback::from(move |event: SubmitEvent| {
            event.prevent_default();
            event.stop_propagation();
            let effort = Some((*draft).clone()).filter(|effort| !effort.is_empty());
            command.emit((lane, MemberCommand::RetuneEffort { effort }));
        })
    };
    let running = props.current.as_deref().unwrap_or("not running");
    let next = props.effort.as_deref().map_or_else(
        || format!("default ({})", props.default_effort),
        str::to_owned,
    );
    html! {
        <form class="member-effort" data-member-control="effort" onsubmit={submit}>
            <label for={format!("member-effort-{lane}")}>{"Reasoning effort"}</label>
            <p class="member-setting-status">{format!("Running: {running}; next launch: {next}")}</p>
            <div class="app-control-rail">
                // A select's `value` lands before its options exist on the
                // first render, so each option also states its selectedness.
                <select
                    id={format!("member-effort-{lane}")}
                    value={(*draft).clone()}
                    disabled={props.busy}
                    {onchange}
                >
                    <option value="" selected={draft.is_empty()}>{format!("default ({})", props.default_effort)}</option>
                    {for props.choices.iter().map(|effort| html! {
                        <option value={effort.clone()} selected={*draft == *effort}>{effort}</option>
                    })}
                </select>
                <button type="submit" class="agent-menu-action" disabled={props.busy}>{"Set effort"}</button>
            </div>
        </form>
    }
}

const fn on_off(enabled: bool) -> &'static str {
    if enabled { "on" } else { "off" }
}

/// What one member's next launch runs, naming the level that turns TURBO on
/// when it is not the member's own flag.
fn next_fast_label(own: bool, team: bool, global: Option<bool>) -> String {
    match (own, team, global) {
        (true, _, _) => "on".to_owned(),
        (false, true, _) => "on (team)".to_owned(),
        (false, false, Some(true)) => "on (global)".to_owned(),
        (false, false, Some(false)) => "off".to_owned(),
        (false, false, None) => "loading".to_owned(),
    }
}

/// The per-member TURBO control. It shows and flips this member's own flag
/// until a team or global parent forces the effective value on. A running OMP
/// agent whose effective value changes takes it at once, and the status line
/// keeps the running value separate in case it did not.
fn render_fast_toggle(
    runtime: &AppRuntime,
    member: &crate::runtime::LaneMember,
    busy: bool,
    command: &Callback<(CanonicalUuid, MemberCommand)>,
) -> Html {
    let global = runtime.settings().map(|settings| settings.fast_mode);
    let team = runtime
        .lanes()
        .and_then(|view| {
            view.lanes
                .iter()
                .find(|team| team.member(member.lane).is_some())
        })
        .is_some_and(|team| team.fast_mode);
    let parent = team || global == Some(true);
    let lane = member.lane;
    let own = member.seat_fast_mode;
    let onclick = {
        let command = command.clone();
        Callback::from(move |event: MouseEvent| {
            event.stop_propagation();
            command.emit((lane, MemberCommand::RetuneFast { fast_mode: !own }));
        })
    };
    let running = member
        .occupant
        .map_or("not running", |_| on_off(member.fast_mode));
    let next = next_fast_label(own, team, global);
    let title = if global == Some(true) {
        "TURBO: global fast mode runs this agent in fast mode"
    } else if team {
        "TURBO: team fast mode runs this agent in fast mode"
    } else {
        "TURBO: toggle fast mode for this agent"
    };
    html! {
        <div class="member-fast" data-member-control="fast">
            <crate::browser::view::chrome::TurboButton
                class={classes!("member-turbo", parent.then_some("member-turbo--inherited"))}
                engaged={parent || own}
                disabled={busy || parent}
                label="Agent fast mode"
                {title}
                {onclick}
            />
            <p class="member-setting-status">{format!("Running: {running}; next launch: {next}")}</p>
        </div>
    }
}

fn render_retune(
    runtime: &AppRuntime,
    member: &crate::runtime::LaneMember,
    offered: &Offered,
    busy: bool,
    command: &Callback<(CanonicalUuid, MemberCommand)>,
) -> Html {
    let vendor = runtime.vendor_catalog().and_then(|catalog| {
        catalog
            .vendors
            .iter()
            .find(|vendor| vendor.name == member.vendor)
            .map(|vendor| (vendor.default_model.clone(), vendor.default_effort.clone()))
    });
    let (default_model, default_effort) =
        vendor.unwrap_or_else(|| ("vendor default".to_owned(), "vendor default".to_owned()));
    let choices = runtime
        .vendor_catalog()
        .map(|catalog| catalog.efforts.clone())
        .unwrap_or_default();
    let running_model = member.model.as_deref().unwrap_or("not running");
    let next_model = member
        .seat_model
        .as_deref()
        .map_or_else(|| format!("default ({default_model})"), str::to_owned);
    html! {
        <>
            if offered.model {
                <div class="member-model-control">
                    <p class="member-setting-status">
                        {format!("Running model: {running_model}; next launch: {next_model}")}
                    </p>
                    <ModelForm
                        key={format!("model-{}", member.lane)}
                        lane={member.lane}
                        model={member.seat_model.clone()}
                        placeholder={format!("{default_model} (default)")}
                        {busy}
                        command={command.clone()}
                    />
                </div>
            }
            if offered.effort {
                <EffortForm
                    key={format!("effort-{}", member.lane)}
                    lane={member.lane}
                    current={member.effort.clone()}
                    effort={member.seat_effort.clone()}
                    {default_effort}
                    {choices}
                    {busy}
                    command={command.clone()}
                />
            }
            if offered.fast {
                {render_fast_toggle(runtime, member, busy, command)}
            }
        </>
    }
}

/// The renewal action, and the refused launch's reason when one is pending.
fn render_renewal(
    runtime: &AppRuntime,
    member: &crate::runtime::LaneMember,
    standing: &Standing,
    busy: bool,
    command: &Callback<(CanonicalUuid, MemberCommand)>,
) -> (Html, Html) {
    let renewal = watchband::agent_renewal_control(
        &standing.label,
        runtime
            .lanes()
            .and_then(|lanes| lanes.renewal(standing.team, member.lane)),
    );
    // A refused launch stays retryable, and its exact reason stays in view.
    let refusal = renewal.refusal.clone().map_or_else(Html::default, |reason| {
        html! {
            <p class="member-refusal" role="status">{format!("Renewal launch refused: {reason}")}</p>
        }
    });
    let next = MemberCommand::Renew {
        requested: renewal.next_requested,
    };
    let action = html! {
        <button
            type="button"
            class="agent-menu-action"
            data-member-control="renew"
            aria-label={renewal.label}
            title={renewal.title}
            disabled={busy}
            onclick={send(command, member.lane, next)}
        >{renewal.heading}</button>
    };
    (action, refusal)
}

fn render_lifecycle(
    member: &crate::runtime::LaneMember,
    standing: &Standing,
    busy: bool,
    command: &Callback<(CanonicalUuid, MemberCommand)>,
) -> Html {
    let lane = member.lane;
    let label = &standing.label;
    html! {
        <>
            if member.occupant.is_none() && member.wakeable {
                <button
                    type="button"
                    class="agent-menu-action"
                    data-member-control="wake"
                    disabled={busy}
                    onclick={send(command, lane, MemberCommand::Wake)}
                >{"Wake"}</button>
            }
            if member.occupant.is_some() {
                {confirmed(
                    "drain",
                    "Drain",
                    format!("End {label}'s occupancy? The seat stays, vacant, until it is woken."),
                    format!("Drain {label}"),
                    busy,
                    send(command, lane, MemberCommand::Drain),
                )}
            }
            if member.occupant.is_none() && standing.handle.is_none() {
                {confirmed(
                    "remove",
                    "Remove",
                    format!("Remove {label}'s seat from this team?"),
                    format!("Remove {label}"),
                    busy,
                    send(command, lane, MemberCommand::Remove),
                )}
            }
        </>
    }
}

/// The member's controls, held while a command is in flight. The command's
/// progress and outcome report through the root toast.
pub(super) fn render(
    runtime: &AppRuntime,
    member: &crate::runtime::LaneMember,
    standing: &Standing,
    command: &Callback<(CanonicalUuid, MemberCommand)>,
) -> Html {
    let offered = Offered::of(runtime);
    if !(offered.lifecycle || offered.renew || offered.model || offered.effort) {
        return Html::default();
    }
    let busy = runtime
        .member_control(member.lane)
        .and_then(MemberControlState::pending)
        .is_some();
    let (renewal, refusal) = if offered.renew {
        render_renewal(runtime, member, standing, busy, command)
    } else {
        (Html::default(), Html::default())
    };
    html! {
        <section
            class="member-controls"
            aria-label={format!("Controls for {}", standing.label)}
            aria-busy={busy.to_string()}
        >
            <div class="app-control-rail">
                if offered.lifecycle {
                    {render_lifecycle(member, standing, busy, command)}
                }
                {renewal}
            </div>
            {refusal}
            if offered.model || offered.effort {
                {render_retune(runtime, member, &offered, busy, command)}
            }
        </section>
    }
}
