//! Write-ahead logging: what a file authority runs in, what its readers see,
//! and why a captured generation opens immutably while a live one never does.

use super::*;

#[test]
fn a_file_authority_runs_in_write_ahead_log_mode_and_an_in_memory_one_does_not() {
    let scratch = Scratch::new();
    let path = scratch.path().join("authority.sqlite3");
    let writer = Journal::open(&path).expect("empty file baselines at the current schema");
    assert_eq!(
        writer.migration().map(|step| (step.from(), step.to())),
        Some((0, SCHEMA_VERSION))
    );
    assert_eq!(
        writer.schema_version().expect("schema reads"),
        SCHEMA_VERSION
    );
    assert_eq!(journal_mode(&writer.connection), "wal");
    drop(writer);
    // The mode persists in the file, so a later open finds it without setting it.
    let reopened = Journal::open(&path).expect("reopen the file authority");
    assert_eq!(reopened.migration(), None);
    assert_eq!(journal_mode(&reopened.connection), "wal");
    let memory = Journal::in_memory().expect("open an in-memory authority");
    assert_eq!(journal_mode(&memory.connection), "memory");
}

#[test]
fn a_read_only_reader_follows_commits_the_writer_has_not_checkpointed() {
    let scratch = Scratch::new();
    let path = scratch.path().join("authority.sqlite3");
    let mut writer = authority_file_fixture(&path);
    let mut write = crate::JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:1kKRRsDPx".parse().expect("fixture origin"),
        instant(),
        wire::Producer::ResidentRecognition,
    );
    write
        .fact(
            text_fact("landed only in the write-ahead log".to_owned()),
            crate::FactCause::Existing(seq(2)),
        )
        .expect("fact builds");
    assert_eq!(writer.apply(write).expect("commit lands").last(), seq(3));
    let log = sidecar(&path, "-wal");
    assert!(
        fs::metadata(&log)
            .expect("the write-ahead log exists while the writer is open")
            .len()
            > 0,
        "the commit did not land in the write-ahead log"
    );

    // A reader opened beside the live writer sees the commit the main file
    // does not yet contain.
    let live = JournalReader::open(&path).expect("open a reader beside the live writer");
    assert_eq!(
        live.fact_snapshot()
            .expect("snapshot beside the writer")
            .horizon(),
        Some(seq(3))
    );
    drop(live);
    drop(writer);
    assert!(
        !log.exists(),
        "closing the last connection did not fold the log back into the authority"
    );

    // A reader opened with no sidecars present at all still opens read-only.
    let settled = JournalReader::open(&path).expect("open a reader with no sidecars present");
    assert_eq!(
        settled
            .fact_snapshot()
            .expect("snapshot after the writer closed")
            .horizon(),
        Some(seq(3))
    );
}

#[test]
fn an_immutable_open_cannot_see_a_live_log_so_it_is_reserved_for_captured_generations() {
    let scratch = Scratch::new();
    let path = scratch.path().join("authority.sqlite3");
    let mut writer = authority_file_fixture(&path);
    let mut write = crate::JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:1kKRRsDPx".parse().expect("fixture origin"),
        instant(),
        wire::Producer::ResidentRecognition,
    );
    write
        .fact(
            text_fact("waiting in the write-ahead log".to_owned()),
            crate::FactCause::Existing(seq(2)),
        )
        .expect("fact builds");
    assert_eq!(writer.apply(write).expect("commit lands").last(), seq(3));

    // The live reader follows the log. The immutable reader, by contract,
    // reads only the main file: initialization is present there, but every
    // fixture fact and the new commit are still in the log. That stale view is
    // why immutable opens are reserved for sealed captures with no log to miss.
    let live = JournalReader::open(&path).expect("open a live reader");
    assert_eq!(
        live.fact_snapshot().expect("live snapshot").horizon(),
        Some(seq(3))
    );
    let stale =
        JournalReader::open_immutable(&path).expect("immutable open reads only the main file");
    assert_eq!(
        stale
            .fact_snapshot()
            .expect("stale immutable snapshot")
            .horizon(),
        None
    );
    drop(stale);
    drop((live, writer));

    // Once the writer has folded the log back in, the two agree, and an
    // immutable open needs no sidecar the sealed capture directory cannot
    // hold.
    let settled = JournalReader::open_immutable(&path).expect("open the settled file immutably");
    assert_eq!(
        settled
            .fact_snapshot()
            .expect("settled immutable snapshot")
            .horizon(),
        Some(seq(3))
    );
}

fn journal_mode(connection: &Connection) -> String {
    connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("read the journal mode")
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}
