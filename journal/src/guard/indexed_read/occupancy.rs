//! The newest lane occupancy facts through one horizon.

use rusqlite::{params_from_iter, types::Value};

use super::{
    FactReadBudget, GuardError, JournalRead, admitted_envelope_length, decode_bounded_fact,
    read_metrics,
};

impl JournalRead<'_> {
    /// The newest [`wire::FactKind::LANE_OCCUPANCY`] facts at or before
    /// `horizon`, oldest first. The budget keeps the newest: older occupancy
    /// is dropped, never continued, because a reader folds intervals ending
    /// at the horizon and wants the most recent of them.
    pub fn recent_lane_occupancy(
        &self,
        horizon: wire::Seq,
        budget: FactReadBudget,
    ) -> Result<wire::Packed<wire::Envelope<wire::AnyFact>>, GuardError> {
        let Some(cursor) = self.cursor else {
            return Err(GuardError::Invalid(
                "lane occupancy read requires a durable cursor".to_owned(),
            ));
        };
        if horizon > cursor {
            return Err(GuardError::Invalid(format!(
                "lane occupancy horizon {horizon} is beyond cursor {cursor}"
            )));
        }
        let horizon_sql = i64::try_from(horizon.get())
            .map_err(|_| GuardError::Invalid("lane occupancy horizon exceeds SQLite".to_owned()))?;
        let limit = i64::try_from(budget.rows())
            .map_err(|_| GuardError::Invalid("lane occupancy limit exceeds SQLite".to_owned()))?;
        let kinds = wire::FactKind::LANE_OCCUPANCY;
        let placeholders = (1..=kinds.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT seq, kind, length(envelope), envelope FROM fact_envelopes \
             WHERE kind IN ({placeholders}) AND seq <= ?{} ORDER BY seq DESC LIMIT ?{}",
            kinds.len() + 1,
            kinds.len() + 2,
        );
        let parameters = kinds
            .iter()
            .map(|kind| Value::Text(kind.as_str().to_owned()))
            .chain([Value::Integer(horizon_sql), Value::Integer(limit)])
            .collect::<Vec<_>>();
        read_metrics::observe_query();
        let mut statement = self.connection.prepare(&sql).map_err(GuardError::Store)?;
        let mut rows = statement
            .query(params_from_iter(&parameters))
            .map_err(GuardError::Store)?;
        let mut facts = Vec::new();
        let mut decoded_bytes = 0usize;
        while let Some(row) = rows.next().map_err(GuardError::Store)? {
            let seq = row.get::<_, i64>(0).map_err(GuardError::Store)?;
            read_metrics::observe_row();
            let length =
                admitted_envelope_length(seq, row.get::<_, i64>(2).map_err(GuardError::Store)?)?;
            if !facts.is_empty() && decoded_bytes.saturating_add(length) > budget.decoded_bytes() {
                break;
            }
            let (fact, length) = decode_bounded_fact(row, seq, 1, 2, 3)?;
            decoded_bytes = decoded_bytes.saturating_add(length);
            facts.push(fact);
        }
        facts.reverse();
        Ok(wire::Packed {
            rows: facts,
            overflow: None,
            horizon,
        })
    }
}
