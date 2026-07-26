"""Canonical durable history for inbox steering and its ACK disposition.

Publishing a metric-bearing directive records its immutable target actor,
team-at-send, and send time here. ACKing or refusing that inbox item completes
the same keyed row with its disposition and auditable response content before
the pending file is removed. The filesystem is the delivery transport; this
SQLite store is the lifecycle history that metrics, rehydration, and UI
surfaces read.

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
from typing import Any, Iterable

from spice.errors import SpiceError
from spice.paths import shared_state_path
from spice.sqliteconnection import ensure_sqlite_schema_once, sqlite_connection

ACK_STATE_DATABASE_FILENAME = "spiceacks.sqlite3"
# Mirrors the default task backend's `data` subdirectory. Unlike task/team
# state, this repository-owned store does not follow SPICE_TASK_BACKEND.
ACK_STATE_DATA_SUBDIR = "data"
ACK_STATE_SQLITE_BUSY_TIMEOUT_MS = 5000
ACK_DISPOSITION_ACKED = "acked"
ACK_DISPOSITION_REFUSED = "refused"
ACK_DISPOSITION_PENDING = "pending"
ACK_DISPOSITIONS = frozenset(
    {ACK_DISPOSITION_ACKED, ACK_DISPOSITION_REFUSED, ACK_DISPOSITION_PENDING}
)
DIRECTIVE_PROVENANCE_ARCHIVE_ONLY = "archiveOnly"
DIRECTIVE_PROVENANCE_PUBLISHED = "published"
DIRECTIVE_PROVENANCES = frozenset(
    {
        DIRECTIVE_PROVENANCE_ARCHIVE_ONLY,
        DIRECTIVE_PROVENANCE_PUBLISHED,
    }
)

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
  archived_at REAL NOT NULL,
  target_actor TEXT NOT NULL DEFAULT '',
  team_id TEXT NOT NULL DEFAULT '',
  sent_at REAL,
  published_text TEXT NOT NULL DEFAULT '',
  acknowledged_at REAL,
  provenance TEXT NOT NULL DEFAULT 'archiveOnly'
);
"""
ACK_STATE_INDEX_SQL = """
CREATE INDEX IF NOT EXISTS acked_inbox_items_archived_at_idx
  ON acked_inbox_items(archived_at);
"""
ACK_STATE_RECORD_SELECT_SQL = """
SELECT key, inbox_name, text, attachments_json, lineage_json,
       ack_text, ack_content, disposition, archived_at
FROM acked_inbox_items
"""
DIRECTIVE_HISTORY_RECORD_SELECT_SQL = """
SELECT key, inbox_name, text, attachments_json, lineage_json,
       ack_text, ack_content, disposition, archived_at, target_actor,
       team_id, sent_at, published_text, acknowledged_at, provenance
FROM acked_inbox_items
"""


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


@dataclass(frozen=True)
class DirectivePublicationWrite:
    key: str
    inbox_name: str
    text: str
    target_actor: str
    team_id: str
    sent_at: float
    attachments: tuple[dict[str, Any], ...] = ()
    lineage: dict[str, Any] | None = None


@dataclass(frozen=True)
class DirectiveHistoryRecord:
    key: str
    inbox_name: str
    text: str
    published_text: str
    attachments: tuple[dict[str, Any], ...]
    lineage: dict[str, Any]
    target_actor: str
    team_id: str
    sent_at: float | None
    disposition: str
    acknowledged_at: float | None
    ack_text: str
    ack_content: str
    provenance: str


def ack_state_database_path(repo_root: str | Path) -> Path:
    return shared_state_path(
        Path(repo_root),
        Path(ACK_STATE_DATA_SUBDIR) / ACK_STATE_DATABASE_FILENAME,
    )


def record_acked_inbox_items(
    repo_root: str | Path, items: Iterable[AckStateWrite], *, now: float | None = None
) -> list[str]:
    writes = tuple(items)
    if not writes:
        return []
    when = float(time.time() if now is None else now)
    return record_acked_inbox_items_to_database(
        ack_state_database_path(repo_root), writes, now=when
    )


