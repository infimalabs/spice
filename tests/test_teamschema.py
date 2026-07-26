"""Migration safety proofs for durable Serve team authority."""

from __future__ import annotations

import sqlite3
from pathlib import Path
from typing import Any

import pytest

import spice.serve.team.store as team_store_module
from spice.errors import SpiceError
from spice.sqliteconnection import sqlite_connection
from spice.serve.team.schema import (
    LEGACY_TEAM_SCHEMA_FINGERPRINT,
    OBSERVATION_ATTRIBUTION_REBUILD_REQUIRED,
    TEAM_AUTHORITY_MIGRATIONS,
    TEAM_AUTHORITY_SCHEMA,
    TEAM_AUTHORITY_SCHEMAS,
    TEAM_AUTHORITY_SCHEMA_VERSION,
    TEAM_AUTHORITY_TABLES,
    TEAM_PROJECTION_SCHEMA,
    TEAM_PROJECTION_TABLES,
)
from spice.serve.team.store import ServeTeamStore


def _forget_initialized(path: Path) -> None:
    ServeTeamStore._initialized_paths.discard(path)


def _initialize(path: Path) -> None:
    _forget_initialized(path)
    with ServeTeamStore(path=path).connect():
        pass


def _seed_authority(path: Path) -> None:
    with sqlite_connection(path) as connection:
        connection.execute(
            "INSERT INTO events (revision, ts, kind, team_id, payload) "
            "VALUES (?, ?, ?, ?, ?)",
            (17, 10.0, "createTeam", "team-a", '{"members":["agent-a"]}'),
        )
        connection.execute(
            "INSERT INTO global_settings "
            "(key, value, updated_at, revision) VALUES (?, ?, ?, ?)",
            ("fast_mode", "true", 10.1, 17),
        )
        connection.execute(
            "INSERT INTO teams "
            "(team_id, status, created_at, revision, config_revision, lifetime, "
            "task_filters, shell_settings) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            (
                "team-a",
                "open",
                10.2,
                17,
                8,
                "Drive",
                '["serve.team"]',
                '{"metrics":{"range":"1h"}}',
            ),
        )
        connection.execute(
            "INSERT INTO memberships "
            "(team_id, agent_id, joined_at, position) VALUES (?, ?, ?, ?)",
            ("team-a", "agent-a", 10.3, 0),
        )
        connection.execute(
            "INSERT INTO team_task_filters "
            "(team_id, project, source, created_at, updated_at) "
            "VALUES (?, ?, ?, ?, ?)",
            ("team-a", "serve.team", "manual", 10.4, 10.5),
        )
        connection.execute(
            "INSERT INTO team_merge_subgroups "
            "(parent_team_id, child_team_id, merged_revision, agent_ids, "
            "created_at, restored_revision) VALUES (?, ?, ?, ?, ?, ?)",
            ("team-a", "team-child", 14, '["agent-a"]', 10.6, None),
        )
        connection.execute(
            "INSERT INTO renewals "
            "(agent_id, team_id, state, ancestor_thread_id, successor_agent_id, "
            "successor_thread_id, team_slot, predecessor_identity, "
            "successor_identity, revision) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (
                "agent-a",
                "team-a",
                "started",
                "thread-old",
                "agent-next",
                "thread-next",
                0,
                '{"model":"old"}',
                '{"model":"new"}',
                16,
            ),
        )
        connection.execute(
            "INSERT INTO agent_identities "
            "(actor_id, target_id, thread_id, actual_driver, actual_model, "
            "actual_effort, actual_service_tier, desired_driver, desired_model, "
            "desired_effort, transcript_owner, renewal_state, "
            "renewal_ancestor_thread_id, renewal_successor_thread_id, "
            "renewal_revision, updated_at) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (
                "agent-a",
                "main-c",
                "thread-old",
                "codex",
                "gpt-current",
                "high",
                "priority",
                "codex",
                "gpt-next",
                "xhigh",
                "codex",
                "started",
                "thread-old",
                "thread-next",
                16,
                10.7,
            ),
        )


def _authority_state(path: Path) -> dict[str, tuple[tuple[Any, ...], ...]]:
    with sqlite_connection(path) as connection:
        return {
            table: tuple(
                tuple(row)
                for row in connection.execute(
                    f'SELECT * FROM "{table}" ORDER BY rowid'
                ).fetchall()
            )
            for table in sorted(TEAM_AUTHORITY_TABLES)
        }


def _logical_state(path: Path) -> tuple[int, tuple[str, ...]]:
    with sqlite_connection(path) as connection:
        version = int(connection.execute("PRAGMA user_version").fetchone()[0])
        return version, tuple(connection.iterdump())


def _schema_state(path: Path) -> tuple[tuple[Any, ...], ...]:
    with sqlite_connection(path) as connection:
        return tuple(
            tuple(row)
            for row in connection.execute(
                "SELECT type, name, tbl_name, sql FROM sqlite_master "
                "WHERE name NOT LIKE 'sqlite_autoindex_%' "
                "ORDER BY type, name"
            ).fetchall()
        )


