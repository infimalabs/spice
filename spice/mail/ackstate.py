"""Durable ACK state for consumed inbox steering.

ACKing an inbox item records the consumed text here and removes the pending
file. The old filesystem archive is intentionally not the source of truth; this
SQLite store is the ACK history that agent rehydration and UI surfaces read.

The store lives with the other repository-owned SQLite databases under the
shared git common dir (`git_common_dir/.spice/data`), next to the default task
backend and `spiceteams.sqlite3`. It deliberately does not follow an explicit
task-backend override: ACK history belongs to the repository and is shared by
every worktree.
"""

from __future__ import annotations

import json
import sqlite3
import time
from dataclasses import dataclass
from pathlib import Path
from threading import Lock
from typing import Any, Iterable

from spice.paths import shared_state_path
from spice.sqliteconnection import sqlite_connection

ACK_STATE_DATABASE_FILENAME = "spiceacks.sqlite3"
# Mirrors the default task backend's `data` subdirectory. Unlike task/team
# state, this repository-owned store does not follow SPICE_TASK_BACKEND.
ACK_STATE_DATA_SUBDIR = "data"
ACK_STATE_SQLITE_BUSY_TIMEOUT_MS = 5000
ACK_DISPOSITION_ACKED = "acked"
ACK_DISPOSITION_REFUSED = "refused"
ACK_DISPOSITIONS = frozenset({ACK_DISPOSITION_ACKED, ACK_DISPOSITION_REFUSED})

ACK_STATE_TABLE_SQL = """
CREATE TABLE IF NOT EXISTS acked_inbox_items (
  key TEXT PRIMARY KEY,
  inbox_name TEXT NOT NULL,
  text TEXT NOT NULL,
  attachments_json TEXT NOT NULL DEFAULT '[]',
  lineage_json TEXT NOT NULL DEFAULT '{}',
  ack_text TEXT NOT NULL DEFAULT '',
  ack_content TEXT NOT NULL DEFAULT '',
  disposition TEXT NOT NULL DEFAULT 'acked',
  archived_at REAL NOT NULL
);
"""
ACK_STATE_INDEX_SQL = """
CREATE INDEX IF NOT EXISTS acked_inbox_items_archived_at_idx
  ON acked_inbox_items(archived_at);
"""

# Schema is initialized once per database path per process. Running the full
# DDL sweep (CREATE + PRAGMA table_info + ALTER) on every write took a write
# lock each time; under concurrent lanes that serialized all access -- reads
# included. In the default rollback journal a reader's SHARED lock and that
# write lock are mutually exclusive on a promotion SQLite refuses to retry, so
# `busy_timeout` could not wait it out and the ACK commit raised "database is
# locked". Initializing once and opening WAL (mirroring ServeTeamStore) lets a
# reader and the single writer proceed together.
_SCHEMA_INIT_LOCK = Lock()
_INITIALIZED_PATHS: set[Path] = set()


@dataclass(frozen=True)
class AckStateRecord:
    key: str
    inbox_name: str
    text: str
    attachments: tuple[dict[str, Any], ...]
    lineage: dict[str, Any]
    ack_text: str
    ack_content: str
    disposition: str
    archived_at: float


@dataclass(frozen=True)
class AckStateWrite:
    key: str
    inbox_name: str
    text: str
    attachments: tuple[dict[str, Any], ...] = ()
    lineage: dict[str, Any] | None = None
    ack_text: str = ""
    ack_content: str = ""
    disposition: str = ACK_DISPOSITION_ACKED


def ack_state_database_path(repo_root: str | Path) -> Path:
    return shared_state_path(
        Path(repo_root),
        Path(ACK_STATE_DATA_SUBDIR) / ACK_STATE_DATABASE_FILENAME,
    )