def record_acked_inbox_items_to_database(
    path: str | Path,
    items: Iterable[AckStateWrite],
    *,
    now: float | None = None,
) -> list[str]:
    writes = tuple(items)
    if not writes:
        return []
    when = float(time.time() if now is None else now)
    database_path = Path(path)
    _ensure_schema_once(database_path)
    with sqlite_connection(
        database_path,
        busy_timeout_ms=ACK_STATE_SQLITE_BUSY_TIMEOUT_MS,
        wal=True,
    ) as connection:
        for item in writes:
            _record_ack_locked(connection, item, acknowledged_at=when)
    return [item.key for item in writes]


def record_directive_publications(
    repo_root: str | Path,
    items: Iterable[DirectivePublicationWrite],
) -> list[str]:
    """Publish immutable metric provenance for one or more steering keys.

    Exact duplicate delivery is idempotent. Reusing a key for a different
    actor, team, timestamp, body, or attachment set is a hard collision because
    silently replacing any of those values would rewrite a historical fact.
    """
    writes = tuple(items)
    if not writes:
        return []
    return record_directive_publications_to_database(
        ack_state_database_path(repo_root), writes
    )


def record_directive_publications_to_database(
    path: str | Path,
    items: Iterable[DirectivePublicationWrite],
) -> list[str]:
    writes = tuple(items)
    if not writes:
        return []
    database_path = Path(path)
    _ensure_schema_once(database_path)
    with sqlite_connection(
        database_path,
        busy_timeout_ms=ACK_STATE_SQLITE_BUSY_TIMEOUT_MS,
        wal=True,
    ) as connection:
        for item in writes:
            _record_publication_locked(connection, item)
    return [item.key for item in writes]


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
            ACK_STATE_RECORD_SELECT_SQL
            + " WHERE disposition IN (?, ?) "
            + "ORDER BY archived_at DESC, key DESC",
            (ACK_DISPOSITION_ACKED, ACK_DISPOSITION_REFUSED),
        ).fetchall()
    return [_ack_state_record(row) for row in rows]


def ack_state_records_for_keys(
    repo_root: str | Path, keys: Iterable[str]
) -> list[AckStateRecord]:
    """Read the exact consumed steering rows named by ``keys``.

    Message hydration needs every context referenced by its bounded message
    window, even when a referenced key is older than the recent-history UI
    limit. The primary-key lookup keeps that read proportional to requested
    contexts instead of loading or truncating the archive.
    """
    wanted = tuple(dict.fromkeys(str(key) for key in keys if key))
    if not wanted:
        return []
    path = ack_state_database_path(repo_root)
    if not path.is_file():
        return []
    placeholders = ", ".join("?" for _key in wanted)
    with sqlite_connection(path, wal=True) as connection:
        rows = connection.execute(
            ACK_STATE_RECORD_SELECT_SQL
            + f" WHERE key IN ({placeholders})"
            + " AND disposition IN (?, ?)"
            + " ORDER BY archived_at DESC, key DESC",
            (*wanted, ACK_DISPOSITION_ACKED, ACK_DISPOSITION_REFUSED),
        ).fetchall()
    return [_ack_state_record(row) for row in rows]


def directive_history_records_from_database(
    path: str | Path,
) -> list[DirectiveHistoryRecord]:
    """Read canonical directive lifecycle facts without running schema DDL."""
    database_path = Path(path)
    if not database_path.is_file():
        return []
    with sqlite_connection(database_path, wal=True) as connection:
        rows = connection.execute(
            DIRECTIVE_HISTORY_RECORD_SELECT_SQL
            + " WHERE target_actor != '' ORDER BY sent_at, key"
        ).fetchall()
    return [_directive_history_record(row) for row in rows]


def prepare_directive_history_database(path: str | Path) -> None:
    """Upgrade an existing ACK database before Serve begins read projection."""
    database_path = Path(path)
    if database_path.is_file():
        _ensure_schema_once(database_path)


def _ensure_schema_once(path: Path) -> None:
    """Create or migrate the ACK schema at most once per path per process."""
    ensure_sqlite_schema_once(
        path,
        busy_timeout_ms=ACK_STATE_SQLITE_BUSY_TIMEOUT_MS,
        initialize=_ensure_schema,
    )


