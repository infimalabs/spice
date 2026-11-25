//! `spice room awareness HANDLE`: profile-selected requests for one Design.
//!
//! Each row is one admitted `room::noticed` record, oldest first: the requested
//! lane and the vendor session its residency named, the exact opportunity,
//! supporting task or conversation evidence, Design statement, and matched
//! terms as every surface shows them. Rows page whole in journal order; a page
//! that withholds any continues at the first withheld one. Reading changes
//! nothing.

use std::fmt;

use clap::ArgMatches;
use serde::Serialize;
use spice::repository::Repository;
use wire::output::Bounded;
use wire::rooms::{DesignItemStanding, DesignMaturity, DesignMaturityState, DesignNoticed};
use wire::{CanonicalUuid, FaultClass, Outcome, Reply, ReplyPosition, Seq, TaskHandle, TaskStamp};

use super::{Failure, current_handle, discover, fault, store};
use crate::read_page::Page;

/// What a continuation's `--after` names.
const CURSOR: &str = "an awareness record of this Design";

/// One page of completion requests for one Design.
#[derive(Debug, Serialize)]
pub(crate) struct DesignAwareness {
    design: TaskHandle,
    anchor: TaskStamp,
    page: Bounded<Noticed>,
    maturity: DesignMaturity,
    /// Maturity judged against the Design's current statement.
    state: DesignMaturityState,
    /// Each current question item's standing.
    items: Vec<ItemStanding>,
    /// Current statement term-net aggregates.
    nets: NetSummary,
    effect: &'static str,
}

/// One current question item with its handle and text.
#[derive(Debug, Serialize)]
pub(crate) struct ItemStanding {
    #[serde(flatten)]
    standing: DesignItemStanding,
    handle: String,
    text: String,
    positive: u64,
    negative: u64,
    net: i64,
    truncated: bool,
}

/// Exact term-net aggregates; `inexact` applies to every numeric field when
/// any current term exceeded the retained lane bound.
#[derive(Debug, Serialize)]
pub(crate) struct NetSummary {
    total: i64,
    agreement: Option<Agreement>,
    cancellation: u64,
    zero_nets: usize,
    regime: &'static str,
    inexact: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    guidance: Option<&'static str>,
}

/// The unreduced agreement fraction `aligned/signed`.
#[derive(Debug, Serialize)]
pub(crate) struct Agreement {
    aligned: u64,
    signed: u64,
}

/// One admitted awareness record.
#[derive(Debug, Serialize)]
pub(crate) struct Noticed {
    seq: Seq,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    session: wire::rooms::DesignSession,
    opportunity: wire::rooms::DesignNoticeOpportunity,
    evidence: wire::rooms::DesignNoticeEvidence,
    statement: Seq,
    admission: wire::rooms::DesignNoticeMatch,
    terms: Vec<String>,
    /// Question items asked, in asked order; empty asks about the whole Design.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    asks: Vec<wire::rooms::DesignAsk>,
}

/// Read one bounded page of a Design's awareness records from the working
/// repository.
pub(crate) fn run(
    matches: &ArgMatches,
    contract: &crate::cli::Contract,
) -> Outcome<DesignAwareness> {
    match discover() {
        Ok(repository) => run_at(&repository, matches, contract),
        Err(failure) => failure.outcome(),
    }
}

/// [`run`] against `repository`.
pub(crate) fn run_at(
    repository: &Repository,
    matches: &ArgMatches,
    contract: &crate::cli::Contract,
) -> Outcome<DesignAwareness> {
    match super::open(repository) {
        Ok((_, journal)) => answer(&journal, matches, contract.path),
        Err(failure) => failure.outcome(),
    }
}

/// One page of the awareness records of the Design `matches` names.
pub(crate) fn answer(
    journal: &journal::JournalReader,
    matches: &ArgMatches,
    path: &[&str],
) -> Outcome<DesignAwareness> {
    match awareness(journal, matches, path) {
        Ok((position, body)) => Outcome::Reply(Reply { position, body }),
        Err(failure) => failure.outcome(),
    }
}

