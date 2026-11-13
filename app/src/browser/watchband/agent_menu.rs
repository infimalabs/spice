//! One agent's lane menu: opening it swaps the lane's composer for a compact
//! panel carrying every per-agent control and fact for that seat.
//!
//! App menus replace the surface they belong to rather than float over
//! it, so the panel takes over the shard below its heading, scrolls within
//! it, and leaves the menu button in place to close it again. Escape and a
//! click elsewhere in the team also close it.

use super::*;
use crate::runtime::MemberCommand;

/// The newest retained activity for one member.
pub(super) struct LatestActivity {
    /// Card the row links to.
    pub(super) target: String,
    pub(super) authored: i64,
    pub(super) class: String,
    pub(super) prefix: String,
    pub(super) detail: String,
}

/// The member's newest activity card, or its newest prose when no activity
/// card is retained, on one row that CSS clips to its width.
pub(super) fn latest_activity(runtime: &AppRuntime, standing: &Standing) -> Option<LatestActivity> {
    let row = runtime
        .newest_lane_activity(standing.lane)
        .and_then(|activity| {
            runtime.activity_message(activity).map(|message| {
                let class = activity.class().to_owned();
                let prefix = if class == "prose" {
                    standing.label.clone()
                } else {
                    activity.title().to_owned()
                };
                let detail = if activity.detail().is_empty() && activity.title() == "tool" {
                    prefix.clone()
                } else {
                    activity.detail().to_owned()
                };
                (
                    agent_message_source_id(message),
                    activity.display_millis(),
                    class,
                    prefix,
                    detail,
                )
            })
        })
        .or_else(|| {
            runtime
                .latest_lane_prose(standing.lane)
                .map(|(message, prose)| {
                    (
                        agent_message_source_id(message),
                        message.display_millis(),
                        "prose".to_owned(),
                        standing.label.clone(),
                        prose,
                    )
                })
        });
    row.map(|(target, authored, class, visible_prefix, source)| {
        let (prefix, detail) =
            watchband::ticker_chunks(&visible_prefix, &source, watchband::STATUS_LINE_DOM_LIMIT);
        let separator = if class == "prose" || class == "tool" || detail.is_empty() {
            ""
        } else {
            ":"
        };
        LatestActivity {
            target,
            authored,
            class,
            prefix: format!("{prefix}{separator}"),
            detail,
        }
    })
}

/// One line previewing the newest waiting direction, when any waits.
pub(super) fn waiting_direction(standing: &Standing) -> Option<String> {
    (standing.pending > 0).then(|| {
        standing
            .headline
            .as_deref()
            .map(crate::composer_markdown::inline::plain_text)
            .map(|headline| watchband::truncate(&headline, watchband::STATUS_LINE_DOM_LIMIT))
            .unwrap_or_else(|| "Preview unavailable".to_owned())
    })
}

/// Everything one lane menu draws from.
pub(super) struct AgentMenu<'a> {
    pub(super) runtime: &'a AppRuntime,
    pub(super) member: &'a crate::runtime::LaneMember,
    pub(super) standing: &'a Standing,
    pub(super) select: &'a Callback<wire::TaskStamp>,
    pub(super) command: &'a Callback<(CanonicalUuid, MemberCommand)>,
    pub(super) can_configure: bool,
    pub(super) topology_revision: u64,
    pub(super) topology: &'a Rc<Vec<(CanonicalUuid, Vec<CanonicalUuid>)>>,
    pub(super) sender: &'a SenderHandle,
    pub(super) team_controls: &'a UseReducerHandle<TeamControls>,
    /// This member's due attention items with their controls.
    pub(super) attention: Html,
}

fn element_target(target: Option<web_sys::EventTarget>) -> Option<web_sys::Element> {
    target.and_then(|target| target.dyn_into::<web_sys::Element>().ok())
}

/// Close the menu holding the clicked control.
pub(super) fn close(event: &MouseEvent) {
    // Yew delegates listeners from the app root, so `current_target` cannot
    // locate the details element. The actual click target remains inside it.
    if let Some(menu) = element_target(event.target())
        .and_then(|target| target.closest(".agent-menu").ok().flatten())
    {
        let _removed = menu.remove_attribute("open");
    }
}

/// Close every open lane menu except one the click landed in.
pub(super) fn dismiss_others(event: MouseEvent) {
    let keep = element_target(event.target())
        .and_then(|target| target.closest(".agent-menu").ok().flatten());
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let Ok(menus) = document.query_selector_all(".agent-menu[open]") else {
        return;
    };
    for index in 0..menus.length() {
        let Some(menu) = menus
            .get(index)
            .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
        else {
            continue;
        };
        if keep.as_ref() != Some(&menu) {
            let _removed = menu.remove_attribute("open");
        }
    }
}

