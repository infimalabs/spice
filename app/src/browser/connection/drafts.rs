//! Typed task changes on the live socket: creation, statement revision,
//! filing, deferral, and a Design's conclusion.

use super::*;

/// Send one typed task change under the heads its control was drawn from:
/// task creation, statement revision or filing, or a Design's conclusion.
pub(super) async fn send_task_change(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    action: &ClientAction,
) -> Result<CanonicalUuid, String> {
    match action {
        ClientAction::Draft { request, respond } => {
            send_draft(principal, writer, actions, request, respond).await
        }
        ClientAction::Conclude {
            request,
            expected,
            respond,
        } => {
            send_typed(
                principal,
                writer,
                actions,
                PendingKind::Conclude {
                    respond: respond.clone(),
                },
                CommandKind::TaskConclude,
                expected.clone(),
                request,
            )
            .await
        }
        ClientAction::Deferral {
            request,
            expected,
            respond,
        } => {
            let pending = PendingKind::Deferral {
                respond: respond.clone(),
            };
            match request {
                crate::client_action::DeferralRequest::Wake(request) => {
                    send_typed(
                        principal,
                        writer,
                        actions,
                        pending,
                        CommandKind::TaskWake,
                        expected.clone(),
                        request,
                    )
                    .await
                }
                crate::client_action::DeferralRequest::Defer(request) => {
                    send_typed(
                        principal,
                        writer,
                        actions,
                        pending,
                        CommandKind::TaskDefer,
                        expected.clone(),
                        request,
                    )
                    .await
                }
            }
        }
        _ => unreachable!("only task changes route here"),
    }
}

async fn send_draft(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    request: &crate::client_action::DraftRequest,
    respond: &crate::client_action::TaskResponse,
) -> Result<CanonicalUuid, String> {
    use crate::client_action::DraftRequest;
    // An image set moves no journal head, so the Draft's detail is read again
    // once it lands instead of waiting on a board change that never comes.
    let reread = match request {
        DraftRequest::Media { request, .. } => request.task.born().ok(),
        DraftRequest::Create { .. } | DraftRequest::Edit { .. } | DraftRequest::File { .. } => None,
    };
    let pending = PendingKind::Draft {
        respond: respond.clone(),
        reread,
    };
    match request {
        DraftRequest::Create { request, expected } => {
            send_typed(
                principal,
                writer,
                actions,
                pending,
                CommandKind::TaskAdd,
                expected.clone(),
                request,
            )
            .await
        }
        DraftRequest::Edit { request, expected } => {
            send_typed(
                principal,
                writer,
                actions,
                pending,
                CommandKind::TaskEdit,
                expected.clone(),
                request,
            )
            .await
        }
        DraftRequest::File { request, expected } => {
            send_typed(
                principal,
                writer,
                actions,
                pending,
                CommandKind::TaskFile,
                expected.clone(),
                request,
            )
            .await
        }
        DraftRequest::Media { request, expected } => {
            send_typed(
                principal,
                writer,
                actions,
                pending,
                CommandKind::TaskMedia,
                expected.clone(),
                request,
            )
            .await
        }
    }
}