def record_acked_inbox_items(
    repo_root: str | Path, items: Iterable[AckStateWrite], *, now: float | None = None
) -> list[str]:
    rows = [
        (
            item.key,
            item.inbox_name,
            item.text,
            json.dumps(list(item.attachments), sort_keys=True),
            json.dumps(item.lineage or {}, sort_keys=True),
            item.ack_text,
            item.ack_content,
            _normalize_disposition(item.disposition),
            float(time.time() if now is None else now),
        )
        for item in items
    ]
    if not rows:
        return []
    path = ack_state_database_path(repo_root)
    _ensure_schema_once(path)
    with sqlite_connection(
        path,
        busy_timeout_ms=ACK_STATE_SQLITE_BUSY_TIMEOUT_MS,
        wal=True,
    ) as connection:
        connection.executemany(
            """
            INSERT INTO acked_inbox_items
              (key, inbox_name, text, attachments_json, lineage_json, ack_text,
               ack_content, disposition, archived_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(key) DO UPDATE SET
              inbox_name=excluded.inbox_name,
              text=excluded.text,
              attachments_json=excluded.attachments_json,
              lineage_json=excluded.lineage_json,
              ack_text=excluded.ack_text,
              ack_content=excluded.ack_content,
              disposition=excluded.disposition,
              archived_at=excluded.archived_at
            """,
            rows,
        )
    return [row[0] for row in rows]


def ack_state_records(repo_root: str | Path) -> list[AckStateRecord]:
    """Read archived steering without creating or migrating the schema.

    Schema creation and migration belong to :func:`record_acked_inbox_items`,
    the store's write boundary, so this reader never runs DDL and takes no
    write lock. It opens WAL -- the mode the writer persists -- so a read that
    overlaps a writer observes the last committed snapshot rather than
    contending on the writer's lock, mirroring ServeTeamStore's timeout-free
    readers. Steady-state readers share this database across every worktree.
    """
    path = ack_state_database_path(repo_root)
    if not path.is_file():
        return []
    with sqlite_connection(path, wal=True) as connection:
        rows = connection.execute(
            """
            SELECT key, inbox_name, text, attachments_json, lineage_json,
                   ack_text, ack_content, disposition, archived_at
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
            lineage=_decode_lineage_json(row[4]),
            ack_text=row[5],
            ack_content=row[6],
            disposition=_normalize_disposition(row[7]),
            archived_at=row[8],
        )
        for row in rows
    ]


def _ensure_schema_once(path: Path) -> None:
    """Create or migrate the schema at most once per database path per process.

    Guarded by a lock and a per-path record so the DDL never re-runs on the hot
    write path, and the initializing connection opens WAL so the journal mode
    persists for every later reader and writer. Mirrors ServeTeamStore.
    """
    if path in _INITIALIZED_PATHS:
        return
    with _SCHEMA_INIT_LOCK:
        if path in _INITIALIZED_PATHS:
            return
        with sqlite_connection(
            path,
            busy_timeout_ms=ACK_STATE_SQLITE_BUSY_TIMEOUT_MS,
            wal=True,
            ensure_parent=True,
        ) as connection:
            _ensure_schema(connection)
        _INITIALIZED_PATHS.add(path)


def _ensure_schema(connection: sqlite3.Connection) -> None:
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
        "lineage_json",
        "ALTER TABLE acked_inbox_items "
        "ADD COLUMN lineage_json TEXT NOT NULL DEFAULT '{}'",
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
        "disposition",
        "ALTER TABLE acked_inbox_items "
        "ADD COLUMN disposition TEXT NOT NULL DEFAULT 'acked'",
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


def _decode_lineage_json(raw: str) -> dict[str, Any]:
    try:
        parsed = json.loads(raw or "{}")
    except json.JSONDecodeError:
        return {}
    return parsed if isinstance(parsed, dict) else {}


def _normalize_disposition(value: str) -> str:
    clean = str(value or "").strip().lower()
    if clean in ACK_DISPOSITIONS:
        return clean
    return ACK_DISPOSITION_ACKED
