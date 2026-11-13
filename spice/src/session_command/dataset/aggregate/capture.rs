use wire::Refusal;

use super::{WindowOptions, WindowScan, WindowTurn, WindowTurnFrontier};
use crate::session_command::capture;
use crate::session_command::dataset::{
    CompactionTracker, DecodedRow, FoldState, FoldStorage, ResidueLedger, SessionWork,
    SessionWorkBudget, captured_row, finish_fold, reduce_record, source_view_text_bytes,
};
use crate::session_command::model::{CompactionRecord, FoldBuffer, Residue, SourceView};
use crate::session_command::project::{Filters, turn_end};

pub(crate) struct CaptureFold {
    pub(crate) source: SourceView,
    pub(crate) data: FoldBuffer,
    pub(crate) compactions: Vec<CompactionRecord>,
    pub(crate) compaction_population: usize,
    pub(crate) residues: Vec<Residue>,
}

pub(crate) fn fold_capture(
    snapshot: capture::Snapshot,
    compactions: usize,
    end: Option<&str>,
    retain_closing_after_end: bool,
) -> Result<CaptureFold, Refusal> {
    let mut work = SessionWork::new(SessionWorkBudget::report());
    let mut residues = ResidueLedger::new();
    let mut data = FoldBuffer::default();
    let mut markers = Vec::new();
    let mut state = FoldState::default();
    let mut tracker = CompactionTracker::new(compactions, None, end)?;
    if retain_closing_after_end {
        tracker.retain_closing_after_end();
    }
    let mut prior_timestamp = None;
    for (index, record) in snapshot.records.into_iter().enumerate() {
        let at = format!("{}:record {}", snapshot.source_name, index + 1);
        match captured_row(
            &snapshot.source_name,
            (index + 1) as u64,
            snapshot.vendor,
            record,
            prior_timestamp.as_deref(),
        )? {
            DecodedRow::Typed(record) => {
                prior_timestamp.clone_from(&record.context.timestamp);
                let before = markers.len();
                reduce_record(
                    &record,
                    &mut FoldStorage {
                        dataset: &mut data,
                        markers: &mut markers,
                        residues: &mut residues,
                        state: &mut state,
                    },
                    &mut work,
                    &at,
                )?;
                let added = markers.len() - before;
                if added > 0
                    && let Some(turn) = state
                        .current
                        .or_else(|| data.turns.len().checked_sub(1))
                        .and_then(|index| data.turns.get_mut(index))
                {
                    turn.compaction_count = turn.compaction_count.saturating_add(added as u64);
                }
                tracker.observe(markers.drain(..), &data.messages, &mut work, &at)?;
                data.messages.clear();
            }
            DecodedRow::Residue(residue) => residues.push(residue, &mut work, &at)?,
            DecodedRow::Ignored => {}
        }
    }
    finish_fold(&mut data, state, &mut work, &snapshot.source_name)?;
    let (compactions, compaction_population) = tracker.finish(&mut work, &snapshot.source_name)?;
    let source = SourceView {
        name: if snapshot.withheld_source_bytes == 0 {
            "selected capture".to_owned()
        } else {
            format!(
                "selected capture ({} older source bytes withheld)",
                snapshot.withheld_source_bytes
            )
        },
        path: snapshot.source_name,
        vendor: snapshot.vendor,
        thread: Some(snapshot.binding.session().to_owned()),
        compressed: false,
    };
    work.admit_retained_text(source_view_text_bytes(&source), &source.path)?;
    Ok(CaptureFold {
        source,
        data,
        compactions,
        compaction_population,
        residues: residues.finish(&mut work, "capture residue summary")?,
    })
}

impl WindowScan {
    pub(crate) fn scan_capture(
        snapshot: capture::Snapshot,
        filters: &Filters,
        options: WindowOptions,
        compaction_end: Option<&str>,
    ) -> Result<Self, Refusal> {
        let sweep = matches!(options, WindowOptions::Sweep { .. });
        let folded = fold_capture(snapshot, options.compactions(), compaction_end, sweep)?;
        let any_turn_identity = folded.data.turns.iter().any(|turn| turn.turn_id.is_some());
        filters.validate_turn_identity_available(any_turn_identity)?;
        let closing = compaction_end.and_then(|end| {
            folded
                .compactions
                .last()
                .filter(|row| row.ts.as_str() > end)
                .map(|row| row.ts.clone())
        });
        let selected = folded
            .compactions
            .len()
            .saturating_sub(usize::from(closing.is_some()));
        let sweep_start = sweep
            .then(|| {
                folded.compactions[..selected]
                    .first()
                    .map(|row| row.ts.clone())
            })
            .flatten();
        let mut turns = WindowTurnFrontier::new(options);
        for turn in folded.data.turns {
            let in_sweep = !sweep
                || (sweep_start
                    .as_ref()
                    .is_none_or(|start| turn_end(&turn) >= *start)
                    && closing.as_ref().is_none_or(|end| turn.start_ts < *end));
            if in_sweep && filters.turn_matches(&turn) {
                turns.push(WindowTurn::project(turn, options))?;
            }
        }
        Ok(Self {
            sources: vec![folded.source],
            population: turns.population,
            turns: turns.rows.into(),
            compactions: folded.compactions,
            compaction_population: folded.compaction_population,
            residues: folded.residues,
        })
    }
}
