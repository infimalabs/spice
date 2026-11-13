//! Current durable lane-hold projection.

use rusqlite::params;

use super::{GuardError, JournalRead, decode_bounded_fact, read_metrics};

impl JournalRead<'_> {
    /// Read the latest hold/resume transition for one lane at this read's
    /// cursor.
    pub fn current_lane_hold(
        &self,
        lane: wire::CanonicalUuid,
    ) -> Result<Option<wire::body::LaneHeld>, GuardError> {
        let Some(cursor) = self.cursor else {
            return Ok(None);
        };
        self.lane_hold_at(lane, cursor)
    }

    /// Read the latest hold/resume transition for one lane at or before
    /// `horizon`, so a reply built at an earlier barrier projects the hold
    /// current at that barrier. A horizon past this read's cursor is read at
    /// the cursor.
    ///
    /// The fact-kind index narrows this to lane-hold transitions, while JSON
    /// extraction selects the exact lane before the one matching envelope is
    /// decoded and validated. A resume is the authoritative absence of a hold.
    pub fn lane_hold_at(
        &self,
        lane: wire::CanonicalUuid,
        horizon: wire::Seq,
    ) -> Result<Option<wire::body::LaneHeld>, GuardError> {
        let Some(cursor) = self.cursor else {
            return Ok(None);
        };
        let horizon = i64::try_from(horizon.min(cursor).get())
            .map_err(|_| GuardError::Invalid("lane hold horizon exceeds SQLite".to_owned()))?;
        read_metrics::observe_query();
        let mut statement = self
            .connection
            .prepare(
                "SELECT seq, kind, length(envelope), envelope FROM fact_envelopes \
                 WHERE seq <= ?1 AND kind IN (?2, ?3) \
                 AND json_extract(CAST(envelope AS TEXT), '$.fact.lane') = ?4 \
                 ORDER BY seq DESC LIMIT 1",
            )
            .map_err(GuardError::Store)?;
        let mut rows = statement
            .query(params![
                horizon,
                wire::FactKind::LaneHeld.as_str(),
                wire::FactKind::LaneResumed.as_str(),
                lane.to_string(),
            ])
            .map_err(GuardError::Store)?;
        let Some(row) = rows.next().map_err(GuardError::Store)? else {
            return Ok(None);
        };
        let seq = row.get::<_, i64>(0).map_err(GuardError::Store)?;
        read_metrics::observe_row();
        let (fact, _) = decode_bounded_fact(row, seq, 1, 2, 3)?;
        match fact.fact.kind() {
            wire::FactKind::LaneHeld => {
                let held = fact.fact.read::<wire::body::LaneHeld>().ok_or_else(|| {
                    GuardError::Invalid(format!("lane hold fact {} has another body", fact.seq))
                })?;
                if held.lane != lane {
                    return Err(GuardError::Invalid(format!(
                        "lane hold fact {} names another lane",
                        fact.seq
                    )));
                }
                Ok(Some(held))
            }
            wire::FactKind::LaneResumed => {
                let resumed = fact.fact.read::<wire::body::LaneResumed>().ok_or_else(|| {
                    GuardError::Invalid(format!("lane resume fact {} has another body", fact.seq))
                })?;
                if resumed.lane != lane {
                    return Err(GuardError::Invalid(format!(
                        "lane resume fact {} names another lane",
                        fact.seq
                    )));
                }
                Ok(None)
            }
            _ => Err(GuardError::Invalid(format!(
                "lane hold index at {} returned another fact kind",
                fact.seq
            ))),
        }
    }
}
