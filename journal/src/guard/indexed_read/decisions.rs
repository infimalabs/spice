//! Bounded newest-first pages of one controller's operator decisions.

use rusqlite::params;

use super::{
    FactReadBudget, GuardError, JournalRead, SelectedFactPage, admitted_envelope_length,
    decode_bounded_fact, read_metrics,
};

impl JournalRead<'_> {
    /// One bounded page of `controller`'s recorded `proposal::decided` facts
    /// strictly before `before` (or through the current horizon without one),
    /// newest first. Its continuation is the oldest returned position, so the
    /// next page resumes strictly before it; the work is bounded by the page,
    /// never by the controller's whole decision history.
    pub fn newest_preparation_decisions(
        &self,
        controller: &wire::TaskStamp,
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
        let invalid =
            || GuardError::Invalid("preparation decision cursor exceeds SQLite".to_owned());
        let horizon_sql = i64::try_from(horizon.get()).map_err(|_| invalid())?;
        let high = match before {
            Some(before) => i64::try_from(before.get())
                .map_err(|_| invalid())?
                .saturating_sub(1)
                .min(horizon_sql),
            None => horizon_sql,
        };
        let probe = budget
            .rows()
            .checked_add(1)
            .and_then(|limit| i64::try_from(limit).ok())
            .ok_or_else(invalid)?;
        read_metrics::observe_query();
        let mut statement = self
            .connection
            .prepare(
                "SELECT facts.seq, facts.kind, length(facts.envelope), facts.envelope \
                 FROM proposal_heads AS heads \
                 JOIN fact_envelopes AS facts ON facts.seq = heads.head_seq \
                 WHERE heads.controller = ?1 AND facts.kind = ?2 \
                 AND facts.seq > 0 AND facts.seq <= ?3 \
                 ORDER BY heads.head_seq DESC LIMIT ?4",
            )
            .map_err(GuardError::Store)?;
        let mut rows = statement
            .query(params![
                controller.as_str(),
                wire::FactKind::PreparationDecided.as_str(),
                high,
                probe
            ])
            .map_err(GuardError::Store)?;
        let mut facts: Vec<wire::Envelope<wire::AnyFact>> = Vec::new();
        let mut decoded = 0usize;
        let mut continuation = None;
        while let Some(row) = rows.next().map_err(GuardError::Store)? {
            let seq = row.get::<_, i64>(0).map_err(GuardError::Store)?;
            read_metrics::observe_row();
            let length =
                admitted_envelope_length(seq, row.get::<_, i64>(2).map_err(GuardError::Store)?)?;
            if facts.len() == budget.rows()
                || (!facts.is_empty() && decoded.saturating_add(length) > budget.decoded_bytes())
            {
                continuation = facts.last().map(|fact| fact.seq);
                break;
            }
            let (fact, length) = decode_bounded_fact(row, seq, 1, 2, 3)?;
            if fact
                .fact
                .read::<wire::rooms::PreparationDecided>()
                .is_none_or(|decided| decided.decision.decision.controller != *controller)
            {
                return Err(GuardError::Invalid(format!(
                    "decision index for {controller} names another fact {seq}"
                )));
            }
            decoded = decoded.saturating_add(length);
            facts.push(fact);
        }
        Ok(SelectedFactPage {
            rows: facts,
            continuation,
            horizon: Some(horizon),
        })
    }
}
