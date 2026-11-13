use std::collections::BTreeSet;

use clap::ArgMatches;
use wire::output::{Admission, Bounded, Limits, Request, Surface};
use wire::{Refusal, SpiceCommand};

use super::authority::RecallAuthorities;
use super::dataset::{self, WindowOptions, WindowScan, WindowTurn};
use super::model::{
    ActivityView, CompactionRecord, CompactionView, DialogueLine, LiveTurnView, LiveView,
    OutstandingAskView, RecallView, RecentTurnView, Report, SweepWindow, TurnRecord, TurnView,
};
use super::{capture, source::ResolvedSource};
use crate::read_page::{self as page, Page, Paged};

/// Pending asks a briefing carries, each cut to a readable preview. Every
/// pending ask is shown whole by `spice inbox LANE`.
const ASKS: Surface = Surface::new(ASK_LIMITS, ASK_LIMITS);
const ASK_LIMITS: Limits = Limits {
    rows: 16,
    bytes: 16_384,
    text: 1_024,
};
/// Bytes of the live prompt and latest final answer a briefing carries.
const LIVE_TEXT: usize = 480;
/// Compactions a sweep retains after its horizon; the page itself is bounded
/// by its row and byte limits.
const SWEEP_COMPACTIONS: usize = 4 * 1_024;
#[derive(Clone)]
pub(crate) enum HistorySource {
    Files(Vec<String>),
    Capture(capture::Binding),
}

impl HistorySource {
    pub(crate) fn arguments(&self) -> Vec<String> {
        match self {
            Self::Files(files) => files.clone(),
            Self::Capture(binding) => binding.command_arguments().into(),
        }
    }

    pub(crate) fn label(&self) -> Option<String> {
        match self {
            Self::Files(files) if files.len() == 1 => files.first().cloned(),
            Self::Files(files) if files.is_empty() => None,
            Self::Files(files) => Some(format!("{} transcripts", files.len())),
            Self::Capture(binding) => Some(format!("captured session {}", binding.session())),
        }
    }
}

struct SweepInterval<'a> {
    start: Option<&'a str>,
    end: Option<&'a str>,
    anchor: Option<&'a CompactionRecord>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Filters {
    start: Option<String>,
    end: Option<String>,
    contains: Option<String>,
    turn_ids: BTreeSet<String>,
    tools: BTreeSet<String>,
}

impl Filters {
    pub(crate) fn from_matches(matches: &ArgMatches) -> Result<Self, Refusal> {
        let start = optional_bound(matches, "start")?;
        let end = optional_bound(matches, "end")?;
        if let (Some(start), Some(end)) = (&start, &end)
            && start > end
        {
            return Err(refusal(
                "session::bounds-reversed",
                Some(format!("{start} -> {end}")),
                "pass a start timestamp not later than the end timestamp",
            ));
        }
        Ok(Self {
            start,
            end,
            contains: one(matches, "contains").map(|value| value.to_lowercase()),
            turn_ids: values(matches, "turn-id").into_iter().collect(),
            tools: values(matches, "tool").into_iter().collect(),
        })
    }

    /// The same filters ending at `end`.
    fn ending(self, end: Option<String>) -> Self {
        Self { end, ..self }
    }

    pub(crate) fn end_bound(&self) -> Option<&str> {
        self.end.as_deref()
    }

    pub(crate) fn turn_matches(&self, turn: &TurnRecord) -> bool {
        if self
            .start
            .as_ref()
            .is_some_and(|start| turn_end(turn) < *start)
            || self.end.as_ref().is_some_and(|end| turn.start_ts > *end)
        {
            return false;
        }
        if !self.turn_ids.is_empty()
            && turn
                .turn_id
                .as_ref()
                .is_none_or(|turn_id| !self.turn_ids.contains(turn_id))
        {
            return false;
        }
        if !self.tools.is_empty() && !turn.tool_calls.keys().any(|tool| self.tools.contains(tool)) {
            return false;
        }
        self.contains.as_ref().is_none_or(|needle| {
            turn.messages
                .iter()
                .any(|message| message.text.to_lowercase().contains(needle))
        })
    }

