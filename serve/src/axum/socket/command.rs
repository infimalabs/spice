//! Authenticated command admission, completion, and correlated answers for one session.

use super::*;

pub(super) async fn route_inbound_command(
    socket: &mut WebSocket,
    state: &Arc<SurfaceState>,
    accepted: &wire::Accepted,
    capability: wire::CanonicalUuid,
    commands: &mut PendingCommands,
    histories: &mut PendingHistories,
    bytes: &[u8],
) -> SessionFlow {
    match route_command(socket, state, accepted, commands, bytes).await {
        CommandRoute::Flow(flow) => flow,
        CommandRoute::Pending(command) if command.dark => {
            route_dark_command(
                socket, state, accepted, capability, commands, histories, *command,
            )
            .await
        }
        CommandRoute::Pending(command) => {
            commands.push(Box::pin((*command).complete(Arc::clone(state))));
            SessionFlow::Continue
        }
    }
}

async fn route_command(
    socket: &mut WebSocket,
    state: &SurfaceState,
    accepted: &wire::Accepted,
    commands: &mut PendingCommands,
    bytes: &[u8],
) -> CommandRoute {
    let crossing = match wire::decode::<wire::Command<serde_json::Value>>(bytes) {
        Ok(command) => command,
        Err(error) => {
            send_encoded(socket, &undecodable_command(error)).await;
            return CommandRoute::Flow(SessionFlow::Continue);
        }
    };
    let id = crossing.id;
    let admitted = state
        .capabilities
        .roles()
        .current()
        .map_err(|error| wire::CommandRejection::Fault(error.fault()))
        .and_then(|roles| crossing.admit(accepted, &roles));
    let command = match admitted {
        Ok(command) => command,
        Err(rejection) => {
            send_encoded(
                socket,
                &wire::Response {
                    v: wire::WireVersion::CURRENT,
                    id,
                    epoch: state.epoch,
                    outcome: rejection.outcome(),
                },
            )
            .await;
            return CommandRoute::Flow(SessionFlow::Continue);
        }
    };
    let dark = is_dark(&command);
    let response = CommandResponse::from(&command);
    let pending = match enqueue_answering(socket, state, commands, command).await {
        Err(flow) => return CommandRoute::Flow(flow),
        Ok(Ok(pending)) => pending,
        Ok(Err(EnqueueError::NotAdmitting)) if *state.closed.borrow() => {
            return CommandRoute::Flow(SessionFlow::Close);
        }
        Ok(Err(EnqueueError::NotAdmitting)) => {
            send_encoded(socket, &response.bind(closing_refusal())).await;
            return CommandRoute::Flow(SessionFlow::Continue);
        }
        Ok(Err(EnqueueError::ControlClosed)) => {
            send_encoded(socket, &response.bind(closing_refusal())).await;
            return CommandRoute::Flow(SessionFlow::Close);
        }
    };
    let PendingResponse { returned, sent } = pending;
    CommandRoute::Pending(Box::new(PendingCommand {
        response,
        returned,
        sent,
        dark,
    }))
}

/// Reserve a control slot while still answering this session's earlier commands.
///
/// The resident waits for each answer's write before it takes the next command, so
/// a session that stopped answering while it waited for a slot would hold the
/// resident on an answer only the session can send.
async fn enqueue_answering(
    socket: &mut WebSocket,
    state: &SurfaceState,
    commands: &mut PendingCommands,
    command: wire::AdmittedCommand,
) -> Result<Result<PendingResponse, EnqueueError>, SessionFlow> {
    let enqueue = state.enqueue(command);
    tokio::pin!(enqueue);
    loop {
        let answering = !commands.is_empty();
        tokio::select! {
            biased;
            pending = &mut enqueue => return Ok(pending),
            completed = commands.next(), if answering => {
                let flow = route_command_completion(socket, state, completed).await;
                if matches!(flow, SessionFlow::Close) {
                    return Err(flow);
                }
            }
        }
    }
}

enum CommandRoute {
    Flow(SessionFlow),
    Pending(Box<PendingCommand>),
}

struct PendingCommand {
    response: CommandResponse,
    returned: tokio::sync::oneshot::Receiver<wire::Outcome<wire::intent::CommandAnswer>>,
    sent: tokio::sync::oneshot::Sender<()>,
    dark: bool,
}

