#[derive(Default)]
pub(super) struct FrameApplication {
    pub(super) directives: Vec<StreamDirective>,
    pub(super) refresh: bool,
    pub(super) stale: crate::Stale,
    /// An applied lane answer arms or cancels the one-shot quiet deadline.
    pub(super) lane_refresh: Option<Option<u32>>,
}

/// Apply one admitted stream frame, publishing the surface it changed.
fn handle_stream_frame(
    session: &Session,
    surface: &UseStateHandle<SurfaceData>,
    data: &SurfaceData,
    bytes: &[u8],
    publish: bool,
) -> Result<FrameApplication, String> {
    let decode = diagnostics::span("frame.decode.stream", || diagnostics::Metadata {
        frame_bytes: Some(bytes.len()),
        ..diagnostics::Metadata::default()
    });
    let admitted = session
        .admit_stream(bytes)
        .map_err(|fault| format!("{}: {}", fault.reason, fault.detail))?;
    drop(decode);
    if let wire::StreamFrame::Fault { fault, .. } = admitted.get() {
        return Err(format!("{}: {}", fault.reason, fault.detail));
    }
    let mut stale = stale_reads(&admitted);
    let apply = diagnostics::span("state.apply.stream", || diagnostics::Metadata {
        frame_bytes: Some(bytes.len()),
        cards: data
            .read()
            .as_ref()
            .map_or(0, |runtime| runtime.messages().len())
            .into(),
        ..diagnostics::Metadata::default()
    });
    let (directives, changed) = {
        let mut data = data.write();
        let runtime = data
            .as_mut()
            .ok_or_else(|| "stream frame arrived before runtime acceptance".to_owned())?;
        stale.resolve_steering_rooms(runtime.room_projection().conversation());
        stale.resolve_invitations(runtime);
        let application = runtime.apply_stream_frame(&admitted);
        if application.board_stale() {
            stale.invalidate_configuration();
        }
        let changed = application.changed();
        if changed && let wire::StreamFrame::MessageGeneration { generation, .. } = admitted.get() {
            stale.invalidate_messages(runtime.lanes(), &generation.messages);
        }
        stale.invalidate_task_details(runtime, changed);
        stale.resolve_task_relations(runtime, changed);
        (application.into_directives(), changed)
    };
    drop(apply);
    if changed && publish {
        let _publication = diagnostics::span("state.publish", || diagnostics::Metadata {
            frame_bytes: Some(bytes.len()),
            cards: data
                .read()
                .as_ref()
                .map_or(0, |runtime| runtime.messages().len())
                .into(),
            ..diagnostics::Metadata::default()
        });
        surface.set(data.publication());
    }
    Ok(FrameApplication {
        directives,
        stale,
        ..FrameApplication::default()
    })
}

