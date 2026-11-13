//! The arrangement panel a team's pips open: swap with or join another team.

use super::controls::TeamControl;
use super::*;

pub(super) struct Context<'a> {
    pub(super) controls: &'a UseReducerHandle<TeamControls>,
    pub(super) sender: &'a SenderHandle,
    pub(super) topology: &'a [(CanonicalUuid, Vec<CanonicalUuid>)],
    pub(super) topology_revision: u64,
    /// Current team rows, which name each team by its lead agent.
    pub(super) lanes: &'a [crate::runtime::LaneRow],
    pub(super) can_arrange: bool,
}

pub(super) fn render(team: CanonicalUuid, control: &TeamControl, context: &Context<'_>) -> Html {
    if control.arrangement.is_some() {
        render_arrangement(team, context)
    } else {
        Html::default()
    }
}

fn render_arrangement(team: CanonicalUuid, context: &Context<'_>) -> Html {
    let choices = watchband::teams(context.topology)
        .filter(|target| *target != team)
        .flat_map(|target| {
            [
                ("swap", "Swap positions", watchband::TeamDrop::Swap),
                ("join", "Join into", watchband::TeamDrop::Join),
            ]
            .into_iter()
            .map(|(kind, verb, drop)| {
                let teams = watchband::arrangement(context.topology, team, target, drop)
                    .map(|rows| topology(&rows));
                (kind, verb, target, teams)
            })
            .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let disbanded = watchband::disband(context.topology, team).map(|rows| topology(&rows));
    let disband = {
        let sender = context.sender.clone();
        let controls = context.controls.clone();
        let topology_revision = context.topology_revision;
        let refusals = sender.refusals.clone();
        let disbanded = disbanded.clone();
        Callback::from(move |_| {
            let Some(teams) = disbanded.clone() else {
                return;
            };
            let refusals = refusals.clone();
            if admit_action(
                &sender,
                ClientAction::ArrangeTeams {
                    topology_revision,
                    teams,
                    respond: Some(Rc::new(move |result| {
                        refusals.settled(match result {
                            Ok(()) => "Team disbanded.".to_owned(),
                            Err(error) => error,
                        });
                    })),
                },
            ) {
                controls.dispatch(TeamControlAction::CloseArrangement);
            }
        })
    };
    html! {
        <div
            class="team-action-menu team-arrangement-menu app-overlay-panel app-overlay-panel--popover"
            role="menu"
            aria-label={format!("Arrange team {}", watchband::team_label_in(context.lanes, team))}
        >
            {for choices.into_iter().map(|(kind, verb, target, teams)| {
                let sender = context.sender.clone();
                let controls = context.controls.clone();
                let topology_revision = context.topology_revision;
                let enabled = context.can_arrange && teams.is_some();
                html! {
                    <button
                        type="button"
                        class="team-action team-arrangement-action"
                        role="menuitem"
                        disabled={!enabled}
                        data-team-arrangement={kind}
                        data-team-arrangement-target={target.to_string()}
                        onclick={Callback::from(move |_| {
                            let Some(teams) = teams.clone() else { return; };
                            if !admit_action(
                                &sender,
                                ClientAction::ArrangeTeams {
                                    topology_revision,
                                    teams,
                                    respond: None,
                                },
                            ) {
                                return;
                            }
                            controls.dispatch(TeamControlAction::CloseArrangement);
                        })}
                    >
                        <span class="team-action-label">{format!("{verb} team {}", watchband::team_label_in(context.lanes, target))}</span>
                        <span class="team-action-detail">{
                            if kind == "join" && !enabled {
                                "limited to six agents"
                            } else if kind == "join" {
                                "combine agents"
                            } else {
                                "exchange team positions"
                            }
                        }</span>
                    </button>
                }
            })}
            {hide_action(team, context.controls)}
            <button
                type="button"
                class="team-action team-arrangement-action"
                role="menuitem"
                disabled={!context.can_arrange || disbanded.is_none()}
                data-team-arrangement="disband"
                onclick={disband}
            >
                <span class="team-action-label">{
                    format!("Disband team {}", watchband::team_label_in(context.lanes, team))
                }</span>
                <span class="team-action-detail">{"leave every agent unassigned"}</span>
            </button>
        </div>
    }
}

fn hide_action(team: CanonicalUuid, controls: &UseReducerHandle<TeamControls>) -> Html {
    let controls = controls.clone();
    html! {
        <button
            type="button"
            class="team-action team-arrangement-action"
            role="menuitem"
            data-team-arrangement="hide"
            onclick={Callback::from(move |_| {
                controls.dispatch(TeamControlAction::HideTeam { team });
            })}
        >
            <span class="team-action-label">{"Hide team"}</span>
            <span class="team-action-detail">{"the team stays available from its agent lanes"}</span>
        </button>
    }
}

fn topology(rows: &[(CanonicalUuid, Vec<CanonicalUuid>)]) -> Vec<TeamArrangement> {
    rows.iter()
        .map(|(team, lanes)| TeamArrangement {
            team: *team,
            lanes: lanes.clone(),
        })
        .collect()
}
