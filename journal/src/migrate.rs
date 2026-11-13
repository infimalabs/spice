use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::SCHEMA_VERSION;

mod crossing;
mod fact_bounds;

#[cfg(test)]
pub(crate) use crossing::STEPS as CROSSING_STEPS;

#[cfg(test)]
pub(crate) use fact_bounds::bounds_sql;
pub(crate) use fact_bounds::{
    FACT_ENVELOPE_INSERT_TRIGGER, FACT_ENVELOPE_UPDATE_TRIGGER, insert_sql, update_sql,
};
const FINALIZED: &str = "finalized";

const COMMAND_REPLY_INDEX_SQL: &str = "CREATE INDEX IF NOT EXISTS command_receipts_operation \
     ON command_receipts(command_kind, actor, first_seq DESC, command_id, last_seq);";

/// The principal head index created with every current baseline.
const PRINCIPAL_HEADS_SQL: &str = "CREATE TABLE principal_heads (
    principal TEXT PRIMARY KEY,
    head_seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    body BLOB NOT NULL
) STRICT;";

/// Claims, their index, and their leases keyed by claim identity, and each
/// task's completion head; created with every current baseline.
const CLAIM_SCHEMA_SQL: &str = "CREATE TABLE completion_heads (
    stamp TEXT PRIMARY KEY REFERENCES task_heads(stamp),
    head_seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    body BLOB NOT NULL
) STRICT;
CREATE TABLE claim_heads (
    stamp TEXT NOT NULL REFERENCES task_heads(stamp),
    claimed INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    head_seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    body BLOB NOT NULL,
    PRIMARY KEY (stamp, claimed)
) STRICT;
CREATE TABLE claim_index (
    stamp TEXT NOT NULL,
    claimed INTEGER NOT NULL,
    actor TEXT NOT NULL,
    lane TEXT NOT NULL,
    disposition TEXT NOT NULL CHECK (disposition IN ('active', 'freed', 'lapsed')),
    head_seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    PRIMARY KEY (stamp, claimed),
    FOREIGN KEY (stamp, claimed) REFERENCES claim_heads(stamp, claimed)
) STRICT;
CREATE INDEX claim_index_actor_disposition_stamp
    ON claim_index(actor, disposition, stamp, claimed, lane, head_seq);
CREATE INDEX claim_index_disposition_actor
    ON claim_index(disposition, actor);
CREATE INDEX claim_index_disposition_lane_stamp
    ON claim_index(disposition, lane, stamp, claimed, head_seq);
CREATE TABLE lease_heads (
    stamp TEXT NOT NULL,
    claimed INTEGER NOT NULL,
    head_seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    body BLOB NOT NULL,
    PRIMARY KEY (stamp, claimed),
    FOREIGN KEY (stamp, claimed) REFERENCES claim_heads(stamp, claimed)
) STRICT;";

/// Admit exact-current authority, cross older authority forward, or
/// initialize one truly empty database.
///
/// Existing authority is classified without a transaction so a retired file
/// refuses before SQLite can create a rollback journal or change its persistent
/// journal mode. Empty initialization and crossing take the exclusive
/// transaction, then recheck what they observed to close the race with another
/// opener.
pub(crate) fn admit(connection: &mut Connection) -> Result<Option<Migration>, OpenError> {
    let trusted_schema: bool = connection
        .query_row("PRAGMA trusted_schema", [], |row| row.get(0))
        .map_err(OpenError::Sqlite)?;
    if trusted_schema {
        return Err(OpenError::TrustedSchemaEnabled);
    }

    if table_exists(connection, "journal_schema")? {
        let version = admit_readable_state(connection, stored_state(connection)?)?;
        if crossing::crosses(version) {
            return cross(connection);
        }
        admit_version(version)?;
        return Ok(None);
    }
    let object_count = authority_object_count(connection)?;
    if object_count != 0 {
        return Err(MigrationError::UnversionedAuthority { object_count }.into());
    }

    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Exclusive)
        .map_err(OpenError::Sqlite)?;
    if table_exists(&transaction, "journal_schema")? {
        admit_state(&transaction, stored_state(&transaction)?)?;
        transaction.commit().map_err(OpenError::Sqlite)?;
        return Ok(None);
    }
    let object_count = authority_object_count(&transaction)?;
    if object_count != 0 {
        return Err(MigrationError::UnversionedAuthority { object_count }.into());
    }

    crossing::cross(&transaction, 0)?;
    transaction
        .execute(
            "INSERT INTO journal_schema(singleton, version, state) VALUES (1, ?1, ?2)",
            params![SCHEMA_VERSION, FINALIZED],
        )
        .map_err(OpenError::Sqlite)?;
    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(OpenError::Sqlite)?;
    transaction.commit().map_err(OpenError::Sqlite)?;
    Ok(Some(Migration {
        from: 0,
        to: SCHEMA_VERSION,
    }))
}

