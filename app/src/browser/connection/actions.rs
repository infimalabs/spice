//! Client-action dispatch and topology mutation commands.

use super::*;

pub(super) async fn dispatch_client_action(
    principal: &Principal,
    data: &SurfaceData,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    handles: &SurfaceHandles,
    mut lease: ActionLease,
) -> Result<(), String> {
    if lease.action().is_topology_mutation() {
        let topology_revision = data
            .read()
            .as_ref()
            .and_then(AppRuntime::lanes)
            .map(|lanes| lanes.topology_revision)
            .ok_or_else(|| "topology action has no authoritative lane table".to_owned())?;
        let id = lease.prepare_topology(topology_revision)?;
        if actions.pending_contains(id) {
            return Err(format!("topology command identity {id} is already pending"));
        }
        send_topology_as(principal, writer, id, lease.action()).await?;
        lease.submit_topology();
        Ok(())
    } else if let ClientAction::ConsentRuling { request, answer } = *lease.action() {
        let reserved = data
            .write()
            .as_mut()
            .and_then(|runtime| runtime.begin_consent_ruling(request, answer));
        let Some((ruling, expected)) = reserved else {
            drop(lease.commit());
            return Ok(());
        };
        match refresh::send_consent_ruling(principal, writer, actions, &ruling, &expected).await {
            Ok(id) => {
                lease.commit_pending(id);
                Ok(())
            }
            Err(error) => {
                if let Some(runtime) = data.write().as_mut() {
                    runtime.refuse_consent_ruling(request, error.clone());
                }
                drop(lease.commit());
                Err(error)
            }
        }
    } else if matches!(lease.action(), ClientAction::TrustDecision { .. }) {
        dispatch_trust_decision(principal, data, writer, actions, handles, lease).await
    } else if matches!(lease.action(), ClientAction::DrainSubmissions) {
        drop(lease.commit());
        drain_submissions(principal, writer, actions, &handles.submissions).await
    } else if matches!(lease.action(), ClientAction::TaskDetails) {
        drop(lease.commit());
        refresh::send_task_detail_reads(principal, writer, actions, data).await?;
        refresh::send_contribution_reads(principal, writer, actions, data).await
    } else {
        if matches!(
            lease.action(),
            ClientAction::GlobalSettings(_)
                | ClientAction::Close { .. }
                | ClientAction::Delete { .. }
                | ClientAction::Steer { .. }
                | ClientAction::ComposeMessage { .. }
        ) {
            lease.prepare_pending_transfer();
        }
        match dispatch_action(
            principal,
            data,
            writer,
            actions,
            &handles.submissions,
            lease.action(),
        )
        .await
        {
            Ok(Some(id)) => {
                if matches!(
                    lease.action(),
                    ClientAction::Job { .. }
                        | ClientAction::Jobs { .. }
                        | ClientAction::Rack { .. }
                        | ClientAction::TrustShow { .. }
                ) {
                    drop(lease.commit());
                } else {
                    lease.commit_pending(id);
                }
                Ok(())
            }
            Ok(None) => {
                drop(lease.commit());
                Ok(())
            }
            Err(error) => {
                if answer_failed_send(lease.action(), &error) {
                    drop(lease.commit());
                } else {
                    lease.restore();
                }
                Err(error)
            }
        }
    }
}

/// Answer an action whose caller awaits its own reply with the send failure;
/// false leaves the action for a retry.
fn answer_failed_send(action: &ClientAction, error: &str) -> bool {
    let error = error.to_owned();
    match action {
        ClientAction::Job { respond, .. } => respond(Err(error)),
        ClientAction::Jobs { respond } => respond(Err(error)),
        ClientAction::Rack { respond, .. } => respond(Err(error)),
        ClientAction::TrustShow { respond, .. } => respond(Err(error)),
        ClientAction::ComposeMessage { respond, .. } => respond(Err(error)),
        _ => return false,
    }
    true
}

async fn dispatch_trust_decision(
    principal: &Principal,
    data: &SurfaceData,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    handles: &SurfaceHandles,
    lease: ActionLease,
) -> Result<(), String> {
    let ClientAction::TrustDecision {
        task,
        revision,
        digest,
        decision,
        ..
    } = lease.action()
    else {
        unreachable!("trust dispatch receives only a native trust decision");
    };
    let ready = data
        .read()
        .as_ref()
        .is_some_and(|runtime| runtime.trust_decision_is_ready(task, *revision, digest));
    if !ready {
        if let Some(runtime) = data.write().as_mut() {
            runtime.settle_trust_decision(
                digest.clone(),
                *decision,
                Some(
                    "This task requirement or trust authority changed before the decision. Review the current blocker."
                        .to_owned(),
                ),
            );
        }
        handles.surface.set(data.publication());
        drop(lease.commit());
        return Ok(());
    }
    match dispatch_action(
        principal,
        data,
        writer,
        actions,
        &handles.submissions,
        lease.action(),
    )
    .await
    {
        Ok(Some(id)) => {
            lease.commit_pending(id);
            Ok(())
        }
        Ok(None) => {
            drop(lease.commit());
            Ok(())
        }
        Err(error) => {
            lease.restore();
            Err(error)
        }
    }
}

