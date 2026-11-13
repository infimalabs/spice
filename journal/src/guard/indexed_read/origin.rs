//! Bounded newest-first facts within one causal origin.

use rusqlite::{params_from_iter, types::Value};

use super::{
    FactReadBudget, GuardError, JournalRead, SelectedFactPage, admitted_envelope_length,
    decode_bounded_fact, read_metrics,
};

impl JournalRead<'_> {
    /// Read one newest-first page for a fact kind inside one causal origin.
    pub fn recent_origin_facts(
        &self,
        origin: &wire::Origin,
        kind: wire::FactKind,
        before: Option<wire::Seq>,
        budget: FactReadBudget,
    ) -> Result<SelectedFactPage, GuardError> {
        let Some(horizon) = self.cursor else {
            return Ok(SelectedFactPage {
                rows: Vec::new(),
                continuation: None,
                horizon: None,
            });
        };
        let horizon_sql = i64::try_from(horizon.get())
            .map_err(|_| GuardError::Invalid("recent origin horizon exceeds SQLite".to_owned()))?;
        let query_limit = budget
            .rows()
            .checked_add(1)
            .expect("repository fact row ceiling leaves room for an overflow probe");
        let query_limit = i64::try_from(query_limit)
            .map_err(|_| GuardError::Invalid("recent origin limit exceeds SQLite".to_owned()))?;
        let mut sql = String::from(
            "SELECT seq, kind, length(envelope), envelope FROM fact_envelopes \
             WHERE origin = ?1 AND kind = ?2 AND seq <= ?3",
        );
        let mut parameters = vec![
            Value::Text(origin.as_str().to_owned()),
            Value::Text(kind.as_str().to_owned()),
            Value::Integer(horizon_sql),
        ];
        if let Some(before) = before {
            let before = i64::try_from(before.get()).map_err(|_| {
                GuardError::Invalid("recent origin cursor exceeds SQLite".to_owned())
            })?;
            sql.push_str(" AND seq < ?4 ORDER BY seq DESC LIMIT ?5");
            parameters.push(Value::Integer(before));
            parameters.push(Value::Integer(query_limit));
        } else {
            sql.push_str(" ORDER BY seq DESC LIMIT ?4");
            parameters.push(Value::Integer(query_limit));
        }
        read_metrics::observe_query();
        let mut statement = self.connection.prepare(&sql).map_err(GuardError::Store)?;
        let mut rows = statement
            .query(params_from_iter(&parameters))
            .map_err(GuardError::Store)?;
        let mut facts = Vec::new();
        let mut decoded_bytes = 0usize;
        let mut continuation = None;
        while let Some(row) = rows.next().map_err(GuardError::Store)? {
            let seq = row.get::<_, i64>(0).map_err(GuardError::Store)?;
            read_metrics::observe_row();
            if facts.len() == budget.rows() {
                continuation = facts
                    .last()
                    .map(|fact: &wire::Envelope<wire::AnyFact>| fact.seq);
                break;
            }
            let length =
                admitted_envelope_length(seq, row.get::<_, i64>(2).map_err(GuardError::Store)?)?;
            if !facts.is_empty() && decoded_bytes.saturating_add(length) > budget.decoded_bytes() {
                continuation = facts
                    .last()
                    .map(|fact: &wire::Envelope<wire::AnyFact>| fact.seq);
                break;
            }
            let (fact, length) = decode_bounded_fact(row, seq, 1, 2, 3)?;
            decoded_bytes = decoded_bytes.saturating_add(length);
            facts.push(fact);
        }
        Ok(SelectedFactPage {
            rows: facts,
            continuation,
            horizon: Some(horizon),
        })
    }
}
