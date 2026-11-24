//! Joined, bounded pages over current task and trust authority.

use rusqlite::{OptionalExtension, Row, params};

use super::{
    ClaimHeads, FactReadBudget, GuardError, JournalHead, JournalRead, TaskHeads, read_metrics,
};

/// Cursor identity shared by every page in one current-head projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CurrentHeadWitness {
    /// Immutable journal cursor observed by the enclosing read transaction.
    pub cursor: Option<wire::Seq>,
}

/// One ordered bounded page of complete current task-head bundles.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskHeadPage {
    /// Complete bundles decoded within this call's row and byte budgets.
    pub rows: Vec<TaskHeads>,
    /// Last returned stamp when more current task authority remains.
    pub continuation: Option<wire::TaskStamp>,
    /// Cursor identity of the projection this page observed.
    pub witness: CurrentHeadWitness,
}

/// One ordered bounded page of current trust heads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustHeadPage {
    /// Trust heads decoded within this call's row and byte budgets.
    pub rows: Vec<JournalHead<wire::TrustHead>>,
    /// Last returned digest when more current trust authority remains.
    pub continuation: Option<wire::Digest>,
    /// Cursor identity of the projection this page observed.
    pub witness: CurrentHeadWitness,
}

/// One bounded operator-report frontier over current trust heads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustReportHeadPage {
    /// Heads selected for possible differential enrichment.
    pub rows: Vec<JournalHead<wire::TrustHead>>,
    /// First head beyond this frontier, without recorded documents loaded.
    pub next: Option<JournalHead<wire::TrustHead>>,
    /// Cursor identity of the projection this page observed.
    pub witness: CurrentHeadWitness,
}

