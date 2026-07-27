"""Migration safety proofs for durable Serve team authority."""

from __future__ import annotations

import hashlib
import sqlite3
import time
from pathlib import Path
from typing import Any

import pytest

import spice.serve.team.store as team_store_module
from spice.errors import SpiceError
from spice.sqliteconnection import sqlite_connection
from spice.serve.team.projection import (
    AGENT_ACTIVITY,
    FIRST_GENERATION,
    PROJECTION_TABLES,
    ServeProjectionStore,
    rebuild_projection_family,
)
from spice.serve.team.schema import (
    TEAM_AUTHORITY_MIGRATIONS,
    TEAM_AUTHORITY_MONOTONIC_VERSION_MAX,
    TEAM_AUTHORITY_SCHEMA_VERSION,
    TEAM_AUTHORITY_SCHEMAS,
    TEAM_AUTHORITY_TABLES,
)
from spice.serve.team.store import (
    GLOBAL_LANE_SCHEMA_KEY_PREFIX,
    LANE_SCHEMA_RECORD_HORIZON_SECONDS,
    AuthorityStoreSupersededError,
    ServeTeamStore,
    record_lane_schema_version,
)

# Two lanes sharing one store, told apart the way the fleet tells them apart:
# by worktree, one per supervisor.
LAGGING_LANE = "/fleet/spice-lagging"
CURRENT_LANE = "/fleet/spice-current"
# Not a lane record, and picked so only a LIKE pattern treating the `_` in the
# lane prefix as a wildcard would ever collect it.
LOOKALIKE_SETTINGS_KEY = "lane-schema:not-a-version"

# The shape each retained authority version describes, pinned. Databases in the
# field are stamped with these numbers, so a number cannot come to mean a
# different set of columns later: editing a shape in place changes a digest here
# and fails. Adding a version adds its line and drops the one that falls out of
# the supported range.
AUTHORITY_SHAPE_DIGESTS = {
    1: "2db781a8730ca90bf610f8c03add298fbbd2a02a925004612ca0d28a89af8eb8",
    2: "5f0f10de12a33355365d89f984d589508a75ee6e78605f148fed306933209a24",
}

# The rehearsals below follow the current version rather than naming a fixed
# one, so the release that adds the next authority version rehearses upgrading
# from the shape it is leaving behind without anyone remembering to.
PRIOR_AUTHORITY_VERSION = TEAM_AUTHORITY_SCHEMA_VERSION - 1

V027_TEAM_SCHEMA_FINGERPRINT = 783663365
V027_RETIRED_TABLES = (
    "agent_metrics",
    "agent_metric_buckets",
    "agent_metric_cursors",
    "task_events",
    "directives",
    "directive_totals",
)
V027_RETIRED_SCHEMA = """
CREATE TABLE agent_metrics (
    agent_id TEXT NOT NULL,
    team_id TEXT NOT NULL,
    tool_calls INTEGER NOT NULL DEFAULT 0,
    updated_at REAL NOT NULL,
    PRIMARY KEY (agent_id, team_id)
);
CREATE TABLE agent_metric_buckets (
    agent_id TEXT NOT NULL,
    team_id TEXT NOT NULL,
    bucket_start INTEGER NOT NULL,
    messages INTEGER NOT NULL DEFAULT 0,
    tool_calls INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (agent_id, team_id, bucket_start)
);
CREATE TABLE agent_metric_cursors (
    agent_id TEXT NOT NULL,
    source_path TEXT NOT NULL,
    offset INTEGER NOT NULL,
    updated_at REAL NOT NULL,
    PRIMARY KEY (agent_id, source_path)
);
CREATE TABLE task_events (
    ts REAL NOT NULL,
    kind TEXT NOT NULL CHECK (
        kind IN ('claim', 'phaseAdvance', 'review', 'complete', 'drain')
    ),
    task_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    team_id TEXT NOT NULL
);
CREATE TABLE directives (
    directive_key TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    team_id TEXT NOT NULL,
    sent_at REAL NOT NULL,
    acked INTEGER NOT NULL DEFAULT 0,
    acked_at REAL
);
CREATE TABLE directive_totals (
    agent_id TEXT NOT NULL,
    team_id TEXT NOT NULL,
    sends INTEGER NOT NULL DEFAULT 0,
    acked INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (agent_id, team_id)
);
CREATE INDEX agent_metric_buckets_by_start
    ON agent_metric_buckets (bucket_start);
CREATE INDEX task_events_by_ts
    ON task_events (ts);
CREATE INDEX task_events_by_agent_team_ts
    ON task_events (agent_id, team_id, ts);
CREATE INDEX directives_by_sent_at
    ON directives (sent_at);
"""


