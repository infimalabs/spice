//! Judge Design item standing by each lane's latest sign alone.
//!
//! Item series drop the run-only fields that judged concession and burial.
//! Every remaining key keeps its value and order, so a crossed row equals the
//! fold of the same history.

use rusqlite::Transaction;

use super::super::OpenError;

pub(super) fn cross(transaction: &Transaction<'_>) -> Result<(), OpenError> {
    transaction
        .execute_batch(
            "UPDATE design_maturity SET body = CAST(json_set(
                CAST(body AS TEXT),
                '$.items',
                (SELECT json_group_array(json(series)) FROM (
                    SELECT json_remove(
                        value,
                        '$.positive',
                        '$.negative',
                        '$.suffix_lanes',
                        '$.suffix_conceded',
                        '$.displaced_strong',
                        '$.last_strong'
                    ) AS series
                    FROM json_each(CAST(body AS TEXT), '$.items')
                    ORDER BY key
                ))
            ) AS BLOB);",
        )
        .map_err(OpenError::Sqlite)
}