async fn send_topology_as(
    principal: &Principal,
    writer: &mut SocketWriter,
    id: CanonicalUuid,
    action: &ClientAction,
) -> Result<(), String> {
    match action {
        ClientAction::RenewLane {
            topology_revision,
            team,
            lane,
            requested,
            ..
        } => {
            send_untracked_as(
                principal,
                writer,
                id,
                CommandKind::LaneRenewal,
                vec![],
                LaneRenewal {
                    topology_revision: *topology_revision,
                    team: *team,
                    lane: *lane,
                    requested: *requested,
                },
            )
            .await
        }
        ClientAction::RetuneLane {
            topology_revision,
            request,
        } => send_lane_retune(principal, writer, id, *topology_revision, request).await,
        ClientAction::ArrangeTeams {
            topology_revision,
            teams,
            ..
        } => {
            send_untracked_as(
                principal,
                writer,
                id,
                CommandKind::TeamArrange,
                vec![],
                BorrowedTeamArrange {
                    topology_revision: *topology_revision,
                    teams,
                },
            )
            .await
        }
        ClientAction::ConfigureTeam {
            topology_revision,
            team,
            member,
            new_team,
            drive,
            drain,
            fast_mode,
        } => {
            send_untracked_as(
                principal,
                writer,
                id,
                CommandKind::TeamConfigure,
                vec![],
                TeamConfigure {
                    topology_revision: *topology_revision,
                    team: *team,
                    member: *member,
                    new_team: *new_team,
                    drive: *drive,
                    drain: *drain,
                    name: None,
                    clear_name: false,
                    fast_mode: *fast_mode,
                },
            )
            .await
        }
        _ => Err("non-topology action reached topology dispatch".to_owned()),
    }
}

async fn send_lane_retune(
    principal: &Principal,
    writer: &mut SocketWriter,
    id: CanonicalUuid,
    topology_revision: u64,
    request: &crate::runtime::MemberRequest,
) -> Result<(), String> {
    match &request.command {
        crate::runtime::MemberCommand::Retune { model } => {
            send_untracked_as(
                principal,
                writer,
                id,
                CommandKind::LaneRetune,
                vec![],
                LaneRetune {
                    topology_revision,
                    lane: request.lane,
                    model: model.clone(),
                },
            )
            .await
        }
        crate::runtime::MemberCommand::RetuneEffort { effort } => {
            send_untracked_as(
                principal,
                writer,
                id,
                CommandKind::LaneEffortRetune,
                vec![],
                LaneEffortRetune {
                    topology_revision,
                    lane: request.lane,
                    effort: effort.clone(),
                },
            )
            .await
        }
        crate::runtime::MemberCommand::RetuneFast { fast_mode } => {
            send_untracked_as(
                principal,
                writer,
                id,
                CommandKind::LaneFastRetune,
                vec![],
                LaneFastRetune {
                    topology_revision,
                    lane: request.lane,
                    fast_mode: *fast_mode,
                },
            )
            .await
        }
        _ => Err("a lane retune carried a non-retune member command".to_owned()),
    }
}

pub(super) fn topology_pending_kind(action: &ClientAction) -> PendingKind {
    match action {
        ClientAction::RenewLane {
            team,
            lane,
            requested,
            member,
            ..
        } => PendingKind::LaneRenewal {
            team: *team,
            lane: *lane,
            requested: *requested,
            member: member.clone(),
        },
        ClientAction::RetuneLane { request, .. } => PendingKind::LaneRetune {
            request: request.clone(),
        },
        ClientAction::ConfigureTeam {
            team,
            drive,
            drain,
            fast_mode,
            ..
        } => PendingKind::TeamConfigure {
            team: *team,
            drive: *drive,
            drain: *drain,
            fast_mode: *fast_mode,
        },
        ClientAction::ArrangeTeams { respond, .. } => PendingKind::TeamArrange {
            respond: respond.clone(),
        },
        _ => unreachable!("only topology mutations have scheduler response state"),
    }
}
