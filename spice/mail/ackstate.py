"""Durable ACK state for consumed inbox steering.

ACKing an inbox item records the consumed text here and removes the pending
file. The old filesystem archive is intentionally not the source of truth; this
SQLite store is the ACK history that agent rehydration and UI surfaces read.
"""

from __future__ import annotations

import sqlite3
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

from spice.paths import STATE_DIRNAME

ACK_STATE_DATABASE_FILENAME = "acks.sqlite3"
ACK_STATE_SQLITE_BUSY_TIMEOUT_MS = 5000

ACK_STATE_SCHEMA = """
CREATE TABLE IF NOT EXISTS acked_inbox_items (
  key TEXT PRIMARY KEY,
  inbox_name TEXT NOT NULL,
  text TEXT NOT NULL,
  archived_at REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS acked_inbox_items_archived_at_idx
  ON acked_inbox_items(archived_at);
"""


@dataclass(frozen=True)
class AckStateRecord:
    key: str
    inbox_name: str
    text: str
    archived_at: float


@dataclass(frozen=True)
class AckStateWrite:
    key: str
    inbox_name: str
    text: str


def ack_state_database_path(repo_root: str | Path) -> Path:
    return Path(repo_root) / STATE_DIRNAME / ACK_STATE_DATABASE_FILENAME


def record_acked_inbox_items(
    repo_root: str | Path, items: Iterable[AckStateWrite], *, now: float | None = None
) -> list[str]:
    rows = [
        (
            item.key,
            item.inbox_name,
            item.text,
            float(time.time() if now is None else now),
        )
        for item in items
    ]
    if not rows:
        return []
    path = ack_state_database_path(repo_root)
    path.parent.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(path) as connection:
        _ensure_schema(connection)
        connection.executemany(
            """
            INSERT INTO acked_inbox_items
              (key, inbox_name, text, archived_at)
            VALUES (?, ?, ?, ?)
            ON CONFLICT(key) DO UPDATE SET
              inbox_name=excluded.inbox_name,
              text=excluded.text,
              archived_at=excluded.archived_at
            """,
            rows,
        )
    return [row[0] for row in rows]


def ack_state_records(repo_root: str | Path) -> list[AckStateRecord]:
    path = ack_state_database_path(repo_root)
    if not path.is_file():
        return []
    with sqlite3.connect(path) as connection:
        _ensure_schema(connection)
        rows = connection.execute(
            """
            SELECT key, inbox_name, text, archived_at
            FROM acked_inbox_items
            ORDER BY archived_at DESC, key DESC
            """
        ).fetchall()
    return [AckStateRecord(*row) for row in rows]


def _ensure_schema(connection: sqlite3.Connection) -> None:
    connection.execute(f"PRAGMA busy_timeout = {ACK_STATE_SQLITE_BUSY_TIMEOUT_MS}")
    connection.executescript(ACK_STATE_SCHEMA)
