"""Executable proofs that the rebuildable store is disposable on its own."""

from __future__ import annotations

import importlib
import re
import sqlite3
from pathlib import Path
from threading import Event, Thread

import pytest

from spice.cli.parser import build_parser
from spice.errors import SpiceError
from spice.serve.cli import run_serve_rebuild_projections
from spice.serve.diagnostics import team_diagnostics_payload
from spice.serve.team.ids import thread_actor_id
from spice.serve.team.projection import (
    AGENT_ACTIVITY,
    FIRST_GENERATION,
    PROJECTION_FAMILIES,
    PROJECTION_SCHEMA,
    PROJECTION_SCHEMA_VERSION,
    PROJECTION_STATUS_REBUILDING,
    PROJECTION_STATUS_STALE,
    PROJECTION_STATUS_UNAVAILABLE,
    PROJECTION_TABLES,
    ServeProjectionStore,
    rebuild_projection_family,
)
from spice.serve.team.store import ServeTeamStore, TeamConfig
from spice.sqliteconnection import sqlite_connection

AGENT_A = thread_actor_id("agent-a")
SUCCESSOR = thread_actor_id("agent-a-next")
RECORDED_TOOL_CALLS = 3
ACTIVITY_TIMESTAMP = 1000.0
REPEATED_READS = 5
# The store's own bookkeeping rather than a replayable family: it records which
# build of each family a reader is looking at.
BOOKKEEPING_TABLES = frozenset({"projection_generations", "projection_status"})
DOTTED_SPICE_NAME = re.compile(r"spice(?:\.[a-zA-Z_][a-zA-Z0-9_]*)+")


def _store(tmp_path) -> ServeTeamStore:
    return ServeTeamStore(path=tmp_path / "teams.sqlite3")


def _seeded_store(tmp_path) -> ServeTeamStore:
    """A store carrying one recorded activity delta in its projection."""
    store = _store(tmp_path)
    store.record_agent_metric_delta(
        AGENT_A,
        tool_calls=RECORDED_TOOL_CALLS,
        message_timestamps=[ACTIVITY_TIMESTAMP],
        tool_call_timestamps=[ACTIVITY_TIMESTAMP],
    )
    return store


def _resolve(dotted: str) -> object:
    module_name, _, attribute = dotted.rpartition(".")
    module = importlib.import_module(module_name)
    return getattr(module, attribute)


def _reopen(projections: ServeProjectionStore) -> None:
    """Force the next open to re-run the schema pass against the file on disk.

    A file rewritten in place keeps its stat identity, so the store has no way
    to notice on its own; a file that was deleted or replaced does not need
    this.
    """
    ServeProjectionStore._initialized_files.pop(projections.path, None)


def _published_state(projections: ServeProjectionStore) -> tuple[int, dict[str, int]]:
    """Read the family through a connection of its own, as a reader would."""
    with sqlite_connection(projections.path) as connection:
        generation = int(
            connection.execute(
                "SELECT generation FROM projection_generations WHERE family = ?",
                (AGENT_ACTIVITY.name,),
            ).fetchone()[0]
        )
        counts = {
            table: int(
                connection.execute(f'SELECT COUNT(*) FROM "{table}"').fetchone()[0]
            )
            for table in AGENT_ACTIVITY.tables
        }
    return generation, counts


def _populate_activity(
    stage: ServeProjectionStore,
    *,
    tool_calls: int,
    started: Event | None = None,
    release: Event | None = None,
    error: str = "",
) -> float:
    with stage.connect() as connection:
        connection.execute(
            "INSERT INTO agent_metrics "
            "(agent_id, team_id, source_path, tool_calls, updated_at) "
            "VALUES (?, ?, ?, ?, ?)",
            (AGENT_A, AGENT_A, "/transcripts/rebuilt.jsonl", tool_calls, 2000.0),
        )
    if started is not None:
        started.set()
    if release is not None and not release.wait(timeout=5):
        raise AssertionError("test did not release staged projection rebuild")
    if error:
        raise RuntimeError(error)
    return 2000.0


