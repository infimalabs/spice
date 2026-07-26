"""Versioning and exact-shape migrations for canonical ACK authority."""

from __future__ import annotations

import sqlite3
from functools import cache

from spice.errors import SpiceError

ACK_STATE_SCHEMA_VERSION = 1
ACK_STATE_TABLE_NAME = "acked_inbox_items"
_LEGACY_TABLE_NAME = "acked_inbox_items_legacy"

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
_RETIRED_METRIC_TABLE_SQL = ACK_STATE_TABLE_SQL.replace(
    "  provenance TEXT NOT NULL DEFAULT 'archiveOnly'\n);",
    "  provenance TEXT NOT NULL DEFAULT 'archiveOnly',\n"
    "  legacy_metric_json TEXT NOT NULL DEFAULT ''\n);",
)

# Every released spiceacks.sqlite3 table was unversioned. These are its exact
# source contracts, grouped only when releases emitted byte-identical DDL:
# v0.8; v0.10; v0.11-v0.16; and v0.17-v0.27. A fifth supported unversioned
# source is the exact current table shipped between v0.27 and this versioning
# boundary; it needs only the version stamp and must not be rewritten.
ACK_STATE_LEGACY_TABLE_SCHEMAS = {
    "v0.8": """
CREATE TABLE IF NOT EXISTS acked_inbox_items (
  key TEXT PRIMARY KEY,
  inbox_name TEXT NOT NULL,
  text TEXT NOT NULL,
  attachments_json TEXT NOT NULL DEFAULT '[]',
  archived_at REAL NOT NULL
);
""",
    "v0.10": """
CREATE TABLE IF NOT EXISTS acked_inbox_items (
  key TEXT PRIMARY KEY,
  inbox_name TEXT NOT NULL,
  text TEXT NOT NULL,
  attachments_json TEXT NOT NULL DEFAULT '[]',
  ack_text TEXT NOT NULL DEFAULT '',
  ack_content TEXT NOT NULL DEFAULT '',
  archived_at REAL NOT NULL
);
""",
    "v0.11-v0.16": """
CREATE TABLE IF NOT EXISTS acked_inbox_items (
  key TEXT PRIMARY KEY,
  inbox_name TEXT NOT NULL,
  text TEXT NOT NULL,
  attachments_json TEXT NOT NULL DEFAULT '[]',
  ack_text TEXT NOT NULL DEFAULT '',
  ack_content TEXT NOT NULL DEFAULT '',
  disposition TEXT NOT NULL DEFAULT 'acked',
  archived_at REAL NOT NULL
);
""",
    "v0.17-v0.27": """
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
""",
}

_LEGACY_ROW_SELECTS = {
    "v0.8": """
key, inbox_name, text, attachments_json, '{}', '', '', 'acked', archived_at,
'', '', NULL, text, archived_at, 'archiveOnly'
""",
    "v0.10": """
key, inbox_name, text, attachments_json, '{}', ack_text, ack_content, 'acked',
archived_at, '', '', NULL, text, archived_at, 'archiveOnly'
""",
    "v0.11-v0.16": """
key, inbox_name, text, attachments_json, '{}', ack_text, ack_content,
disposition, archived_at, '', '', NULL, text,
CASE WHEN disposition IN ('acked', 'refused') THEN archived_at ELSE NULL END,
'archiveOnly'
""",
    "v0.17-v0.27": """
key, inbox_name, text, attachments_json, lineage_json, ack_text, ack_content,
disposition, archived_at, '', '', NULL, text,
CASE WHEN disposition IN ('acked', 'refused') THEN archived_at ELSE NULL END,
'archiveOnly'
""",
}

