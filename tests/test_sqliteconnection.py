"""Deterministic SQLite connection ownership through sqlite_connection."""

from __future__ import annotations

import sqlite3
import threading

import pytest

from spice.sqliteconnection import ensure_sqlite_schema_once, sqlite_connection

CLOSED_DATABASE = "closed database"
CONFIGURED_BUSY_TIMEOUT_MS = 1500


def _table_names(path) -> list[str]:
    with sqlite_connection(path) as connection:
        rows = connection.execute(
            "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name"
        )
        return [row[0] for row in rows]


def _counting_initializer(runs: list, table: str = "entries"):
    """Build an initializer that records each time the helper actually runs it."""

    def initialize(connection: sqlite3.Connection) -> None:
        runs.append(table)
        connection.execute(f"CREATE TABLE {table} (value TEXT)")

    return initialize


def _entries(path) -> list[str]:
    with sqlite_connection(path) as connection:
        rows = connection.execute("SELECT value FROM entries ORDER BY value")
        return [row[0] for row in rows]


def _seed_entries(path, values: tuple[str, ...] = ()) -> None:
    with sqlite_connection(path, ensure_parent=True) as connection:
        connection.execute("CREATE TABLE entries (value TEXT)")
        connection.executemany(
            "INSERT INTO entries VALUES (?)", [(value,) for value in values]
        )


def test_write_commits_and_closes(tmp_path):
    path = tmp_path / "store" / "entries.sqlite3"

    with sqlite_connection(path, ensure_parent=True) as connection:
        connection.execute("CREATE TABLE entries (value TEXT)")
        connection.execute("INSERT INTO entries VALUES ('a')")

    with pytest.raises(sqlite3.ProgrammingError, match=CLOSED_DATABASE):
        connection.execute("SELECT 1")
    assert _entries(path) == ["a"]


def test_read_closes_after_success(tmp_path):
    path = tmp_path / "entries.sqlite3"
    _seed_entries(path, ("a",))

    with sqlite_connection(path) as connection:
        values = [row[0] for row in connection.execute("SELECT value FROM entries")]

    with pytest.raises(sqlite3.ProgrammingError, match=CLOSED_DATABASE):
        connection.execute("SELECT 1")
    assert values == ["a"]


def test_query_failure_rolls_back_and_closes(tmp_path):
    path = tmp_path / "entries.sqlite3"
    _seed_entries(path, ("a",))

    with pytest.raises(sqlite3.OperationalError, match="missing_table"):
        with sqlite_connection(path) as connection:
            connection.execute("INSERT INTO entries VALUES ('b')")
            connection.execute("SELECT * FROM missing_table")

    with pytest.raises(sqlite3.ProgrammingError, match=CLOSED_DATABASE):
        connection.execute("SELECT 1")
    assert _entries(path) == ["a"]


def test_schema_failure_closes(tmp_path):
    path = tmp_path / "entries.sqlite3"
    _seed_entries(path)

    with pytest.raises(sqlite3.OperationalError, match="already exists"):
        with sqlite_connection(path) as connection:
            connection.executescript("CREATE TABLE entries (value TEXT)")

    with pytest.raises(sqlite3.ProgrammingError, match=CLOSED_DATABASE):
        connection.execute("SELECT 1")


def test_caller_error_rolls_back_and_closes(tmp_path):
    path = tmp_path / "entries.sqlite3"
    _seed_entries(path, ("a",))

    with pytest.raises(RuntimeError, match="caller stops mid-transaction"):
        with sqlite_connection(path) as connection:
            connection.execute("INSERT INTO entries VALUES ('b')")
            raise RuntimeError("caller stops mid-transaction")

    with pytest.raises(sqlite3.ProgrammingError, match=CLOSED_DATABASE):
        connection.execute("SELECT 1")
    assert _entries(path) == ["a"]