impl JournalRead<'_> {
    /// Read one joined page of task statement, workflow, completion, claim,
    /// and lease heads.
    pub fn task_head_page(
        &self,
        after: Option<&wire::TaskStamp>,
        budget: FactReadBudget,
    ) -> Result<TaskHeadPage, GuardError> {
        self.task_head_page_with(
            TASK_PAGE_SQL,
            params![after.map(wire::TaskStamp::as_str), query_limit(budget)?],
            budget,
        )
    }

    /// Current direct neighbors in either dependency direction, including settled work.
    pub fn task_neighbor_head_page(
        &self,
        task: &wire::TaskStamp,
        dependencies: &[wire::TaskStamp],
        after: Option<&wire::TaskStamp>,
        budget: FactReadBudget,
    ) -> Result<TaskHeadPage, GuardError> {
        let dependencies = serde_json::to_string(dependencies)
            .map_err(|error| GuardError::Invalid(error.to_string()))?;
        self.task_head_page_with(
            TASK_NEIGHBOR_PAGE_SQL,
            params![
                task.as_str(),
                dependencies,
                after.map(wire::TaskStamp::as_str),
                query_limit(budget)?
            ],
            budget,
        )
    }

    fn task_head_page_with<P: rusqlite::Params>(
        &self,
        sql: &str,
        parameters: P,
        budget: FactReadBudget,
    ) -> Result<TaskHeadPage, GuardError> {
        read_metrics::observe_query();
        let mut statement = self.connection.prepare(sql).map_err(GuardError::Store)?;
        let mut rows = statement.query(parameters).map_err(GuardError::Store)?;
        let mut joined = Vec::with_capacity(budget.rows());
        let mut decoded = 0usize;
        let mut continuation = None;
        while let Some(row) = rows.next().map_err(GuardError::Store)? {
            let indexed = row.get::<_, String>(0).map_err(GuardError::Store)?;
            read_metrics::observe_row();
            if joined.len() == budget.rows() {
                continuation = joined.last().map(|task: &JoinedTask| task.stamp.clone());
                break;
            }
            let row_bytes = task_row_bytes(row, &indexed)?;
            if decoded.saturating_add(row_bytes) > budget.decoded_bytes() {
                if joined.is_empty() {
                    return Err(GuardError::Invalid(format!(
                        "task head stamp {indexed:?} has {row_bytes} body bytes, exceeding this call's {}-byte decoded budget",
                        budget.decoded_bytes(),
                    )));
                }
                continuation = joined.last().map(|task| task.stamp.clone());
                break;
            }
            joined.push(self.decode_task_row(row, &indexed)?);
            read_metrics::observe_decode(row_bytes);
            decoded = decoded.saturating_add(row_bytes);
        }
        drop(rows);
        // The joined row already charged each task's claim and lease bytes.
        let (mut claims, _) =
            self.claims_of(&joined.iter().map(|task| &task.stamp).collect::<Vec<_>>())?;
        let tasks = joined
            .into_iter()
            .map(|task| {
                let held = claims
                    .binary_search_by(|(stamp, _)| stamp.as_str().cmp(task.stamp.as_str()))
                    .map(|index| std::mem::take(&mut claims[index].1))
                    .unwrap_or_default();
                task.with_claims(held)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(TaskHeadPage {
            rows: tasks,
            continuation,
            witness: CurrentHeadWitness {
                cursor: self.cursor,
            },
        })
    }

    /// Read one joined page of current trust heads in digest order.
    pub fn trust_head_page(
        &self,
        after: Option<&wire::Digest>,
        budget: FactReadBudget,
    ) -> Result<TrustHeadPage, GuardError> {
        let query_limit = budget
            .rows()
            .checked_add(1)
            .expect("bounded trust page probe");
        let query_limit = i64::try_from(query_limit)
            .map_err(|_| GuardError::Invalid("trust head page limit exceeds SQLite".to_owned()))?;
        read_metrics::observe_query();
        let mut statement = self
            .connection
            .prepare(TRUST_PAGE_SQL)
            .map_err(GuardError::Store)?;
        let mut rows = statement
            .query(params![after.map(wire::Digest::as_str), query_limit])
            .map_err(GuardError::Store)?;
        let mut heads = Vec::with_capacity(budget.rows());
        let mut decoded = 0usize;
        let mut continuation = None;
        while let Some(row) = rows.next().map_err(GuardError::Store)? {
            let indexed = row.get::<_, String>(0).map_err(GuardError::Store)?;
            read_metrics::observe_row();
            if heads.len() == budget.rows() {
                continuation = heads
                    .last()
                    .map(|head: &JournalHead<wire::TrustHead>| head.body.digest.clone());
                break;
            }
            let length = joined_length(row, 2, "trust head key", &indexed)?;
            if decoded.saturating_add(length) > budget.decoded_bytes() {
                if heads.is_empty() {
                    return Err(GuardError::Invalid(format!(
                        "trust head key {indexed:?} has {length} body bytes, exceeding this call's {}-byte decoded budget",
                        budget.decoded_bytes(),
                    )));
                }
                continuation = heads.last().map(|head| head.body.digest.clone());
                break;
            }
            let position = row.get::<_, i64>(1).map_err(GuardError::Store)?;
            let head: JournalHead<wire::TrustHead> = decode_joined_head(
                row,
                position,
                3,
                "trust_heads",
                ("digest", &indexed),
                self.cursor,
                super::decode_head,
            )?;
            let digest = indexed.parse::<wire::Digest>().map_err(|error| {
                GuardError::Invalid(format!("trust head key {indexed:?} is invalid: {error}"))
            })?;
            if head.body.digest != digest {
                return Err(GuardError::Invalid(format!(
                    "trust head indexed as {digest} carries body digest {}",
                    head.body.digest
                )));
            }
            read_metrics::observe_decode(length);
            decoded = decoded.saturating_add(length);
            heads.push(head);
        }
        Ok(TrustHeadPage {
            rows: heads,
            continuation,
            witness: CurrentHeadWitness {
                cursor: self.cursor,
            },
        })
    }

    /// Read one bounded page of current trust heads for the operator report,
    /// ordered by digest. A digest is the one key a ruling never changes, so a
    /// ruling made between pages can neither skip nor repeat a row. A cursor
    /// that names no recorded ruling reads no page.
    pub fn trust_report_head_page(
        &self,
        after: Option<&wire::Digest>,
        budget: FactReadBudget,
    ) -> Result<Option<TrustReportHeadPage>, GuardError> {
        if let Some(after) = after {
            read_metrics::observe_query();
            let recorded = self
                .connection
                .query_row(
                    "SELECT 1 FROM trust_heads WHERE digest = ?1",
                    [after.as_str()],
                    |_| Ok(()),
                )
                .optional()
                .map_err(GuardError::Store)?;
            if recorded.is_none() {
                return Ok(None);
            }
        }
        let query_limit = query_limit(budget)?;
        read_metrics::observe_query();
        let mut statement = self
            .connection
            .prepare(TRUST_REPORT_PAGE_SQL)
            .map_err(GuardError::Store)?;
        let mut rows = statement
            .query(params![after.map(wire::Digest::as_str), query_limit])
            .map_err(GuardError::Store)?;
        let mut heads = Vec::with_capacity(budget.rows());
        let mut decoded = 0usize;
        let mut next = None;
        while let Some(row) = rows.next().map_err(GuardError::Store)? {
            let rank = row.get::<_, i64>(0).map_err(GuardError::Store)?;
            let indexed = row.get::<_, String>(1).map_err(GuardError::Store)?;
            read_metrics::observe_row();
            let length = joined_length(row, 3, "trust head key", &indexed)?;
            if decoded.saturating_add(length) > budget.decoded_bytes() {
                return Err(GuardError::Invalid(format!(
                    "trust report frontier exceeds this call's {}-byte decoded budget",
                    budget.decoded_bytes(),
                )));
            }
            let position = row.get::<_, i64>(2).map_err(GuardError::Store)?;
            let head: JournalHead<wire::TrustHead> = decode_joined_head(
                row,
                position,
                4,
                "trust_heads",
                ("digest", &indexed),
                self.cursor,
                super::decode_head,
            )?;
            validate_trust_index(rank, &indexed, &head)?;
            read_metrics::observe_decode(length);
            decoded = decoded.saturating_add(length);
            if heads.len() == budget.rows() {
                next = Some(head);
                break;
            }
            heads.push(head);
        }
        Ok(Some(TrustReportHeadPage {
            rows: heads,
            next,
            witness: CurrentHeadWitness {
                cursor: self.cursor,
            },
        }))
    }

    /// Most recently ruled current grant other than `excluded`.
    pub fn latest_other_granted_trust(
        &self,
        excluded: &wire::Digest,
    ) -> Result<Option<JournalHead<wire::TrustHead>>, GuardError> {
        read_metrics::observe_query();
        self.connection
            .query_row(LATEST_OTHER_GRANT_SQL, [excluded.as_str()], |row| {
                let indexed = row.get::<_, String>(0)?;
                let position = row.get::<_, i64>(1)?;
                let head = decode_joined_head(
                    row,
                    position,
                    3,
                    "trust_heads",
                    ("digest", &indexed),
                    self.cursor,
                    super::decode_head,
                )
                .map_err(|error| match error {
                    GuardError::Store(error) => error,
                    other => rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Blob,
                        Box::new(other),
                    ),
                })?;
                validate_trust_index(1, &indexed, &head).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Blob,
                        Box::new(error),
                    )
                })?;
                Ok(head)
            })
            .optional()
            .map_err(GuardError::Store)
    }
}

