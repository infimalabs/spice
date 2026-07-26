"""Migration safety proofs for canonical ACK authority."""

from __future__ import annotations

import json
import sqlite3
from pathlib import Path
from typing import Any

import pytest

import spice.mail.ackschema as ackschema
from spice.errors import SpiceError
from spice.mail.ackschema import (
    ACK_STATE_LEGACY_TABLE_SCHEMAS,
    ACK_STATE_SCHEMA_VERSION,
    ACK_STATE_TABLE_SQL,
)
from spice.mail.ackstate import (
    ACK_DISPOSITION_ACKED,
    ACK_DISPOSITION_REFUSED,
    DIRECTIVE_PROVENANCE_ARCHIVE_ONLY,
    DIRECTIVE_PROVENANCE_PUBLISHED,
    AckStateWrite,
    DirectivePublicationWrite,
    prepare_directive_history_database,
    record_acked_inbox_items_to_database,
    record_directive_publications_to_database,
)
from spice.sqliteconnection import sqlite_connection

KEY = "1kLegacyAck"
INBOX_NAME = f"{KEY}.txt"
TEXT = "canonical steering text"
ATTACHMENTS = [{"name": "proof.txt", "path": "/shared/proof.txt"}]
LINEAGE = {"resendOf": "1kAncestor"}
ACK_TEXT = f"ACK {KEY}: retained"
ACK_CONTENT = "retained"


def _table_shape(connection: sqlite3.Connection) -> tuple[tuple[object, ...], ...]:
    return tuple(
        (
            str(row[1]),
            str(row[2]).upper(),
            int(row[3]),
            row[4],
            int(row[5]),
            int(row[6]),
        )
        for row in connection.execute(
            'PRAGMA table_xinfo("acked_inbox_items")'
        ).fetchall()
    )


def _current_shape() -> tuple[tuple[object, ...], ...]:
    connection = sqlite3.connect(":memory:")
    try:
        connection.execute(ACK_STATE_TABLE_SQL)
        return _table_shape(connection)
    finally:
        connection.close()


def _logical_state(path: Path) -> tuple[int, str, tuple[str, ...]]:
    with sqlite_connection(path) as connection:
        return (
            int(connection.execute("PRAGMA user_version").fetchone()[0]),
            str(connection.execute("PRAGMA journal_mode").fetchone()[0]),
            tuple(connection.iterdump()),
        )


def _seed_released_schema(path: Path, source: str) -> tuple[str, ...]:
    values: dict[str, Any] = {
        "key": KEY,
        "inbox_name": INBOX_NAME,
        "text": TEXT,
        "attachments_json": json.dumps(ATTACHMENTS, sort_keys=True),
        "lineage_json": json.dumps(LINEAGE, sort_keys=True),
        "ack_text": ACK_TEXT,
        "ack_content": ACK_CONTENT,
        "disposition": ACK_DISPOSITION_REFUSED,
        "archived_at": 20.0,
    }
    with sqlite_connection(path) as connection:
        connection.execute(ACK_STATE_LEGACY_TABLE_SCHEMAS[source])
        columns = tuple(
            str(row[1])
            for row in connection.execute(
                'PRAGMA table_xinfo("acked_inbox_items")'
            ).fetchall()
        )
        placeholders = ", ".join("?" for _column in columns)
        connection.execute(
            f"INSERT INTO acked_inbox_items ({', '.join(columns)}) "
            f"VALUES ({placeholders})",
            tuple(values[column] for column in columns),
        )
    return columns


def test_fresh_ack_authority_writes_only_the_versioned_current_shape(tmp_path):
    path = tmp_path / "fresh.sqlite3"
    record_directive_publications_to_database(
        path,
        [
            DirectivePublicationWrite(
                key=KEY,
                inbox_name=INBOX_NAME,
                text=TEXT,
                target_actor="thread:actor",
                team_id="team-a",
                sent_at=10.0,
            )
        ],
    )

    with sqlite_connection(path) as connection:
        version = int(connection.execute("PRAGMA user_version").fetchone()[0])
        shape = _table_shape(connection)
        tables = tuple(
            str(row[0])
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name"
            )
        )

    assert version == ACK_STATE_SCHEMA_VERSION
    assert shape == _current_shape()
    assert tables == ("acked_inbox_items",)