def _digest_of_shape(shape: dict[str, Any]) -> str:
    return hashlib.sha256(repr(sorted(shape.items())).encode("utf-8")).hexdigest()


def _shape_digest(version: int) -> str:
    return _digest_of_shape(team_store_module._authority_schema_shape(version))


def _forget_initialized(path: Path) -> None:
    ServeTeamStore._initialized_paths.discard(path)


def _build_authority_at(path: Path, version: int) -> None:
    """Leave a database exactly as the writer that stamped `version` left it."""
    with sqlite_connection(path) as connection:
        team_store_module._execute_schema_script(
            connection, TEAM_AUTHORITY_SCHEMAS[version]
        )
        connection.execute(f"PRAGMA user_version = {version}")


def _initialize(path: Path) -> None:
    _forget_initialized(path)
    with ServeTeamStore(path=path).connect():
        pass


def _forget_projection_file(projections: ServeProjectionStore) -> None:
    """Re-sync a file rewritten in place, which keeps its stat identity."""
    ServeProjectionStore._initialized_files.pop(projections.path, None)


def _open_projections(path: Path) -> ServeProjectionStore:
    """Open the projection database that belongs beside this authority file."""
    store = ServeTeamStore(path=path)
    _forget_projection_file(store.projections)
    with store.projections.connect():
        pass
    return store.projections


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
        identity = {
            "actor_id": "agent-a",
            "target_id": "main-c",
            "thread_id": "thread-old",
            "actual_driver": "codex",
            "actual_model": "gpt-current",
            "actual_effort": "high",
            "actual_service_tier": "priority",
            "desired_driver": "codex",
            "desired_model": "gpt-next",
            "desired_effort": "xhigh",
            "transcript_owner": "codex",
            "renewal_state": "started",
            "renewal_ancestor_thread_id": "thread-old",
            "renewal_successor_thread_id": "thread-next",
            "renewal_revision": 16,
            "updated_at": 10.7,
        }
        # Fill whichever identity columns this database's version carries, so
        # one seeder serves a store at any version and the row that survives a
        # migration is demonstrably the row that went in.
        columns = [
            str(row[1])
            for row in connection.execute("PRAGMA table_info(agent_identities)")
        ]
        connection.execute(
            f"INSERT INTO agent_identities ({', '.join(columns)}) "
            f"VALUES ({', '.join('?' for _ in columns)})",
            tuple(identity[column] for column in columns),
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


def _table_state(
    path: Path, tables: tuple[str, ...]
) -> dict[str, tuple[str, tuple[tuple[Any, ...], ...]]]:
    with sqlite_connection(path) as connection:
        return {
            table: (
                str(
                    connection.execute(
                        "SELECT sql FROM sqlite_master "
                        "WHERE type = 'table' AND name = ?",
                        (table,),
                    ).fetchone()[0]
                ),
                tuple(
                    tuple(row)
                    for row in connection.execute(
                        f'SELECT * FROM "{table}" ORDER BY rowid'
                    ).fetchall()
                ),
            )
            for table in tables
        }


def _identity_state(path: Path) -> tuple[dict[str, Any], ...]:
    with sqlite_connection(path) as connection:
        columns = tuple(
            str(row[1])
            for row in connection.execute("PRAGMA table_info(agent_identities)")
        )
        return tuple(
            dict(zip(columns, row, strict=True))
            for row in connection.execute(
                "SELECT * FROM agent_identities ORDER BY actor_id"
            ).fetchall()
        )


def _logical_state(path: Path) -> tuple[int, tuple[str, ...]]:
    with sqlite_connection(path) as connection:
        version = int(connection.execute("PRAGMA user_version").fetchone()[0])
        return version, tuple(connection.iterdump())


def test_failed_forward_migration_rolls_back_every_logical_change(
    tmp_path, monkeypatch
):
    path = tmp_path / "migration-failure.sqlite3"
    _initialize(path)
    _seed_authority(path)
    before = _logical_state(path)
    next_version = TEAM_AUTHORITY_SCHEMA_VERSION + 1
    monkeypatch.setattr(
        team_store_module, "TEAM_AUTHORITY_SCHEMA_VERSION", next_version
    )
    monkeypatch.setattr(
        team_store_module,
        "TEAM_AUTHORITY_MIGRATIONS",
        {
            **TEAM_AUTHORITY_MIGRATIONS,
            next_version: (
                "CREATE TABLE migration_probe (value TEXT NOT NULL);"
                "INSERT INTO migration_probe VALUES ('must roll back');"
                "THIS IS NOT SQL;"
            ),
        },
    )
    _forget_initialized(path)

    with pytest.raises(sqlite3.OperationalError):
        ServeTeamStore(path=path)._ensure_schema()

    assert _logical_state(path) == before


def test_newer_writer_fails_before_mutating_database_or_journal_mode(tmp_path):
    path = tmp_path / "newer.sqlite3"
    _build_authority_at(path, TEAM_AUTHORITY_SCHEMA_VERSION)
    _seed_authority(path)
    with sqlite_connection(path) as connection:
        connection.execute(
            "CREATE TABLE future_authority_records ("
            "record_id TEXT PRIMARY KEY, value TEXT NOT NULL)"
        )
        connection.execute(f"PRAGMA user_version = {TEAM_AUTHORITY_SCHEMA_VERSION + 1}")
    before = _logical_state(path)
    with sqlite_connection(path) as connection:
        assert connection.execute("PRAGMA journal_mode").fetchone()[0] == "delete"

    with pytest.raises(SpiceError, match="newer schema version"):
        ServeTeamStore(path=path)._ensure_schema()

    assert _logical_state(path) == before
    with sqlite_connection(path) as connection:
        assert connection.execute("PRAGMA journal_mode").fetchone()[0] == "delete"


def test_v027_fingerprint_store_migrates_by_shape_and_leaves_retired_tables_whole(
    tmp_path,
):
    assert V027_TEAM_SCHEMA_FINGERPRINT > TEAM_AUTHORITY_MONOTONIC_VERSION_MAX
    path = tmp_path / "v0.27.sqlite3"
    _build_authority_at(path, PRIOR_AUTHORITY_VERSION)
    with sqlite_connection(path) as connection:
        team_store_module._execute_schema_script(connection, V027_RETIRED_SCHEMA)
        connection.execute(
            "INSERT INTO agent_metrics VALUES ('agent-a', 'team-a', 13, 10.8)"
        )
        connection.execute(
            "INSERT INTO agent_metric_buckets VALUES ('agent-a', 'team-a', 60, 5, 13)"
        )
        connection.execute(
            "INSERT INTO agent_metric_cursors "
            "VALUES ('agent-a', '/tmp/agent-a.jsonl', 41, 10.9)"
        )
        connection.execute(
            "INSERT INTO task_events "
            "VALUES (11.0, 'claim', 'TASK-1', 'agent-a', 'team-a')"
        )
        connection.execute(
            "INSERT INTO directives "
            "VALUES ('directive-1', 'agent-a', 'team-a', 11.1, 1, 11.2)"
        )
        connection.execute(
            "INSERT INTO directive_totals VALUES ('agent-a', 'team-a', 3, 2)"
        )
        connection.execute(f"PRAGMA user_version = {V027_TEAM_SCHEMA_FINGERPRINT}")
    _seed_authority(path)
    authority_before = _authority_state(path)
    identity_before = _identity_state(path)
    retired_before = _table_state(path, V027_RETIRED_TABLES)

    store = ServeTeamStore(path=path)
    _forget_initialized(path)
    identity = store.agent_identity_for_actor("agent-a")

    with sqlite_connection(path) as connection:
        assert int(connection.execute("PRAGMA user_version").fetchone()[0]) == (
            TEAM_AUTHORITY_SCHEMA_VERSION
        )
    authority_after = _authority_state(path)
    identity_after = _identity_state(path)
    retired_after = _table_state(path, V027_RETIRED_TABLES)

    assert (identity.actor_id, identity.actual_model, identity.renewal_revision) == (
        "agent-a",
        "gpt-current",
        16,
    )
    assert {
        table: rows
        for table, rows in authority_after.items()
        if table != "agent_identities"
    } == {
        table: rows
        for table, rows in authority_before.items()
        if table != "agent_identities"
    }
    assert identity_after == tuple(
        {
            column: value
            for column, value in row.items()
            if column != "actual_service_tier"
        }
        for row in identity_before
    )
    assert retired_after == retired_before


def test_a_store_that_moved_ahead_refuses_as_superseded_and_says_so_to_the_process(
    tmp_path,
):
    """The one refusal a restart resolves, told apart and told to the process.

    Both halves matter. The type is what a caller can act on, because every
    caller that reaches this store today catches `SpiceError` broadly and turns
    it into an answer; the wording is asserted beside it because those callers
    keep reading the message, and a type added for one of them must not change
    what the rest print.

    The hook carries it out of the exception's path on purpose: by the time
    this is raised, the process is running code the store no longer matches,
    and the exception is about to be handled by someone with no interest in
    that fact.
    """
    path = tmp_path / "moved-ahead.sqlite3"
    heard: list[str] = []

    def hear(error: AuthorityStoreSupersededError) -> None:
        heard.append(str(error))

    store = ServeTeamStore(path=path, superseded_hook=hear)
    with store.connect():
        pass
    with sqlite_connection(path) as connection:
        connection.execute(f"PRAGMA user_version = {TEAM_AUTHORITY_SCHEMA_VERSION + 1}")
    before = _logical_state(path)

    with pytest.raises(AuthorityStoreSupersededError) as refusal:
        with store.connect():
            pytest.fail("superseded database partially opened")

    assert isinstance(refusal.value, SpiceError)
    assert str(refusal.value) == (
        "team authority database changed to newer schema version "
        f"{TEAM_AUTHORITY_SCHEMA_VERSION + 1}; this writer requires "
        f"{TEAM_AUTHORITY_SCHEMA_VERSION} and will not mutate it"
    )
    assert heard == [str(refusal.value)]
    assert _logical_state(path) == before


def test_a_store_stamped_behind_this_writer_stays_the_refusal_it_already_was(tmp_path):
    """Backwards is not superseded: restarting on this code would meet it again.

    The distinction is the whole point of the type. A store ahead of this
    process names a build that exists and can serve it; a store behind it names
    nothing, so a process that exited over it would exit again, and the loop
    that restarts it would spin instead of upgrading.
    """
    path = tmp_path / "moved-behind.sqlite3"
    heard: list[str] = []

    def hear(error: AuthorityStoreSupersededError) -> None:
        heard.append(str(error))

    store = ServeTeamStore(path=path, superseded_hook=hear)
    with store.connect():
        pass
    with sqlite_connection(path) as connection:
        connection.execute(f"PRAGMA user_version = {PRIOR_AUTHORITY_VERSION}")

    with pytest.raises(SpiceError) as refusal:
        with store.connect():
            pytest.fail("unsupported database partially opened")

    assert type(refusal.value) is SpiceError
    assert str(refusal.value) == (
        "team authority database changed to unsupported schema version "
        f"{PRIOR_AUTHORITY_VERSION}; this writer requires "
        f"{TEAM_AUTHORITY_SCHEMA_VERSION} and will not mutate it"
    )
    assert heard == []


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


def test_a_drifted_projection_rebuilds_in_its_own_file_leaving_authority_whole(
    tmp_path,
):
    path = tmp_path / "drifted-projection.sqlite3"
    _initialize(path)
    _seed_authority(path)
    before = _authority_state(path)
    projections = _open_projections(path)
    with sqlite_connection(projections.path) as connection:
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
            "(agent_id, team_id, tool_calls, updated_at) "
            "VALUES ('agent-a', 'team-a', 3, 1.0)"
        )
    _forget_projection_file(projections)

    with projections.connect() as connection:
        columns = tuple(
            str(row[1])
            for row in connection.execute('PRAGMA table_info("agent_metric_cursors")')
        )
    rebuilt = {state.family.name: state for state in projections.family_states()}

    # A projection whose shape drifted is discarded and rebuilt from the current
    # DDL, and it takes its family with it: counts left standing beside a reset
    # checkpoint would be counted again by the replay that follows. The discard
    # publishes a new generation, so an operator sees a rebuild rather than
    # inferring one from empty tables.
    assert columns == (
        "agent_id",
        "source_path",
        "offset",
        "source_device",
        "source_inode",
        "updated_at",
    )
    assert rebuilt["agentActivity"].row_counts == dict.fromkeys(
        AGENT_ACTIVITY.tables, 0
    )
    assert rebuilt["agentActivity"].generation == FIRST_GENERATION + 1
    # The drift never reached the authority file, which was open and seeded the
    # whole time.
    assert _authority_state(path) == before


