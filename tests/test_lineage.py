"""Executable proofs for immutable observation actor lineage."""

from __future__ import annotations

import json
from dataclasses import dataclass

import pytest

from spice.errors import SpiceError
from spice.sqliteconnection import sqlite_connection
from tests.test_directivefacthelpers import (
    complete_directive_fact,
    publish_directive_fact,
)
from spice.serve.team.schema import (
    LEGACY_TEAM_SCHEMA_FINGERPRINT,
    OBSERVATION_ATTRIBUTION_REBUILD_REQUIRED,
    TEAM_AUTHORITY_SCHEMA,
    TEAM_PROJECTION_SCHEMA,
)
from spice.serve.team.store import ObservationAttributionMode, ServeTeamStore


def _record_identity(store: ServeTeamStore, actor_id: str) -> None:
    store.record_agent_identity(
        actor_id=actor_id,
        target_id="main-c",
        thread_id=actor_id.removeprefix("thread:"),
        actual_driver="codex",
        actual_model="gpt-current",
        actual_effort="high",
        actual_service_tier="priority",
        desired_driver="codex",
        desired_model="gpt-next",
        desired_effort="xhigh",
        transcript_owner="codex",
    )


def _record_session_facts(
    store: ServeTeamStore,
    *,
    actor_id: str,
    team_id: str,
    timestamp: float,
    suffix: str,
) -> None:
    store.record_agent_metric_delta(
        actor_id,
        tool_calls=1,
        message_timestamps=[timestamp],
        tool_call_timestamps=[timestamp],
    )
    store.record_agent_metric_cursor(
        actor_id,
        source_path=f"/transcripts/{suffix}.jsonl",
        offset=int(timestamp),
    )
    publish_directive_fact(
        store.directive_state_path,
        f"directive-{suffix}",
        agent_id=actor_id,
        team_id=team_id,
        sent_at=timestamp,
    )
    complete_directive_fact(
        store.directive_state_path,
        f"directive-{suffix}",
        acked_at=timestamp + 1,
    )
    store.record_task_lifecycle_event(
        "complete",
        task_id=f"task-{suffix}",
        agent_id=actor_id,
        team_id=team_id,
        ts=timestamp,
    )


def _actor_observation_rows(
    store: ServeTeamStore, actor_id: str
) -> dict[str, tuple[tuple[object, ...], ...]]:
    with store.connect() as connection:
        return {
            table: tuple(
                tuple(row)
                for row in connection.execute(
                    f'SELECT rowid, * FROM "{table}" WHERE agent_id = ? ORDER BY rowid',
                    (actor_id,),
                ).fetchall()
            )
            for table in (
                "agent_metrics",
                "agent_metric_buckets",
                "agent_metric_cursors",
                "task_events",
            )
        }


@dataclass(frozen=True)
class _LineageScenario:
    store: ServeTeamStore
    team_id: str
    actors: tuple[str, str, str]
    original_before: dict[str, tuple[tuple[object, ...], ...]]
    first_revision: int
    first_event_before: tuple[object, ...]


def _chained_lineage_scenario(tmp_path, monkeypatch) -> _LineageScenario:
    clock = {"now": 0.0}
    monkeypatch.setattr("spice.serve.team.store.time.time", lambda: clock["now"])
    monkeypatch.setattr("spice.serve.team.metrics.time.time", lambda: clock["now"])
    original = "thread:original"
    renewed = "thread:renewed"
    current = "thread:current"
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(team_id="team-lineage", members=[original])
    _record_identity(store, original)

    clock["now"] = 60
    _record_session_facts(
        store,
        actor_id=original,
        team_id=team.team_id,
        timestamp=60,
        suffix="original",
    )
    original_before = _actor_observation_rows(store, original)

    clock["now"] = 120
    first = store.record_started_renewal(
        predecessor_agent_id=original,
        successor_agent_id=renewed,
        ancestor_thread_id="original",
    )
    with store.connect() as connection:
        first_event_before = tuple(
            connection.execute(
                "SELECT revision, ts, team_id, payload FROM events WHERE revision = ?",
                (first.revision,),
            ).fetchone()
        )
    repeated_first = store.record_started_renewal(
        predecessor_agent_id=original,
        successor_agent_id=renewed,
        ancestor_thread_id="original",
    )
    assert repeated_first == first
    _record_identity(store, renewed)

    clock["now"] = 180
    _record_session_facts(
        store,
        actor_id=renewed,
        team_id=team.team_id,
        timestamp=180,
        suffix="renewed",
    )

    clock["now"] = 240
    second = store.record_started_renewal(
        predecessor_agent_id=renewed,
        successor_agent_id=current,
        ancestor_thread_id="renewed",
    )
    repeated_second = store.record_started_renewal(
        predecessor_agent_id=renewed,
        successor_agent_id=current,
        ancestor_thread_id="renewed",
    )
    assert repeated_second == second
    _record_identity(store, current)

    clock["now"] = 300
    _record_session_facts(
        store,
        actor_id=current,
        team_id=team.team_id,
        timestamp=300,
        suffix="current",
    )
    return _LineageScenario(
        store=store,
        team_id=team.team_id,
        actors=(original, renewed, current),
        original_before=original_before,
        first_revision=first.revision,
        first_event_before=first_event_before,
    )


