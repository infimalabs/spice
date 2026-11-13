//! Spice sidebar content and the team roster.

use super::*;
use crate::browser::roster::{self, RosterView, Wording};
use crate::watchband;

const TEAM_WORDING: Wording = Wording {
    kind: "team",
    ungrouped_title: "Unassigned Agents",
    new_group_title: "Create new team",
    ungrouped_hint: "drop here to unassign",
    full: "Teams are limited to six agents",
    drop_base: None,
};

pub(super) struct Context<'a> {
    pub(super) sender: &'a SenderHandle,
    pub(super) team_controls: &'a UseReducerHandle<TeamControls>,
}

/// The team roster: each team a card named for its member zero, its agents as
/// full-width status slots, then Unassigned Agents.
pub(super) fn render_arrangement(
    runtime: Option<&AppRuntime>,
    context: &Context<'_>,
    standings: &[watchband::Standing],
) -> Html {
    let lanes = runtime.and_then(AppRuntime::lanes);
    let awaited = match (runtime, lanes) {
        (None, _) => Some("Waiting for resident teams…"),
        (Some(_), None) => Some("Waiting for agent state…"),
        (Some(_), Some(_)) => None,
    };
    let enabled = lanes.is_some()
        && runtime.is_some_and(|runtime| {
            runtime.offers(CommandKind::TeamConfigure) && runtime.offers(CommandKind::TeamArrange)
        });
    let topology_revision = lanes.map_or(0, |lanes| lanes.topology_revision);
    let teams = lanes.map_or(&[][..], |lanes| lanes.lanes.as_slice());
    let sender = context.sender.clone();
    let controls = context.team_controls.clone();
    let topology = Rc::new(watchband::topology(teams));
    let head = |team: &CanonicalUuid, members: &[CanonicalUuid]| {
        let name = teams
            .iter()
            .find(|row| row.lane == *team)
            .map_or_else(|| watchband::short(*team), super::team_name);
        html! {
            <div>
                <strong>{name}</strong>
                <span>{super::topology_count(members.len(), "agent", "agents")}</span>
            </div>
        }
    };
    let base = |team: &CanonicalUuid| {
        teams
            .iter()
            .find(|row| row.lane == *team)
            .map(|team| team.accent(0))
    };
    let member = |lane: &CanonicalUuid, _grouped: bool| {
        let member = teams.iter().find_map(|row| row.member(*lane));
        let standing = standings.iter().find(|standing| standing.lane == *lane);
        match (member, standing) {
            (Some(member), Some(standing)) => super::render_topology_agent(member, standing),
            _ => Html::default(),
        }
    };
    roster::render(RosterView {
        wording: &TEAM_WORDING,
        roster: Rc::new(crate::roster::teams(&topology)),
        revision: topology_revision,
        enabled,
        empty: awaited.unwrap_or("No unassigned agents"),
        submit: Rc::new(move |change| {
            super::super::watchband::team_action(
                change,
                topology_revision,
                &topology,
                &controls,
                &sender,
            );
        }),
        head: &head,
        base: &base,
        member: &member,
    })
}

/// The resident summary the open view's header carries beside its kicker:
/// resident state and its current admitted journal cursor over the product
/// version, which the kicker beside it already names.
pub(super) fn render_resident_summary(
    connection: &Connection,
    cursor: Option<wire::Seq>,
    identity: Option<&AppIdentity>,
    update: Option<&AppUpdate>,
) -> Html {
    // Starting and reconnecting retain the old runtime on screen, so freshness
    // comes from the connection before any retained cursor is presented.
    let cursor =
        crate::browser_branding::JournalCursor::resolve(connection.branding_state(), cursor);
    let (status, status_details) = match connection {
        Connection::Starting => ("Starting Spice", "Starting Spice".to_owned()),
        Connection::Connecting => (
            "Opening current resident",
            "Opening current resident".to_owned(),
        ),
        Connection::Online { .. } => (
            "Connected",
            format!(
                "Connected; {}",
                cursor
                    .as_ref()
                    .expect("an online connection presents its journal cursor")
                    .accessible
            ),
        ),
        Connection::Offline(reason) => ("Reconnecting", format!("Reconnecting / {reason}")),
    };
    html! {
        <div
            class="app-resident-summary"
            data-connection={connection.indicator_state()}
            data-journal-cursor={cursor.as_ref().and_then(|cursor| cursor.value.clone())}
        >
            <span
                class="app-resident-status"
                role="status"
                aria-label={status_details.clone()}
                title={status_details}
            >
                {status}
            </span>
            {render_product_version(identity, update)}
        </div>
    }
}