def test_projection_lifecycle_cannot_change_authority_or_its_version(tmp_path):
    path = tmp_path / "projection-reset.sqlite3"
    _initialize(path)
    _seed_authority(path)
    before = _authority_state(path)
    with sqlite_connection(path) as connection:
        version_before = connection.execute("PRAGMA user_version").fetchone()[0]
    projections = _open_projections(path)

    rebuild_projection_family(
        projections,
        AGENT_ACTIVITY.name,
        lambda _stage: None,
    )
    projections.path.write_bytes(b"not a database at all")
    _forget_projection_file(projections)
    with projections.connect() as connection:
        rebuilt_tables = {
            str(row[0])
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table'"
            )
        }
    projections.path.unlink()
    with projections.connect():
        pass

    # Rebuilding it empty, corrupting it, and deleting it outright are all
    # recoverable in one file: each one costs a replay and the store rebuilds
    # itself.
    assert set(PROJECTION_TABLES) <= rebuilt_tables
    assert projections.path.exists()
    # None of it reached authority, whose rows and version are byte-identical.
    assert _authority_state(path) == before
    with sqlite_connection(path) as connection:
        version_after = connection.execute("PRAGMA user_version").fetchone()[0]
    assert version_after == version_before == TEAM_AUTHORITY_SCHEMA_VERSION


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