_FRESH_SOURCE = "fresh"
_CURRENT_UNVERSIONED_SOURCE = "post-v0.27"
_ACCRETED_CURRENT_SOURCE = "post-v0.27-accreted"
_RETIRED_METRIC_SOURCE = "post-v0.27-retired-metric"
_CURRENT_ROW_SELECT = """
key, inbox_name, text, attachments_json, lineage_json, ack_text, ack_content,
disposition, archived_at, target_actor, team_id, sent_at, published_text,
acknowledged_at, provenance
"""
type _TableShape = tuple[str, tuple[tuple[object, ...], ...]]


def sync_ack_state_schema(connection: sqlite3.Connection) -> None:
    """Create or transactionally migrate one compatible ACK authority."""
    _ack_source_contract_locked(connection)
    connection.execute("BEGIN IMMEDIATE")
    try:
        source = _ack_source_contract_locked(connection)
        if source == _FRESH_SOURCE:
            connection.execute(ACK_STATE_TABLE_SQL)
        elif source not in (None, _CURRENT_UNVERSIONED_SOURCE):
            _migrate_legacy_ack_table_locked(connection, source)
        _validate_ack_table_shape_locked(connection)
        connection.execute(ACK_STATE_INDEX_SQL)
        connection.execute(f"PRAGMA user_version = {ACK_STATE_SCHEMA_VERSION}")
        connection.commit()
    except BaseException:
        connection.rollback()
        raise


def validate_current_ack_state_schema(connection: sqlite3.Connection) -> None:
    """Revalidate a warm process before it writes through a cached path."""
    stored = int(connection.execute("PRAGMA user_version").fetchone()[0])
    if stored != ACK_STATE_SCHEMA_VERSION:
        relation = "newer" if stored > ACK_STATE_SCHEMA_VERSION else "unsupported"
        raise SpiceError(
            f"ACK state database changed to {relation} schema version {stored}; "
            f"this writer requires {ACK_STATE_SCHEMA_VERSION} and will not mutate it"
        )
    _validate_ack_table_shape_locked(connection)


def _ack_source_contract_locked(connection: sqlite3.Connection) -> str | None:
    stored = int(connection.execute("PRAGMA user_version").fetchone()[0])
    if stored == ACK_STATE_SCHEMA_VERSION:
        _validate_ack_table_shape_locked(connection)
        return None
    if stored > ACK_STATE_SCHEMA_VERSION:
        raise SpiceError(
            "ACK state database was written by newer schema version "
            f"{stored}; this writer supports through {ACK_STATE_SCHEMA_VERSION} "
            "and will not mutate it"
        )
    if stored != 0:
        raise SpiceError(
            f"unsupported ACK state schema version {stored}; refusing to mutate "
            "canonical ACK history"
        )

    actual = _ack_table_shape(connection)
    if actual is None:
        tables = {
            str(row[0])
            for row in connection.execute(
                "SELECT name FROM sqlite_master "
                "WHERE type = 'table' AND name NOT LIKE 'sqlite_%'"
            )
        }
        if not tables:
            return _FRESH_SOURCE
        raise SpiceError(
            "unversioned populated ACK state database has no supported canonical "
            "table; refusing to rebuild or mutate ACK history"
        )
    if actual == _schema_table_shape(ACK_STATE_TABLE_SQL):
        return _CURRENT_UNVERSIONED_SOURCE
    semantic = _semantic_table_shape(actual)
    if semantic == _semantic_table_shape(_schema_table_shape(ACK_STATE_TABLE_SQL)):
        return _ACCRETED_CURRENT_SOURCE
    if semantic == _semantic_table_shape(
        _schema_table_shape(_RETIRED_METRIC_TABLE_SQL)
    ):
        _validate_retired_metric_column_locked(connection)
        return _RETIRED_METRIC_SOURCE
    for source, schema in ACK_STATE_LEGACY_TABLE_SCHEMAS.items():
        if semantic == _semantic_table_shape(_schema_table_shape(schema)):
            return source
    raise SpiceError(
        "unversioned ACK state database has incompatible canonical table shape; "
        "refusing to rebuild or mutate ACK history"
    )


