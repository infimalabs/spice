//! Bounded windows of turns and compactions for the briefing, sweep, and turns reads.

use std::collections::VecDeque;

use wire::Refusal;

use super::super::model::{CompactionRecord, Residue, SourceView, TurnRecord, TurnView, Vendor};
use super::super::project::{Filters, turn_end, turn_view};
use super::super::source::ResolvedSource;
use super::stream::{ForwardCursor, ForwardProgress, compressed};
use super::{
    CompactionTracker, DecodedRow, FoldState, FoldStorage, FrontierDirection, Marker,
    OPEN_TURN_LIMIT, ResidueLedger, ResultFrontier, SessionWork, SessionWorkBudget,
    admit_source_order, finish_fold, reduce_record, refusal, source_view_text_bytes,
};

mod capture;
const SOURCE_LIMIT: usize = 64;
const WINDOW_TURN_LIMIT: usize = 4 * 1_024;

/// Turns of the live transcript a briefing retains: the newest, and the one
/// before it so an open turn still has the latest final answer beside it.
const LIVE_TURNS: usize = 2;

#[derive(Clone, Copy)]
pub(crate) enum WindowOptions {
    /// The newest turns after the latest compaction, without dialogue.
    Live,
    Sweep {
        compactions: usize,
    },
    Turns {
        limit: usize,
        full: bool,
        max_text: usize,
    },
}

impl WindowOptions {
    pub(crate) const fn compactions(self) -> usize {
        match self {
            Self::Sweep { compactions } => compactions,
            Self::Live | Self::Turns { .. } => 1,
        }
    }
}

#[derive(Clone)]
pub(crate) struct WindowTurn {
    pub(crate) view: TurnView,
    pub(crate) end: String,
    pub(crate) prompt: Option<String>,
    pub(crate) latest_final: Option<String>,
}

impl WindowTurn {
    fn project(turn: TurnRecord, options: WindowOptions) -> Self {
        let latest_final = turn.messages.iter().rev().find_map(|message| {
            (message.phase == "final_answer")
                .then(|| message.authored.clone())
                .flatten()
        });
        let prompt = turn
            .messages
            .iter()
            .find(|message| message.side == "user")
            .and_then(|message| message.authored.clone());
        let end = turn_end(&turn);
        let view = match options {
            WindowOptions::Turns { full, max_text, .. } => {
                turn_view(&turn, Some((!full, max_text)))
            }
            WindowOptions::Live | WindowOptions::Sweep { .. } => turn_view(&turn, None),
        };
        Self {
            view,
            end,
            prompt,
            latest_final,
        }
    }
}

pub(crate) struct WindowScan {
    pub(crate) sources: Vec<SourceView>,
    pub(crate) turns: Vec<WindowTurn>,
    pub(crate) compactions: Vec<CompactionRecord>,
    pub(crate) residues: Vec<Residue>,
    /// Every turn the scan matched, retained or not.
    pub(crate) population: usize,
    /// Every compaction matched by the capture bound, excluding its closing row.
    pub(crate) compaction_population: usize,
}