def test_changing_an_authority_shape_without_advancing_the_version_fails_here():
    versions = sorted(TEAM_AUTHORITY_SCHEMAS)

    # The retained shapes are the current version and the one predecessor this
    # writer converts, and each describes the shape pinned to it. Editing one in
    # place moves a digest and lands here, which is the whole point: a version
    # number stamped on a database in the field has to keep naming the columns
    # that database has.
    assert versions == [PRIOR_AUTHORITY_VERSION, TEAM_AUTHORITY_SCHEMA_VERSION]
    assert {version: _shape_digest(version) for version in versions} == (
        AUTHORITY_SHAPE_DIGESTS
    )


def test_no_two_authority_versions_describe_the_same_table_shape():
    digests = [_shape_digest(version) for version in sorted(TEAM_AUTHORITY_SCHEMAS)]

    # This is what lets the opener read a database's version off its columns
    # when the stamp disagrees with them: matching a shape identifies exactly
    # one version. Two versions sharing a shape would make that answer a guess,
    # so pointing the predecessor entry back at the current DDL -- the aliasing
    # that cost a fleet its authority store -- fails right here.
    assert len(set(digests)) == len(digests)


def test_the_forward_migration_carries_the_predecessor_onto_the_current_shape():
    upgraded = sqlite3.connect(":memory:")
    try:
        team_store_module._execute_schema_script(
            upgraded, TEAM_AUTHORITY_SCHEMAS[PRIOR_AUTHORITY_VERSION]
        )
        team_store_module._execute_schema_script(
            upgraded, TEAM_AUTHORITY_MIGRATIONS[TEAM_AUTHORITY_SCHEMA_VERSION]
        )
        migrated = team_store_module._authority_table_shape(upgraded)
    finally:
        upgraded.close()

    # Two roads reach the current version: a store being created runs its DDL,
    # and a store at the predecessor runs the migration. Nothing else makes
    # those agree, so without this an upgraded store and a new one could wear
    # one version number over two different sets of columns.
    assert _digest_of_shape(migrated) == _shape_digest(TEAM_AUTHORITY_SCHEMA_VERSION)