def _migrate_legacy_ack_table_locked(
    connection: sqlite3.Connection, source: str
) -> None:
    select_columns = (
        _CURRENT_ROW_SELECT
        if source in (_ACCRETED_CURRENT_SOURCE, _RETIRED_METRIC_SOURCE)
        else _LEGACY_ROW_SELECTS[source]
    )
    connection.execute(
        f'ALTER TABLE "{ACK_STATE_TABLE_NAME}" RENAME TO "{_LEGACY_TABLE_NAME}"'
    )
    connection.execute(ACK_STATE_TABLE_SQL)
    connection.execute(
        f"""
        INSERT INTO "{ACK_STATE_TABLE_NAME}"
          (key, inbox_name, text, attachments_json, lineage_json, ack_text,
           ack_content, disposition, archived_at, target_actor, team_id,
           sent_at, published_text, acknowledged_at, provenance)
        SELECT {select_columns}
        FROM "{_LEGACY_TABLE_NAME}"
        """
    )
    connection.execute(f'DROP TABLE "{_LEGACY_TABLE_NAME}"')


def _validate_retired_metric_column_locked(
    connection: sqlite3.Connection,
) -> None:
    row = connection.execute(
        "SELECT key FROM acked_inbox_items WHERE legacy_metric_json != '' LIMIT 1"
    ).fetchone()
    if row is not None:
        raise SpiceError(
            "unversioned ACK state database retains nonempty retired metric "
            f"audit content for {str(row[0])!r}; refusing to drop or mutate it"
        )


def _validate_ack_table_shape_locked(connection: sqlite3.Connection) -> None:
    if _ack_table_shape(connection) != _schema_table_shape(ACK_STATE_TABLE_SQL):
        raise SpiceError(
            f"ACK state schema version {ACK_STATE_SCHEMA_VERSION} has incompatible "
            "canonical table shape (acked_inbox_items); refusing to rebuild or "
            "open it"
        )


def _ack_table_shape(connection: sqlite3.Connection) -> _TableShape | None:
    rows = connection.execute(
        f'PRAGMA table_xinfo("{ACK_STATE_TABLE_NAME}")'
    ).fetchall()
    if not rows:
        return None
    sql_row = connection.execute(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?",
        (ACK_STATE_TABLE_NAME,),
    ).fetchone()
    table_sql = " ".join(str(sql_row[0]).split()) if sql_row else ""
    return (
        table_sql,
        tuple(
            (
                str(row[1]),
                str(row[2]).upper(),
                int(row[3]),
                row[4],
                int(row[5]),
                int(row[6]),
            )
            for row in rows
        ),
    )


def _semantic_table_shape(shape: _TableShape) -> _TableShape:
    """Ignore only column order while retaining every column/table constraint.

    Released unversioned writers added missing columns to an existing table, so
    two databases from the same release can differ in physical column order
    depending on the release where each was first created. Named reads/writes
    make that order semantically irrelevant. The normalized SQL fragments and
    xinfo rows still retain names, declared types, nullability, defaults, keys,
    hidden-column flags, and table suffixes; extra constraints therefore do not
    become an accidentally supported source.
    """
    table_sql, columns = shape
    opening = table_sql.find("(")
    closing = table_sql.rfind(")")
    if opening < 0 or closing < opening:
        return shape
    header = table_sql[:opening].strip()
    definitions = tuple(
        sorted(part.strip() for part in table_sql[opening + 1 : closing].split(","))
    )
    suffix = table_sql[closing + 1 :].strip()
    semantic_sql = f"{header} ({' | '.join(definitions)}) {suffix}".strip()
    return (
        semantic_sql,
        tuple(sorted(columns, key=lambda column: str(column[0]))),
    )


@cache
def _schema_table_shape(schema: str) -> _TableShape:
    expected = sqlite3.connect(":memory:")
    try:
        expected.execute(schema)
        shape = _ack_table_shape(expected)
        if shape is None:
            raise SpiceError("ACK schema contract did not create its canonical table")
        return shape
    finally:
        expected.close()