def test_every_family_registers_a_replay_contract_that_resolves():
    """Registration answers all six questions, and its code references are real."""
    for family in PROJECTION_FAMILIES:
        registration = (
            family.source,
            family.cursor,
            family.horizon,
            family.rebuild,
            family.beyond_horizon,
            family.recovery_action,
        )
        assert family.tables
        assert all(field.strip() for field in registration)
        named = {
            dotted
            for field in registration
            for dotted in DOTTED_SPICE_NAME.findall(field)
        }
        assert named, f"{family.name} names no spice symbol to replay from"
        for dotted in sorted(named):
            assert _resolve(dotted) is not None
        # The recovery action is the one answer carrying no dotted symbol, so
        # parsing it with the real CLI is what keeps it from rotting into prose
        # exactly when an operator needs it: on an unavailable family.
        command = family.recovery_action.split()
        assert command[0] == "spice"
        recovery = build_parser().parse_args(command[1:])
        assert recovery.func is run_serve_rebuild_projections
        assert recovery.families == [family.name]


def test_the_schema_builds_exactly_the_registered_families():
    """A table nobody registered would be a fact with no way back."""
    probe = sqlite3.connect(":memory:")
    try:
        probe.executescript(PROJECTION_SCHEMA)
        built = {
            str(row[0])
            for row in probe.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table'"
            )
        }
    finally:
        probe.close()

    assert built - BOOKKEEPING_TABLES == set(PROJECTION_TABLES)
    assert len(PROJECTION_TABLES) == len(set(PROJECTION_TABLES))


def test_isolated_rebuild_serves_the_prior_generation_until_atomic_publish(tmp_path):
    store = _seeded_store(tmp_path)
    projections = store.projections
    before = _published_state(projections)
    started = Event()
    release = Event()
    failures: list[BaseException] = []

    def rebuild() -> None:
        try:
            rebuild_projection_family(
                projections,
                AGENT_ACTIVITY.name,
                lambda stage: _populate_activity(
                    stage,
                    tool_calls=9,
                    started=started,
                    release=release,
                ),
            )
        except BaseException as exc:
            failures.append(exc)

    worker = Thread(target=rebuild)
    worker.start()
    assert started.wait(timeout=5)

    state = projections.family_states()[0]
    served_during_rebuild = store.lane_metric_summary(
        AGENT_A, bucket_count=1, now=2000.0
    )

    assert state.status == PROJECTION_STATUS_REBUILDING
    assert state.servable is True
    assert _published_state(projections) == before
    assert served_during_rebuild.tool_calls == RECORDED_TOOL_CALLS
    with pytest.raises(SpiceError, match=PROJECTION_STATUS_REBUILDING):
        store.record_agent_metric_delta(AGENT_A, tool_calls=1)

    release.set()
    worker.join(timeout=5)

    assert not worker.is_alive()
    assert failures == []
    assert _published_state(projections)[0] == before[0] + 1
    assert (
        store.lane_metric_summary(AGENT_A, bucket_count=1, now=2000.0).tool_calls == 9
    )


def test_failed_isolated_rebuild_keeps_the_prior_generation_stale_and_servable(
    tmp_path,
):
    store = _seeded_store(tmp_path)
    projections = store.projections
    before = _published_state(projections)

    with pytest.raises(RuntimeError, match="synthetic interruption"):
        rebuild_projection_family(
            projections,
            AGENT_ACTIVITY.name,
            lambda stage: _populate_activity(
                stage,
                tool_calls=9,
                error="synthetic interruption",
            ),
        )

    state = projections.family_states()[0]
    assert state.status == PROJECTION_STATUS_STALE
    assert state.servable is True
    assert "synthetic interruption" in state.detail
    assert _published_state(projections) == before
    assert (
        store.lane_metric_summary(AGENT_A, bucket_count=1, now=2000.0).tool_calls
        == RECORDED_TOOL_CALLS
    )