/// Cross finalized older authority to the current schema in one exclusive
/// transaction, see [`crossing`].
fn cross(connection: &mut Connection) -> Result<Option<Migration>, OpenError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Exclusive)
        .map_err(OpenError::Sqlite)?;
    let version = admit_readable_state(&transaction, stored_state(&transaction)?)?;
    if !crossing::crosses(version) {
        admit_version(version)?;
        transaction.commit().map_err(OpenError::Sqlite)?;
        return Ok(None);
    }
    crossing::cross(&transaction, version)?;
    transaction
        .execute(
            "UPDATE journal_schema SET version = ?1 WHERE singleton = 1",
            params![SCHEMA_VERSION],
        )
        .map_err(OpenError::Sqlite)?;
    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(OpenError::Sqlite)?;
    transaction.commit().map_err(OpenError::Sqlite)?;
    Ok(Some(Migration {
        from: version,
        to: SCHEMA_VERSION,
    }))
}

/// Current-schema initialization or crossing completed by this open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Migration {
    from: u32,
    to: u32,
}

impl Migration {
    /// Schema identity before this open: `0` for initialization.
    #[must_use]
    pub const fn from(self) -> u32 {
        self.from
    }

    /// Current schema this open reached.
    #[must_use]
    pub const fn to(self) -> u32 {
        self.to
    }
}

pub(crate) fn finalized_version(connection: &Connection) -> Result<u32, OpenError> {
    admit_state(connection, stored_state(connection)?)
}

