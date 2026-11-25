//! Judge Design item standing by each lane's latest sign alone.
//!
//! Item series drop the run-only fields that judged concession and burial.
//! Every remaining key keeps its value and order, so a crossed row equals the
//! fold of the same history.

use rusqlite::Transaction;
use serde::Deserialize;

use super::super::OpenError;

#[expect(
    dead_code,
    reason = "the complete legacy shape is validated by deserialization"
)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Schema3DesignMaturity {
    contributions: u64,
    items: Vec<Schema3ItemSeries>,
    contributing_lanes: Vec<wire::CanonicalUuid>,
    contributing_lanes_truncated: bool,
    requests: u64,
    requested_lanes: Vec<wire::CanonicalUuid>,
    requested_lanes_truncated: bool,
    recent_heads: Vec<wire::rooms::DesignMaturityHead>,
    older_heads: wire::rooms::DesignMaturityOlder,
}

#[expect(
    dead_code,
    reason = "the complete legacy shape is validated by deserialization"
)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Schema3ItemSeries {
    item: wire::rooms::StatementItem,
    charges: u64,
    positive: bool,
    negative: bool,
    suffix_positive: bool,
    suffix_lanes: Vec<wire::CanonicalUuid>,
    suffix_strong: bool,
    suffix_conceded: bool,
    displaced_strong: bool,
    last_strong: bool,
    lanes: Vec<wire::rooms::DesignItemLane>,
    lanes_truncated: bool,
}

fn admit_schema_three_maturity(transaction: &Transaction<'_>) -> Result<(), OpenError> {
    let mut statement = transaction
        .prepare("SELECT design, body FROM design_maturity ORDER BY design")
        .map_err(OpenError::Sqlite)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .map_err(OpenError::Sqlite)?;
    for row in rows {
        let (design, body) = row.map_err(OpenError::Sqlite)?;
        serde_json::from_slice::<Schema3DesignMaturity>(&body)
            .map_err(|source| OpenError::InvalidDesignMaturity { design, source })?;
    }
    Ok(())
}
pub(super) fn cross(transaction: &Transaction<'_>) -> Result<(), OpenError> {
    admit_schema_three_maturity(transaction)?;
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