pub(super) fn handle_incoming(
    session: &Session,
    handles: &SurfaceHandles,
    data: &SurfaceData,
    actions: &ActionQueue,
    bytes: &[u8],
    publish: bool,
) -> Result<FrameApplication, String> {
    let classify = diagnostics::span("frame.decode.classify", || diagnostics::Metadata {
        frame_bytes: Some(bytes.len()),
        ..diagnostics::Metadata::default()
    });
    let crossing: serde_json::Value = wire::decode(bytes).map_err(|error| error.to_string())?;
    drop(classify);
    if crossing.get("stream").is_some() {
        return handle_stream_frame(session, &handles.surface, data, bytes, publish);
    }
    let identity_decode = diagnostics::span("frame.decode.identity", || diagnostics::Metadata {
        frame_bytes: Some(bytes.len()),
        ..diagnostics::Metadata::default()
    });
    let identity: ResponseIdentity = wire::decode(bytes).map_err(|error| error.to_string())?;
    drop(identity_decode);
    let ordinary_kind = actions.settle_pending(identity.id);
    let topology_kind = actions
        .map_in_flight_topology(identity.id, topology_pending_kind)
        .ok();
    let (kind, topology) = match (ordinary_kind, topology_kind) {
        (Some(_), Some(_)) => {
            return Err(format!(
                "response identity {} names both ordinary and topology commands",
                identity.id
            ));
        }
        (Some(kind), None) => (kind, false),
        (None, Some(kind)) => (kind, true),
        (None, None) => {
            return Err(format!("response named unknown command {}", identity.id));
        }
    };
    let topology_response = topology
        .then(|| classify_topology_response(session, bytes))
        .transpose()?;
    if let Some(TopologyResponse::Committed(range)) = &topology_response {
        actions.admit_topology(identity.id, *range)?;
    }
    let mut runtime_slot = data.write();
    let runtime = runtime_slot
        .as_mut()
        .ok_or_else(|| "response arrived before runtime acceptance".to_owned())?;
    let apply = diagnostics::span("state.apply.response", || diagnostics::Metadata {
        frame_bytes: Some(bytes.len()),
        ..diagnostics::Metadata::default()
    });
    let application = apply_command_response(session, handles, runtime, identity.id, kind, bytes)?;
    drop(apply);
    if !topology && application.lane_authority {
        actions.observe_topology_authority();
    }
    if let Some(response) = topology_response {
        let committed =
            settle_topology(actions, identity.id, application.lane_authority, response)?;
        if application.refresh {
            actions.await_topology_authority(committed);
        }
    }
    let lane_refresh = application.lane_authority.then(|| {
        runtime
            .lanes()
            .expect("an applied lane answer installs its snapshot")
            .refresh_after_millis
    });
    drop(runtime_slot);
    if publish && application.publish {
        let _publication = diagnostics::span("state.publish", || diagnostics::Metadata {
            frame_bytes: Some(bytes.len()),
            cards: data
                .read()
                .as_ref()
                .map_or(0, |runtime| runtime.messages().len())
                .into(),
            ..diagnostics::Metadata::default()
        });
        handles.surface.set(data.publication());
    }
    Ok(FrameApplication {
        refresh: application.refresh,
        lane_refresh,
        ..FrameApplication::default()
    })
}

struct CommandApplication {
    refresh: bool,
    lane_authority: bool,
    publish: bool,
}

impl CommandApplication {
    fn ordinary(refresh: bool) -> Self {
        Self {
            refresh,
            lane_authority: false,
            publish: true,
        }
    }
}

enum TopologyResponse {
    Committed(wire::SeqRange),
    Refused,
}

fn classify_topology_response(session: &Session, bytes: &[u8]) -> Result<TopologyResponse, String> {
    let admitted: crate::Admitted<Response<serde_json::Value>> = admit_response(session, bytes)?;
    match &admitted.get().outcome {
        wire::Outcome::Reply(reply) => match reply.position {
            wire::ReplyPosition::Committed(range) => Ok(TopologyResponse::Committed(range)),
            wire::ReplyPosition::Snapshot(_) | wire::ReplyPosition::Direct {} => Err(format!(
                "topology command {} returned a non-committed position",
                admitted.get().id
            )),
        },
        wire::Outcome::Refusal(_) | wire::Outcome::Fault(_) => Ok(TopologyResponse::Refused),
    }
}

fn settle_topology(
    actions: &ActionQueue,
    id: CanonicalUuid,
    rebase_next: bool,
    response: TopologyResponse,
) -> Result<bool, String> {
    match response {
        TopologyResponse::Committed(range) => {
            actions.settle_topology(id, Some(range), rebase_next)?;
            Ok(true)
        }
        TopologyResponse::Refused => {
            actions.settle_topology(id, None, false)?;
            Ok(false)
        }
    }
}