def _lineage_views(scenario: _LineageScenario):
    store = scenario.store
    current = scenario.actors[2]
    source = store.lane_metric_summary(
        current,
        bucket_count=6,
        now=300,
        attribution=ObservationAttributionMode.SOURCE_ACTOR,
    )
    lineage = store.lane_metric_summary(
        current,
        bucket_count=6,
        now=300,
        attribution=ObservationAttributionMode.LINEAGE_CUMULATIVE,
    )
    session = store.lane_metric_summary(
        current,
        bucket_count=6,
        now=300,
        attribution=ObservationAttributionMode.PER_SESSION,
    )
    historical = store.team_historical_metric_summary(
        scenario.team_id,
        bucket_count=7,
        now=360,
        attribution=ObservationAttributionMode.TEAM_AT_EVENT_TIME,
    )
    source_series = store.agent_activity_series(
        [current],
        start=0,
        end=360,
        attribution=ObservationAttributionMode.SOURCE_ACTOR,
    )
    lineage_series = store.agent_activity_series(
        [current],
        start=0,
        end=360,
        attribution=ObservationAttributionMode.LINEAGE_CUMULATIVE,
    )
    session_series = store.agent_activity_series(
        [current],
        start=0,
        end=360,
        attribution=ObservationAttributionMode.PER_SESSION,
    )
    return (
        source,
        lineage,
        session,
        historical,
        source_series,
        lineage_series,
        session_series,
    )


def _assert_lineage_events(scenario: _LineageScenario) -> None:
    with scenario.store.connect() as connection:
        first_event_after = tuple(
            connection.execute(
                "SELECT revision, ts, team_id, payload FROM events WHERE revision = ?",
                (scenario.first_revision,),
            ).fetchone()
        )
        renewal_events = connection.execute(
            "SELECT payload FROM events WHERE kind = 'renewalStarted' ORDER BY revision"
        ).fetchall()
    assert first_event_after == scenario.first_event_before
    assert len(renewal_events) == 2
    assert [
        (
            json.loads(str(row["payload"]))["predecessor"],
            json.loads(str(row["payload"]))["successor"],
        )
        for row in renewal_events
    ] == [
        (scenario.actors[0], scenario.actors[1]),
        (scenario.actors[1], scenario.actors[2]),
    ]


def test_chained_renewal_keeps_source_rows_and_derives_all_four_lenses(
    tmp_path, monkeypatch
):
    scenario = _chained_lineage_scenario(tmp_path, monkeypatch)
    original, renewed, current = scenario.actors
    (
        source,
        lineage,
        session,
        historical,
        source_series,
        lineage_series,
        session_series,
    ) = _lineage_views(scenario)

    assert (source.acked, source.sends, source.tool_calls) == (1, 1, 1)
    assert sum(source.sparkline) == 1
    assert (lineage.acked, lineage.sends, lineage.tool_calls) == (3, 3, 3)
    assert sum(lineage.sparkline) == 3
    assert session == source
    assert historical.agent_ids == (original, renewed, current)
    assert historical.messages == 3
    assert sum(historical.sparkline) == 3
    assert [(point.bucket_start, point.messages) for point in source_series] == [
        (300, 1)
    ]
    assert [(point.bucket_start, point.messages) for point in lineage_series] == [
        (60, 1),
        (180, 1),
        (300, 1),
    ]
    assert session_series == source_series
    assert _actor_observation_rows(scenario.store, original) == (
        scenario.original_before
    )
    _assert_lineage_events(scenario)


