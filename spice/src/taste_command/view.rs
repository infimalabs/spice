//! Bounded `spice taste` pages.
//!
//! The bare read shows the tracked generation and one compact row per taste,
//! tastes with unanswered reminders and recurring identities first. Fires and
//! judgments sit behind `--name NAME`, one newest-first page of that taste's
//! history. Both pages admit through [`crate::read_page::Page`], so every
//! partial page names the exact command that continues it.

use std::fmt;

use serde::Serialize;
use wire::output::{Bounded, ClippedText};

use super::{
    Disposition, HistoryCursor, HistoryKind, Name, Seq, TasteFire, TasteJudgment, TasteProvenance,
    TasteRow, TasteSnapshot, VendorTally, Verdict,
};
use crate::read_page::Page;

/// One `spice taste` answer.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum TasteView {
    Tastes(TasteReport),
    Taste(Box<TasteDetail>),
}

/// Bare `spice taste`: the tracked generation and a page of compact rows.
#[derive(Debug, Serialize)]
pub(crate) struct TasteReport {
    digest: wire::Digest,
    provenance: TasteProvenance,
    tastes: Bounded<TasteSummary>,
}

/// `spice taste --name NAME`: one taste and a page of its history.
#[derive(Debug, Serialize)]
pub(crate) struct TasteDetail {
    digest: wire::Digest,
    taste: TasteSummary,
    history: Bounded<TasteEvent>,
}

/// One taste's configuration and evidence counts.
#[derive(Debug, Serialize)]
struct TasteSummary {
    name: Name,
    enabled: bool,
    words: Vec<String>,
    message: ClippedText,
    scopes: gate::ScopeSelector,
    /// Recent reminders still awaiting an answer.
    live: usize,
    /// Recent reminders carried.
    fires: usize,
    vendors: Vec<VendorTally>,
    /// Identities that fired again in a later compaction epoch.
    recurrence: usize,
    confirmed: usize,
    cleared: usize,
}

/// One fire or judgment in a taste's history.
#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "lowercase")]
enum TasteEvent {
    Fire(TasteFire),
    Judgment(TasteJudgment),
}

impl TasteEvent {
    const fn position(&self) -> Seq {
        match self {
            Self::Fire(fire) => fire.position,
            Self::Judgment(judgment) => judgment.position,
        }
    }

    const fn kind(&self) -> HistoryKind {
        match self {
            Self::Fire(_) => HistoryKind::Fire,
            Self::Judgment(_) => HistoryKind::Judgment,
        }
    }

    fn cursor(&self, horizon: Seq) -> HistoryCursor {
        HistoryCursor {
            horizon,
            position: self.position(),
            kind: self.kind(),
        }
    }
}

/// Project one complete snapshot onto the page the caller asked for.
pub(super) fn project(
    snapshot: TasteSnapshot,
    page: Page,
    name: Option<&Name>,
) -> Result<TasteView, wire::Refusal> {
    let text = page.limits().text;
    let TasteSnapshot {
        digest,
        provenance,
        horizon,
        tastes,
    } = snapshot;
    let Some(name) = name else {
        let mut rows = tastes
            .iter()
            .map(|row| summary(row, text))
            .collect::<Vec<_>>();
        // Unanswered reminders, then recurring identities, then the tracked
        // order; the sort is stable, so the tracked order breaks every tie.
        rows.sort_by_key(|row| (row.live == 0, row.recurrence == 0));
        let tastes = page.listed(rows, |row| row.name.to_string(), "NAME of a tracked taste")?;
        return Ok(TasteView::Tastes(TasteReport {
            digest,
            provenance,
            tastes,
        }));
    };
    let Some(row) = tastes.into_iter().find(|row| row.name == *name) else {
        return Err(wire::Refusal::bare(
            wire::RefusalDiagnostic::new(
                "taste::unknown".to_owned(),
                None,
                Some("one tracked taste name".to_owned()),
                Some(name.to_string()),
            )
            .with_detail("read bare `spice taste` and name one tracked taste"),
        ));
    };
    let taste = summary(&row, text);
    let mut population = row.history_population;
    let start = row.history_start;
    let mut history = row
        .fires
        .into_iter()
        .map(TasteEvent::Fire)
        .chain(
            row.judgments
                .into_iter()
                .map(|judgment| TasteEvent::Judgment(clip_judgment(judgment, text))),
        )
        .collect::<Vec<_>>();
    history.sort_by_key(|event| {
        (
            std::cmp::Reverse(event.position()),
            matches!(event, TasteEvent::Judgment(_)),
        )
    });
    if let Some(start) = start {
        let Some(offset) = history
            .iter()
            .position(|event| event.cursor(horizon) == start)
        else {
            return Err(page.cursor_refusal("HORIZON/POSITION/fire or HORIZON/POSITION/judgment"));
        };
        history.drain(..offset);
        population = population.saturating_sub(offset);
    }
    let history = page.shown(
        history
            .into_iter()
            .map(|event| {
                let key = event.cursor(horizon).key();
                (event, key)
            })
            .collect(),
        population,
    )?;
    Ok(TasteView::Taste(Box::new(TasteDetail {
        digest,
        taste,
        history,
    })))
}

