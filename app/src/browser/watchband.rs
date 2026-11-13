//! Lane composer rendered inside the Lanes surface.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{DragEvent, HtmlInputElement, HtmlSelectElement, PointerEvent};

use super::clock::{Age, Countdown};
use super::*;
use crate::composer_drafts::ComposerDraft;
use crate::watchband::{self, Standing, SubmissionAuthority, SubmissionTarget};

mod agent_menu;
mod arrangement;
mod attention;
mod command_preview;
mod composer;
mod controls;
mod fanout;
mod feedback;
mod fleet;
mod member_controls;
mod mosaic_view;
mod provider_emblem;
mod reading_focus;
mod state;
mod team_drag;
mod trace;

pub(super) use command_preview::use_command_modifier_preview;
use composer::{AgentMessageCard, drop_on_lane, keyboard_submit, primary_composer_dragging};
pub(in crate::browser) use composer::{
    ComposerTextarea, drop_images, focus_after_render, paste_images, quote_textarea_id,
    render_image_indicators, render_quote_shards, team_quote_sources,
};
pub(super) use composer::{
    PRIMARY_TEXTAREA_PREFIX, PrimaryComposerDragState, agent_message_source_id, lane_composer_href,
    primary_textarea_id, use_primary_composer_drag_state,
};
use controls::ArrangementOpener;
pub(in crate::browser) use controls::SendSlot;
use controls::render_team_controls;
pub(super) use controls::{TeamControlAction, TeamControls, TeamRequest};
pub(in crate::browser) use fanout::{Composition, submit_broadcast};
pub(in crate::browser) use fleet::use_team_arrangement_focus;
use fleet::{FleetRender, render_empty_fleet, render_populated_fleet};
use mosaic_view::CardLayout;
pub(crate) use mosaic_view::use_markdown_image_resolution_events;
pub(in crate::browser) use mosaic_view::{
    ActivityCard, HistorySender, LIFECYCLE_CARD_PREFIX, MosaicPlane, PlaneOwner,
};
pub(super) use provider_emblem::render_provider_emblem;
pub(super) use reading_focus::use_composer_reading_focus;
pub(in crate::browser) use state::{ComposerAction, ComposerDrafts, use_composer_issue_toast};
use team_drag::team_dragging;
pub(super) use team_drag::{TeamDragState, use_team_drag_state};

const MAX_ATTACHMENTS: usize = wire::media::MAX_MEDIA_ITEMS;
const MAX_ATTACHMENT_BYTES: f64 = wire::media::MAX_MEDIA_BYTES as f64;

#[derive(Clone, Copy)]
pub(super) struct BroadcastTarget {
    lane: CanonicalUuid,
    /// None steers a vacant, wakeable seat.
    occupant: Option<CanonicalUuid>,
    epoch: CanonicalUuid,
}

impl BroadcastTarget {
    /// Where one chosen agent's branch goes, as the submission authority
    /// resolved it.
    pub(super) fn chosen(lane: CanonicalUuid, target: SubmissionTarget) -> Self {
        match target {
            SubmissionTarget::Resident { occupant, epoch } => Self {
                lane,
                occupant: Some(occupant),
                epoch,
            },
            SubmissionTarget::Vacant { epoch } => Self {
                lane,
                occupant: None,
                epoch,
            },
        }
    }
}

/// The observer that toasts lane and team request progress and outcomes. It mounts
/// once at the app root so a request settles in the toast whichever
/// surface is showing.
pub(super) fn render_feedback(runtime: Option<&AppRuntime>, controls: &TeamControls) -> Html {
    html! { <feedback::RequestFeedback notices={feedback::Notices::of(runtime, controls)} /> }
}

/// Render every team through the same browser-local hide/show controls.
/// Own singleton teams start hidden but retain a revealable composer and stream.
pub(super) fn render_team(
    render: &view::AppRender<'_>,
    work: Option<&crate::work::WorkProjection<'_>>,
) -> Html {
    let view::AppRender {
        connection,
        surface,
        sender,
        composer_drafts: drafts,
        submissions,
        team_fleet,
        team_controls,
        composer_drag,
        composer_slots,
        team_drag,
        select_task,
        open_message_task,
        navigate,
        agents: controls,
        ..
    } = render;
    let surface_state = (*surface).clone();
    let surface = surface.read();
    let Some(runtime) = surface.as_ref() else {
        return Html::default();
    };
    let Some(lanes) = runtime.lanes() else {
        return Html::default();
    };
    let waiting = runtime
        .inbox()
        .map(|inbox| inbox.lanes.as_slice())
        .unwrap_or(&[]);
    let tasks = runtime
        .board()
        .map(|board| board.tasks.as_slice())
        .unwrap_or(&[]);
    let mut standings = watchband::standings(&lanes.lanes, waiting, tasks);
    for standing in &mut standings {
        standing.pending = view::member_pending(waiting, &submissions.status, standing.lane);
        standing.waking = view::member_waking(waiting, &submissions.status, standing.lane);
    }
    let authority =
        SubmissionAuthority::derive(connection.is_online(), |kind| runtime.offers(kind));
    let can_configure = runtime.offers(CommandKind::TeamConfigure);
    let can_arrange_teams = runtime.offers(CommandKind::TeamArrange);
    let topology = Rc::new(watchband::topology(&lanes.lanes));
    let quote_agents = quote_agents(runtime, &lanes.lanes);
    let mut teams = lanes.lanes.iter().collect::<Vec<_>>();
    composer_slots.retain(|lane| teams.iter().any(|team| team.lane == *lane));
    let has_teams = !teams.is_empty();
    teams.retain(|team| !team_controls.is_hidden(team.lane));
    if teams.is_empty() {
        let message = if has_teams {
            "All teams are hidden. Open an agent lane to show its team."
        } else {
            "No teams are assigned."
        };
        return render_empty_fleet(team_fleet, message);
    }
    let render = FleetRender {
        work,
        navigate,
        select_task,
        open_message_task,
        runtime,
        epoch: lanes.epoch,
        topology_revision: lanes.topology_revision,
        authority,
        can_arrange_teams,
        can_configure,
        topology,
        quote_agents,
        surface: surface_state,
        sender,
        drafts,
        submissions,
        team_controls,
        controls,
        composer_drag,
        composer_slots,
        team_drag,
    };
    render_populated_fleet(&teams, &standings, team_fleet, &render)
}