fn apply_callback_reply<T: for<'de> serde::Deserialize<'de>>(
    session: &Session,
    bytes: &[u8],
    label: &str,
    respond: &std::rc::Rc<dyn Fn(Result<T, String>)>,
) -> Result<CommandApplication, String> {
    let admitted: crate::Admitted<Response<T>> = match admit_response(session, bytes) {
        Ok(admitted) => admitted,
        Err(error) => {
            respond(Err(error.clone()));
            return Err(error);
        }
    };
    let result = match admitted.into_inner().outcome {
        wire::Outcome::Reply(reply) => Ok(reply.body),
        outcome => {
            Err(outcome_issue(label, &outcome).expect("a non-reply outcome describes its issue"))
        }
    };
    respond(result);
    Ok(CommandApplication::ordinary(false))
}

fn apply_command_response(
    session: &Session,
    handles: &SurfaceHandles,
    runtime: &mut AppRuntime,
    id: CanonicalUuid,
    kind: PendingKind,
    bytes: &[u8],
) -> Result<CommandApplication, String> {
    Ok(match kind {
        PendingKind::Rack { respond } => {
            apply_callback_reply(session, bytes, "Documentation read failed", &respond)?
        }
        kind @ (PendingKind::Board
        | PendingKind::PrincipalDirectory
        | PendingKind::Settings
        | PendingKind::Vendors
        | PendingKind::Consent
        | PendingKind::Principals) => apply_scalar_response(session, runtime, kind, bytes)?,
        PendingKind::ConsentRuling { request } => {
            let admitted: crate::Admitted<Response<wire::intent::ConsentAnswer>> =
                admit_response(session, bytes)?;
            runtime.apply_consent_ruling(request, &admitted);
            CommandApplication::ordinary(false)
        }
        PendingKind::Jobs { respond } => {
            let admitted: crate::Admitted<Response<wire::intent::LaneJobsAnswer>> =
                match admit_response(session, bytes) {
                    Ok(admitted) => admitted,
                    Err(error) => {
                        respond(Err(error.clone()));
                        return Err(error);
                    }
                };
            let result = match admitted.into_inner().outcome {
                wire::Outcome::Reply(reply) => Ok(reply.body),
                outcome => Err(outcome_issue("Job catalog failed", &outcome)
                    .expect("a non-reply outcome describes its issue")),
            };
            respond(result);
            CommandApplication {
                refresh: false,
                lane_authority: false,
                publish: false,
            }
        }
        PendingKind::Job {
            target,
            stop,
            respond,
        } => {
            let admitted: crate::Admitted<Response<wire::intent::LaneJobAnswer>> =
                match admit_response(session, bytes) {
                    Ok(admitted) => admitted,
                    Err(error) => {
                        respond(Err(error.clone()));
                        return Err(error);
                    }
                };
            let result = match admitted.into_inner().outcome {
                wire::Outcome::Reply(reply) if reply.body.target == target => Ok(reply.body),
                wire::Outcome::Reply(_) => Err("Resident answered for another job".to_owned()),
                outcome => Err(outcome_issue("Job request failed", &outcome)
                    .expect("a non-reply outcome describes its issue")),
            };
            let refresh = stop && result.is_ok();
            respond(result);
            CommandApplication {
                refresh,
                lane_authority: false,
                publish: false,
            }
        }
        kind @ (PendingKind::RoomPost { .. }
        | PendingKind::SteerLine { .. }
        | PendingKind::SteerDirective { .. }) => apply_room_response(session, kind, bytes)?,
        PendingKind::TaskDetail { task } => {
            let admitted: crate::Admitted<Response<wire::intent::TaskDetailAnswer>> =
                admit_response(session, bytes)?;
            runtime.apply_task_detail(&task, &admitted);
            CommandApplication::ordinary(false)
        }
        PendingKind::TaskRelations { key } => {
            let admitted: crate::Admitted<Response<wire::intent::TaskRelationsAnswer>> =
                admit_response(session, bytes)?;
            let publish = runtime.apply_task_relations(&key, &admitted);
            CommandApplication {
                refresh: false,
                lane_authority: false,
                publish,
            }
        }
        kind => apply_mutating_response(session, handles, runtime, id, kind, bytes)?,
    })
}
fn apply_room_response(
    session: &Session,
    kind: PendingKind,
    bytes: &[u8],
) -> Result<CommandApplication, String> {
    match kind {
        PendingKind::RoomPost { respond } => {
            apply_callback_reply(session, bytes, "Room post failed", &respond)
        }
        PendingKind::SteerLine { respond } => {
            apply_callback_reply(session, bytes, "Line control failed", &respond)
        }
        PendingKind::SteerDirective { respond } => {
            apply_callback_reply(session, bytes, "Steering control failed", &respond)
        }
        _ => unreachable!("only room responses reach room response dispatch"),
    }
}