def test_repeated_concurrent_access_commits_and_closes_every_owner(tmp_path):
    path = tmp_path / "entries.sqlite3"
    _seed_entries(path)
    # SQLite connections are thread-affine, so each worker proves its own
    # connection closed and reports the observed error message.
    closed_messages: dict[int, str] = {}

    def write_entry(index: int) -> None:
        with sqlite_connection(path, busy_timeout_ms=5000) as connection:
            connection.execute("INSERT INTO entries VALUES (?)", (f"t{index}",))
        try:
            connection.execute("SELECT 1")
        except sqlite3.ProgrammingError as error:
            closed_messages[index] = str(error)

    threads = [
        threading.Thread(target=write_entry, args=(index,)) for index in range(5)
    ]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()

    assert _entries(path) == [f"t{index}" for index in range(5)]
    assert sorted(closed_messages) == list(range(5))
    assert all(CLOSED_DATABASE in message for message in closed_messages.values())


def test_pragmas_apply_before_the_caller_runs(tmp_path):
    path = tmp_path / "nested" / "entries.sqlite3"

    with sqlite_connection(
        path,
        busy_timeout_ms=CONFIGURED_BUSY_TIMEOUT_MS,
        wal=True,
        ensure_parent=True,
    ) as connection:
        busy_timeout = connection.execute("PRAGMA busy_timeout").fetchone()[0]
        journal_mode = connection.execute("PRAGMA journal_mode").fetchone()[0]

    assert busy_timeout == CONFIGURED_BUSY_TIMEOUT_MS
    assert journal_mode == "wal"
    assert path.parent.is_dir()


def test_schema_ddl_runs_once_per_path(tmp_path):
    path = tmp_path / "entries.sqlite3"
    runs: list[str] = []

    for _ in range(3):
        ensure_sqlite_schema_once(
            path,
            busy_timeout_ms=CONFIGURED_BUSY_TIMEOUT_MS,
            initialize=_counting_initializer(runs),
        )

    assert runs == ["entries"]
    assert _table_names(path) == ["entries"]


def test_each_database_path_is_initialized_on_its_own(tmp_path):
    """Two unrelated stores each get their DDL; the memo is per path, not global."""
    first = tmp_path / "first.sqlite3"
    second = tmp_path / "second.sqlite3"
    runs: list[str] = []

    ensure_sqlite_schema_once(
        first,
        busy_timeout_ms=CONFIGURED_BUSY_TIMEOUT_MS,
        initialize=_counting_initializer(runs, "first_entries"),
    )
    ensure_sqlite_schema_once(
        second,
        busy_timeout_ms=CONFIGURED_BUSY_TIMEOUT_MS,
        initialize=_counting_initializer(runs, "second_entries"),
    )

    assert runs == ["first_entries", "second_entries"]
    assert _table_names(first) == ["first_entries"]
    assert _table_names(second) == ["second_entries"]
    assert _table_names(first) != _table_names(second)


def test_the_initializing_connection_opens_wal_under_a_created_parent(tmp_path):
    path = tmp_path / "nested" / "entries.sqlite3"
    observed: list[str] = []

    def initialize(connection: sqlite3.Connection) -> None:
        observed.append(connection.execute("PRAGMA journal_mode").fetchone()[0])
        connection.execute("CREATE TABLE entries (value TEXT)")

    ensure_sqlite_schema_once(
        path, busy_timeout_ms=CONFIGURED_BUSY_TIMEOUT_MS, initialize=initialize
    )

    assert observed == ["wal"]
    assert path.parent.is_dir()
    with sqlite_connection(path) as connection:
        assert connection.execute("PRAGMA journal_mode").fetchone()[0] == "wal"


def test_a_failed_initializer_leaves_the_path_for_the_next_caller(tmp_path):
    """A database that was never built must not be recorded as initialized."""
    path = tmp_path / "entries.sqlite3"
    runs: list[str] = []

    def failing(connection: sqlite3.Connection) -> None:
        runs.append("failed")
        raise RuntimeError("schema build stops partway")

    with pytest.raises(RuntimeError, match="schema build stops partway"):
        ensure_sqlite_schema_once(
            path, busy_timeout_ms=CONFIGURED_BUSY_TIMEOUT_MS, initialize=failing
        )
    ensure_sqlite_schema_once(
        path,
        busy_timeout_ms=CONFIGURED_BUSY_TIMEOUT_MS,
        initialize=_counting_initializer(runs),
    )

    assert runs == ["failed", "entries"]
    assert _table_names(path) == ["entries"]