fn stored_state(connection: &Connection) -> Result<SchemaState, OpenError> {
    let count: u32 = connection
        .query_row("SELECT count(*) FROM journal_schema", [], |row| row.get(0))
        .map_err(OpenError::Sqlite)?;
    if count != 1 {
        return Err(MigrationError::SchemaRowCount { count }.into());
    }

    let state = connection
        .query_row(
            "SELECT version, state FROM journal_schema WHERE singleton = 1",
            [],
            |row| {
                Ok(SchemaState {
                    version: row.get(0)?,
                    state: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(OpenError::Sqlite)?
        .ok_or(MigrationError::MissingSingleton)?;
    Ok(state)
}

/// Reads an existing schema identity without performing baseline or migration.
pub(crate) fn inspect_version(connection: &Connection) -> Result<u32, OpenError> {
    if !table_exists(connection, "journal_schema")? {
        let object_count = authority_object_count(connection)?;
        return Err(if object_count == 0 {
            MigrationError::MissingSchema
        } else {
            MigrationError::UnversionedAuthority { object_count }
        }
        .into());
    }
    finalized_version(connection)
}

/// Observe a finalized stored schema identity without admitting its version.
///
/// Upgrade orchestration needs the exact before/after version even when this
/// build would refuse to read that version's authority. The identity still has
/// to be finalized and agree with SQLite's independent version record.
pub(crate) fn inspect_stored_version(connection: &Connection) -> Result<u32, OpenError> {
    if !table_exists(connection, "journal_schema")? {
        let object_count = authority_object_count(connection)?;
        return Err(if object_count == 0 {
            MigrationError::MissingSchema
        } else {
            MigrationError::UnversionedAuthority { object_count }
        }
        .into());
    }
    finalized_stored_state(connection, stored_state(connection)?)
}

/// Admit a finalized authority at the sole executable schema.
fn admit_state(connection: &Connection, state: SchemaState) -> Result<u32, OpenError> {
    admit_version(admit_readable_state(connection, state)?)
}

fn admit_version(version: u32) -> Result<u32, OpenError> {
    if version != SCHEMA_VERSION {
        return Err(MigrationError::UnknownVersion { found: version }.into());
    }
    Ok(version)
}

/// Create every projection outside the core baseline for a new authority.
fn create_current_projections(transaction: &rusqlite::Transaction<'_>) -> Result<(), OpenError> {
    for sql in [
        crate::rooms::SCHEMA_SQL,
        crate::rooms::CURSOR_SCHEMA_SQL,
        crate::rooms::CONVERSATION_SCHEMA_SQL,
        crate::rooms::MATURITY_SCHEMA_SQL,
        crate::rooms::OOPS_GENERATION_SCHEMA_SQL,
        PRINCIPAL_HEADS_SQL,
        crate::topic::SCHEMA_SQL,
        crate::topic::metadata::HISTORY_SQL,
        crate::topic::metadata::REVISION_SQL,
        CLAIM_SCHEMA_SQL,
        crate::peer_lines::SCHEMA_SQL,
        crate::peer_lines::PAIR_SCHEMA_SQL,
    ] {
        transaction.execute_batch(sql).map_err(OpenError::Sqlite)?;
    }
    Ok(())
}

/// Validate a finalized schema identity before applying the current cutoff.
///
/// Version disagreement and forward authority are corruption/compatibility
/// failures distinct from retired authority. They are classified here before
/// [`admit_state`] applies the exact-current requirement.
fn admit_readable_state(connection: &Connection, state: SchemaState) -> Result<u32, OpenError> {
    let version = finalized_stored_state(connection, state)?;
    if version > SCHEMA_VERSION {
        return Err(MigrationError::ForwardSchema {
            found: version,
            current: SCHEMA_VERSION,
        }
        .into());
    }

    Ok(version)
}

fn finalized_stored_state(connection: &Connection, state: SchemaState) -> Result<u32, OpenError> {
    if state.state != FINALIZED {
        return Err(MigrationError::UnknownState { state: state.state }.into());
    }

    let pragma: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(OpenError::Sqlite)?;
    if pragma != state.version {
        return Err(MigrationError::VersionDisagreement {
            row: state.version,
            pragma,
        }
        .into());
    }
    Ok(state.version)
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool, OpenError> {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            params![table],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
        .map_err(OpenError::Sqlite)
}

fn authority_object_count(connection: &Connection) -> Result<u32, OpenError> {
    connection
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )
        .map_err(OpenError::Sqlite)
}

struct SchemaState {
    version: u32,
    state: String,
}

/// A database open, schema admission, or SQLite operation failure.
#[derive(Debug)]
pub enum OpenError {
    Open {
        path: PathBuf,
        source: rusqlite::Error,
    },
    Sqlite(rusqlite::Error),
    Migration(MigrationError),
    /// Schema admission was attempted before disabling trusted schema objects.
    TrustedSchemaEnabled,
    InvalidCursor {
        found: i64,
    },
    /// The on-disk authority would not enter write-ahead-log mode.
    JournalMode {
        found: String,
    },
}

impl fmt::Display for OpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open { path, source } => {
                write!(
                    formatter,
                    "journal could not open {}: {source}",
                    path.display()
                )
            }
            Self::Sqlite(error) => write!(formatter, "journal SQLite operation failed: {error}"),
            Self::Migration(error) => error.fmt(formatter),
            Self::TrustedSchemaEnabled => {
                formatter.write_str("journal schema admission requires PRAGMA trusted_schema = OFF")
            }
            Self::InvalidCursor { found } => {
                write!(
                    formatter,
                    "journal cursor must be a positive position, found {found}"
                )
            }
            Self::JournalMode { found } => {
                write!(
                    formatter,
                    "journal authority must run in write-ahead-log mode, found {found:?}"
                )
            }
        }
    }
}

impl Error for OpenError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Open { source, .. } => Some(source),
            Self::Sqlite(error) => Some(error),
            Self::Migration(error) => Some(error),
            Self::InvalidCursor { .. } | Self::JournalMode { .. } | Self::TrustedSchemaEnabled => {
                None
            }
        }
    }
}