fn apply_mutating_response(
    session: &Session,
    handles: &SurfaceHandles,
    runtime: &mut AppRuntime,
    id: CanonicalUuid,
    kind: PendingKind,
    bytes: &[u8],
) -> Result<CommandApplication, String> {
    Ok(match kind {
        // A read of the lane table and an arrangement of it answer with the
        // same authoritative table, so successful replies apply identically.
        // Orgs move only when that answer lands. A refused mutation differs:
        // it means this client lost its optimistic revision, so it immediately
        // re-reads the winning table rather than retaining the stale render.
        PendingKind::Lanes { rebases } => {
            let (refresh, applied, _) =
                apply_lane_response(session, handles, runtime, &rebases, false, bytes)?;
            CommandApplication {
                refresh,
                lane_authority: applied,
                publish: true,
            }
        }
        PendingKind::TeamArrange { respond } => {
            let (refresh, applied, issue) =
                apply_lane_response(session, handles, runtime, &[], true, bytes)?;
            if let Some(respond) = respond {
                respond(issue.map_or(Ok(()), Err));
            }
            CommandApplication {
                refresh,
                lane_authority: applied,
                publish: true,
            }
        }
        kind @ (PendingKind::LaneRenewal { .. }
        | PendingKind::MemberLane { .. }
        | PendingKind::LaneRetune { .. }) => {
            apply_member_response(session, handles, runtime, kind, bytes)?
        }
        PendingKind::Inbox { covered } => {
            apply_inbox_response(session, handles, runtime, &covered, bytes)?
        }
        PendingKind::ConversationText { publication } => {
            let admitted: crate::Admitted<Response<wire::intent::RoomConversationTextAnswer>> =
                admit_response(session, bytes)?;
            runtime.apply_conversation_text(&publication, &admitted);
            CommandApplication::ordinary(false)
        }
        PendingKind::ContributionRead { publication } => {
            let admitted: crate::Admitted<Response<wire::intent::RoomContributionReadAnswer>> =
                admit_response(session, bytes)?;
            runtime.apply_contribution_read(&publication, &admitted);
            CommandApplication::ordinary(false)
        }
        PendingKind::TeamConfigure {
            team,
            drive,
            drain,
            fast_mode,
        } => {
            let (refresh, applied) = apply_team_configuration(
                session,
                &handles.team_controls,
                runtime,
                TeamRequest {
                    team,
                    drive,
                    drain,
                    fast_mode,
                },
                bytes,
            )?;
            CommandApplication {
                refresh,
                lane_authority: applied,
                publish: true,
            }
        }
        PendingKind::ComposeMessage { respond } => {
            apply_callback_reply(session, bytes, "Message was not sent", &respond)?
        }
        PendingKind::Steer => {
            let _admitted: crate::Admitted<Response<wire::intent::DirectiveAnswer>> =
                admit_response(session, bytes)?;
            CommandApplication::ordinary(true)
        }
        PendingKind::SteerSubmission { lane } => CommandApplication::ordinary(
            apply_submission_response(session, handles, id, lane, bytes)?,
        ),
        PendingKind::PrincipalWrite { respond } => {
            apply_callback_reply(session, bytes, "Principal change failed", &respond)?
        }
        kind => apply_task_mutation_response(session, handles, runtime, kind, bytes)?,
    })
}

