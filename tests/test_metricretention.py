"""Retention bounds the high-growth metric series, not the durable aggregates."""

from __future__ import annotations

import time

from spice.serve.directivestats import DirectiveTotals
from spice.serve.teams import ServeTeamStore
from spice.serve.teamschema import METRIC_HISTORY_RETENTION_SECONDS

RECENT_TOOL_CALLS = 4


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
        tool_calls = connection.execute(
            "SELECT tool_calls FROM agent_metrics WHERE agent_id = ?", ("agent-a",)
        ).fetchone()["tool_calls"]

    floor = int(now) - METRIC_HISTORY_RETENTION_SECONDS
    # Old series rows are gone; the recent ones survive.
    assert all(start >= floor for start in bucket_starts)
    assert bucket_starts  # the recent bucket remains
    assert directive_keys == {"new"}
    # Durable aggregates are untouched by retention.
    assert int(tool_calls) == RECENT_TOOL_CALLS
    assert store.directive_totals_for_agents(["agent-a"]) == DirectiveTotals(
        sends=2, acked=2
    )