def test_partial_retry_reuses_existing_lineage_event_and_finishes_topology(
    tmp_path, monkeypatch
):
    clock = {"now": 0.0}
    monkeypatch.setattr("spice.serve.team.store.time.time", lambda: clock["now"])
    predecessor = "thread:predecessor"
    successor = "thread:successor"
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(team_id="team-retry", members=[predecessor])
    _record_identity(store, predecessor)

    clock["now"] = 120
    payload = {
        "predecessor": predecessor,
        "successor": successor,
        "ancestor": "predecessor",
        "successorThreadId": "successor",
        "teamSlot": 0,
    }
    with store.connect() as connection:
        revision = store._record_event(
            connection,
            "renewalStarted",
            team.team_id,
            payload,
        )

    resumed = store.record_started_renewal(
        predecessor_agent_id=predecessor,
        successor_agent_id=successor,
        ancestor_thread_id="predecessor",
    )
    repeated = store.record_started_renewal(
        predecessor_agent_id=predecessor,
        successor_agent_id=successor,
        ancestor_thread_id="predecessor",
    )

    assert resumed.revision == revision
    assert repeated == resumed
    assert store.current_team_for_agent(predecessor) is None
    assert store.current_team_for_agent(successor) == team.team_id
    with store.connect() as connection:
        rows = connection.execute(
            "SELECT revision, payload FROM events WHERE kind = 'renewalStarted'"
        ).fetchall()
    assert [
        (int(row["revision"]), json.loads(str(row["payload"]))) for row in rows
    ] == [(revision, payload)]


def test_pre_transition_reassigned_rows_require_named_projection_rebuild(
    tmp_path, monkeypatch
):
    clock = {"now": 0.0}
    monkeypatch.setattr("spice.serve.team.store.time.time", lambda: clock["now"])
    monkeypatch.setattr("spice.serve.team.metrics.time.time", lambda: clock["now"])
    predecessor = "thread:predecessor"
    successor = "thread:successor"
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(team_id="team-legacy", members=[predecessor])
    _record_identity(store, predecessor)

    clock["now"] = 60
    # This impossible timestamp/actor combination models a row rewritten by
    # the old renewal path: it is attributed to the successor before that
    # source session existed.
    store.record_agent_metric_delta(
        successor,
        tool_calls=1,
        message_timestamps=[60],
        tool_call_timestamps=[60],
    )
    clock["now"] = 120
    store.record_started_renewal(
        predecessor_agent_id=predecessor,
        successor_agent_id=successor,
        ancestor_thread_id="predecessor",
    )

    with pytest.raises(
        SpiceError,
        match="rebuild Serve observation projections from their native facts",
    ):
        store.lane_metric_summary(
            successor,
            bucket_count=4,
            now=180,
            attribution=ObservationAttributionMode.SOURCE_ACTOR,
        )
    with pytest.raises(
        SpiceError,
        match="rebuild Serve observation projections from their native facts",
    ):
        store.team_historical_metric_summary(
            team.team_id,
            bucket_count=4,
            now=180,
            attribution=ObservationAttributionMode.TEAM_AT_EVENT_TIME,
        )


def test_legacy_aggregate_without_timestamp_provenance_is_marked_for_rebuild(
    tmp_path,
):
    path = tmp_path / "legacy-lineage.sqlite3"
    predecessor = "thread:predecessor"
    successor = "thread:successor"
    payload = json.dumps(
        {"predecessor": predecessor, "successor": successor},
        separators=(",", ":"),
    )
    with sqlite_connection(path) as connection:
        connection.executescript(TEAM_AUTHORITY_SCHEMA)
        connection.executescript(TEAM_PROJECTION_SCHEMA)
        connection.execute(
            "INSERT INTO events (revision, ts, kind, team_id, payload) "
            "VALUES (1, 120, 'renewalStarted', 'team-a', ?)",
            (payload,),
        )
        connection.execute(
            "INSERT INTO teams "
            "(team_id, created_at, revision, lifetime) "
            "VALUES ('team-a', 0, 1, 'Drive')"
        )
        connection.execute(
            "INSERT INTO memberships "
            "(team_id, agent_id, joined_at, position) "
            "VALUES ('team-a', ?, 120, 0)",
            (successor,),
        )
        connection.execute(
            "INSERT INTO agent_metrics "
            "(agent_id, team_id, tool_calls, updated_at) "
            "VALUES (?, 'team-a', 9, 300)",
            (successor,),
        )
        connection.execute(f"PRAGMA user_version = {LEGACY_TEAM_SCHEMA_FINGERPRINT}")
    store = ServeTeamStore(path=path)

    with pytest.raises(SpiceError, match="rebuild Serve observation projections"):
        store.lane_metric_summary(
            successor,
            bucket_count=4,
            now=360,
            attribution=ObservationAttributionMode.SOURCE_ACTOR,
        )
    with store.connect() as connection:
        status = connection.execute(
            "SELECT status FROM observation_attribution_state WHERE singleton = 1"
        ).fetchone()["status"]
    assert status == OBSERVATION_ATTRIBUTION_REBUILD_REQUIRED