fn apply_task_mutation_response(
    session: &Session,
    handles: &SurfaceHandles,
    runtime: &mut AppRuntime,
    kind: PendingKind,
    bytes: &[u8],
) -> Result<CommandApplication, String> {
    Ok(match kind {
        PendingKind::TrustDecision { digest, decision } => {
            apply_trust_decision_response(session, runtime, digest, decision, bytes)?
        }
        PendingKind::Close { task } => {
            apply_settlement_response(session, handles, runtime, task, bytes, &CLOSE_WORDING)?
        }
        PendingKind::Delete { task } => {
            apply_settlement_response(session, handles, runtime, task, bytes, &DELETE_WORDING)?
        }
        PendingKind::Preparation { respond } => {
            apply_callback_reply(session, bytes, "Preparation failed", &respond)?
        }
        // A landed Draft filing change re-reads the board so its list and
        // detail move; an image set re-reads the Draft's detail too.
        PendingKind::Draft { respond, reread } => {
            if let Some(task) = reread {
                runtime.reread_task_detail(&task);
            }
            CommandApplication {
                refresh: true,
                ..apply_callback_reply(session, bytes, "Task change failed", &respond)?
            }
        }
        // A landed conclusion re-reads the board so every standing moves.
        PendingKind::Conclude { respond } => CommandApplication {
            refresh: true,
            ..apply_callback_reply(session, bytes, "Conclusion failed", &respond)?
        },
        // A landed wake or deferral re-reads the board so the task leaves or
        // joins the deferred presentation.
        PendingKind::Deferral { respond } => CommandApplication {
            refresh: true,
            ..apply_callback_reply(session, bytes, "Deferral change failed", &respond)?
        },
        _ => unreachable!("read and mutation response partitions are closed"),
    })
}

fn apply_trust_decision_response(
    session: &Session,
    runtime: &mut AppRuntime,
    digest: wire::Digest,
    decision: crate::client_action::TrustDecision,
    bytes: &[u8],
) -> Result<CommandApplication, String> {
    let admitted: crate::Admitted<Response<wire::intent::TrustAnswer>> =
        admit_response(session, bytes)?;
    let expected = decision.standing();
    let issue = match &admitted.get().outcome {
        wire::Outcome::Reply(reply)
            if reply.body.digest == digest && reply.body.standing == expected =>
        {
            None
        }
        wire::Outcome::Reply(reply) => Some(format!(
            "The resident answered for {} at {:?}; expected {digest} at {expected:?}.",
            reply.body.digest, reply.body.standing,
        )),
        outcome => outcome_issue(
            match decision {
                crate::client_action::TrustDecision::Approve => "Trust was not approved",
                crate::client_action::TrustDecision::Reject => "Trust was not rejected",
            },
            outcome,
        ),
    };
    let refresh = issue.is_none();
    runtime.settle_trust_decision(digest, decision, issue);
    Ok(CommandApplication::ordinary(refresh))
}

/// How one settlement operation reads in its failure messages.
#[derive(Clone, Copy)]
struct SettlementWording {
    /// The operation as a verb: "close".
    operation: &'static str,
    /// The operation in the past tense: "closed".
    past: &'static str,
    /// The refusal headline: "Task was not closed".
    failure: &'static str,
}

const CLOSE_WORDING: SettlementWording = SettlementWording {
    operation: "close",
    past: "closed",
    failure: "Task was not closed",
};

const DELETE_WORDING: SettlementWording = SettlementWording {
    operation: "delete",
    past: "deleted",
    failure: "Task was not deleted",
};

