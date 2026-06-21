"""Retention bounds the high-growth metric series, not the durable aggregates."""

from __future__ import annotations

import time

from spice.serve.directivestats import DirectiveTotals
from spice.serve.teammetrics import METRIC_HISTORY_RETENTION_DAYS_ENV
from spice.serve.teams import ServeTeamStore, TeamConfig
from spice.serve.teamschema import METRIC_HISTORY_RETENTION_SECONDS

RECENT_TOOL_CALLS = 4
SECONDS_PER_DAY = 24 * 60 * 60


def _store(tmp_path):
    return ServeTeamStore(path=tmp_path / "teams.sqlite3")


def test_prune_drops_old_series_but_keeps_aggregates_and_recent(tmp_path):
    store = _store(tmp_path)
    now = time.time()
    old = now - METRIC_HISTORY_RETENTION_SECONDS - 60
    recent = now - 60

    # Old + recent activity buckets, and old + recent directives.
    store.record_agent_metric_delta(
        "agent-a", tool_calls=RECENT_TOOL_CALLS, message_timestamps=[old, recent]
    )
    store.record_task_lifecycle_event(
        "claim", task_id="old-task", agent_id="agent-a", team_id="t", ts=old
    )
    store.record_task_lifecycle_event(
        "claim", task_id="new-task", agent_id="agent-a", team_id="t", ts=recent
    )
    store.record_directive_sent("old", agent_id="agent-a", team_id="t", sent_at=old)
    store.record_directive_sent("new", agent_id="agent-a", team_id="t", sent_at=recent)
    store.mark_directive_acked("old", acked_at=old)
    store.mark_directive_acked("new", acked_at=recent)

    store.team_snapshot()  # runs the prune pass

    with store.connect() as connection:
        bucket_starts = [
            int(row["bucket_start"])
            for row in connection.execute(
                "SELECT bucket_start FROM agent_metric_buckets WHERE agent_id = ?",
                ("agent-a",),
            )
        ]
        directive_keys = {
            str(row["directive_key"])
            for row in connection.execute("SELECT directive_key FROM directives")
        }
        task_ids = {
            str(row["task_id"])
            for row in connection.execute("SELECT task_id FROM task_events")
        }
        tool_calls = connection.execute(
            "SELECT tool_calls FROM agent_metrics WHERE agent_id = ?", ("agent-a",)
        ).fetchone()["tool_calls"]

    floor = int(now) - METRIC_HISTORY_RETENTION_SECONDS
    # Old series rows are gone; the recent ones survive.
    assert all(start >= floor for start in bucket_starts)
    assert bucket_starts  # the recent bucket remains
    assert directive_keys == {"new"}
    assert task_ids == {"new-task"}
    # Durable aggregates are untouched by retention.
    assert int(tool_calls) == RECENT_TOOL_CALLS
    assert store.directive_totals_for_agents(["agent-a"]) == DirectiveTotals(
        sends=2, acked=2
    )


def test_prune_uses_team_configured_metric_retention_horizon(tmp_path):
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
    store.record_agent_metric_delta("agent-a", message_timestamps=[old, recent])
    store.record_task_lifecycle_event(
        "claim", task_id="old-task", agent_id="agent-a", team_id="t", ts=old
    )
    store.record_task_lifecycle_event(
        "claim", task_id="new-task", agent_id="agent-a", team_id="t", ts=recent
    )
    store.record_directive_sent("old", agent_id="agent-a", team_id="t", sent_at=old)
    store.record_directive_sent("new", agent_id="agent-a", team_id="t", sent_at=recent)

    store.team_snapshot()

    with store.connect() as connection:
        bucket_starts = [
            int(row["bucket_start"])
            for row in connection.execute(
                "SELECT bucket_start FROM agent_metric_buckets WHERE agent_id = ?",
                ("agent-a",),
            )
        ]
        directive_keys = {
            str(row["directive_key"])
            for row in connection.execute("SELECT directive_key FROM directives")
        }
        task_ids = {
            str(row["task_id"])
            for row in connection.execute("SELECT task_id FROM task_events")
        }

    assert store.metric_history_retention_seconds() == retention_seconds
    assert all(start >= int(now) - retention_seconds for start in bucket_starts)
    assert bucket_starts
    assert directive_keys == {"new"}
    assert task_ids == {"new-task"}


def test_metric_retention_horizon_uses_env_fallback(tmp_path, monkeypatch):
    monkeypatch.setenv(METRIC_HISTORY_RETENTION_DAYS_ENV, "14")
    store = _store(tmp_path)

    assert store.metric_history_retention_seconds() == 14 * SECONDS_PER_DAY
