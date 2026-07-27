"""Fleet-visible schema-migration intent and agent-start refusal contracts."""

from __future__ import annotations

from dataclasses import replace
import sqlite3
import subprocess
import time
from pathlib import Path

import pytest

from spice.agent import lifecycle
from spice.errors import SpiceError
from spice.sqliteconnection import sqlite_connection
from spice.serve.team import store as team_store_module
from spice.serve.team.schema import (
    TEAM_AUTHORITY_SCHEMA_VERSION,
    TEAM_AUTHORITY_SCHEMAS,
)
from spice.serve.team.store import (
    GLOBAL_LANE_SCHEMA_KEY_PREFIX,
    GLOBAL_PENDING_AUTHORITY_MIGRATION_KEY,
    LANE_SCHEMA_RECORD_HORIZON_SECONDS,
    PendingAuthorityMigration,
    ServeTeamStore,
    pending_authority_migration,
    record_lane_schema_version,
)
from tests.test_lifecyclehelpers import status

PRIOR_AUTHORITY_VERSION = TEAM_AUTHORITY_SCHEMA_VERSION - 1
LAGGING_LANE = "/fleet/old-code"


@pytest.fixture(autouse=True)
def _git_worktree_tmp_path(tmp_path):
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)


def _build_authority_at(path: Path, version: int) -> None:
    with sqlite_connection(path) as connection:
        team_store_module._execute_schema_script(
            connection, TEAM_AUTHORITY_SCHEMAS[version]
        )
        connection.execute(f"PRAGMA user_version = {version}")


def _defer_migration(path: Path) -> None:
    _build_authority_at(path, PRIOR_AUTHORITY_VERSION)
    with sqlite_connection(path) as connection:
        record_lane_schema_version(connection, LAGGING_LANE, PRIOR_AUTHORITY_VERSION)
    ServeTeamStore._initialized_paths.discard(path)
    with pytest.raises(SpiceError, match="is not being applied yet"):
        with ServeTeamStore(path=path).connect():
            pass


def _route_launch_check_to(monkeypatch, path: Path) -> None:
    monkeypatch.setattr(
        team_store_module, "team_database_path", lambda _repo_root=None: path
    )
    monkeypatch.setattr(lifecycle, "agent_status", lambda *_args, **_kwargs: status())


def test_deferred_intent_is_readable_at_the_old_schema_constant(tmp_path, monkeypatch):
    path = tmp_path / "spiceteams.sqlite3"
    _defer_migration(path)

    with sqlite_connection(path) as connection:
        version = int(connection.execute("PRAGMA user_version").fetchone()[0])
        stored = connection.execute(
            "SELECT value, revision FROM global_settings WHERE key = ?",
            (GLOBAL_PENDING_AUTHORITY_MIGRATION_KEY,),
        ).fetchone()

    assert version == PRIOR_AUTHORITY_VERSION
    assert stored == (
        f"{PRIOR_AUTHORITY_VERSION}:{TEAM_AUTHORITY_SCHEMA_VERSION}",
        0,
    )
    monkeypatch.setattr(
        team_store_module,
        "TEAM_AUTHORITY_SCHEMA_VERSION",
        PRIOR_AUTHORITY_VERSION,
    )
    assert pending_authority_migration(path) == PendingAuthorityMigration(
        source_version=PRIOR_AUTHORITY_VERSION,
        target_version=TEAM_AUTHORITY_SCHEMA_VERSION,
    )


def test_agent_start_refuses_then_succeeds_immediately_after_migration(
    tmp_path, monkeypatch
):
    path = tmp_path / "spiceteams.sqlite3"
    _defer_migration(path)
    _route_launch_check_to(monkeypatch, path)

    with pytest.raises(SpiceError) as refusal:
        lifecycle.ensure_agent(tmp_path, dry_run=True)

    assert str(refusal.value) == (
        "refusing to start an agent while team authority schema migration "
        f"{PRIOR_AUTHORITY_VERSION} -> {TEAM_AUTHORITY_SCHEMA_VERSION} is "
        "pending; the migration clears this signal once the older lanes drain, "
        "and an abandoned signal expires after 4 hours"
    )

    with sqlite_connection(path) as connection:
        connection.execute(
            "UPDATE global_settings SET updated_at = ? WHERE key = ?",
            (
                time.time() - LANE_SCHEMA_RECORD_HORIZON_SECONDS - 1,
                f"{GLOBAL_LANE_SCHEMA_KEY_PREFIX}{LAGGING_LANE}",
            ),
        )
    ServeTeamStore._initialized_paths.discard(path)
    with ServeTeamStore(path=path).connect():
        pass

    assert pending_authority_migration(path) is None
    assert lifecycle.ensure_agent(tmp_path, dry_run=True).action == "would-start"


def test_pending_migration_does_not_take_a_running_agent_down(tmp_path, monkeypatch):
    path = tmp_path / "spiceteams.sqlite3"
    _defer_migration(path)
    monkeypatch.setattr(
        team_store_module, "team_database_path", lambda _repo_root=None: path
    )
    monkeypatch.setattr(
        lifecycle,
        "agent_status",
        lambda *_args, **_kwargs: replace(status(), process_status="running", pid=1234),
    )

    assert lifecycle.ensure_agent(tmp_path, dry_run=True).action == "already-running"


def test_abandoned_pending_intent_expires_without_cleanup(tmp_path, monkeypatch):
    path = tmp_path / "spiceteams.sqlite3"
    _build_authority_at(path, TEAM_AUTHORITY_SCHEMA_VERSION)
    expired_at = time.time() - LANE_SCHEMA_RECORD_HORIZON_SECONDS - 1
    with sqlite_connection(path) as connection:
        connection.execute(
            "INSERT INTO global_settings (key, value, updated_at, revision) "
            "VALUES (?, ?, ?, 0)",
            (
                GLOBAL_PENDING_AUTHORITY_MIGRATION_KEY,
                f"{PRIOR_AUTHORITY_VERSION}:{TEAM_AUTHORITY_SCHEMA_VERSION}",
                expired_at,
            ),
        )
    _route_launch_check_to(monkeypatch, path)

    assert pending_authority_migration(path) is None
    assert lifecycle.ensure_agent(tmp_path, dry_run=True).action == "would-start"


def test_empty_uninitialized_team_file_does_not_block_agent_start(
    tmp_path, monkeypatch
):
    path = tmp_path / "spiceteams.sqlite3"
    path.touch()
    _route_launch_check_to(monkeypatch, path)

    assert pending_authority_migration(path) is None
    assert lifecycle.ensure_agent(tmp_path, dry_run=True).action == "would-start"


def test_malformed_live_pending_intent_fails_closed(tmp_path):
    path = tmp_path / "spiceteams.sqlite3"
    _build_authority_at(path, TEAM_AUTHORITY_SCHEMA_VERSION)
    with sqlite3.connect(path) as connection:
        connection.execute(
            "INSERT INTO global_settings (key, value, updated_at, revision) "
            "VALUES (?, ?, ?, 0)",
            (GLOBAL_PENDING_AUTHORITY_MIGRATION_KEY, "not-a-version", time.time()),
        )

    with pytest.raises(SpiceError, match="invalid version pair"):
        pending_authority_migration(path)