    pub(crate) fn validate_turn_identity_available(&self, available: bool) -> Result<(), Refusal> {
        if self.turn_ids.is_empty() || available {
            return Ok(());
        }
        Err(refusal(
            "session::turn-id-unavailable",
            Some(self.turn_ids.iter().cloned().collect::<Vec<_>>().join(",")),
            "filter by --start, --end, or --contains; this transcript carries no turn identities",
        ))
    }
}

/// The briefing: what binds this lane now, then the least transcript that
/// lets it resume. History stays behind the bounded views it names.
pub(crate) fn recall(
    sources: &[ResolvedSource],
    files: &[String],
    authorities: RecallAuthorities,
) -> Result<Report, Refusal> {
    let horizon = dataset::recent_compaction_horizon(sources, 1, None)?;
    // One compaction further back, so a briefing read right after a
    // compaction still recalls the turns that compaction folded away.
    let reach = dataset::recent_compaction_horizon(sources, RECENT_COMPACTIONS, None)?;
    let data = WindowScan::scan(
        sources,
        reach.start.as_deref(),
        None,
        &Filters::default(),
        WindowOptions::Live,
    )?;
    recall_report(
        data,
        horizon.start,
        HistorySource::Files(files.to_vec()),
        authorities,
    )
}

pub(crate) fn recall_capture(
    snapshot: capture::Snapshot,
    authorities: RecallAuthorities,
) -> Result<Report, Refusal> {
    let history = HistorySource::Capture(snapshot.binding.clone());
    let data = WindowScan::scan_capture(snapshot, &Filters::default(), WindowOptions::Live, None)?;
    let compacted = data.compactions.last().map(|row| row.ts.clone());
    recall_report(data, compacted, history, authorities)
}

fn recall_report(
    data: WindowScan,
    compacted: Option<String>,
    history: HistorySource,
    authorities: RecallAuthorities,
) -> Result<Report, Refusal> {
    let recent = data
        .turns
        .iter()
        .rev()
        .take(RECENT_TURNS)
        .collect::<Vec<_>>();
    let mut touched = Vec::<String>::new();
    for turn in &recent {
        for path in turn.view.touched_paths.iter().rev() {
            if touched.len() < RECENT_PATHS && !touched.contains(path) {
                touched.push(path.clone());
            }
        }
    }
    let live = LiveView {
        turn: data.turns.last().map(|turn| LiveTurnView {
            start: turn.view.start.clone(),
            end: turn.view.end.clone(),
            completed: turn.view.completed,
            commands: turn.view.commands,
            patches: turn.view.patches,
            errors: turn.view.errors,
            prompt: turn
                .prompt
                .as_deref()
                .map(|prompt| page::clip(prompt, LIVE_TEXT)),
        }),
        latest_final: data
            .turns
            .iter()
            .rev()
            .find_map(|turn| turn.latest_final.as_deref())
            .map(|text| page::clip(text, LIVE_TEXT)),
        compacted,
        recent: recent
            .iter()
            .map(|turn| RecentTurnView {
                start: turn.view.start.clone(),
                completed: turn.view.completed,
                commands: turn.view.commands,
                patches: turn.view.patches,
                errors: turn.view.errors,
                final_answer: turn
                    .latest_final
                    .as_deref()
                    .map(|text| page::clip(text, RECENT_FINAL_TEXT)),
            })
            .collect(),
        touched,
        history: ["sweep", "turns"]
            .into_iter()
            .map(|view| session_command(view, &history, []))
            .collect(),
    };
    let lane = authorities.held_work.lane.clone();
    let asks = authorities
        .asks
        .map(|pending| {
            let mut asks = Admission::new(
                ASKS.budget(Request::default())
                    .expect("the briefing ask surface admits its own defaults"),
            );
            for ask in pending {
                asks.offer(OutstandingAskView {
                    directive: ask.directive,
                    key: ask.key,
                    stage: ask.stage,
                    text: page::clip(&ask.text, ASK_LIMITS.text),
                    attachments: ask.attachments,
                    authored: ask.authored,
                    expires: ask.expires,
                })
                .map_err(unencodable)?;
            }
            Ok(asks
                .finish(|_| SpiceCommand::new("inbox", lane).expect("inbox is one Spice command")))
        })
        .transpose()?;
    let mut recall = Box::new(RecallView {
        asks,
        held_work: authorities.held_work,
        posture: authorities.posture,
        peers: authorities.peers,
        directions: authorities.directions,
        board: authorities.board,
        latch_debt: authorities.latch_debt,
        repository: authorities.repository,
        unknowns: authorities.unknowns,
        live,
        sources: data.sources,
        residues: data.residues.len(),
        digest: String::new(),
    });
    recall.digest = match &history {
        HistorySource::Files(files) => super::digest::fold(&recall, files),
        HistorySource::Capture(_) => super::digest::fold_history(&recall, &history),
    };
    Ok(Report::Recall(recall))
}