@pytest.mark.parametrize("source", tuple(ACK_STATE_LEGACY_TABLE_SCHEMAS))
def test_every_released_ack_shape_migrates_without_losing_history(tmp_path, source):
    path = tmp_path / f"{source}.sqlite3"
    source_columns = _seed_released_schema(path, source)

    prepare_directive_history_database(path)

    with sqlite_connection(path) as connection:
        version = int(connection.execute("PRAGMA user_version").fetchone()[0])
        shape = _table_shape(connection)
        row = connection.execute(
            "SELECT key, inbox_name, text, attachments_json, lineage_json, "
            "ack_text, ack_content, disposition, archived_at, target_actor, "
            "team_id, sent_at, published_text, acknowledged_at, provenance "
            "FROM acked_inbox_items"
        ).fetchone()
    assert version == ACK_STATE_SCHEMA_VERSION
    assert shape == _current_shape()
    assert row is not None
    assert tuple(row) == (
        KEY,
        INBOX_NAME,
        TEXT,
        json.dumps(ATTACHMENTS, sort_keys=True),
        (
            json.dumps(LINEAGE, sort_keys=True)
            if "lineage_json" in source_columns
            else "{}"
        ),
        ACK_TEXT if "ack_text" in source_columns else "",
        ACK_CONTENT if "ack_content" in source_columns else "",
        (
            ACK_DISPOSITION_REFUSED
            if "disposition" in source_columns
            else ACK_DISPOSITION_ACKED
        ),
        20.0,
        "",
        "",
        None,
        TEXT,
        20.0,
        DIRECTIVE_PROVENANCE_ARCHIVE_ONLY,
    )


def test_unversioned_current_rows_keep_publication_and_audit_content(tmp_path):
    path = tmp_path / "unversioned-current.sqlite3"
    expected = (
        KEY,
        INBOX_NAME,
        TEXT,
        json.dumps(ATTACHMENTS, sort_keys=True),
        json.dumps(LINEAGE, sort_keys=True),
        ACK_TEXT,
        ACK_CONTENT,
        ACK_DISPOSITION_REFUSED,
        20.0,
        "thread:actor",
        "team-a",
        10.0,
        "immutable publication",
        21.0,
        DIRECTIVE_PROVENANCE_PUBLISHED,
    )
    with sqlite_connection(path) as connection:
        connection.execute(ACK_STATE_TABLE_SQL)
        connection.execute(
            "INSERT INTO acked_inbox_items VALUES "
            "(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            expected,
        )

    prepare_directive_history_database(path)

    with sqlite_connection(path) as connection:
        actual = tuple(
            connection.execute(
                "SELECT * FROM acked_inbox_items WHERE key = ?", (KEY,)
            ).fetchone()
        )
        version = int(connection.execute("PRAGMA user_version").fetchone()[0])
    assert actual == expected
    assert version == ACK_STATE_SCHEMA_VERSION


def test_alter_grown_v027_shape_migrates_into_canonical_column_order(tmp_path):
    path = tmp_path / "alter-grown-v027.sqlite3"
    _seed_released_schema(path, "v0.8")
    with sqlite_connection(path) as connection:
        connection.execute(
            "ALTER TABLE acked_inbox_items ADD COLUMN ack_text TEXT NOT NULL DEFAULT ''"
        )
        connection.execute(
            "ALTER TABLE acked_inbox_items "
            "ADD COLUMN ack_content TEXT NOT NULL DEFAULT ''"
        )
        connection.execute(
            "ALTER TABLE acked_inbox_items "
            "ADD COLUMN disposition TEXT NOT NULL DEFAULT 'acked'"
        )
        connection.execute(
            "ALTER TABLE acked_inbox_items "
            "ADD COLUMN lineage_json TEXT NOT NULL DEFAULT '{}'"
        )
        assert _table_shape(connection) != _current_shape()

    prepare_directive_history_database(path)

    with sqlite_connection(path) as connection:
        assert _table_shape(connection) == _current_shape()
        assert connection.execute(
            "SELECT key, text, published_text FROM acked_inbox_items"
        ).fetchone() == (KEY, TEXT, TEXT)


