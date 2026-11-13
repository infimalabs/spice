//! Current durable fact-envelope size enforcement.

use crate::MAX_DURABLE_FACT_ENVELOPE_BYTES;

pub(crate) const FACT_ENVELOPE_INSERT_TRIGGER: &str = "fact_envelopes_envelope_size_insert";
pub(crate) const FACT_ENVELOPE_UPDATE_TRIGGER: &str = "fact_envelopes_envelope_size_update";

pub(crate) fn insert_sql() -> String {
    format!(
        r#"
CREATE TRIGGER fact_envelopes_envelope_size_insert
BEFORE INSERT ON fact_envelopes
WHEN length(NEW.envelope) > {limit}
BEGIN
    SELECT RAISE(ABORT, 'fact envelope exceeds {limit} bytes');
END
"#,
        limit = MAX_DURABLE_FACT_ENVELOPE_BYTES,
    )
}

pub(crate) fn update_sql() -> String {
    format!(
        r#"
CREATE TRIGGER fact_envelopes_envelope_size_update
BEFORE UPDATE OF envelope ON fact_envelopes
WHEN length(NEW.envelope) > {limit}
BEGIN
    SELECT RAISE(ABORT, 'fact envelope exceeds {limit} bytes');
END
"#,
        limit = MAX_DURABLE_FACT_ENVELOPE_BYTES,
    )
}

pub(crate) fn bounds_sql() -> String {
    format!(
        "DROP TRIGGER IF EXISTS {FACT_ENVELOPE_INSERT_TRIGGER};\n\
         DROP TRIGGER IF EXISTS {FACT_ENVELOPE_UPDATE_TRIGGER};\n{};\n{};",
        insert_sql(),
        update_sql(),
    )
}