/// A settled task clears its composer and rereads the detail; a refusal keeps
/// the draft with the resident's reason.
fn apply_settlement_response(
    session: &Session,
    handles: &SurfaceHandles,
    runtime: &mut AppRuntime,
    task: wire::TaskStamp,
    bytes: &[u8],
    wording: &SettlementWording,
) -> Result<CommandApplication, String> {
    let SettlementWording {
        operation,
        past,
        failure,
    } = *wording;
    let admitted: crate::Admitted<Response<TaskAnswer>> = admit_response(session, bytes)?;
    match &admitted.get().outcome {
        wire::Outcome::Reply(reply) => {
            let answered = reply.body.task.born().map_err(|error| {
                format!("{operation} answer named a non-canonical task: {error}")
            })?;
            if answered != task {
                handles.notes.update(&task, |composer| {
                    composer.submission = NoteSubmission::Failed(format!(
                        "The resident {past} a different task; the draft was retained."
                    ));
                });
                return Err(format!(
                    "{operation} answer task identity did not match its command"
                ));
            }
            handles
                .notes
                .update(&task, |composer| *composer = NoteComposer::default());
            runtime.reread_task_detail(&task);
            Ok(CommandApplication::ordinary(false))
        }
        outcome => {
            let issue = outcome_issue(failure, outcome).unwrap_or_else(|| format!("{failure}."));
            handles.notes.update(&task, |composer| {
                composer.submission = NoteSubmission::Failed(issue);
            });
            Ok(CommandApplication::ordinary(false))
        }
    }
}

fn apply_scalar_response(
    session: &Session,
    runtime: &mut AppRuntime,
    kind: PendingKind,
    bytes: &[u8],
) -> Result<CommandApplication, String> {
    match kind {
        PendingKind::Board => runtime.apply_board(&admit_response(session, bytes)?),
        PendingKind::PrincipalDirectory => {
            let admitted =
                admit_response::<wire::principal::PrincipalDirectoryAnswer>(session, bytes)?;
            if runtime.apply_principals(&admitted) {
                return Err(
                    "connected principal authority changed; reopening its subscription".to_owned(),
                );
            }
        }
        PendingKind::Settings => runtime.apply_settings(&admit_response(session, bytes)?),
        PendingKind::Vendors => runtime.apply_vendor_catalog(&admit_response(session, bytes)?),
        PendingKind::Consent => runtime.apply_consent_queue(&admit_response(session, bytes)?),
        PendingKind::Principals => {
            runtime.apply_principal_listing(&admit_response(session, bytes)?);
        }
        _ => unreachable!("scalar response partition is closed"),
    }
    Ok(CommandApplication::ordinary(false))
}

fn apply_lane_response(
    session: &Session,
    handles: &SurfaceHandles,
    runtime: &mut AppRuntime,
    rebases: &[CanonicalUuid],
    refresh_on_failure: bool,
    bytes: &[u8],
) -> Result<(bool, bool, Option<String>), String> {
    let admitted: crate::Admitted<Response<wire::intent::LaneAnswer>> =
        admit_response(session, bytes)?;
    let issue = outcome_issue("Team arrangement failed", &admitted.get().outcome);
    let rebase_lanes = handles.submissions.queue.borrow().awaiting_lanes(rebases);
    let reply = matches!(&admitted.get().outcome, wire::Outcome::Reply(_));
    let applied = runtime.apply_lanes(&admitted);
    if let wire::Outcome::Reply(answer) = &admitted.get().outcome
        && applied
    {
        for lane in &rebase_lanes {
            let target = runtime
                .lanes()
                .and_then(|lanes| lanes.member(*lane))
                .and_then(|member| {
                    (member.occupant.is_some() || member.wakeable).then_some(member.occupant)
                });
            if let Some(occupant) = target {
                handles.submissions.queue.borrow_mut().rebase_awaiting_lane(
                    rebases,
                    *lane,
                    occupant,
                    answer.body.epoch,
                );
            } else {
                let reason = "Message was not sent because a fresh lane snapshot no longer \
                              presents a reachable seat. Review the lane and retry.";
                let settles = handles
                    .submissions
                    .queue
                    .borrow_mut()
                    .refuse_awaiting_lane(*lane, rebases, reason)?;
                if !settles {
                    handles.refusals.settled(reason.to_owned());
                }
            }
        }
        handles
            .team_controls
            .dispatch(TeamControlAction::SyncLanes {
                answer: answer.body.clone(),
                settle: None,
            });
    }
    if applied && !rebase_lanes.is_empty() {
        handles.submissions.publish();
    }
    let needs_followup = reply
        && !handles
            .submissions
            .queue
            .borrow()
            .awaiting_residency()
            .is_empty();
    let refresh = needs_followup
        || (refresh_on_failure && topology_mutation_requires_refresh(&admitted.get().outcome));
    Ok((refresh, applied, issue))
}

