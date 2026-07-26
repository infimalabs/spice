"""Stable, range-queryable activity series for graphing (no windowing)."""

from __future__ import annotations

import json
from types import SimpleNamespace

import pytest

from spice.errors import SpiceError
from tests.test_directivefacthelpers import (
    complete_directive_fact,
    publish_directive_fact,
)
from spice.serve.payload import metric
from spice.serve.team.history import (
    METRIC_BUCKET_SECONDS,
    ObservationAttributionMode,
    TEAM_HISTORICAL_MAX_BUCKET_COUNT,
)
from spice.serve.team.metrics import (
    MetricSeriesPoint,
    TaskDistributionSeriesPoint,
    TaskLifecycleSeriesPoint,
    TaskStallState,
)
from spice.serve.team.ids import thread_actor_id
from spice.serve.team.store import ServeTeamStore

AGENT_A = thread_actor_id("agent-a")
AGENT_B = thread_actor_id("agent-b")
AGENT_NEW = thread_actor_id("agent-new")
AGENT_OLD = thread_actor_id("agent-old")
FIRST_RENEWAL_TS = 120
LATEST_RENEWAL_TS = 240
POST_RENEWAL_ACTIVITY_TS = 300
SERIES_END_TS = 360


def _store(tmp_path):
    return ServeTeamStore(path=tmp_path / "teams.sqlite3")


class _NoHistoricalSummaryStore:
    def __init__(self) -> None:
        self.summary_calls = 0

    def team_state(self, _team_id):
        return SimpleNamespace(members=[SimpleNamespace(agent_id=AGENT_A)])

    def team_historical_metric_summary(self, *_args, **_kwargs):
        self.summary_calls += 1
        raise AssertionError("team_historical_metric_summary should not be called")


def test_activity_series_is_stable_full_fidelity_and_range_queryable(tmp_path):
    store = _store(tmp_path)
    store.record_agent_metric_delta(AGENT_A, message_timestamps=[60, 120, 180])

    first = store.agent_activity_series([AGENT_A], start=0, end=240)
    second = store.agent_activity_series([AGENT_A], start=0, end=240)

    # Stable: re-querying the same range yields identical points.
    assert first == second
    # Full fidelity: every stored bucket appears, with no rolling-window aging.
    assert first == (
        MetricSeriesPoint(60, 1),
        MetricSeriesPoint(120, 1),
        MetricSeriesPoint(180, 1),
    )
    # Arbitrary sub-range.
    assert store.agent_activity_series([AGENT_A], start=120, end=180) == (
        MetricSeriesPoint(120, 1),
        MetricSeriesPoint(180, 1),
    )
    assert store.agent_activity_series([], start=0, end=240) == ()


def test_activity_series_sums_across_agents(tmp_path):
    store = _store(tmp_path)
    store.record_agent_metric_delta(AGENT_A, message_timestamps=[60])
    store.record_agent_metric_delta(AGENT_B, message_timestamps=[60, 120])

    series = store.agent_activity_series([AGENT_A, AGENT_B], start=0, end=180)

    assert series == (MetricSeriesPoint(60, 2), MetricSeriesPoint(120, 1))


def test_metric_series_payload_returns_stable_activity_directive_and_task_points(
    tmp_path,
    task_plane,
):
    store = _store(tmp_path)
    state = SimpleNamespace(team_store=store)
    team = store.create_team(team_id="team-a", members=[AGENT_A])
    store.record_agent_metric_delta(AGENT_A, message_timestamps=[60, 120])
    publish_directive_fact(
        store.directive_state_path,
        "dir-1",
        agent_id=AGENT_A,
        team_id="team-a",
        sent_at=60,
    )
    complete_directive_fact(store.directive_state_path, "dir-1", acked_at=120)
    task_plane.record("claim", task_id="task-1", agent_id=AGENT_A, ts=120)
    task_plane.record("complete", task_id="task-1", agent_id=AGENT_A, ts=180)

    activity = metric.metric_series_payload(
        state,
        {"agentId": AGENT_A, "metric": "activity", "start": 0, "end": 180},
    )
    sends = metric.metric_series_payload(
        state,
        {"agentId": AGENT_A, "metric": "sends", "start": 0, "end": 180},
    )
    acks = metric.metric_series_payload(
        state,
        {"agentId": AGENT_A, "metric": "acks", "start": 0, "end": 180},
    )
    team_sends = metric.metric_series_payload(
        state,
        {"teamId": team.team_id, "metric": "sends", "start": 0, "end": 180},
    )
    burndown = metric.metric_series_payload(
        state,
        {"agentId": AGENT_A, "metric": "burndown", "start": 0, "end": 180},
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
            "drained": 1,
        }
    ]