pub(in crate::browser) fn quote_agents(
    runtime: &AppRuntime,
    teams: &[crate::runtime::LaneRow],
) -> Rc<crate::composer_markdown::QuoteAgents> {
    Rc::new(crate::composer_markdown::QuoteAgents {
        members: teams
            .iter()
            .flat_map(|team| {
                team.members
                    .iter()
                    .enumerate()
                    .map(move |(position, member)| {
                        (
                            (member.lane, member.occupant),
                            crate::composer_markdown::QuoteAgent {
                                accent: team.accent(position),
                                label: watchband::agent_message_label(
                                    &member.worktree,
                                    member.name.as_deref(),
                                )
                                .to_owned(),
                            },
                        )
                    })
            })
            .collect(),
        principals: runtime.principals().cloned().unwrap_or_default(),
    })
}

/// Each team's composer host in its Chat plane's scroll content, where the
/// browser's own scroll moves the composer with the cards on every engine.
/// Yew portals the composer into the host and the plane renders the host as
/// its own node, so a draft keystroke never re-renders the plane and the plane
/// never re-renders the composer.
#[derive(Clone)]
pub(in crate::browser) struct ComposerSlots<K = CanonicalUuid>(
    pub(in crate::browser) Rc<RefCell<HashMap<K, web_sys::HtmlElement>>>,
);

#[hook]
pub(super) fn use_composer_slots() -> ComposerSlots {
    ComposerSlots(use_mut_ref(HashMap::new))
}

impl<K: Eq + std::hash::Hash> ComposerSlots<K> {
    pub(in crate::browser) fn slot(&self, owner: K) -> web_sys::HtmlElement {
        self.0
            .borrow_mut()
            .entry(owner)
            .or_insert_with(|| {
                let slot = web_sys::window()
                    .and_then(|window| window.document())
                    .and_then(|document| document.create_element("div").ok())
                    .and_then(|slot| slot.dyn_into::<web_sys::HtmlElement>().ok())
                    .expect("the app document creates a composer slot");
                slot.set_class_name("team-composer-slot");
                // The plane's activity region is live; typing is not news.
                slot.set_attribute("aria-live", "off")
                    .expect("the composer slot accepts its live-region opt-out");
                slot
            })
            .clone()
    }

    pub(in crate::browser) fn retain(&self, live: impl Fn(&K) -> bool) {
        self.0.borrow_mut().retain(|owner, _| live(owner));
    }
}

/// One team's composer and activity plane, portalled into `composer_slot`.
fn render_compose(
    render: &fleet::FleetRender<'_, '_>,
    team: &crate::runtime::LaneRow,
    standings: &[Standing],
    attention: &attention::TeamAttention,
    composer_slot: web_sys::HtmlElement,
) -> Html {
    if team.members.is_empty() {
        return empty("No agents are assigned.");
    }
    let runtime = render.runtime;
    let (sender, drafts, team_controls) = (render.sender, render.drafts, render.team_controls);
    let (epoch, topology_revision, authority) =
        (render.epoch, render.topology_revision, render.authority);
    let plane = team_controls.current(team);
    let submit = submit_team_callback(
        &team.members,
        epoch,
        authority,
        sender.clone(),
        drafts.clone(),
        render.submissions.clone(),
    );
    let broadcast = broadcast_targets(&team.members, epoch, authority.send);
    let broadcast_help = format!("composer-broadcast-help-{}", team.lane);
    let dormant = team_dormant(team, drafts);
    let slots = send_slots(
        team,
        standings,
        drafts,
        &render.submissions.status,
        authority,
        epoch,
        &broadcast,
    );
    let ready = slots.iter().any(|slot| slot.reachable);
    let cards = activity_cards(runtime, team, &render.quote_agents);
    let member_command = member_controls::command_callback(sender, &render.surface, team.lane);
    let composer_team = TeamComposer {
        epoch,
        topology_revision,
        topology: &render.topology,
        authority,
        can_arrange: render.can_arrange_teams,
        sender,
        composer_drag: render.composer_drag,
        drafts,
        submissions: render.submissions,
        broadcast_targets: &broadcast,
        broadcast_help: &broadcast_help,
    };
    let composer = html! {
        <form class="team-composer" onsubmit={submit.clone()}>
            <div class="team-agents">
                {for team.members.iter().zip(standings.iter()).map(|(member, standing)| {
                    render_agent_composer(
                        &composer_team,
                        member,
                        standing,
                        agent_menu::render(agent_menu::AgentMenu {
                            runtime,
                            member,
                            standing,
                            select: render.select_task,
                            command: &member_command,
                            can_configure: render.can_configure,
                            topology_revision,
                            topology: &render.topology,
                            sender,
                            team_controls: render.team_controls,
                            attention: attention.lane(member.lane),
                        }),
                        trace::render_composer_rail(runtime, member),
                        primary_composer_header(runtime, team, member.lane, standing),
                    )
                })}
            </div>
            {render_team_controls(
                render,
                team,
                &plane,
                controls::SendReadiness {
                    ready,
                    dormant,
                    slots,
                },
            )}
            <span id={broadcast_help} class="visually-hidden">
                {"Control or Command plus Shift and Enter broadcasts this primary message and its direct images to every occupied agent in this team."}
            </span>
        </form>
    };
    html! {
        <div class="team-compose" onclick={Callback::from(agent_menu::dismiss_others)}>
            // The composer renders into its slot in the plane's scroll content.
            {yew::create_portal(composer, composer_slot.clone().into())}
            {render_activity(
                render,
                team,
                &cards,
                &plane,
                HistoryPaging::of(runtime, team, sender),
                composer_slot,
            )}
        </div>
    }
}

