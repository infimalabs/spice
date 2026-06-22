"""Durable ACK state for consumed inbox steering.

ACKing an inbox item records the consumed text here and removes the pending
file. The old filesystem archive is intentionally not the source of truth; this
SQLite store is the ACK history that agent rehydration and UI surfaces read.

The store lives with the other spice SQLite databases under the shared git
common dir (`git_common_dir/<SHARED_DIR>/data`, the same `data_dir()` that
holds the task backend and `spiceteams.sqlite3`), not in a per-worktree
`.spice/`. That keeps one ACK history per repository across every worktree.
"""

from __future__ import annotations

import json
import sqlite3
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

from spice.paths import git_common_dir
from spice.tasks.config import SHARED_DIR

ACK_STATE_DATABASE_FILENAME = "spiceacks.sqlite3"
# Mirrors task_config.data_dir() == backend_root() / "data"; the ack store is a
# sibling of the task backend db under the shared git common dir.
ACK_STATE_DATA_SUBDIR = "data"
ACK_STATE_SQLITE_BUSY_TIMEOUT_MS = 5000

ACK_STATE_TABLE_SQL = """
CREATE TABLE IF NOT EXISTS acked_inbox_items (
  key TEXT PRIMARY KEY,
  inbox_name TEXT NOT NULL,
  text TEXT NOT NULL,
  attachments_json TEXT NOT NULL DEFAULT '[]',
  ack_text TEXT NOT NULL DEFAULT '',
  ack_content TEXT NOT NULL DEFAULT '',
  archived_at REAL NOT NULL
);
"""
ACK_STATE_INDEX_SQL = """
CREATE INDEX IF NOT EXISTS acked_inbox_items_archived_at_idx
  ON acked_inbox_items(archived_at);
"""


@dataclass(frozen=True)
class AckStateRecord:
    key: str
    inbox_name: str
    text: str
    attachments: tuple[dict[str, Any], ...]
    ack_text: str
    ack_content: str
    archived_at: float


@dataclass(frozen=True)
class AckStateWrite:
    key: str
    inbox_name: str
    text: str
    attachments: tuple[dict[str, Any], ...] = ()
    ack_text: str = ""
    ack_content: str = ""


def ack_state_database_path(repo_root: str | Path) -> Path:
    common = git_common_dir(Path(repo_root))
    return common / SHARED_DIR / ACK_STATE_DATA_SUBDIR / ACK_STATE_DATABASE_FILENAME


def record_acked_inbox_items(
    repo_root: str | Path, items: Iterable[AckStateWrite], *, now: float | None = None
) -> list[str]:
    rows = [
        (
            item.key,
            item.inbox_name,
            item.text,
            json.dumps(list(item.attachments), sort_keys=True),
            item.ack_text,
            item.ack_content,
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
              (key, inbox_name, text, attachments_json, ack_text, ack_content,
               archived_at)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(key) DO UPDATE SET
              inbox_name=excluded.inbox_name,
              text=excluded.text,
              attachments_json=excluded.attachments_json,
              ack_text=excluded.ack_text,
              ack_content=excluded.ack_content,
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
            SELECT key, inbox_name, text, attachments_json, ack_text, ack_content,
                   archived_at
            FROM acked_inbox_items
            ORDER BY archived_at DESC, key DESC
            """
        ).fetchall()
    return [
        AckStateRecord(
            key=row[0],
            inbox_name=row[1],
            text=row[2],
            attachments=_decode_attachments_json(row[3]),
            ack_text=row[4],
            ack_content=row[5],
            archived_at=row[6],
        )
        for row in rows
    ]


def _ensure_schema(connection: sqlite3.Connection) -> None:
    connection.execute(f"PRAGMA busy_timeout = {ACK_STATE_SQLITE_BUSY_TIMEOUT_MS}")
    connection.execute(ACK_STATE_TABLE_SQL)
    _ensure_column(
        connection,
        "inbox_name",
        "ALTER TABLE acked_inbox_items ADD COLUMN inbox_name TEXT NOT NULL DEFAULT ''",
    )
    _ensure_column(
        connection,
        "text",
        "ALTER TABLE acked_inbox_items ADD COLUMN text TEXT NOT NULL DEFAULT ''",
    )
    _ensure_column(
        connection,
        "attachments_json",
        "ALTER TABLE acked_inbox_items "
        "ADD COLUMN attachments_json TEXT NOT NULL DEFAULT '[]'",
    )
    _ensure_column(
        connection,
        "ack_text",
        "ALTER TABLE acked_inbox_items ADD COLUMN ack_text TEXT NOT NULL DEFAULT ''",
    )
    _ensure_column(
        connection,
        "ack_content",
        "ALTER TABLE acked_inbox_items ADD COLUMN ack_content TEXT NOT NULL DEFAULT ''",
    )
    _ensure_column(
        connection,
        "archived_at",
        "ALTER TABLE acked_inbox_items ADD COLUMN archived_at REAL NOT NULL DEFAULT 0",
    )
    connection.execute(ACK_STATE_INDEX_SQL)


def _ensure_column(connection: sqlite3.Connection, column: str, statement: str) -> None:
    columns = {
        str(row[1])
        for row in connection.execute("PRAGMA table_info(acked_inbox_items)")
    }
    if column in columns:
        return
    connection.execute(statement)


def _decode_attachments_json(raw: str) -> tuple[dict[str, Any], ...]:
    try:
        parsed = json.loads(raw or "[]")
    except json.JSONDecodeError:
        return ()
    if not isinstance(parsed, list):
        return ()
    attachments = [item for item in parsed if isinstance(item, dict)]
    return tuple(attachments)
