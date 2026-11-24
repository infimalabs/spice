//! Indexed reads exposed by one immutable journal transaction.

use rusqlite::{OptionalExtension, Row, params, params_from_iter, types::Value};
use serde::Serialize;

use super::commit::decode_seq;
use super::{
    ClaimHeads, FactPageStart, FactReadBudget, GuardError, JournalHead, JournalRead,
    SelectedFactPage, TaskHeads, claim_disposition_name, read_metrics,
};
use wire::body::SourceBoundary;

mod bounds;
mod checkpoint;
mod decisions;
mod head_page;
mod hold;
mod occupancy;
mod origin;
mod peer_line;
mod principal;
mod rooms;
mod taste;
mod thread;

use bounds::{admitted_envelope_length, decode_bounded_fact};
pub use checkpoint::LaneCheckpointHead;
#[cfg(test)]
pub(crate) use head_page::verify_task_page_plan;
pub use head_page::{CurrentHeadWitness, TaskHeadPage, TrustHeadPage, TrustReportHeadPage};
pub use taste::{TasteEvidenceFact, TasteEvidencePage, TasteEvidenceSummary};
pub use thread::ThreadError;

struct CausalPage {
    facts: Vec<wire::Envelope<wire::AnyFact>>,
    total: Option<u64>,
    origin: Option<wire::Origin>,
    decoded_bytes: usize,
    overflow: Option<wire::Overflow>,
}

impl CausalPage {
    fn new(capacity: usize) -> Self {
        Self {
            facts: Vec::with_capacity(capacity),
            total: None,
            origin: None,
            decoded_bytes: 0,
            overflow: None,
        }
    }

    fn push(&mut self, row: &Row<'_>, budget: FactReadBudget) -> Result<bool, GuardError> {
        let seq = row.get::<_, i64>(0).map_err(GuardError::Store)?;
        let remaining = row.get::<_, i64>(3).map_err(GuardError::Store)?;
        read_metrics::observe_row();
        let length =
            admitted_envelope_length(seq, row.get::<_, i64>(5).map_err(GuardError::Store)?)?;
        if self.facts.len() == budget.rows()
            || (!self.facts.is_empty()
                && self.decoded_bytes.saturating_add(length) > budget.decoded_bytes())
        {
            let continuation = u64::try_from(seq)
                .ok()
                .and_then(wire::Seq::new)
                .ok_or_else(|| {
                    GuardError::Invalid(format!(
                        "thread index sequence {seq} is outside the wire range"
                    ))
                })?;
            let omitted = u64::try_from(remaining)
                .map_err(|_| {
                    GuardError::Invalid(format!(
                        "thread index depth {remaining} is outside the wire range"
                    ))
                })?
                .saturating_add(1);
            self.overflow = Some(wire::Overflow {
                omitted,
                continuation,
            });
            return Ok(false);
        }
        let origin = row.get::<_, String>(1).map_err(GuardError::Store)?;
        let cause = row.get::<_, Option<i64>>(2).map_err(GuardError::Store)?;
        let (fact, length) = decode_bounded_fact(row, seq, 4, 5, 6)?;
        self.decoded_bytes = self.decoded_bytes.saturating_add(length);
        let indexed_cause = cause
            .map(|cause| {
                u64::try_from(cause)
                    .ok()
                    .and_then(wire::Seq::new)
                    .ok_or_else(|| {
                        GuardError::Invalid(format!(
                            "thread index cause {cause} is outside the wire range"
                        ))
                    })
            })
            .transpose()?;
        if fact.origin.as_str() != origin || fact.cause != indexed_cause {
            return Err(GuardError::Invalid(format!(
                "thread index at {} disagrees with its canonical envelope",
                fact.seq
            )));
        }
        if self
            .origin
            .as_ref()
            .is_some_and(|chain_origin| chain_origin != &fact.origin)
        {
            return Err(GuardError::Invalid(format!(
                "thread index at {} crosses causal origins",
                fact.seq
            )));
        }
        self.origin.get_or_insert_with(|| fact.origin.clone());
        let remaining = u64::try_from(remaining).map_err(|_| {
            GuardError::Invalid(format!(
                "thread index depth {remaining} is outside the wire range"
            ))
        })?;
        if let Some(total) = self.total {
            let step = u64::try_from(self.facts.len()).unwrap_or(u64::MAX);
            let expected = total.saturating_sub(step.saturating_add(1));
            if remaining != expected {
                return Err(GuardError::Invalid(format!(
                    "thread index at {} has depth {remaining}, expected {expected}",
                    fact.seq
                )));
            }
        } else {
            self.total = Some(remaining.saturating_add(1));
        }
        self.facts.push(fact);
        Ok(true)
    }

    fn finish(
        self,
        limit: usize,
        horizon: wire::Seq,
    ) -> Result<Option<wire::Packed<wire::Envelope<wire::AnyFact>>>, GuardError> {
        let Some(total) = self.total else {
            return Ok(None);
        };
        let expected = usize::try_from(total).unwrap_or(usize::MAX).min(limit);
        if self.overflow.is_none() && self.facts.len() != expected {
            return Err(GuardError::Invalid(format!(
                "indexed causal range expected {expected} row(s), found {}",
                self.facts.len()
            )));
        }
        Ok(Some(wire::Packed {
            rows: self.facts,
            overflow: self.overflow,
            horizon,
        }))
    }
}

/// Retained claims grouped by task, in stamp order.
pub(super) type ClaimsByTask = Vec<(wire::TaskStamp, Vec<ClaimHeads>)>;