fn validate_trust_index(
    rank: i64,
    indexed: &str,
    head: &JournalHead<wire::TrustHead>,
) -> Result<(), GuardError> {
    let digest = indexed.parse::<wire::Digest>().map_err(|error| {
        GuardError::Invalid(format!("trust head key {indexed:?} is invalid: {error}"))
    })?;
    if head.body.digest != digest {
        return Err(GuardError::Invalid(format!(
            "trust head indexed as {digest} carries body digest {}",
            head.body.digest
        )));
    }
    let expected = standing_rank(head.body.state);
    if rank != expected {
        return Err(GuardError::Invalid(format!(
            "trust head {digest} standing rank {rank} disagrees with its {} body",
            head.body.state.as_str()
        )));
    }
    Ok(())
}

/// Report order of one standing, as `trust_heads.standing_rank` stores it.
const fn standing_rank(standing: wire::TrustState) -> i64 {
    match standing {
        wire::TrustState::Pended => 0,
        wire::TrustState::Granted => 1,
        wire::TrustState::Revoked => 2,
    }
}

const TRUST_REPORT_PAGE_SQL: &str = "SELECT standing_rank, digest, head_seq, length(body), body FROM trust_heads \
     WHERE digest > COALESCE(?1, '') ORDER BY digest LIMIT ?2";

