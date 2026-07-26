"""One deterministic owner for every short-lived SQLite connection.

Python's sqlite3.Connection context manager commits or rolls back but never
closes, so a store written as `with sqlite3.connect(path):` keeps its file
descriptor and any database lock alive until garbage collection happens to
run. Long-lived agents must not depend on collection timing for descriptor
release or database unlock: short-lived stores open their connection through
`sqlite_connection`, which commits on success, rolls back on failure, and
always closes. A store whose transaction boundary must interleave with other
work (the serve team store wakes watchers between commit and close) keeps its
own explicit try/finally owner instead.

Opening a connection is also when a store's schema would be established, and
running that DDL on every open is what makes a database contend with itself, so
`ensure_sqlite_schema_once` lives here too: the same module that decides how a
connection is opened decides how many times a database is built.
"""

from __future__ import annotations

import sqlite3
from contextlib import contextmanager
from pathlib import Path
from threading import Lock
from typing import Callable, Iterator


@contextmanager
def sqlite_connection(
    path: str | Path,
    *,
    busy_timeout_ms: int | None = None,
    wal: bool = False,
    ensure_parent: bool = False,
) -> Iterator[sqlite3.Connection]:
    """Yield a connection that commits on success and always closes."""
    if ensure_parent:
        Path(path).parent.mkdir(parents=True, exist_ok=True)
    connection = sqlite3.connect(path)
    try:
        if busy_timeout_ms is not None:
            connection.execute(f"PRAGMA busy_timeout = {int(busy_timeout_ms)}")
        if wal:
            connection.execute("PRAGMA journal_mode = WAL")
        yield connection
        connection.commit()
    except BaseException:
        connection.rollback()
        raise
    finally:
        connection.close()


# One record for every path any store has initialized, because what it answers
# is a fact about the file rather than about the caller asking: a path is one
# database, and one database has one schema owner. Two stores that shared a
# path would already be two schemas in one file, which is a defect in how the
# paths were assigned and not something this record should hide.
_SCHEMA_INIT_LOCK = Lock()
_INITIALIZED_PATHS: set[Path] = set()


def ensure_sqlite_schema_once(
    path: Path,
    *,
    busy_timeout_ms: int,
    initialize: Callable[[sqlite3.Connection], None],
    validate: Callable[[sqlite3.Connection], None] | None = None,
    wal_after_initialize: bool = False,
) -> None:
    """Run a database's DDL at most once per path per process.

    Idempotent DDL is not free: every sweep takes a write lock, and on the
    default rollback journal a reader's SHARED lock and that write lock collide
    on a promotion SQLite refuses to retry, so `busy_timeout` cannot wait it out
    and an ordinary read or commit raises "database is locked". Running the DDL
    once and opening WAL lets a reader and the single writer proceed together.

    The double-check around the lock keeps the hot path to one set membership
    test, and the path is recorded only after the DDL commits, so an
    initializer that raises leaves the next caller to try again rather than
    marking a database that was never built.

    WAL normally opens here, before the DDL. A versioned store can instead set
    ``wal_after_initialize`` so its initializer rejects an incompatible file
    before journal mode mutates it. Such a store can also supply ``validate``:
    cached paths then get a read-only compatibility check, catching a file
    replaced or advanced by another writer without repeating the DDL.
    """
    if path in _INITIALIZED_PATHS:
        if validate is not None:
            _validate_initialized_schema(
                path, busy_timeout_ms=busy_timeout_ms, validate=validate
            )
        return
    with _SCHEMA_INIT_LOCK:
        if path in _INITIALIZED_PATHS:
            if validate is not None:
                _validate_initialized_schema(
                    path, busy_timeout_ms=busy_timeout_ms, validate=validate
                )
            return
        with sqlite_connection(
            path,
            busy_timeout_ms=busy_timeout_ms,
            wal=not wal_after_initialize,
            ensure_parent=True,
        ) as connection:
            initialize(connection)
            if wal_after_initialize:
                connection.execute("PRAGMA journal_mode = WAL")
        _INITIALIZED_PATHS.add(path)


def _validate_initialized_schema(
    path: Path,
    *,
    busy_timeout_ms: int,
    validate: Callable[[sqlite3.Connection], None],
) -> None:
    with sqlite_connection(
        path,
        busy_timeout_ms=busy_timeout_ms,
    ) as connection:
        validate(connection)