def test_a_prior_shape_store_reaches_the_settled_shape_in_one_forward_step(
    tmp_path, monkeypatch
):
    path = tmp_path / "prior-shape.sqlite3"
    _build_authority_at(path, PRIOR_AUTHORITY_VERSION)
    _seed_authority(path)
    before = _authority_state(path)
    applied: list[str] = []
    run_script = team_store_module._execute_schema_script

    def record_script(connection: sqlite3.Connection, script: str) -> None:
        # Expected shapes are derived in memory, whose database_list carries no
        # file. Only scripts run against the store on disk are steps it took.
        if connection.execute("PRAGMA database_list").fetchone()[2]:
            applied.append(script)
        run_script(connection, script)

    monkeypatch.setattr(team_store_module, "_execute_schema_script", record_script)

    store = ServeTeamStore(path=path)
    _forget_initialized(path)
    identity = store.agent_identity_for_actor("agent-a")

    # One migration, applied by the writer on open, is the entire recovery: no
    # hand-edited authority database anywhere in it.
    assert applied == [TEAM_AUTHORITY_MIGRATIONS[TEAM_AUTHORITY_SCHEMA_VERSION]]
    with sqlite_connection(path) as connection:
        assert int(connection.execute("PRAGMA user_version").fetchone()[0]) == (
            TEAM_AUTHORITY_SCHEMA_VERSION
        )
    # The identity read that every allocator answer goes through returns the
    # seeded row, with the columns the settled shape kept still carrying what
    # was written under the shape that had one more of them.
    assert (identity.actor_id, identity.actual_model, identity.renewal_revision) == (
        "agent-a",
        "gpt-current",
        16,
    )
    # Only the dropped column left. Every other authority row is untouched.
    after = _authority_state(path)
    assert {table: after[table] for table in after if table != "agent_identities"} == {
        table: before[table] for table in before if table != "agent_identities"
    }


