//! Team-fleet shell, topology drag, and per-team toolbar rendering.

use super::*;
use crate::browser::clock::{Age, local_full};

pub(super) struct FleetRender<'a, 'w> {
    pub(super) work: Option<&'a crate::work::WorkProjection<'w>>,
    pub(super) navigate: &'a Callback<navigation::Intent>,
    pub(super) select_task: &'a Callback<wire::TaskStamp>,
    pub(super) open_message_task: &'a Callback<(wire::TaskStamp, web_sys::Element)>,
    pub(super) runtime: &'a AppRuntime,
    pub(super) epoch: CanonicalUuid,
    pub(super) topology_revision: u64,
    pub(super) authority: SubmissionAuthority,
    pub(super) can_arrange_teams: bool,
    pub(super) can_configure: bool,
    pub(super) topology: Rc<Vec<(CanonicalUuid, Vec<CanonicalUuid>)>>,
    pub(super) quote_agents: Rc<crate::composer_markdown::QuoteAgents>,
    pub(super) surface: UseStateHandle<SurfaceData>,
    pub(super) sender: &'a SenderHandle,
    pub(super) drafts: &'a UseReducerHandle<ComposerDrafts>,
    pub(super) submissions: &'a SubmissionHandle,
    pub(super) team_controls: &'a UseReducerHandle<TeamControls>,
    pub(super) controls: &'a view::AgentControls,
    pub(super) composer_drag: &'a PrimaryComposerDragState,
    pub(super) composer_slots: &'a super::ComposerSlots,
    pub(super) team_drag: &'a TeamDragState,
}

/// The teamless console keeps its toolbar bands and stream, with `message`
/// as the empty placeholder.
pub(super) fn render_empty_fleet(fleet: &NodeRef, message: &'static str) -> Html {
    html! {
        <div ref={fleet.clone()} class="team-fleet" data-pane="agent-console" data-view="mosaic"
            tabindex="0" aria-label="Agent teams">
            <section class="team-surface" data-team-weight="1">
                <header class="team-toolbar">
                    <div class="team-task-rail"></div>
                    <div class="app-glass-control-surface app-status-band team-status-row">
                        {view::chrome::render_empty_team_ticker()}
                    </div>
                </header>
                <div class="team-pane">
                    <div class="team-activity-grid" data-empty="true">
                        <div class="mosaic-message-empty">{message}</div>
                    </div>
                </div>
            </section>
        </div>
    }
}

pub(super) fn render_populated_fleet(
    teams: &[&crate::runtime::LaneRow],
    standings: &[watchband::Standing],
    fleet: &NodeRef,
    render: &FleetRender<'_, '_>,
) -> Html {
    let dismiss = arrangement_dismissal(render.team_controls);
    html! {
        <div ref={fleet.clone()} class="team-fleet" data-pane="agent-console" data-view="mosaic"
            tabindex="0" aria-label="Agent teams" onclick={dismiss}>
            // Every team keeps its own subtree, so a reorder, fusion, or split moves
            // composers with their drafts, focus, and open menus instead of
            // repainting a neighbour's nodes. Yew diffs a list positionally unless
            // every child is keyed.
            {for teams.iter().map(|team| {
                let team_standings = standings
                    .iter()
                    .filter(|standing| standing.team == team.lane)
                    .cloned()
                    .collect::<Vec<_>>();
                let attention = attention::TeamAttention::new(
                    render.runtime,
                    team.lane,
                    &team_standings,
                    render.work,
                    render.sender,
                    render.select_task,
                    render.navigate,
                );
                html! {
                    <section
                        key={team.lane.to_string()}
                        class="team-surface"
                        data-team={team.lane.to_string()}
                        data-base-flavor="console"
                        data-base-accent={team.accent(0).to_string()}
                        data-team-weight={watchband::team_visual_weight(team.members.len()).to_string()}
                    >
                        {render_team_toolbar(
                            team,
                            &team_standings,
                            render,
                        )}
                        <div class="team-pane">
                            {render_compose(
                                render,
                                team,
                                &team_standings,
                                &attention,
                                render.composer_slots.slot(team.lane),
                            )}
                        </div>
                    </section>
                }
            })}
        </div>
    }
}

