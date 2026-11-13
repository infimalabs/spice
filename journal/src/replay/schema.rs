//! Exact finalized schema objects verified without mutating authority.

use rusqlite::{Connection, OptionalExtension};

use super::{VerifyError, Violation, ViolationClass};

pub(super) fn verify_fact_envelope_bound_objects(
    connection: &Connection,
    violations: &mut Vec<Violation>,
) -> Result<(), VerifyError> {
    for (name, expected) in [
        (
            crate::migrate::FACT_ENVELOPE_INSERT_TRIGGER,
            crate::migrate::insert_sql(),
        ),
        (
            crate::migrate::FACT_ENVELOPE_UPDATE_TRIGGER,
            crate::migrate::update_sql(),
        ),
    ] {
        let found = connection
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type = 'trigger' AND name = ?1",
                [name],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(VerifyError::Sqlite)?;
        match found {
            None => violations.push(Violation::new(
                ViolationClass::Schema,
                name,
                "durable fact-envelope size enforcement trigger is missing",
            )),
            Some(found) if normalize_sql(&found) != normalize_sql(&expected) => {
                violations.push(Violation::new(
                    ViolationClass::Schema,
                    name,
                    "durable fact-envelope size enforcement trigger differs from current schema",
                ));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

fn normalize_sql(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}
