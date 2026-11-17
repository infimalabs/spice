//! Room history sections beside the publication page: the page's typed
//! contributions grouped by kind, and every operator decision on the anchor's
//! candidates in decision order.
//!
//! A decision is the anchor's own immutable record, never a room publication,
//! so it pages with neither publications nor catch-up. The newest return's
//! reason is the anchor's newest input; a decision mutates no candidate it
//! names.

use std::collections::VecDeque;
use std::fmt;

use serde::Serialize;
use wire::principal::PrincipalDirectory;
use wire::rooms::{
    DecisionOutcome, OutputVerdict, PreparationDecided, RoomAuthorityStamp, RoomContributionKind,
};
use wire::{Instant, Seq, SequenceCode, TaskHandle, TaskStamp};

use super::{Content, Failure, Named, Publication, fault, store};

/// Most decisions one room read shows, newest kept.
pub(crate) const DECISION_ROWS: usize = 32;

/// The shown contributions of one kind, in journal order.
#[derive(Debug, Serialize)]
pub(crate) struct ContributionGroup {
    kind: RoomContributionKind,
    publications: Vec<RoomAuthorityStamp>,
}

/// `rows`' contributions grouped by kind in [`RoomContributionKind::ALL`]
/// order, each group in journal order.
pub(super) fn group(rows: &[Publication]) -> Vec<ContributionGroup> {
    RoomContributionKind::ALL
        .into_iter()
        .filter_map(|kind| {
            let publications: Vec<_> = rows
                .iter()
                .filter(|row| {
                    matches!(&row.content, Content::Contribution { kind: shown, .. } if *shown == kind)
                })
                .map(|row| row.publication.clone())
                .collect();
            (!publications.is_empty()).then_some(ContributionGroup { kind, publications })
        })
        .collect()
}

impl fmt::Display for ContributionGroup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "\n{}:", self.kind)?;
        for publication in &self.publications {
            write!(formatter, " {publication}")?;
        }
        Ok(())
    }
}

/// The anchor's newest decisions, oldest first, and the read that pages its
/// complete decision record when older ones exist.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Decisions {
    rows: VecDeque<Decision>,
    /// The command paging every decision from the first, when this read
    /// withholds older ones.
    #[serde(skip_serializing_if = "Option::is_none")]
    older: Option<String>,
}

/// One operator decision as the journal records it.
#[derive(Debug, Serialize)]
struct Decision {
    decision: SequenceCode,
    seq: Seq,
    at: Instant,
    actor: Named,
    outcome: DecisionOutcome,
}

impl Decisions {
    pub(super) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// The newest [`DECISION_ROWS`] decisions recorded on `anchor`'s candidates,
/// in decision order, read as one newest-first page so the work never grows
/// with the anchor's whole history. Older decisions stay reachable through
/// the preparation authority of `room`, which pages from the first record.
pub(super) fn decisions(
    read: &journal::JournalRead<'_>,
    principals: &PrincipalDirectory,
    anchor: &TaskStamp,
    room: &TaskHandle,
) -> Result<Decisions, Failure> {
    let budget = journal::FactReadBudget::new(
        DECISION_ROWS,
        journal::FactReadBudget::RETAINED.decoded_bytes(),
    )
    .map_err(store)?;
    let page = read
        .newest_preparation_decisions(anchor, None, budget)
        .map_err(store)?;
    let mut shown = Decisions {
        rows: VecDeque::with_capacity(page.rows.len()),
        older: page
            .continuation
            .map(|_| format!("spice task authority {room}")),
    };
    for envelope in page.rows {
        let decided = envelope.fact.read::<PreparationDecided>().ok_or_else(|| {
            fault(
                wire::FaultClass::Store,
                "room::decision-invalid",
                format!("decision {} is not a recorded decision", envelope.seq),
            )
        })?;
        shown.rows.push_front(Decision {
            decision: decided.decision.decision.decision,
            seq: envelope.seq,
            at: envelope.at,
            actor: Named::of(principals, envelope.actor),
            outcome: decided.decision.outcome,
        });
    }
    Ok(shown)
}

impl fmt::Display for Decisions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(older) = &self.older {
            write!(
                formatter,
                "\ndecisions: older withheld; every decision: {older}"
            )?;
        }
        for row in &self.rows {
            write!(
                formatter,
                "\ndecision {} seq={} at={} from={} ",
                row.decision, row.seq, row.at, row.actor
            )?;
            match &row.outcome {
                DecisionOutcome::Returned { reason } => {
                    write!(formatter, "returned the pass")?;
                    write_reason(formatter, reason)?;
                }
                DecisionOutcome::Resolved { outputs, comment } => {
                    write!(formatter, "resolved the pass")?;
                    for output in outputs {
                        let verdict = match output.verdict {
                            OutputVerdict::Accepted => "accepted",
                            OutputVerdict::Rejected => "rejected",
                        };
                        write!(formatter, "\n  {verdict} output {}", output.output)?;
                        if let Some(comment) = &output.comment {
                            write_reason(formatter, comment)?;
                        }
                    }
                    if let Some(comment) = comment {
                        write!(formatter, "\n  overall")?;
                        write_reason(formatter, comment)?;
                    }
                }
                DecisionOutcome::Concluded {} => write!(formatter, "concluded the Design")?,
                DecisionOutcome::Collapsed { reason } => {
                    write!(formatter, "collapsed the Design")?;
                    write_reason(formatter, reason)?;
                }
            }
        }
        Ok(())
    }
}

fn write_reason(formatter: &mut fmt::Formatter<'_>, reason: &str) -> fmt::Result {
    for line in reason.lines() {
        write!(formatter, "\n  | {line}")?;
    }
    Ok(())
}