/// With no agent running and no quote or image drafted, a team's controls step
/// back unless a composer holds typed text or a wake is in flight, both of
/// which only the controls follow.
fn team_dormant(team: &crate::runtime::LaneRow, drafts: &ComposerDrafts) -> bool {
    let members = || team.members.iter();
    !members().any(|member| member.liveness != wire::intent::LaneLiveness::Vacant)
        && !members().any(|member| drafts.holds_quotes_or_images(&member.lane.to_string()))
}

fn history_target(
    runtime: &AppRuntime,
    lane: CanonicalUuid,
) -> Option<(CanonicalUuid, (wire::MessageHistoryCursor, u64))> {
    runtime
        .message_history_target(lane)
        .map(|cursor| (lane, (cursor, runtime.message_history_completion(lane))))
}

fn team_history_targets(
    runtime: &AppRuntime,
    team: &crate::runtime::LaneRow,
) -> BTreeMap<CanonicalUuid, (wire::MessageHistoryCursor, u64)> {
    team.members
        .iter()
        .filter_map(|member| history_target(runtime, member.lane))
        .collect()
}

fn broadcast_targets(
    members: &[crate::runtime::LaneMember],
    epoch: CanonicalUuid,
    can_send: bool,
) -> Vec<BroadcastTarget> {
    if !can_send {
        return Vec::new();
    }
    members
        .iter()
        .filter_map(|member| {
            member.occupant.map(|occupant| BroadcastTarget {
                lane: member.lane,
                occupant: Some(occupant),
                epoch,
            })
        })
        .collect()
}

/// Every occupied agent in the fleet, each once, for a broadcast from the
/// creator; none while sending is unavailable.
pub(in crate::browser) fn fleet_broadcast_targets(
    runtime: &AppRuntime,
    online: bool,
) -> Vec<BroadcastTarget> {
    let Some(lanes) = runtime.lanes() else {
        return Vec::new();
    };
    let authority = SubmissionAuthority::derive(online, |kind| runtime.offers(kind));
    let mut seen = BTreeSet::new();
    lanes
        .lanes
        .iter()
        .flat_map(|team| broadcast_targets(&team.members, lanes.epoch, authority.send))
        .filter(|target| seen.insert(target.lane))
        .collect()
}

/// The draft key a submission source's composer content lives under.
pub(in crate::browser) fn source_key(source: ComposerSource) -> String {
    match source {
        ComposerSource::Lane(lane) => lane.to_string(),
        ComposerSource::Creator => crate::composer_drafts::NEW_DRAFT_KEY.to_owned(),
    }
}

/// Shared authenticated quote context for every card from one team. A card
/// carries only the agents, sources, and tasks its own quotes name.
struct AgentCardResources<'a> {
    team: &'a crate::runtime::LaneRow,
    quote_agents: &'a Rc<crate::composer_markdown::QuoteAgents>,
    quote_sources: Rc<crate::composer_markdown::QuoteSources>,
    quote_tasks: Rc<crate::runtime::QuoteTasks>,
    unquoted_agents: Rc<crate::composer_markdown::QuoteAgents>,
    unquoted_sources: Rc<crate::composer_markdown::QuoteSources>,
    unquoted_tasks: Rc<crate::runtime::QuoteTasks>,
}

impl<'a> AgentCardResources<'a> {
    fn new(
        runtime: &AppRuntime,
        team: &'a crate::runtime::LaneRow,
        quote_agents: &'a Rc<crate::composer_markdown::QuoteAgents>,
        directions: &[wire::AgentMessage],
    ) -> Self {
        Self {
            team,
            quote_agents,
            quote_sources: Rc::new(team_quote_sources(runtime, team, directions)),
            quote_tasks: runtime
                .board()
                .map(|board| Rc::clone(&board.quote_tasks))
                .unwrap_or_default(),
            unquoted_agents: Rc::new(crate::composer_markdown::QuoteAgents::new()),
            unquoted_sources: Rc::new(crate::composer_markdown::QuoteSources::new()),
            unquoted_tasks: Rc::new(crate::runtime::QuoteTasks::new()),
        }
    }

    fn card(
        &self,
        position: usize,
        member: &crate::runtime::LaneMember,
        message: Rc<wire::AgentMessage>,
    ) -> ActivityCard {
        let has_attributed_context =
            !message.quotes.is_empty() || !message.directive_contexts.is_empty();
        ActivityCard::Agent {
            accent: self.team.accent(position),
            member: member.clone(),
            quote_agents: if has_attributed_context || !message.sends.is_empty() {
                Rc::new(crate::composer_markdown::referenced_quote_agents(
                    &message.quotes,
                    &message.directive_contexts,
                    &message.sends,
                    self.quote_agents,
                ))
            } else {
                Rc::clone(&self.unquoted_agents)
            },
            quote_sources: if has_attributed_context {
                Rc::new(crate::composer_markdown::referenced_quote_sources(
                    &message.quotes,
                    &message.directive_contexts,
                    &self.quote_sources,
                ))
            } else {
                Rc::clone(&self.unquoted_sources)
            },
            quote_tasks: if has_attributed_context {
                Rc::new(crate::composer_markdown::referenced_quote_tasks(
                    &message.quotes,
                    &message.directive_contexts,
                    &self.quote_tasks,
                ))
            } else {
                Rc::clone(&self.unquoted_tasks)
            },
            quote_lane: crate::composer_drafts::quote_lane(&self.team.members, &message),
            message,
            companions: Rc::from([]),
        }
    }
}

