//! Typed room mutations on the live principal socket.

use super::*;

/// Post principal text to one room under the anchor head its composer read.
pub(super) async fn send_room_post(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    post: &crate::client_action::RoomPostAction,
) -> Result<CanonicalUuid, String> {
    let crate::client_action::RoomPostAction {
        room,
        head,
        text,
        respond,
    } = post;
    let expected = vec![Witness::Task {
        task: room
            .born()
            .map_err(|error| format!("room anchor is invalid: {error}"))?,
        head: *head,
    }];
    send_typed(
        principal,
        writer,
        actions,
        PendingKind::RoomPost {
            respond: respond.clone(),
        },
        CommandKind::RoomPost,
        expected,
        wire::rooms::RoomPost {
            room: room.clone(),
            text: text.to_owned(),
        },
    )
    .await
}

/// Submit one principal line control or recovery. Both commands guard their
/// exact steering input, so the app supplies no independent witness.
pub(super) async fn send_steer_control(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    request: &crate::client_action::SteerControlRequest,
    line_respond: &crate::client_action::SteerLineResponse,
    directive_respond: &crate::client_action::DirectiveResponse,
) -> Result<CanonicalUuid, String> {
    match request {
        crate::client_action::SteerControlRequest::Line(control) => {
            send_typed(
                principal,
                writer,
                actions,
                PendingKind::SteerLine {
                    respond: line_respond.clone(),
                },
                CommandKind::SteerLineControl,
                Vec::new(),
                control.clone(),
            )
            .await
        }
        crate::client_action::SteerControlRequest::Recover(reference) => {
            send_typed(
                principal,
                writer,
                actions,
                PendingKind::SteerDirective {
                    respond: directive_respond.clone(),
                },
                CommandKind::SteerRecover,
                Vec::new(),
                reference.clone(),
            )
            .await
        }
    }
}

/// Submit one preparation write under the heads its surface observed.
pub(super) async fn send_preparation(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    request: &crate::client_action::PreparationRequest,
    respond: &crate::client_action::PreparationResponse,
) -> Result<CanonicalUuid, String> {
    let pending = PendingKind::Preparation {
        respond: respond.clone(),
    };
    match request {
        crate::client_action::PreparationRequest::Decide { request, expected } => {
            send_typed(
                principal,
                writer,
                actions,
                pending,
                CommandKind::PreparationDecide,
                expected.clone(),
                (**request).clone(),
            )
            .await
        }
    }
}