def test_metric_series_payload_distribution_returns_agent_share_points(
    tmp_path, task_plane, team_event
):
    store = _store(tmp_path)
    state = SimpleNamespace(team_store=store)
    # The projection names the lane's team now; the event log dates when that
    # membership began, which is what the movements are credited against.
    store.create_team(team_id="team-a", members=[AGENT_A, AGENT_B])
    team_event(store, "createTeam", team_id="team-a", ts=0, members=[AGENT_A, AGENT_B])
    task_plane.record("claim", task_id="task-a", agent_id=AGENT_A, ts=60)
    task_plane.record("phaseAdvance", task_id="task-a", agent_id=AGENT_A, ts=61)
    task_plane.record("claim", task_id="task-b", agent_id=AGENT_B, ts=62)
    task_plane.record("review", task_id="task-b", agent_id=AGENT_B, ts=120)

    payload = metric.metric_series_payload(
        state,
        {
            "agentId": AGENT_A,
            "metric": "distribution",
            "start": 0,
            "end": 180,
            "bucketSeconds": 60,
        },
    )

    assert payload["metric"] == "distribution"
    assert payload["subject"]["teamId"] == "team-a"
    assert [
        {
            "bucketStart": point["bucketStart"],
            "agentId": point["agentId"],
            "claimed": point["claimed"],
            "active": point["active"],
            "work": point["work"],
        }
        for point in payload["points"]
    ] == [
        {
            "bucketStart": 60,
            "agentId": AGENT_A,
            "claimed": 0,
            "active": 1,
            "work": 1,
        },
        {
            "bucketStart": 60,
            "agentId": AGENT_B,
            "claimed": 1,
            "active": 0,
            "work": 1,
        },
        {
            "bucketStart": 120,
            "agentId": AGENT_A,
            "claimed": 0,
            "active": 1,
            "work": 1,
        },
        {
            "bucketStart": 120,
            "agentId": AGENT_B,
            "claimed": 0,
            "active": 1,
            "work": 1,
        },
        {
            "bucketStart": 180,
            "agentId": AGENT_A,
            "claimed": 0,
            "active": 1,
            "work": 1,
        },
        {
            "bucketStart": 180,
            "agentId": AGENT_B,
            "claimed": 0,
            "active": 1,
            "work": 1,
        },
    ]
    assert [point["share"] for point in payload["points"]] == [
        pytest.approx(1 / 2),
        pytest.approx(1 / 2),
        pytest.approx(1 / 2),
        pytest.approx(1 / 2),
        pytest.approx(1 / 2),
        pytest.approx(1 / 2),
    ]
    assert [point["value"] for point in payload["points"]] == [
        pytest.approx(point["share"]) for point in payload["points"]
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

    payload = metric.metric_series_payload(
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


@pytest.mark.parametrize(
    ("query", "error_text"),
    [
        (
            {
                "teamId": "team-a",
                "metric": "activity",
                "lens": "teamHistorical",
                "start": 0,
                "end": "inf",
            },
            "end must be finite",
        ),
        (
            {
                "teamId": "team-a",
                "metric": "activity",
                "lens": "teamHistorical",
                "start": 0,
                "end": TEAM_HISTORICAL_MAX_BUCKET_COUNT * METRIC_BUCKET_SECONDS,
                "bucketSeconds": METRIC_BUCKET_SECONDS,
            },
            "range exceeds",
        ),
    ],
)
def test_metric_series_payload_team_historical_rejects_unbounded_ranges(
    query, error_text
):
    store = _NoHistoricalSummaryStore()
    state = SimpleNamespace(team_store=store)

    with pytest.raises(SpiceError, match=error_text):
        metric.metric_series_payload(state, query)

    assert store.summary_calls == 0


def test_task_lifecycle_series_is_stable_full_fidelity_and_range_queryable(
    tmp_path, task_plane, team_event
):
    store = _store(tmp_path)
    team_event(store, "createTeam", team_id="team-a", ts=0, members=[AGENT_A])
    task_plane.record("claim", task_id="task-1", agent_id=AGENT_A, ts=60)
    task_plane.record("phaseAdvance", task_id="task-1", agent_id=AGENT_A, ts=65)
    task_plane.record("claim", task_id="task-1", agent_id=AGENT_A, ts=70)
    task_plane.record("review", task_id="task-1", agent_id=AGENT_A, ts=75)
    # Completing drains the task off the open board in the same movement, so
    # one completion is one completed and one drained.
    task_plane.record("complete", task_id="task-1", agent_id=AGENT_A, ts=120)

    first = store.task_lifecycle_series([AGENT_A], start=0, end=180)
    second = store.task_lifecycle_series([AGENT_A], start=0, end=180)

    assert first == second
    assert first == (
        TaskLifecycleSeriesPoint(
            bucket_start=60,
            claimed=2,
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
    assert store.task_lifecycle_series([AGENT_A], start=120, end=180) == (
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
        store.task_lifecycle_series([AGENT_A], team_ids=["team-b"], start=0, end=180)
        == ()
    )
    assert store.task_lifecycle_series(start=0, end=180) == ()


def test_task_lifecycle_events_are_tagged_with_team_at_capture(
    tmp_path, task_plane, team_event
):
    store = _store(tmp_path)
    team_event(store, "createTeam", team_id="team-a", ts=0, members=[AGENT_A])

    task_plane.record("claim", task_id="task-1", agent_id=AGENT_A, ts=60)
    team_event(store, "assignAgent", team_id="team-b", ts=90, agentId=AGENT_A)
    task_plane.record("complete", task_id="task-1", agent_id=AGENT_A, ts=120)

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
            drained=1,
        ),
    )


def test_task_lifecycle_events_keep_source_actor_and_derive_alias_lineage(
    tmp_path, task_plane
):
    store = _store(tmp_path)
    store.create_team(team_id="team-a", members=())
    task_plane.record("claim", task_id="task-1", agent_id=AGENT_OLD, ts=60)

    store.assign_agent("team-a", AGENT_NEW, aliases=[AGENT_OLD])

    expected = (
        TaskLifecycleSeriesPoint(
            bucket_start=60,
            claimed=1,
            active=0,
            completed=0,
            drained=0,
        ),
    )
    assert store.task_lifecycle_series([AGENT_OLD], start=0, end=180) == expected
    assert store.task_lifecycle_series([AGENT_NEW], start=0, end=180) == ()
    assert (
        store.task_lifecycle_series(
            [AGENT_NEW],
            start=0,
            end=180,
            attribution=ObservationAttributionMode.LINEAGE_CUMULATIVE,
        )
        == expected
    )


def test_task_distribution_series_shows_per_agent_work_share(
    tmp_path, task_plane, team_event
):
    store = _store(tmp_path)
    team_event(store, "createTeam", team_id="team-a", ts=0, members=[AGENT_A, AGENT_B])
    task_plane.record("claim", task_id="task-a", agent_id=AGENT_A, ts=60)
    task_plane.record("phaseAdvance", task_id="task-a", agent_id=AGENT_A, ts=61)
    task_plane.record("claim", task_id="task-b", agent_id=AGENT_B, ts=62)
    task_plane.record("review", task_id="task-b", agent_id=AGENT_B, ts=120)
    task_plane.record("claim", task_id="task-a", agent_id=AGENT_A, ts=180)
    task_plane.record("complete", task_id="task-a", agent_id=AGENT_A, ts=180)

    first = store.task_distribution_series(team_ids=["team-a"], start=0, end=180)
    second = store.task_distribution_series(team_ids=["team-a"], start=0, end=180)

    assert first == second
    assert [
        TaskDistributionSeriesPoint(
            point.bucket_start,
            point.agent_id,
            point.claimed,
            point.active,
            share=0.0,
        )
        for point in first
    ] == [
        TaskDistributionSeriesPoint(60, AGENT_A, claimed=0, active=1, share=0.0),
        TaskDistributionSeriesPoint(60, AGENT_B, claimed=1, active=0, share=0.0),
        TaskDistributionSeriesPoint(120, AGENT_A, claimed=0, active=1, share=0.0),
        TaskDistributionSeriesPoint(120, AGENT_B, claimed=0, active=1, share=0.0),
        TaskDistributionSeriesPoint(180, AGENT_B, claimed=0, active=1, share=0.0),
    ]
    assert first[0].share == pytest.approx(1 / 2)
    assert first[1].share == pytest.approx(1 / 2)
    assert first[2].share == pytest.approx(1 / 2)
    assert first[3].share == pytest.approx(1 / 2)
    assert first[4].share == pytest.approx(1.0)
    assert store.task_distribution_series(
        [AGENT_A], team_ids=["team-a"], start=0, end=180
    ) == (
        TaskDistributionSeriesPoint(60, AGENT_A, 0, 1, 1.0),
        TaskDistributionSeriesPoint(120, AGENT_A, 0, 1, 1.0),
    )


def test_task_distribution_series_carries_staggered_open_claims(
    tmp_path, task_plane, team_event
):
    store = _store(tmp_path)
    team_event(store, "createTeam", team_id="team-a", ts=0, members=[AGENT_A, AGENT_B])
    task_plane.record("claim", task_id="task-a", agent_id=AGENT_A, ts=60)
    task_plane.record("claim", task_id="task-b", agent_id=AGENT_B, ts=120)

    series = store.task_distribution_series(team_ids=["team-a"], start=0, end=180)

    assert series == (
        TaskDistributionSeriesPoint(60, AGENT_A, claimed=1, active=0, share=1.0),
        TaskDistributionSeriesPoint(120, AGENT_A, claimed=1, active=0, share=0.5),
        TaskDistributionSeriesPoint(120, AGENT_B, claimed=1, active=0, share=0.5),
        TaskDistributionSeriesPoint(180, AGENT_A, claimed=1, active=0, share=0.5),
        TaskDistributionSeriesPoint(180, AGENT_B, claimed=1, active=0, share=0.5),
    )


def test_task_stall_states_flag_claimed_idle_task_after_threshold(
    tmp_path, task_plane, team_event
):
    store = _store(tmp_path)
    team_event(store, "createTeam", team_id="team-a", ts=0, members=[AGENT_A])
    task_plane.record("claim", task_id="task-1", agent_id=AGENT_A, ts=60)

    states = store.task_stall_states(now=1_000, threshold_seconds=900)

    assert states == (
        TaskStallState(
            task_id="task-1",
            agent_id=AGENT_A,
            team_id="team-a",
            claimed_at=60.0,
            last_activity_at=0.0,
            last_progress_at=60.0,
            idle_seconds=940,
            threshold_seconds=900,
            stuck=True,
        ),
    )


def test_task_stall_states_use_activity_and_phase_progress(
    tmp_path, task_plane, team_event
):
    store = _store(tmp_path)
    team_event(store, "createTeam", team_id="team-a", ts=0, members=[AGENT_A])
    task_plane.record("claim", task_id="task-1", agent_id=AGENT_A, ts=60)
    store.record_agent_metric_delta(AGENT_A, message_timestamps=[600])

    active = store.task_stall_states(now=800, threshold_seconds=300)

    assert active == (
        TaskStallState(
            task_id="task-1",
            agent_id=AGENT_A,
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
            [AGENT_B], team_ids=["team-a"], now=800, threshold_seconds=300
        )
        == ()
    )

    task_plane.record("phaseAdvance", task_id="task-1", agent_id=AGENT_A, ts=900)

    assert store.task_stall_states(now=1_000, threshold_seconds=300) == ()
