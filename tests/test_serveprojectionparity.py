"""Terminal integration proof for Serve authority/projection separation."""

from __future__ import annotations

import hashlib
import json
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import pytest

from spice.errors import SpiceError
from spice.serve.metrics import (
    rebuild_transcript_metrics,
    record_transcript_metrics_for_agent,
)
from spice.serve.team.projection import (
    PROJECTION_SCHEMA_VERSION,
    PROJECTION_STATUS_INCOMPATIBLE,
    PROJECTION_STATUS_READY,
    ServeProjectionStore,
)
from spice.serve.team.store import (
    ObservationAttributionMode,
    ServeTeamStore,
    TeamConfig,
)
from spice.sqliteconnection import sqlite_connection
from tests.test_directivefacthelpers import (
    complete_directive_fact,
    publish_directive_fact,
)

ORIGINAL = "thread:original"
RENEWED = "thread:renewed"
CURRENT = "thread:current"
ACTORS = (ORIGINAL, RENEWED, CURRENT)
TEAM_A = "team-a"
TEAM_B = "team-b"
NOW = datetime(2026, 7, 20, 12, 0, tzinfo=UTC).timestamp()
OLD_ACTIVITY = NOW - (2 * 24 * 60 * 60)
RETENTION_SECONDS = 24 * 60 * 60


def _record_identity(store: ServeTeamStore, actor_id: str) -> None:
    store.record_agent_identity(
        actor_id=actor_id,
        target_id="main-a",
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


def _iso(timestamp: float) -> str:
    return datetime.fromtimestamp(timestamp, tz=UTC).isoformat()


def _write_activity_transcript(path: Path, timestamp: float, suffix: str) -> None:
    entries = (
        {
            "timestamp": _iso(timestamp),
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": f"activity {suffix}"}],
            },
        },
        {
            "timestamp": _iso(timestamp + 1),
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "call_id": f"call-{suffix}",
                "arguments": "{}",
            },
        },
    )
    path.write_text(
        "".join(json.dumps(entry, separators=(",", ":")) + "\n" for entry in entries),
        encoding="utf-8",
    )


def _record_session_facts(
    store: ServeTeamStore,
    *,
    actor_id: str,
    team_id: str,
    timestamp: float,
    suffix: str,
    transcript: Path,
    task_plane,
) -> None:
    _write_activity_transcript(transcript, timestamp, suffix)
    record_transcript_metrics_for_agent(
        store,
        agent_id=actor_id,
        transcript_path=transcript,
    )
    publish_directive_fact(
        store.directive_state_path,
        f"directive-{suffix}",
        agent_id=actor_id,
        team_id=team_id,
        sent_at=timestamp,
    )
    assert complete_directive_fact(
        store.directive_state_path,
        f"directive-{suffix}",
        acked_at=timestamp + 2,
    )
    task_plane.record(
        "claim",
        task_id=f"task-{suffix}",
        agent_id=actor_id,
        ts=timestamp,
    )
    task_plane.record(
        "complete",
        task_id=f"task-{suffix}",
        agent_id=actor_id,
        ts=timestamp + 3,
    )


def _supported_metrics(store: ServeTeamStore) -> dict[str, Any]:
    return {
        "laneSource": store.lane_metric_summary(
            CURRENT,
            bucket_count=20,
            now=NOW,
            attribution=ObservationAttributionMode.SOURCE_ACTOR,
        ),
        "laneLineage": store.lane_metric_summary(
            CURRENT,
            bucket_count=20,
            now=NOW,
            attribution=ObservationAttributionMode.LINEAGE_CUMULATIVE,
        ),
        "laneSession": store.lane_metric_summary(
            CURRENT,
            bucket_count=20,
            now=NOW,
            attribution=ObservationAttributionMode.PER_SESSION,
        ),
        "teamHistory": store.team_historical_metric_summary(
            TEAM_B,
            bucket_count=20,
            now=NOW,
        ),
        "activity": store.agent_activity_series(
            (CURRENT,),
            start=OLD_ACTIVITY - 60,
            end=NOW,
            attribution=ObservationAttributionMode.LINEAGE_CUMULATIVE,
        ),
        "taskLifecycle": store.task_lifecycle_series(
            (CURRENT,),
            start=OLD_ACTIVITY - 60,
            end=NOW,
            attribution=ObservationAttributionMode.LINEAGE_CUMULATIVE,
        ),
        "taskDistribution": store.task_distribution_series(
            ACTORS,
            start=OLD_ACTIVITY - 60,
            end=NOW,
            bucket_seconds=60,
        ),
        "directiveLifecycle": store.directive_lifecycle_summary_for_agents(ACTORS),
    }


def _logical_snapshot(path: Path) -> dict[str, object]:
    with sqlite_connection(path) as connection:
        dump = tuple(connection.iterdump())
        schema = tuple(
            tuple(row)
            for row in connection.execute(
                "SELECT type, name, tbl_name, sql FROM sqlite_master "
                "ORDER BY type, name"
            )
        )
        version = int(connection.execute("PRAGMA user_version").fetchone()[0])
    encoded = "\n".join(dump).encode()
    return {
        "dump": dump,
        "schema": schema,
        "version": version,
        "checksum": hashlib.sha256(encoded).hexdigest(),
    }