const LATEST_OTHER_GRANT_SQL: &str = "SELECT digest, head_seq, length(body), body FROM trust_heads \
     WHERE standing_rank = 1 AND digest <> ?1 ORDER BY head_seq DESC LIMIT 1";

fn query_limit(budget: FactReadBudget) -> Result<i64, GuardError> {
    let limit = budget
        .rows()
        .checked_add(1)
        .expect("bounded task page probe");
    i64::try_from(limit)
        .map_err(|_| GuardError::Invalid("task head page limit exceeds SQLite".to_owned()))
}

const TRUST_PAGE_SQL: &str = "SELECT digest, head_seq, length(body), body FROM trust_heads \
    WHERE digest > COALESCE(?1, '') ORDER BY digest LIMIT ?2";

const TASK_PAGE_SQL: &str = "WITH task_stamps(stamp) AS (\
    SELECT stamp FROM (SELECT stamp FROM task_heads \
        WHERE stamp > COALESCE(?1, '') ORDER BY stamp LIMIT ?2) \
    UNION SELECT stamp FROM (SELECT stamp FROM workflow_heads \
        WHERE stamp > COALESCE(?1, '') ORDER BY stamp LIMIT ?2) \
    UNION SELECT stamp FROM (SELECT stamp FROM completion_heads \
        WHERE stamp > COALESCE(?1, '') ORDER BY stamp LIMIT ?2) \
    UNION SELECT stamp FROM (SELECT DISTINCT stamp FROM claim_heads \
        WHERE stamp > COALESCE(?1, '') ORDER BY stamp LIMIT ?2)\
) SELECT keys.stamp, statement.head_seq, length(statement.body), statement.body, \
         workflow.head_seq, length(workflow.body), workflow.body, \
         completion.head_seq, length(completion.body), completion.body, \
         (SELECT COALESCE(sum(length(claim.body)), 0) FROM claim_heads AS claim \
            WHERE claim.stamp = keys.stamp) \
         + (SELECT COALESCE(sum(length(lease.body)), 0) FROM lease_heads AS lease \
            WHERE lease.stamp = keys.stamp) \
  FROM task_stamps AS keys \
  LEFT JOIN task_heads AS statement ON statement.stamp = keys.stamp \
  LEFT JOIN workflow_heads AS workflow ON workflow.stamp = keys.stamp \
  LEFT JOIN completion_heads AS completion ON completion.stamp = keys.stamp \
 ORDER BY keys.stamp LIMIT ?2";

const TASK_NEIGHBOR_PAGE_SQL: &str = "WITH task_stamps(stamp) AS (\
    SELECT stamp FROM task_heads \
     WHERE stamp > COALESCE(?3, '') AND (\
       stamp IN (SELECT value FROM json_each(?2)) OR \
       EXISTS (SELECT 1 FROM json_each(CAST(body AS TEXT), '$.dependencies') WHERE value = ?1)) \
     ORDER BY stamp LIMIT ?4\
) SELECT keys.stamp, statement.head_seq, length(statement.body), statement.body, \
         workflow.head_seq, length(workflow.body), workflow.body, \
         completion.head_seq, length(completion.body), completion.body, \
         (SELECT COALESCE(sum(length(claim.body)), 0) FROM claim_heads AS claim \
            WHERE claim.stamp = keys.stamp) \
         + (SELECT COALESCE(sum(length(lease.body)), 0) FROM lease_heads AS lease \
            WHERE lease.stamp = keys.stamp) \
  FROM task_stamps AS keys \
  LEFT JOIN task_heads AS statement ON statement.stamp = keys.stamp \
  LEFT JOIN workflow_heads AS workflow ON workflow.stamp = keys.stamp \
  LEFT JOIN completion_heads AS completion ON completion.stamp = keys.stamp \
 ORDER BY keys.stamp LIMIT ?4";

