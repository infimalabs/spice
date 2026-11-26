//! App surface rendering for the operator browser.

use super::*;
use crate::work::WorkProjection;

pub(super) mod chrome;
pub(super) mod quota;
mod sidebar;
mod spice_jobs;
mod spice_principals;
mod spice_sidebar;
mod work_blockers;
mod work_detail;
mod work_list;
mod work_map;

pub(super) use work_list::{WorkListControls, graph_selection, use_work_list};
pub(super) use work_map::{WorkMapState, use_work_map};

#[derive(Clone)]
pub(super) struct AgentControls {
    /// Admit manual speech to the one browser-global playback queue.
    pub(super) speak: Callback<SpeechRequest>,
    /// Advance durable narration cursors and admit their live prefix.
    pub(super) narrate: Callback<NarrationBatch>,
    /// Set one team's exact automatic-speech mode and media-session posture.
    pub(super) set_automatic_speech: Callback<(String, crate::speech_queue::SpeechMode)>,
    /// Exact rendered message hunk currently owned by the speech queue.
    pub(super) speech_owner: UseStateHandle<Option<SpeechOwner>>,
}

/// Pending direction count shared by compact and expanded presence indicators.
pub(super) fn member_pending(
    waiting: &[wire::intent::LaneInbox],
    submissions: &SubmissionStatus,
    lane: CanonicalUuid,
) -> usize {
    submissions.pending(lane, member_backend(waiting, lane))
}

/// Pending direction that can wake the lane, the only kind that marks it queued.
pub(super) fn member_waking(
    waiting: &[wire::intent::LaneInbox],
    submissions: &SubmissionStatus,
    lane: CanonicalUuid,
) -> usize {
    submissions.waking(lane, member_backend(waiting, lane))
}

fn member_backend(
    waiting: &[wire::intent::LaneInbox],
    lane: CanonicalUuid,
) -> &[wire::intent::PendingDirective] {
    waiting
        .iter()
        .find(|row| row.lane == lane)
        .map_or(&[], |row| row.pending.as_slice())
}

/// One shared live-state marker for global and team action menus: colour is
/// the member's status, and a hollow centre means the resident parked it.
pub(super) fn render_member_presence(
    member: &crate::runtime::LaneMember,
    status: crate::runtime::LaneStatus,
) -> Html {
    let label = member.presence_label(status);
    html! {
        <span
            class={classes!(
                "app-presence-pip",
                "app-presence-pip--member",
                status.key(),
                member.parked.map(|_| "parked")
            )}
            role="img"
            data-lane-status={status.key()}
            data-lane-parked={member.parked.map(wire::body::LanePark::as_str)}
            aria-label={label.clone()}
            title={label}
        ></span>
    }
}

pub(super) struct AppRender<'a> {
    pub connection: &'a UseStateHandle<Connection>,
    pub surface: &'a UseStateHandle<SurfaceData>,
    pub card_target_notice: Option<String>,
    pub navigate: &'a Callback<navigation::Intent>,
    pub spice_sidebar_open: bool,
    pub open_spice_sidebar: Callback<web_sys::Element>,
    pub identity: &'a UseStateHandle<Option<AppIdentity>>,
    pub app_update: &'a UseStateHandle<Option<AppUpdate>>,
    pub team_controls: &'a UseReducerHandle<TeamControls>,
    pub composer_drafts: &'a UseReducerHandle<ComposerDrafts>,
    pub submissions: &'a SubmissionHandle,
    pub composer_drag: &'a super::watchband::PrimaryComposerDragState,
    pub composer_slots: &'a super::watchband::ComposerSlots,
    pub team_drag: &'a super::watchband::TeamDragState,
    pub team_fleet: &'a NodeRef,
    pub unsent: &'a UseStateHandle<Option<Vec<crate::unsent_work::UnsentWork>>>,
    pub sender: &'a SenderHandle,
    pub room_controls: &'a super::rooms::Controls,
    pub draft_controls: &'a super::drafts::Controls,
    pub agents: &'a AgentControls,
    pub sidebar_content: &'a UseStateHandle<Option<super::sidebar::Content>>,
    pub sidebar_presented: bool,
    pub sidebar_closing: bool,
    pub work_list: &'a WorkListControls,
    pub work_map: &'a WorkMapState,
    pub sidebar_close: &'a NodeRef,
    pub sidebar_sheet: &'a NodeRef,
    pub notes: &'a NoteComposers,
    pub submit_note: Callback<(wire::TaskStamp, SubmitEvent)>,
    pub decide_trust: Callback<task_detail::TrustDecisionRequest>,
    pub select_task: Callback<wire::TaskStamp>,
    pub open_overview: Callback<crate::work::TaskOverviewSelection>,
    pub open_message_task: Callback<(wire::TaskStamp, web_sys::Element)>,
    pub request_task: Callback<wire::TaskStamp>,
    pub retry_relations: Callback<MouseEvent>,
    pub close_sidebar: Callback<()>,
}

