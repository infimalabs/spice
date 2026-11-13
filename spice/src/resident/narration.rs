//! Resident-owned bounds on active work without visible prose.
//!
//! Call thresholds and monotonic deadlines share one prod ordinal. The first
//! reminder is due after the configured call bound or three minutes; the second
//! is due after another call bound or five minutes. Prose resets both measures,
//! and explicit turn completion stops the clock. A prod is a durable fact, not
//! a directive: it asks for progress, never an acknowledgment.

use std::collections::BTreeMap;
use std::io;
use std::time::{Duration, Instant as Monotonic};

use bus::Bus;
use journal::{FactCause, GuardError, JournalGuard, JournalWrite};
use wire::body::{NarrationCleared, NarrationProdded, NarrationRelief};
use wire::{Actor, CanonicalUuid, Outcome, Producer};

use super::{
    CommandResult, RuntimePaths, commit, current_instant, lane_origin, store_fault, supervisor,
};

const REMIND_AFTER: Duration = Duration::from_secs(180);
const ESCALATE_AFTER: Duration = Duration::from_secs(300);

#[cfg(test)]
mod tests;

/// Per-lane silent streaks, folded from live capture records in tape order.
#[derive(Debug, Default)]
pub(super) struct Streaks {
    lanes: BTreeMap<CanonicalUuid, Streak>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Streak {
    occupant: CanonicalUuid,
    moments: u32,
    prods: u32,
    active_since: Option<Monotonic>,
}

/// One durable step the fold asks the resident to record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Step {
    Prodded(NarrationProdded),
    Cleared(NarrationCleared),
}

impl Streaks {
    /// Fold live records in tape order. Zero disables the closing-seat sweep.
    pub(super) fn observe(
        &mut self,
        bound: u32,
        record: &lane::capture::CaptureRecord,
    ) -> Vec<Step> {
        let mut steps = Vec::new();
        if bound == 0 {
            self.lanes.remove(&record.lane);
            return steps;
        }
        let Some(context) = record.decoded.context() else {
            return steps;
        };
        let now = Monotonic::now();
        let streak = self
            .lanes
            .entry(record.lane)
            .or_insert_with(|| Streak::new(record.occupant));
        if streak.occupant != record.occupant {
            *streak = Streak::new(record.occupant);
        }
        if context.turn == Some(listen::TurnMoment::Started) {
            streak.clear(record, NarrationRelief::Turn, &mut steps);
            streak.active_since = Some(now);
        }
        for event in record.decoded.events() {
            match event {
                listen::Event::Prose(prose)
                    if prose.role == listen::ProseRole::Assistant
                        && !prose.text.trim().is_empty() =>
                {
                    streak.clear(record, NarrationRelief::Prose, &mut steps);
                    streak.active_since = Some(now);
                }
                listen::Event::Tool(tool)
                    if matches!(tool.moment, listen::ToolMoment::Call { .. }) =>
                {
                    streak.active_since.get_or_insert(now);
                    streak.moments = streak.moments.saturating_add(1);
                    let due = u64::from(bound) * (u64::from(streak.prods) + 1);
                    if u64::from(streak.moments) >= due {
                        streak.prods = streak.prods.saturating_add(1);
                        steps.push(streak.prod(record.lane));
                    }
                }
                listen::Event::Reasoning(_) => {
                    streak.active_since.get_or_insert(now);
                }
                _ => {}
            }
        }
        if context.prompt_outcome.is_some() {
            streak.clear(record, NarrationRelief::Turn, &mut steps);
            streak.active_since = None;
        }
        steps
    }

    pub(super) fn next_deadline(&self) -> Option<Monotonic> {
        self.lanes.values().filter_map(Streak::deadline).min()
    }

    /// Re-prove occupant identity before advancing a time-driven reminder.
    pub(super) fn due(
        &mut self,
        now: Monotonic,
        current: impl Fn(CanonicalUuid, CanonicalUuid) -> bool,
    ) -> Vec<Step> {
        let mut steps = Vec::new();
        self.lanes.retain(|lane, streak| {
            if !current(*lane, streak.occupant) {
                return false;
            }
            if streak.deadline().is_some_and(|deadline| deadline <= now) {
                let since = streak
                    .active_since
                    .expect("a deadline belongs to active work");
                streak.prods = if now.duration_since(since) >= ESCALATE_AFTER {
                    2
                } else {
                    1
                };
                steps.push(streak.prod(*lane));
            }
            true
        });
        steps
    }
}

pub(super) async fn wait(deadline: Option<Monotonic>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}

impl Streak {
    fn new(occupant: CanonicalUuid) -> Self {
        Self {
            occupant,
            moments: 0,
            prods: 0,
            active_since: None,
        }
    }

    fn deadline(&self) -> Option<Monotonic> {
        let since = self.active_since?;
        match self.prods {
            0 => Some(since + REMIND_AFTER),
            1 => Some(since + ESCALATE_AFTER),
            _ => None,
        }
    }

    fn prod(&self, lane: CanonicalUuid) -> Step {
        Step::Prodded(NarrationProdded {
            lane,
            occupant: self.occupant,
            moments: self.moments,
            count: self.prods,
        })
    }

    fn clear(
        &mut self,
        record: &lane::capture::CaptureRecord,
        relief: NarrationRelief,
        steps: &mut Vec<Step>,
    ) {
        if self.prods > 0 {
            steps.push(Step::Cleared(NarrationCleared {
                lane: record.lane,
                occupant: record.occupant,
                moments: self.moments,
                prods: self.prods,
                relief,
            }));
        }
        self.moments = 0;
        self.prods = 0;
    }
}

/// Record every step one record produced, each guarded on the occupant still
/// holding its seat under this epoch.
///
/// A refusal means the seat moved between the fold and the commit; the
/// streak it described is over either way, so it is diagnosed and dropped.
pub(super) fn record(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    steps: Vec<Step>,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    for step in steps {
        let (lane, occupant) = match &step {
            Step::Prodded(prodded) => (prodded.lane, prodded.occupant),
            Step::Cleared(cleared) => (cleared.lane, cleared.occupant),
        };
        let origin = lane_origin(lane)?;
        let at = current_instant()?;
        let mut proposal =
            JournalWrite::new(Actor::Spice, origin, at, Producer::ResidentRecognition);
        proposal.guard(JournalGuard::new(move |read| {
            let residency = read
                .residency_head()?
                .ok_or_else(|| GuardError::refused("narration::residency-missing"))?;
            if residency.body().epoch != epoch
                || !residency
                    .body()
                    .lanes
                    .iter()
                    .any(|row| row.lane == lane && row.occupant == Some(occupant))
            {
                return Err(GuardError::refused("narration::occupant-moved"));
            }
            Ok(())
        }));
        let prod = match &step {
            Step::Prodded(prodded) => Some(prodded.clone()),
            Step::Cleared(_) => None,
        };
        let built = match step {
            Step::Prodded(prodded) => proposal.fact(prodded, FactCause::Root),
            Step::Cleared(cleared) => proposal.fact(cleared, FactCause::Root),
        };
        built.map_err(|error| store_fault("narration::step-invalid", &error.to_string()))?;
        match commit(bus, proposal) {
            Ok(_) => {
                if let Some(prod) = &prod {
                    supervisor::command::narrate(paths, prod);
                }
            }
            Err(Outcome::Refusal(refusal)) => {
                let _ = writeln!(
                    diagnostic,
                    "narration step not recorded for lane {lane}: {}",
                    refusal.reason()
                );
            }
            Err(outcome) => return Err(outcome),
        }
    }
    Ok(())
}