fn arrangement_dismissal(controls: &UseReducerHandle<TeamControls>) -> Callback<MouseEvent> {
    let controls = controls.clone();
    Callback::from(move |event: MouseEvent| {
        let inside_control = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .is_some_and(|target| {
                target
                    .closest(".team-action-menu, .team-lights[data-lane-drag-handle]")
                    .ok()
                    .flatten()
                    .is_some()
            });
        if !inside_control {
            controls.dispatch(TeamControlAction::CloseArrangement);
        }
    })
}

/// An open team arrangement owns Escape only while Chat shows it. Opening it
/// moves focus to its first available choice, so either opener reaches the
/// choices directly; Escape returns focus to the control that opened it.
#[hook]
pub(in crate::browser) fn use_team_arrangement_focus(
    controls: &UseReducerHandle<TeamControls>,
    visible: bool,
) {
    let open = controls.open_arrangement();
    use_effect_with(open, |open| {
        if let Some((team, _)) = open
            && let Some(choice) = team_element(*team, ".team-arrangement-action:not([disabled])")
        {
            let _ = choice.focus();
        }
    });
    let dismiss = {
        let controls = controls.clone();
        Callback::from(move |()| {
            if let Some((team, opener)) = open {
                let selector = match opener {
                    ArrangementOpener::Lights => ".team-lights[data-lane-drag-handle]",
                    ArrangementOpener::Menu => ".team-menu",
                };
                if let Some(trigger) = team_element(team, selector) {
                    crate::browser::overlay::return_focus(&trigger);
                }
            }
            controls.dispatch(TeamControlAction::CloseArrangement);
        })
    };
    crate::browser::overlay::use_escape_owner(visible && open.is_some(), dismiss);
}

fn team_element(team: CanonicalUuid, selector: &str) -> Option<web_sys::HtmlElement> {
    web_sys::window()?
        .document()?
        .query_selector(&format!(".team-surface[data-team=\"{team}\"] {selector}"))
        .ok()
        .flatten()?
        .dyn_into()
        .ok()
}

fn render_team_toolbar(
    team: &crate::runtime::LaneRow,
    standings: &[Standing],
    render: &FleetRender<'_, '_>,
) -> Html {
    let can_arrange = render.can_arrange_teams;
    let id = team.lane;
    let control = render.team_controls.current(team);
    let arrangement_context = arrangement::Context {
        controls: render.team_controls,
        sender: render.sender,
        topology: &render.topology,
        topology_revision: render.topology_revision,
        lanes: render.runtime.lanes().map_or(&[][..], |view| &view.lanes),
        can_arrange: render.can_arrange_teams,
    };
    let lights = if standings.is_empty() {
        Html::default()
    } else {
        let arrangement_help_id = format!("team-arrangement-help-{id}");
        let lights_label = if can_arrange {
            format!(
                "{} admitted agents; arrange team {}",
                standings.len(),
                watchband::team_label(team)
            )
        } else {
            format!("{} admitted agents", standings.len())
        };
        let toggle_arrangement = team_arrangement_click(id, can_arrange, render.team_controls);
        let keyboard_arrangement = team_arrangement_key(id, can_arrange, render.team_controls);
        let render_pip = |standing: &Standing| {
            let member = team.member(standing.lane)?;
            let status = member.status(standing.waking);
            let label = format!("{}: {}", standing.label, member.presence_label(status));
            Some(html! {
                <span
                    class={classes!(
                        "app-presence-pip",
                        "app-presence-pip--status",
                        status.key(),
                        member.parked.map(|_| "parked")
                    )}
                    role="img"
                    data-lane-light-target-id={member.lane.to_string()}
                    data-lane-status={status.key()}
                    data-lane-parked={member.parked.map(wire::body::LanePark::as_str)}
                    aria-label={label.clone()}
                    title={label}
                ></span>
            })
        };
        let dragging = team_dragging(
            id,
            can_arrange,
            render.topology_revision,
            &render.topology,
            render.sender,
            render.team_drag,
        );
        let lost_capture = dragging.cancel.clone();
        html! {
        <span
            class="team-lights"
            role={if can_arrange { "button" } else { "group" }}
            aria-label={lights_label}
            aria-haspopup={can_arrange.then_some("menu")}
            aria-expanded={can_arrange.then(|| control.arrangement.is_some().to_string())}
            aria-describedby={can_arrange.then(|| arrangement_help_id.clone())}
            tabindex={can_arrange.then_some("0")}
            title={can_arrange.then_some("Drag, or press Enter or Space to arrange this team")}
            data-lane-drag-handle={can_arrange.then_some("")}
            data-team-drag-id={can_arrange.then(|| team.lane.to_string())}
            data-team-size={standings.len().to_string()}
            onpointerdown={dragging.start}
            onpointermove={dragging.movement}
            onpointerup={dragging.finish}
            onpointercancel={dragging.cancel}
            onlostpointercapture={lost_capture}
            onclick={toggle_arrangement}
            onkeydown={keyboard_arrangement}
        >
            if can_arrange {
                <span id={arrangement_help_id} class="visually-hidden">
                    {"Press Enter or Space, or tap, to choose where to move or join this team."}
                </span>
            }
            {for standings.iter().filter_map(render_pip)}
        </span>
        }
    };
    html! {
        <>
        <header class="team-toolbar">
            {render_team_rail(team.lane, standings, render)}
            <div class="app-glass-control-surface app-status-band team-status-row">
                {view::chrome::render_team_ticker(render.runtime, team, standings)}
                {lights}
            </div>
        </header>
        {arrangement::render(team.lane, &control, &arrangement_context)}
        </>
    }
}