pub(super) fn render_app(render: AppRender<'_>, work: Option<&WorkProjection<'_>>) -> Html {
    let surface = render.surface.read();
    let topology_revision = surface
        .as_ref()
        .and_then(AppRuntime::lanes)
        .map(|lanes| lanes.topology_revision.to_string());
    let inbox_waiting = surface.as_ref().map_or(0, chrome::waiting_messages);
    let inbox = spice_principals::render(
        render.connection,
        surface.as_ref(),
        render.sender,
        super::rooms::render_inbox(
            surface.as_ref(),
            render.room_controls,
            render.sender,
            render.composer_drafts,
            render.agents,
            &render.open_message_task,
        ),
    );
    html! {
        <div
            class="shell"
            data-topology-revision={topology_revision}
            data-submission-count={render.submissions.status.len().to_string()}
        >
            {chrome::render_command_bar(chrome::CommandBarRender {
                connection: render.connection,
                navigate: render.navigate,
                spice_sidebar_open: render.spice_sidebar_open,
                open_spice_sidebar: &render.open_spice_sidebar,
                app_update: render.app_update,
                work,
                surface: render.surface,
                sender: render.sender,
                composer: html! {
                    <super::drafts::ComposeEntry
                        surface={render.surface.clone()}
                        connection={render.connection.clone()}
                        controls={render.draft_controls.clone()}
                        sender={render.sender.clone()}
                        submissions={render.submissions.clone()}
                        open_task={render.select_task.clone()}
                        navigate={render.navigate.clone()}
                        inbox={inbox}
                        inbox_waiting={inbox_waiting}
                    />
                },
            })}
            {super::unsent::render(&super::unsent::UnsentRender {
                held: render.unsent,
                runtime: surface.as_ref(),
                drafts: render.composer_drafts,
                notes: render.notes,
                select_task: &render.select_task,
            })}
            {watchband::render_feedback(surface.as_ref(), render.team_controls)}
            if let Some(notice) = &render.card_target_notice {
                <p
                    class="visually-hidden"
                    role="status"
                    aria-live="polite"
                    data-card-target-status=""
                >{notice.clone()}</p>
            }
            {render_workspace(&render, work)}
            {render_sidebar(&render, work)}
        </div>
    }
}

fn render_workspace(render: &AppRender<'_>, work: Option<&WorkProjection<'_>>) -> Html {
    html! {
        <div class="workspace mosaic-workspace">
            <main class="mosaic-content content">
                {watchband::render_team(render, work)}
            </main>
        </div>
    }
}