impl From<MigrationError> for OpenError {
    fn from(error: MigrationError) -> Self {
        Self::Migration(error)
    }
}

/// A named schema-state refusal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MigrationError {
    MissingSchema,
    UnversionedAuthority {
        object_count: u32,
    },
    SchemaRowCount {
        count: u32,
    },
    MissingSingleton,
    UnknownState {
        state: String,
    },
    ForwardSchema {
        found: u32,
        current: u32,
    },
    /// Authority older than this build's sole executable schema.
    UnknownVersion {
        found: u32,
    },
    VersionDisagreement {
        row: u32,
        pragma: u32,
    },
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSchema => formatter.write_str(
                "journal has no schema identity; release 1 requires a finalized schema before read-only inspection",
            ),
            Self::UnversionedAuthority { object_count } => write!(
                formatter,
                "journal refuses an unversioned nonempty authority containing {object_count} schema objects; release 1 requires backup and an empty authority"
            ),
            Self::SchemaRowCount { count } => write!(
                formatter,
                "journal schema identity requires exactly one row, found {count}"
            ),
            Self::MissingSingleton => {
                formatter.write_str("journal schema singleton row 1 is missing")
            }
            Self::UnknownState { state } => write!(
                formatter,
                "journal schema state {state:?} is unknown to release 1"
            ),
            Self::ForwardSchema { found, current } => write!(
                formatter,
                "journal schema {found} is newer than this build's schema {current}; use the release that owns schema {found}"
            ),
            Self::UnknownVersion { found } => write!(
                formatter,
                "journal schema {found} has no crossing to schema {SCHEMA_VERSION}; this build \
                 opens only a fresh journal"
            ),
            Self::VersionDisagreement { row, pragma } => write!(
                formatter,
                "journal schema row names version {row}, but SQLite user_version names {pragma}"
            ),
        }
    }
}

impl Error for MigrationError {}

const BASELINE_SQL: &str = r#"
CREATE TABLE journal_schema (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    version INTEGER NOT NULL CHECK (version > 0),
    state TEXT NOT NULL CHECK (state IN ('prepared', 'finalized')),
    from_version INTEGER,
    to_version INTEGER,
    plan_digest TEXT,
    CHECK (
        (state = 'finalized' AND from_version IS NULL AND to_version IS NULL AND plan_digest IS NULL)
        OR
        (state = 'prepared' AND to_version IS NOT NULL AND plan_digest IS NOT NULL)
    )
) STRICT;

CREATE TABLE fact_envelopes (
    seq INTEGER PRIMARY KEY CHECK (seq > 0),
    wire_version INTEGER NOT NULL CHECK (wire_version > 0),
    instant TEXT NOT NULL,
    actor TEXT NOT NULL,
    origin TEXT NOT NULL,
    cause INTEGER REFERENCES fact_envelopes(seq) DEFERRABLE INITIALLY DEFERRED,
    kind TEXT NOT NULL,
    envelope BLOB NOT NULL,
    CHECK (cause IS NULL OR cause < seq)
) STRICT;
CREATE INDEX fact_envelopes_origin_seq ON fact_envelopes(origin, seq);
CREATE INDEX fact_envelopes_cause ON fact_envelopes(cause) WHERE cause IS NOT NULL;
CREATE INDEX fact_envelopes_kind_seq ON fact_envelopes(kind, seq);
CREATE INDEX fact_envelopes_origin_kind_seq ON fact_envelopes(origin, kind, seq);