def test_alter_grown_current_shape_is_rewritten_without_losing_canonical_facts(
    tmp_path,
):
    path = tmp_path / "alter-grown-current.sqlite3"
    _seed_released_schema(path, "v0.8")
    alterations = (
        "ADD COLUMN ack_text TEXT NOT NULL DEFAULT ''",
        "ADD COLUMN ack_content TEXT NOT NULL DEFAULT ''",
        "ADD COLUMN disposition TEXT NOT NULL DEFAULT 'acked'",
        "ADD COLUMN lineage_json TEXT NOT NULL DEFAULT '{}'",
        "ADD COLUMN target_actor TEXT NOT NULL DEFAULT ''",
        "ADD COLUMN team_id TEXT NOT NULL DEFAULT ''",
        "ADD COLUMN sent_at REAL",
        "ADD COLUMN published_text TEXT NOT NULL DEFAULT ''",
        "ADD COLUMN acknowledged_at REAL",
        "ADD COLUMN provenance TEXT NOT NULL DEFAULT 'archiveOnly'",
    )
    with sqlite_connection(path) as connection:
        for alteration in alterations:
            connection.execute(f"ALTER TABLE acked_inbox_items {alteration}")
        connection.execute(
            "UPDATE acked_inbox_items SET lineage_json = ?, ack_text = ?, "
            "ack_content = ?, disposition = ?, target_actor = ?, team_id = ?, "
            "sent_at = ?, published_text = ?, acknowledged_at = ?, provenance = ?",
            (
                json.dumps(LINEAGE, sort_keys=True),
                ACK_TEXT,
                ACK_CONTENT,
                ACK_DISPOSITION_REFUSED,
                "thread:actor",
                "team-a",
                10.0,
                "immutable publication",
                21.0,
                DIRECTIVE_PROVENANCE_PUBLISHED,
            ),
        )
        before = tuple(
            connection.execute(
                "SELECT key, inbox_name, text, attachments_json, lineage_json, "
                "ack_text, ack_content, disposition, archived_at, target_actor, "
                "team_id, sent_at, published_text, acknowledged_at, provenance "
                "FROM acked_inbox_items"
            ).fetchone()
        )
        assert _table_shape(connection) != _current_shape()

    prepare_directive_history_database(path)

    with sqlite_connection(path) as connection:
        after = tuple(
            connection.execute(
                "SELECT key, inbox_name, text, attachments_json, lineage_json, "
                "ack_text, ack_content, disposition, archived_at, target_actor, "
                "team_id, sent_at, published_text, acknowledged_at, provenance "
                "FROM acked_inbox_items"
            ).fetchone()
        )
        assert _table_shape(connection) == _current_shape()
    assert after == before


def test_empty_retired_metric_column_is_removed_from_transitional_shape(tmp_path):
    path = tmp_path / "retired-metric.sqlite3"
    with sqlite_connection(path) as connection:
        connection.execute(ackschema._RETIRED_METRIC_TABLE_SQL)
        connection.execute(
            "INSERT INTO acked_inbox_items "
            "(key, inbox_name, text, archived_at, legacy_metric_json) "
            "VALUES (?, ?, ?, ?, '')",
            (KEY, INBOX_NAME, TEXT, 20.0),
        )

    prepare_directive_history_database(path)

    with sqlite_connection(path) as connection:
        columns = tuple(
            str(row[1])
            for row in connection.execute('PRAGMA table_xinfo("acked_inbox_items")')
        )
        row = connection.execute("SELECT key, text FROM acked_inbox_items").fetchone()
        shape = _table_shape(connection)
    assert "legacy_metric_json" not in columns
    assert shape == _current_shape()
    assert row == (KEY, TEXT)


