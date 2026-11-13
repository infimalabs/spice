//! Deferring a held open Design asks its holder to park.
//!
//! When the operator or another non-holder defers an open Design a lane
//! holds, the deferral write sends that lane one operator directive carrying a
//! [`wire::rooms::StopControl`] for the exact claim win, asking it to
//! checkpoint and free. The control makes the request idempotent: deferring
//! again while the same holder keeps the same win sends nothing new, and a
//! deferral that finds the holder still owed its stop sends it.

use super::*;

use wire::CanonicalUuid;
use wire::rooms::StopControl;

/// An open Design's room as the deferral that stops it read it.
pub(super) struct Deactivation {
    anchor: Entry,
    heads: wire::intent::BoardTask,
    /// Whether a stop already stands for the holder's current claim win.
    stopped: bool,
}

impl Deactivation {
    /// Read `anchor`'s room from the deferral's own `snapshot`.
    pub(super) fn of(
        bus: &Bus,
        snapshot: &ValidatedSnapshot,
        anchor: &Entry,
    ) -> Result<Self, Outcome<CommandAnswer>> {
        let heads = task_heads(&snapshot.answer, anchor.handle())?.clone();
        let stopped = match stop_control(anchor, &heads) {
            Some(control) => stop_stands(bus, &control)?,
            None => false,
        };
        Ok(Self {
            anchor: anchor.clone(),
            heads,
            stopped,
        })
    }

    /// Stage the stop request into the deferral's task-origin `write`,
    /// guarding the claim it read and the absence of a standing stop, so a
    /// claim won or a stop sent meanwhile refuses the deferral. Returns the
    /// lane asked to park; none when no lane holds the Design or its stop
    /// already stands.
    pub(super) fn stage(
        self,
        bus: &Bus,
        command: &wire::AdmittedCommand,
        config: &steer::Config,
        write: &mut JournalWrite,
    ) -> Result<Option<CanonicalUuid>, Outcome<CommandAnswer>> {
        guard(write, vec![claim_witness(&self.heads)]);
        if self.stopped {
            return Ok(None);
        }
        let Some((lane, claim)) = active_lane_claim(&self.heads) else {
            return Ok(None);
        };
        let control = StopControl {
            room: self.anchor.born().clone(),
            claim: wire::SequenceCode::new(claim.body.claimed.get()),
        };
        let guarded = control.clone();
        write.guard(journal::JournalGuard::new(move |read| {
            if read.room_stop(&guarded)?.is_none() {
                Ok(())
            } else {
                Err(journal::GuardError::refused("room::stop-standing"))
            }
        }));
        let message = stop_message(self.anchor.handle());
        let control = control.to_string();
        let at = write.at();
        crate::resident::append_operator_directive(
            bus,
            task_origin(self.anchor.born())?,
            config,
            write,
            crate::resident::OperatorDirective {
                directive: command.id(),
                fanout: None,
                lane,
                sender: command.actor().clone(),
                text: &message,
                control: Some(&control),
                room: None,
            },
            at,
            FactCause::Existing(claim.body.claimed),
        )?;
        crate::resident::steering_envelope_admits(write)?;
        Ok(Some(lane))
    }
}

fn stop_message(anchor: &TaskHandle) -> String {
    format!(
        "Design {anchor} was deferred. Checkpoint its current work and run \
         `spice task free {anchor}` at the next safe boundary; free parks unpublished commits \
         and dirty work under the task, and your claim stays authoritative until then. Do not \
         release child-plan claims or unrelated steering or Design-request duties."
    )
}

/// The stop control a deferral of `anchor` would send its holder, absent
/// while no lane holds it.
fn stop_control(anchor: &Entry, heads: &wire::intent::BoardTask) -> Option<StopControl> {
    active_lane_claim(heads).map(|(_, claim)| StopControl {
        room: anchor.born().clone(),
        claim: wire::SequenceCode::new(claim.body.claimed.get()),
    })
}

/// Whether a stop carrying exactly `control` was ever sent, whatever its
/// disposition: a holder asked once is not asked again for the same win.
fn stop_stands(bus: &Bus, control: &StopControl) -> Result<bool, Outcome<CommandAnswer>> {
    let control = control.clone();
    bus.read(move |read| read.room_stop(&control).map(|head| head.is_some()))
        .map_err(|error| store_fault("room::stop-unreadable", error.to_string()))
}

/// The lane holding the Design's earliest-won active claim.
fn active_lane_claim(
    heads: &wire::intent::BoardTask,
) -> Option<(CanonicalUuid, &wire::intent::Positioned<wire::ClaimHead>)> {
    heads
        .active_claims()
        .find_map(|held| match held.claim.body.actor {
            Actor::Lane(lane) => Some((lane, &held.claim)),
            _ => None,
        })
}