impl JournalRead<'_> {
    /// Greatest current task-statement, workflow or topic-metadata head position.
    ///
    /// This is the compact movement witness for the board document projection:
    /// claim, lease, and trust head movement cannot change it.
    pub fn task_projection_cursor(&self) -> Result<Option<wire::Seq>, GuardError> {
        read_metrics::observe_query();
        let position = self
            .connection
            .query_row(
                "SELECT NULLIF(max(\
                    coalesce((SELECT max(head_seq) FROM task_heads), 0), \
                    coalesce((SELECT max(head_seq) FROM workflow_heads), 0), \
                    coalesce((SELECT max(head_seq) FROM topic_heads), 0)\
                 ), 0)",
                [],
                |row| row.get::<_, Option<i64>>(0),
            )
            .map_err(GuardError::Store)?;
        read_metrics::observe_row();
        position
            .map(|position| decode_seq(position).map_err(GuardError::Invalid))
            .transpose()
    }

    /// Observe only the current trust heads named by one canonical requirement set.
    pub fn environment(
        &self,
        requirements: &wire::EnvironmentRequirements,
    ) -> Result<wire::Environment, GuardError> {
        let mut heads = Vec::new();
        for requirement in requirements.as_slice() {
            if let wire::EnvironmentRequirement::TrustGrant { value } = requirement
                && let Some(head) = self.trust_head(value)?
            {
                heads.push(head.into_body());
            }
        }
        Ok(wire::Environment::journal_schema(crate::SCHEMA_VERSION)
            .expect("the compiled journal schema is positive")
            .with_trust_heads(heads))
    }

    /// Last durable fact position visible to this transaction.
    pub fn cursor(&self) -> Result<Option<wire::Seq>, GuardError> {
        Ok(self.cursor)
    }

    /// First durable fact position: 1 for a journal created before the
    /// four-character boundary, 52³ (`1000`) for one created since.
    pub fn first_position(&self) -> Result<Option<wire::Seq>, GuardError> {
        if self.cursor.is_none() {
            return Ok(None);
        }
        read_metrics::observe_query();
        // Facts are never removed and every committed position precedes the
        // cursor, so the first rowid in key order is the first visible fact:
        // one B-tree descent, never a scan.
        let position = self
            .connection
            .query_row(
                "SELECT seq FROM fact_envelopes ORDER BY seq LIMIT 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(GuardError::Store)?;
        read_metrics::observe_row();
        position
            .map(|position| decode_seq(position).map_err(GuardError::Invalid))
            .transpose()
    }

    /// Read one bounded page of the named fact kinds after an admitted cursor.
    ///
    /// `upper` freezes a historical prefix; `None` uses this guard's cursor.
    /// An unavailable upper bound or an `after` beyond it is refused. The
    /// `(kind, seq)` index preserves original positions and order, and the
    /// continuation resumes strictly after the last returned position.
    pub fn selected_fact_page(
        &self,
        kinds: &[wire::FactKind],
        after: Option<wire::Seq>,
        upper: Option<wire::Seq>,
        budget: FactReadBudget,
    ) -> Result<SelectedFactPage, GuardError> {
        if upper.is_some_and(|upper| self.cursor.is_none_or(|cursor| upper > cursor)) {
            return Err(GuardError::Invalid(format!(
                "selected fact upper bound {upper:?} is beyond journal cursor {:?}",
                self.cursor
            )));
        }
        let Some(horizon) = upper.or(self.cursor) else {
            return Ok(SelectedFactPage {
                rows: Vec::new(),
                continuation: None,
                horizon: None,
            });
        };
        if after.is_some_and(|after| after > horizon) {
            return Err(GuardError::Invalid(format!(
                "selected fact cursor {after:?} is beyond selected horizon {horizon}"
            )));
        }
        if kinds.is_empty() || after == Some(horizon) {
            return Ok(SelectedFactPage {
                rows: Vec::new(),
                continuation: None,
                horizon: Some(horizon),
            });
        }
        let placeholders = (3..kinds.len() + 3)
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let limit_parameter = kinds.len() + 3;
        let sql = format!(
            "SELECT seq, kind, length(envelope), envelope FROM fact_envelopes \
             WHERE seq > ?1 AND seq <= ?2 AND kind IN ({placeholders}) \
             ORDER BY seq LIMIT ?{limit_parameter}"
        );
        let mut parameters = Vec::with_capacity(kinds.len() + 3);
        let after = after
            .map(|seq| {
                i64::try_from(seq.get()).map_err(|_| {
                    GuardError::Invalid("selected fact cursor exceeds SQLite".to_owned())
                })
            })
            .transpose()?
            .unwrap_or(0);
        parameters.push(Value::Integer(after));
        parameters.push(Value::Integer(i64::try_from(horizon.get()).map_err(
            |_| GuardError::Invalid("selected fact horizon exceeds SQLite".to_owned()),
        )?));
        parameters.extend(
            kinds
                .iter()
                .map(|kind| Value::Text(kind.as_str().to_owned())),
        );
        let query_limit = budget
            .rows()
            .checked_add(1)
            .expect("repository fact row ceiling leaves room for an overflow probe");
        parameters.push(Value::Integer(i64::try_from(query_limit).map_err(
            |_| GuardError::Invalid("selected fact page limit exceeds SQLite".to_owned()),
        )?));
        read_metrics::observe_query();
        let mut statement = self.connection.prepare(&sql).map_err(GuardError::Store)?;
        let mut rows = statement
            .query(params_from_iter(&parameters))
            .map_err(GuardError::Store)?;
        let mut facts: Vec<wire::Envelope<wire::AnyFact>> = Vec::with_capacity(budget.rows());
        let mut decoded_bytes = 0usize;
        let mut continuation = None;
        while let Some(row) = rows.next().map_err(GuardError::Store)? {
            let seq = row.get::<_, i64>(0).map_err(GuardError::Store)?;
            read_metrics::observe_row();
            if facts.len() == budget.rows() {
                continuation = facts.last().map(|fact| fact.seq);
                break;
            }
            let length =
                admitted_envelope_length(seq, row.get::<_, i64>(2).map_err(GuardError::Store)?)?;
            if !facts.is_empty() && decoded_bytes.saturating_add(length) > budget.decoded_bytes() {
                continuation = facts.last().map(|fact| fact.seq);
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

    /// Read one bounded newest-first page of a retained fact kind.
    ///
    /// The continuation is the oldest returned cursor, so the next call passes
    /// it as `before` and resumes strictly before it.
    pub fn recent_fact_page(
        &self,
        kind: wire::FactKind,
        before: Option<wire::Seq>,
        budget: FactReadBudget,
    ) -> Result<SelectedFactPage, GuardError> {
        self.recent_page(Some(kind), before, budget)
    }

    /// Read one bounded newest-first page of every fact kind, by primary key.
    ///
    /// The continuation is the oldest returned cursor, so the next call passes
    /// it as `before` and resumes strictly before it.
    pub fn recent_facts(
        &self,
        before: Option<wire::Seq>,
        budget: FactReadBudget,
    ) -> Result<SelectedFactPage, GuardError> {
        self.recent_page(None, before, budget)
    }

    fn recent_page(
        &self,
        kind: Option<wire::FactKind>,
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
        if before.is_some_and(|before| before > horizon) {
            return Err(GuardError::Invalid(format!(
                "recent fact cursor {before:?} is beyond journal cursor {horizon}"
            )));
        }
        let query_limit = budget
            .rows()
            .checked_add(1)
            .expect("repository fact row ceiling leaves room for an overflow probe");
        let horizon_sql = i64::try_from(horizon.get())
            .map_err(|_| GuardError::Invalid("recent fact horizon exceeds SQLite".to_owned()))?;
        let query_limit = i64::try_from(query_limit)
            .map_err(|_| GuardError::Invalid("recent fact page limit exceeds SQLite".to_owned()))?;
        let mut sql = String::from(
            "SELECT seq, kind, length(envelope), envelope FROM fact_envelopes WHERE seq <= ?",
        );
        let mut parameters = vec![Value::Integer(horizon_sql)];
        if let Some(kind) = kind {
            sql.push_str(" AND kind = ?");
            parameters.push(Value::Text(kind.as_str().to_owned()));
        }
        if let Some(before) = before {
            let before = i64::try_from(before.get())
                .map_err(|_| GuardError::Invalid("recent fact cursor exceeds SQLite".to_owned()))?;
            sql.push_str(" AND seq < ?");
            parameters.push(Value::Integer(before));
        }
        sql.push_str(" ORDER BY seq DESC LIMIT ?");
        parameters.push(Value::Integer(query_limit));
        read_metrics::observe_query();
        let mut statement = self.connection.prepare(&sql).map_err(GuardError::Store)?;
        let mut rows = statement
            .query(params_from_iter(&parameters))
            .map_err(GuardError::Store)?;
        let mut facts: Vec<wire::Envelope<wire::AnyFact>> = Vec::with_capacity(budget.rows());
        let mut decoded_bytes = 0usize;
        let mut continuation = None;
        while let Some(row) = rows.next().map_err(GuardError::Store)? {
            let seq = row.get::<_, i64>(0).map_err(GuardError::Store)?;
            read_metrics::observe_row();
            if facts.len() == budget.rows() {
                continuation = facts.last().map(|fact| fact.seq);
                break;
            }
            let length =
                admitted_envelope_length(seq, row.get::<_, i64>(2).map_err(GuardError::Store)?)?;
            if !facts.is_empty() && decoded_bytes.saturating_add(length) > budget.decoded_bytes() {
                continuation = facts.last().map(|fact| fact.seq);
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

    /// Read the newest fact of one kind through the immutable horizon.
    pub fn latest_fact(
        &self,
        kind: wire::FactKind,
    ) -> Result<Option<wire::Envelope<wire::AnyFact>>, GuardError> {
        let Some(horizon) = self.cursor else {
            return Ok(None);
        };
        let horizon = i64::try_from(horizon.get())
            .map_err(|_| GuardError::Invalid("latest fact horizon exceeds SQLite".to_owned()))?;
        read_metrics::observe_query();
        let mut statement = self
            .connection
            .prepare(
                "SELECT seq, kind, length(envelope), envelope FROM fact_envelopes \
                 WHERE kind = ?1 AND seq <= ?2 ORDER BY seq DESC LIMIT 1",
            )
            .map_err(GuardError::Store)?;
        let mut rows = statement
            .query(params![kind.as_str(), horizon])
            .map_err(GuardError::Store)?;
        let Some(row) = rows.next().map_err(GuardError::Store)? else {
            return Ok(None);
        };
        let seq = row.get::<_, i64>(0).map_err(GuardError::Store)?;
        read_metrics::observe_row();
        decode_bounded_fact(row, seq, 1, 2, 3).map(|(fact, _)| Some(fact))
    }

    /// Read one bounded primary-key fact range through an immutable horizon.
    pub fn fact_page(
        &self,
        start: FactPageStart,
        horizon: wire::Seq,
        budget: FactReadBudget,
    ) -> Result<wire::Packed<wire::Envelope<wire::AnyFact>>, GuardError> {
        let limit = budget.rows();
        let Some(cursor) = self.cursor else {
            return Err(GuardError::Invalid(
                "indexed fact page requires a durable cursor".to_owned(),
            ));
        };
        if horizon > cursor {
            return Err(GuardError::Invalid(format!(
                "indexed fact page horizon {horizon} is beyond cursor {cursor}"
            )));
        }
        let first = match start {
            FactPageStart::After(None) => 1,
            FactPageStart::After(Some(after)) => after.get().saturating_add(1),
            FactPageStart::At(position) => position.get(),
        };
        if first > horizon.get() {
            return Ok(wire::Packed {
                rows: Vec::new(),
                overflow: None,
                horizon,
            });
        }
        let total = horizon.get() - first + 1;
        let query_limit = limit.saturating_add(1);
        let first = i64::try_from(first)
            .map_err(|_| GuardError::Invalid("indexed fact start exceeds SQLite".to_owned()))?;
        let horizon_sql = i64::try_from(horizon.get())
            .map_err(|_| GuardError::Invalid("indexed fact horizon exceeds SQLite".to_owned()))?;
        let query_limit = i64::try_from(query_limit)
            .map_err(|_| GuardError::Invalid("indexed fact limit exceeds SQLite".to_owned()))?;
        read_metrics::observe_query();
        let mut statement = self
            .connection
            .prepare(
                "SELECT seq, kind, length(envelope), envelope FROM fact_envelopes \
                 WHERE seq >= ?1 AND seq <= ?2 ORDER BY seq LIMIT ?3",
            )
            .map_err(GuardError::Store)?;
        let mut rows = statement
            .query(params![first, horizon_sql, query_limit])
            .map_err(GuardError::Store)?;
        let mut facts = Vec::with_capacity(limit.min(usize::try_from(total).unwrap_or(usize::MAX)));
        let mut decoded_bytes = 0usize;
        let mut continuation = None;
        while let Some(row) = rows.next().map_err(GuardError::Store)? {
            let seq = row.get::<_, i64>(0).map_err(GuardError::Store)?;
            read_metrics::observe_row();
            let length =
                admitted_envelope_length(seq, row.get::<_, i64>(2).map_err(GuardError::Store)?)?;
            if facts.len() == limit
                || (!facts.is_empty()
                    && decoded_bytes.saturating_add(length) > budget.decoded_bytes())
            {
                continuation = u64::try_from(seq).ok().and_then(wire::Seq::new);
                break;
            }
            let (fact, length) = decode_bounded_fact(row, seq, 1, 2, 3)?;
            decoded_bytes = decoded_bytes.saturating_add(length);
            facts.push(fact);
        }
        if continuation.is_none() && u64::try_from(facts.len()).unwrap_or(u64::MAX) != total {
            return Err(GuardError::Invalid(format!(
                "indexed fact range expected {total} row(s), found {}",
                facts.len()
            )));
        }
        let overflow = continuation.map(|continuation| wire::Overflow {
            omitted: horizon
                .get()
                .saturating_sub(continuation.get())
                .saturating_add(1),
            continuation,
        });
        Ok(wire::Packed {
            rows: facts,
            overflow,
            horizon,
        })
    }

    /// Walk one bounded causal page through the durable thread index.
    pub fn causal_page(
        &self,
        position: wire::Seq,
        budget: FactReadBudget,
    ) -> Result<Option<wire::Packed<wire::Envelope<wire::AnyFact>>>, GuardError> {
        let limit = budget.rows();
        let Some(horizon) = self.cursor else {
            return Ok(None);
        };
        let position_sql = i64::try_from(position.get())
            .map_err(|_| GuardError::Invalid("causal position exceeds SQLite".to_owned()))?;
        let query_limit = limit.saturating_add(1);
        let query_limit_sql = i64::try_from(query_limit)
            .map_err(|_| GuardError::Invalid("causal page limit exceeds SQLite".to_owned()))?;
        read_metrics::observe_query();
        let mut statement = self
            .connection
            .prepare(
                "WITH RECURSIVE chain(seq, origin, cause, remaining, step) AS (\
                     SELECT seq, origin, cause, depth, 0 FROM thread_index WHERE seq = ?1 \
                     UNION ALL \
                     SELECT indexed.seq, indexed.origin, indexed.cause, indexed.depth, chain.step + 1 \
                     FROM thread_index AS indexed JOIN chain ON indexed.seq = chain.cause \
                     WHERE chain.step + 1 < ?2\
                 ) \
                 SELECT chain.seq, chain.origin, chain.cause, chain.remaining, \
                        fact.kind, length(fact.envelope), fact.envelope \
                 FROM chain JOIN fact_envelopes AS fact ON fact.seq = chain.seq \
                 ORDER BY chain.step LIMIT ?2",
            )
            .map_err(GuardError::Store)?;
        let mut rows = statement
            .query(params![position_sql, query_limit_sql])
            .map_err(GuardError::Store)?;
        let mut page = CausalPage::new(query_limit);
        while let Some(row) = rows.next().map_err(GuardError::Store)? {
            if !page.push(row, budget)? {
                break;
            }
        }
        page.finish(limit, horizon)
    }

    /// Current storage bundle for one exact task birth stamp.
    pub fn task_heads_for(&self, stamp: &wire::TaskStamp) -> Result<Option<TaskHeads>, GuardError> {
        let statement = self.task_head(stamp)?;
        let workflow = self.workflow_head(stamp)?;
        let completion = self.completion_head(stamp)?;
        let claims = self.task_claims(stamp)?;
        if statement.is_none() && workflow.is_none() && completion.is_none() && claims.is_empty() {
            return Ok(None);
        }
        let loose_planes = [
            workflow.as_ref().map(|_| "workflow"),
            completion.as_ref().map(|_| "completion"),
            (!claims.is_empty()).then_some("claim"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        let statement = statement.ok_or_else(|| {
            GuardError::Invalid(format!(
                "task {stamp} has loose {} head(s) without a statement head",
                loose_planes.join(", ")
            ))
        })?;
        let workflow = workflow.ok_or_else(|| {
            GuardError::Invalid(format!(
                "task {stamp} has a statement head but no workflow head"
            ))
        })?;
        let completion = completion.ok_or_else(|| {
            GuardError::Invalid(format!(
                "task {stamp} has a statement head but no completion head"
            ))
        })?;
        Ok(Some(TaskHeads {
            statement,
            workflow,
            completion,
            claims,
        }))
    }

    /// Task bundles whose indexed heads changed inside one committed range,
    /// including workflow bodies revised in place without a transition.
    pub fn changed_task_heads(
        &self,
        first: wire::Seq,
        last: wire::Seq,
    ) -> Result<Vec<TaskHeads>, GuardError> {
        let Some(cursor) = self.cursor else {
            return Err(GuardError::Invalid(
                "changed task heads require a durable cursor".to_owned(),
            ));
        };
        if first > last || last > cursor {
            return Err(GuardError::Invalid(format!(
                "changed task head range {first}..={last} is outside cursor {cursor}"
            )));
        }
        let first = i64::try_from(first.get())
            .map_err(|_| GuardError::Invalid("task head range exceeds SQLite".to_owned()))?;
        let last = i64::try_from(last.get())
            .map_err(|_| GuardError::Invalid("task head range exceeds SQLite".to_owned()))?;
        read_metrics::observe_query();
        let mut statement = self
            .connection
            .prepare(
                "SELECT DISTINCT stamp FROM (\
                    SELECT stamp, head_seq FROM task_heads UNION ALL \
                    SELECT stamp, head_seq FROM workflow_heads UNION ALL \
                    SELECT stamp, revised_seq FROM workflow_heads \
                        WHERE revised_seq IS NOT NULL UNION ALL \
                    SELECT stamp, head_seq FROM completion_heads UNION ALL \
                    SELECT stamp, head_seq FROM claim_heads UNION ALL \
                    SELECT stamp, head_seq FROM lease_heads\
                 ) WHERE head_seq >= ?1 AND head_seq <= ?2 ORDER BY stamp",
            )
            .map_err(GuardError::Store)?;
        let mut stamps = statement
            .query_map(params![first, last], |row| row.get::<_, String>(0))
            .map_err(GuardError::Store)?
            .collect::<Result<std::collections::BTreeSet<_>, _>>()
            .map_err(GuardError::Store)?;
        stamps.extend(self.tasks_under_topics_changed(first, last)?);
        let mut bundles = Vec::new();
        for indexed in stamps {
            let stamp = indexed.parse::<wire::TaskStamp>().map_err(|error| {
                GuardError::Invalid(format!("task head stamp {indexed:?} is invalid: {error}"))
            })?;
            bundles.push(
                self.task_heads_for(&stamp)?
                    .expect("a changed head range names a current task bundle"),
            );
        }
        Ok(bundles)
    }

    /// Tasks whose rendered topic moved because a node at or above theirs was
    /// created, renamed, or moved inside the range.
    fn tasks_under_topics_changed(&self, first: i64, last: i64) -> Result<Vec<String>, GuardError> {
        read_metrics::observe_query();
        let changed = self
            .connection
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM topic_heads WHERE head_seq >= ?1 AND head_seq <= ?2)",
                params![first, last],
                |row| row.get::<_, bool>(0),
            )
            .map_err(GuardError::Store)?;
        if !changed {
            return Ok(Vec::new());
        }
        read_metrics::observe_query();
        let mut statement = self
            .connection
            .prepare(
                "WITH RECURSIVE moved(node) AS (\
                    SELECT node FROM topic_heads WHERE head_seq >= ?1 AND head_seq <= ?2 \
                    UNION SELECT topic_heads.node FROM topic_heads \
                        JOIN moved ON topic_heads.parent = moved.node\
                 ) SELECT stamp FROM task_heads \
                 WHERE json_extract(CAST(body AS TEXT), '$.topic.node') IN moved \
                    OR json_extract(CAST(body AS TEXT), '$.topic.placed') IN moved",
            )
            .map_err(GuardError::Store)?;
        statement
            .query_map(params![first, last], |row| row.get::<_, String>(0))
            .map_err(GuardError::Store)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(GuardError::Store)
    }

    /// Up to `limit` of one task's recorded advances past `after`, in
    /// generation order; a caller resumes from the last generation returned.
    pub fn task_generations_after(
        &self,
        stamp: &wire::TaskStamp,
        after: u64,
        limit: usize,
    ) -> Result<Vec<super::TaskGeneration>, GuardError> {
        let invalid = || GuardError::Invalid("task generation read exceeds SQLite".to_owned());
        let after = i64::try_from(after).map_err(|_| invalid())?;
        let limit = i64::try_from(limit).map_err(|_| invalid())?;
        read_metrics::observe_query();
        let mut statement = self
            .connection
            .prepare(
                "SELECT generation, seq FROM task_generations \
                 WHERE stamp = ?1 AND generation > ?2 ORDER BY generation LIMIT ?3",
            )
            .map_err(GuardError::Store)?;
        let rows = statement
            .query_map(params![stamp.as_str(), after, limit], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(GuardError::Store)?;
        let mut advances = Vec::new();
        for row in rows {
            let (generation, position) = row.map_err(GuardError::Store)?;
            let generation = u64::try_from(generation).map_err(|_| {
                GuardError::Invalid(format!("task {stamp} generation {generation} is negative"))
            })?;
            let position = u64::try_from(position)
                .ok()
                .and_then(wire::Seq::new)
                .ok_or_else(|| {
                    GuardError::Invalid(format!(
                        "task {stamp} generation {generation} names invalid position {position}"
                    ))
                })?;
            advances.push(super::TaskGeneration {
                generation,
                position,
            });
        }
        Ok(advances)
    }

    /// Current task statement head, when the birth stamp exists.
    pub fn task_head(
        &self,
        stamp: &wire::TaskStamp,
    ) -> Result<Option<JournalHead<wire::TaskHead>>, GuardError> {
        let head =
            self.text_head_with("task_heads", "stamp", stamp.as_str(), |position, bytes| {
                decode_task_head(self.connection, position, bytes)
            })?;
        if let Some(head) = &head
            && head.body.born != *stamp
        {
            return Err(GuardError::Invalid(format!(
                "task head indexed as {stamp} carries body stamp {}",
                head.body.born
            )));
        }
        if let Some(head) = &head
            && head.body.revision != head.position
        {
            return Err(GuardError::Invalid(format!(
                "task {stamp} statement head revision disagrees with head_seq"
            )));
        }
        Ok(head)
    }

    /// Current workflow head, when the birth stamp exists.
    pub fn workflow_head(
        &self,
        stamp: &wire::TaskStamp,
    ) -> Result<Option<JournalHead<wire::WorkflowHead>>, GuardError> {
        self.text_head("workflow_heads", "stamp", stamp.as_str())
    }

    /// Current completion head, when the birth stamp exists.
    pub fn completion_head(
        &self,
        stamp: &wire::TaskStamp,
    ) -> Result<Option<JournalHead<wire::CompletionHead>>, GuardError> {
        self.text_head(super::COMPLETION_HEADS, "stamp", stamp.as_str())
    }

    /// Every retained claim of one task with its lease, in winning order.
    pub fn task_claims(&self, stamp: &wire::TaskStamp) -> Result<Vec<ClaimHeads>, GuardError> {
        let (mut claims, bytes) = self.claims_of(&[stamp])?;
        read_metrics::observe_decode(bytes);
        Ok(claims.pop().map(|(_, claims)| claims).unwrap_or_default())
    }

    /// Every retained claim of `stamps` with its lease in one query, grouped
    /// by task in stamp order and by claim in winning order, with the claim
    /// and lease body bytes decoded. Tasks without claims are absent.
    pub(super) fn claims_of(
        &self,
        stamps: &[&wire::TaskStamp],
    ) -> Result<(ClaimsByTask, usize), GuardError> {
        let mut grouped = ClaimsByTask::new();
        let mut decoded = 0usize;
        if stamps.is_empty() {
            return Ok((grouped, decoded));
        }
        read_metrics::observe_query();
        let placeholders = vec!["?"; stamps.len()].join(", ");
        let mut statement = self
            .connection
            .prepare(&format!(
                "SELECT claim.stamp, claim.claimed, claim.head_seq, claim.body, \
                        lease.head_seq, lease.body \
                 FROM claim_heads AS claim LEFT JOIN lease_heads AS lease \
                   ON lease.stamp = claim.stamp AND lease.claimed = claim.claimed \
                 WHERE claim.stamp IN ({placeholders}) ORDER BY claim.stamp, claim.claimed"
            ))
            .map_err(GuardError::Store)?;
        let rows = statement
            .query_map(
                params_from_iter(stamps.iter().map(|stamp| stamp.as_str())),
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<Vec<u8>>>(5)?,
                    ))
                },
            )
            .map_err(GuardError::Store)?;
        for row in rows {
            let (stamp, claimed, claim_seq, claim, lease_seq, lease) =
                row.map_err(GuardError::Store)?;
            read_metrics::observe_row();
            decoded = decoded.saturating_add(claim.len());
            let claim = self.decoded_claim_head(&stamp, claimed, claim_seq, &claim)?;
            let lease = match (lease_seq, lease) {
                (Some(position), Some(bytes)) => {
                    decoded = decoded.saturating_add(bytes.len());
                    Some(self.decoded_lease_head(&stamp, claimed, position, &bytes)?)
                }
                _ => None,
            };
            let held = ClaimHeads { claim, lease };
            match grouped.last_mut() {
                Some((task, claims)) if task.as_str() == stamp => claims.push(held),
                _ => {
                    let task = stamp.parse::<wire::TaskStamp>().map_err(|error| {
                        GuardError::Invalid(format!("claim head key {stamp:?} is invalid: {error}"))
                    })?;
                    grouped.push((task, vec![held]));
                }
            }
        }
        Ok((grouped, decoded))
    }

    /// One exact claim's head, when the task retains it.
    pub fn claim_head(
        &self,
        stamp: &wire::TaskStamp,
        claimed: wire::Seq,
    ) -> Result<Option<JournalHead<wire::ClaimHead>>, GuardError> {
        let key = claim_key(claimed)?;
        self.claim_row("claim_heads", stamp, key)?
            .map(|(position, bytes)| self.decoded_claim_head(stamp.as_str(), key, position, &bytes))
            .transpose()
    }

    /// One exact claim's lease head, when the task retains it.
    pub fn lease_head(
        &self,
        stamp: &wire::TaskStamp,
        claimed: wire::Seq,
    ) -> Result<Option<JournalHead<wire::LeaseHead>>, GuardError> {
        let key = claim_key(claimed)?;
        self.claim_row("lease_heads", stamp, key)?
            .map(|(position, bytes)| self.decoded_lease_head(stamp.as_str(), key, position, &bytes))
            .transpose()
    }

    /// The task's claim-plane head: the newest head across its retained
    /// claims, absent before the first win.
    pub fn claim_plane(&self, stamp: &wire::TaskStamp) -> Result<Option<wire::Seq>, GuardError> {
        read_metrics::observe_query();
        let newest = self
            .connection
            .query_row(
                "SELECT MAX(head_seq) FROM claim_heads WHERE stamp = ?1",
                [stamp.as_str()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .map_err(GuardError::Store)?;
        newest
            .map(|position| {
                u64::try_from(position)
                    .ok()
                    .and_then(wire::Seq::new)
                    .ok_or_else(|| {
                        GuardError::Invalid(format!(
                            "task {stamp} claim head position {position} is not a journal position"
                        ))
                    })
            })
            .transpose()
    }

    fn claim_row(
        &self,
        table: &'static str,
        stamp: &wire::TaskStamp,
        claimed: i64,
    ) -> Result<Option<(i64, Vec<u8>)>, GuardError> {
        read_metrics::observe_query();
        let row = self
            .connection
            .query_row(
                &format!("SELECT head_seq, body FROM {table} WHERE stamp = ?1 AND claimed = ?2"),
                params![stamp.as_str(), claimed],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()
            .map_err(GuardError::Store)?;
        if let Some((_, bytes)) = &row {
            read_metrics::observe_row();
            read_metrics::observe_decode(bytes.len());
        }
        Ok(row)
    }

    /// Current trust head, when the digest exists.
    pub fn trust_head(
        &self,
        digest: &wire::Digest,
    ) -> Result<Option<JournalHead<wire::TrustHead>>, GuardError> {
        let head = self.text_head::<wire::TrustHead>("trust_heads", "digest", digest.as_str())?;
        if let Some(head) = &head
            && head.body.digest != *digest
        {
            return Err(GuardError::Invalid(format!(
                "trust head indexed as {digest} carries body digest {}",
                head.body.digest
            )));
        }
        Ok(head)
    }

    /// Current transcript source head, when that stream has been accounted.
    ///
    /// Addressed by [`wire::TranscriptStreamKey`] rather than by a stream name,
    /// because the row is keyed by lane, vendor and stream together. A bare
    /// stream name matches no row, and this lookup reports no row as `Ok(None)`
    /// -- the same answer a genuinely unaccounted stream gives. Passing one is
    /// not merely discouraged, it does not compile:
    ///
    /// ```compile_fail
    /// fn catch_up(read: &journal::JournalRead<'_>) {
    ///     let _ = read.transcript_source_head("sidecar");
    /// }
    /// ```
    pub fn transcript_source_head(
        &self,
        stream: &wire::TranscriptStreamKey,
    ) -> Result<Option<JournalHead<wire::TranscriptSourceHead>>, GuardError> {
        self.text_head("transcript_source_heads", "stream", stream.as_str())
    }

    /// Every accounted transcript head for one lane, in ascending stream order.
    ///
    /// The singular [`Self::transcript_source_head`] addresses one stream and
    /// needs a vendor and stream name to compose its key. A lane alone cannot
    /// supply them and nothing here maps a lane to the vendors that have
    /// occupied it, so this answers the question a lane *can* ask: everything
    /// accounted beneath it. One lane hosts several vendors over its life, which
    /// makes the whole set the honest answer rather than a convenience.
    ///
    /// The redundant lane predicate selects only matching rows; each selected
    /// value is then proved against its canonical body. Global verification owns
    /// corruption outside this scoped read, so another lane's retained history
    /// cannot make this lane's current boundary progressively more expensive.
    pub fn transcript_source_heads(
        &self,
        lane: &wire::CanonicalUuid,
    ) -> Result<Vec<JournalHead<wire::TranscriptSourceHead>>, GuardError> {
        let lane_text = lane.to_string();
        let mut statement = self
            .connection
            .prepare(
                "SELECT stream, lane, head_seq, body FROM transcript_source_heads \
                 WHERE lane = ?1 ORDER BY stream",
            )
            .map_err(GuardError::Store)?;
        read_metrics::observe_query();
        let rows = statement
            .query_map([&lane_text], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            })
            .map_err(GuardError::Store)?;
        let mut heads = Vec::new();
        for row in rows {
            let (stream, indexed_lane, position, bytes) = row.map_err(GuardError::Store)?;
            read_metrics::observe_row();
            read_metrics::observe_decode(bytes.len());
            let head = self.decoded_source_head(&stream, position, &bytes)?;
            if indexed_lane != lane_text || head.body.lane != *lane {
                return Err(GuardError::Invalid(format!(
                    "transcript source head {stream:?} lane predicate disagrees with its body"
                )));
            }
            heads.push(head);
        }
        Ok(heads)
    }

    /// The accounted transcript boundary a lane's phase interval opens or closes at.
    ///
    /// This is the only way a lifecycle fact obtains a boundary, so every such
    /// fact names the same authority. It answers a proven boundary or it fails:
    /// a lane with nothing accounted yet is proven at zero heads, and a journal
    /// that cannot be read is a violated assumption rather than an unprovable
    /// boundary.
    pub fn source_boundary(
        &self,
        lane: &wire::CanonicalUuid,
    ) -> Result<SourceBoundary, GuardError> {
        Ok(SourceBoundary::Proven {
            heads: self
                .transcript_source_heads(lane)?
                .into_iter()
                .map(|head| head.body)
                .collect(),
        })
    }

    /// Every lane a live claim is currently bound to.
    ///
    /// A membership transition records boundaries only for these, because a
    /// lane nobody is working spends nothing across the move and a boundary for
    /// it would be attribution noise the reducer has to filter back out. The
    /// active predicate selects only matching index rows, then every selected
    /// predicate and position is proved against its joined canonical head.
    pub fn actively_claimed_lanes(&self) -> Result<Vec<wire::CanonicalUuid>, GuardError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT indexed.stamp, indexed.claimed, indexed.lane, indexed.disposition, \
                        indexed.head_seq, heads.head_seq, heads.body \
                 FROM claim_index AS indexed \
                 JOIN claim_heads AS heads \
                   ON heads.stamp = indexed.stamp AND heads.claimed = indexed.claimed \
                 WHERE indexed.disposition = 'active' \
                 ORDER BY indexed.lane, indexed.stamp, indexed.claimed",
            )
            .map_err(GuardError::Store)?;
        read_metrics::observe_query();
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Vec<u8>>(6)?,
                ))
            })
            .map_err(GuardError::Store)?;
        let mut lanes = Vec::new();
        for row in rows {
            let (stamp, claimed, indexed_lane, disposition, indexed_seq, head_seq, bytes) =
                row.map_err(GuardError::Store)?;
            read_metrics::observe_row();
            if indexed_seq != head_seq {
                return Err(GuardError::Invalid(format!(
                    "claim index for task {stamp} disagrees with claim head position"
                )));
            }
            read_metrics::observe_decode(bytes.len());
            let head = self.decoded_claim_head(&stamp, claimed, head_seq, &bytes)?;
            if indexed_lane != head.body.lane.to_string() {
                return Err(GuardError::Invalid(format!(
                    "claim index for task {stamp} disagrees with claim lane"
                )));
            }
            if disposition != claim_disposition_name(head.body.disposition)
                || head.body.disposition != wire::ClaimDisposition::Active
            {
                return Err(GuardError::Invalid(format!(
                    "claim index for task {stamp} disagrees with claim disposition"
                )));
            }
            lanes.push(head.body.lane);
        }
        Ok(lanes)
    }

    /// Boundaries for each named lane that is actively claimed, in the given order.
    ///
    /// The filter is applied here rather than by each caller so every membership
    /// transition scopes attribution the same way, and the answer stays empty
    /// rather than becoming a second spelling of *unknown* when no named lane is
    /// claimed: an empty list says no claimed lane moved, which is exactly what
    /// the reducer needs to attribute nothing across the transition.
    pub fn lane_source_boundaries(
        &self,
        lanes: &[wire::CanonicalUuid],
    ) -> Result<Vec<wire::body::LaneSourceBoundary>, GuardError> {
        let claimed = self.actively_claimed_lanes()?;
        lanes
            .iter()
            .filter(|lane| claimed.contains(lane))
            .map(|lane| {
                Ok(wire::body::LaneSourceBoundary {
                    lane: *lane,
                    source: self.source_boundary(lane)?,
                })
            })
            .collect()
    }

    /// Decode one claim head and prove it against its stored key and position.
    ///
    /// The same checks [`Self::claim_head`] makes when it is asked for one
    /// claim, so a scan cannot admit a row the keyed read would have refused.
    pub(super) fn decoded_claim_head(
        &self,
        stamp: &str,
        claimed: i64,
        position: i64,
        bytes: &[u8],
    ) -> Result<JournalHead<wire::ClaimHead>, GuardError> {
        let head: JournalHead<wire::ClaimHead> =
            self.decoded_claim_row("claim", stamp, position, bytes)?;
        if head.body.fact != head.position {
            return Err(GuardError::Invalid(format!(
                "task {stamp} claim head fact disagrees with head_seq"
            )));
        }
        if i64::try_from(head.body.claimed.get()).ok() != Some(claimed) {
            return Err(GuardError::Invalid(format!(
                "task {stamp} claim head identity disagrees with its key {claimed}"
            )));
        }
        Ok(head)
    }

    /// Decode one lease head and prove it against its stored key and position.
    fn decoded_lease_head(
        &self,
        stamp: &str,
        claimed: i64,
        position: i64,
        bytes: &[u8],
    ) -> Result<JournalHead<wire::LeaseHead>, GuardError> {
        let head: JournalHead<wire::LeaseHead> =
            self.decoded_claim_row("lease", stamp, position, bytes)?;
        if i64::try_from(head.body.claimed.get()).ok() != Some(claimed) {
            return Err(GuardError::Invalid(format!(
                "task {stamp} lease head identity disagrees with its key {claimed}"
            )));
        }
        Ok(head)
    }

    fn decoded_claim_row<T>(
        &self,
        plane: &str,
        stamp: &str,
        position: i64,
        bytes: &[u8],
    ) -> Result<JournalHead<T>, GuardError>
    where
        T: serde::de::DeserializeOwned + Serialize,
    {
        let head: JournalHead<T> = decode_head(position, bytes).map_err(|error| {
            let detail = match error {
                GuardError::Invalid(detail) => detail,
                other => other.to_string(),
            };
            GuardError::Invalid(format!("{plane} head {stamp:?} is invalid: {detail}"))
        })?;
        if self.cursor.is_none_or(|cursor| head.position > cursor) {
            return Err(GuardError::Invalid(format!(
                "{plane} head {stamp:?} position {} is beyond cursor {:?}",
                head.position,
                self.cursor.map(wire::Seq::get)
            )));
        }
        Ok(head)
    }

    /// Decode one transcript source head and prove it against its stored key.
    fn decoded_source_head(
        &self,
        stream: &str,
        position: i64,
        bytes: &[u8],
    ) -> Result<JournalHead<wire::TranscriptSourceHead>, GuardError> {
        let head: JournalHead<wire::TranscriptSourceHead> =
            decode_head(position, bytes).map_err(|error| {
                let detail = match error {
                    GuardError::Invalid(detail) => detail,
                    other => other.to_string(),
                };
                GuardError::Invalid(format!(
                    "transcript source head {stream:?} is invalid: {detail}"
                ))
            })?;
        if head.body.key().as_str() != stream {
            return Err(GuardError::Invalid(format!(
                "transcript source head stored as {stream:?} names {:?}",
                head.body.key().as_str()
            )));
        }
        if self.cursor.is_none_or(|cursor| head.position > cursor) {
            return Err(GuardError::Invalid(format!(
                "transcript source head {stream:?} position {} is beyond cursor {:?}",
                head.position,
                self.cursor.map(wire::Seq::get)
            )));
        }
        Ok(head)
    }

    /// Current resident head, when an epoch has been seated.
    pub fn residency_head(&self) -> Result<Option<JournalHead<wire::ResidencyHead>>, GuardError> {
        let cached_position = self
            .residency_cache
            .and_then(|cache| cache.borrow().as_ref().map(JournalHead::position))
            .map(|position| {
                i64::try_from(position.get()).map_err(|_| {
                    GuardError::Invalid(format!(
                        "residency head position {position} is outside the SQLite range"
                    ))
                })
            })
            .transpose()?;
        read_metrics::observe_query();
        let row = self
            .connection
            .query_row(
                "SELECT head_seq, \
                 CASE WHEN ?1 IS NOT NULL AND head_seq = ?1 THEN NULL ELSE body END \
                 FROM residency_head WHERE singleton = 1",
                [cached_position],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<Vec<u8>>>(1)?)),
            )
            .optional()
            .map_err(GuardError::Store)?;
        let Some((stored_position, bytes)) = row else {
            if let Some(cache) = self.residency_cache {
                *cache.borrow_mut() = None;
            }
            return Ok(None);
        };
        read_metrics::observe_row();
        let position = decode_seq(stored_position).map_err(GuardError::Invalid)?;
        let head = if let Some(bytes) = bytes {
            read_metrics::observe_decode(bytes.len());
            let head = decode_head(stored_position, &bytes).map_err(|error| {
                GuardError::Invalid(format!("residency head is invalid: {error}"))
            })?;
            if let Some(cache) = self.residency_cache {
                *cache.borrow_mut() = Some(head.clone());
            }
            head
        } else {
            self.residency_cache
                .and_then(|cache| cache.borrow().clone())
                .filter(|head| head.position == position)
                .ok_or_else(|| {
                    GuardError::Invalid(
                        "residency head cache disagrees with its stored position".to_owned(),
                    )
                })?
        };
        if self.cursor.is_none_or(|cursor| head.position > cursor) {
            return Err(GuardError::Invalid(format!(
                "residency head position {} is beyond cursor {:?}",
                head.position,
                self.cursor.map(wire::Seq::get)
            )));
        }
        Ok(Some(head))
    }

    fn text_head<T>(
        &self,
        table: &'static str,
        key_column: &'static str,
        key: &str,
    ) -> Result<Option<JournalHead<T>>, GuardError>
    where
        T: serde::de::DeserializeOwned + Serialize,
    {
        self.text_head_with(table, key_column, key, decode_head)
    }

    fn text_head_with<T>(
        &self,
        table: &'static str,
        key_column: &'static str,
        key: &str,
        decode: impl FnOnce(i64, &[u8]) -> Result<JournalHead<T>, GuardError>,
    ) -> Result<Option<JournalHead<T>>, GuardError> {
        let statement = format!("SELECT head_seq, body FROM {table} WHERE {key_column} = ?1");
        read_metrics::observe_query();
        let row = self
            .connection
            .query_row(&statement, [key], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .optional()
            .map_err(GuardError::Store)?;
        row.map_or(Ok(None), |(position, bytes)| {
            read_metrics::observe_row();
            read_metrics::observe_decode(bytes.len());
            let head = decode(position, &bytes).map_err(|error| {
                let detail = match error {
                    GuardError::Invalid(detail) => detail,
                    other => other.to_string(),
                };
                GuardError::Invalid(format!("{table} {key_column} {key:?} is invalid: {detail}"))
            })?;
            if self.cursor.is_none_or(|cursor| head.position > cursor) {
                return Err(GuardError::Invalid(format!(
                    "{table} {key_column} {key:?} head position {} is beyond cursor {:?}",
                    head.position,
                    self.cursor.map(wire::Seq::get)
                )));
            }
            Ok(Some(head))
        })
    }
}