fn summary(row: &TasteRow, text: usize) -> TasteSummary {
    TasteSummary {
        name: row.name.clone(),
        enabled: row.enabled,
        words: row.words.clone(),
        message: wire::output::clip(&row.message, text),
        scopes: row.scopes.clone(),
        live: row
            .fires
            .iter()
            .filter(|fire| fire.standing == Disposition::Live)
            .count(),
        fires: row.fires.len(),
        vendors: row.vendors.clone(),
        recurrence: row.recurrence.len(),
        confirmed: row.confirmed,
        cleared: row.cleared,
    }
}

/// Cut a judgment's free text to the page's text limit.
fn clip_judgment(mut judgment: TasteJudgment, text: usize) -> TasteJudgment {
    let cut = |value: &mut String| {
        if value.len() > text {
            *value = wire::output::clip(value, text).to_string();
        }
    };
    cut(&mut judgment.locus);
    if let Some(trigger) = &mut judgment.trigger {
        cut(trigger);
    }
    if let Some(source) = &mut judgment.source {
        cut(source);
    }
    for attempt in &mut judgment.attempts {
        cut(&mut attempt.reply);
    }
    judgment
}

impl fmt::Display for TasteView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tastes(report) => {
                write!(
                    formatter,
                    "taste {} {} {}",
                    report.digest,
                    report.provenance.source.as_str(),
                    report.provenance.location
                )?;
                for row in &report.tastes.rows {
                    write!(formatter, "\n{row}")?;
                }
                omission(formatter, &report.tastes)
            }
            Self::Taste(detail) => {
                write!(formatter, "taste {}\n{}", detail.digest, detail.taste)?;
                for event in &detail.history.rows {
                    write!(formatter, "\n{event}")?;
                }
                omission(formatter, &detail.history)
            }
        }
    }
}

fn omission<T>(formatter: &mut fmt::Formatter<'_>, page: &Bounded<T>) -> fmt::Result {
    match &page.omission {
        Some(omission) => write!(formatter, "\n{omission}"),
        None => Ok(()),
    }
}

impl fmt::Display for TasteSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {} live={} fires={} recurrence={} confirmed={} cleared={} words={} {}",
            self.name,
            if self.enabled { "enabled" } else { "disabled" },
            self.live,
            self.fires,
            self.recurrence,
            self.confirmed,
            self.cleared,
            self.words.join(","),
            self.message,
        )
    }
}

impl fmt::Display for TasteEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fire(fire) => write!(
                formatter,
                "{} {} fire {} {} key={} lane={} compaction={} {}",
                fire.position,
                fire.at,
                fire.standing.as_str(),
                fire.vendor,
                fire.key,
                fire.lane,
                fire.compaction,
                fire.locus,
            ),
            Self::Judgment(judgment) => write!(
                formatter,
                "{} {} judgment {} {}",
                judgment.position,
                judgment.at,
                match judgment.verdict {
                    Verdict::Confirmed => "confirmed",
                    Verdict::Cleared => "cleared",
                },
                judgment.locus,
            ),
        }
    }
}
