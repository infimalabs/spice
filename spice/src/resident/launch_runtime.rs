//! Off-loop supervisor proof establishment and resident-owned continuations.

use tokio::sync::mpsc;
use wire::CanonicalUuid;

use super::{CommandLaunch, CommandLaunchCompletion, CommandResult, lifecycle, supervisor};

pub(super) enum Authority {
    Command {
        request: Box<serve::axum::ControlRequest>,
        completion: CommandLaunchCompletion,
        lane: CanonicalUuid,
        lifecycle_released: bool,
        command: CanonicalUuid,
        epoch: CanonicalUuid,
    },
    Automatic(lifecycle::PendingLaunch),
}

pub(super) struct Completion {
    pub(super) authority: Authority,
    pub(super) result: CommandResult<supervisor::Launch>,
}

pub(super) struct Runtime {
    sender: mpsc::UnboundedSender<Completion>,
    receiver: mpsc::UnboundedReceiver<Completion>,
    /// Spawned launches whose completion the loop has not received yet.
    in_flight: usize,
}

impl Runtime {
    pub(super) fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            sender,
            receiver,
            in_flight: 0,
        }
    }

    /// Whether any spawned occupant is still owed its completion.
    pub(super) fn in_flight(&self) -> bool {
        self.in_flight > 0
    }

    pub(super) fn start_command(
        &mut self,
        launch: CommandLaunch,
        request: serve::axum::ControlRequest,
        lifecycle_released: bool,
        command: CanonicalUuid,
        epoch: CanonicalUuid,
    ) {
        self.start(
            launch.launch,
            Authority::Command {
                request: Box::new(request),
                completion: launch.completion,
                lane: launch.lane,
                lifecycle_released,
                command,
                epoch,
            },
        );
    }

    pub(super) fn start_automatic(
        &mut self,
        launch: supervisor::PendingLaunch,
        pending: lifecycle::PendingLaunch,
    ) {
        self.start(launch, Authority::Automatic(pending));
    }

    fn start(&mut self, launch: supervisor::PendingLaunch, authority: Authority) {
        let completed = self.sender.clone();
        self.in_flight += 1;
        tokio::task::spawn_blocking(move || {
            let result = launch.establish();
            let _resident_stopped = completed.send(Completion { authority, result });
        });
    }

    /// Receive the next completion; cancel-safe, so a losing `select!` arm
    /// neither drops a completion nor miscounts it.
    pub(super) async fn recv(&mut self) -> Completion {
        let completed = self
            .receiver
            .recv()
            .await
            .expect("launch completion sender remains owned by its runtime");
        self.in_flight -= 1;
        completed
    }
}