/// Every stable assistant-authored record the team can render, and one card
/// per operator send its members received, newest first. Display chronology
/// distinguishes edge additions from structural middle insertions.
fn activity_cards(
    runtime: &AppRuntime,
    team: &crate::runtime::LaneRow,
    quote_agents: &Rc<crate::composer_markdown::QuoteAgents>,
) -> Vec<ActivityCard> {
    let lanes = team
        .members
        .iter()
        .map(|member| member.lane)
        .collect::<Vec<_>>();
    let provisional = runtime.provisional_directions(&team.members);
    let mut directions = crate::watchband::direction_cards(
        runtime
            .messages()
            .iter()
            .map(Rc::as_ref)
            .chain(provisional.iter()),
        &lanes,
    );
    let resources = AgentCardResources::new(runtime, team, quote_agents, &directions);
    let member_of = |lane: CanonicalUuid| {
        team.members
            .iter()
            .enumerate()
            .find(|(_, member)| member.lane == lane)
    };
    let mut cards = runtime
        .messages()
        .iter()
        .filter(|message| message.activity != wire::AgentActivityKind::Direction)
        .filter_map(|message| {
            let (position, member) = member_of(message.lane)?;
            Some(resources.card(position, member, Rc::clone(message)))
        })
        .collect::<Vec<_>>();
    cards.extend(directions.drain(..).filter_map(|message| {
        let (position, member) = member_of(message.lane)?;
        Some(resources.card(position, member, Rc::new(message)))
    }));
    cards.extend(
        runtime
            .lifecycle_boundaries()
            .iter()
            .filter_map(|boundary| {
                let (position, member) = team
                    .members
                    .iter()
                    .enumerate()
                    .find(|(_, member)| member.lane == boundary.lane())?;
                Some(ActivityCard::Lifecycle {
                    accent: team.accent(position),
                    member: member.clone(),
                    boundary: boundary.clone(),
                })
            }),
    );
    cards.sort_by_key(|card| Reverse(card.chronology_key()));
    cards
}

/// Build only the exact team-plane message cards named by loaded inbox
/// publications; ordinary app renders do not duplicate the full activity list.
pub(in crate::browser) fn inbox_activity_cards(
    runtime: &AppRuntime,
    sources: &BTreeSet<String>,
) -> BTreeMap<String, (CanonicalUuid, ActivityCard)> {
    let Some(lanes) = runtime.lanes() else {
        return BTreeMap::new();
    };
    let quote_agents = quote_agents(runtime, &lanes.lanes);
    let membership = lanes
        .lanes
        .iter()
        .flat_map(|team| {
            team.members
                .iter()
                .enumerate()
                .map(move |(position, member)| (member.lane, (team.lane, position)))
        })
        .collect::<HashMap<_, _>>();
    let mut selected = HashMap::<_, Vec<_>>::new();
    for message in runtime.messages() {
        let source = agent_message_source_id(message);
        if !sources.contains(&source) {
            continue;
        }
        let Some((team, position)) = membership.get(&message.lane) else {
            continue;
        };
        selected
            .entry(*team)
            .or_default()
            .push((source, *position, Rc::clone(message)));
    }
    let mut cards = BTreeMap::new();
    for team in &lanes.lanes {
        let Some(selected) = selected.remove(&team.lane) else {
            continue;
        };
        let team_lanes = team
            .members
            .iter()
            .map(|member| member.lane)
            .collect::<Vec<_>>();
        let directions = crate::watchband::direction_cards(
            runtime.messages().iter().map(Rc::as_ref),
            &team_lanes,
        );
        let resources = AgentCardResources::new(runtime, team, &quote_agents, &directions);
        cards.extend(selected.into_iter().map(|(source, position, message)| {
            (
                source,
                (
                    team.lane,
                    resources.card(position, &team.members[position], message),
                ),
            )
        }));
    }
    cards
}

/// Render one loaded transcript message through the same component as its
/// team-plane card, without duplicating the outer card identity owned there.
pub(in crate::browser) fn render_inbox_agent_card(
    team_lane: CanonicalUuid,
    card: ActivityCard,
    drafts: &UseReducerHandle<ComposerDrafts>,
    open_task: &Callback<(wire::TaskStamp, web_sys::Element)>,
    controls: &view::AgentControls,
) -> Option<Html> {
    let ActivityCard::Agent {
        accent,
        member,
        quote_agents,
        quote_sources,
        quote_tasks,
        quote_lane,
        message,
        companions,
    } = card
    else {
        return None;
    };
    let message_key = agent_message_source_id(&message);
    let speech_owner = controls
        .speech_owner
        .as_ref()
        .filter(|owner| owner.message_key() == message_key)
        .cloned();
    let order = message.locus.offset;
    Some(html! {
        <AgentMessageCard
            {accent}
            {member}
            {quote_agents}
            {quote_sources}
            {quote_tasks}
            {quote_lane}
            {message}
            quote_dispatcher={drafts.dispatcher()}
            open_task={open_task.clone()}
            speech_queue={controls.speak.clone()}
            {speech_owner}
            lane={team_lane}
            {order}
            layout={Option::<mosaic_view::CardLayout>::None}
            {companions}
        />
    })
}