def test_a_migration_waits_while_a_lane_still_runs_the_older_schema(tmp_path):
    path = tmp_path / "occupied.sqlite3"
    _build_authority_at(path, PRIOR_AUTHORITY_VERSION)
    _seed_authority(path)
    with sqlite_connection(path) as connection:
        record_lane_schema_version(connection, LAGGING_LANE, PRIOR_AUTHORITY_VERSION)
        record_lane_schema_version(
            connection, CURRENT_LANE, TEAM_AUTHORITY_SCHEMA_VERSION
        )
    before = _authority_state(path)

    _forget_initialized(path)
    with pytest.raises(SpiceError) as refusal:
        ServeTeamStore(path=path).agent_identity_for_actor("agent-a")

    # The lane that would lose the store is named with the schema it is on, so
    # whoever reads this knows which process it is waiting for. The lane already
    # on the current schema is one this migration cannot hurt, so it is counted
    # out: one lane holds the store back, and the message says one.
    message = str(refusal.value)
    assert f"{LAGGING_LANE} at schema {PRIOR_AUTHORITY_VERSION}" in message
    assert "1 lane(s)" in message
    # Declining has to leave the store exactly as the lane still using it can
    # keep using it. A store carried halfway is the outage this exists to stop.
    assert _authority_state(path) == before
    with sqlite_connection(path) as connection:
        assert int(connection.execute("PRAGMA user_version").fetchone()[0]) == (
            PRIOR_AUTHORITY_VERSION
        )


