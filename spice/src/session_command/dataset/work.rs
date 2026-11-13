//! Fixed work accounting for one session transcript reduction.

use std::time::{Duration, Instant};

use super::*;

const REPORT_SOURCE_BYTES: u64 = 512 * 1_024 * 1_024;
const REPORT_RECORDS: u64 = 256 * 1_024;
const REPORT_EVENTS: u64 = 1_024 * 1_024;
const REPORT_RETAINED_TEXT_BYTES: u64 = 512 * 1_024 * 1_024;
const REPORT_PASSES: u32 = 64;
const REPORT_DURATION: Duration = Duration::from_secs(30);
const MAX_RESIDUES: usize = 256;
const MAX_RESIDUE_BYTES: u64 = 256 * 1_024;
const RESIDUE_SUMMARY_RESERVE: u64 = 512;
// Three decimal u64 values plus the fixed summary prose occupy at most 116 bytes.
const RESIDUE_GROUP_SUMMARY_RESERVE: u64 = 128;

pub(super) fn decoded_row_events(row: &DecodedRow) -> usize {
    match row {
        DecodedRow::Typed(record) => record.events.len(),
        DecodedRow::Residue(_) | DecodedRow::Ignored => 0,
    }
}

pub(crate) fn text_bytes<'a>(values: impl IntoIterator<Item = &'a str>) -> u64 {
    values
        .into_iter()
        .map(|value| u64::try_from(value.len()).unwrap_or(u64::MAX))
        .fold(0, u64::saturating_add)
}

#[derive(Clone, Copy)]
pub(crate) struct SessionWorkBudget {
    source_bytes: u64,
    records: u64,
    events: u64,
    retained_text_bytes: u64,
    passes: u32,
    duration: Duration,
}

impl SessionWorkBudget {
    pub(crate) fn report() -> Self {
        Self::new(
            REPORT_SOURCE_BYTES,
            REPORT_RECORDS,
            REPORT_EVENTS,
            REPORT_RETAINED_TEXT_BYTES,
            REPORT_PASSES,
            REPORT_DURATION,
        )
        .expect("repository session report budget is positive")
    }