def test_legacy_current_schema_upgrades_without_rewriting_authority(tmp_path):
    path = tmp_path / "legacy.sqlite3"
    with sqlite_connection(path) as connection:
        connection.executescript(TEAM_AUTHORITY_SCHEMA)
        connection.executescript(TEAM_PROJECTION_SCHEMA)
        connection.execute(f"PRAGMA user_version = {LEGACY_TEAM_SCHEMA_FINGERPRINT}")
    _seed_authority(path)
    before = _authority_state(path)

    _initialize(path)

    assert _authority_state(path) == before
    with sqlite_connection(path) as connection:
        assert connection.execute("PRAGMA user_version").fetchone()[0] == (
            TEAM_AUTHORITY_SCHEMA_VERSION
        )


def test_fresh_and_legacy_upgrade_converge_on_one_schema_and_version(tmp_path):
    fresh_path = tmp_path / "fresh.sqlite3"
    legacy_path = tmp_path / "legacy.sqlite3"
    _initialize(fresh_path)
    with sqlite_connection(legacy_path) as connection:
        connection.executescript(TEAM_AUTHORITY_SCHEMA)
        connection.executescript(TEAM_PROJECTION_SCHEMA)
        connection.execute(f"PRAGMA user_version = {LEGACY_TEAM_SCHEMA_FINGERPRINT}")

    _initialize(legacy_path)

    assert _logical_state(fresh_path)[0] == TEAM_AUTHORITY_SCHEMA_VERSION
    assert _logical_state(legacy_path)[0] == TEAM_AUTHORITY_SCHEMA_VERSION
    assert _schema_state(legacy_path) == _schema_state(fresh_path)


def test_failed_forward_migration_rolls_back_every_logical_change(
    tmp_path, monkeypatch
):
    path = tmp_path / "migration-failure.sqlite3"
    _initialize(path)
    _seed_authority(path)
    before = _logical_state(path)
    monkeypatch.setattr(team_store_module, "TEAM_AUTHORITY_SCHEMA_VERSION", 2)
    monkeypatch.setattr(
        team_store_module,
        "TEAM_AUTHORITY_MIGRATIONS",
        {
            **TEAM_AUTHORITY_MIGRATIONS,
            2: (
                "CREATE TABLE migration_probe (value TEXT NOT NULL);"
                "INSERT INTO migration_probe VALUES ('must roll back');"
                "THIS IS NOT SQL;"
            ),
        },
    )
    monkeypatch.setattr(
        team_store_module,
        "TEAM_AUTHORITY_SCHEMAS",
        {**TEAM_AUTHORITY_SCHEMAS, 2: TEAM_AUTHORITY_SCHEMA},
    )
    _forget_initialized(path)

    with pytest.raises(sqlite3.OperationalError):
        ServeTeamStore(path=path)._ensure_schema()

    assert _logical_state(path) == before


def test_newer_writer_fails_before_mutating_database_or_journal_mode(tmp_path):
    path = tmp_path / "newer.sqlite3"
    with sqlite_connection(path) as connection:
        connection.executescript(TEAM_AUTHORITY_SCHEMA)
        connection.executescript(TEAM_PROJECTION_SCHEMA)
        connection.execute(f"PRAGMA user_version = {TEAM_AUTHORITY_SCHEMA_VERSION + 1}")
    _seed_authority(path)
    before = _logical_state(path)
    with sqlite_connection(path) as connection:
        assert connection.execute("PRAGMA journal_mode").fetchone()[0] == "delete"

    with pytest.raises(SpiceError, match="newer schema version"):
        ServeTeamStore(path=path)._ensure_schema()

    assert _logical_state(path) == before
    with sqlite_connection(path) as connection:
        assert connection.execute("PRAGMA journal_mode").fetchone()[0] == "delete"


def test_cached_store_rechecks_newer_writer_version_before_use(tmp_path):
    path = tmp_path / "newer-after-initialize.sqlite3"
    store = ServeTeamStore(path=path)
    with store.connect():
        pass
    with sqlite_connection(path) as connection:
        connection.execute(f"PRAGMA user_version = {TEAM_AUTHORITY_SCHEMA_VERSION + 1}")
    before = _logical_state(path)

    with pytest.raises(SpiceError, match="changed to newer schema version"):
        with store.connect():
            pytest.fail("newer database partially opened")

    assert _logical_state(path) == before


