"""Deterministic SQLite connection ownership through sqlite_connection."""

from __future__ import annotations

import sqlite3
import threading

import pytest

from spice.sqliteconnection import sqlite_connection

CLOSED_DATABASE = "closed database"
CONFIGURED_BUSY_TIMEOUT_MS = 1500


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
