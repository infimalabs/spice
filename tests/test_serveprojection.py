"""Executable proofs that the rebuildable store is disposable on its own."""

from __future__ import annotations

import importlib
import re
import sqlite3

import pytest

import spice.serve.team.projection as projection_module
from spice.errors import SpiceError
from spice.serve.team.ids import thread_actor_id
from spice.serve.team.projection import (
    AGENT_ACTIVITY,
    FIRST_GENERATION,
    PROJECTION_FAMILIES,
    PROJECTION_SCHEMA,
    PROJECTION_SCHEMA_VERSION,
    PROJECTION_TABLES,
    ServeProjectionStore,
)
from spice.serve.team.store import ServeTeamStore, TeamConfig
from spice.sqliteconnection import sqlite_connection

AGENT_A = thread_actor_id("agent-a")
SUCCESSOR = thread_actor_id("agent-a-next")
RECORDED_TOOL_CALLS = 3
ACTIVITY_TIMESTAMP = 1000.0
# The store's own bookkeeping rather than a replayable family: it records which
# build of each family a reader is looking at.
BOOKKEEPING_TABLES = frozenset({"projection_generations"})
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
    """Force the next open to re-run the schema pass against the file on disk."""
    ServeProjectionStore._initialized_paths.discard(projections.path)


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


def test_every_family_registers_a_replay_contract_that_resolves():
    """Registration answers all five questions, and its code references are real."""
    for family in PROJECTION_FAMILIES:
        registration = (
            family.source,
            family.cursor,
            family.horizon,
            family.rebuild,
            family.beyond_horizon,
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


def test_reset_is_idempotent_and_republishes_on_every_run(tmp_path):
    store = _seeded_store(tmp_path)
    projections = store.projections

    first = projections.reset()
    after_first = _published_state(projections)
    second = projections.reset()
    after_second = _published_state(projections)

    # Emptying an empty family is not a no-op for the reader: the generation
    # still advances, which is what makes a reset retried after a crash arrive
    # at the same place as one that ran once.
    assert first == second == PROJECTION_FAMILIES
    assert after_first[1] == dict.fromkeys(AGENT_ACTIVITY.tables, 0)
    assert after_second[1] == after_first[1]
    assert after_first[0] == FIRST_GENERATION + 1
    assert after_second[0] == FIRST_GENERATION + 2


def test_a_reader_never_sees_a_new_generation_beside_old_rows(tmp_path, monkeypatch):
    """The emptied rows and the new generation are published as one fact."""
    store = _seeded_store(tmp_path)
    projections = store.projections
    before = _published_state(projections)
    observed: list[tuple[int, dict[str, int]]] = []
    publish = projection_module._bump_generation_locked

    def observing_publish(connection, family, now):
        # The writer has already deleted the rows in its open transaction. A
        # separate connection must still see the whole previous build.
        observed.append(_published_state(projections))
        publish(connection, family, now)

    monkeypatch.setattr(projection_module, "_bump_generation_locked", observing_publish)

    projections.reset()

    assert before[1]["agent_metrics"] == 1
    assert observed == [before]
    assert _published_state(projections) == (
        before[0] + 1,
        dict.fromkeys(AGENT_ACTIVITY.tables, 0),
    )


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
        actual_service_tier="priority",
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
    _reopen(projections)
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


def test_an_unknown_family_name_is_named_beside_the_known_ones(tmp_path):
    projections = _store(tmp_path).projections

    with pytest.raises(SpiceError) as exc_info:
        projections.reset("laneWarehouse")

    assert "laneWarehouse" in str(exc_info.value)
    assert AGENT_ACTIVITY.name in str(exc_info.value)