def _ack_state_record(row: tuple[Any, ...]) -> AckStateRecord:
    return AckStateRecord(
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


def _directive_history_record(row: tuple[Any, ...]) -> DirectiveHistoryRecord:
    record = DirectiveHistoryRecord(
        key=str(row[0]),
        inbox_name=str(row[1]),
        text=str(row[2]),
        attachments=_decode_attachments_json(str(row[3])),
        lineage=_decode_lineage_json(str(row[4])),
        ack_text=str(row[5]),
        ack_content=str(row[6]),
        disposition=_canonical_disposition(str(row[7])),
        acknowledged_at=(float(row[13]) if row[13] is not None else None),
        target_actor=str(row[9]),
        team_id=str(row[10]),
        sent_at=float(row[11]) if row[11] is not None else None,
        published_text=str(row[12]),
        provenance=str(row[14]),
    )
    if record.target_actor and (not record.team_id or record.sent_at is None):
        raise SpiceError(
            f"canonical directive {record.key!r} has incomplete publication "
            "provenance (target actor requires team-at-send and sent time); "
            "restore/replay the steering publication"
        )
    if record.target_actor and record.provenance not in DIRECTIVE_PROVENANCES:
        raise SpiceError(
            f"canonical directive {record.key!r} has unknown provenance "
            f"{record.provenance!r}; repair or replay the steering publication"
        )
    if (
        record.target_actor
        and record.disposition != ACK_DISPOSITION_PENDING
        and record.acknowledged_at is None
    ):
        raise SpiceError(
            f"canonical directive {record.key!r} has a consumed disposition "
            "without an ACK time; restore/replay the ACK record"
        )
    return record


def _record_publication_locked(
    connection: sqlite3.Connection, item: DirectivePublicationWrite
) -> None:
    key = _required_value(item.key, "directive key")
    inbox_name = _required_value(item.inbox_name, "directive inbox name")
    target_actor = _required_value(item.target_actor, "directive target actor")
    team_id = _required_value(item.team_id, "directive team-at-send")
    sent_at = max(0.0, float(item.sent_at))
    attachments_json = json.dumps(list(item.attachments), sort_keys=True)
    lineage_json = json.dumps(item.lineage or {}, sort_keys=True)
    existing = connection.execute(
        DIRECTIVE_HISTORY_RECORD_SELECT_SQL + " WHERE key = ?", (key,)
    ).fetchone()
    if existing is None:
        connection.execute(
            """
            INSERT INTO acked_inbox_items
              (key, inbox_name, text, attachments_json, lineage_json, ack_text,
               ack_content, disposition, archived_at, target_actor, team_id,
               sent_at, published_text, acknowledged_at, provenance)
            VALUES (?, ?, ?, ?, ?, '', '', ?, 0, ?, ?, ?, ?, NULL, ?)
            """,
            (
                key,
                inbox_name,
                item.text,
                attachments_json,
                lineage_json,
                ACK_DISPOSITION_PENDING,
                target_actor,
                team_id,
                sent_at,
                item.text,
                DIRECTIVE_PROVENANCE_PUBLISHED,
            ),
        )
        return
    record = _directive_history_record(existing)
    immutable = (
        record.inbox_name,
        record.published_text,
        json.dumps(list(record.attachments), sort_keys=True),
        record.target_actor,
        record.team_id,
        record.sent_at,
    )
    proposed = (
        inbox_name,
        item.text,
        attachments_json,
        target_actor,
        team_id,
        sent_at,
    )
    if record.target_actor and immutable != proposed:
        raise _directive_collision(key, immutable, proposed)
    if not record.target_actor:
        archived_publication = (
            record.inbox_name,
            record.text,
            json.dumps(list(record.attachments), sort_keys=True),
        )
        proposed_publication = (inbox_name, item.text, attachments_json)
        if archived_publication != proposed_publication:
            raise SpiceError(
                f"directive history collision for {key!r}: archived steering "
                "content or attachments do not match the publication; preserve "
                "both stores and replay with an explicit key mapping"
            )
        connection.execute(
            """
            UPDATE acked_inbox_items
            SET target_actor = ?, team_id = ?, sent_at = ?,
                published_text = ?, provenance = ?
            WHERE key = ?
            """,
            (
                target_actor,
                team_id,
                sent_at,
                item.text,
                DIRECTIVE_PROVENANCE_PUBLISHED,
                key,
            ),
        )


def _record_ack_locked(
    connection: sqlite3.Connection,
    item: AckStateWrite,
    *,
    acknowledged_at: float,
) -> None:
    key = _required_value(item.key, "ACK key")
    disposition = _normalize_consumed_disposition(item.disposition)
    attachments_json = json.dumps(list(item.attachments), sort_keys=True)
    lineage_json = json.dumps(item.lineage or {}, sort_keys=True)
    existing = connection.execute(
        DIRECTIVE_HISTORY_RECORD_SELECT_SQL + " WHERE key = ?", (key,)
    ).fetchone()
    if existing is None:
        _insert_archive_only_ack_locked(
            connection,
            item,
            key=key,
            disposition=disposition,
            attachments_json=attachments_json,
            lineage_json=lineage_json,
            acknowledged_at=acknowledged_at,
        )
        return
    record = _directive_history_record(existing)
    if record.target_actor and not (item.ack_text.strip() or item.ack_content.strip()):
        raise SpiceError(
            f"directive ACK for {key!r} is missing auditable response content; "
            "record the transcript-visible ACK/NACK text and retry"
        )
    if record.disposition != ACK_DISPOSITION_PENDING:
        _validate_or_upgrade_consumed_ack_locked(
            connection,
            record,
            item,
            disposition=disposition,
            attachments_json=attachments_json,
            lineage_json=lineage_json,
            acknowledged_at=acknowledged_at,
        )
        return
    _complete_pending_ack_locked(
        connection,
        item,
        key=key,
        disposition=disposition,
        attachments_json=attachments_json,
        lineage_json=lineage_json,
        acknowledged_at=acknowledged_at,
    )


def _insert_archive_only_ack_locked(
    connection: sqlite3.Connection,
    item: AckStateWrite,
    *,
    key: str,
    disposition: str,
    attachments_json: str,
    lineage_json: str,
    acknowledged_at: float,
) -> None:
    connection.execute(
        """
        INSERT INTO acked_inbox_items
          (key, inbox_name, text, attachments_json, lineage_json, ack_text,
           ack_content, disposition, archived_at, target_actor, team_id,
           sent_at, published_text, acknowledged_at, provenance)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, '', '', NULL, ?, ?, ?)
        """,
        (
            key,
            item.inbox_name,
            item.text,
            attachments_json,
            lineage_json,
            item.ack_text,
            item.ack_content,
            disposition,
            acknowledged_at,
            item.text,
            acknowledged_at,
            DIRECTIVE_PROVENANCE_ARCHIVE_ONLY,
        ),
    )


def _validate_or_upgrade_consumed_ack_locked(
    connection: sqlite3.Connection,
    record: DirectiveHistoryRecord,
    item: AckStateWrite,
    *,
    disposition: str,
    attachments_json: str,
    lineage_json: str,
    acknowledged_at: float,
) -> None:
    if (
        record.provenance == DIRECTIVE_PROVENANCE_ARCHIVE_ONLY
        and not record.text
        and not record.ack_text
        and not record.ack_content
    ):
        _replace_incomplete_archive_locked(
            connection,
            item,
            key=record.key,
            disposition=disposition,
            attachments_json=attachments_json,
            lineage_json=lineage_json,
            acknowledged_at=acknowledged_at,
        )
        return
    existing_ack = _recorded_ack_signature(record)
    proposed_ack = _proposed_ack_signature(
        item,
        disposition=disposition,
        attachments_json=attachments_json,
        lineage_json=lineage_json,
    )
    if existing_ack != proposed_ack:
        raise SpiceError(
            f"directive ACK collision for {record.key!r}: the key already has "
            f"disposition {record.disposition!r} with different auditable "
            "content; the existing record was left unchanged"
        )


def _replace_incomplete_archive_locked(
    connection: sqlite3.Connection,
    item: AckStateWrite,
    *,
    key: str,
    disposition: str,
    attachments_json: str,
    lineage_json: str,
    acknowledged_at: float,
) -> None:
    connection.execute(
        """
        UPDATE acked_inbox_items
        SET inbox_name = ?, text = ?, attachments_json = ?,
            lineage_json = ?, ack_text = ?, ack_content = ?,
            disposition = ?, archived_at = ?, published_text = ?,
            acknowledged_at = ?
        WHERE key = ?
        """,
        (
            item.inbox_name,
            item.text,
            attachments_json,
            lineage_json,
            item.ack_text,
            item.ack_content,
            disposition,
            acknowledged_at,
            item.text,
            acknowledged_at,
            key,
        ),
    )


def _complete_pending_ack_locked(
    connection: sqlite3.Connection,
    item: AckStateWrite,
    *,
    key: str,
    disposition: str,
    attachments_json: str,
    lineage_json: str,
    acknowledged_at: float,
) -> None:
    connection.execute(
        """
        UPDATE acked_inbox_items
        SET inbox_name = ?, text = ?, attachments_json = ?, lineage_json = ?,
            ack_text = ?, ack_content = ?, disposition = ?, archived_at = ?,
            acknowledged_at = ?
        WHERE key = ?
        """,
        (
            item.inbox_name,
            item.text,
            attachments_json,
            lineage_json,
            item.ack_text,
            item.ack_content,
            disposition,
            acknowledged_at,
            acknowledged_at,
            key,
        ),
    )


def _recorded_ack_signature(record: DirectiveHistoryRecord) -> tuple[Any, ...]:
    return (
        record.inbox_name,
        record.text,
        json.dumps(list(record.attachments), sort_keys=True),
        json.dumps(record.lineage, sort_keys=True),
        record.ack_text,
        record.ack_content,
        record.disposition,
    )


def _proposed_ack_signature(
    item: AckStateWrite,
    *,
    disposition: str,
    attachments_json: str,
    lineage_json: str,
) -> tuple[Any, ...]:
    return (
        item.inbox_name,
        item.text,
        attachments_json,
        lineage_json,
        item.ack_text,
        item.ack_content,
        disposition,
    )


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
    _ensure_column(
        connection,
        "target_actor",
        "ALTER TABLE acked_inbox_items "
        "ADD COLUMN target_actor TEXT NOT NULL DEFAULT ''",
    )
    _ensure_column(
        connection,
        "team_id",
        "ALTER TABLE acked_inbox_items ADD COLUMN team_id TEXT NOT NULL DEFAULT ''",
    )
    _ensure_column(
        connection,
        "sent_at",
        "ALTER TABLE acked_inbox_items ADD COLUMN sent_at REAL",
    )
    _ensure_column(
        connection,
        "published_text",
        "ALTER TABLE acked_inbox_items "
        "ADD COLUMN published_text TEXT NOT NULL DEFAULT ''",
    )
    _ensure_column(
        connection,
        "acknowledged_at",
        "ALTER TABLE acked_inbox_items ADD COLUMN acknowledged_at REAL",
    )
    _ensure_column(
        connection,
        "provenance",
        "ALTER TABLE acked_inbox_items "
        "ADD COLUMN provenance TEXT NOT NULL DEFAULT 'archiveOnly'",
    )
    connection.execute(
        "UPDATE acked_inbox_items SET published_text = text WHERE published_text = ''"
    )
    connection.execute(
        "UPDATE acked_inbox_items SET acknowledged_at = archived_at "
        "WHERE acknowledged_at IS NULL AND disposition IN (?, ?)",
        (ACK_DISPOSITION_ACKED, ACK_DISPOSITION_REFUSED),
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


def _normalize_consumed_disposition(value: str) -> str:
    clean = str(value or "").strip().lower()
    if clean in {ACK_DISPOSITION_ACKED, ACK_DISPOSITION_REFUSED}:
        return clean
    raise SpiceError(
        f"ACK archival disposition must be 'acked' or 'refused'; got {value!r}"
    )


def _canonical_disposition(value: str) -> str:
    clean = str(value or "").strip().lower()
    if clean in ACK_DISPOSITIONS:
        return clean
    raise SpiceError(
        f"canonical directive history has unknown disposition {value!r}; "
        "repair or replay the steering/ACK record"
    )


def _required_value(value: str, label: str) -> str:
    clean = str(value or "").strip()
    if not clean:
        raise SpiceError(f"{label} must be non-empty")
    return clean


def _directive_collision(
    key: str, existing: tuple[Any, ...], proposed: tuple[Any, ...]
) -> SpiceError:
    labels = (
        "inbox_name",
        "published_text",
        "attachments",
        "target_actor",
        "team_id",
        "sent_at",
    )
    differences = ", ".join(
        f"{label}={old!r}->{new!r}"
        for label, old, new in zip(labels, existing, proposed, strict=True)
        if old != new
    )
    return SpiceError(
        f"directive history collision for {key!r}: immutable publication "
        f"provenance differs ({differences}); the existing record was left unchanged"
    )