def test_nonempty_retired_metric_audit_is_rejected_without_loss(tmp_path):
    path = tmp_path / "retired-metric-audit.sqlite3"
    with sqlite_connection(path) as connection:
        connection.execute(ackschema._RETIRED_METRIC_TABLE_SQL)
        connection.execute(
            "INSERT INTO acked_inbox_items "
            "(key, inbox_name, text, archived_at, legacy_metric_json) "
            "VALUES (?, ?, ?, ?, ?)",
            (KEY, INBOX_NAME, TEXT, 20.0, '{"sends":1}'),
        )
    before = _logical_state(path)

    with pytest.raises(SpiceError, match="retired metric audit content"):
        prepare_directive_history_database(path)

    assert _logical_state(path) == before


def test_failed_legacy_migration_rolls_back_every_logical_change(tmp_path, monkeypatch):
    path = tmp_path / "rollback.sqlite3"
    _seed_released_schema(path, "v0.17-v0.27")
    before = _logical_state(path)
    monkeypatch.setattr(ackschema, "ACK_STATE_INDEX_SQL", "THIS IS NOT SQL")

    with pytest.raises(sqlite3.OperationalError):
        prepare_directive_history_database(path)

    assert _logical_state(path) == before


def test_newer_ack_writer_fails_without_mutating_database_or_journal(tmp_path):
    path = tmp_path / "newer.sqlite3"
    with sqlite_connection(path) as connection:
        connection.execute(ACK_STATE_TABLE_SQL)
        connection.execute(f"PRAGMA user_version = {ACK_STATE_SCHEMA_VERSION + 1}")
    before = _logical_state(path)

    with pytest.raises(SpiceError, match="newer schema version"):
        record_acked_inbox_items_to_database(
            path,
            [AckStateWrite(key=KEY, inbox_name=INBOX_NAME, text=TEXT)],
        )

    assert _logical_state(path) == before


@pytest.mark.parametrize("drift", ("partial-unversioned", "extra-current-column"))
def test_partial_or_drifted_ack_shape_is_rejected_without_mutation(tmp_path, drift):
    path = tmp_path / f"{drift}.sqlite3"
    with sqlite_connection(path) as connection:
        if drift == "partial-unversioned":
            connection.execute(
                "CREATE TABLE acked_inbox_items ("
                "key TEXT PRIMARY KEY, inbox_name TEXT NOT NULL, "
                "archived_at REAL NOT NULL)"
            )
        else:
            connection.execute(ACK_STATE_TABLE_SQL)
            connection.execute(
                "ALTER TABLE acked_inbox_items ADD COLUMN unexpected TEXT"
            )
            connection.execute(f"PRAGMA user_version = {ACK_STATE_SCHEMA_VERSION}")
    before = _logical_state(path)

    with pytest.raises(SpiceError, match="canonical table shape"):
        prepare_directive_history_database(path)

    assert _logical_state(path) == before


def test_warm_process_revalidates_after_another_writer_advances_schema(tmp_path):
    path = tmp_path / "warm.sqlite3"
    record_acked_inbox_items_to_database(
        path,
        [AckStateWrite(key=KEY, inbox_name=INBOX_NAME, text=TEXT)],
    )
    with sqlite_connection(path) as connection:
        connection.execute(f"PRAGMA user_version = {ACK_STATE_SCHEMA_VERSION + 1}")
    before = _logical_state(path)

    with pytest.raises(SpiceError, match="changed to newer schema version"):
        record_acked_inbox_items_to_database(
            path,
            [
                AckStateWrite(
                    key="1kSecondAck",
                    inbox_name="1kSecondAck.txt",
                    text="must not land",
                )
            ],
        )

    assert _logical_state(path) == before
