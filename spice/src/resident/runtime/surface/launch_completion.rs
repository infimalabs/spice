//! Resident-owned completion of off-loop supervisor proof establishment.

use super::*;

#[cfg(test)]
mod tests;

/// What one settled launch still owes the live loop.
enum Settled {
    /// An operator launch answered its waker; a released lifecycle owes this
    /// reconciliation witness.
    Command(Option<lifecycle::ReconciliationWitness>),
    Automatic,
}

pub(super) async fn accept(
    command_surface: &command_surface::Surface<'_>,
    seats: &mut Seats,
    wakes: &mut Wakes,
    diagnostic: &mut dyn io::Write,
    completed: super::super::launch_runtime::Completion,
) -> CommandResult<()> {
    match settle(
        command_surface,
        seats,
        &mut wakes.lifecycle,
        diagnostic,
        completed,
    )
    .await?
    {
        Settled::Command(witness) => {
            if let Some(witness) = witness {
                wakes.reconciliation.request(witness);
            }
            wakes.settle_for_control(command_surface, seats, diagnostic)
        }
        Settled::Automatic => wakes.reconcile(command_surface, seats, diagnostic),
    }
}

/// Settle every launch still proving its session before the resident goes
/// dark.
///
/// A successor generation adopts only occupants committed to the residency
/// head, so a completion the dark resident never receives would leave its
/// spawned supervisor running unowned beside the claim or steering that woke
/// it. Each pending proof instead finishes through the ordinary success commit
/// or failure compensation, and its waker gets its ordinary answer. Dispatch
/// holds the loop meanwhile, so no further launch begins; each proof is
/// bounded by the supervisor start horizon.
pub(super) async fn settle_before_dark(
    command_surface: &command_surface::Surface<'_>,
    seats: &mut Seats,
    lifecycle: &mut lifecycle::Runtime,
    launches: &mut super::super::launch_runtime::Runtime,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    while launches.in_flight() {
        let completed = launches.recv().await;
        settle(command_surface, seats, lifecycle, diagnostic, completed).await?;
    }
    Ok(())
}

/// Settle one launch. A launch that seated its occupant and committed its
/// start kicks that lane's inbox: the supervisor's earlier reads ran before
/// the resident named the occupant and refused, so this is the first read
/// that can answer the steering that woke or awaits the lane.
async fn settle(
    command_surface: &command_surface::Surface<'_>,
    seats: &mut Seats,
    lifecycle: &mut lifecycle::Runtime,
    diagnostic: &mut dyn io::Write,
    completed: super::super::launch_runtime::Completion,
) -> CommandResult<Settled> {
    let (started, settled) = match completed.authority {
        super::super::launch_runtime::Authority::Command {
            request,
            completion,
            lane,
            lifecycle_released,
            command,
            epoch,
        } => {
            lifecycle.finish_external_launch(lane);
            let outcome = command_surface.complete_command_launch(
                seats,
                request.command(),
                completion,
                completed.result,
            );
            let position = match &outcome {
                Outcome::Reply(reply) => Some(reply.position),
                Outcome::Refusal(_) | Outcome::Fault(_) => None,
            };
            let witness = lifecycle_released
                .then(|| lifecycle::ReconciliationWitness::new(command, epoch, position));
            control_dispatch::complete_response(*request, outcome).await;
            (position.map(|_| lane), Settled::Command(witness))
        }
        super::super::launch_runtime::Authority::Automatic(pending) => {
            let started = command_surface.complete_automatic_launch(
                lifecycle,
                seats,
                diagnostic,
                pending,
                completed.result,
            )?;
            (started, Settled::Automatic)
        }
    };
    if let Some(lane) = started {
        command_surface.notify_inbox(lane);
    }
    Ok(settled)
}
