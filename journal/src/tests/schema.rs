//! Schema identity, admission, and the read-only probe.
//!
//! Admission accepts only the current schema and otherwise answers by name
//! without mutating refused authority.

use super::*;

mod probe;
mod topic_metadata;

#[test]
fn current_schema_is_the_only_executable_journal_shape() {
    let mut journal = Journal::in_memory().expect("empty journal creates the current schema");
    let migration = journal
        .migration()
        .expect("empty journal records its baseline");
    assert_eq!((migration.from(), migration.to()), (0, SCHEMA_VERSION));
    assert_eq!(
        journal.schema_version().expect("schema version"),
        SCHEMA_VERSION
    );
    assert_eq!(
        crate::migrate::admit(&mut journal.connection).expect("baseline is idempotent"),
        None
    );
    let verified = journal.verify().expect("empty baseline verifies");
    assert_eq!(
        (
            verified.facts,
            verified.receipts,
            verified.heads,
            verified.threads
        ),
        (0, 0, 0, 0)
    );
}

#[test]
fn schema_admission_requires_trusted_schema_to_be_disabled_before_inspection() {
    let mut connection = Connection::open_in_memory().expect("open empty fixture");
    let trusted: bool = connection
        .query_row("PRAGMA trusted_schema", [], |row| row.get(0))
        .expect("default trust posture reads");
    assert!(trusted);

    let error = crate::migrate::admit(&mut connection)
        .expect_err("unhardened connection refuses schema inspection");

    assert!(matches!(error, OpenError::TrustedSchemaEnabled));
    let objects: u32 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )
        .expect("empty fixture remains inspectable");
    assert_eq!(objects, 0, "refusal initialized no schema objects");

    connection
        .pragma_update(None, "trusted_schema", false)
        .expect("disable trusted schema");
    let migration = crate::migrate::admit(&mut connection)
        .expect("hardened empty connection initializes")
        .expect("initialization is recorded");
    assert_eq!((migration.from(), migration.to()), (0, SCHEMA_VERSION));
}

/// Every schema retains the complete crossing chain from empty schema zero.
#[test]
fn crossing_steps_reach_the_current_schema_one_version_at_a_time() {
    let steps = crate::migrate::CROSSING_STEPS;
    let Some((first, _)) = steps.first() else {
        panic!("schema {SCHEMA_VERSION} needs its zero-to-one crossing");
    };
    assert_eq!(*first, 0);
    let Some((last, _)) = steps.last() else {
        unreachable!("the registry has a first step");
    };
    assert_eq!(*last + 1, SCHEMA_VERSION);
    for pair in steps.windows(2) {
        assert_eq!(pair[0].0 + 1, pair[1].0);
    }
}

#[test]
fn forward_schema_is_refused_by_name() {
    let forward = schema_fixture(SCHEMA_VERSION + 1, "finalized", None, None, None);
    let error = Journal::from_connection(forward).expect_err("forward schema refuses open");
    assert!(matches!(
        error,
        OpenError::Migration(MigrationError::ForwardSchema {
            found,
            current: SCHEMA_VERSION
        }) if found == SCHEMA_VERSION + 1
    ));
}

#[test]
fn unknown_states_and_occupied_databases_are_refused_by_name() {
    for unknown in [
        schema_fixture(SCHEMA_VERSION, "migrating", None, None, None),
        schema_fixture(
            SCHEMA_VERSION,
            "prepared",
            Some(SCHEMA_VERSION - 1),
            Some(SCHEMA_VERSION),
            Some("plan:crossing"),
        ),
    ] {
        let error = Journal::from_connection(unknown).expect_err("unknown state refuses open");
        assert!(matches!(
            error,
            OpenError::Migration(MigrationError::UnknownState { .. })
        ));
    }

    let occupied = Connection::open_in_memory().expect("open occupied fixture");
    occupied
        .execute_batch("CREATE TABLE somebody_elses_table(id INTEGER PRIMARY KEY) STRICT;")
        .expect("occupy the fixture database");
    let error =
        Journal::from_connection(occupied).expect_err("an occupied database refuses baselining");
    assert!(matches!(
        error,
        OpenError::Migration(MigrationError::UnversionedAuthority { object_count: 1 })
    ));
}

#[test]
fn malformed_schema_identity_is_refused_by_name() {
    let duplicate = schema_fixture(SCHEMA_VERSION, "finalized", None, None, None);
    duplicate
        .execute(
            "INSERT INTO journal_schema(singleton, version, state) VALUES (2, ?1, 'finalized')",
            [SCHEMA_VERSION],
        )
        .expect("insert duplicate schema row");
    let error = Journal::from_connection(duplicate).expect_err("two schema rows refuse open");
    assert!(matches!(
        error,
        OpenError::Migration(MigrationError::SchemaRowCount { count: 2 })
    ));

    let missing = schema_fixture(SCHEMA_VERSION, "finalized", None, None, None);
    missing
        .execute("UPDATE journal_schema SET singleton = 2", [])
        .expect("move schema row away from singleton one");
    let error = Journal::from_connection(missing).expect_err("missing singleton refuses open");
    assert!(matches!(
        error,
        OpenError::Migration(MigrationError::MissingSingleton)
    ));

    let disagreement = schema_fixture(SCHEMA_VERSION, "finalized", None, None, None);
    disagreement
        .pragma_update(None, "user_version", 0)
        .expect("make pragma disagree with schema row");
    let error =
        Journal::from_connection(disagreement).expect_err("version disagreement refuses open");
    assert!(matches!(
        error,
        OpenError::Migration(MigrationError::VersionDisagreement {
            row: SCHEMA_VERSION,
            pragma: 0
        })
    ));
    assert!(error.to_string().contains(&format!(
        "row names version {SCHEMA_VERSION}, but SQLite user_version names 0"
    )));
}
