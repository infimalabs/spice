"""Stable, range-queryable activity series for graphing (no windowing)."""

from __future__ import annotations

import json
from types import SimpleNamespace

from spice.serve import metricpayload
from spice.serve.teammetrics import (
    METRIC_BUCKET_SECONDS,
    MetricSeriesPoint,
    TaskLifecycleSeriesPoint,
    TaskStallState,
)
from spice.serve.teams import ServeTeamStore

FIRST_RENEWAL_TS = 120
LATEST_RENEWAL_TS = 240
POST_RENEWAL_ACTIVITY_TS = 300
SERIES_END_TS = 360


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


def test_metric_series_payload_returns_stable_activity_directive_and_task_points(
    tmp_path,
):
    store = _store(tmp_path)
    state = SimpleNamespace(team_store=store)
    team = store.create_team(team_id="team-a", members=["agent-a"])
    store.record_agent_metric_delta("agent-a", message_timestamps=[60, 120])
    store.record_directive_sent(
        "dir-1", agent_id="agent-a", team_id="team-a", sent_at=60
    )
    store.mark_directive_acked("dir-1", acked_at=120)
    store.record_task_lifecycle_event(
        "complete", task_id="task-1", agent_id="agent-a", team_id="team-a", ts=180
    )

    activity = metricpayload.metric_series_payload(
        state,
        {"agentId": "agent-a", "metric": "activity", "start": 0, "end": 180},
    )
    sends = metricpayload.metric_series_payload(
        state,
        {"agentId": "agent-a", "metric": "sends", "start": 0, "end": 180},
    )
    acks = metricpayload.metric_series_payload(
        state,
        {"agentId": "agent-a", "metric": "acks", "start": 0, "end": 180},
    )
    team_sends = metricpayload.metric_series_payload(
        state,
        {"teamId": team.team_id, "metric": "sends", "start": 0, "end": 180},
    )
    burndown = metricpayload.metric_series_payload(
        state,
        {"agentId": "agent-a", "metric": "burndown", "start": 0, "end": 180},
    )

    assert activity["points"] == [
        {"bucketStart": 60, "value": 1, "messages": 1},
        {"bucketStart": 120, "value": 1, "messages": 1},
    ]
    assert sends["points"] == [{"bucketStart": 60, "value": 1, "sends": 1}]
    assert acks["points"] == [{"bucketStart": 120, "value": 1, "acks": 1}]
    assert team_sends["subject"]["teamId"] == team.team_id
    assert team_sends["points"] == sends["points"]
    assert burndown["points"] == [
        {
            "bucketStart": 180,
            "value": 1,
            "claimed": 0,
            "active": 0,
            "completed": 1,
            "drained": 0,
        }
    ]


def test_metric_series_payload_per_session_uses_latest_renewal_boundary(tmp_path):
    store = _store(tmp_path)
    state = SimpleNamespace(team_store=store)
    successor = "thread:successor"
    store.record_agent_metric_delta(
        successor,
        message_timestamps=[
            FIRST_RENEWAL_TS - METRIC_BUCKET_SECONDS,
            FIRST_RENEWAL_TS + METRIC_BUCKET_SECONDS,
            POST_RENEWAL_ACTIVITY_TS,
        ],
    )
    with store.connect() as connection:
        for timestamp in (FIRST_RENEWAL_TS, LATEST_RENEWAL_TS):
            connection.execute(
                "INSERT INTO events (ts, kind, team_id, payload) VALUES (?, ?, ?, ?)",
                (
                    timestamp,
                    "renewalStarted",
                    "team-a",
                    json.dumps({"successor": successor}),
                ),
            )

    payload = metricpayload.metric_series_payload(
        state,
        {
            "agentId": successor,
            "metric": "activity",
            "lens": "perSession",
            "start": 0,
            "end": SERIES_END_TS,
            "bucketSeconds": METRIC_BUCKET_SECONDS,
        },
    )

    assert payload["effectiveStart"] == LATEST_RENEWAL_TS
    assert payload["points"] == [
        {
            "bucketStart": POST_RENEWAL_ACTIVITY_TS,
            "value": 1,
            "messages": 1,
        }
    ]


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


def test_task_stall_states_flag_claimed_idle_task_after_threshold(tmp_path):
    store = _store(tmp_path)
    store.record_task_lifecycle_event(
        "claim", task_id="task-1", agent_id="agent-a", team_id="team-a", ts=60
    )

    states = store.task_stall_states(now=1_000, threshold_seconds=900)

    assert states == (
        TaskStallState(
            task_id="task-1",
            agent_id="agent-a",
            team_id="team-a",
            claimed_at=60.0,
            last_activity_at=0.0,
            last_progress_at=60.0,
            idle_seconds=940,
            threshold_seconds=900,
            stuck=True,
        ),
    )


def test_task_stall_states_use_activity_and_phase_progress(tmp_path):
    store = _store(tmp_path)
    store.record_task_lifecycle_event(
        "claim", task_id="task-1", agent_id="agent-a", team_id="team-a", ts=60
    )
    store.record_agent_metric_delta("agent-a", message_timestamps=[600])

    active = store.task_stall_states(now=800, threshold_seconds=300)

    assert active == (
        TaskStallState(
            task_id="task-1",
            agent_id="agent-a",
            team_id="team-a",
            claimed_at=60.0,
            last_activity_at=600.0,
            last_progress_at=600.0,
            idle_seconds=200,
            threshold_seconds=300,
            stuck=False,
        ),
    )
    assert (
        store.task_stall_states(
            ["agent-b"], team_ids=["team-a"], now=800, threshold_seconds=300
        )
        == ()
    )

    store.record_task_lifecycle_event(
        "phaseAdvance", task_id="task-1", agent_id="agent-a", team_id="team-a", ts=900
    )

    assert store.task_stall_states(now=1_000, threshold_seconds=300) == ()
