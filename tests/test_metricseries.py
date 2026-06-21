"""Stable, range-queryable activity series for graphing (no windowing)."""

from __future__ import annotations

from spice.serve.teammetrics import MetricSeriesPoint, TaskLifecycleSeriesPoint
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


def test_task_lifecycle_series_is_stable_full_fidelity_and_range_queryable(tmp_path):
    store = _store(tmp_path)
    store.record_task_lifecycle_event(
        "claim", task_id="task-1", agent_id="agent-a", team_id="team-a", ts=60
    )
    store.record_task_lifecycle_event(
        "phaseAdvance",
        task_id="task-1",
        agent_id="agent-a",
        team_id="team-a",
        ts=65,
    )
    store.record_task_lifecycle_event(
        "review", task_id="task-1", agent_id="agent-a", team_id="team-a", ts=70
    )
    store.record_task_lifecycle_event(
        "complete", task_id="task-1", agent_id="agent-a", team_id="team-a", ts=120
    )
    store.record_task_lifecycle_event(
        "drain", task_id="task-1", agent_id="agent-a", team_id="team-a", ts=121
    )

    first = store.task_lifecycle_series(["agent-a"], start=0, end=180)
    second = store.task_lifecycle_series(["agent-a"], start=0, end=180)

    assert first == second
    assert first == (
        TaskLifecycleSeriesPoint(
            bucket_start=60,
            claimed=1,
            active=2,
            completed=0,
            drained=0,
        ),
        TaskLifecycleSeriesPoint(
            bucket_start=120,
            claimed=0,
            active=0,
            completed=1,
            drained=1,
        ),
    )
    assert store.task_lifecycle_series(["agent-a"], start=120, end=180) == (
        TaskLifecycleSeriesPoint(
            bucket_start=120,
            claimed=0,
            active=0,
            completed=1,
            drained=1,
        ),
    )
    assert store.task_lifecycle_series(team_ids=["team-a"], start=0, end=180) == first
    assert (
        store.task_lifecycle_series(["agent-a"], team_ids=["team-b"], start=0, end=180)
        == ()
    )
    assert store.task_lifecycle_series(start=0, end=180) == ()


def test_task_lifecycle_events_are_tagged_with_team_at_capture(tmp_path):
    store = _store(tmp_path)
    store.create_team(team_id="team-a", members=["agent-a"])
    store.create_team(team_id="team-b", members=())

    store.record_task_lifecycle_event(
        "claim", task_id="task-1", agent_id="agent-a", ts=60
    )
    store.assign_agent("team-b", "agent-a")
    store.record_task_lifecycle_event(
        "complete", task_id="task-1", agent_id="agent-a", ts=120
    )

    assert store.task_lifecycle_series(team_ids=["team-a"], start=0, end=180) == (
        TaskLifecycleSeriesPoint(
            bucket_start=60,
            claimed=1,
            active=0,
            completed=0,
            drained=0,
        ),
    )
    assert store.task_lifecycle_series(team_ids=["team-b"], start=0, end=180) == (
        TaskLifecycleSeriesPoint(
            bucket_start=120,
            claimed=0,
            active=0,
            completed=1,
            drained=0,
        ),
    )


def test_task_lifecycle_events_rewrite_across_agent_id_assignment(tmp_path):
    store = _store(tmp_path)
    store.create_team(team_id="team-a", members=())
    store.record_task_lifecycle_event(
        "claim", task_id="task-1", agent_id="agent-old", team_id="team-a", ts=60
    )

    store.assign_agent("team-a", "agent-new", aliases=["agent-old"])

    assert store.task_lifecycle_series(["agent-old"], start=0, end=180) == ()
    assert store.task_lifecycle_series(["agent-new"], start=0, end=180) == (
        TaskLifecycleSeriesPoint(
            bucket_start=60,
            claimed=1,
            active=0,
            completed=0,
            drained=0,
        ),
    )