fn render_activity(
    render: &fleet::FleetRender<'_, '_>,
    team: &crate::runtime::LaneRow,
    cards: &[ActivityCard],
    plane: &controls::TeamControl,
    history: HistoryPaging<'_>,
    composer_slot: web_sys::HtmlElement,
) -> Html {
    let (drafts, controls) = (render.drafts, render.controls);
    let empty = cards.is_empty();
    let narrate = controls.narrate.clone();
    let history_sender = HistorySender::new(history.sender.clone());
    html! {
        <div
            id={crate::view::team_stream_card_target(team.lane)}
            tabindex="-1"
            class="team-activity-grid"
            aria-live="polite"
            aria-label="Live agent activity"
            data-empty={empty.to_string()}
        >
            if empty {
                <div class="mosaic-message-empty">{"No pending direction."}</div>
            }
            <MosaicPlane
                key={team.lane.to_string()}
                owner={PlaneOwner::Team { lane: team.lane, drive: plane.drive, drain: plane.drain }}
                members={team.members.len()}
                cards={cards.to_vec()}
                quote_dispatcher={drafts.dispatcher()}
                open_message_task={render.open_message_task.clone()}
                speak={controls.speak.clone()}
                {narrate}
                speech={plane.speech}
                speech_owner={(*controls.speech_owner).clone()}
                history_targets={history.targets}
                history_revision={history.revision}
                history={history_sender}
                {composer_slot}
            />
        </div>
    }
}

struct HistoryPaging<'a> {
    sender: &'a SenderHandle,
    targets: BTreeMap<CanonicalUuid, (wire::MessageHistoryCursor, u64)>,
    revision: u64,
}

impl<'a> HistoryPaging<'a> {
    fn of(runtime: &AppRuntime, team: &crate::runtime::LaneRow, sender: &'a SenderHandle) -> Self {
        Self {
            sender,
            targets: team_history_targets(runtime, team),
            revision: runtime.message_history_revision(),
        }
    }
}

/// One lease instant as a ticking age, or `vacant` for a lane holding no claim.
fn lease_age(instant: Option<i64>, class: &'static str, vacant: &'static str) -> Html {
    match instant {
        Some(authored) => html! { <Age {authored} class={class} /> },
        None => html! {
            <span
                class={class}
                data-age-slot={watchband::AGE_SLOT_MIN_WIDTH.to_string()}
            >{vacant}</span>
        },
    }
}

struct ComposerStreamLink {
    target: String,
    label: String,
    display_millis: Option<i64>,
}

struct ComposerHeader {
    stream: ComposerStreamLink,
    prose: String,
}

/// Resolve one human-facing message once, then derive every header field from it.
///
/// Tool previews, turn starts, and compaction boundaries remain useful activity
/// cards, but none is the assistant prose printed beside this age. Selecting
/// them here would make the clock describe a different event from the text it
/// labels. Only this lane's own prose is eligible: a lane without any, such as
/// a vacant member that has never run, shows the empty header rather than the
/// latest message another member of the team happened to produce.
fn primary_composer_header(
    runtime: &AppRuntime,
    team: &crate::runtime::LaneRow,
    lane: CanonicalUuid,
    standing: &Standing,
) -> ComposerHeader {
    runtime.latest_lane_prose(lane).map_or_else(
        || ComposerHeader {
            stream: ComposerStreamLink {
                target: crate::view::team_stream_card_target(team.lane),
                label: "Jump to team activity".to_owned(),
                display_millis: None,
            },
            prose: "No agent message yet".to_owned(),
        },
        |(message, prose)| ComposerHeader {
            stream: ComposerStreamLink {
                target: agent_message_source_id(message),
                label: format!("Jump to latest {} message", standing.label),
                display_millis: Some(message.display_millis()),
            },
            prose,
        },
    )
}

fn copy_dragover() -> Callback<DragEvent> {
    Callback::from(move |event: DragEvent| {
        if let Some(transfer) = event.data_transfer() {
            transfer.set_drop_effect("copy");
        }
        event.prevent_default();
    })
}

struct PrimaryComposerDragging {
    enabled: bool,
    start: Callback<PointerEvent>,
    movement: Callback<PointerEvent>,
    finish: Callback<PointerEvent>,
    cancel: Callback<PointerEvent>,
    lost_capture: Callback<PointerEvent>,
}

struct AgentTeamControl {
    disabled: bool,
    activate: Callback<MouseEvent>,
}

/// Apply local visibility without replacing identity, or submit a guarded move.
pub(in crate::browser) fn team_action(
    change: crate::roster::Change<CanonicalUuid, CanonicalUuid>,
    topology_revision: u64,
    topology: &[(CanonicalUuid, Vec<CanonicalUuid>)],
    controls: &UseReducerHandle<TeamControls>,
    sender: &SenderHandle,
) -> bool {
    use crate::roster::{Change, Destination};
    let action = match change {
        Change::Move { member, to } => {
            let Some((source, members)) = topology
                .iter()
                .find(|(_, members)| members.contains(&member))
            else {
                return false;
            };
            if matches!(to, Destination::Ungrouped) && members.len() == 1 && *source == member {
                controls.dispatch(TeamControlAction::HideTeam { team: *source });
                return true;
            }
            ClientAction::ConfigureTeam {
                topology_revision,
                new_team: matches!(to, Destination::NewGroup(_)),
                team: match to {
                    Destination::Group(team) => team,
                    Destination::NewGroup(_) => *source,
                    Destination::Ungrouped => member,
                },
                member: Some(member),
                drive: None,
                drain: None,
                fast_mode: None,
            }
        }
        Change::Arrange(teams) => ClientAction::ArrangeTeams {
            topology_revision,
            teams: teams
                .into_iter()
                .map(|(team, lanes)| TeamArrangement { team, lanes })
                .collect(),
            respond: None,
        },
    };
    admit_action(sender, action)
}