CREATE TABLE thread_index (
    seq INTEGER PRIMARY KEY REFERENCES fact_envelopes(seq) ON DELETE RESTRICT,
    origin TEXT NOT NULL,
    cause INTEGER REFERENCES fact_envelopes(seq) ON DELETE RESTRICT,
    root INTEGER NOT NULL REFERENCES fact_envelopes(seq) ON DELETE RESTRICT,
    depth INTEGER NOT NULL CHECK (depth >= 0),
    CHECK ((cause IS NULL AND root = seq AND depth = 0) OR cause IS NOT NULL)
) STRICT;
CREATE INDEX thread_index_origin_seq ON thread_index(origin, seq);
CREATE INDEX thread_index_root_seq ON thread_index(root, seq);

CREATE TABLE command_receipts (
    command_id TEXT PRIMARY KEY,
    actor TEXT NOT NULL,
    command_kind TEXT NOT NULL,
    request BLOB NOT NULL,
    evaluated_witnesses BLOB NOT NULL,
    response BLOB NOT NULL,
    first_seq INTEGER REFERENCES fact_envelopes(seq),
    last_seq INTEGER REFERENCES fact_envelopes(seq),
    snapshot_seq INTEGER REFERENCES fact_envelopes(seq),
    epoch TEXT NOT NULL,
    CHECK (
        (first_seq IS NOT NULL AND last_seq IS NOT NULL AND snapshot_seq IS NULL AND first_seq <= last_seq)
        OR
        (first_seq IS NULL AND last_seq IS NULL AND snapshot_seq IS NOT NULL)
    )
) STRICT;

CREATE TABLE task_heads (
    stamp TEXT PRIMARY KEY,
    head_seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    body BLOB NOT NULL
) STRICT;
CREATE TABLE release_landings (
    seq INTEGER PRIMARY KEY REFERENCES fact_envelopes(seq) ON DELETE RESTRICT,
    commit_id TEXT NOT NULL,
    task TEXT NOT NULL REFERENCES task_heads(stamp) ON DELETE RESTRICT,
    topic TEXT NOT NULL,
    title TEXT NOT NULL,
    contributes INTEGER NOT NULL CHECK (contributes IN (0, 1)),
    verdict TEXT CHECK (verdict IN ('clean', 'finding'))
) STRICT;
CREATE UNIQUE INDEX release_landings_contribution
    ON release_landings(commit_id) WHERE contributes = 1;
CREATE INDEX release_landings_commit_seq ON release_landings(commit_id, seq);
CREATE INDEX release_landings_task_seq ON release_landings(task, seq);
CREATE TABLE release_corrections (
    correction TEXT PRIMARY KEY REFERENCES task_heads(stamp) ON DELETE RESTRICT,
    parent TEXT NOT NULL REFERENCES task_heads(stamp) ON DELETE RESTRICT,
    head_seq INTEGER NOT NULL REFERENCES fact_envelopes(seq) ON DELETE RESTRICT,
    CHECK (correction <> parent)
) STRICT;
CREATE TABLE workflow_heads (
    stamp TEXT PRIMARY KEY REFERENCES task_heads(stamp),
    head_seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    body BLOB NOT NULL,
    revised_seq INTEGER REFERENCES fact_envelopes(seq)
) STRICT;
CREATE INDEX task_heads_head_seq ON task_heads(head_seq);
CREATE INDEX workflow_heads_head_seq ON workflow_heads(head_seq);
CREATE INDEX workflow_heads_revised_seq ON workflow_heads(revised_seq)
    WHERE revised_seq IS NOT NULL;
