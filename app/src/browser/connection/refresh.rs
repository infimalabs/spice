//! Demand-driven app current reads and duplicate-request coalescing.

use super::*;
use crate::action_mailbox::{ActionAdmissionError, CurrentRead};
const RECENT_FINISHED_LIMIT: u16 = 20;

pub(super) async fn send_rack(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    request: &wire::intent::RackRead,
    respond: &crate::client_action::RackResponse,
) -> Result<CanonicalUuid, String> {
    send_typed(
        principal,
        writer,
        actions,
        PendingKind::Rack {
            respond: respond.clone(),
        },
        CommandKind::RackRead,
        Vec::new(),
        request,
    )
    .await
}

pub(super) async fn send_jobs(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    respond: &crate::client_action::JobsResponse,
) -> Result<CanonicalUuid, String> {
    send_typed(
        principal,
        writer,
        actions,
        PendingKind::Jobs {
            respond: respond.clone(),
        },
        CommandKind::LaneJobsRead,
        Vec::new(),
        wire::intent::LaneJobsRead {},
    )
    .await
}

pub(super) async fn send_job(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    target: &wire::intent::LaneJobRef,
    stop: bool,
    respond: &crate::client_action::JobResponse,
) -> Result<CanonicalUuid, String> {
    send_typed(
        principal,
        writer,
        actions,
        PendingKind::Job {
            target: target.clone(),
            stop,
            respond: respond.clone(),
        },
        if stop {
            CommandKind::LaneJobStop
        } else {
            CommandKind::LaneJobRead
        },
        Vec::new(),
        target,
    )
    .await
}

pub(super) async fn send_after_frame(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    submissions: &SubmissionHandle,
    full: bool,
    stale: crate::Stale,
) -> Result<(), String> {
    send_stale(principal, writer, actions, submissions, stale).await?;
    if full {
        send_refresh(principal, writer, actions, submissions).await?;
    }

    // A lane read already in flight can coalesce a causal refresh, but it
    // cannot prove activity observed by a newer inbox. Keep the requirement
    // live until a post-observation lane read starts.
    let causal_lane_read = submissions
        .queue
        .borrow()
        .activity_read_requirement()
        .is_some();
    if causal_lane_read {
        send_lanes(principal, writer, actions, submissions).await?;
    }
    Ok(())
}

pub(super) async fn send_refresh(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    submissions: &SubmissionHandle,
) -> Result<(), String> {
    send_board(principal, writer, actions).await?;
    send_lanes(principal, writer, actions, submissions).await?;
    send_inbox(principal, writer, actions, submissions).await?;
    send_principal_directory(principal, writer, actions).await?;
    send_consent(principal, writer, actions).await?;
    send_settings(principal, writer, actions).await?;
    send_principals(principal, writer, actions).await?;
    send_vendors(principal, writer, actions).await
}

/// Remember each invalidated current read until every older request for it has
/// settled, then issue exactly one replacement from the websocket callback.
pub(super) async fn send_stale(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    submissions: &SubmissionHandle,
    stale: crate::Stale,
) -> Result<(), String> {
    actions.defer_stale_current_reads(&stale);

    if actions.take_ready_current_read(CurrentRead::Board) {
        send_board(principal, writer, actions).await?;
    }
    if actions.take_ready_current_read(CurrentRead::Lanes) {
        send_lanes(principal, writer, actions, submissions).await?;
    }
    if actions.take_ready_current_read(CurrentRead::Inbox) {
        send_inbox(principal, writer, actions, submissions).await?;
    }
    if actions.take_ready_current_read(CurrentRead::PrincipalDirectory) {
        send_principal_directory(principal, writer, actions).await?;
    }
    if actions.take_ready_current_read(CurrentRead::Consent) {
        send_consent(principal, writer, actions).await?;
    }
    if actions.take_ready_current_read(CurrentRead::Settings) {
        send_settings(principal, writer, actions).await?;
    }
    if actions.take_ready_current_read(CurrentRead::Principals) {
        send_principals(principal, writer, actions).await?;
    }
    Ok(())
}

