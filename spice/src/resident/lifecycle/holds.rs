use super::*;

impl Runtime {
    /// Classify one observed occupant death through the shared hold reducer.
    ///
    /// `seated` is the occupant's own seating, or nothing for an exit that
    /// named no seated occupant, which is nobody's death. A death is young
    /// only when the occupant died inside the young window without settling
    /// its wake: an occupant whose transcript ends on its own answer and
    /// leaves no direction pending did the work its wake asked for, however
    /// quickly, so its exit says nothing against the seat. One that answered
    /// but left the direction it was woken for live hands the same wake to
    /// its successor, so its exit counts like silence.
    pub(in crate::resident) fn observe_death(
        &mut self,
        bus: &Bus,
        epoch: CanonicalUuid,
        seated: Option<Activity>,
        lane: CanonicalUuid,
        settled: bool,
        diagnostic: &mut dyn std::io::Write,
    ) -> CommandResult<()> {
        let Some(activity) = seated else {
            return Ok(());
        };
        let death = if !settled && activity.at().elapsed() < self.young {
            Death::Young
        } else {
            Death::Mature
        };
        let until = hold_deadline(self.hold, current_instant()?)?;
        let Some(held) = self.holds.died(lane, death, until) else {
            return Ok(());
        };
        self.commit_hold(bus, epoch, held, diagnostic)
    }

    /// Hold a lane whose occupant's transcript ended on a vendor failure.
    ///
    /// The vendor refused the turn this seat resumes into, so a successor
    /// resumes into the same refusal a few seconds later and the lane spends
    /// its budget on wakes that say nothing new. One such exit holds the lane
    /// at once, with the vendor's own text journaled against the occupant as
    /// the hold's evidence, rather than waiting for a run of young deaths to
    /// reach the limit.
    pub(in crate::resident) fn observe_vendor_failure(
        &mut self,
        bus: &Bus,
        epoch: CanonicalUuid,
        exit: lane::capture::CaptureExit,
        failure: &str,
        diagnostic: &mut dyn std::io::Write,
    ) -> CommandResult<()> {
        let now = current_instant()?;
        self.observe_vendor_failure_at(bus, epoch, exit, failure, now, diagnostic)
    }

    /// [`Self::observe_vendor_failure`] with the clock supplied, so the hold
    /// deadline a fixture asserts is the deadline the reducer was handed.
    pub(super) fn observe_vendor_failure_at(
        &mut self,
        bus: &Bus,
        epoch: CanonicalUuid,
        exit: lane::capture::CaptureExit,
        failure: &str,
        now: Instant,
        diagnostic: &mut dyn std::io::Write,
    ) -> CommandResult<()> {
        let until = hold_deadline(self.hold, now)?;
        let evidence = vendor_failure_evidence(exit.occupant, failure);
        let input = vendor_failure_input(bus, epoch, exit.lane)?;
        let Some(held) = self.holds.failed(exit.lane, evidence, until, input) else {
            return Ok(());
        };
        self.commit_hold(bus, epoch, held, diagnostic)
    }

    /// Journal one hold the reducer already recorded, forgetting it again
    /// when the journal refuses so the reducer and the journal agree, and
    /// write the hold down where an operator reading why a lane went quiet
    /// will look.
    fn commit_hold(
        &mut self,
        bus: &Bus,
        epoch: CanonicalUuid,
        held: wire::body::LaneHeld,
        diagnostic: &mut dyn std::io::Write,
    ) -> CommandResult<()> {
        let lane = held.lane;
        let observed = current_residency(bus, epoch)?;
        let mut proposal = write(Actor::Spice, epoch, Producer::BusTimer)?;
        guard_residency(&mut proposal, &observed);
        let line = format!(
            "lifecycle held lane={lane} until={} evidence={}",
            held.until, held.evidence
        );
        proposal
            .fact(held, FactCause::Existing(observed.position()))
            .map_err(|error| store_fault("lifecycle::hold-fact-invalid", &error.to_string()))?;
        match commit(bus, proposal) {
            Ok(_) => {
                let _ = writeln!(diagnostic, "{line}");
                Ok(())
            }
            Err(outcome) => {
                let _ = self.holds.wake(lane);
                Err(outcome)
            }
        }
    }