#[cfg(test)]
pub(crate) fn verify_task_page_plan(connection: &rusqlite::Connection) -> Result<(), String> {
    let mut trust_plans = Vec::new();
    for after in [None, Some("spice.test.digest")] {
        let mut statement = connection
            .prepare(&format!("EXPLAIN QUERY PLAN {TRUST_PAGE_SQL}"))
            .map_err(|error| error.to_string())?;
        let plan = statement
            .query_map(params![after, 3_i64], |row| row.get::<_, String>(3))
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        trust_plans.push(plan);
    }
    let trust_seeks = trust_plans.iter().all(|plan| {
        plan.iter().any(|step| step.contains("(digest>?)"))
            && !plan.iter().any(|step| step.contains("SCAN trust_heads "))
    });
    let mut statement = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {TRUST_REPORT_PAGE_SQL}"))
        .map_err(|error| error.to_string())?;
    let report_plan = statement
        .query_map(params!["spice.test.digest", 3_i64], |row| {
            row.get::<_, String>(3)
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let report_seeks = report_plan.iter().any(|step| step.contains("(digest>?)"))
        && !report_plan
            .iter()
            .any(|step| step.contains("SCAN trust_heads "));
    let mut statement = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {LATEST_OTHER_GRANT_SQL}"))
        .map_err(|error| error.to_string())?;
    let grant_plan = statement
        .query_map(["spice.test.digest"], |row| row.get::<_, String>(3))
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let grant_seeks = grant_plan
        .iter()
        .any(|step| step.contains("trust_heads_granted_position"))
        && !grant_plan
            .iter()
            .any(|step| step.contains("SCAN trust_heads "));
    let mut statement = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {TASK_PAGE_SQL}"))
        .map_err(|error| error.to_string())?;
    let plan = statement
        .query_map(params![Option::<String>::None, 3_i64], |row| {
            row.get::<_, String>(3)
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let seeks = plan
        .iter()
        .filter(|step| step.contains("(stamp>?)"))
        .count();
    let full_scan = plan.iter().any(|step| {
        [
            "task_heads",
            "workflow_heads",
            "completion_heads",
            "claim_heads",
            "lease_heads",
        ]
        .iter()
        .any(|table| step.contains(&format!("SCAN {table} ")))
    });
    (trust_seeks && report_seeks && grant_seeks && seeks == 4 && !full_scan)
        .then_some(())
        .ok_or_else(|| {
            format!(
                "expected bounded trust and global-task seeks, found trust={trust_plans:?} report={report_plan:?} grant={grant_plan:?} global={plan:?}"
            )
        })
}

fn task_row_bytes(row: &Row<'_>, indexed: &str) -> Result<usize, GuardError> {
    let claims = row.get::<_, i64>(10).map_err(GuardError::Store)?;
    [2, 5, 8]
        .into_iter()
        .map(|column| {
            row.get::<_, Option<i64>>(column)
                .map_err(GuardError::Store)?
                .map(|length| joined_length_value(length, "task head stamp", indexed))
                .transpose()
        })
        .chain([joined_length_value(claims, "task head stamp", indexed).map(Some)])
        .try_fold(0usize, |total, length| {
            total
                .checked_add(length?.unwrap_or_default())
                .ok_or_else(|| {
                    GuardError::Invalid(format!(
                        "task head stamp {indexed:?} byte count overflowed"
                    ))
                })
        })
}

/// One task page row's joined heads, before its claims are attached.
struct JoinedTask {
    stamp: wire::TaskStamp,
    statement: Option<JournalHead<wire::TaskHead>>,
    workflow: Option<JournalHead<wire::WorkflowHead>>,
    completion: Option<JournalHead<wire::CompletionHead>>,
}

impl JoinedTask {
    /// The complete bundle, refusing a task whose heads do not cohere.
    fn with_claims(self, claims: Vec<ClaimHeads>) -> Result<TaskHeads, GuardError> {
        let Self {
            stamp,
            statement,
            workflow,
            completion,
        } = self;
        let loose = [
            workflow.as_ref().map(|_| "workflow"),
            completion.as_ref().map(|_| "completion"),
            (!claims.is_empty()).then_some("claim"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        let statement: JournalHead<wire::TaskHead> = statement.ok_or_else(|| {
            GuardError::Invalid(format!(
                "task {stamp} has loose {} head(s) without a statement head",
                loose.join(", ")
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
        if statement.body.born != stamp {
            return Err(GuardError::Invalid(format!(
                "task head indexed as {stamp} carries body stamp {}",
                statement.body.born
            )));
        }
        if statement.body.revision != statement.position {
            return Err(GuardError::Invalid(format!(
                "task {stamp} statement head revision disagrees with head_seq"
            )));
        }
        Ok(TaskHeads {
            statement,
            workflow,
            completion,
            claims,
        })
    }
}

impl JournalRead<'_> {
    fn decode_task_row(&self, row: &Row<'_>, indexed: &str) -> Result<JoinedTask, GuardError> {
        let cursor = self.cursor;
        let stamp = indexed.parse::<wire::TaskStamp>().map_err(|error| {
            GuardError::Invalid(format!("task head key {indexed:?} is invalid: {error}"))
        })?;
        Ok(JoinedTask {
            stamp,
            statement: optional_head(
                row,
                1,
                3,
                "task_heads",
                indexed,
                cursor,
                |position, bytes| super::decode_task_head(self.connection, position, bytes),
            )?,
            workflow: optional_head(
                row,
                4,
                6,
                "workflow_heads",
                indexed,
                cursor,
                super::decode_head,
            )?,
            completion: optional_head(
                row,
                7,
                9,
                "completion_heads",
                indexed,
                cursor,
                super::decode_head,
            )?,
        })
    }
}

fn optional_head<T>(
    row: &Row<'_>,
    position_column: usize,
    body_column: usize,
    table: &'static str,
    key: &str,
    cursor: Option<wire::Seq>,
    decode: impl FnOnce(i64, &[u8]) -> Result<JournalHead<T>, GuardError>,
) -> Result<Option<JournalHead<T>>, GuardError> {
    let position = row
        .get::<_, Option<i64>>(position_column)
        .map_err(GuardError::Store)?;
    position
        .map(|position| {
            decode_joined_head(
                row,
                position,
                body_column,
                table,
                ("stamp", key),
                cursor,
                decode,
            )
        })
        .transpose()
}

/// Decode one joined head whose `key` is its `(column, value)` pair.
fn decode_joined_head<T>(
    row: &Row<'_>,
    position: i64,
    body_column: usize,
    table: &'static str,
    (key_column, key): (&'static str, &str),
    cursor: Option<wire::Seq>,
    decode: impl FnOnce(i64, &[u8]) -> Result<JournalHead<T>, GuardError>,
) -> Result<JournalHead<T>, GuardError> {
    let bytes = row
        .get::<_, Vec<u8>>(body_column)
        .map_err(GuardError::Store)?;
    let head = decode(position, &bytes).map_err(|error| {
        let detail = match error {
            GuardError::Invalid(detail) => detail,
            other => other.to_string(),
        };
        GuardError::Invalid(format!("{table} {key_column} {key:?} is invalid: {detail}"))
    })?;
    if cursor.is_none_or(|cursor| head.position > cursor) {
        return Err(GuardError::Invalid(format!(
            "{table} {key_column} {key:?} head position {} is beyond cursor {:?}",
            head.position,
            cursor.map(wire::Seq::get)
        )));
    }
    Ok(head)
}

fn joined_length(
    row: &Row<'_>,
    column: usize,
    identity: &str,
    value: &str,
) -> Result<usize, GuardError> {
    let length = row.get::<_, i64>(column).map_err(GuardError::Store)?;
    joined_length_value(length, identity, value)
}

fn joined_length_value(length: i64, identity: &str, value: &str) -> Result<usize, GuardError> {
    usize::try_from(length)
        .map_err(|_| GuardError::Invalid(format!("{identity} {value:?} has invalid body length")))
}