/// Global TURBO, between the theme selector and the Spice button in the open
/// view's header.
pub(super) fn render_global_turbo(runtime: Option<&AppRuntime>, sender: &SenderHandle) -> Html {
    let settings = runtime.and_then(AppRuntime::settings);
    let fast_mode = settings.is_some_and(|settings| settings.fast_mode);
    let enabled = settings.is_some()
        && runtime.is_some_and(|runtime| runtime.offers(CommandKind::GlobalSettingsSet));
    let sender = sender.clone();
    let onclick = Callback::from(move |event: MouseEvent| {
        if enabled
            && admit_action(
                &sender,
                ClientAction::GlobalSettings(GlobalSettingsSet {
                    fast_mode: !fast_mode,
                }),
            )
        {
            event.stop_propagation();
        }
    });
    let title = match (settings.is_some(), enabled, fast_mode) {
        (false, _, _) => "Fast mode is loading",
        (true, false, _) => "Fast mode cannot be changed by this resident",
        (true, true, true) => "Turbo: every agent runs in fast mode",
        (true, true, false) => "Turbo off: press to run every agent in fast mode",
    };
    html! {
        <chrome::TurboButton
            class={classes!("app-spice-turbo")}
            engaged={fast_mode}
            label={format!(
                "Global fast mode: {}",
                if fast_mode { "on" } else { "off" },
            )}
            disabled={!enabled}
            title={title}
            onclick={onclick}
        />
    }
}

/// The cumulative plan-weighted weekly quota remaining across accounts,
/// hanging under the open sidebar's Spice button. Its fixed-width fallback
/// keeps the button still while quota state loads.
pub(super) fn render_capacity(runtime: Option<&AppRuntime>) -> Html {
    let generation = runtime.and_then(AppRuntime::quota);
    let remaining = generation
        .and_then(|generation| generation.last_good.as_deref())
        .and_then(crate::quota::weekly_remaining);
    let stale = generation.is_some_and(crate::quota::last_good_stale);
    let (value, title, state) = if let Some(remaining) = remaining {
        let percentage = format!("{:.0}%", remaining * 100.0);
        let title = if stale {
            format!(
                "Weekly quota {percentage} remaining across accounts ({})",
                crate::quota::Condition::StaleLastGood.label()
            )
        } else {
            format!("Weekly quota {percentage} remaining across accounts")
        };
        (percentage, title, "ready")
    } else if runtime.is_none() {
        (
            "…".to_owned(),
            "Waiting for weekly quota".to_owned(),
            "loading",
        )
    } else {
        (
            "—".to_owned(),
            "Weekly quota is unavailable".to_owned(),
            "unavailable",
        )
    };
    html! {
        <span
            class="app-spice-capacity"
            data-state={state}
            data-quota-stale={stale.then_some("true")}
            aria-label={title.clone()}
            {title}
        >{value}</span>
    }
}