/// The head's state and age on one line. The handle a working agent holds is
/// the Task row's value, so the head names only the state.
fn render_presence(
    runtime: &AppRuntime,
    member: &crate::runtime::LaneMember,
    standing: &Standing,
) -> Html {
    let presence = watchband::member_presence(runtime, member, standing);
    html! {
        <span class="agent-menu-presence" data-agent-presence={presence.state.key()}>
            <span class="team-presence-state">{presence.state.label()}</span>
            if let Some(authored) = presence.since_millis {
                {" · "}
                <Age {authored} class={classes!("team-presence-age")} />
            }
        </span>
    }
}

fn render_task(standing: &Standing, select: &Callback<wire::TaskStamp>) -> Html {
    let Some(handle) = standing.handle.as_deref() else {
        return html! {
            <li class="agent-menu-row" data-menu-row="task" data-empty="true">
                <span class="agent-menu-key">{"Task"}</span>
                <span class="agent-menu-value">{"No task held"}</span>
            </li>
        };
    };
    let Some(task) = watchband::canonical_task_handle(handle).and_then(|handle| handle.born().ok())
    else {
        return html! {
            <li class="agent-menu-row" data-menu-row="task" data-empty="true">
                <span class="agent-menu-key">{"Task"}</span>
                <span class="agent-menu-value">{format!("{handle} (unavailable)")}</span>
            </li>
        };
    };
    let focus_key = format!("agent-task:{handle}");
    let open = {
        let select = select.clone();
        Callback::from(move |event: MouseEvent| {
            close(&event);
            select.emit(task.clone());
        })
    };
    let title = standing.task_title.as_deref().unwrap_or("Current task");
    let label = crate::composer_markdown::inline::plain_text(title);
    let phase = standing.phase.as_deref().unwrap_or("active");
    html! {
        <li class="agent-menu-row" data-menu-row="task">
            <span class="agent-menu-key">{"Task"}</span>
            <button
                type="button"
                class="agent-menu-value agent-menu-link"
                data-task-handle={handle.to_owned()}
                data-sidebar-focus-key={focus_key}
                aria-label={format!("Open task {handle}: {label}")}
                title={label}
                onclick={open}
            >
                <strong>{handle.to_owned()}</strong>
                {format!(" · {phase} · ")}
                {crate::browser::task_detail::render_task_title(
                    title,
                    &crate::composer_markdown::inline::LineControls::Nested,
                )}
            </button>
            // The head already carries the held age; the row keeps the lease's
            // remaining time.
            if let Some(expiry) = standing.expires_at {
                <span class="agent-menu-meta">
                    <Countdown
                        {expiry}
                        class={classes!("agent-menu-countdown")}
                        title={format!("Lease expiry for {handle}")}
                    />
                </span>
            }
        </li>
    }
}

fn render_activity(runtime: &AppRuntime, standing: &Standing) -> Html {
    let Some(activity) = latest_activity(runtime, standing) else {
        return html! {
            <li class="agent-menu-row" data-menu-row="activity" data-empty="true">
                <span class="agent-menu-key">{"Latest"}</span>
                <span class="agent-menu-value">{"No retained activity"}</span>
            </li>
        };
    };
    let full = if activity.detail.is_empty() {
        activity.prefix.clone()
    } else {
        format!("{} {}", activity.prefix, activity.detail)
    };
    html! {
        <li class="agent-menu-row" data-menu-row="activity" data-activity-class={activity.class}>
            <span class="agent-menu-key">{"Latest"}</span>
            <a
                class="agent-menu-value agent-menu-link"
                href={crate::view::card_href(&activity.target)}
                aria-label={format!("Jump to latest {} activity", standing.label)}
                title={full}
                onclick={Callback::from(|event: MouseEvent| close(&event))}
            >
                <strong>{activity.prefix}</strong>
                if !activity.detail.is_empty() {
                    {" "}
                    {activity.detail}
                }
            </a>
            <span class="agent-menu-meta">
                <Age authored={activity.authored} class={classes!("agent-menu-age")} />
            </span>
        </li>
    }
}

fn render_direction(standing: &Standing) -> Html {
    let Some(preview) = waiting_direction(standing) else {
        return html! {
            <li class="agent-menu-row" data-menu-row="direction" data-empty="true">
                <span class="agent-menu-key">{"Waiting"}</span>
                <span class="agent-menu-value">{"No pending direction"}</span>
            </li>
        };
    };
    html! {
        <li class="agent-menu-row" data-menu-row="direction" data-pending={standing.pending.to_string()}>
            <span class="agent-menu-key">{"Waiting"}</span>
            <a
                class="agent-menu-value agent-menu-link"
                href={crate::view::card_href(&primary_textarea_id(&standing.lane.to_string()))}
                aria-label={format!("Open {}'s composer", standing.label)}
                title={preview.clone()}
                onclick={Callback::from(|event: MouseEvent| close(&event))}
            >{preview}</a>
            <span class="agent-menu-meta">{format!("{} pending", standing.pending)}</span>
        </li>
    }
}