/// Compactions a briefing reaches back across for recent turns.
const RECENT_COMPACTIONS: usize = 2;
/// Recent turns a briefing recalls as outcomes.
const RECENT_TURNS: usize = 6;
/// Paths those turns touched that a briefing names.
const RECENT_PATHS: usize = 16;
/// Bytes of one recent turn's final answer.
const RECENT_FINAL_TEXT: usize = 360;

/// One row per compaction window, newest first, each summarizing its turns
/// and naming the bounded read of them.
pub(crate) fn sweep(
    sources: &[ResolvedSource],
    files: &[String],
    page: Page,
) -> Result<Report, Refusal> {
    let through = page.end(None)?;
    let count = page.frontier()?;
    let horizon = dataset::counted_compaction_horizon(sources, count, through.as_deref())?;
    let total = horizon
        .total
        .expect("a counted compaction horizon counts every compaction");
    let data = WindowScan::scan(
        sources,
        horizon.start.as_deref(),
        None,
        &Filters::default(),
        WindowOptions::Sweep {
            compactions: SWEEP_COMPACTIONS,
        },
    )?;
    sweep_report(
        data,
        HistorySource::Files(files.to_vec()),
        page,
        through,
        count,
        total,
    )
}

pub(crate) fn sweep_capture(snapshot: capture::Snapshot, page: Page) -> Result<Report, Refusal> {
    let history = HistorySource::Capture(snapshot.binding.clone());
    let through = page.end(None)?;
    let count = page.frontier()?;
    let data = WindowScan::scan_capture(
        snapshot,
        &Filters::default(),
        WindowOptions::Sweep { compactions: count },
        through.as_deref(),
    )?;
    let total = data.compaction_population;
    sweep_report(data, history, page, through, count, total)
}

fn sweep_report(
    data: WindowScan,
    history: HistorySource,
    page: Page,
    through: Option<String>,
    count: usize,
    total: usize,
) -> Result<Report, Refusal> {
    let rows = sweep_intervals(&data.compactions, through.as_deref(), count)
        .iter()
        .rev()
        .filter_map(|interval| sweep_window(interval, &data.turns, &history, &page))
        .collect::<Vec<_>>();
    let population = if total == 0 { rows.len() } else { total };
    let paged = page.newest(rows, population, |row| &row.start)?;
    Ok(report(paged, |page| Report::Sweep {
        sources: data.sources,
        page,
        residues: data.residues,
    }))
}