fn render_sidebar(render: &AppRender<'_>, work: Option<&WorkProjection<'_>>) -> Html {
    let sidebar_content = render.sidebar_content.as_ref();
    let Some(content) = sidebar_content.as_ref() else {
        return Html::default();
    };
    let surface = render.surface.read();
    let runtime = surface.as_ref();
    let content = match content {
        super::sidebar::Content::Spice => render_spice_sidebar(render, runtime),
        super::sidebar::Content::Tasks(selection) => {
            let Some(work) = work else {
                return Html::default();
            };
            work_list::render(work_list::WorkListRender {
                detail: work_detail::WorkDetailRender {
                    work,
                    lanes: runtime.and_then(AppRuntime::lanes),
                    navigate: render.navigate,
                    creator_messages: runtime.map(AppRuntime::messages).unwrap_or_default(),
                    selection,
                    runtime,
                    room_controls: render.room_controls,
                    sender: render.sender,
                    draft_controls: render.draft_controls,
                    connected: render.connection.is_online(),
                    can_note: runtime.is_some_and(|runtime| runtime.offers(CommandKind::RoomPost)),
                    can_close: runtime
                        .is_some_and(|runtime| runtime.offers(CommandKind::TaskClose)),
                    can_conclude: runtime
                        .is_some_and(|runtime| runtime.offers(CommandKind::TaskConclude)),
                    notes: render.notes,
                    submit_note: render.submit_note.clone(),
                    decide_trust: render.decide_trust.clone(),
                    request_task: render.request_task.clone(),
                    open_message_task: render.open_message_task.clone(),
                },
                controls: render.work_list,
                map: render.work_map,
                retry_relations: render.retry_relations.clone(),
                open_overview: render.open_overview.clone(),
            })
        }
    };
    sidebar::render(sidebar::SidebarRender {
        content,
        presented: render.sidebar_presented,
        closing: render.sidebar_closing,
        close_ref: render.sidebar_close,
        sheet_ref: render.sidebar_sheet,
        close: render.close_sidebar.clone(),
    })
}
fn render_spice_sidebar(render: &AppRender<'_>, runtime: Option<&AppRuntime>) -> sidebar::Content {
    let standings = spice_standings(render, runtime).map(Rc::new);
    let body = html! {
        <div class="app-spice-sidebar-content">
            {quota::render_sidebar(runtime)}
            {render_spice_topology(render, runtime, standings.as_deref().map(Vec::as_slice))}
            {spice_jobs::render(standings.clone(), render.connection, runtime, render.sender)}
        </div>
    };
    let anchor = html! {
        <div class="app-spice-sidebar-anchor">
            <div
                class="app-spice-sidebar-readouts"
                inert={(!render.sidebar_presented || render.sidebar_closing).then_some("true")}
            >
                {spice_sidebar::render_theme_selector()}
                {spice_sidebar::render_global_turbo(runtime, render.sender)}
                {spice_sidebar::render_capacity(runtime)}
            </div>
            {chrome::render_spice_button(chrome::SpiceButton {
                connection: render.connection,
                app_update: (**render.app_update).as_ref(),
                runtime,
                place: chrome::SpicePlace::Anchor,
                node_ref: render.sidebar_close.clone(),
                onclick: render.close_sidebar.reform(|_: MouseEvent| ()),
            })}
        </div>
    };
    sidebar::Content {
        label: "Spice".into(),
        heading: spice_sidebar::render_resident_summary(
            render.connection,
            runtime
                .and_then(AppRuntime::resume)
                .map(|cursor| cursor.seq),
            (**render.identity).as_ref(),
            (**render.app_update).as_ref(),
        ),
        width: sidebar::Width::Small,
        base: None,
        body,
        anchor: Some(anchor),
    }
}

/// Every lane's standing with this browser's own pending submissions folded
/// in; `None` until the resident's lane state has arrived.
fn spice_standings(
    render: &AppRender<'_>,
    runtime: Option<&AppRuntime>,
) -> Option<Vec<crate::watchband::Standing>> {
    let lanes = runtime.and_then(AppRuntime::lanes)?;
    let waiting = runtime
        .and_then(AppRuntime::inbox)
        .map(|inbox| inbox.lanes.as_slice())
        .unwrap_or(&[]);
    let tasks = runtime
        .and_then(AppRuntime::board)
        .map(|board| board.tasks.as_slice())
        .unwrap_or(&[]);
    let mut standings = crate::watchband::standings(&lanes.lanes, waiting, tasks);
    for standing in &mut standings {
        standing.pending = member_pending(waiting, &render.submissions.status, standing.lane);
        standing.waking = member_waking(waiting, &render.submissions.status, standing.lane);
    }
    Some(standings)
}

fn render_spice_topology(
    render: &AppRender<'_>,
    runtime: Option<&AppRuntime>,
    standings: Option<&[crate::watchband::Standing]>,
) -> Html {
    let Some(standings) = standings else {
        return html! {
            <section
                class="app-sheet-section app-spice-topology"
                aria-labelledby="spice-teams-heading"
                data-state="loading"
            >
                <div class="section-heading">
                    <h2 id="spice-teams-heading">{"Teams"}</h2>
                    <span class="section-count">{"Waiting for agent state…"}</span>
                </div>
                <div class="app-spice-topology-placeholder" aria-hidden="true"></div>
            </section>
        };
    };
    let live = standings
        .iter()
        .filter(|standing| standing.thread.is_some())
        .count();
    // Teams, held tasks, and pending steering read from the roster itself;
    // the heading keeps only the roster-wide totals no group or slot shows.
    let totals = format!(
        "{} · {live} live",
        topology_count(standings.len(), "agent", "agents")
    );
    html! {
        <section
            class="app-sheet-section app-spice-topology"
            aria-labelledby="spice-teams-heading"
            data-state="ready"
        >
            <div class="section-heading">
                <h2 id="spice-teams-heading">{"Teams"}</h2>
                <span class="section-count">{totals}</span>
            </div>
            {spice_sidebar::render_arrangement(
                runtime,
                &spice_sidebar::Context {
                    sender: render.sender,
                    team_controls: render.team_controls,
                },
                standings,
            )}
        </section>
    }
}