/// One identity or configuration fact; identities copy on click.
fn render_fact(key: &'static str, value: String, copy: Option<&'static str>) -> Html {
    html! {
        <div class="agent-menu-fact">
            <dt>{key}</dt>
            <dd>
                if let Some(subject) = copy {
                    <view::chrome::CopyButton
                        class="agent-menu-copy"
                        text={AttrValue::from(value.clone())}
                        subject={subject}
                        title={format!("Copy {subject}")}
                        label={AttrValue::from(format!("Copy {subject} {value}"))}
                    ><code>{value}</code></view::chrome::CopyButton>
                } else {
                    <code>{value}</code>
                }
            </dd>
        </div>
    }
}

fn render_facts(member: &crate::runtime::LaneMember, standing: &Standing) -> Html {
    let occupant = member.occupant.map(|value| value.to_string());
    html! {
        <dl class="agent-menu-facts">
            {render_fact("Lane", standing.lane.to_string(), Some("lane"))}
            {match occupant {
                Some(occupant) => render_fact("Occupant", occupant, Some("occupant")),
                None => render_fact("Occupant", "vacant".to_owned(), None),
            }}
            {match standing.session.clone() {
                Some(session) => render_fact("Session", session, Some("session")),
                None => render_fact("Session", "unbound".to_owned(), None),
            }}
            {render_fact("Worktree", standing.worktree.clone(), Some("worktree"))}
            {render_fact(
                "Model",
                format!(
                    "{} / {}",
                    watchband::model_display(standing.model.as_deref()),
                    standing.life
                ),
                None,
            )}
            {for standing.jobs.iter().map(|job| render_fact(
                "Job",
                format!("{} ({}) {}", job.id, job.standing.as_str(), job.command),
                None,
            ))}
        </dl>
    }
}

fn render_team_actions(
    standing: &Standing,
    leave: AgentTeamControl,
    new_group: AgentTeamControl,
) -> Html {
    html! {
        <div class="app-control-rail" role="group" aria-label="Team">
            <button
                class="agent-menu-action"
                type="button"
                disabled={new_group.disabled}
                aria-label={format!("Create own team for {}", standing.label)}
                title={format!("Move {} into a new team", standing.label)}
                data-team-new-lane={standing.lane.to_string()}
                onclick={new_group.activate}
            >{"Create own team"}</button>
            <button
                class="agent-menu-action"
                type="button"
                disabled={leave.disabled}
                aria-label={format!("Leave all teams for {}", standing.label)}
                title={format!("Remove {} from all teams", standing.label)}
                data-team-leave-lane={standing.lane.to_string()}
                onclick={leave.activate}
            >{"Leave all teams"}</button>
        </div>
    }
}

/// The menu button and its panel.
pub(super) fn render(menu: AgentMenu<'_>) -> Html {
    let AgentMenu {
        runtime,
        member,
        standing,
        select,
        command,
        can_configure,
        topology_revision,
        topology,
        sender,
        team_controls,
        attention,
    } = menu;
    let menu = crate::roster::teams(topology).menu(&member.lane);
    let leave = agent_team_control(
        menu.leave,
        can_configure,
        topology_revision,
        sender,
        topology,
        team_controls,
    );
    let new_group = agent_team_control(
        menu.new_group,
        can_configure,
        topology_revision,
        sender,
        topology,
        team_controls,
    );
    let renewal = watchband::agent_renewal_control(
        &standing.label,
        runtime
            .lanes()
            .and_then(|lanes| lanes.renewal(standing.team, member.lane)),
    );
    let label = if renewal.pressed {
        format!("Menu for {} (renewal requested)", standing.label)
    } else {
        format!("Menu for {}", standing.label)
    };
    let provider = super::provider_emblem::provider_title(standing);
    let title = if renewal.pressed {
        format!("{provider}\n{}", renewal.detail)
    } else {
        provider
    };
    html! {
        <details
            class="agent-menu"
            data-app-overlay-disclosure=""
            data-renewal-pending={renewal.pressed.to_string()}
            onkeydown={Callback::from(crate::browser::overlay::dismiss_disclosure_on_escape)}
        >
            <summary class="agent-menu-button" aria-label={label} {title}>
                {super::provider_emblem::render_provider_mark(standing, "menu")}
            </summary>
            <div
                class="agent-menu-panel app-overlay-panel app-overlay-panel--seated"
                role="group"
                aria-label={format!("{} menu", standing.label)}
                data-agent-menu-lane={member.lane.to_string()}
            >
                <header class="agent-menu-head">
                    <strong>{standing.label.clone()}</strong>
                    {render_presence(runtime, member, standing)}
                </header>
                {attention}
                {super::member_controls::render(runtime, member, standing, command)}
                <ul class="agent-menu-rows">
                    {render_task(standing, select)}
                    {render_activity(runtime, standing)}
                    {render_direction(standing)}
                </ul>
                {render_facts(member, standing)}
                {render_team_actions(standing, leave, new_group)}
            </div>
        </details>
    }
}