fn render_product_version(identity: Option<&AppIdentity>, update: Option<&AppUpdate>) -> Html {
    let IdentityPresentation {
        label,
        details,
        accessible,
    } = identity_presentation(identity);
    let product_version = identity.map(|identity| identity.version.product.to_string());
    let native_binary = identity.map(|identity| identity.version.binary.to_string());
    let wire_schema = identity.map(|identity| identity.version.wire.to_string());
    let journal_schema = identity.map(|identity| identity.version.journal.to_string());
    let app_protocol = identity.map(|identity| identity.version.app_protocol.to_string());
    let loaded_app = identity.map(|identity| identity.loaded_app.to_string());
    let served_app = identity.map(|identity| identity.version.app.to_string());
    let reload = Callback::from(move |_| {
        web_sys::window()
            .expect("app update action has a browser window")
            .location()
            .reload()
            .expect("app update action reloads the current document");
    });
    if let Some(update) = update {
        let update_identity = crate::version_identity::update_rows(
            identity.map(|identity| &identity.version),
            &update.loaded_generation,
            update.version,
            &update.generation,
        )
        .join("\n");
        let update_details = format!("{update_identity}\nAction: Reload to update");
        let update_accessible = format!(
            "Reload Spice to update; {}",
            update_identity.replace('\n', "; ")
        );
        html! {
            <button
                class="app-product-version app-product-version--update"
                type="button"
                aria-label={update_accessible}
                title={update_details}
                data-product-version={product_version}
                data-native-binary={native_binary}
                data-wire-schema={wire_schema}
                data-journal-schema={journal_schema}
                data-app-protocol={update.version.to_string()}
                data-loaded-app-generation={update.loaded_generation.to_string()}
                data-served-app-generation={update.generation.to_string()}
                data-app-update-version={update.version.to_string()}
                data-app-update-generation={update.generation.to_string()}
                onclick={reload}
            >
                <span class="app-product-version-label">{label}</span>
                <span class="app-product-version-indicator" aria-hidden="true">{"↻"}</span>
            </button>
        }
    } else {
        html! {
            <strong
                class="app-product-version"
                tabindex="0"
                aria-label={accessible}
                title={details}
                data-product-version={product_version}
                data-native-binary={native_binary}
                data-wire-schema={wire_schema}
                data-journal-schema={journal_schema}
                data-app-protocol={app_protocol}
                data-loaded-app-generation={loaded_app}
                data-served-app-generation={served_app}
            >{label}</strong>
        }
    }
}

struct IdentityPresentation {
    label: String,
    details: Option<String>,
    accessible: String,
}

fn identity_presentation(identity: Option<&AppIdentity>) -> IdentityPresentation {
    let Some(identity) = identity else {
        return IdentityPresentation {
            label: "…".to_owned(),
            details: None,
            accessible: "Spice product identity is loading".to_owned(),
        };
    };
    let details =
        crate::version_identity::identity_rows(&identity.version, &identity.loaded_app).join("\n");
    IdentityPresentation {
        label: identity.version.product.to_string(),
        accessible: details.replace('\n', "; "),
        details: Some(details),
    }
}

/// The one theme selector, beside the open view's Spice button.
pub(super) fn render_theme_selector() -> Html {
    let selected = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.document_element())
        .and_then(|root| root.get_attribute("data-app-theme"))
        .unwrap_or_else(|| "system".to_owned());
    html! {
            <div class="app-control-rail app-segment-slider app-theme-selector" role="group" aria-label="Theme">
                {for [("light", "Light"), ("system", "System"), ("dark", "Dark")]
                    .into_iter()
                    .map(|(mode, label)| {
                        let choose = Callback::from(move |_| {
                            let Some(window) = web_sys::window() else { return; };
                            let Ok(setter) = js_sys::Reflect::get(
                                &window,
                                &JsValue::from_str("__spiceAppSetTheme"),
                            ) else { return; };
                            let Ok(setter) = setter.dyn_into::<js_sys::Function>() else { return; };
                            let _ = setter.call1(&JsValue::NULL, &JsValue::from_str(mode));
                        });
                        let icon = match mode {
                            "light" => html! {
                                <svg class="app-theme-icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
                                    <circle cx="12" cy="12" r="3.25"></circle>
                                    <path d="M12 2.5v2M12 19.5v2M2.5 12h2M19.5 12h2M5.28 5.28 6.7 6.7M17.3 17.3l1.42 1.42M18.72 5.28 17.3 6.7M6.7 17.3l-1.42 1.42"></path>
                                </svg>
                            },
                            "system" => html! {
                                <svg class="app-theme-icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
                                    <rect x="3.25" y="4.5" width="17.5" height="13" rx="1.8"></rect>
                                    <path d="M8.5 21h7M12 17.5V21"></path>
                                </svg>
                            },
                            "dark" => html! {
                                <svg class="app-theme-icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
                                    <path d="M20.2 15.3A8.7 8.7 0 0 1 8.7 3.8a8.7 8.7 0 1 0 11.5 11.5Z"></path>
                                </svg>
                            },
                            _ => unreachable!("app theme modes are closed"),
                        };
                        let accessible = format!("Use {label} theme");
                        html! {
                            <button
                                class="app-choice-chip"
                                type="button"
                                aria-label={accessible}
                                title={format!("{label} theme")}
                                aria-pressed={(selected == mode).to_string()}
                                data-app-theme={mode}
                                onclick={choose}
                            >{icon}</button>
                        }
                    })}
            </div>
    }
}