fn sweep_intervals<'a>(
    compactions: &'a [CompactionRecord],
    through: Option<&str>,
    count: usize,
) -> Vec<SweepInterval<'a>> {
    if compactions.is_empty() {
        return vec![SweepInterval {
            start: None,
            end: None,
            anchor: None,
        }];
    }
    let split = compactions.partition_point(|row| through.is_none_or(|end| row.ts.as_str() <= end));
    let closing = compactions.get(split).map(|row| row.ts.as_str());
    let selected = &compactions[split.saturating_sub(count)..split];
    selected
        .iter()
        .enumerate()
        .map(|(index, row)| SweepInterval {
            start: Some(row.ts.as_str()),
            end: selected
                .get(index + 1)
                .map(|next| next.ts.as_str())
                .or(closing),
            anchor: Some(row),
        })
        .collect()
}

fn sweep_window(
    interval: &SweepInterval<'_>,
    turns: &[WindowTurn],
    history: &HistorySource,
    page: &Page,
) -> Option<SweepWindow> {
    let turns = turns
        .iter()
        .filter(|turn| {
            interval
                .start
                .is_none_or(|start| turn.end.as_str() >= start)
                && interval
                    .end
                    .is_none_or(|end| turn.view.start.as_str() < end)
        })
        .collect::<Vec<_>>();
    let start = interval
        .start
        .map(str::to_owned)
        .or_else(|| turns.first().map(|turn| turn.view.start.clone()))?;
    // `turns --end` admits turns starting at or before it, so the window's
    // last turn start bounds exactly the turns this window counted.
    let last = turns
        .last()
        .map_or(start.as_str(), |turn| turn.view.start.as_str());
    let detail = vec![
        "--start".to_owned(),
        start.clone(),
        "--end".to_owned(),
        last.to_owned(),
    ];
    Some(SweepWindow {
        compaction: interval.anchor.map(|row| compaction_view(row, page)),
        end: turns.last().map(|turn| turn.end.clone()),
        turns: turns.len(),
        open: turns.iter().any(|turn| !turn.view.completed),
        activity: ActivityView {
            commands: turns.iter().map(|turn| turn.view.commands).sum(),
            patches: turns.iter().map(|turn| turn.view.patches).sum(),
            errors: turns.iter().map(|turn| turn.view.errors).sum(),
            web_searches: turns.iter().map(|turn| turn.view.web_searches).sum(),
        },
        latest_final: turns
            .iter()
            .rev()
            .find_map(|turn| turn.latest_final.as_deref())
            .map(|text| page.clip(text)),
        detail: session_command("turns", history, detail),
        start,
    })
}

pub(crate) fn turns(
    sources: &[ResolvedSource],
    matches: &ArgMatches,
    page: Page,
) -> Result<Report, Refusal> {
    let filters = Filters::from_matches(matches)?;
    let end = page.end(filters.end_bound())?;
    let filters = filters.ending(end.clone());
    let data = WindowScan::scan(
        sources,
        None,
        end.as_deref(),
        &filters,
        WindowOptions::Turns {
            limit: page.frontier()?,
            full: one(matches, "view").is_some_and(|view| view == "full"),
            max_text: page.limits().text,
        },
    )?;
    turns_report(data, page)
}

pub(crate) fn turns_capture(
    snapshot: capture::Snapshot,
    matches: &ArgMatches,
    page: Page,
) -> Result<Report, Refusal> {
    let filters = Filters::from_matches(matches)?;
    let end = page.end(filters.end_bound())?;
    let filters = filters.ending(end);
    let data = WindowScan::scan_capture(
        snapshot,
        &filters,
        WindowOptions::Turns {
            limit: page.frontier()?,
            full: one(matches, "view").is_some_and(|view| view == "full"),
            max_text: page.limits().text,
        },
        filters.end_bound(),
    )?;
    turns_report(data, page)
}

fn turns_report(data: WindowScan, page: Page) -> Result<Report, Refusal> {
    let rows = data.turns.into_iter().rev().map(|turn| turn.view).collect();
    let paged = page.newest(rows, data.population, |row| &row.start)?;
    Ok(report(paged, |page| Report::Turns {
        sources: data.sources,
        page,
        residues: data.residues,
    }))
}

