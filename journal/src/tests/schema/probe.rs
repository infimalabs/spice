//! Read-only admission and the stored-schema probe: each reports or refuses by
//! name without creating, repairing, or changing the store.

use super::*;

#[test]
fn read_only_handle_reports_schema_and_cursor_without_mutation() {
    let scratch = Scratch::new();
    let path = scratch.path().join("journal.sqlite3");
    drop(authority_file_fixture(&path));
    let before = FileState::read(&path);

    let reader = JournalReader::open(&path).expect("finalized authority opens read-only");

    assert_eq!(
        reader.schema_version().expect("read schema version"),
        SCHEMA_VERSION
    );
    assert_eq!(reader.cursor().expect("read cursor"), Some(seq(2)));
    let query_only: bool = reader
        .connection
        .query_row("PRAGMA query_only", [], |row| row.get(0))
        .expect("query-only posture");
    assert!(query_only);
    let write = reader
        .connection
        .execute("UPDATE cursor_head SET wake = seq WHERE singleton = 1", [])
        .expect_err("SQLite rejects a write through the reader");
    assert_eq!(
        write.sqlite_error_code(),
        Some(rusqlite::ErrorCode::ReadOnly)
    );
    drop(reader);

    assert_eq!(FileState::read(&path), before);

    let empty = scratch.path().join("empty.sqlite3");
    drop(Journal::open(&empty).expect("create empty finalized authority"));
    let before = FileState::read(&empty);

    let reader = JournalReader::open(&empty).expect("empty finalized authority opens read-only");

    assert_eq!(reader.cursor().expect("read empty cursor"), None);
    drop(reader);
    assert_eq!(FileState::read(&empty), before);
}

#[test]
fn read_only_admission_refuses_schema_states_without_mutation() {
    let scratch = Scratch::new();
    let cases = [
        (
            "prepared.sqlite3",
            1,
            "prepared",
            Some(0),
            Some(1),
            Some("plan:baseline"),
        ),
        (
            "newer.sqlite3",
            SCHEMA_VERSION + 1,
            "finalized",
            None,
            None,
            None,
        ),
        (
            "older.sqlite3",
            SCHEMA_VERSION - 1,
            "finalized",
            None,
            None,
            None,
        ),
    ];

    for (name, version, state, from, to, digest) in cases {
        let path = scratch.path().join(name);
        drop(schema_file_fixture(&path, version, state, from, to, digest));
        let before = FileState::read(&path);

        let error = JournalReader::open(&path).expect_err("schema state refuses read-only open");

        assert!(matches!(error, OpenError::Migration(_)));
        assert_eq!(FileState::read(&path), before, "{name} changed on refusal");
    }
}

#[test]
fn read_only_admission_never_creates_or_repairs_an_authority() {
    let scratch = Scratch::new();
    let absent = scratch.path().join("absent.sqlite3");
    let entries_before = directory_entries(scratch.path());

    let error = JournalReader::open(&absent).expect_err("absent authority refuses open");

    assert!(matches!(error, OpenError::Open { ref path, .. } if path == &absent));
    assert!(!absent.exists());
    assert_eq!(directory_entries(scratch.path()), entries_before);

    let empty = scratch.path().join("empty.sqlite3");
    drop(Connection::open(&empty).expect("create empty SQLite fixture"));
    let before = FileState::read(&empty);

    let error = JournalReader::open(&empty).expect_err("missing schema identity refuses");

    assert!(matches!(
        error,
        OpenError::Migration(MigrationError::MissingSchema)
    ));
    assert_eq!(FileState::read(&empty), before);

    let malformed = scratch.path().join("malformed.sqlite3");
    fs::write(&malformed, b"not a SQLite authority").expect("write malformed fixture");
    let before = FileState::read(&malformed);

    let error = JournalReader::open(&malformed).expect_err("malformed authority refuses open");

    assert!(matches!(error, OpenError::Sqlite(_)));
    assert_eq!(FileState::read(&malformed), before);
}

#[test]
fn stored_schema_probe_observes_finalized_versions_without_admitting_them() {
    for version in [SCHEMA_VERSION - 1, SCHEMA_VERSION + 1] {
        let scratch = Scratch::new();
        let path = scratch.path().join(format!("schema-{version}.db"));
        drop(schema_file_fixture(
            &path,
            version,
            "finalized",
            None,
            None,
            None,
        ));
        let before = directory_bytes(scratch.path());

        assert_eq!(
            Journal::inspect_stored_schema(&path).expect("stored version observes"),
            version
        );
        assert_eq!(directory_bytes(scratch.path()), before);
    }
}

#[test]
fn stored_schema_probe_still_refuses_unfinalized_state_without_mutation() {
    let scratch = Scratch::new();
    let path = scratch.path().join("prepared.db");
    drop(schema_file_fixture(
        &path,
        SCHEMA_VERSION - 1,
        "prepared",
        Some(SCHEMA_VERSION - 1),
        Some(SCHEMA_VERSION),
        Some("plan:upgrade"),
    ));
    let before = directory_bytes(scratch.path());

    let error = Journal::inspect_stored_schema(&path).expect_err("prepared state refuses");

    assert!(matches!(
        error,
        OpenError::Migration(MigrationError::UnknownState { .. })
    ));
    assert_eq!(directory_bytes(scratch.path()), before);
}
