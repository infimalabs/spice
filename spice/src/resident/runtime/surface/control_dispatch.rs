//! Fair resident command dispatch, including deferred launch replies.

use super::*;
pub(in crate::resident) enum ControlDeliveryTurn {
    Control,
    Delivery,
}

pub(in crate::resident) async fn control_delivery_turn(
    control: &mut serve::axum::Control,
    inbox_deliveries: &InboxDeliveries,
    control_enabled: bool,
) -> (ControlDeliveryTurn, Option<serve::axum::ControlRequest>) {
    if inbox_deliveries.is_empty() {
        return if control_enabled {
            (ControlDeliveryTurn::Control, control.recv().await)
        } else {
            std::future::pending().await
        };
    }
    if !control_enabled {
        return (ControlDeliveryTurn::Delivery, None);
    }
    tokio::select! {
        biased;
        request = control.recv() => (ControlDeliveryTurn::Control, request),
        () = std::future::ready(()) => (ControlDeliveryTurn::Delivery, None),
    }
}

/// The app command door, quota sampling, and effects their dispatched commands
/// owe.
pub(super) struct Controls {
    pub(super) control: serve::axum::Control,
    pub(super) inbox_deliveries: InboxDeliveries,
    pub(super) live_changes: lane_surface::LiveChanges,
    pub(super) quota: quota::Runtime,
}

pub(super) async fn dispatch_control_delivery_burst(
    command_surface: &command_surface::Surface<'_>,
    seats: &mut Seats,
    wakes: &mut Wakes,
    controls: &mut Controls,
    request: Option<serve::axum::ControlRequest>,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<bool> {
    let dispatched = dispatch_control_burst(
        command_surface,
        seats,
        &mut wakes.lifecycle,
        &mut wakes.launches,
        controls,
        request,
        diagnostic,
    )
    .await?;
    if let Some(witness) = dispatched.lifecycle_witness {
        wakes.reconciliation.request(witness);
    }
    if !dispatched.reached_dark {
        deliver_owed_inbox(command_surface, &mut controls.inbox_deliveries).await?;
    }
    Ok(dispatched.reached_dark)
}

#[derive(Default)]
pub(super) struct ControlDispatch {
    pub(super) reached_dark: bool,
    pub(super) lifecycle_witness: Option<lifecycle::ReconciliationWitness>,
    pub(super) inbox_lanes: Vec<CanonicalUuid>,
}

pub(super) async fn dispatch_control(
    command_surface: &command_surface::Surface<'_>,
    seats: &mut Seats,
    lifecycle: &mut lifecycle::Runtime,
    launches: &mut super::super::launch_runtime::Runtime,
    controls: &mut Controls,
    request: Option<serve::axum::ControlRequest>,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<ControlDispatch> {
    let request = request
        .ok_or_else(|| environment_fault("serve::control-closed", "app control receiver closed"))?;
    let replayed = command_surface.replay(seats, request.command());
    let is_replay = replayed.is_some();
    let dark = is_dark_command(request.command());
    if dark && !is_replay {
        launch_completion::settle_before_dark(
            command_surface,
            seats,
            lifecycle,
            launches,
            diagnostic,
        )
        .await?;
    }
    let lifecycle_released = if is_replay {
        false
    } else {
        match command_surface.prepare_explicit_lifecycle(lifecycle, request.command()) {
            Ok(released) => released,
            Err(Outcome::Refusal(refusal)) => {
                // A refused request leaves the resident owning every pending
                // launch and its eventual completion.
                complete_response(request, Outcome::Refusal(refusal)).await;
                return Ok(ControlDispatch::default());
            }
            Err(outcome) => return Err(outcome),
        }
    };
    let command = request.command().id();
    let epoch = request.command().epoch();
    let dispatched = replayed.map_or_else(
        || command_surface.dispatch_fresh(seats, request.command()),
        CommandDispatch::ordinary,
    );
    let (outcome, effects) = match dispatched {
        CommandDispatch::Complete { outcome, effects } => (outcome, effects),
        CommandDispatch::Launch(launch) => {
            lifecycle.begin_external_launch(launch.lane);
            launches.start_command(launch, request, lifecycle_released, command, epoch);
            return Ok(ControlDispatch::default());
        }
    };
    controls.live_changes.owe(effects.live_changes);
    let lifecycle_witness = lifecycle_released.then(|| {
        let position = match &outcome {
            Outcome::Reply(reply) => Some(reply.position),
            Outcome::Refusal(_) | Outcome::Fault(_) => None,
        };
        lifecycle::ReconciliationWitness::new(command, epoch, position)
    });
    let reached_dark = !is_replay && dark && matches!(outcome, Outcome::Reply(_));
    complete_response(request, outcome).await;
    if reached_dark {
        controls.control.close();
    }
    Ok(ControlDispatch {
        reached_dark,
        lifecycle_witness,
        inbox_lanes: effects.inbox_lanes,
    })
}

pub(super) async fn dispatch_control_burst(
    command_surface: &command_surface::Surface<'_>,
    seats: &mut Seats,
    lifecycle: &mut lifecycle::Runtime,
    launches: &mut super::super::launch_runtime::Runtime,
    controls: &mut Controls,
    request: Option<serve::axum::ControlRequest>,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<ControlDispatch> {
    let mut request = request;
    let mut burst = ControlDispatch::default();
    for turn in 0..serve::axum::Control::FAIR_CYCLE_LIMIT {
        let dispatched = dispatch_control(
            command_surface,
            seats,
            lifecycle,
            launches,
            controls,
            request.take(),
            diagnostic,
        )
        .await?;
        for lane in dispatched.inbox_lanes {
            controls.inbox_deliveries.insert(lane);
        }
        burst.reached_dark |= dispatched.reached_dark;
        if dispatched.lifecycle_witness.is_some() {
            burst.lifecycle_witness = dispatched.lifecycle_witness;
        }
        if burst.reached_dark
            || burst.lifecycle_witness.is_some()
            || turn + 1 == serve::axum::Control::FAIR_CYCLE_LIMIT
        {
            break;
        }
        request = match controls.control.try_recv() {
            Ok(request) => Some(request),
            Err(
                tokio::sync::mpsc::error::TryRecvError::Empty
                | tokio::sync::mpsc::error::TryRecvError::Disconnected,
            ) => break,
        };
    }
    Ok(burst)
}

pub(super) async fn complete_response(
    request: serve::axum::ControlRequest,
    outcome: Outcome<CommandAnswer>,
) {
    if let Ok(response) = request.respond(outcome) {
        response.wait().await;
    }
}

fn is_dark_command(command: &wire::AdmittedCommand) -> bool {
    matches!(
        command.body(),
        Intent::UpChange(wire::intent::UpChange {
            state: ResidentState::Dark
        })
    )
}
