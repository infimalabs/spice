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
"""

from __future__ import annotations

import sqlite3
from contextlib import contextmanager
from pathlib import Path
from typing import Iterator


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
