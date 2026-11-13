//! The operator's newest pass feedback as an open Design's next input.
//!
//! Returns and output-by-output decisions record what the operator sent
//! back or accepted from one completed pass. Each open, undeferred
//! Design whose claim an occupied lane holds is owed
//! its newest feedback until that exact occupant's supervisor confirms enqueue
//! and the resident commits `room::relayed` for the still-current claim.
//! Failed or stale attempts remain owed. A holder present at the decision hears
//! it; an unheld Design tells its next claimant. Completed delivery survives
//! restart. Only the newest feedback is input; older decisions stay history.

use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use wire::CanonicalUuid;
use wire::rooms::{
    DecisionOutcome, DecisionRef, DesignReturnDelivered, PreparationDecided, PreparationDecision,
};

/// One return an occupied holder of an open, undeferred Design is owed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReturnOwed {
    pub(crate) lane: CanonicalUuid,
    pub(crate) occupant: CanonicalUuid,
    /// Fact that won the claim the delivery serves.
    pub(crate) claim: Seq,
    pub(crate) design: TaskHandle,
    pub(crate) decision: DecisionRef,
    pub(crate) reason: String,
}

/// Every return owed to the occupied holders in `occupied` (lane to its
/// current occupant), in Design order.
pub(crate) fn owed_returns(
    bus: &Bus,
    occupied: &BTreeMap<CanonicalUuid, CanonicalUuid>,
) -> Result<Vec<ReturnOwed>, Outcome<CommandAnswer>> {
    let snapshot = bus
        .task_snapshot()
        .map_err(|error| store_fault("room::return-snapshot-unreadable", error.to_string()))?;
    let mut owed = Vec::new();
    let mut since: Option<Seq> = None;
    for heads in snapshot.tasks() {
        let statement = heads.statement().body();
        let workflow = heads.workflow().body();
        if !wire::intent::is_open_design(statement.intent, workflow) || workflow.deferral.is_some()
        {
            continue;
        }
        // A return is recorded once per decision, so it reaches one holder:
        // the earliest-won active claim on an occupied lane.
        let Some((claim, occupant)) = heads
            .claims()
            .iter()
            .map(|held| held.claim().body())
            .filter(|claim| claim.disposition == ClaimDisposition::Active)
            .find_map(|claim| occupied.get(&claim.lane).map(|occupant| (claim, occupant)))
        else {
            continue;
        };
        let Some((seq, decision, reason)) = newest_feedback(bus, &statement.born)? else {
            continue;
        };
        since = Some(since.map_or(seq, |since| since.min(seq)));
        owed.push(ReturnOwed {
            lane: claim.lane,
            occupant: *occupant,
            claim: claim.claimed,
            design: TaskHandle::for_topic(&statement.topic.path, &statement.born),
            decision: decision.decision,
            reason,
        });
    }
    let Some(since) = since else {
        return Ok(owed);
    };
    let delivered = delivered(bus, since)?;
    owed.retain(|owed| !delivered.contains(&owed.decision));
    Ok(owed)
}

/// Commit the durable record that consumes one owed return after its exact
/// occupant's supervisor has confirmed enqueue.
pub(crate) fn record_return(bus: &Bus, owed: &ReturnOwed) -> Result<(), Outcome<CommandAnswer>> {
    let at = current_instant()?;
    let origin = format!("lane:{}", owed.lane)
        .parse::<wire::Origin>()
        .map_err(|error| store_fault("room::return-origin-invalid", error.to_string()))?;
    let mut write = JournalWrite::new(Actor::Spice, origin, at, Producer::ResidentRecognition);
    write
        .fact(
            DesignReturnDelivered {
                decision: owed.decision.clone(),
                lane: owed.lane,
                occupant: owed.occupant,
                claim: wire::SequenceCode::new(owed.claim.get()),
            },
            FactCause::Root,
        )
        .map_err(|error| store_fault("room::return-build", error.to_string()))?;
    match bus
        .commit(write)
        .map_err(|error| store_fault("room::return-commit", error.to_string()))?
    {
        CommitOutcome::Committed(_) => Ok(()),
        CommitOutcome::Refused(refusal) => {
            Err(store_fault("room::return-refused", refusal.to_string()))
        }
        CommitOutcome::Fault(fault) => Err(store_fault("room::return-fault", fault.to_string())),
    }
}

/// A decision carrying holder input, with its position and the input.
type Feedback = (Seq, PreparationDecision, String);

/// `design`'s newest decision carrying holder input and its position, read
/// newest-first so the work stops there instead of walking all history.
fn newest_feedback(
    bus: &Bus,
    design: &TaskStamp,
) -> Result<Option<Feedback>, Outcome<CommandAnswer>> {
    let design = design.clone();
    bus.read(move |read| {
        let mut before = None;
        loop {
            let page = read.newest_preparation_decisions(
                &design,
                before,
                journal::FactReadBudget::RETAINED,
            )?;
            for envelope in page.rows {
                if let Some(decided) = envelope.fact.read::<PreparationDecided>() {
                    let decision = decided.decision;
                    if let Some(feedback) = feedback(&decision.outcome) {
                        return Ok(Some((envelope.seq, decision, feedback)));
                    }
                }
            }
            match page.continuation {
                Some(continuation) => before = Some(continuation),
                None => return Ok(None),
            }
        }
    })
    .map_err(|error| store_fault("room::return-unreadable", error.to_string()))
}

fn feedback(outcome: &DecisionOutcome) -> Option<String> {
    match outcome {
        DecisionOutcome::Returned { reason } => Some(reason.clone()),
        DecisionOutcome::Resolved { outputs, comment } if outcome.carries_feedback() => {
            let mut text = String::new();
            for output in outputs {
                let verdict = match output.verdict {
                    wire::rooms::OutputVerdict::Accepted => "accepted",
                    wire::rooms::OutputVerdict::Rejected => "rejected",
                };
                let _ = write!(text, "{verdict} output {}", output.output);
                if let Some(comment) = &output.comment {
                    let _ = write!(text, ": {comment}");
                }
                text.push('\n');
            }
            if let Some(comment) = comment {
                let _ = write!(text, "overall: {comment}");
            } else {
                text.pop();
            }
            Some(text)
        }
        _ => None,
    }
}

/// Every return decision already delivered at or after `since`.
fn delivered(bus: &Bus, since: Seq) -> Result<BTreeSet<DecisionRef>, Outcome<CommandAnswer>> {
    bus.read(move |read| {
        let mut delivered = BTreeSet::new();
        let mut after = since.get().checked_sub(1).and_then(Seq::new);
        loop {
            let page = read.selected_fact_page(
                &[wire::FactKind::DesignReturnDelivered],
                after,
                None,
                journal::FactReadBudget::RETAINED,
            )?;
            delivered.extend(
                page.rows
                    .iter()
                    .filter_map(|envelope| envelope.fact.read::<DesignReturnDelivered>())
                    .map(|body| body.decision),
            );
            match page.continuation {
                Some(continuation) => after = Some(continuation),
                None => return Ok(delivered),
            }
        }
    })
    .map_err(|error| store_fault("room::return-unreadable", error.to_string()))
}
