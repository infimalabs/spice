use super::*;

impl DurableHistory {
    /// Chain older windows behind `newest` until the page holds `limit` visible
    /// cards or the tape begins.
    ///
    /// A tool-heavy lane can run for megabytes without one visible card, so a
    /// single window cannot promise a page. The request keeps reading the way
    /// the predecessor did, bounded by [`HISTORY_READ_LIMIT`] windows and
    /// [`HISTORY_DURATION`] of scan time; a page still short keeps `more` and
    /// its continuation so the next request resumes where this one stopped.
    pub(super) fn fill_tape_messages(
        &self,
        lane: CanonicalUuid,
        source: &DurableLane,
        before: Option<&wire::AgentMessage>,
        limit: usize,
        newest: TapeHistoryScan,
    ) -> Result<TapeHistoryScan, String> {
        let mut scan = newest;
        let mut reads = 1;
        while scan.messages.len() < limit
            && scan.start.next != listen::Position::start()
            && reads < HISTORY_READ_LIMIT
            && scan.work.duration < HISTORY_DURATION
        {
            let tail = history_tail(&source.tape, source.dialect, &scan.start)
                .map_err(history_tail_error)?;
            let remaining = limit - scan.messages.len();
            let older = self.collect_tape_messages(lane, source, before, remaining, tail)?;
            reads += 1;
            scan = older.extend_newer(scan);
        }
        Ok(scan)
    }
}

impl TapeHistoryScan {
    /// Join `newer`, the window read immediately after this one, onto it.
    ///
    /// `more` and `complete` stay as this older window judged them: the newer
    /// window was only chained onto because it held every card it found and
    /// only claimed more because this one had not been read yet.
    fn extend_newer(mut self, newer: Self) -> Self {
        debug_assert!(self.end.next <= newer.start.next);
        self.messages.extend(newer.messages);
        self.end = newer.end;
        self.work.source_bytes = self
            .work
            .source_bytes
            .saturating_add(newer.work.source_bytes);
        self.work.records = self.work.records.saturating_add(newer.work.records);
        self.work.passes = self.work.passes.saturating_add(newer.work.passes);
        self.work.duration = self.work.duration.saturating_add(newer.work.duration);
        self
    }
}

pub(super) fn history_newest(
    tape: &Path,
    dialect: listen::Dialect,
) -> Result<listen::TailScan, listen::ScanError> {
    listen::scan_tail(tape, dialect, history_budget())
}

pub(super) fn history_tail(
    tape: &Path,
    dialect: listen::Dialect,
    before: &listen::Checkpoint,
) -> Result<listen::TailScan, listen::ScanError> {
    listen::scan_before(tape, dialect, before, history_budget())
}

pub(super) fn history_tail_error(error: listen::ScanError) -> String {
    if matches!(error, listen::ScanError::SourceChanged { .. }) {
        "the transcript source changed after app bootstrap".to_owned()
    } else {
        error.to_string()
    }
}

pub(super) struct HistorySources {
    pub(super) tape: Option<TapeHistoryScan>,
}

impl HistorySources {
    pub(super) fn page(
        &self,
        before: Option<&wire::AgentMessage>,
        limit: usize,
    ) -> (Vec<wire::AgentMessage>, bool) {
        let mut visible = self
            .tape
            .as_ref()
            .map_or_else(Vec::new, |tape| tape.messages.clone());
        let more = self.tape.as_ref().is_some_and(|tape| tape.more);
        visible.sort_by_key(wire::AgentMessage::chronology_key);
        visible.dedup_by(|right, left| right.locus == left.locus);
        for position in wire::superseded_prose(&visible).into_iter().rev() {
            visible.remove(position);
        }
        let end = before.map_or(visible.len(), |before| {
            visible.partition_point(|message| message.chronology_key() < before.chronology_key())
        });
        let start = end.saturating_sub(limit);
        (visible[start..end].to_vec(), more || start > 0)
    }
}