/// Send owed task detail reads while the mailbox can hold their answers.
///
/// At most [`crate::action_mailbox::TASK_DETAIL_READ_LIMIT`] reads are outstanding, which leaves the
/// rest of the record pool to current reads and mutations. A read the record
/// or payload pool cannot hold stays owed, and every settled response runs
/// this again, so any number of shown tasks hydrates without a refusal.
pub(super) async fn send_task_detail_reads(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    data: &SurfaceData,
) -> Result<(), String> {
    let pane = SteerPane::over(principal);
    if !pane.offers(CommandKind::TaskDetail) {
        return Ok(());
    }
    let tasks = data
        .write()
        .as_mut()
        .map(|runtime| {
            runtime.take_task_detail_reads(actions.task_detail_slots(), |task| {
                actions.has_task_detail(task)
            })
        })
        .unwrap_or_default();
    let mut tasks = tasks.into_iter();
    while let Some(task) = tasks.next() {
        let id = random_uuid()?;
        let command = pane
            .command(
                id,
                CommandKind::TaskDetail,
                vec![],
                wire::intent::TaskDetail { task: task.clone() },
            )
            .map_err(|error| error.to_string())?;
        let bytes = wire::encode(&command).map_err(|error| error.to_string())?;
        match actions.reserve_pending(
            id,
            PendingKind::TaskDetail { task: task.clone() },
            bytes.len(),
        ) {
            Ok(()) => {}
            Err(ActionAdmissionError::Records | ActionAdmissionError::Payload) => {
                if let Some(runtime) = data.write().as_mut() {
                    std::iter::once(task)
                        .chain(tasks)
                        .for_each(|task| runtime.restore_task_detail_read(task));
                }
                return Ok(());
            }
            Err(error) => return Err(error.to_string()),
        }
        if let Err(error) = writer.send(Message::Bytes(bytes)).await {
            actions.cancel_pending(id);
            return Err(websocket_error(error));
        }
    }
    Ok(())
}

/// Read the source text of every open principal input that arrived without it;
/// a lane's bytes stay in its transcript, which only the resident reads.
pub(super) async fn send_principal_input_texts(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    data: &SurfaceData,
) -> Result<(), String> {
    if !SteerPane::over(principal).offers(CommandKind::RoomConversationText) {
        return Ok(());
    }
    let publications = data
        .write()
        .as_mut()
        .map(AppRuntime::begin_principal_input_texts)
        .unwrap_or_default();
    for publication in publications {
        send_typed(
            principal,
            writer,
            actions,
            PendingKind::ConversationText {
                publication: publication.clone(),
            },
            CommandKind::RoomConversationText,
            vec![],
            wire::intent::RoomConversationText { publication },
        )
        .await?;
    }
    Ok(())
}

/// Read the sources and question items of contributions in shown Designs
/// while the mailbox can hold their answers; a read the pool cannot hold
/// stays owed for the next settled response.
pub(super) async fn send_contribution_reads(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    data: &SurfaceData,
) -> Result<(), String> {
    let pane = SteerPane::over(principal);
    if !pane.offers(CommandKind::RoomContributionRead) {
        return Ok(());
    }
    let owed = data
        .write()
        .as_mut()
        .map(|runtime| runtime.take_contribution_reads(actions.contribution_read_slots()))
        .unwrap_or_default();
    let mut owed = owed.into_iter();
    while let Some((room, publication)) = owed.next() {
        let id = random_uuid()?;
        let command = pane
            .command(
                id,
                CommandKind::RoomContributionRead,
                vec![],
                wire::intent::RoomContributionRead {
                    room,
                    publication: publication.clone(),
                },
            )
            .map_err(|error| error.to_string())?;
        let bytes = wire::encode(&command).map_err(|error| error.to_string())?;
        match actions.reserve_pending(
            id,
            PendingKind::ContributionRead {
                publication: publication.clone(),
            },
            bytes.len(),
        ) {
            Ok(()) => {}
            Err(ActionAdmissionError::Records | ActionAdmissionError::Payload) => {
                if let Some(runtime) = data.write().as_mut() {
                    std::iter::once(publication)
                        .chain(owed.map(|(_, publication)| publication))
                        .for_each(|publication| {
                            runtime.restore_contribution_read(&publication);
                        });
                }
                return Ok(());
            }
            Err(error) => return Err(error.to_string()),
        }
        if let Err(error) = writer.send(Message::Bytes(bytes)).await {
            actions.cancel_pending(id);
            return Err(websocket_error(error));
        }
    }
    Ok(())
}

async fn send_board(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
) -> Result<(), String> {
    send_if_offered(
        principal,
        writer,
        actions,
        PendingKind::Board,
        CommandKind::BoardRead,
        BoardRead::Open {
            recent_finished: RECENT_FINISHED_LIMIT,
            after: None,
        },
    )
    .await
}