/// A shown page as its report, or the receipt of an exported one.
fn report<T>(paged: Paged<T>, build: impl FnOnce(Bounded<T>) -> Report) -> Report {
    match paged {
        Paged::Shown(page) => build(page),
        Paged::Exported(export) => Report::Exported { export },
    }
}

/// `spice session VIEW SOURCE ARGS...`.
pub(crate) fn session_command(
    view: &str,
    history: &HistorySource,
    arguments: impl IntoIterator<Item = String>,
) -> SpiceCommand {
    SpiceCommand::new(
        "session",
        std::iter::once(view.to_owned())
            .chain(history.arguments())
            .chain(arguments),
    )
    .expect("a session view is one Spice command")
}

/// One turn; with `dialogue`, its messages (authored text only when the flag
/// is set) cut to the text bound.
pub(crate) fn turn_view(turn: &TurnRecord, dialogue: Option<(bool, usize)>) -> TurnView {
    TurnView {
        source: turn.source.clone(),
        vendor: turn.vendor,
        turn_id: turn.turn_id.clone(),
        start: turn.start_ts.clone(),
        end: turn.end_ts.clone(),
        completed: turn.completed,
        commands: turn.command_count,
        patches: turn.patch_count,
        errors: turn.error_count,
        web_searches: turn.web_search_count,
        compactions: turn.compaction_count,
        touched_paths: turn.touched_files.iter().cloned().collect(),
        dialogue: dialogue
            .map(|(authored_only, max_text)| {
                turn.messages
                    .iter()
                    .filter_map(|message| {
                        let text = if authored_only {
                            message.authored.as_deref()?
                        } else {
                            &message.text
                        };
                        Some(DialogueLine {
                            side: message.side.clone(),
                            text: page::clip(text, max_text),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn compaction_view(row: &CompactionRecord, page: &Page) -> CompactionView {
    CompactionView {
        source: row.source.clone(),
        ts: row.ts.clone(),
        assistant_before: page.clip(&row.assistant_before),
        summary_after: page.clip(&row.summary_after),
        intent: page.clip(&row.intent),
        user_after: page.clip(&row.user_after),
    }
}

pub(crate) fn turn_end(turn: &TurnRecord) -> String {
    turn.end_ts
        .clone()
        .unwrap_or_else(|| turn.last_activity_ts.clone())
}

fn optional_bound(matches: &ArgMatches, name: &str) -> Result<Option<String>, Refusal> {
    one(matches, name)
        .map(|value| normalize_bound(value))
        .transpose()
}

pub(crate) fn normalize_bound(value: &str) -> Result<String, Refusal> {
    value
        .parse::<jiff::Timestamp>()
        .ok()
        .and_then(|timestamp| wire::Instant::new(timestamp).ok())
        .map(|timestamp| timestamp.to_string())
        .ok_or_else(|| {
            refusal(
                "session::timestamp-invalid",
                Some(value.to_owned()),
                "pass an ISO-8601 timestamp with an explicit UTC offset",
            )
        })
}

pub(crate) fn values(matches: &ArgMatches, name: &str) -> Vec<String> {
    matches
        .try_get_many::<String>(name)
        .ok()
        .flatten()
        .map(|values| {
            values
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn one<'a>(matches: &'a ArgMatches, name: &str) -> Option<&'a String> {
    matches.try_get_one::<String>(name).ok().flatten()
}

fn unencodable(error: serde_json::Error) -> Refusal {
    refusal(
        "session::record-unencodable",
        Some(error.to_string()),
        "report the transcript record that cannot be encoded",
    )
}

fn refusal(reason: &str, observed: Option<String>, detail: &str) -> Refusal {
    Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason.to_owned(),
            Some("session-query".to_owned()),
            Some("one admitted bounded transcript query".to_owned()),
            observed,
        )
        .with_detail(detail),
    )
}
