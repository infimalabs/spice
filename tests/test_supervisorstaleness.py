"""A lane whose supervisor predates the store stops receiving new work."""

from __future__ import annotations

from pathlib import Path
import shutil
import time

import pytest

from spice.agent.driver import DRIVER
from spice.agent.lifecyclebinding import (
    SUPERVISOR_SCHEMA_VERSION_FIELD,
    write_agent_state,
)
from spice.agent.paths import write_agent_thread_pointer
from spice.errors import SpiceError
from spice.serve.team.ids import thread_actor_id
from spice.serve.team.schema import (
    TEAM_AUTHORITY_SCHEMAS,
    TEAM_AUTHORITY_SCHEMA_VERSION,
)
from spice.serve.team.store import (
    GLOBAL_LANE_SCHEMA_KEY_PREFIX,
    GLOBAL_PENDING_AUTHORITY_MIGRATION_KEY,
    PendingAuthorityMigration,
    ServeTeamStore,
    TeamConfig,
    pending_authority_migration,
    team_database_path,
)
from spice.sqliteconnection import sqlite_connection
from spice.tasks import alloc, config, create, identity, ops
from tests.test_reposcaffolding import init_committed_repo as _init_repo

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR = "cccccccccccccccccccccccccccccccc"
PEER_ACTOR = "dddddddddddddddddddddddddddddddd"
# One release behind whatever the tree currently writes, so the fixture stays
# an older supervisor as the real constant advances instead of pinning a
# literal that a future bump would quietly turn into "current".
STALE_SUPERVISOR_VERSION = TEAM_AUTHORITY_SCHEMA_VERSION - 1
WIND_DOWN_WORDING = "predates the deployment and should wind down"
PENDING_WIND_DOWN_WORDING = (
    f"migration {STALE_SUPERVISOR_VERSION} -> "
    f"{TEAM_AUTHORITY_SCHEMA_VERSION} is pending"
)