def _renew_session(
    store: ServeTeamStore,
    *,
    clock: dict[str, float],
    predecessor: str,
    successor: str,
    renewal_time: float,
    activity_time: float,
    transcript: Path,
    task_plane: Any,
) -> None:
    ancestor = predecessor.removeprefix("thread:")
    suffix = successor.removeprefix("thread:")
    clock["now"] = renewal_time
    renewal = store.record_started_renewal(
        predecessor_agent_id=predecessor,
        successor_agent_id=successor,
        ancestor_thread_id=ancestor,
    )
    assert (
        store.record_started_renewal(
            predecessor_agent_id=predecessor,
            successor_agent_id=successor,
            ancestor_thread_id=ancestor,
        )
        == renewal
    )
    _record_identity(store, successor)
    _record_session_facts(
        store,
        actor_id=successor,
        team_id=TEAM_B,
        timestamp=activity_time,
        suffix=suffix,
        transcript=transcript,
        task_plane=task_plane,
    )


def _representative_history(
    tmp_path: Path, monkeypatch: Any, task_plane: Any
) -> tuple[ServeTeamStore, dict[str, Path]]:
    clock = {"now": OLD_ACTIVITY - 60}
    monkeypatch.setattr("spice.serve.team.store.time.time", lambda: clock["now"])
    retention = TeamConfig(
        shell_settings={"metrics": {"historyRetentionSeconds": RETENTION_SECONDS}}
    )
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    store.create_team(team_id=TEAM_A, members=[ORIGINAL], config=retention)
    _record_identity(store, ORIGINAL)
    transcripts = {
        ORIGINAL: tmp_path / "rollout-original.jsonl",
        RENEWED: tmp_path / "rollout-renewed.jsonl",
        CURRENT: tmp_path / "rollout-current.jsonl",
    }
    _record_session_facts(
        store,
        actor_id=ORIGINAL,
        team_id=TEAM_A,
        timestamp=OLD_ACTIVITY,
        suffix="original",
        transcript=transcripts[ORIGINAL],
        task_plane=task_plane,
    )

    clock["now"] = NOW - 600
    store.create_team(team_id=TEAM_B, config=retention)
    store.assign_agent(TEAM_B, ORIGINAL)
    _renew_session(
        store,
        clock=clock,
        predecessor=ORIGINAL,
        successor=RENEWED,
        renewal_time=NOW - 480,
        activity_time=NOW - 420,
        transcript=transcripts[RENEWED],
        task_plane=task_plane,
    )
    _renew_session(
        store,
        clock=clock,
        predecessor=RENEWED,
        successor=CURRENT,
        renewal_time=NOW - 300,
        activity_time=NOW - 240,
        transcript=transcripts[CURRENT],
        task_plane=task_plane,
    )
    store = ServeTeamStore(path=store.path)
    for actor_id, transcript in transcripts.items():
        record_transcript_metrics_for_agent(
            store,
            agent_id=actor_id,
            transcript_path=transcript,
        )
    clock["now"] = NOW
    store.prune_metric_history(now=NOW)
    return store, transcripts


def _discard_projection_schema(store: ServeTeamStore) -> None:
    with store.projections.connect() as projection:
        projection.execute(f"PRAGMA user_version = {PROJECTION_SCHEMA_VERSION + 1}")
    ServeProjectionStore._initialized_files.pop(store.projections.path, None)


def test_representative_history_has_full_parity_after_schema_reset_and_rebuild(
    tmp_path, monkeypatch, task_plane
):
    store, transcripts = _representative_history(tmp_path, monkeypatch, task_plane)
    before_metrics = _supported_metrics(store)
    before_authority = _logical_snapshot(store.path)
    before_directives = _logical_snapshot(store.directive_state_path)
    assert before_metrics["laneLineage"].tool_calls == 3
    assert sum(point.messages for point in before_metrics["activity"]) == 4
    assert sum(point.claimed for point in before_metrics["taskLifecycle"]) == 3
    assert sum(point.completed for point in before_metrics["taskLifecycle"]) == 3

    # A schema this writer cannot interpret is discarded in full. The family is
    # unavailable until a deterministic replay from the native transcript facts
    # publishes a complete replacement.
    _discard_projection_schema(store)
    incompatible = store.projections.family_states()[0]
    assert incompatible.status == PROJECTION_STATUS_INCOMPATIBLE
    assert incompatible.servable is False
    with pytest.raises(SpiceError, match="incompatible"):
        _supported_metrics(store)

    sources = tuple((actor, transcripts[actor]) for actor in ACTORS)
    rebuilt = rebuild_transcript_metrics(store, sources=sources)
    after_metrics = _supported_metrics(store)
    retried = rebuild_transcript_metrics(store, sources=sources)

    assert rebuilt.status == retried.status == PROJECTION_STATUS_READY
    assert rebuilt.generation + 1 == retried.generation
    assert rebuilt.retention_floor == retried.retention_floor == NOW - RETENTION_SECONDS
    assert after_metrics == before_metrics == _supported_metrics(store)
    assert _logical_snapshot(store.path) == before_authority
    assert _logical_snapshot(store.directive_state_path) == before_directives