/// One team composer-menu membership action.
fn agent_team_control(
    change: Option<crate::roster::Change<CanonicalUuid, CanonicalUuid>>,
    enabled: bool,
    topology_revision: u64,
    sender: &SenderHandle,
    topology: &Rc<Vec<(CanonicalUuid, Vec<CanonicalUuid>)>>,
    controls: &UseReducerHandle<TeamControls>,
) -> AgentTeamControl {
    let change = change.filter(|_| enabled);
    let disabled = change.is_none();
    let sender = sender.clone();
    let topology = topology.clone();
    let controls = controls.clone();
    let activate = Callback::from(move |event: MouseEvent| {
        if let Some(change) = change.clone()
            && team_action(change, topology_revision, &topology, &controls, &sender)
        {
            agent_menu::close(&event);
        }
    });
    AgentTeamControl { disabled, activate }
}

/// What every member composer of one team shares: the session it submits
/// under, the topology it may rearrange, the handles it edits drafts and
/// submissions through, and the team's broadcast targets.
#[derive(Clone, Copy)]
struct TeamComposer<'a> {
    epoch: CanonicalUuid,
    topology_revision: u64,
    topology: &'a [(CanonicalUuid, Vec<CanonicalUuid>)],
    authority: SubmissionAuthority,
    can_arrange: bool,
    sender: &'a SenderHandle,
    composer_drag: &'a PrimaryComposerDragState,
    drafts: &'a UseReducerHandle<ComposerDrafts>,
    submissions: &'a SubmissionHandle,
    broadcast_targets: &'a [BroadcastTarget],
    broadcast_help: &'a str,
}

fn render_agent_composer(
    team: &TeamComposer<'_>,
    member: &crate::runtime::LaneMember,
    standing: &Standing,
    menu: Html,
    rail: Html,
    header: ComposerHeader,
) -> Html {
    let TeamComposer {
        epoch,
        topology_revision,
        topology,
        authority,
        can_arrange,
        sender,
        composer_drag,
        drafts,
        submissions,
        broadcast_targets,
        broadcast_help,
    } = *team;
    let lane_id = member.lane;
    let lane_key = lane_id.to_string();
    let draft = drafts.get(&lane_key).cloned().unwrap_or_default();
    // A send stays in its composer, uneditable, until durably accepted.
    let locked = submissions.status.holds(ComposerSource::Lane(lane_id));
    let target = authority.target(member.occupant, member.wakeable, epoch);
    let wake = matches!(target, Some(SubmissionTarget::Vacant { .. }));
    let broadcast_eligible = broadcast_targets
        .iter()
        .any(|target| target.lane == lane_id);
    let eligibility = ShardEligibility {
        command_submit: target.is_some(),
        broadcast_target: broadcast_eligible,
        broadcast_source: broadcast_eligible,
    };
    let onkeydown = keyboard_submit(
        lane_id,
        target,
        lane_key.clone(),
        sender.clone(),
        drafts.clone(),
        submissions.clone(),
        broadcast_targets.to_vec(),
    );
    let ondragover = copy_dragover();
    let ondrop = drop_on_lane(
        lane_id,
        target,
        lane_key.clone(),
        sender.clone(),
        drafts.clone(),
    );
    let dragging = primary_composer_dragging(
        lane_id,
        can_arrange,
        topology_revision,
        topology,
        sender,
        composer_drag,
    );
    let status = member.status(standing.waking);
    html! {
        <section
            key={lane_key.clone()}
            class={classes!(
                "team-agent",
                member.occupant.is_none().then_some("team-agent--vacant"),
            )}
            data-agent-lane={lane_id.to_string()}
            data-wake-and-send={wake.to_string()}
            data-accent={standing.accent.to_string()}
            data-send-state={locked.then_some("sending")}
            aria-busy={locked.then_some("true")}
            title={watchband::standing_title(standing)}
            {ondragover}
            {ondrop}
        >
            <div
                class="agent-composer-stack"
                data-agent-composer-stack={lane_id.to_string()}
                data-has-quote-shards={(!draft.quotes.is_empty()).to_string()}
            >
                {render_composer_body(
                    ShardDraft {
                        lane: lane_id,
                        lane_key: &lane_key,
                        draft: &draft,
                        locked,
                        standing,
                        drafts,
                    },
                    ShardChrome {
                        status,
                        header,
                        menu,
                        rail,
                    },
                    eligibility,
                    broadcast_help,
                    onkeydown.clone(),
                    dragging,
                )}
                {render_quote_shards(
                    &lane_key,
                    &draft,
                    Some(status),
                    drafts,
                    onkeydown,
                    &composer_placeholder(standing, standing.pending),
                    locked,
                )}
            </div>
        </section>
    }
}

/// The lane one primary shard composes for, with its draft and the store the
/// shard edits it in. A locked shard's send awaits durable acceptance.
struct ShardDraft<'a> {
    lane: CanonicalUuid,
    lane_key: &'a str,
    draft: &'a ComposerDraft,
    locked: bool,
    standing: &'a Standing,
    drafts: &'a UseReducerHandle<ComposerDrafts>,
}