def test_destructive_reset_and_failed_recovery_are_explicitly_unavailable(tmp_path):
    store = _seeded_store(tmp_path)
    projections = store.projections
    with projections.connect() as connection:
        connection.execute(f"PRAGMA user_version = {PROJECTION_SCHEMA_VERSION + 1}")
    _reopen(projections)
    state = projections.family_states()[0]

    with pytest.raises(SpiceError) as reset_error:
        store.lane_metric_summary(AGENT_A, bucket_count=1, now=2000.0)
    assert state.status == "incompatible"
    assert "incompatible" in str(reset_error.value)
    assert AGENT_ACTIVITY.recovery_action in str(reset_error.value)

    with pytest.raises(RuntimeError, match="still interrupted"):
        rebuild_projection_family(
            projections,
            AGENT_ACTIVITY.name,
            lambda stage: _populate_activity(
                stage,
                tool_calls=9,
                error="still interrupted",
            ),
        )

    state = projections.family_states()[0]
    assert state.status == PROJECTION_STATUS_UNAVAILABLE
    assert state.servable is False
    assert state.row_counts == dict.fromkeys(AGENT_ACTIVITY.tables, 0)
    with pytest.raises(SpiceError, match="still interrupted"):
        store.lane_metric_summary(AGENT_A, bucket_count=1, now=2000.0)


def test_authority_answers_every_read_while_projections_are_unavailable(
    tmp_path, monkeypatch
):
    """Topology, routing, filters, renewals, and identities never wait on a replay."""
    store = _seeded_store(tmp_path)
    team = store.create_team(
        team_id="team-a",
        members=[AGENT_A],
        config=TeamConfig(task_filters=("serve.team",)),
    )
    store.record_agent_identity(
        actor_id=AGENT_A,
        target_id="main-e",
        thread_id="agent-a",
        actual_driver="codex",
        actual_model="gpt-current",
        actual_effort="high",
        desired_driver="codex",
        desired_model="gpt-next",
        desired_effort="xhigh",
        transcript_owner="codex",
    )
    store.record_started_renewal(
        predecessor_agent_id=AGENT_A,
        successor_agent_id=SUCCESSOR,
        ancestor_thread_id="agent-a",
    )

    def unavailable(self):
        raise SpiceError("projection database is unavailable")

    monkeypatch.setattr(ServeProjectionStore, "connect", unavailable)

    state = store.team_state(team.team_id)
    routed_team = store.current_team_for_agent(SUCCESSOR)
    renewal = store.renewal_state_for_agent(AGENT_A)
    identity = store.agent_identity_for_actor(AGENT_A)

    # The renewal moved the successor into the predecessor's slot, and every
    # one of those facts reads back with the projection refusing to open.
    assert [member.agent_id for member in state.members] == [SUCCESSOR]
    assert state.config.task_filters == ("serve.team",)
    assert routed_team == team.team_id
    assert renewal is not None and renewal.successor_agent_id == SUCCESSOR
    assert identity is not None and identity.actual_model == "gpt-current"


def test_discarding_the_projection_file_leaves_authority_byte_identical(tmp_path):
    store = _seeded_store(tmp_path)
    store.create_team(team_id="team-a", members=[AGENT_A])
    projections = store.projections
    with sqlite_connection(store.path) as connection:
        authority_before = tuple(connection.iterdump())

    projections.path.unlink()
    with projections.connect() as connection:
        rebuilt = {
            str(row[0])
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table'"
            )
        }

    assert set(PROJECTION_TABLES) <= rebuilt
    with sqlite_connection(store.path) as connection:
        assert tuple(connection.iterdump()) == authority_before