@pytest.fixture
def task_repo(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-staleness")
    config.set_backend(str(backend))
    try:
        yield repo
    finally:
        config.set_backend(None)


def _record_supervisor_version(repo, version: int) -> None:
    """Stand in for a supervisor that started while `version` was current."""
    write_agent_thread_pointer(repo, ACTOR)
    write_agent_state(
        repo,
        {"thread_id": ACTOR, SUPERVISOR_SCHEMA_VERSION_FIELD: version},
    )


def _recorded_lane_schema() -> str:
    """Read back what this lane told the shared store it is running."""
    with sqlite_connection(team_database_path()) as connection:
        row = connection.execute(
            "SELECT value FROM global_settings WHERE key = ?",
            (f"{GLOBAL_LANE_SCHEMA_KEY_PREFIX}{config.repo_root()}",),
        ).fetchone()
    return str(row[0]) if row is not None else ""


def _build_authority_at(version: int) -> Path:
    path = team_database_path()
    with sqlite_connection(path, ensure_parent=True) as connection:
        connection.executescript(TEAM_AUTHORITY_SCHEMAS[version])
        connection.execute(f"PRAGMA user_version = {version}")
    ServeTeamStore._initialized_paths.discard(path)
    return path


def _record_pending_migration(source_version: int, target_version: int) -> None:
    with sqlite_connection(team_database_path()) as connection:
        connection.execute(
            "INSERT INTO global_settings (key, value, updated_at, revision) "
            "VALUES (?, ?, ?, 0)",
            (
                GLOBAL_PENDING_AUTHORITY_MIGRATION_KEY,
                f"{source_version}:{target_version}",
                time.time(),
            ),
        )


def _stored_authority_version() -> int:
    with sqlite_connection(team_database_path()) as connection:
        return int(connection.execute("PRAGMA user_version").fetchone()[0])


def _seed_lane(title: str) -> str:
    ServeTeamStore().create_team(
        members=[thread_actor_id(ACTOR), thread_actor_id(PEER_ACTOR)],
        config=TeamConfig(lifetime="Drain"),
    )
    return create.add(
        title,
        project="task.deployment",
        origin="ack:1kH7wqmd",
        priority="medium",
        acceptance=["the lane stops taking work its supervisor cannot serve"],
    )


def test_stale_supervisor_finishes_its_task_then_is_refused_new_work(task_repo):
    """The order a real lane meets: hold, finish, then ask for more.

    Recording the older constant only after the claim is what makes the two
    halves independent. The held task is finished by a lane that is already
    stale, so its completion is evidence about the refusal's scope rather than
    about timing.
    """
    handle = _seed_lane("Stale lane keeps the task it holds")

    claimed = alloc.next_task()
    assert identity.render_handle(claimed or {}) == handle

    _record_supervisor_version(task_repo, STALE_SUPERVISOR_VERSION)

    still_held = alloc.next_task()
    assert identity.render_handle(still_held or {}) == handle

    ops.done(handle, validation=["implementation complete"])
    assert identity.resolve(handle)["phase"] == "review"

    with pytest.raises(SpiceError, match=WIND_DOWN_WORDING) as refusal:
        alloc.next_task()
    assert str(STALE_SUPERVISOR_VERSION) in str(refusal.value)
    assert str(TEAM_AUTHORITY_SCHEMA_VERSION) in str(refusal.value)


def test_current_supervisor_is_handed_new_work(task_repo):
    """The same lane, one version later, allocates normally.

    This is what makes the refusal a comparison rather than the mere presence
    of a recorded constant: only the version differs between the two tests.
    """
    handle = _seed_lane("Current lane takes new work")
    _record_supervisor_version(task_repo, TEAM_AUTHORITY_SCHEMA_VERSION)

    assigned = alloc.next_task()

    assert identity.render_handle(assigned or {}) == handle
    assert _recorded_lane_schema() == str(TEAM_AUTHORITY_SCHEMA_VERSION)


def test_pending_migration_refuses_equal_stamp_and_unblocks_upgrade(task_repo):
    """Refusal retires the lane record, so migration does not wait four hours."""
    path = _build_authority_at(STALE_SUPERVISOR_VERSION)
    _record_supervisor_version(task_repo, STALE_SUPERVISOR_VERSION)

    # First ask records lane=1 beside stamp=1. The route read then reaches the
    # version-2 writer, which defers and publishes the pending migration.
    with pytest.raises(SpiceError, match="is not being applied yet"):
        alloc.next_task()

    assert _stored_authority_version() == STALE_SUPERVISOR_VERSION
    assert _recorded_lane_schema() == str(STALE_SUPERVISOR_VERSION)
    assert pending_authority_migration(path) == PendingAuthorityMigration(
        source_version=STALE_SUPERVISOR_VERSION,
        target_version=TEAM_AUTHORITY_SCHEMA_VERSION,
    )

    with pytest.raises(SpiceError, match=PENDING_WIND_DOWN_WORDING) as refusal:
        alloc.next_task()

    assert "should wind down instead of taking new work" in str(refusal.value)
    assert _recorded_lane_schema() == ""

    # No clock manipulation: retiring the refused lane is sufficient for the
    # same forward migration to apply and clear its pending signal immediately.
    ServeTeamStore._initialized_paths.discard(path)
    with ServeTeamStore(path=path).connect():
        pass

    assert _stored_authority_version() == TEAM_AUTHORITY_SCHEMA_VERSION
    assert pending_authority_migration(path) is None


def test_pending_migration_keeps_the_task_already_held(task_repo):
    handle = _seed_lane("Pending migration does not displace held work")
    claimed = alloc.next_task()
    assert identity.render_handle(claimed or {}) == handle

    _record_supervisor_version(task_repo, TEAM_AUTHORITY_SCHEMA_VERSION)
    _record_pending_migration(
        TEAM_AUTHORITY_SCHEMA_VERSION,
        TEAM_AUTHORITY_SCHEMA_VERSION + 1,
    )

    still_held = alloc.next_task()

    assert identity.render_handle(still_held or {}) == handle


def test_asking_for_work_records_this_lane_in_the_shared_store(task_repo):
    """A lane the coming migration would strand says so where a migrator looks.

    This is the half that has to happen while the lane is still working: the
    process that will be hurt is compiled against the older constant and cannot
    know a newer one exists, so it can only state what it is running and leave
    the decision to whoever arrives next. Recorded even here, on the way to
    being refused, because being too old for new work and being too old to
    survive a migration are the same fact reaching two different readers.
    """
    _seed_lane("Lagging lane still announces itself")
    _record_supervisor_version(task_repo, STALE_SUPERVISOR_VERSION)

    with pytest.raises(SpiceError, match=WIND_DOWN_WORDING):
        alloc.next_task()

    assert _recorded_lane_schema() == str(STALE_SUPERVISOR_VERSION)