def test_a_migration_proceeds_once_the_lagging_lane_ages_past_the_horizon(tmp_path):
    path = tmp_path / "departed.sqlite3"
    _build_authority_at(path, PRIOR_AUTHORITY_VERSION)
    _seed_authority(path)
    with sqlite_connection(path) as connection:
        record_lane_schema_version(connection, LAGGING_LANE, PRIOR_AUTHORITY_VERSION)
        record_lane_schema_version(
            connection, CURRENT_LANE, TEAM_AUTHORITY_SCHEMA_VERSION
        )
        connection.execute(
            "UPDATE global_settings SET updated_at = ? WHERE key = ?",
            (
                time.time() - LANE_SCHEMA_RECORD_HORIZON_SECONDS - 1,
                f"{GLOBAL_LANE_SCHEMA_KEY_PREFIX}{LAGGING_LANE}",
            ),
        )

    _forget_initialized(path)
    identity = ServeTeamStore(path=path).agent_identity_for_actor("agent-a")

    # Nothing proved that lane dead, and for a lane that exits without cleanup
    # nothing ever would. Going the horizon without asking for work is what
    # retires its record, so one abandoned worktree cannot leave an unattended
    # fleet unable to migrate for the rest of its life.
    with sqlite_connection(path) as connection:
        assert int(connection.execute("PRAGMA user_version").fetchone()[0]) == (
            TEAM_AUTHORITY_SCHEMA_VERSION
        )
    assert identity.actor_id == "agent-a"


def test_a_lookalike_settings_key_is_not_read_as_a_lane_record(tmp_path):
    """Only keys this store wrote as lane records may be read back as versions.

    Every key the sweep collects is read as a schema version, so collecting one
    it had no business collecting produces a value that never was a version --
    raised inside the transaction that was about to migrate, which leaves a
    store that would have opened as one nothing in the fleet can open. The
    lane prefix ends in `_`, which SQLite's LIKE reads as a wildcard, so
    matching it exactly is what keeps that key out.
    """
    path = tmp_path / "lookalike.sqlite3"
    _build_authority_at(path, PRIOR_AUTHORITY_VERSION)
    _seed_authority(path)
    with sqlite_connection(path) as connection:
        connection.execute(
            "INSERT INTO global_settings (key, value, updated_at, revision) "
            "VALUES (?, ?, ?, 0)",
            (LOOKALIKE_SETTINGS_KEY, "nonsense", time.time()),
        )

    _forget_initialized(path)
    identity = ServeTeamStore(path=path).agent_identity_for_actor("agent-a")

    with sqlite_connection(path) as connection:
        assert int(connection.execute("PRAGMA user_version").fetchone()[0]) == (
            TEAM_AUTHORITY_SCHEMA_VERSION
        )
    assert identity.actor_id == "agent-a"


def test_a_store_stamped_behind_its_own_shape_is_carried_forward_by_the_writer(
    tmp_path,
):
    path = tmp_path / "stamped-behind.sqlite3"
    _build_authority_at(path, TEAM_AUTHORITY_SCHEMA_VERSION)
    _seed_authority(path)
    with sqlite_connection(path) as connection:
        connection.execute(f"PRAGMA user_version = {PRIOR_AUTHORITY_VERSION}")
    before = _authority_state(path)

    store = ServeTeamStore(path=path)
    _forget_initialized(path)
    identity = store.agent_identity_for_actor("agent-a")

    # A database whose stamp outran its columns is what the fleet was left
    # holding, and reading the source version off the shape is what makes it
    # openable again. Its rows are already at the settled shape, so carrying it
    # forward is a stamp and nothing else.
    with sqlite_connection(path) as connection:
        assert int(connection.execute("PRAGMA user_version").fetchone()[0]) == (
            TEAM_AUTHORITY_SCHEMA_VERSION
        )
    assert identity.actor_id == "agent-a"
    assert _authority_state(path) == before


def test_current_version_with_partial_authority_fails_without_opening(tmp_path):
    path = tmp_path / "partial.sqlite3"
    _build_authority_at(path, TEAM_AUTHORITY_SCHEMA_VERSION)
    with sqlite_connection(path) as connection:
        connection.execute("DROP TABLE renewals")
    before = _logical_state(path)

    with pytest.raises(SpiceError, match=r"durable table shape \(renewals\)"):
        ServeTeamStore(path=path)._ensure_schema()

    assert _logical_state(path) == before