impl WindowScan {
    pub(crate) fn scan(
        sources: &[ResolvedSource],
        start: Option<&str>,
        end: Option<&str>,
        filters: &Filters,
        options: WindowOptions,
    ) -> Result<Self, Refusal> {
        admit_sources(sources)?;
        let mut work = SessionWork::new(SessionWorkBudget::report());
        let mut residues = ResidueLedger::new();
        let mut streams = sources
            .iter()
            .map(|source| {
                WindowSource::window(source, start, end, options.compactions(), &mut work)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut turns = WindowTurnFrontier::new(options);
        let mut any_turn_identity = false;
        merge_ready_turns(&mut streams, &mut residues, &mut work, |turn| {
            any_turn_identity |= turn.turn_id.is_some();
            if filters.turn_matches(&turn) {
                turns.push(WindowTurn::project(turn, options))?;
            }
            Ok(())
        })?;
        filters.validate_turn_identity_available(any_turn_identity)?;
        let mut sources = Vec::with_capacity(streams.len());
        let mut compaction_sources = Vec::with_capacity(streams.len());
        let mut compaction_population = 0_usize;
        for stream in streams {
            let (source, compactions, population) = stream.into_window(&mut work)?;
            work.admit_retained_text(source_view_text_bytes(&source), &source.path)?;
            sources.push(source);
            compaction_sources.push(VecDeque::from(compactions));
            compaction_population = compaction_population.saturating_add(population);
        }
        let compactions = merge_compactions(
            compaction_sources,
            options.compactions(),
            FrontierDirection::Newest,
        )?;

        Ok(Self {
            sources,
            population: turns.population,
            turns: turns.rows.into(),
            compactions,
            compaction_population,
            residues: residues.finish(&mut work, "window residue summary")?,
        })
    }
}

struct WindowTurnFrontier {
    options: WindowOptions,
    rows: VecDeque<WindowTurn>,
    population: usize,
}

impl WindowTurnFrontier {
    fn new(options: WindowOptions) -> Self {
        Self {
            options,
            rows: VecDeque::new(),
            population: 0,
        }
    }

    fn push(&mut self, turn: WindowTurn) -> Result<(), Refusal> {
        let retained = match self.options {
            WindowOptions::Live => LIVE_TURNS,
            WindowOptions::Turns { limit, .. } => limit,
            WindowOptions::Sweep { .. } => {
                if self.rows.len() == WINDOW_TURN_LIMIT {
                    return Err(refusal(
                        "session::window-turn-limit",
                        Some(format!("maximum={WINDOW_TURN_LIMIT}")),
                        "narrow the requested compaction windows",
                    ));
                }
                WINDOW_TURN_LIMIT
            }
        };
        self.population = self.population.saturating_add(1);
        self.rows.push_back(turn);
        if self.rows.len() > retained {
            self.rows.pop_front();
        }
        Ok(())
    }
}

pub(super) fn merge_compactions(
    mut sources: Vec<VecDeque<CompactionRecord>>,
    limit: usize,
    direction: FrontierDirection,
) -> Result<Vec<CompactionRecord>, Refusal> {
    let mut rows = ResultFrontier::new(limit, direction)?;
    while let Some(index) = sources
        .iter()
        .enumerate()
        .filter(|(_, source)| !source.is_empty())
        .min_by_key(|(_, source)| {
            let row = source
                .front()
                .expect("filtered compaction source is nonempty");
            (&row.ts, &row.source)
        })
        .map(|(index, _)| index)
    {
        rows.push(
            sources[index]
                .pop_front()
                .expect("selected compaction source is nonempty"),
        );
    }
    Ok(rows.into_rows())
}

pub(super) fn source_view(source: &ResolvedSource, vendor: Vendor) -> SourceView {
    let path = source.path.display().to_string();
    SourceView {
        name: source
            .path
            .file_name()
            .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned()),
        path,
        vendor,
        thread: None,
        compressed: compressed(&source.path),
    }
}

struct WindowSource<'a> {
    source: &'a ResolvedSource,
    cursor: ForwardCursor<'a>,
    scratch: super::super::model::FoldBuffer,
    markers: Vec<Marker>,
    fold: FoldState,
    ready: VecDeque<TurnRecord>,
    last_turn: Option<String>,
    turn_start: Option<String>,
    compactions: CompactionTracker,
    complete: bool,
}

impl<'a> WindowSource<'a> {
    fn window(
        source: &'a ResolvedSource,
        start: Option<&str>,
        end: Option<&str>,
        compactions: usize,
        work: &mut SessionWork,
    ) -> Result<Self, Refusal> {
        Ok(Self {
            source,
            cursor: ForwardCursor::new(source, start, work)?,
            scratch: super::super::model::FoldBuffer::default(),
            markers: Vec::new(),
            fold: FoldState::default(),
            ready: VecDeque::with_capacity(OPEN_TURN_LIMIT),
            last_turn: None,
            turn_start: start.map(str::to_owned),
            compactions: CompactionTracker::new(compactions, start, end)?,
            complete: false,
        })
    }