/// Prefix of every renewal refusal.
const RENEWAL_ISSUE: &str = "Could not renew this agent";

/// Answers to commands a member's own controls issue, each settling the
/// member reservation it carries.
fn apply_member_response(
    session: &Session,
    handles: &SurfaceHandles,
    runtime: &mut AppRuntime,
    kind: PendingKind,
    bytes: &[u8],
) -> Result<CommandApplication, String> {
    Ok(match kind {
        PendingKind::LaneRenewal {
            team,
            lane,
            requested,
            member,
        } => {
            let issue = apply_lane_renewal_response(session, team, lane, requested, bytes)?;
            // A member reservation reports its settlement through request
            // feedback; any other renewal has only this toast.
            match (member, issue) {
                (Some(member), issue) => runtime.settle_member_command(&member, issue),
                (None, Some(issue)) => handles.refusals.settled(issue),
                (None, None) => {}
            }
            CommandApplication::ordinary(true)
        }
        PendingKind::MemberLane { request } => {
            let admitted: crate::Admitted<Response<wire::intent::LaneChangeAnswer>> =
                admit_response(session, bytes)?;
            let outcome = &admitted.get().outcome;
            if let wire::Outcome::Reply(reply) = outcome
                && reply.body.lane != request.lane
            {
                return Err(format!(
                    "lane state response named {}; expected {}",
                    reply.body.lane, request.lane
                ));
            }
            runtime
                .settle_member_command(&request, outcome_issue(request.command.failure(), outcome));
            CommandApplication::ordinary(matches!(outcome, wire::Outcome::Reply(_)))
        }
        PendingKind::LaneRetune { request } => {
            let admitted: crate::Admitted<Response<wire::intent::LaneAnswer>> =
                admit_response(session, bytes)?;
            let refresh = topology_mutation_requires_refresh(&admitted.get().outcome);
            let applied = runtime.apply_lanes(&admitted);
            if let wire::Outcome::Reply(reply) = &admitted.get().outcome
                && applied
            {
                handles
                    .team_controls
                    .dispatch(TeamControlAction::SyncLanes {
                        answer: reply.body.clone(),
                        settle: None,
                    });
            }
            runtime.settle_member_command(
                &request,
                outcome_issue(request.command.failure(), &admitted.get().outcome),
            );
            CommandApplication {
                refresh,
                lane_authority: applied,
                publish: true,
            }
        }
        _ => unreachable!("member response partition is closed"),
    })
}