    /// Release the first vendor hold whose refused session or operator
    /// direction set changed. The committed release is then admitted through
    /// the ordinary snapshot loop before another hold is considered.
    pub(super) fn release_changed_vendor_hold(
        &mut self,
        bus: &Bus,
        epoch: CanonicalUuid,
        snapshot: &bus::TaskSnapshot,
    ) -> CommandResult<bool> {
        let Some(residency) = snapshot.residency() else {
            return Ok(false);
        };
        for row in &residency.body().lanes {
            let Some(refused) = self.holds.vendor_failure(row.lane).cloned() else {
                continue;
            };
            let current = wire::body::VendorFailureInput {
                session: row.session.as_ref().map(|session| session.identity.clone()),
                directives: self.projection.operator_steering(row.lane),
                renewal: residency
                    .body()
                    .renewals
                    .iter()
                    .find(|renewal| renewal.lane == row.lane)
                    .map(|renewal| renewal.revision),
            };
            let until = self.holds.held(row.lane);
            let resumed = self.holds.input_changed(row.lane, &current);
            if resumed.is_some() {
                self.commit_resume(bus, epoch, row.lane, until, Some(refused), resumed)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The deadline the lane is held until, when it is held.
    #[cfg(test)]
    pub(super) fn held_until(&self, lane: CanonicalUuid) -> Option<Instant> {
        self.holds.held(lane)
    }

    /// Release one hold from its explicit timer event.
    pub(in crate::resident) fn expire_hold(
        &mut self,
        bus: &Bus,
        epoch: CanonicalUuid,
        lane: CanonicalUuid,
    ) -> CommandResult<()> {
        let now = current_instant()?;
        let until = self.holds.held(lane);
        let vendor_failure = self.holds.vendor_failure(lane).cloned();
        let resumed = self.holds.expire(lane, now);
        self.commit_resume(bus, epoch, lane, until, vendor_failure, resumed)
    }

    /// Release one hold because an operator explicitly requested wake.
    pub(in crate::resident) fn operator_wake(
        &mut self,
        bus: &Bus,
        epoch: CanonicalUuid,
        lane: CanonicalUuid,
    ) -> CommandResult<()> {
        self.backoff.release(lane);
        let until = self.holds.held(lane);
        let vendor_failure = self.holds.vendor_failure(lane).cloned();
        let resumed = self.holds.wake(lane);
        self.commit_resume(bus, epoch, lane, until, vendor_failure, resumed)
    }

    fn commit_resume(
        &mut self,
        bus: &Bus,
        epoch: CanonicalUuid,
        lane: CanonicalUuid,
        until: Option<Instant>,
        vendor_failure: Option<wire::body::VendorFailureInput>,
        resumed: Option<wire::body::LaneResumed>,
    ) -> CommandResult<()> {
        let Some(resumed) = resumed else {
            return Ok(());
        };
        let observed = current_residency(bus, epoch)?;
        let mut proposal = write(Actor::Spice, epoch, Producer::BusTimer)?;
        guard_residency(&mut proposal, &observed);
        proposal
            .fact(resumed, FactCause::Existing(observed.position()))
            .map_err(|error| store_fault("lifecycle::resume-fact-invalid", &error.to_string()))?;
        match commit(bus, proposal) {
            Ok(_) => Ok(()),
            Err(outcome) => {
                if let Some(until) = until {
                    self.holds.absorb_held(&wire::body::LaneHeld {
                        lane,
                        evidence: if vendor_failure.is_some() {
                            "restored occupant ended on a vendor error: refused release".to_owned()
                        } else {
                            "durable young-death hold restored after refused release".to_owned()
                        },
                        until,
                        vendor_failure,
                    });
                }
                Err(outcome)
            }
        }
    }

    pub(super) fn synchronize_holds(
        &mut self,
        snapshot: &bus::TaskSnapshot,
        cursor: Option<Seq>,
    ) -> CommandResult<()> {
        for envelope in snapshot.facts().after(cursor) {
            match envelope.fact.kind() {
                wire::FactKind::LaneHeld => {
                    let held = fact_body::<wire::body::LaneHeld>(envelope)?;
                    self.holds.absorb_held(&held);
                }
                wire::FactKind::LaneResumed => {
                    let resumed = fact_body::<wire::body::LaneResumed>(envelope)?;
                    self.holds.absorb_resumed(&resumed);
                }
                _ => {}
            }
        }
        Ok(())
    }
}
/// Exact input a successor would resume after the vendor failure.
fn vendor_failure_input(
    bus: &Bus,
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
) -> CommandResult<wire::body::VendorFailureInput> {
    let observed = current_residency(bus, epoch)?;
    let row = observed
        .body()
        .lanes
        .iter()
        .find(|row| row.lane == lane)
        .ok_or_else(|| {
            store_fault(
                "lifecycle::held-lane-missing",
                &format!("vendor failure names lane {lane} outside current residency"),
            )
        })?;
    Ok(wire::body::VendorFailureInput {
        session: row.session.as_ref().map(|session| session.identity.clone()),
        directives: pending_operator_directives(bus, lane)?,
        renewal: observed
            .body()
            .renewals
            .iter()
            .find(|renewal| renewal.lane == lane)
            .map(|renewal| renewal.revision),
    })
}

fn pending_operator_directives(
    bus: &Bus,
    lane: CanonicalUuid,
) -> CommandResult<Vec<CanonicalUuid>> {
    let mut start = journal::FactPageStart::After(None);
    let mut directives = Vec::new();
    loop {
        let page = bus
            .read(move |read| read.pending_operator_steering(start))
            .map_err(|error| {
                store_fault("lifecycle::steering-index-unreadable", &error.to_string())
            })?;
        directives.extend(
            page.rows()
                .iter()
                .filter(|head| head.body().recipient == wire::Actor::Lane(lane))
                .map(|head| head.body().directive),
        );
        let Some(overflow) = page.overflow() else {
            return Ok(directives);
        };
        start = journal::FactPageStart::At(overflow.continuation);
    }
}

/// The deadline a hold taken now runs until, under the configured lane hold.
fn hold_deadline(hold: Duration, now: Instant) -> CommandResult<Instant> {
    let seconds = i64::try_from(hold.as_secs()).map_err(|_| {
        super::super::environment_fault(
            "lifecycle::hold-range",
            "the configured lane hold does not fit a signed deadline",
        )
    })?;
    after(now, seconds)
}

/// Longest vendor failure text a hold carries as evidence.
const VENDOR_FAILURE_EVIDENCE_BYTES: usize = 512;

/// The evidence a vendor failure hold names: the occupant that ended on the
/// failure and the vendor's own words, cut at a character boundary so one
/// verbose vendor does not fill the journal.
fn vendor_failure_evidence(occupant: CanonicalUuid, failure: &str) -> String {
    let failure = failure.trim();
    let mut end = failure.len().min(VENDOR_FAILURE_EVIDENCE_BYTES);
    while !failure.is_char_boundary(end) {
        end -= 1;
    }
    let cut = if end < failure.len() { "…" } else { "" };
    format!(
        "occupant {occupant} ended on a vendor error: {}{cut}",
        &failure[..end]
    )
}