/// The chrome around one primary shard's textarea: the lane's status, the
/// stream link and prose in its heading, the agent menu, and the trace rail.
struct ShardChrome {
    status: crate::runtime::LaneStatus,
    header: ComposerHeader,
    menu: Html,
    rail: Html,
}

/// What a primary shard advertises to its keyboard and drop handlers: whether
/// Enter can submit, and whether the lane can receive or source a broadcast.
#[derive(Clone, Copy)]
struct ShardEligibility {
    command_submit: bool,
    broadcast_target: bool,
    broadcast_source: bool,
}

fn render_composer_body(
    shard: ShardDraft<'_>,
    chrome: ShardChrome,
    eligibility: ShardEligibility,
    broadcast_help: &str,
    onkeydown: Callback<KeyboardEvent>,
    dragging: PrimaryComposerDragging,
) -> Html {
    let ShardDraft {
        lane,
        lane_key,
        draft,
        locked,
        standing,
        drafts,
    } = shard;
    let ShardChrome {
        status,
        header,
        menu,
        rail,
    } = chrome;
    let onpaste = if locked {
        Callback::noop()
    } else {
        paste_images(lane_key.to_owned(), None, drafts.clone())
    };
    let heading = render_composer_heading(
        status,
        menu,
        header,
        render_image_indicators(lane_key, &draft.attachments, None, drafts, locked),
        &dragging,
    );
    // Pointer capture stays on prose, leaving textarea editing and attachment
    // drag/drop untouched while the lifted composer can cross sibling slots.
    let PrimaryComposerDragging {
        enabled: drag_enabled,
        movement: drag_movement,
        finish: drag_finish,
        cancel: drag_cancel,
        ..
    } = dragging;
    html! {
        <div
            class="mosaic-composer composer-shard composer-shard--primary"
            data-shard-kind="primary"
            data-shard-target-id={lane.to_string()}
            data-primary-composer-draggable={drag_enabled.to_string()}
            data-command-submit-eligible={eligibility.command_submit.to_string()}
            data-broadcast-target-eligible={eligibility.broadcast_target.to_string()}
            data-broadcast-source-eligible={eligibility.broadcast_source.to_string()}
            data-draft-unsafe={(
                !draft.body.is_empty()
                    || !draft.quotes.is_empty()
                    || !draft.attachments.is_empty()
            ).to_string()}
            onpointermove={drag_movement}
            onpointerup={drag_finish}
            onpointercancel={drag_cancel}
        >
            {heading}
            {rail}
            <ComposerTextarea
                key={draft.body.clone()}
                id={primary_textarea_id(lane_key)}
                lane={lane_key.to_owned()}
                drafts={drafts.dispatcher()}
                label={format!("Message for agent {lane}")}
                body={draft.body.clone()}
                placeholder={composer_placeholder(standing, standing.pending)}
                rows="4"
                primary_target={Some(lane.to_string())}
                quote_identity={None::<String>}
                broadcast_help={Some(broadcast_help.to_owned())}
                {onkeydown}
                {onpaste}
                {locked}
            />
        </div>
    }
}

fn render_composer_heading(
    status: crate::runtime::LaneStatus,
    menu: Html,
    header: ComposerHeader,
    indicators: Html,
    dragging: &PrimaryComposerDragging,
) -> Html {
    let ComposerHeader {
        stream:
            ComposerStreamLink {
                target,
                label,
                display_millis,
            },
        prose,
    } = header;
    html! {
        <header class="app-glass-control-surface composer-shard-heading">
            <a
                class={classes!(
                    "composer-shard-link",
                    "composer-shard-age",
                    status.key(),
                )}
                href={crate::view::card_href(&target)}
                aria-label={label}
                data-lane-status={status.key()}
            >{lease_age(display_millis, "composer-shard-age-label", "—")}</a>
            <strong
                class="composer-shard-prose"
                title={prose.clone()}
                data-composer-drag-handle={dragging.enabled.to_string()}
                onpointerdown={dragging.start.clone()}
                onlostpointercapture={dragging.lost_capture.clone()}
            >{prose}</strong>
            {indicators}
            {menu}
        </header>
    }
}

fn composer_placeholder(standing: &Standing, pending: usize) -> String {
    let branch = watchband::agent_message_label(&standing.worktree, None);
    let target = if standing.label == branch {
        standing.label.clone()
    } else {
        format!("{}, {branch}", standing.label)
    };
    let mut lines = vec![
        target,
        format!("{pending} pending, {}", standing.liveness.as_str()),
    ];
    if let Some(handle) = &standing.handle {
        lines.push(match &standing.phase {
            Some(phase) => format!("{handle}, {phase}"),
            None => handle.clone(),
        });
    }
    if let Some(kept) = watchband::jobs_kept(&standing.jobs) {
        lines.push(kept);
    }
    lines.join("\n")
}

