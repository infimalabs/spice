"""Retention bounds the high-growth metric series, not the durable aggregates."""

from __future__ import annotations

import time

import pytest

from spice.errors import SpiceError
from spice.mail.ackstate import directive_history_records_from_database
from spice.serve.directivestats import DirectiveTotals
from spice.serve.team.ids import thread_actor_id
from spice.serve.team.lifecycle import team_task_transitions
from spice.serve.team.metrics import METRIC_HISTORY_RETENTION_DAYS_ENV
from spice.serve.team.store import ServeTeamStore, TeamConfig
from tests.test_directivefacthelpers import (
    complete_directive_fact,
    publish_directive_fact,
)
from spice.serve.team.schema import METRIC_HISTORY_RETENTION_SECONDS

AGENT_A = thread_actor_id("agent-a")
RECENT_TOOL_CALLS = 4
SECONDS_PER_DAY = 24 * 60 * 60


def _store(tmp_path):
    return ServeTeamStore(path=tmp_path / "teams.sqlite3")


def test_prune_drops_old_series_but_keeps_aggregates_and_recent(tmp_path, task_plane):
    store = _store(tmp_path)
    now = time.time()
    old = now - METRIC_HISTORY_RETENTION_SECONDS - 60
    recent = now - 60

    # Old + recent activity buckets and canonical directives.
    store.record_agent_metric_delta(
        AGENT_A, tool_calls=RECENT_TOOL_CALLS, message_timestamps=[old, recent]
    )
    task_plane.record("claim", task_id="old-task", agent_id=AGENT_A, ts=old)
    task_plane.record("claim", task_id="new-task", agent_id=AGENT_A, ts=recent)
    publish_directive_fact(
        store.directive_state_path, "old", agent_id=AGENT_A, team_id="t", sent_at=old
    )
    publish_directive_fact(
        store.directive_state_path,
        "new",
        agent_id=AGENT_A,
        team_id="t",
        sent_at=recent,
    )
    complete_directive_fact(store.directive_state_path, "old", acked_at=old)
    complete_directive_fact(store.directive_state_path, "new", acked_at=recent)

    store.team_snapshot()  # runs the prune pass

    with store.connect() as connection:
        bucket_starts = [
            int(row["bucket_start"])
            for row in connection.execute(
                "SELECT bucket_start FROM agent_metric_buckets WHERE agent_id = ?",
                (AGENT_A,),
            )
        ]
        task_ids = {
            transition.task_id
            for transition in team_task_transitions(connection, end_time=now)
        }
        tool_calls = connection.execute(
            "SELECT tool_calls FROM agent_metrics WHERE agent_id = ?", (AGENT_A,)
        ).fetchone()["tool_calls"]

    floor = int(now) - METRIC_HISTORY_RETENTION_SECONDS
    # Old series rows are gone; the recent ones survive.
    assert all(start >= floor for start in bucket_starts)
    assert bucket_starts  # the recent bucket remains
    assert {
        record.key
        for record in directive_history_records_from_database(
            store.directive_state_path
        )
    } == {"old", "new"}
    # Retention bounds Serve's own series; the task plane keeps every
    # movement it ever recorded, so the older one still reads back.
    assert task_ids == {"old-task", "new-task"}
    # Durable aggregates are untouched by retention.
    assert int(tool_calls) == RECENT_TOOL_CALLS
    assert store.directive_totals_for_agents([AGENT_A]) == DirectiveTotals(
        sends=2, acked=2
    )


def test_prune_uses_team_configured_metric_retention_horizon(tmp_path, task_plane):
    store = _store(tmp_path)
    retention_seconds = 7 * SECONDS_PER_DAY
    now = time.time()
    old = now - retention_seconds - 60
    recent = now - retention_seconds + 60
    store.create_team(
        team_id="team-config",
        members=[],
        config=TeamConfig(shell_settings={"metrics": {"historyRetentionDays": 7}}),
    )
    store.record_agent_metric_delta(AGENT_A, message_timestamps=[old, recent])
    task_plane.record("claim", task_id="old-task", agent_id=AGENT_A, ts=old)
    task_plane.record("claim", task_id="new-task", agent_id=AGENT_A, ts=recent)
    publish_directive_fact(
        store.directive_state_path, "old", agent_id=AGENT_A, team_id="t", sent_at=old
    )
    publish_directive_fact(
        store.directive_state_path,
        "new",
        agent_id=AGENT_A,
        team_id="t",
        sent_at=recent,
    )

    store.team_snapshot()

    with store.connect() as connection:
        bucket_starts = [
            int(row["bucket_start"])
            for row in connection.execute(
                "SELECT bucket_start FROM agent_metric_buckets WHERE agent_id = ?",
                (AGENT_A,),
            )
        ]
        task_ids = {
            transition.task_id
            for transition in team_task_transitions(connection, end_time=now)
        }

    assert store.metric_history_retention_seconds() == retention_seconds
    assert all(start >= int(now) - retention_seconds for start in bucket_starts)
    assert bucket_starts
    assert {
        record.key
        for record in directive_history_records_from_database(
            store.directive_state_path
        )
    } == {"old", "new"}
    # Retention bounds Serve's own series; the task plane keeps every
    # movement it ever recorded, so the older one still reads back.
    assert task_ids == {"old-task", "new-task"}


def test_metric_retention_horizon_uses_env_fallback(tmp_path, monkeypatch):
    monkeypatch.setenv(METRIC_HISTORY_RETENTION_DAYS_ENV, "14")
    store = _store(tmp_path)

    assert store.metric_history_retention_seconds() == 14 * SECONDS_PER_DAY


@pytest.mark.parametrize("value", ["inf", "nan"])
def test_metric_retention_env_rejects_non_finite_days(tmp_path, monkeypatch, value):
    monkeypatch.setenv(METRIC_HISTORY_RETENTION_DAYS_ENV, value)
    store = _store(tmp_path)

    with pytest.raises(SpiceError) as exc_info:
        store.metric_history_retention_seconds()

    assert METRIC_HISTORY_RETENTION_DAYS_ENV in str(exc_info.value)
    assert "finite" in str(exc_info.value)


@pytest.mark.parametrize(
    ("settings", "field_name"),
    [
        (
            {"metrics": {"historyRetentionDays": "inf"}},
            "shellSettings.metrics.historyRetentionDays",
        ),
        (
            {"metrics": {"retentionDays": "nan"}},
            "shellSettings.metrics.retentionDays",
        ),
        (
            {"metricHistoryRetentionDays": float("inf")},
            "shellSettings.metricHistoryRetentionDays",
        ),
    ],
)
def test_metric_retention_shell_settings_reject_non_finite_days(
    tmp_path, settings, field_name
):
    store = _store(tmp_path)
    store.create_team(
        team_id="team-config",
        config=TeamConfig(shell_settings=settings),
        members=[],
    )

    with pytest.raises(SpiceError) as exc_info:
        store.metric_history_retention_seconds()

    assert field_name in str(exc_info.value)
    assert "finite" in str(exc_info.value)