/// A team's display name, shared by the topology summary and team-addressed
/// pages.
fn team_name(team: &crate::runtime::LaneRow) -> String {
    let host = team.host().map_or("Empty", |member| {
        crate::watchband::agent_message_label(&member.worktree, member.name.as_deref())
    });
    format!("{host} team")
}

/// The operator's avatar: the Gravatar banded with the initials badge, or the
/// initials alone.
pub(super) fn render_operator_avatar(
    principal: Option<&wire::principal::PrincipalDisplay>,
) -> Html {
    render_avatar(principal, true)
}

/// The operator's avatar without the initials band over a Gravatar, for a
/// control too small to spare any of the image.
pub(super) fn render_operator_face(principal: Option<&wire::principal::PrincipalDisplay>) -> Html {
    render_avatar(principal, false)
}

fn render_avatar(principal: Option<&wire::principal::PrincipalDisplay>, badge: bool) -> Html {
    let label = principal
        .and_then(|identity| identity.name.as_deref())
        .unwrap_or("Your");
    let initials = principal.and_then(|identity| identity.initials.as_deref());
    match principal.and_then(|identity| identity.gravatar.as_ref()) {
        Some(gravatar) => html! {
            <span class="operator-avatar" data-avatar-source="gravatar">
                <img
                    src={format!("https://gravatar.com/avatar/{}?s=160&d=retro", gravatar.as_str())}
                    alt={format!("{label} Gravatar")}
                />
                if let Some(initials) = initials.filter(|_| badge) {
                    <span class="operator-avatar-badge" aria-hidden="true">{initials}</span>
                }
            </span>
        },
        None => html! {
            <span
                class="operator-avatar"
                data-avatar-source="initials"
                role="img"
                aria-label={format!("{label} badge")}
            >{initials.unwrap_or("?")}</span>
        },
    }
}

fn topology_count(count: usize, singular: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { plural })
}

/// One lane as a full-width topology slot of two fixed lines: presence, the
/// accent-dealt name, worktree, and model over the "pending, liveness" state,
/// the held task, and kept jobs, with the provider emblem at the end. The
/// model drops a known provider's prefix because the emblem names it.
///
/// Every seated lane links to its composer, revealing its team when hidden.
fn render_topology_agent(
    member: &crate::runtime::LaneMember,
    standing: &crate::watchband::Standing,
) -> Html {
    debug_assert_eq!(member.lane, standing.lane, "standings keep member order");
    let status = member.status(standing.waking);
    let branch = crate::watchband::agent_message_label(&standing.worktree, None);
    let lane = standing.lane.to_string();
    let href = crate::view::card_href(&super::watchband::primary_textarea_id(&lane));
    let label = format!("Open {}'s composer in Chat", standing.label);
    html! {
        <a
            class="app-roster-tile app-spice-topology-agent"
            href={href}
            aria-label={label}
            data-accent={standing.accent.to_string()}
            data-agent-lane={lane.clone()}
            data-lane-status={status.key()}
            title={format!("Lane {lane}\nGroup {}", standing.team)}
        >
            <header>
                {render_member_presence(member, status)}
                <strong data-roster-handle="">{standing.label.clone()}</strong>
                if standing.label != branch {
                    <span class="app-spice-topology-agent-worktree" title={standing.worktree.clone()}>{branch.to_owned()}</span>
                }
                <span class="app-spice-topology-agent-model">{
                    crate::watchband::model_name(standing.model.as_deref())
                        .unwrap_or(crate::watchband::MODEL_UNRECORDED)
                        .to_owned()
                }</span>
            </header>
            <p>
                <span>{format!("{} pending, {}", standing.pending, standing.liveness.as_str())}</span>
                if let Some(handle) = &standing.handle {
                    <span class="app-spice-topology-agent-task" title={standing.task_title.as_deref().map(crate::composer_markdown::inline::plain_text)}>{
                        standing.phase.as_ref().map_or_else(|| handle.clone(), |phase| format!("{handle}, {phase}"))
                    }</span>
                }
                if let Some(kept) = crate::watchband::jobs_kept(&standing.jobs) {
                    <span>{kept}</span>
                }
            </p>
            {super::watchband::render_provider_emblem(standing, "sidebar")}
        </a>
    }
}