def test_an_unrecognized_projection_version_discards_the_whole_file(tmp_path):
    """A build this writer has no contract for costs a replay, not a refusal."""
    store = _seeded_store(tmp_path)
    projections = store.projections
    with sqlite_connection(projections.path) as connection:
        connection.execute(
            "CREATE TABLE agent_reactions (agent_id TEXT PRIMARY KEY, value TEXT)"
        )
        connection.execute("INSERT INTO agent_reactions VALUES ('agent-a', 'from v99')")
        connection.execute(f"PRAGMA user_version = {PROJECTION_SCHEMA_VERSION + 98}")
    _reopen(projections)

    with projections.connect() as connection:
        surviving = {
            str(row[0])
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table'"
            )
        }
        version = int(connection.execute("PRAGMA user_version").fetchone()[0])

    assert surviving == set(PROJECTION_TABLES) | BOOKKEEPING_TABLES
    assert version == PROJECTION_SCHEMA_VERSION
    assert _published_state(projections)[1] == dict.fromkeys(AGENT_ACTIVITY.tables, 0)


def test_a_corrupt_projection_file_is_rebuilt_rather_than_reported(tmp_path):
    store = _seeded_store(tmp_path)
    projections = store.projections
    projections.path.write_bytes(b"this is not a SQLite database")
    _reopen(projections)

    with projections.connect() as connection:
        rebuilt = {
            str(row[0])
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table'"
            )
        }

    assert rebuilt == set(PROJECTION_TABLES) | BOOKKEEPING_TABLES
    assert _published_state(projections) == (
        FIRST_GENERATION,
        dict.fromkeys(AGENT_ACTIVITY.tables, 0),
    )


def test_a_deleted_projection_file_is_rebuilt_for_the_next_read_and_write(tmp_path):
    """Deleting a disposable store costs a replay, not a restart of the process."""
    store = _seeded_store(tmp_path)
    store.create_team(team_id="team-a", members=[AGENT_A])
    projections = store.projections
    with sqlite_connection(store.path) as connection:
        authority_before = tuple(connection.iterdump())

    projections.path.unlink()
    rebuilt = team_diagnostics_payload(store=store)["projections"]
    store.record_agent_metric_delta(
        AGENT_A,
        tool_calls=RECORDED_TOOL_CALLS,
        message_timestamps=[ACTIVITY_TIMESTAMP],
        tool_call_timestamps=[ACTIVITY_TIMESTAMP],
    )

    # The same live store that lost the file underneath it answers the next
    # read from a rebuilt one and counts the next delta into it.
    assert [row["family"] for row in rebuilt] == [AGENT_ACTIVITY.name]
    assert rebuilt[0]["generation"] == FIRST_GENERATION
    assert rebuilt[0]["rowCounts"] == dict.fromkeys(AGENT_ACTIVITY.tables, 0)
    assert _published_state(projections)[1]["agent_metrics"] == 1
    with sqlite_connection(store.path) as connection:
        assert tuple(connection.iterdump()) == authority_before


def test_an_unchanged_projection_file_is_synced_once_across_many_reads(
    tmp_path, monkeypatch
):
    """Noticing a vanished file costs a stat, so an unchanged one stays cheap."""
    store = _seeded_store(tmp_path)
    projections = store.projections
    synced: list[Path] = []
    sync = ServeProjectionStore._open_and_sync_locked

    def counting_sync(self: ServeProjectionStore) -> None:
        synced.append(self.path)
        sync(self)

    monkeypatch.setattr(ServeProjectionStore, "_open_and_sync_locked", counting_sync)

    for _ in range(REPEATED_READS):
        with projections.connect() as connection:
            connection.execute("SELECT COUNT(*) FROM agent_metrics").fetchone()
    projections.path.unlink()
    with projections.connect():
        pass

    # Every read of the file this store already synced went straight to SQLite;
    # the one schema pass belongs to the file that went missing.
    assert synced == [projections.path]


def test_an_unknown_family_name_is_named_beside_the_known_ones(tmp_path):
    projections = _store(tmp_path).projections

    with pytest.raises(SpiceError) as exc_info:
        rebuild_projection_family(
            projections,
            "laneWarehouse",
            lambda _stage: None,
        )

    assert "laneWarehouse" in str(exc_info.value)
    assert AGENT_ACTIVITY.name in str(exc_info.value)