/// Apply one renewal answer, returning the refusal it reported, if any.
fn apply_lane_renewal_response(
    session: &Session,
    team: CanonicalUuid,
    lane: CanonicalUuid,
    requested: bool,
    bytes: &[u8],
) -> Result<Option<String>, String> {
    let admitted: crate::Admitted<Response<wire::intent::LaneRenewalAnswer>> =
        admit_response(session, bytes)?;
    match &admitted.get().outcome {
        wire::Outcome::Reply(reply) => {
            if reply.body.team != team || reply.body.lane != lane {
                return Err(format!(
                    "agent renewal response named {}/{}; expected {team}/{lane}",
                    reply.body.team, reply.body.lane,
                ));
            }
            if requested != reply.body.renewal.is_some() {
                return Err(format!(
                    "agent renewal response for {team}/{lane} did not settle requested={requested}",
                ));
            }
            Ok(None)
        }
        outcome => Ok(Some(
            outcome_issue(RENEWAL_ISSUE, outcome)
                .expect("the refused or faulted renewal outcome has an issue"),
        )),
    }
}

fn apply_inbox_response(
    session: &Session,
    handles: &SurfaceHandles,
    runtime: &mut AppRuntime,
    covered: &BTreeMap<CanonicalUuid, String>,
    bytes: &[u8],
) -> Result<CommandApplication, String> {
    let admitted: crate::Admitted<Response<wire::intent::InboxAnswer>> =
        admit_response(session, bytes)?;
    let answered = if let wire::Outcome::Reply(reply) = &admitted.get().outcome {
        handles.submissions.queue.borrow_mut().observe_backend(
            reply.body.lanes.iter().flat_map(|lane| lane.pending.iter()),
            covered,
        )?;
        handles.submissions.publish();
        true
    } else {
        false
    };
    runtime.apply_inbox(&admitted);
    Ok(CommandApplication::ordinary(
        answered && !handles.submissions.queue.borrow().accepted().is_empty(),
    ))
}

fn apply_submission_response(
    session: &Session,
    handles: &SurfaceHandles,
    id: CanonicalUuid,
    lane: CanonicalUuid,
    bytes: &[u8],
) -> Result<bool, String> {
    let admitted: crate::Admitted<Response<wire::intent::DirectiveAnswer>> =
        admit_response(session, bytes)?;
    match &admitted.get().outcome {
        wire::Outcome::Reply(reply) => {
            let held = handles.submissions.queue.borrow().held_content(id);
            let _ = handles.submissions.queue.borrow_mut().accept(
                id,
                reply.body.directive,
                reply.body.key.clone(),
                reply.body.disposition,
            )?;
            // Durable acceptance alone clears what the locked composer sent.
            if let Some((composer, sent)) = held {
                crate::browser::watchband::clear_sent_composer(
                    &crate::browser::watchband::source_key(composer),
                    &handles.composer_drafts,
                    sent,
                );
            }
        }
        wire::Outcome::Refusal(_) | wire::Outcome::Fault(_) => {
            let source = handles.submissions.queue.borrow().source(id)?;
            let action = if source == ComposerSource::Lane(lane) {
                "Message was not sent".to_owned()
            } else {
                format!("Broadcast was not sent to agent {lane}")
            };
            let issue = outcome_issue(&action, &admitted.get().outcome)
                .expect("the refused or faulted steering outcome has an issue");
            let reason = outcome_reason(&admitted.get().outcome)
                .expect("the refused or faulted steering outcome has a reason");
            // The content never left its composer; refusal only unlocks it. A
            // several-agent send reports every branch once it settles.
            if !handles.submissions.queue.borrow_mut().refuse(id, &reason)? {
                handles.refusals.settled(issue);
            }
        }
    }
    handles.submissions.publish();
    Ok(true)
}

fn outcome_issue<T>(action: &str, outcome: &wire::Outcome<T>) -> Option<String> {
    outcome_reason(outcome).map(|reason| format!("{action}: {reason}"))
}

fn outcome_reason<T>(outcome: &wire::Outcome<T>) -> Option<String> {
    match outcome {
        wire::Outcome::Reply(_) => None,
        wire::Outcome::Refusal(refusal) => {
            Some(format!("{}. {}", refusal.reason(), refusal.explanation()))
        }
        wire::Outcome::Fault(fault) => Some(format!("{}. {}", fault.reason, fault.detail)),
    }
}
use super::*;