pub(in crate::axum) enum CommandCompletion {
    Closed,
    Returned(Box<ReturnedCommand>),
}

pub(in crate::axum) struct ReturnedCommand {
    response: CommandResponse,
    returned:
        Result<wire::Outcome<wire::intent::CommandAnswer>, tokio::sync::oneshot::error::RecvError>,
    sent: tokio::sync::oneshot::Sender<()>,
    dark: bool,
}

#[derive(Clone, Copy)]
struct CommandResponse {
    id: wire::CanonicalUuid,
    epoch: wire::CanonicalUuid,
}

impl CommandResponse {
    fn from(command: &wire::AdmittedCommand) -> Self {
        Self {
            id: command.id(),
            epoch: command.epoch(),
        }
    }

    fn bind(
        self,
        outcome: wire::Outcome<wire::intent::CommandAnswer>,
    ) -> wire::Response<wire::intent::CommandAnswer> {
        wire::Response {
            v: wire::WireVersion::CURRENT,
            id: self.id,
            epoch: self.epoch,
            outcome,
        }
    }
}

impl PendingCommand {
    async fn complete(self, state: Arc<SurfaceState>) -> CommandCompletion {
        if *state.closed.borrow() || state.connections.is_cancelled() {
            return CommandCompletion::Closed;
        }
        let mut closed = state.closed.subscribe();
        let returned = tokio::select! {
            biased;
            changed = closed.changed() => {
                let _ = changed;
                return CommandCompletion::Closed;
            },
            _ = state.connections.cancelled() => return CommandCompletion::Closed,
            returned = self.returned => returned,
        };
        CommandCompletion::Returned(Box::new(ReturnedCommand {
            response: self.response,
            returned,
            sent: self.sent,
            dark: self.dark,
        }))
    }
}

async fn route_dark_command(
    socket: &mut WebSocket,
    state: &Arc<SurfaceState>,
    accepted: &wire::Accepted,
    capability: wire::CanonicalUuid,
    commands: &mut PendingCommands,
    histories: &mut PendingHistories,
    command: PendingCommand,
) -> SessionFlow {
    while !commands.is_empty() || !histories.is_empty() {
        let command_ready = !commands.is_empty();
        let history_ready = !histories.is_empty();
        let flow = tokio::select! {
            completed = commands.next(), if command_ready => {
                route_command_completion(socket, state, completed).await
            },
            completed = histories.next(), if history_ready => {
                route_history_completion(
                    socket,
                    state,
                    accepted,
                    capability,
                    completed,
                )
                .await
            },
        };
        if matches!(flow, SessionFlow::Close) {
            return SessionFlow::Close;
        }
    }
    let completed = command.complete(Arc::clone(state)).await;
    route_command_completion(socket, state, Some(completed)).await
}

pub(super) async fn route_command_completion(
    socket: &mut WebSocket,
    state: &SurfaceState,
    completed: Option<CommandCompletion>,
) -> SessionFlow {
    let Some(CommandCompletion::Returned(completed)) = completed else {
        return SessionFlow::Close;
    };
    let ReturnedCommand {
        response,
        returned,
        sent,
        dark,
    } = *completed;
    let Ok(outcome) = returned else {
        return SessionFlow::Close;
    };
    let reached_dark = dark && matches!(outcome, wire::Outcome::Reply(_));
    send_encoded(socket, &response.bind(outcome)).await;
    let _ = sent.send(());
    if dark && !reached_dark {
        state.reopen_after_dark_refusal().await;
    }
    if reached_dark {
        SessionFlow::Close
    } else {
        SessionFlow::Continue
    }
}

fn undecodable_command(error: serde_json::Error) -> wire::Fault {
    wire::Fault {
        class: wire::FaultClass::Protocol,
        reason: "command::undecodable".to_owned(),
        detail: error.to_string(),
    }
}

fn closing_refusal() -> wire::Outcome<wire::intent::CommandAnswer> {
    wire::Outcome::Refusal(wire::Refusal::bare(wire::RefusalDiagnostic::new(
        "resident::not-admitting",
        Some("resident-state".to_owned()),
        Some("seated".to_owned()),
        Some("quiescing".to_owned()),
    )))
}