fn awareness(
    journal: &journal::JournalReader,
    matches: &ArgMatches,
    path: &[&str],
) -> Result<(ReplyPosition, DesignAwareness), Failure> {
    let page = Page::from_matches(matches, path)?;
    let (named, anchor) = super::named_room(matches)?;
    let requested = page
        .after()
        .map(|after| {
            after
                .parse::<Seq>()
                .map_err(|_| page.cursor_refusal(CURSOR))
        })
        .transpose()?;
    let after = requested.and_then(|seq| seq.get().checked_sub(1).and_then(Seq::new));
    let retain = page.retain();
    let (cursor, design, maturity, term, rows) = journal
        .read_with(|read| -> Result<_, Failure> {
            let design = current_handle(read, &anchor)?;
            let cursor = read.cursor().map_err(store)?.ok_or_else(|| {
                fault(
                    FaultClass::Store,
                    "room::cursor-missing",
                    "the journal names a task but no read position",
                )
            })?;
            let maturity = read.design_maturity(&anchor).map_err(store)?;
            let term = read
                .task_head(&anchor)
                .map_err(store)?
                .map(|head| head.body().term.clone())
                .unwrap_or_default();
            let population = read.design_notice_count(&anchor, after).map_err(store)?;
            let budget = journal::FactReadBudget::new(
                retain.clamp(1, journal::FactReadBudget::RETAINED.rows()),
                journal::FactReadBudget::RETAINED.decoded_bytes(),
            )
            .map_err(store)?;
            let mut rows = Vec::with_capacity(retain.min(population));
            let mut from = after;
            while rows.len() < retain.min(population) {
                let fetched = read.design_notices(&anchor, from, budget).map_err(store)?;
                let wanted = retain - rows.len();
                rows.extend(fetched.rows.into_iter().take(wanted));
                match fetched.continuation {
                    Some(next) => from = Some(next),
                    None => break,
                }
            }
            let rows = rows
                .into_iter()
                .map(noticed)
                .collect::<Result<Vec<_>, _>>()?;
            let rows = page.shown(keyed(rows, |row| row.seq), population)?;
            Ok((cursor, design, maturity, term, rows))
        })
        .map_err(|error| match error {
            journal::ReadError::Journal(error) => store(error),
            journal::ReadError::Answer(failure) => failure,
        })?;
    if let Some(notice) = crate::board_command::rename_notice(&named, &design) {
        eprintln!("{notice}");
    }

    let state = maturity.state(&term);
    let nets = maturity.items.nets(&term);
    let standings = maturity.items.standings(&term);
    let current = nets
        .terms
        .iter()
        .map(|term| term.item.clone())
        .collect::<Vec<_>>();
    let items = standings
        .into_iter()
        .zip(&nets.terms)
        .zip(term)
        .map(|((standing, term), text)| ItemStanding {
            handle: term.item.handle(&current).to_owned(),
            standing,
            text,
            positive: term.positive,
            negative: term.negative,
            net: term.net(),
            truncated: term.truncated,
        })
        .collect();
    let summary = NetSummary {
        total: nets.total(),
        agreement: nets
            .agreement()
            .map(|(aligned, signed)| Agreement { aligned, signed }),
        cancellation: nets.cancellation(),
        zero_nets: nets.zero_nets(),
        regime: nets.regime().as_str(),
        inexact: nets.inexact(),
        guidance: nets.advice(),
    };
    Ok((
        ReplyPosition::Snapshot(cursor),
        DesignAwareness {
            design,
            anchor,
            page: rows,
            maturity,
            state,
            items,
            nets: summary,
            effect: "advisory only; neither blocks nor authorizes a Design decision",
        },
    ))
}

/// Each row beside its continuation key, the journal position.
fn keyed<T>(rows: Vec<T>, seq: impl Fn(&T) -> Seq) -> Vec<(T, String)> {
    rows.into_iter()
        .map(|row| {
            let key = seq(&row).to_string();
            (row, key)
        })
        .collect()
}

/// The row one indexed notice envelope reads as.
fn noticed(envelope: wire::Envelope<wire::AnyFact>) -> Result<Noticed, Failure> {
    let Some(body) = envelope.fact.read::<DesignNoticed>() else {
        return Err(fault(
            FaultClass::Store,
            "room::awareness-index-invalid",
            "the Design notice index returned another fact kind",
        ));
    };
    Ok(Noticed {
        seq: envelope.seq,
        lane: body.lane,
        occupant: body.occupant,
        session: body.session,
        opportunity: body.opportunity,
        evidence: body.evidence,
        statement: body.statement,
        admission: body.admission,
        terms: wire::rooms::shown_notice_terms(&body.terms)
            .into_iter()
            .map(str::to_owned)
            .collect(),
        asks: body.asks,
    })
}

