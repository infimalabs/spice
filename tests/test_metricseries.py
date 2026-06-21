"""Stable, range-queryable activity series for graphing (no windowing)."""

from __future__ import annotations

from spice.serve.teammetrics import MetricSeriesPoint
from spice.serve.teams import ServeTeamStore


def _store(tmp_path):
    return ServeTeamStore(path=tmp_path / "teams.sqlite3")


def test_activity_series_is_stable_full_fidelity_and_range_queryable(tmp_path):
    store = _store(tmp_path)
    store.record_agent_metric_delta("agent-a", message_timestamps=[60, 120, 180])

    first = store.agent_activity_series(["agent-a"], start=0, end=240)
    second = store.agent_activity_series(["agent-a"], start=0, end=240)

    # Stable: re-querying the same range yields identical points.
    assert first == second
    # Full fidelity: every stored bucket appears, with no rolling-window aging.
    assert first == (
        MetricSeriesPoint(60, 1),
        MetricSeriesPoint(120, 1),
        MetricSeriesPoint(180, 1),
    )
    # Arbitrary sub-range.
    assert store.agent_activity_series(["agent-a"], start=120, end=180) == (
        MetricSeriesPoint(120, 1),
        MetricSeriesPoint(180, 1),
    )
    assert store.agent_activity_series([], start=0, end=240) == ()


def test_activity_series_sums_across_agents(tmp_path):
    store = _store(tmp_path)
    store.record_agent_metric_delta("agent-a", message_timestamps=[60])
    store.record_agent_metric_delta("agent-b", message_timestamps=[60, 120])

    series = store.agent_activity_series(["agent-a", "agent-b"], start=0, end=180)

    assert series == (MetricSeriesPoint(60, 2), MetricSeriesPoint(120, 1))
