use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use super::{
    Mode, Observation, RetroSession, StreamOutcome, SurfaceError, TranscriptObservation, discover,
    surface_scan,
};
use wire::output::Budget;

use crate::{Checkpoint, EventKind, Position, Scan, TailBudget, scan_tail};

/// Durable resume state for every tape one watch has observed.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WatchCursor {
    sources: Vec<WatchSourceCursor>,
    /// Status this run last reported per root or tape selector. Never
    /// serialized, so a resumed run reports current status once.
    #[serde(skip)]
    reported: BTreeMap<PathBuf, Observation>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct WatchSourceCursor {
    tape: PathBuf,
    checkpoint: Checkpoint,
    seen: BTreeSet<EventKind>,
}

impl WatchCursor {
    fn progress(&self) -> Progress {
        Progress {
            checkpoints: self
                .sources
                .iter()
                .map(|source| (source.tape.clone(), source.checkpoint.clone()))
                .collect(),
            seen: self
                .sources
                .iter()
                .map(|source| (source.tape.clone(), source.seen.clone()))
                .collect(),
            reported: self.reported.clone(),
        }
    }
}

/// Mutable resume state while one reconciliation builds its rows.
struct Progress {
    checkpoints: BTreeMap<PathBuf, Checkpoint>,
    seen: BTreeMap<PathBuf, BTreeSet<EventKind>>,
    reported: BTreeMap<PathBuf, Observation>,
}

impl Progress {
    fn cursor(&self) -> WatchCursor {
        WatchCursor {
            sources: self
                .checkpoints
                .iter()
                .map(|(tape, checkpoint)| WatchSourceCursor {
                    tape: tape.clone(),
                    checkpoint: checkpoint.clone(),
                    seen: self.seen.get(tape).cloned().unwrap_or_default(),
                })
                .collect(),
            reported: self.reported.clone(),
        }
    }

    /// Forget a tape that vanished or was replaced.
    fn forget(&mut self, tape: &Path) {
        self.seen.remove(tape);
        self.reported.remove(tape);
    }

    /// Report `status` for `selector` unless this run already reported it
    /// unchanged; `None` clears the selector so a recurrence reports again.
    fn report_status(
        &mut self,
        rows: &mut Vec<WatchDeltaRow>,
        root: &Path,
        selector: &Path,
        status: Option<Observation>,
    ) {
        let Some(status) = status else {
            self.reported.remove(selector);
            return;
        };
        if self.reported.get(selector) == Some(&status) {
            return;
        }
        self.reported.insert(selector.to_path_buf(), status.clone());
        push(rows, root, vec![status], self);
    }
}

/// One independently resumable watch row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatchDeltaRow {
    /// Existing transcript observations for one complete record or status transition.
    pub outcome: StreamOutcome,
    /// Resume state immediately after this row.
    pub cursor: WatchCursor,
}

/// Rows discovered by one bounded reconciliation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WatchDelta {
    /// Rows in deterministic tape order and chronological record order.
    pub rows: Vec<WatchDeltaRow>,
    /// Resume state after every row, including records that surfaced nothing
    /// and status already reported; adopt it only once every row is admitted.
    pub cursor: WatchCursor,
}
/// One aggregate-budgeted watch row with exact text clipping evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WatchBatch {
    /// Existing transcript observations for one complete record or status transition.
    pub outcome: StreamOutcome,
    /// UTF-8 bytes withheld across every text field in this row.
    pub withheld_text_bytes: usize,
}

impl WatchBatch {
    /// Clip every free-text field before aggregate admission.
    #[must_use]
    pub fn clipped(mut outcome: StreamOutcome, budget: Budget) -> Self {
        let mut withheld_text_bytes: usize = 0;
        outcome.observations = outcome
            .observations
            .into_iter()
            .map(|observation| {
                let clipped = TranscriptObservation::clipped(observation, budget);
                withheld_text_bytes =
                    withheld_text_bytes.saturating_add(clipped.withheld_text_bytes);
                clipped.observation
            })
            .collect();
        Self {
            outcome,
            withheld_text_bytes,
        }
    }
}