pub(super) async fn send_task_relations(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    key: &crate::TaskRelationsKey,
) -> Result<Option<CanonicalUuid>, String> {
    if !SteerPane::over(principal).offers(CommandKind::TaskRelations)
        || actions.has_task_relations(key)
    {
        return Ok(None);
    }
    send_typed(
        principal,
        writer,
        actions,
        PendingKind::TaskRelations { key: key.clone() },
        CommandKind::TaskRelations,
        vec![],
        key.request(),
    )
    .await
    .map(Some)
}
pub(super) async fn send_lanes(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    submissions: &SubmissionHandle,
) -> Result<(), String> {
    if !SteerPane::over(principal).offers(CommandKind::LaneRead)
        || actions.has_current_read(CurrentRead::Lanes)
    {
        return Ok(());
    }
    let (activity_generation, rebases) = {
        let queue = submissions.queue.borrow();
        (
            queue.activity_read_requirement(),
            queue.awaiting_residency(),
        )
    };
    send_typed(
        principal,
        writer,
        actions,
        PendingKind::Lanes { rebases },
        CommandKind::LaneRead,
        vec![],
        LaneRead {},
    )
    .await?;
    submissions
        .queue
        .borrow_mut()
        .activity_read_started(activity_generation);
    Ok(())
}

async fn send_inbox(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    submissions: &SubmissionHandle,
) -> Result<(), String> {
    let covered = submissions.queue.borrow().accepted();
    send_if_offered(
        principal,
        writer,
        actions,
        PendingKind::Inbox { covered },
        CommandKind::SteerInbox,
        SteerInbox { lane: None },
    )
    .await
}

/// The operator's consent queue; a lane principal is not offered it.
async fn send_consent(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
) -> Result<(), String> {
    send_if_offered(
        principal,
        writer,
        actions,
        PendingKind::Consent,
        CommandKind::ConsentRead,
        wire::intent::ConsentRead {},
    )
    .await
}

/// One operator ruling, carrying the witnesses its queue snapshot implied.
pub(super) async fn send_consent_ruling(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    ruling: &wire::intent::ConsentDecide,
    expected: &[wire::Witness],
) -> Result<CanonicalUuid, String> {
    send_typed(
        principal,
        writer,
        actions,
        PendingKind::ConsentRuling {
            request: ruling.request,
        },
        CommandKind::ConsentDecide,
        expected.to_vec(),
        ruling.clone(),
    )
    .await
}

async fn send_principal_directory(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
) -> Result<(), String> {
    send_if_offered(
        principal,
        writer,
        actions,
        PendingKind::PrincipalDirectory,
        CommandKind::PrincipalDirectoryRead,
        wire::principal::PrincipalDirectoryRead {},
    )
    .await
}

async fn send_settings(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
) -> Result<(), String> {
    send_if_offered(
        principal,
        writer,
        actions,
        PendingKind::Settings,
        CommandKind::GlobalSettingsRead,
        GlobalSettingsRead {},
    )
    .await
}

async fn send_principals(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
) -> Result<(), String> {
    send_if_offered(
        principal,
        writer,
        actions,
        PendingKind::Principals,
        CommandKind::PrincipalRead,
        wire::principal::PrincipalRead {},
    )
    .await
}

/// One principal write; its answer reaches the panel that asked.
pub(super) async fn send_principal_write(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    request: &crate::client_action::PrincipalWrite,
    respond: &crate::client_action::PrincipalResponse,
) -> Result<CanonicalUuid, String> {
    use crate::client_action::PrincipalWrite;
    let pending = PendingKind::PrincipalWrite {
        respond: respond.clone(),
    };
    let kind = request.kind();
    match request {
        PrincipalWrite::Admit(admit) => {
            send_typed(principal, writer, actions, pending, kind, vec![], admit).await
        }
        PrincipalWrite::Edit(edit) => {
            send_typed(principal, writer, actions, pending, kind, vec![], edit).await
        }
        PrincipalWrite::Grant(role) | PrincipalWrite::Revoke(role) => {
            send_typed(principal, writer, actions, pending, kind, vec![], role).await
        }
        PrincipalWrite::Deauthorize(request) => {
            send_typed(principal, writer, actions, pending, kind, vec![], request).await
        }
    }
}

async fn send_vendors(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
) -> Result<(), String> {
    send_if_offered(
        principal,
        writer,
        actions,
        PendingKind::Vendors,
        CommandKind::VendorCatalog,
        VendorCatalog {},
    )
    .await
}

async fn send_if_offered<B: Serialize>(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    pending_kind: PendingKind,
    kind: CommandKind,
    body: B,
) -> Result<(), String> {
    if !SteerPane::over(principal).offers(kind) {
        return Ok(());
    }
    if let Some(read) = pending_kind.current_read()
        && actions.has_current_read(read)
    {
        return Ok(());
    }
    send_typed(principal, writer, actions, pending_kind, kind, vec![], body)
        .await
        .map(|_| ())
}