def test_drifted_projection_table_is_rebuilt_from_the_current_ddl(tmp_path):
    path = tmp_path / "drifted-projection.sqlite3"
    _initialize(path)
    _seed_authority(path)
    before = _authority_state(path)
    with sqlite_connection(path) as connection:
        connection.execute('DROP TABLE "agent_metric_cursors"')
        connection.execute(
            "CREATE TABLE agent_metric_cursors ("
            "agent_id TEXT NOT NULL, source_path TEXT NOT NULL, "
            "offset INTEGER NOT NULL, updated_at REAL NOT NULL, "
            "PRIMARY KEY (agent_id, source_path))"
        )
        connection.execute(
            "INSERT INTO agent_metric_cursors VALUES ('agent-a', '/t.jsonl', 10, 1.0)"
        )
        connection.execute(
            "INSERT INTO agent_metrics "
            "(agent_id, team_id, tool_calls, updated_at) VALUES ('agent-a', 'team-a', 3, 1.0)"
        )
        connection.execute(
            "UPDATE observation_attribution_state SET status = ? WHERE singleton = 1",
            (OBSERVATION_ATTRIBUTION_REBUILD_REQUIRED,),
        )

    _initialize(path)

    with sqlite_connection(path) as connection:
        columns = tuple(
            str(row[1])
            for row in connection.execute('PRAGMA table_info("agent_metric_cursors")')
        )
        cursor_rows = connection.execute(
            "SELECT count(*) FROM agent_metric_cursors"
        ).fetchone()[0]
        counted_rows = connection.execute(
            "SELECT count(*) FROM agent_metrics WHERE agent_id = 'agent-a'"
        ).fetchone()[0]
        kept_attribution_status = connection.execute(
            "SELECT status FROM observation_attribution_state WHERE singleton = 1"
        ).fetchone()[0]

    # A projection whose shape drifted is discarded and rebuilt from the current
    # DDL, and it takes its family with it: counts left standing beside a reset
    # checkpoint would be counted again by the replay that follows. Projections
    # outside that family keep their rows, and authority is untouched either way.
    assert columns == (
        "agent_id",
        "source_path",
        "offset",
        "source_device",
        "source_inode",
        "updated_at",
    )
    assert cursor_rows == 0
    assert counted_rows == 0
    assert kept_attribution_status == OBSERVATION_ATTRIBUTION_REBUILD_REQUIRED
    assert _authority_state(path) == before


def test_projection_schema_reset_cannot_change_authority_or_its_version(tmp_path):
    path = tmp_path / "projection-reset.sqlite3"
    _initialize(path)
    _seed_authority(path)
    before = _authority_state(path)
    with sqlite_connection(path) as connection:
        version_before = connection.execute("PRAGMA user_version").fetchone()[0]
        for table in TEAM_PROJECTION_TABLES:
            connection.execute(f'DROP TABLE "{table}"')
        connection.execute(
            "CREATE TABLE projection_experiment "
            "(projection_key TEXT PRIMARY KEY, value TEXT NOT NULL)"
        )
        connection.execute(
            "INSERT INTO projection_experiment VALUES ('kept', 'outside authority')"
        )

    _initialize(path)

    assert _authority_state(path) == before
    with sqlite_connection(path) as connection:
        version_after = connection.execute("PRAGMA user_version").fetchone()[0]
        recreated = {
            str(row[0])
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table'"
            )
        }
        experiment = connection.execute(
            "SELECT projection_key, value FROM projection_experiment"
        ).fetchone()
    assert version_after == version_before == TEAM_AUTHORITY_SCHEMA_VERSION
    assert TEAM_PROJECTION_TABLES <= recreated
    assert tuple(experiment) == ("kept", "outside authority")


def test_unversioned_drifted_authority_fails_without_destructive_rebuild(tmp_path):
    path = tmp_path / "drifted.sqlite3"
    with sqlite_connection(path) as connection:
        connection.executescript(
            """
            CREATE TABLE teams (
                team_id TEXT PRIMARY KEY,
                status TEXT NOT NULL DEFAULT 'open',
                created_at REAL NOT NULL,
                revision INTEGER NOT NULL,
                config_revision INTEGER NOT NULL DEFAULT 0,
                lifetime TEXT NOT NULL,
                speech_mode TEXT NOT NULL,
                selected_view TEXT NOT NULL,
                task_filters TEXT NOT NULL DEFAULT '[]',
                shell_settings TEXT NOT NULL DEFAULT '{}'
            );
            INSERT INTO teams (
                team_id, created_at, revision, lifetime, speech_mode, selected_view
            ) VALUES ('team-irreplaceable', 1.0, 42, 'Drive', 'on', 'messages');
            """
        )
    before = _logical_state(path)

    with pytest.raises(SpiceError, match="refusing to rebuild or open"):
        ServeTeamStore(path=path)._ensure_schema()

    assert _logical_state(path) == before


def test_current_version_with_partial_authority_fails_without_opening(tmp_path):
    path = tmp_path / "partial.sqlite3"
    with sqlite_connection(path) as connection:
        connection.executescript(TEAM_AUTHORITY_SCHEMA)
        connection.execute("DROP TABLE renewals")
        connection.execute(f"PRAGMA user_version = {TEAM_AUTHORITY_SCHEMA_VERSION}")
    before = _logical_state(path)

    with pytest.raises(SpiceError, match=r"durable table shape \(renewals\)"):
        ServeTeamStore(path=path)._ensure_schema()

    assert _logical_state(path) == before