    fn advance(
        &mut self,
        residues: &mut ResidueLedger,
        work: &mut SessionWork,
    ) -> Result<(), Refusal> {
        while self.ready.is_empty() && !self.complete {
            let window = self.cursor.advance(work)?;
            if let Some(row) = window.row {
                match row {
                    DecodedRow::Typed(record) => {
                        let before = self.markers.len();
                        reduce_record(
                            &record,
                            &mut FoldStorage {
                                dataset: &mut self.scratch,
                                markers: &mut self.markers,
                                residues,
                                state: &mut self.fold,
                            },
                            work,
                            &self.cursor.record_locus(),
                        )?;
                        let added = self.markers.len() - before;
                        if added > 0
                            && let Some(turn) = self
                                .fold
                                .current
                                .or_else(|| self.scratch.turns.len().checked_sub(1))
                                .and_then(|index| self.scratch.turns.get_mut(index))
                        {
                            turn.compaction_count =
                                turn.compaction_count.saturating_add(added as u64);
                        }
                        self.compactions.observe(
                            self.markers.drain(..),
                            &self.scratch.messages,
                            work,
                            &self.cursor.record_locus(),
                        )?;
                    }
                    DecodedRow::Residue(residue) => {
                        residues.push(residue, work, &self.cursor.record_locus())?;
                    }
                    DecodedRow::Ignored => {}
                }
                self.scratch.messages.clear();
                self.drain_ready(false)?;
            }
            if !self.complete && matches!(window.progress, ForwardProgress::Eof(_)) {
                let state = std::mem::take(&mut self.fold);
                finish_fold(
                    &mut self.scratch,
                    state,
                    work,
                    &self.source.path.display().to_string(),
                )?;
                self.drain_ready(true)?;
                self.complete = true;
            }
        }
        Ok(())
    }

    fn drain_ready(&mut self, finish: bool) -> Result<(), Refusal> {
        let at = self.source.path.display().to_string();
        let ready = &mut self.ready;
        let last_turn = &mut self.last_turn;
        let turn_start = self.turn_start.as_deref();
        drain_fold_turns(&mut self.scratch, &mut self.fold, finish, &at, |turn| {
            admit_source_order(last_turn, &turn.start_ts)?;
            if turn_start.is_none_or(|start| turn_end(&turn).as_str() >= start) {
                ready.push_back(turn);
            }
            Ok(())
        })
    }

    /// The source view, its retained compactions, and every compaction the
    /// window matched.
    fn into_window(
        self,
        work: &mut SessionWork,
    ) -> Result<(SourceView, Vec<CompactionRecord>, usize), Refusal> {
        let (compactions, population) = self
            .compactions
            .finish(work, &self.source.path.display().to_string())?;
        Ok((
            source_view(self.source, self.cursor.vendor()),
            compactions,
            population,
        ))
    }
}

fn drain_fold_turns(
    scratch: &mut super::super::model::FoldBuffer,
    fold: &mut FoldState,
    finish: bool,
    at: &str,
    mut accept: impl FnMut(TurnRecord) -> Result<(), Refusal>,
) -> Result<(), Refusal> {
    let retain = if finish {
        scratch.turns.len()
    } else {
        fold.closed_prefix(scratch.turns.len())
    };
    for turn in scratch.turns.drain(..retain) {
        accept(turn)?;
    }
    fold.release(retain, scratch.turns.len(), at)
}

/// Hands every buffered turn across `streams` to `accept` in start-time then
/// source order, refilling each stream's ready frontier as it empties.
fn merge_ready_turns(
    streams: &mut [WindowSource],
    residues: &mut ResidueLedger,
    work: &mut SessionWork,
    mut accept: impl FnMut(TurnRecord) -> Result<(), Refusal>,
) -> Result<(), Refusal> {
    for stream in streams.iter_mut() {
        stream.advance(residues, work)?;
    }
    while let Some(index) = streams
        .iter()
        .enumerate()
        .filter(|(_, stream)| !stream.ready.is_empty())
        .min_by_key(|(_, stream)| {
            let turn = stream.ready.front().expect("filtered nonempty frontier");
            (&turn.start_ts, &turn.source)
        })
        .map(|(index, _)| index)
    {
        let turn = streams[index]
            .ready
            .pop_front()
            .expect("selected frontier is nonempty");
        accept(turn)?;
        streams[index].advance(residues, work)?;
    }
    Ok(())
}

pub(crate) fn admit_sources(sources: &[ResolvedSource]) -> Result<(), Refusal> {
    if sources.len() <= SOURCE_LIMIT {
        return Ok(());
    }
    Err(refusal(
        "session::aggregate-source-limit",
        Some(format!("observed={} maximum={SOURCE_LIMIT}", sources.len())),
        "select at most the repository aggregate source ceiling and retry",
    ))
}
