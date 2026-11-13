//! Task settlement commands sent over the authenticated browser socket.

use super::*;

pub(super) async fn send_task_close(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    id: CanonicalUuid,
    expected: &[Witness],
    close: BorrowedTaskClose<'_>,
) -> Result<CanonicalUuid, String> {
    let born = close.task.born().map_err(|error| {
        format!(
            "task handle {} has no canonical birth stamp: {error}",
            close.task
        )
    })?;
    send_typed_as(
        principal,
        writer,
        actions,
        TrackedCommand {
            id,
            pending_kind: PendingKind::Close { task: born },
            kind: CommandKind::TaskClose,
            expected: expected.to_vec(),
        },
        close,
    )
    .await
}

#[derive(Serialize)]
pub(super) struct BorrowedTaskClose<'a> {
    pub(super) task: &'a TaskHandle,
    pub(super) validation: &'a str,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(super) force_claimed: bool,
}

pub(super) async fn send_task_delete(
    principal: &Principal,
    writer: &mut SocketWriter,
    actions: &ActionQueue,
    id: CanonicalUuid,
    expected: &[Witness],
    delete: BorrowedTaskDelete<'_>,
) -> Result<CanonicalUuid, String> {
    let born = delete.task.born().map_err(|error| {
        format!(
            "task handle {} has no canonical birth stamp: {error}",
            delete.task
        )
    })?;
    send_typed_as(
        principal,
        writer,
        actions,
        TrackedCommand {
            id,
            pending_kind: PendingKind::Delete { task: born },
            kind: CommandKind::TaskDelete,
            expected: expected.to_vec(),
        },
        delete,
    )
    .await
}

#[derive(Serialize)]
pub(super) struct BorrowedTaskDelete<'a> {
    pub(super) task: &'a TaskHandle,
    pub(super) reason: &'a str,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(super) force_claimed: bool,
}
