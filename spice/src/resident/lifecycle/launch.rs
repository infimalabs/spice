//! Automatic-launch authority retained across off-loop session proof.

use super::*;

pub(in crate::resident) struct PendingLaunch {
    trigger: Trigger,
    lane: CanonicalUuid,
    /// The steering an inbox wake was admitted to deliver.
    woken: Option<super::super::steering::WokenInbox>,
    claim: Option<crate::board_command::AutomaticClaim>,
    wake: lane_surface::PendingAutomaticWake,
}

pub(super) struct Launching {
    pub(super) launch: super::super::supervisor::PendingLaunch,
    pub(super) pending: PendingLaunch,
}

pub(super) fn pending_launch(
    trigger: Trigger,
    lane: CanonicalUuid,
    woken: Option<super::super::steering::WokenInbox>,
    claim: Option<crate::board_command::AutomaticClaim>,
    launch: lane_surface::AutomaticLaunch,
) -> Attempted {
    let (launch, wake) = launch.into_parts();
    Attempted::Launching(Launching {
        launch,
        pending: PendingLaunch {
            trigger,
            lane,
            woken,
            claim,
            wake,
        },
    })
}

impl Runtime {
    /// Settle one automatic launch, answering the lane it seated and started.
    pub(in crate::resident) fn complete_launch(
        &mut self,
        resident: &Resident<'_>,
        seats: &mut Seats,
        diagnostic: &mut dyn std::io::Write,
        pending: PendingLaunch,
        launched: CommandResult<super::super::supervisor::Launch>,
    ) -> CommandResult<Option<CanonicalUuid>> {
        let &Resident {
            bus, paths, epoch, ..
        } = resident;
        self.launching.remove(&pending.lane);
        let result =
            lane_surface::finish_automatic_wake(bus, paths, epoch, seats, pending.wake, launched);
        let attempted = settle_launch(
            resident,
            pending.trigger,
            pending.lane,
            pending.woken.as_ref(),
            pending.claim.as_ref(),
            result,
        );
        match attempted {
            Ok(Attempted::Started) => {
                self.backoff.release(pending.lane);
                let _ = writeln!(
                    diagnostic,
                    "lifecycle started trigger={} lane={}",
                    pending.trigger.as_str(),
                    pending.lane
                );
                Ok(Some(pending.lane))
            }
            Ok(Attempted::DeadLettered(dead)) => {
                self.backoff.release(pending.lane);
                dead.log(diagnostic, pending.trigger.as_str(), pending.lane);
                Ok(None)
            }
            Ok(Attempted::Launching(_)) => {
                unreachable!("one launch completion cannot begin another launch")
            }
            Ok(Attempted::Retry(_) | Attempted::TrustReconciled) => {
                unreachable!("retained-seat delivery does not establish a new process")
            }
            Err(Outcome::Refusal(refusal)) => {
                let now = current_instant()?;
                let _ = self.backoff.refused(
                    pending.trigger,
                    pending.lane,
                    refusal.reason(),
                    now,
                    diagnostic,
                )?;
                Ok(None)
            }
            Err(Outcome::Fault(fault)) => Err(Outcome::Fault(fault)),
            Err(Outcome::Reply(_)) => {
                unreachable!("unit lifecycle failures carry no reply")
            }
        }
    }
}