/// The fixed team total and active-task scroller remain independent of the
/// status row. The total holds the rail's right edge and the tasks flow
/// leftward from it in the composers' order, each lane's tabs together and in
/// their canonical order, so a tab stands over its own lane's composer.
fn render_team_rail(
    team: CanonicalUuid,
    standings: &[Standing],
    render: &FleetRender<'_, '_>,
) -> Html {
    let mut tasks = render
        .work
        .map(|work| work.scope_strip(&crate::work::WorkScope::Team { team }))
        .unwrap_or_default();
    tasks.sort_by_key(|task| {
        task.working_lane()
            .and_then(|lane| standings.iter().position(|standing| standing.lane == lane))
            .unwrap_or(standings.len())
    });
    html! {
        <div class="team-task-rail">
            if !tasks.is_empty() {
                {render_team_total(team, &tasks, render.navigate)}
                <nav class="app-horn-tabs team-tasks" aria-label="Tasks currently worked by this team">
                    {for tasks.iter().copied().map(|task| render_team_task_handle(task, render))}
                </nav>
            }
        </div>
    }
}

/// The trailing tab: only a badge counting every active task the rail shows,
/// opening the unified overview with exactly that current selection.
fn render_team_total(
    team: CanonicalUuid,
    tasks: &[&crate::work::WorkTask<'_>],
    navigate: &Callback<navigation::Intent>,
) -> Html {
    use crate::work::{WorkAvailability, WorkCounts, WorkScope};
    let counts = tasks.iter().copied().collect::<WorkCounts>();
    let claimed_drafts = tasks
        .iter()
        .filter(|task| task.availability() == WorkAvailability::Draft)
        .count();
    let title = view::chrome::team_task_count_title(
        "Open work active on this team",
        counts,
        claimed_drafts,
    );
    let navigate = navigate.clone();
    let open_team = Callback::from(move |_| {
        navigate.emit(navigation::Intent::Work(WorkScope::Team { team }));
    });
    html! {
        <span
            key="team-total"
            class="app-horn-tab app-horn-tab--up app-horn-tab--group"
            data-work-scope="team"
            data-work-mark={view::chrome::work_mark(counts)}
        >
            <button
                class="app-horn-tab__target"
                type="button"
                title={title.clone()}
                aria-label={title}
                data-sidebar-focus-key={format!("team-work:{team}:all")}
                onclick={open_team}
            >
                {view::chrome::render_task_count(tasks.len())}
            </button>
        </span>
    }
}

/// One compound task tab: the neutral handle target keeps the task-detail
/// action, then each holder's lane target opens its Chat composer. Team scope
/// admits only actively lane-claimed work, so every tab has a working lane.
/// Several holders read as a stack beside the handle: comma separated, each
/// in its own accent, latest claim first and the earliest claim last, at the
/// tab's edge; that first holder's accent is the tab's.
fn render_team_task_handle(task: &crate::work::WorkTask<'_>, render: &FleetRender<'_, '_>) -> Html {
    let statement = &task.row().statement.body;
    let availability = task.availability();
    let lanes = render.runtime.lanes();
    let holders = task.working_lanes().collect::<Vec<_>>();
    let lane_target = match holders.as_slice() {
        [] => unreachable!("team task strips contain only lane-claimed work"),
        [lane] => render_team_task_holder(
            lanes,
            *lane,
            "app-horn-tab__target team-task-tab__lane",
            None,
        ),
        several => html! {
            <span class="app-horn-tab__target team-task-tab__lane" data-holders={several.len().to_string()}>
                {for several.iter().rev().enumerate().map(|(index, lane)| html! {
                    <>
                        if index > 0 {
                            <span class="team-task-tab__holder-separator">{", "}</span>
                        }
                        {render_team_task_holder(
                            lanes,
                            *lane,
                            "team-task-tab__holder",
                            lanes.and_then(|lanes| crate::watchband::lane_accent(&lanes.lanes, *lane)),
                        )}
                    </>
                })}
            </span>
        },
    };
    let original_claim = task.row().original_claimed_at.as_ref();
    let original_claim_times = original_claim.map(|at| {
        let local = local_full(at);
        (
            format!(", originally claimed at {local}"),
            AttrValue::from(format!("Original claim won at {local}")),
        )
    });
    let handle_label = format!(
        "Open {}: {}, {}{}",
        statement.handle,
        crate::composer_markdown::inline::plain_text(task.row().presentation_title()),
        availability.key(),
        original_claim_times
            .as_ref()
            .map_or("", |(label, _)| label.as_str()),
    );
    let focus_key = format!("team-task:{}", statement.born);
    let open_task = {
        let select = render.select_task.clone();
        let born = statement.born.clone();
        Callback::from(move |_| select.emit(born.clone()))
    };
    html! {
        <span
            key={statement.born.to_string()}
            class={classes!(
                "app-horn-tab",
                "app-horn-tab--up",
                "app-horn-tab--group",
                "app-horn-tab__handle",
                "team-task-tab",
            )}
            data-availability={availability.key()}
            data-accent={task.working_claimant_accent(lanes).map(|accent| accent.to_string())}
        >
            <button
                class="app-horn-tab__target team-task-tab__task"
                type="button"
                title={handle_label.clone()}
                aria-label={handle_label}
                data-sidebar-focus-key={focus_key}
                onclick={open_task}
            >
                if let Some(claimed) = original_claim {
                    <Age
                        authored={claimed.get().as_millisecond()}
                        class="team-task-tab__age"
                        title={original_claim_times.as_ref().expect("a claimed task has local claim text").1.clone()}
                    />
                }
                {statement.handle.to_string()}
            </button>
            {lane_target}
        </span>
    }
}

/// One holder's lane target: a link to its Chat composer when it has one.
fn render_team_task_holder(
    lanes: Option<&crate::runtime::LaneView>,
    lane: CanonicalUuid,
    class: &'static str,
    accent: Option<usize>,
) -> Html {
    let member = lanes.and_then(|lanes| lanes.member(lane));
    let label = crate::work::owner_label(&wire::Actor::Lane(lane), member, None);
    let lane_text = lane.to_string();
    let accent = accent.map(|accent| accent.to_string());
    if let Some(href) = super::lane_composer_href(lanes, lane) {
        let title = format!("Open {label} in Chat");
        html! {
            <a
                {class}
                title={title.clone()}
                aria-label={title}
                data-lane={lane_text}
                data-accent={accent}
                data-sidebar-focus-key={format!("team-lane:{lane}")}
                {href}
            >{label}</a>
        }
    } else {
        html! {
            <span {class} title={label.clone()} data-lane={lane_text} data-accent={accent}>{label}</span>
        }
    }
}

fn team_arrangement_click(
    team: CanonicalUuid,
    can_arrange: bool,
    controls: &UseReducerHandle<TeamControls>,
) -> Callback<MouseEvent> {
    let controls = controls.clone();
    Callback::from(move |event: MouseEvent| {
        event.stop_propagation();
        if can_arrange {
            controls.dispatch(TeamControlAction::ToggleArrangement {
                team,
                opener: ArrangementOpener::Lights,
            });
        }
    })
}

fn team_arrangement_key(
    team: CanonicalUuid,
    can_arrange: bool,
    controls: &UseReducerHandle<TeamControls>,
) -> Callback<KeyboardEvent> {
    let controls = controls.clone();
    Callback::from(move |event: KeyboardEvent| {
        if !can_arrange || !matches!(event.key().as_str(), "Enter" | " ") {
            return;
        }
        event.prevent_default();
        event.stop_propagation();
        controls.dispatch(TeamControlAction::ToggleArrangement {
            team,
            opener: ArrangementOpener::Lights,
        });
    })
}