impl fmt::Display for DesignAwareness {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "design {} awareness (background notices; no attention or acknowledgment)",
            self.design
        )?;
        write!(
            formatter,
            "\n{}\n{}",
            maturity_summary(&self.maturity, &self.state),
            net_summary(&self.nets),
        )?;
        for item in &self.items {
            let (charges, lanes) = (item.standing.charges, item.standing.lanes);
            write!(
                formatter,
                "\n- {} {} · {}+/{}- net {:+}: {charges} charge{} from {lanes} lane{}: {}",
                item.handle,
                item.standing.state.label(),
                item.positive,
                item.negative,
                item.net,
                if charges == 1 { "" } else { "s" },
                if lanes == 1 { "" } else { "s" },
                item.text,
            )?;
            if item.truncated {
                write!(
                    formatter,
                    " (more than {} lanes charged this term; exact counts unavailable)",
                    wire::rooms::DESIGN_MATURITY_LANE_LIMIT
                )?;
            }
        }
        if let Some(guidance) = self.nets.guidance {
            write!(formatter, "\nterm guidance: {guidance}")?;
        }
        for head in self.maturity.recent_heads.iter().rev() {
            write!(
                formatter,
                "\nstatement {}: {} asked; {} contributed, {} declined, {} unanswered{}",
                head.statement,
                head.requested_lanes.len(),
                head.requested_contributors(),
                head.requested_decliners(),
                head.requested_unanswered(),
                if head.yield_truncated() {
                    " (bounded)"
                } else {
                    ""
                },
            )?;
        }
        if self
            .maturity
            .recent_heads
            .iter()
            .any(|head| head.requested_decliners() > 0)
        {
            write!(
                formatter,
                "\ndecline clauses: spice room history {}",
                self.design
            )?;
        }
        write!(formatter, "\n{}", self.effect)?;
        for row in &self.page.rows {
            write!(formatter, "\n{row}")?;
        }
        match &self.page.omission {
            Some(omission) => write!(formatter, "\n{omission}"),
            None => Ok(()),
        }
    }
}

fn maturity_summary(maturity: &DesignMaturity, state: &DesignMaturityState) -> String {
    let contributions = maturity.contributions;
    let contributing_lanes = maturity.contributing_lanes.len();
    let requested_lanes = maturity.requested_lanes.len();
    format!(
        "maturity {}; {contributions} contribution{} from {contributing_lanes} lane{}; \
         requests={} to {requested_lanes} lane{}",
        state.summary(),
        if contributions == 1 { "" } else { "s" },
        if contributing_lanes == 1 { "" } else { "s" },
        maturity.requests,
        if requested_lanes == 1 { "" } else { "s" },
    )
}

fn net_summary(nets: &NetSummary) -> String {
    let marker = if nets.inexact { " (inexact)" } else { "" };
    let agreement = nets.agreement.as_ref().map_or_else(
        || "n/a".to_owned(),
        |agreement| format!("{}/{}", agreement.aligned, agreement.signed),
    );
    format!(
        "term nets: total {:+}{marker}; agreement {agreement}{marker}; cancellation {}{marker}; \
         zero-net terms {}{marker}; regime {}",
        nets.total, nets.cancellation, nets.zero_nets, nets.regime
    )
}