    pub(super) fn new(
        source_bytes: u64,
        records: u64,
        events: u64,
        retained_text_bytes: u64,
        passes: u32,
        duration: Duration,
    ) -> Result<Self, Refusal> {
        if source_bytes == 0
            || records == 0
            || events == 0
            || retained_text_bytes == 0
            || passes == 0
            || duration.is_zero()
        {
            return Err(refusal(
                "session::transcript-work-budget-invalid",
                Some(format!(
                    "source_bytes={source_bytes} records={records} events={events} retained_text_bytes={retained_text_bytes} passes={passes} duration={duration:?}"
                )),
                "use one positive repository-owned session work allowance",
            ));
        }
        Ok(Self {
            source_bytes,
            records,
            events,
            retained_text_bytes,
            passes,
            duration,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct SessionWorkReport {
    pub(super) source_bytes: u64,
    pub(super) records: u64,
    pub(super) events: u64,
    /// Cumulative logical bytes of each `String` admitted into fold state.
    pub(super) retained_text_bytes: u64,
    pub(super) passes: u32,
    pub(super) elapsed: Duration,
}

pub(crate) struct SessionWork {
    budget: SessionWorkBudget,
    report: SessionWorkReport,
    started: Instant,
}

impl SessionWork {
    pub(crate) fn new(budget: SessionWorkBudget) -> Self {
        Self {
            budget,
            report: SessionWorkReport::default(),
            started: Instant::now(),
        }
    }

    pub(super) fn begin_pass(&mut self, at: &str) -> Result<(), Refusal> {
        self.check_deadline(at)?;
        let next = self.report.passes.saturating_add(1);
        if next > self.budget.passes {
            return Err(self.exhausted("passes", at));
        }
        self.report.passes = next;
        Ok(())
    }

    pub(super) fn admit_source_bytes(&mut self, bytes: u64, at: &str) -> Result<(), Refusal> {
        self.check_deadline(at)?;
        let next = self.report.source_bytes.checked_add(bytes);
        if next.is_none_or(|next| next > self.budget.source_bytes) {
            return Err(self.exhausted("source-bytes", at));
        }
        self.report.source_bytes = next.expect("admitted source-byte sum exists");
        Ok(())
    }

    pub(super) fn admit_record(&mut self, at: &str) -> Result<(), Refusal> {
        self.ensure_record(at)?;
        self.report.records = self.report.records.saturating_add(1);
        Ok(())
    }

    pub(super) fn ensure_record(&mut self, at: &str) -> Result<(), Refusal> {
        self.check_deadline(at)?;
        if self.report.records >= self.budget.records {
            Err(self.exhausted("records", at))
        } else {
            Ok(())
        }
    }

    pub(super) fn admit_decoded(&mut self, events: usize, at: &str) -> Result<(), Refusal> {
        self.check_deadline(at)?;
        let events = u64::try_from(events).unwrap_or(u64::MAX);
        let next_events = self.report.events.checked_add(events);
        if next_events.is_none_or(|next| next > self.budget.events) {
            return Err(self.exhausted("events", at));
        }
        self.report.events = next_events.expect("admitted event sum exists");
        Ok(())
    }

    /// Charge one allocation before its `String` enters retained fold state.
    pub(crate) fn admit_retained_text(&mut self, bytes: u64, at: &str) -> Result<(), Refusal> {
        self.check_deadline(at)?;
        let next_text = self.report.retained_text_bytes.checked_add(bytes);
        if next_text.is_none_or(|next| next > self.budget.retained_text_bytes) {
            return Err(self.exhausted("retained-text-bytes", at));
        }
        self.report.retained_text_bytes = next_text.expect("admitted text-byte sum exists");
        Ok(())
    }

    pub(super) fn check_deadline(&mut self, at: &str) -> Result<(), Refusal> {
        self.report.elapsed = self.started.elapsed();
        if self.report.elapsed > self.budget.duration {
            Err(self.exhausted("wall-duration", at))
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    pub(super) fn report(&mut self) -> SessionWorkReport {
        self.report.elapsed = self.started.elapsed();
        self.report
    }

    fn exhausted(&self, limit: &str, at: &str) -> Refusal {
        refusal(
            "session::transcript-work-exhausted",
            Some(format!(
                "limit={limit} at={at:?} work=source_bytes:{} records:{} events:{} retained_text_bytes:{} passes:{} elapsed:{:?}",
                self.report.source_bytes,
                self.report.records,
                self.report.events,
                self.report.retained_text_bytes,
                self.report.passes,
                self.report.elapsed,
            )),
            "narrow the immutable transcript inputs or filters and retry; the observed source boundary identifies the exact exhausted position",
        )
    }
}

struct ResidueGroup {
    row: Residue,
    count: u64,
    first_line: u64,
    last_line: u64,
}

impl ResidueGroup {
    fn new(row: Residue) -> Self {
        let line = row.line;
        Self {
            row,
            count: 1,
            first_line: line,
            last_line: line,
        }
    }

    fn matches(&self, candidate: &Residue) -> bool {
        self.row.source == candidate.source
            && self.row.kind == candidate.kind
            && self.row.detail == candidate.detail
            && self.row.recovery == candidate.recovery
    }

    fn observe(&mut self, line: u64) {
        self.count = self.count.saturating_add(1);
        self.first_line = self.first_line.min(line);
        self.last_line = self.last_line.max(line);
    }

    fn finish(mut self) -> Residue {
        if self.count > 1 {
            self.row.line = self.first_line;
            self.row.detail = format!(
                "{} records matched this residue from line {} through line {}: {}",
                self.count, self.first_line, self.last_line, self.row.detail
            );
        }
        self.row
    }
}

pub(crate) struct ResidueLedger {
    rows: Vec<ResidueGroup>,
    bytes: u64,
    summary_reserve: u64,
    omitted: u64,
    omitted_bytes: u64,
}

impl ResidueLedger {
    pub(crate) fn new() -> Self {
        Self {
            rows: Vec::new(),
            bytes: 0,
            summary_reserve: 0,
            omitted: 0,
            omitted_bytes: 0,
        }
    }

    pub(crate) fn push(
        &mut self,
        residue: Residue,
        work: &mut SessionWork,
        at: &str,
    ) -> Result<(), Refusal> {
        let retained_bytes = residue_bytes(&residue);
        if let Some(index) = self.rows.iter().position(|row| row.matches(&residue)) {
            let reserve = u64::from(self.rows[index].count == 1)
                .saturating_mul(RESIDUE_GROUP_SUMMARY_RESERVE);
            let fits = self
                .bytes
                .checked_add(self.summary_reserve)
                .and_then(|bytes| bytes.checked_add(reserve))
                .and_then(|bytes| bytes.checked_add(RESIDUE_SUMMARY_RESERVE))
                .is_some_and(|bytes| bytes <= MAX_RESIDUE_BYTES);
            if fits {
                self.summary_reserve = self.summary_reserve.saturating_add(reserve);
                self.rows[index].observe(residue.line);
            } else {
                self.omitted = self.omitted.saturating_add(1);
                self.omitted_bytes = self.omitted_bytes.saturating_add(retained_bytes);
            }
            return Ok(());
        }

        let fits = self.rows.len() < MAX_RESIDUES.saturating_sub(1)
            && self
                .bytes
                .checked_add(self.summary_reserve)
                .and_then(|bytes| bytes.checked_add(retained_bytes))
                .and_then(|bytes| bytes.checked_add(RESIDUE_SUMMARY_RESERVE))
                .is_some_and(|bytes| bytes <= MAX_RESIDUE_BYTES);
        if fits {
            work.admit_retained_text(retained_bytes, at)?;
            self.bytes = self.bytes.saturating_add(retained_bytes);
            self.rows.push(ResidueGroup::new(residue));
        } else {
            self.omitted = self.omitted.saturating_add(1);
            self.omitted_bytes = self.omitted_bytes.saturating_add(retained_bytes);
        }
        Ok(())
    }

    pub(crate) fn finish(self, work: &mut SessionWork, at: &str) -> Result<Vec<Residue>, Refusal> {
        let mut rows = Vec::with_capacity(self.rows.len() + usize::from(self.omitted > 0));
        for group in self.rows {
            let original_bytes = residue_bytes(&group.row);
            let row = group.finish();
            let summary_bytes = residue_bytes(&row).saturating_sub(original_bytes);
            debug_assert!(summary_bytes <= RESIDUE_GROUP_SUMMARY_RESERVE);
            if summary_bytes > 0 {
                work.admit_retained_text(summary_bytes, at)?;
            }
            rows.push(row);
        }
        if self.omitted > 0 {
            let summary = Residue {
                source: String::new(),
                line: 0,
                kind: "residue-overflow".to_owned(),
                detail: format!(
                    "{} transcript residues totaling {} logical text bytes were omitted",
                    self.omitted, self.omitted_bytes
                ),
                recovery: "narrow the transcript inputs to inspect omitted residue evidence"
                    .to_owned(),
            };
            work.admit_retained_text(residue_bytes(&summary), at)?;
            rows.push(summary);
        }
        Ok(rows)
    }
}

fn residue_bytes(residue: &Residue) -> u64 {
    [
        &residue.source,
        &residue.kind,
        &residue.detail,
        &residue.recovery,
    ]
    .into_iter()
    .map(|value| u64::try_from(value.len()).unwrap_or(u64::MAX))
    .fold(0, u64::saturating_add)
}