impl fmt::Display for WatchBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.outcome)?;
        if self.withheld_text_bytes > 0 {
            writeln!(
                formatter,
                "withheld {} text bytes in this row",
                self.withheld_text_bytes
            )?;
        }
        Ok(())
    }
}

/// Bounded, resumable observation over one selected root.
#[derive(Clone, Debug)]
pub struct Watch {
    root: PathBuf,
    dialects: Vec<(&'static str, crate::Dialect)>,
    cursor: WatchCursor,
}

impl Watch {
    /// Begin at the current bounded tail of every discovered tape.
    #[must_use]
    pub fn new(
        root: impl Into<PathBuf>,
        dialects: impl IntoIterator<Item = (&'static str, crate::Dialect)>,
    ) -> Self {
        Self {
            root: root.into(),
            dialects: dialects.into_iter().collect(),
            cursor: WatchCursor::default(),
        }
    }

    /// Resume from an exact cursor previously emitted for this root.
    #[must_use]
    pub fn resume(
        root: impl Into<PathBuf>,
        dialects: impl IntoIterator<Item = (&'static str, crate::Dialect)>,
        cursor: WatchCursor,
    ) -> Self {
        Self {
            root: root.into(),
            dialects: dialects.into_iter().collect(),
            cursor,
        }
    }

    /// Current durable resume state.
    #[must_use]
    pub fn cursor(&self) -> &WatchCursor {
        &self.cursor
    }

    /// Adopt an emitted row's cursor after its output was admitted.
    pub fn resume_at(&mut self, cursor: WatchCursor) {
        self.cursor = cursor;
    }

    /// Reconcile bounded current tails without advancing this watch.
    ///
    /// Every returned row carries the cursor immediately after it. The caller
    /// advances with [`Watch::resume_at`] only after admitting that row to its
    /// aggregate output budget, so an omitted row is replayed exactly once on
    /// reconnect, and adopts [`WatchDelta::cursor`] once every row is admitted.
    /// Records that surface no observations advance the cursor silently, and a
    /// root or tape status (absent, pending spans) is reported once per run
    /// until it changes.
    ///
    /// # Errors
    ///
    /// Returns a typed surface failure when discovery or a bounded tape scan
    /// cannot complete.
    pub fn poll_bounded(&self, budget: TailBudget) -> Result<WatchDelta, SurfaceError> {
        let sessions = discover(&self.root, &self.dialects)?;
        let mut progress = self.cursor.progress();
        let mut rows = Vec::new();
        let current = sessions
            .iter()
            .map(|session| session.tape.clone())
            .collect::<BTreeSet<_>>();

        let vanished = progress
            .checkpoints
            .keys()
            .filter(|tape| !current.contains(*tape))
            .cloned()
            .collect::<Vec<_>>();
        for tape in vanished {
            progress.checkpoints.remove(&tape);
            progress.forget(&tape);
            push(
                &mut rows,
                &self.root,
                vec![Observation::Missing {
                    selector: tape.display().to_string(),
                    detail: "previously observed transcript source is absent".to_owned(),
                }],
                &progress,
            );
        }

        let root_status =
            (sessions.is_empty() && progress.checkpoints.is_empty()).then(|| Observation::Absent {
                selector: self.root.display().to_string(),
            });
        progress.report_status(&mut rows, &self.root, &self.root, root_status);

        for session in sessions {
            reconcile_session(&self.root, &session, budget, &mut progress, &mut rows)?;
        }
        Ok(WatchDelta {
            rows,
            cursor: progress.cursor(),
        })
    }
}

fn reconcile_session(
    root: &Path,
    session: &RetroSession,
    budget: TailBudget,
    progress: &mut Progress,
    rows: &mut Vec<WatchDeltaRow>,
) -> Result<(), SurfaceError> {
    let tail = scan_tail(&session.tape, session.decoder(), budget).map_err(|source| {
        SurfaceError::Scan {
            path: session.tape.clone(),
            source,
        }
    })?;
    let mut next = starting_checkpoint(root, session, &tail, progress, rows);

    if next.source == tail.scan.checkpoint.source && next.next > tail.scan.checkpoint.next {
        let source = if next.next.member == tail.scan.checkpoint.next.member {
            crate::ScanError::CheckpointRetreated {
                next: next.next,
                observed_length: tail.scan.checkpoint.next.offset,
            }
        } else {
            crate::ScanError::CheckpointGap {
                next: next.next,
                observed_next_member: tail.scan.checkpoint.next.member,
            }
        };
        return Err(SurfaceError::Scan {
            path: session.tape.clone(),
            source,
        });
    }

    let mut accounted = false;
    for record in tail.scan.records {
        let start = Position {
            member: record.locus.member,
            offset: record.locus.offset,
        };
        if record.next.source != next.source || start < next.next {
            continue;
        }
        accounted = true;
        let scan = Scan {
            records: vec![record.clone()],
            checkpoint: record.next.clone(),
            pending: Vec::new(),
            extents_empty: false,
        };
        let surfaced = surface_scan(session, &scan, false);
        let observed = progress.seen.entry(session.tape.clone()).or_default();
        observed.extend(surfaced.event_kinds);
        next = record.next;
        progress
            .checkpoints
            .insert(session.tape.clone(), next.clone());
        if !surfaced.observations.is_empty() {
            push(rows, root, surfaced.observations, progress);
        }
    }

    let status = if !tail.scan.pending.is_empty() {
        Some(Observation::Pending {
            lane: session.lane.clone(),
            dialect: session.dialect,
            tape: session.tape.clone(),
            spans: tail.scan.pending,
        })
    } else {
        tail.scan.extents_empty.then(|| Observation::Absent {
            selector: session.tape.display().to_string(),
        })
    };
    progress.report_status(rows, root, &session.tape, status);

    let observed = progress.seen.entry(session.tape.clone()).or_default();
    let canaries = EventKind::ALL
        .into_iter()
        .filter(|kind| !observed.contains(kind))
        .map(|expected| Observation::Canary {
            lane: session.lane.clone(),
            dialect: session.dialect,
            tape: session.tape.clone(),
            expected,
        })
        .collect::<Vec<_>>();
    if accounted && !canaries.is_empty() {
        observed.extend(EventKind::ALL);
        push(rows, root, canaries, progress);
    }
    Ok(())
}

fn starting_checkpoint(
    root: &Path,
    session: &RetroSession,
    tail: &crate::TailScan,
    progress: &mut Progress,
    rows: &mut Vec<WatchDeltaRow>,
) -> Checkpoint {
    let previous = progress.checkpoints.get(&session.tape).cloned();
    match previous {
        Some(previous) if previous.source != tail.start.source => {
            progress.forget(&session.tape);
            progress
                .checkpoints
                .insert(session.tape.clone(), tail.start.clone());
            push(
                rows,
                root,
                vec![Observation::Missing {
                    selector: session.tape.display().to_string(),
                    detail: format!(
                        "transcript source replaced: expected {}, observed {}",
                        previous.source, tail.start.source
                    ),
                }],
                progress,
            );
            tail.start.clone()
        }
        Some(previous) if previous.next < tail.start.next => {
            progress
                .checkpoints
                .insert(session.tape.clone(), tail.start.clone());
            push(
                rows,
                root,
                vec![Observation::Missing {
                    selector: session.tape.display().to_string(),
                    detail: format!(
                        "bounded tail skipped {} older source bytes before {}:{}",
                        tail.withheld_source_bytes, tail.start.next.member, tail.start.next.offset
                    ),
                }],
                progress,
            );
            tail.start.clone()
        }
        Some(previous) => previous,
        None => {
            progress
                .checkpoints
                .insert(session.tape.clone(), tail.start.clone());
            if tail.withheld_source_bytes > 0 {
                push(
                    rows,
                    root,
                    vec![Observation::Missing {
                        selector: session.tape.display().to_string(),
                        detail: format!(
                            "bounded current snapshot skipped {} older source bytes",
                            tail.withheld_source_bytes
                        ),
                    }],
                    progress,
                );
            }
            tail.start.clone()
        }
    }
}

fn push(
    rows: &mut Vec<WatchDeltaRow>,
    root: &Path,
    observations: Vec<Observation>,
    progress: &Progress,
) {
    rows.push(WatchDeltaRow {
        outcome: StreamOutcome {
            mode: Mode::Watch,
            root: root.to_path_buf(),
            observations,
        },
        cursor: progress.cursor(),
    });
}