fn settle_launch(
    resident: &Resident<'_>,
    trigger: Trigger,
    lane: CanonicalUuid,
    woken: Option<&super::super::steering::WokenInbox>,
    claim: Option<&crate::board_command::AutomaticClaim>,
    launched: CommandResult<lane_surface::AutomaticWake>,
) -> CommandResult<Attempted> {
    let &Resident {
        bus,
        repository,
        paths,
        epoch,
        config,
        ..
    } = resident;
    let host = crate::command_client::Host {
        repository,
        ambient: &config.ambient,
    };
    match trigger {
        Trigger::PendingInbox => {
            let woken = woken.expect("an inbox wake retains the steering it was woken for");
            match launched {
                Ok(lane_surface::AutomaticWake::Started) => Ok(Attempted::Started),
                Ok(lane_surface::AutomaticWake::Unstarted(failure)) => {
                    super::super::steering::dead_letter_pending(
                        bus, paths, epoch, lane, woken, &failure,
                    )
                    .map(Attempted::DeadLettered)
                }
                Err(failure) if pending_start_failure_is_terminal(&failure) => {
                    super::super::steering::dead_letter_pending(
                        bus, paths, epoch, lane, woken, &failure,
                    )
                    .map(Attempted::DeadLettered)
                }
                Err(failure) => Err(failure),
            }
        }
        Trigger::HeldClaim | Trigger::DrainQueue => match (launched, claim) {
            (Ok(lane_surface::AutomaticWake::Started), _) => Ok(Attempted::Started),
            (Ok(lane_surface::AutomaticWake::Unstarted(failure)) | Err(failure), Some(claim)) => {
                release_unstarted(bus, host, claim, failure)
            }
            (Ok(lane_surface::AutomaticWake::Unstarted(failure)) | Err(failure), None) => {
                assert_eq!(
                    trigger,
                    Trigger::HeldClaim,
                    "work launch retains its reserved claim"
                );
                Err(failure)
            }
        },
    }
}

/// Begin one work launch under the claim it reserves, if any; a launch that
/// did not start releases that reservation.
pub(super) fn begin_work(
    resident: &Resident<'_>,
    seats: &mut Seats,
    (trigger, lane): (Trigger, CanonicalUuid),
    occupant: CanonicalUuid,
    decision: Option<&bus::TaskSnapshot>,
    reserved: Option<crate::board_command::AutomaticClaim>,
) -> CommandResult<Attempted> {
    let &Resident {
        bus,
        repository,
        paths,
        epoch,
        lane_runtime,
        config,
        ..
    } = resident;
    let launched = lane_surface::begin_automatic_wake(
        bus,
        paths,
        epoch,
        seats,
        lane_runtime,
        &lane_surface::WakeRequest {
            lane,
            occupant: reserved.as_ref().map_or(occupant, |claim| claim.occupant),
            decision,
            reserved: reserved.as_ref(),
            inbox: false,
        },
    );
    match (launched, reserved) {
        (Ok(lane_surface::BeginAutomaticWake::Launch(launch)), reserved) => {
            Ok(pending_launch(trigger, lane, None, reserved, launch))
        }
        (Ok(lane_surface::BeginAutomaticWake::Unstarted(failure)) | Err(failure), Some(claim)) => {
            let host = crate::command_client::Host::new(repository, &config.ambient);
            release_unstarted(bus, host, &claim, failure)
        }
        (Ok(lane_surface::BeginAutomaticWake::Unstarted(failure)) | Err(failure), None) => {
            Err(failure)
        }
    }
}

/// The automatic claim one vacant lane holds for an occupant it never seated:
/// a reservation an allocator or invitation minted ahead of its launch.
pub(super) fn unlaunched_claim(
    snapshot: &bus::TaskSnapshot,
    lane: CanonicalUuid,
) -> Option<crate::board_command::AutomaticClaim> {
    let row = snapshot
        .residency()?
        .body()
        .lanes
        .iter()
        .find(|row| row.lane == lane)?;
    snapshot.tasks().iter().find_map(|heads| {
        let claim = heads
            .claims()
            .iter()
            .map(|held| held.claim().body())
            .find(|claim| {
                claim.disposition == wire::ClaimDisposition::Active && claim.lane == lane
            })?;
        (crate::board_command::is_automatic_claim(claim, snapshot.facts())
            && !row.lineage.contains(&claim.occupant))
        .then(|| crate::board_command::AutomaticClaim {
            task: heads.statement().body().born.clone(),
            lane,
            occupant: claim.occupant,
        })
    })
}

/// Release the reserved claim a launch did not start, answering its failure.
fn release_unstarted(
    bus: &Bus,
    host: crate::command_client::Host<'_>,
    claim: &crate::board_command::AutomaticClaim,
    failure: Outcome<()>,
) -> CommandResult<Attempted> {
    match crate::board_command::release_automatic(bus, host, claim) {
        Ok(()) => Err(failure),
        Err(release) => Err(typed_outcome(release)),
    }
}