CREATE TABLE task_generations (
    stamp TEXT NOT NULL REFERENCES task_heads(stamp),
    generation INTEGER NOT NULL CHECK (generation > 0),
    seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    baseline INTEGER NOT NULL DEFAULT 0 CHECK (baseline IN (0, 1)),
    PRIMARY KEY (stamp, generation)
) STRICT, WITHOUT ROWID;
CREATE TABLE trust_heads (
    digest TEXT PRIMARY KEY,
    head_seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    body BLOB NOT NULL
) STRICT;
CREATE TABLE transcript_source_heads (
    stream TEXT PRIMARY KEY,
    lane TEXT NOT NULL,
    head_seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    body BLOB NOT NULL
) STRICT;
CREATE INDEX transcript_source_heads_lane_stream
    ON transcript_source_heads(lane, stream, head_seq);
CREATE TABLE steering_heads (
    directive TEXT PRIMARY KEY,
    steer_key TEXT NOT NULL UNIQUE,
    recipient TEXT NOT NULL,
    disposition TEXT NOT NULL,
    created_seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    minted_micros INTEGER NOT NULL,
    head_seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    body BLOB NOT NULL,
    room TEXT
) STRICT;
CREATE INDEX steering_heads_recipient_disposition_created
    ON steering_heads(recipient, disposition, created_seq);
CREATE INDEX steering_heads_disposition_created
    ON steering_heads(disposition, created_seq);
CREATE INDEX steering_heads_minted_micros
    ON steering_heads(minted_micros);
CREATE INDEX steering_heads_recipient_room_created
    ON steering_heads(recipient, room, created_seq);
CREATE TABLE steering_misses (
    identity TEXT PRIMARY KEY,
    head_seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    body BLOB NOT NULL
) STRICT;
CREATE TABLE steering_index (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    indexed_seq INTEGER REFERENCES fact_envelopes(seq)
) STRICT;
CREATE TABLE residency_head (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    head_seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    body BLOB NOT NULL
) STRICT;
CREATE TABLE cursor_head (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    seq INTEGER REFERENCES fact_envelopes(seq),
    wake INTEGER REFERENCES fact_envelopes(seq),
    CHECK (wake IS NULL OR (seq IS NOT NULL AND wake <= seq))
) STRICT;

CREATE TABLE taste_fires (
    seq INTEGER PRIMARY KEY REFERENCES fact_envelopes(seq),
    taste TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal > 0),
    UNIQUE(taste, ordinal)
) STRICT;
CREATE INDEX taste_fires_taste_seq ON taste_fires(taste, seq DESC);
CREATE TABLE taste_judgments (
    seq INTEGER PRIMARY KEY REFERENCES fact_envelopes(seq),
    taste TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal > 0),
    confirmed INTEGER NOT NULL CHECK (confirmed >= 0),
    cleared INTEGER NOT NULL CHECK (cleared >= 0),
    UNIQUE(taste, ordinal)
) STRICT;
CREATE INDEX taste_judgments_taste_seq ON taste_judgments(taste, seq DESC);
CREATE TABLE lane_seats (
    lane TEXT PRIMARY KEY,
    vendor TEXT NOT NULL,
    seq INTEGER NOT NULL REFERENCES fact_envelopes(seq)
) STRICT;
CREATE TABLE lane_checkpoints (
    lane TEXT PRIMARY KEY,
    seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    turn_seq INTEGER REFERENCES fact_envelopes(seq)
) STRICT;

CREATE TRIGGER cursor_head_monotone
BEFORE UPDATE ON cursor_head
WHEN (OLD.seq IS NOT NULL AND (NEW.seq IS NULL OR NEW.seq < OLD.seq))
  OR (OLD.wake IS NOT NULL AND (NEW.wake IS NULL OR NEW.wake < OLD.wake))
BEGIN
    SELECT RAISE(ABORT, 'cursor head cannot move backward');
END;

INSERT INTO cursor_head(singleton, seq, wake) VALUES (1, NULL, NULL);
INSERT INTO steering_index(singleton, indexed_seq) VALUES (1, NULL);
"#;
