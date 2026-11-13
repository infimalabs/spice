//! Cross an empty SQLite database into the initial Journal schema.

use rusqlite::Transaction;

use super::super::{
    BASELINE_SQL, COMMAND_REPLY_INDEX_SQL, OpenError, create_current_projections, fact_bounds,
};

pub(super) fn cross(transaction: &Transaction<'_>) -> Result<(), OpenError> {
    transaction
        .execute_batch(BASELINE_SQL)
        .map_err(OpenError::Sqlite)?;
    transaction
        .execute_batch(COMMAND_REPLY_INDEX_SQL)
        .map_err(OpenError::Sqlite)?;
    create_current_projections(transaction)?;
    transaction
        .execute_batch(&fact_bounds::bounds_sql())
        .map_err(OpenError::Sqlite)?;
    Ok(())
}