impl fmt::Display for Noticed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let opportunity = spice::rooms::awareness::notice_opportunity_text(&self.opportunity);
        let evidence = spice::rooms::awareness::notice_evidence_text(&self.evidence);
        write!(
            formatter,
            "{} lane {} occupant {} session {}/{} opportunity {opportunity} evidence {evidence} \
             statement {}",
            self.seq,
            self.lane,
            self.occupant,
            self.session.vendor,
            self.session.identity,
            self.statement,
        )?;
        let matching = &self.admission;
        write!(
            formatter,
            " reach {} score {} floor {} fit {}",
            matching.reach(),
            matching.score(),
            matching.floor(),
            matching.generation().as_str(),
        )?;
        if !self.terms.is_empty() {
            write!(formatter, " terms {}", self.terms.join(", "))?;
        }
        let current = self
            .asks
            .iter()
            .map(|ask| ask.item.clone())
            .collect::<Vec<_>>();
        for (index, ask) in self.asks.iter().enumerate() {
            let lead = if index == 0 { " asks" } else { "," };
            write!(
                formatter,
                "{lead} {} ({})",
                ask.item.handle(&current),
                ask.state.label()
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(byte: u8) -> CanonicalUuid {
        format!("00000000-0000-4000-8000-0000000000{byte:02x}")
            .parse()
            .expect("fixture uuid")
    }

    fn conversation() -> wire::rooms::DesignConversationEvidence {
        wire::rooms::DesignConversationEvidence {
            locus: wire::body::TranscriptLocus {
                source: Some(uuid(9)),
                member: 3,
                offset: 5,
                length: 7,
                event: 0,
                line: 1,
                column: 1,
            },
            digest: wire::Digest::compute(wire::Domain::RoomSource, b"fixture conversation"),
        }
    }

    fn row(
        opportunity: wire::rooms::DesignNoticeOpportunity,
        evidence: wire::rooms::DesignNoticeEvidence,
    ) -> Noticed {
        Noticed {
            seq: Seq::new(1).expect("fixture sequence"),
            lane: uuid(1),
            occupant: uuid(2),
            session: wire::rooms::DesignSession {
                vendor: "omp".parse().expect("fixture vendor"),
                identity: "fixture-session".to_owned(),
            },
            opportunity,
            evidence,
            statement: Seq::new(2).expect("fixture statement"),
            terms: vec!["rendering".to_owned()],
            admission: wire::rooms::DesignNoticeMatch::new(
                wire::rooms::DesignReach::Wide,
                0.2,
                0.1,
                [1; 32],
            )
            .expect("fixture match clears its floor"),
            asks: Vec::new(),
        }
    }

    #[test]
    fn notice_rows_render_semantic_sources_without_debug_structs() {
        use wire::rooms::{
            DesignNoticeEvidence as Evidence, DesignNoticeOpportunity as Opportunity, DesignSupport,
        };

        let task = wire::TaskStamp::for_micros(1_800_000_000_000_000).expect("fixture task stamp");
        let statement = Seq::new(3).expect("fixture statement");
        let completed = Seq::new(4).expect("fixture completion");
        let terminal = Seq::new(5).expect("fixture terminal");
        let claim = wire::SequenceCode::new(6);
        let conversation = conversation();
        let rows = [
            row(
                Opportunity::TaskCompletion {
                    task: task.clone(),
                    completed,
                },
                Evidence::Task {
                    task: task.clone(),
                    statement,
                    completed,
                    support: DesignSupport::Implemented,
                },
            ),
            row(
                Opportunity::TaskClaim {
                    task: task.clone(),
                    claim,
                    terminal: None,
                },
                Evidence::TaskClaim {
                    task: task.clone(),
                    statement,
                    claim,
                    terminal: None,
                    support: DesignSupport::Implemented,
                },
            ),
            row(
                Opportunity::TaskClaim {
                    task: task.clone(),
                    claim,
                    terminal: Some(terminal),
                },
                Evidence::TaskClaim {
                    task: task.clone(),
                    statement,
                    claim,
                    terminal: Some(terminal),
                    support: DesignSupport::Implemented,
                },
            ),
            row(
                Opportunity::Conversation {
                    source: conversation.clone(),
                },
                Evidence::Conversation {
                    source: conversation.clone(),
                },
            ),
            row(
                Opportunity::DesignStatement,
                Evidence::Task {
                    task: task.clone(),
                    statement,
                    completed,
                    support: DesignSupport::Implemented,
                },
            ),
            row(
                Opportunity::ProfileReady {
                    horizon: Some(completed),
                },
                Evidence::Task {
                    task: task.clone(),
                    statement,
                    completed,
                    support: DesignSupport::Implemented,
                },
            ),
            row(
                Opportunity::SessionResumed {
                    declaration: completed,
                },
                Evidence::Task {
                    task,
                    statement,
                    completed,
                    support: DesignSupport::Implemented,
                },
            ),
        ]
        .map(|row| row.to_string());

        for rendered in &rows {
            assert_eq!(rendered.lines().count(), 1, "{rendered}");
            for debug in ["{", "CanonicalUuid(", "Digest(", "Some("] {
                assert!(!rendered.contains(debug), "{rendered}");
            }
        }
        let source = conversation
            .locus
            .source
            .expect("fixture conversation has a source")
            .to_string();
        assert_eq!(
            rows[3].matches(&source).count(),
            1,
            "one conversation source is rendered once"
        );
    }

    #[test]
    fn notice_rows_render_the_recorded_match() {
        let mut scored = row(
            wire::rooms::DesignNoticeOpportunity::DesignStatement,
            wire::rooms::DesignNoticeEvidence::Conversation {
                source: conversation(),
            },
        );
        scored.admission = wire::rooms::DesignNoticeMatch::new(
            wire::rooms::DesignReach::Narrow,
            0.25,
            0.1,
            [7; 32],
        )
        .expect("fixture match clears its floor");
        let scored = scored.to_string();
        assert!(
            scored.contains("reach narrow score 0.25 floor 0.1"),
            "{scored}"
        );
    }

    #[test]
    fn net_summary_keeps_the_exact_fraction_and_marks_each_inexact_aggregate() {
        let summary = NetSummary {
            total: 3,
            agreement: Some(Agreement {
                aligned: 7,
                signed: 11,
            }),
            cancellation: 4,
            zero_nets: 2,
            regime: "contested",
            inexact: true,
            guidance: None,
        };
        let rendered = net_summary(&summary);
        assert!(rendered.contains("agreement 7/11 (inexact)"), "{rendered}");
        assert!(rendered.contains("total +3 (inexact)"), "{rendered}");
        assert!(rendered.contains("cancellation 4 (inexact)"), "{rendered}");
        assert!(
            rendered.contains("zero-net terms 2 (inexact)"),
            "{rendered}"
        );
        assert!(!rendered.contains('%'), "{rendered}");
    }
}