pub(in crate::browser) fn current_composer_draft(
    lane_key: &str,
    drafts: &ComposerDrafts,
) -> ComposerDraft {
    let mut draft = drafts.get(lane_key).cloned().unwrap_or_default();
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return draft;
    };
    if let Some(textarea) = document
        .get_element_by_id(&primary_textarea_id(lane_key))
        .and_then(|element| element.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
    {
        draft.body = textarea.value();
    }
    if lane_key == crate::composer_drafts::NEW_DRAFT_KEY
        && let Some(textarea) = document
            .get_element_by_id(crate::composer_drafts::CREATOR_CONTEXT_ID)
            .and_then(|element| element.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
    {
        draft.continuation = textarea.value();
    }
    for quote in &mut draft.quotes {
        if let Some(textarea) = document
            .get_element_by_id(&format!("composer-quote-{lane_key}-{}", quote.identity))
            .and_then(|element| element.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
        {
            quote.body = textarea.value();
        }
    }
    draft
}

/// One composer's live text and drafted quotes, composed into its message, or
/// the whole-set media refusal reported to that composer.
pub(in crate::browser) fn prepare_composer(
    lane_key: &str,
    drafts: &UseReducerHandle<ComposerDrafts>,
) -> Result<Option<crate::composer_drafts::PreparedSubmission>, ()> {
    let mut current = drafts.0.clone();
    *current.draft_mut(lane_key.to_owned()) = current_composer_draft(lane_key, drafts);
    current
        .prepare(lane_key)
        .map_err(|error| report_refused_media(drafts, &error))
}

fn report_refused_media(
    drafts: &UseReducerHandle<ComposerDrafts>,
    error: &wire::media::MediaError,
) {
    drafts.dispatch(ComposerAction::Report {
        issue: format!("Attached images were refused: {error}"),
    });
}

/// Empty one composer: its element's text and its drafted quotes and images.
pub(in crate::browser) fn clear_composer(
    lane_key: &str,
    drafts: &UseReducerHandle<ComposerDrafts>,
) {
    clear_primary_composer(lane_key);
    drafts.dispatch(ComposerAction::Clear {
        lane: lane_key.to_owned(),
    });
}

/// Empty what a durably accepted send carried from its still-locked composer:
/// the primary element's text in place, and the sent body, quotes, and images.
pub(in crate::browser) fn clear_sent_composer(
    lane_key: &str,
    drafts: &UseReducerHandle<ComposerDrafts>,
    sent: Rc<ComposerDraft>,
) {
    clear_primary_composer(lane_key);
    drafts.dispatch(ComposerAction::ClearSent {
        lane: lane_key.to_owned(),
        sent,
    });
}

fn clear_primary_composer(lane_key: &str) {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let primary = primary_textarea_id(lane_key);
    let continuation = (lane_key == crate::composer_drafts::NEW_DRAFT_KEY)
        .then_some(crate::composer_drafts::CREATOR_CONTEXT_ID);
    for id in std::iter::once(primary.as_str()).chain(continuation) {
        if let Some(textarea) = document
            .get_element_by_id(id)
            .and_then(|element| element.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
        {
            textarea.set_value("");
            composer::announce_value(&textarea);
        }
    }
}

/// Each member as the send button sees it, in member order.
fn send_slots(
    team: &crate::runtime::LaneRow,
    standings: &[Standing],
    drafts: &ComposerDrafts,
    submissions: &SubmissionStatus,
    authority: SubmissionAuthority,
    epoch: CanonicalUuid,
    broadcast_targets: &[BroadcastTarget],
) -> Vec<SendSlot> {
    team.members
        .iter()
        .zip(standings)
        .map(|(member, standing)| {
            let lane = member.lane.to_string();
            SendSlot {
                attached: drafts.holds_quotes_or_images(&lane),
                section: format!("[data-agent-lane=\"{lane}\"]"),
                accent: standing.accent,
                reachable: authority
                    .target(member.occupant, member.wakeable, epoch)
                    .is_some(),
                broadcast: broadcast_targets
                    .iter()
                    .any(|target| target.lane == member.lane),
                sending: submissions.holds(ComposerSource::Lane(member.lane)),
            }
        })
        .collect()
}

fn submit_team_callback(
    members: &[crate::runtime::LaneMember],
    epoch: CanonicalUuid,
    authority: SubmissionAuthority,
    sender: SenderHandle,
    drafts: UseReducerHandle<ComposerDrafts>,
    submissions: SubmissionHandle,
) -> Callback<SubmitEvent> {
    let targets = members
        .iter()
        .map(|member| {
            let target = authority.target(member.occupant, member.wakeable, epoch);
            (member.lane, target, member.lane.to_string())
        })
        .collect::<Vec<_>>();
    Callback::from(move |event: SubmitEvent| {
        event.prevent_default();
        let Ok(fanout) = random_uuid() else {
            return;
        };
        for (lane, target, lane_key) in &targets {
            submit_draft(
                fanout,
                *lane,
                *target,
                lane_key,
                &sender,
                &drafts,
                &submissions,
            );
        }
    })
}

fn submit_draft(
    fanout: CanonicalUuid,
    lane: CanonicalUuid,
    target: Option<SubmissionTarget>,
    lane_key: &str,
    sender: &SenderHandle,
    drafts: &UseReducerHandle<ComposerDrafts>,
    submissions: &SubmissionHandle,
) -> bool {
    // A locked composer's content is already in flight; sending it again
    // would duplicate the direction.
    if submissions.queue.borrow().holds(ComposerSource::Lane(lane)) {
        return false;
    }
    let Ok(Some(prepared)) = prepare_composer(lane_key, drafts) else {
        return false;
    };
    let Ok(id) = random_uuid() else {
        return false;
    };
    let typed = crate::browser::input_history::typed_texts(&prepared.snapshot);
    let authored_millis = js_sys::Date::now() as i64;
    let submission = match target {
        Some(SubmissionTarget::Resident { occupant, epoch }) => Submission::new(
            id,
            fanout,
            lane,
            (occupant, epoch, authored_millis),
            prepared.message,
            prepared.attachments,
            prepared.snapshot,
        ),
        Some(SubmissionTarget::Vacant { epoch }) => Submission::vacant(
            id,
            fanout,
            lane,
            (epoch, authored_millis),
            prepared.message,
            prepared.attachments,
            prepared.snapshot,
        ),
        None => return false,
    };
    if !queue_submission(sender, submissions, submission) {
        return false;
    }
    crate::browser::input_history::record(lane_key, &typed);
    true
}