/// Decode a stored task head, rendering its topic nodes as their current paths.
pub(super) fn decode_task_head(
    connection: &rusqlite::Connection,
    position: i64,
    bytes: &[u8],
) -> Result<JournalHead<wire::TaskHead>, GuardError> {
    let body =
        crate::topic::decode_stored_task_head(connection, bytes).map_err(GuardError::Invalid)?;
    let position = decode_seq(position).map_err(GuardError::Invalid)?;
    Ok(JournalHead { position, body })
}

pub(super) fn decode_head<T>(position: i64, bytes: &[u8]) -> Result<JournalHead<T>, GuardError>
where
    T: serde::de::DeserializeOwned + Serialize,
{
    let head = wire::decode(bytes).map_err(|error| GuardError::Invalid(error.to_string()))?;
    let canonical = wire::encode(&head).map_err(|error| GuardError::Invalid(error.to_string()))?;
    if canonical != bytes {
        return Err(GuardError::Invalid(
            "head is not stored as canonical wire bytes".to_owned(),
        ));
    }
    let position = decode_seq(position).map_err(GuardError::Invalid)?;
    Ok(JournalHead {
        position,
        body: head,
    })
}

fn claim_key(claimed: wire::Seq) -> Result<i64, GuardError> {
    i64::try_from(claimed.get())
        .map_err(|_| GuardError::Invalid("claim identity exceeds SQLite".to_owned()))
}
